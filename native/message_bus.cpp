// The engine's message bus, from the Java side the engine expects to find.
//
// Written from a behavioural spec of the bus contract and the JNI
// specification. Roblox's universal app talks to its host over a message bus
// inside libroblox.so: a subscriber hands the engine a callback object, a
// request handler hands it a handler object, and the engine calls those
// objects back on whichever of its threads published or asked. On Android the
// objects are Roblox's own Java; here they are the three classes below, and
// the C entry points at the end are what `crates/intoxicated-runtime/src/bus.rs`
// declares. Nothing else in Rust calls them: bus.rs is the only path to a
// subscription (one per message id, fanned out to every listener) and to a
// request handler.
//
// | Java class                                            | hooked                                      |
// |-------------------------------------------------------|---------------------------------------------|
// | com/roblox/universalapp/messagebus/RawCallback         | run(Ljava/lang/String;)V                    |
// | com/roblox/universalapp/messagebus/Connection          | <init>(J)V, as libjnivm's static factory    |
// | com/roblox/universalapp/messagebus/RequestHandlerRaw   | run(Ljava/lang/String;)Ljava/lang/String;   |
//
// The descriptor of RequestHandlerRaw.run and the meaning of doSubscribeRaw's
// boolean (always false here) are INFERRED; the rest is what the engine has
// been seen to call.
//
// **Payloads are never parsed or printed here.** A payload can be clipboard
// text, a link with a ticket in it or a sign-in answer, so the only things
// this file says about one are its message id and its length.
//
// Tracing (`[messagebus] ...` on stderr) is on when INTOXICATED_TRACE_BUS or
// INTOXICATED_TRACE_CLIPBOARD is set to anything; the second is the
// clipboard's own switch, so one variable traces a copy end to end.

#include <jnivm.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

namespace intoxicated {
jnivm::ENV* process_env();
} // namespace intoxicated

namespace intoxicated {
namespace {

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

/// Receives one published payload, NUL-terminated, valid for the call.
using Sink = void (*)(const char*);
/// Answers one request: the request text, a zeroed output buffer and its
/// length. Non-zero when it wrote an answer.
using RequestSink = int (*)(const char*, char*, size_t);

constexpr const char* kMessageBusClass = "com/roblox/universalapp/messagebus/MessageBus";
constexpr const char* kRawCallbackClass = "com/roblox/universalapp/messagebus/RawCallback";
constexpr const char* kConnectionClass = "com/roblox/universalapp/messagebus/Connection";
constexpr const char* kRequestHandlerClass = "com/roblox/universalapp/messagebus/RequestHandlerRaw";

/// The answer a request gets when nothing here wrote one.
constexpr const char* kNoAnswer = "{\"success\":false}";
/// Room a request sink has for its answer, NUL included.
constexpr size_t kAnswerCapacity = 256;

bool tracing() {
    static const bool on = getenv("INTOXICATED_TRACE_BUS") != nullptr ||
                           getenv("INTOXICATED_TRACE_CLIPBOARD") != nullptr;
    return on;
}

template <class T>
auto to_jni(ENV* env, const std::shared_ptr<T>& p) {
    return jnivm::JNITypes<std::shared_ptr<T>>::ToJNIType(env, p);
}

/// Writes a message into a caller's error buffer, truncated to fit.
void fail(char* err, size_t err_len, const char* message) {
    if (err && err_len > 0) {
        snprintf(err, err_len, "%s", message);
    }
}

/// A Java exception the engine left pending, cleared so the next JNI call on
/// this thread does not inherit it.
bool cleared_exception(JNIEnv* jni, const char* native, char* err, size_t err_len) {
    if (!jni->ExceptionCheck()) {
        return false;
    }
    jni->ExceptionClear();
    if (err && err_len > 0) {
        snprintf(err, err_len, "%s threw (a Java exception was left pending and is cleared)", native);
    }
    return true;
}

/// A local frame for one call, popped however the call ends.
class LocalFrame {
public:
    explicit LocalFrame(JNIEnv* jni) : jni_(jni) { jni_->PushLocalFrame(16); }
    ~LocalFrame() { jni_->PopLocalFrame(nullptr); }
    LocalFrame(const LocalFrame&) = delete;
    LocalFrame& operator=(const LocalFrame&) = delete;

private:
    JNIEnv* jni_;
};

/// `RawCallback`: one subscription's callback. Its id and sink are set before
/// the engine ever sees the object and never change, so `run` reads them
/// from any thread without a lock.
class RawCallback : public Object {
public:
    std::string id;
    Sink sink = nullptr;

    void run(ENV*, std::shared_ptr<String> payload) {
        const char* text = payload ? payload->c_str() : "";
        const size_t n = payload ? payload->size() : 0;
        if (tracing()) {
            fprintf(stderr, "[messagebus] %s published %zu bytes%s\n", id.c_str(), n,
                    sink ? "" : " and nothing is listening");
        }
        if (sink) {
            sink(text);
        }
    }
};

/// `Connection`: what `doSubscribeRaw` hands back. The engine builds it with
/// the subscription's 64-bit value, which `isConnected` takes back.
class Connection : public Object {
public:
    jlong value = 0;

    static std::shared_ptr<Connection> ctor(ENV* env, Class*, jlong value) {
        auto self = std::make_shared<Connection>();
        self->value = value;
        to_jni(env, self);
        return self;
    }
};

/// `RequestHandlerRaw`: one bound request, answered by its sink.
class RequestHandlerRaw : public Object {
public:
    std::string name;
    RequestSink sink = nullptr;

    std::shared_ptr<String> run(ENV*, std::shared_ptr<String> request) {
        const char* text = request ? request->c_str() : "";
        const size_t n = request ? request->size() : 0;
        char answer[kAnswerCapacity];
        memset(answer, 0, sizeof answer);
        const bool answered = sink && sink(text, answer, sizeof answer) != 0;
        answer[sizeof answer - 1] = '\0';
        if (tracing()) {
            fprintf(stderr, "[messagebus] request %s: %zu bytes in, %s\n", name.c_str(), n,
                    answered ? "answered" : "no handler");
        }
        return std::make_shared<String>(std::string(answered ? answer : kNoAnswer));
    }
};

/// One message id's subscription as the engine last answered it.
struct Subscription {
    std::shared_ptr<RawCallback> callback;
    std::shared_ptr<Connection> connection;
    int64_t value = 0;
};

std::mutex g_mutex;
std::map<std::string, Subscription> g_subscriptions;
/// Every callback and handler ever handed to the engine. The engine keeps its
/// own reference and may call an object long after the hand-over, even one a
/// later subscribe replaced, so none of them is ever freed.
std::vector<std::shared_ptr<Object>> g_kept;

void keep(const std::shared_ptr<Object>& object) {
    std::lock_guard<std::mutex> lock(g_mutex);
    g_kept.push_back(object);
}

} // namespace

/// Called once from `register_framework_classes` (jni_framework.cpp). A second call does
/// nothing: registering a class again under the same name would replace it
/// and drop its hooks.
void register_message_bus_classes(jnivm::ENV* env) {
    static bool registered = false;
    if (registered) {
        return;
    }
    registered = true;

    env->GetClass<RawCallback>(kRawCallbackClass);
    env->GetClass(kRawCallbackClass)->HookInstanceFunction(env, "run", &RawCallback::run);

    env->GetClass<Connection>(kConnectionClass);
    env->GetClass(kConnectionClass)->Hook(env, "<init>", &Connection::ctor);

    env->GetClass<RequestHandlerRaw>(kRequestHandlerClass);
    env->GetClass(kRequestHandlerClass)->HookInstanceFunction(env, "run", &RequestHandlerRaw::run);
}

} // namespace intoxicated

extern "C" {

/// `MessageBus.doSubscribeRaw(id, callback, false)` through the engine's
/// native at `fn`. `sink` may be null: the id is then subscribed with nobody
/// listening. 0 on success, -1 with a message in `err`.
int intoxicated_messagebus_subscribe(void* fn, const char* message_id, void (*sink)(const char*), char* err,
                                     size_t err_len) {
    using namespace intoxicated;
    using Subscribe = jobject (*)(JNIEnv*, jclass, jstring, jobject, jboolean);
    auto* env = intoxicated::process_env();
    if (!fn) {
        fail(err, err_len, "doSubscribeRaw is not exported");
        return -1;
    }
    if (!env) {
        fail(err, err_len, "there is no JavaVM yet");
        return -1;
    }
    if (!message_id || !*message_id) {
        fail(err, err_len, "no message id");
        return -1;
    }
    try {
        auto callback = std::make_shared<RawCallback>();
        callback->id = message_id;
        callback->sink = sink;
        // Stored before the call: the bus may deliver from inside it.
        keep(callback);
        {
            std::lock_guard<std::mutex> lock(g_mutex);
            Subscription& s = g_subscriptions[message_id];
            s.callback = callback;
            s.connection.reset();
            s.value = 0;
        }
        JNIEnv* jni = env->GetJNIEnv();
        LocalFrame frame(jni);
        auto bus = env->GetClass(kMessageBusClass);
        auto id = std::make_shared<String>(std::string(message_id));
        jobject returned = reinterpret_cast<Subscribe>(fn)(jni, (jclass)to_jni(env, bus), (jstring)to_jni(env, id),
                                                           (jobject)to_jni(env, callback), JNI_FALSE);
        if (cleared_exception(jni, "doSubscribeRaw", err, err_len)) {
            return -1;
        }
        std::shared_ptr<Connection> connection;
        if (returned) {
            try {
                connection = jnivm::JNITypes<std::shared_ptr<Connection>>::JNICast(env, returned);
            } catch (...) {
                // Something that is not a Connection: there is no value to keep.
            }
        }
        std::lock_guard<std::mutex> lock(g_mutex);
        Subscription& s = g_subscriptions[message_id];
        if (s.callback == callback) {
            s.connection = connection;
            s.value = connection ? static_cast<int64_t>(connection->value) : 0;
        }
        return 0;
    } catch (const std::exception& e) {
        fail(err, err_len, e.what());
        return -1;
    } catch (...) {
        fail(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// The value inside the `Connection` the engine returned for `message_id`,
/// or 0 when it was never subscribed or no `Connection` came back.
int64_t intoxicated_messagebus_connection_value(const char* message_id) {
    using namespace intoxicated;
    if (!message_id) {
        return 0;
    }
    std::lock_guard<std::mutex> lock(g_mutex);
    auto it = g_subscriptions.find(message_id);
    return it == g_subscriptions.end() ? 0 : it->second.value;
}

/// `Connection.isConnected(value)`, a static native taking a `long`.
int intoxicated_messagebus_is_connected(void* fn, int64_t value, int* out_connected, char* err, size_t err_len) {
    using namespace intoxicated;
    using IsConnected = jboolean (*)(JNIEnv*, jclass, jlong);
    auto* env = intoxicated::process_env();
    if (!fn) {
        fail(err, err_len, "Connection.isConnected is not exported");
        return -1;
    }
    if (!env) {
        fail(err, err_len, "there is no JavaVM yet");
        return -1;
    }
    if (value == 0) {
        fail(err, err_len, "no Connection came back from doSubscribeRaw");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        LocalFrame frame(jni);
        auto cls = env->GetClass(kConnectionClass);
        jboolean live = reinterpret_cast<IsConnected>(fn)(jni, (jclass)to_jni(env, cls), static_cast<jlong>(value));
        if (cleared_exception(jni, "Connection.isConnected", err, err_len)) {
            return -1;
        }
        if (out_connected) {
            *out_connected = live ? 1 : 0;
        }
        return 0;
    } catch (const std::exception& e) {
        fail(err, err_len, e.what());
        return -1;
    } catch (...) {
        fail(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `MessageBus.setRequestHandlerRaw(protocol, method, handler)`. Handlers
/// accumulate; none is ever replaced or freed.
int intoxicated_messagebus_set_request_handler(void* fn, const char* protocol, const char* method,
                                               int (*sink)(const char*, char*, size_t), char* err,
                                               size_t err_len) {
    using namespace intoxicated;
    using SetHandler = void (*)(JNIEnv*, jclass, jstring, jstring, jobject);
    auto* env = intoxicated::process_env();
    if (!fn) {
        fail(err, err_len, "setRequestHandlerRaw is not exported");
        return -1;
    }
    if (!env) {
        fail(err, err_len, "there is no JavaVM yet");
        return -1;
    }
    if (!protocol || !*protocol || !method || !*method) {
        fail(err, err_len, "no protocol or no method");
        return -1;
    }
    try {
        auto handler = std::make_shared<RequestHandlerRaw>();
        handler->name = std::string(protocol) + "." + method;
        handler->sink = sink;
        // Kept before the call, for the same reason as a subscription's.
        keep(handler);
        JNIEnv* jni = env->GetJNIEnv();
        LocalFrame frame(jni);
        auto bus = env->GetClass(kMessageBusClass);
        auto p = std::make_shared<String>(std::string(protocol));
        auto m = std::make_shared<String>(std::string(method));
        reinterpret_cast<SetHandler>(fn)(jni, (jclass)to_jni(env, bus), (jstring)to_jni(env, p),
                                         (jstring)to_jni(env, m), (jobject)to_jni(env, handler));
        return cleared_exception(jni, "setRequestHandlerRaw", err, err_len) ? -1 : 0;
    } catch (const std::exception& e) {
        fail(err, err_len, e.what());
        return -1;
    } catch (...) {
        fail(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

} // extern "C"
