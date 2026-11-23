// Callers for static engine natives of a few fixed shapes: the deep-link
// protocols' cold-start entry and init, the MessageBus getters and the web
// view's protocol getters. `crates/intoxicated-linker-sys/src/game_activity/linking.rs`
// wraps them.
//
// Each calls `native(env, <class named class_name>, args...)` with Java
// strings made from the C strings (null as ""), inside a local frame, and
// turns any C++ exception into -1 and its text. A URL handed in has already
// been checked and rewritten by the Rust side; it is passed as the one string
// argument and used for nothing else.
//
// String answers go into the caller's buffer whole or not at all: an answer
// of N bytes needs N + 1 bytes of buffer, and a shorter buffer is refused
// naming both sizes. A null jstring is the empty answer.

#include "jni_world.h"

#include <cstdio>
#include <cstring>
#include <string>

namespace intoxicated {

jnivm::ENV* process_env();

namespace {

using java::LocalFrame;
using java::contained;
using java::report;
using java::to_jni;

/// The env to call through, or null after writing the common refusal.
jnivm::ENV* env_for(void* native, const char* class_name, char* err, size_t err_len) {
    jnivm::ENV* env = native != nullptr && class_name != nullptr ? process_env() : nullptr;
    if (env == nullptr) {
        report(err, err_len, "no JavaVM, or the native is not exported");
    }
    return env;
}

jstring jstring_of(jnivm::ENV* env, const char* s) {
    return static_cast<jstring>(to_jni(env, java::jstr(s != nullptr ? s : "")));
}

/// Copies the answer, or refuses it when it does not fit.
int answer(jstring result, char* out, size_t out_len, char* err, size_t err_len) {
    auto text = java::object_as<jnivm::String>(reinterpret_cast<jobject>(result));
    const size_t n = text ? text->size() : 0;
    if (out == nullptr || n >= out_len) {
        char message[96];
        snprintf(message, sizeof message, "the answer is %zu bytes and the buffer is %zu", n,
                 out == nullptr ? static_cast<size_t>(0) : out_len);
        report(err, err_len, message);
        return -1;
    }
    if (n > 0) {
        std::memcpy(out, text->data(), n);
    }
    out[n] = '\0';
    return 0;
}

} // namespace
} // namespace intoxicated

extern "C" {

/// `jstring (JNIEnv*, jclass)`: a protocol getter.
int intoxicated_deeplink_protocol_string(void* native, const char* class_name, char* out, size_t out_len, char* err,
                                         size_t err_len) {
    using namespace intoxicated;
    jnivm::ENV* env = env_for(native, class_name, err, err_len);
    if (env == nullptr) {
        return -1;
    }
    return contained(err, err_len, [&]() -> int {
        LocalFrame frame(env, 8);
        using Call = jstring (*)(JNIEnv*, jclass);
        jstring r = reinterpret_cast<Call>(native)(env->GetJNIEnv(), java::find_class(env, class_name));
        return answer(r, out, out_len, err, err_len);
    });
}

/// `jstring (JNIEnv*, jclass, jstring)`: `MessageBus.getLastRaw`.
int intoxicated_deeplink_string_ret_string(void* native, const char* class_name, const char* arg, char* out,
                                           size_t out_len, char* err, size_t err_len) {
    using namespace intoxicated;
    jnivm::ENV* env = env_for(native, class_name, err, err_len);
    if (env == nullptr) {
        return -1;
    }
    return contained(err, err_len, [&]() -> int {
        LocalFrame frame(env, 8);
        using Call = jstring (*)(JNIEnv*, jclass, jstring);
        jstring r = reinterpret_cast<Call>(native)(env->GetJNIEnv(), java::find_class(env, class_name),
                                                   jstring_of(env, arg));
        return answer(r, out, out_len, err, err_len);
    });
}

/// `jstring (JNIEnv*, jclass, jstring, jstring)`: `MessageBus.getMessageId`.
int intoxicated_deeplink_two_strings_ret_string(void* native, const char* class_name, const char* a, const char* b,
                                                char* out, size_t out_len, char* err, size_t err_len) {
    using namespace intoxicated;
    jnivm::ENV* env = env_for(native, class_name, err, err_len);
    if (env == nullptr) {
        return -1;
    }
    return contained(err, err_len, [&]() -> int {
        LocalFrame frame(env, 8);
        using Call = jstring (*)(JNIEnv*, jclass, jstring, jstring);
        jstring r = reinterpret_cast<Call>(native)(env->GetJNIEnv(), java::find_class(env, class_name),
                                                   jstring_of(env, a), jstring_of(env, b));
        return answer(r, out, out_len, err, err_len);
    });
}

/// `jboolean (JNIEnv*, jclass, jstring)`: `maybeHandleColdStartProtocolLaunch`.
/// On success `*out_handled` (when given) is 1 for any non-zero answer, else
/// 0; on failure it is left as it was.
int intoxicated_deeplink_cold_start(void* native, const char* class_name, const char* url, int* out_handled,
                                    char* err, size_t err_len) {
    using namespace intoxicated;
    jnivm::ENV* env = env_for(native, class_name, err, err_len);
    if (env == nullptr) {
        return -1;
    }
    return contained(err, err_len, [&]() -> int {
        LocalFrame frame(env, 8);
        using Call = jboolean (*)(JNIEnv*, jclass, jstring);
        const jboolean handled = reinterpret_cast<Call>(native)(env->GetJNIEnv(), java::find_class(env, class_name),
                                                                jstring_of(env, url));
        if (out_handled != nullptr) {
            *out_handled = handled != 0 ? 1 : 0;
        }
        return 0;
    });
}

/// `void (JNIEnv*, jclass, jobject context)`: a linking protocol's `init`.
/// There is no `android.content.Context` here; the context is a fresh plain
/// `java/lang/Object`.
int intoxicated_deeplink_protocol_init(void* native, const char* class_name, char* err, size_t err_len) {
    using namespace intoxicated;
    jnivm::ENV* env = env_for(native, class_name, err, err_len);
    if (env == nullptr) {
        return -1;
    }
    return contained(err, err_len, [&]() -> int {
        LocalFrame frame(env, 8);
        auto context = java::instance_of(env, java::find_class(env, "java/lang/Object"));
        using Call = void (*)(JNIEnv*, jclass, jobject);
        reinterpret_cast<Call>(native)(env->GetJNIEnv(), java::find_class(env, class_name),
                                       static_cast<jobject>(to_jni(env, context)));
        return 0;
    });
}

} // extern "C"
