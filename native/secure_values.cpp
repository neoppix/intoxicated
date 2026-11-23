// The Java side of the engine's per-account "secure values".
//
// The engine asks the platform to hold small string values per account
// through a djinni interface: the static native
// `ILocalStorageHandlerCore.setPlatformImpl(IPlatformLocalStorageHandler)`
// takes a Java object implementing
// `com/roblox/protocols/localstorageplatforminterface/generated/IPlatformLocalStorageHandler`,
// and the engine calls that object from its own threads for the rest of the
// run. This file is that object, plus the few `java.*` classes djinni's glue
// builds real objects of while it wraps it. The values themselves live in
// `crates/intoxicated-runtime/src/secure_values.rs`, which hands this file a table
// of functions (`IntoxSecureValueOps`) to call; nothing is stored here.
//
// Unrelated to the engine's content store (`RbxStorage`, `rbx-storage.db`) and
// to `LocalStorageManager`. Handing the handler over is what stops the
// engine's `FLog::LocalStorageHandler` warning "Not available on the current
// platform".
//
// What djinni needs besides the handler:
//
// - `java/lang/ref/WeakReference`, constructed with one Object and asked
//   `get()`. Its proxy cache keys Java objects on it, so `get()` must answer
//   the referent while something else still holds it and null once nothing
//   does. A strong reference here would pin every wrapped object forever.
//   libjnivm rewrites an instance `<init>` lookup into a static one returning
//   the class, so the constructor is a static factory with descriptor
//   `(Ljava/lang/Object;)Ljava/lang/ref/WeakReference;`.
// - `java/lang/System.identityHashCode(Object)`: 0 for null, stable for an
//   object's life, distinct for distinct live objects. A constant would
//   collapse the cache.
// - `java/util/HashSet`, size and emptiness only, as `getUsers()` returns it.
// - `ILocalStorageHandlerCore$CppProxy`, a constructor taking a long and a
//   long field `nativeRef`, the core `setPlatformImpl` returns. Observed
//   (2026-11-22, traced launch): the engine constructs one and the call
//   returns it; it is discarded.
//
// Observed in the same traced launch: the only handler method the engine
// called was `deleteSecureValue`, twice, for one key. That delete is the
// write per launch that keeps the store's mtime at the launch time.
//
// **Registration order matters.** Every hook here that takes or returns an
// Object derives its descriptor from libjnivm's mapping of `jnivm::Object`,
// and that mapping is whatever class was registered for it last. It is set
// back to `java/lang/Object` before anything here is hooked.
//
// **Nothing here prints a value or an account id.** With
// `INTOXICATED_TRACE_LOCAL_STORAGE` set, each call prints its method name and
// key name on stderr.
//
// Also here, because it shares the bring-up moment and nothing else:
// `NativeInputInterface.nativeUpdateScreenOrientation(I)V`.

#include <jnivm.h>

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>

extern "C" {
/// Must match `secure_values::Ops` field for field.
struct IntoxSecureValueOps {
    long long (*current_user)(void);
    void (*set_current_user)(long long user);
    long long (*known_users)(void);
    /// 1 and one call of `receive` when found, else 0.
    int (*get)(long long user, const char* key, size_t key_len,
               void (*receive)(void* ctx, const char* bytes, size_t len), void* ctx);
    /// 1 when saved.
    int (*set)(long long user, const char* key, size_t key_len, const char* value,
               size_t value_len);
    int (*remove)(long long user, const char* key, size_t key_len);
    int (*remove_user)(long long user);
};
}

namespace intoxicated {
jnivm::ENV* process_env();
} // namespace intoxicated

namespace intoxicated::secure_values {

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

template <class T>
static auto to_jni(jnivm::ENV* env, const std::shared_ptr<T>& p) {
    return jnivm::JNITypes<std::shared_ptr<T>>::ToJNIType(env, p);
}

namespace {

const char* const kHandlerClass =
    "com/roblox/protocols/localstorageplatforminterface/generated/IPlatformLocalStorageHandler";
const char* const kCoreClass =
    "com/roblox/protocols/localstorageplatforminterface/generated/ILocalStorageHandlerCore";
const char* const kCoreProxyClass =
    "com/roblox/protocols/localstorageplatforminterface/generated/ILocalStorageHandlerCore$CppProxy";

std::atomic<const IntoxSecureValueOps*> g_ops{nullptr};

bool tracing() {
    static const bool on = getenv("INTOXICATED_TRACE_LOCAL_STORAGE") != nullptr;
    return on;
}

void trace(const char* method, const std::shared_ptr<String>& key = nullptr) {
    if (tracing()) {
        if (key) {
            fprintf(stderr, "[local-storage] %s(%s)\n", method, key->c_str());
        } else {
            fprintf(stderr, "[local-storage] %s\n", method);
        }
    }
}

void receive_into(void* ctx, const char* bytes, size_t len) {
    static_cast<std::string*>(ctx)->assign(bytes, len);
}

} // namespace

/// `java.lang.ref.WeakReference`.
class WeakReference : public Object {
public:
    std::weak_ptr<Object> referent;

    static std::shared_ptr<WeakReference> ctor(ENV* env, Class*, std::shared_ptr<Object> referent) {
        auto self = std::make_shared<WeakReference>();
        self->referent = referent;
        to_jni(env, self);
        return self;
    }

    std::shared_ptr<Object> get(ENV*) { return referent.lock(); }

    static void Register(ENV* env) {
        const char* name = "java/lang/ref/WeakReference";
        env->GetClass<WeakReference>(name);
        auto c = env->GetClass(name);
        c->Hook(env, "<init>", &WeakReference::ctor);
        c->HookInstanceFunction(env, "get", &WeakReference::get);
    }
};

/// `java.lang.System`, for `identityHashCode` alone.
///
/// Codes are handed out from a counter, so two live objects never share one,
/// and spread by multiplying with an odd constant, which keeps them distinct
/// modulo 2^32. An object keeps its code until it dies; the table forgets dead
/// objects as it grows.
class JavaSystem : public Object {
public:
    static jint identityHashCode(ENV*, Class*, std::shared_ptr<Object> object) {
        if (!object) {
            return 0;
        }
        struct Entry {
            std::weak_ptr<Object> object;
            jint code;
        };
        static std::mutex mutex;
        static std::unordered_map<const Object*, Entry> table;
        static uint32_t next = 0;
        static size_t prune_at = 1024;

        std::lock_guard<std::mutex> lock(mutex);
        auto it = table.find(object.get());
        if (it != table.end()) {
            auto alive = it->second.object.lock();
            if (alive == object) {
                return it->second.code;
            }
            table.erase(it);
        }
        if (table.size() >= prune_at) {
            for (auto i = table.begin(); i != table.end();) {
                i = i->second.object.expired() ? table.erase(i) : std::next(i);
            }
            prune_at = table.size() * 2 + 1024;
        }
        uint32_t code = 0;
        while (code == 0) {
            code = ++next * 0x9E3779B1u;
        }
        table.emplace(object.get(), Entry{object, static_cast<jint>(code)});
        return static_cast<jint>(code);
    }

    static void Register(ENV* env) {
        const char* name = "java/lang/System";
        env->GetClass<JavaSystem>(name);
        env->GetClass(name)->Hook(env, "identityHashCode", &JavaSystem::identityHashCode);
    }
};

/// `java.util.HashSet`, as much of it as `getUsers()` needs.
class HashSet : public Object {
public:
    jint count = 0;

    jint size(ENV*) { return count; }
    jboolean isEmpty(ENV*) { return count == 0; }

    static void Register(ENV* env) {
        const char* name = "java/util/HashSet";
        env->GetClass<HashSet>(name);
        auto c = env->GetClass(name);
        c->HookInstanceFunction(env, "size", &HashSet::size);
        c->HookInstanceFunction(env, "isEmpty", &HashSet::isEmpty);
    }
};

/// `ILocalStorageHandlerCore$CppProxy`.
class LocalStorageCoreProxy : public Object {
public:
    jlong nativeRef = 0;

    static std::shared_ptr<LocalStorageCoreProxy> ctor(ENV* env, Class*, jlong ref) {
        trace("ILocalStorageHandlerCore$CppProxy constructed");
        auto self = std::make_shared<LocalStorageCoreProxy>();
        self->nativeRef = ref;
        to_jni(env, self);
        return self;
    }

    static void Register(ENV* env) {
        env->GetClass<LocalStorageCoreProxy>(kCoreProxyClass);
        auto c = env->GetClass(kCoreProxyClass);
        c->Hook(env, "<init>", &LocalStorageCoreProxy::ctor);
        c->HookInstance(env, "nativeRef", &LocalStorageCoreProxy::nativeRef);
    }
};

/// `IPlatformLocalStorageHandler`, backed by the Rust store.
///
/// A read that finds nothing, or that cannot be answered for any reason,
/// is Java null, never an empty string. A write or delete answers true only
/// when the store was saved.
class PlatformLocalStorageHandler : public Object {
    static const IntoxSecureValueOps* ops() { return g_ops.load(std::memory_order_acquire); }

    static std::shared_ptr<String> read(long long user, const std::shared_ptr<String>& key) {
        auto* o = ops();
        if (!o || !key) {
            return nullptr;
        }
        std::string value;
        if (o->get(user, key->data(), key->size(), &receive_into, &value) != 1) {
            return nullptr;
        }
        return std::make_shared<String>(std::move(value));
    }

    static jboolean write(long long user, const std::shared_ptr<String>& key,
                          const std::shared_ptr<String>& value) {
        auto* o = ops();
        if (!o || !key || !value) {
            return false;
        }
        return o->set(user, key->data(), key->size(), value->data(), value->size()) == 1;
    }

    static long long current() {
        auto* o = ops();
        return o ? o->current_user() : 0;
    }

public:
    jlong getCurrentUser(ENV*) {
        trace("getCurrentUser");
        return current();
    }

    jboolean setCurrentUser(ENV*, jlong user) {
        trace("setCurrentUser");
        if (auto* o = ops()) {
            o->set_current_user(user);
            return true;
        }
        return false;
    }

    std::shared_ptr<HashSet> getUsers(ENV* env) {
        trace("getUsers");
        auto set = std::make_shared<HashSet>();
        if (auto* o = ops()) {
            set->count = static_cast<jint>(o->known_users());
        }
        to_jni(env, set);
        return set;
    }

    std::shared_ptr<String> getSecureValue(ENV*, std::shared_ptr<String> key) {
        trace("getSecureValue", key);
        return read(current(), key);
    }

    std::shared_ptr<String> getSecureValueForCurrentUser(ENV*, std::shared_ptr<String> key) {
        trace("getSecureValueForCurrentUser", key);
        return read(current(), key);
    }

    std::shared_ptr<String> getSecureValueForUser(ENV*, std::shared_ptr<String> key, jlong user) {
        trace("getSecureValueForUser", key);
        return read(user, key);
    }

    jboolean setSecureValue(ENV*, std::shared_ptr<String> key, std::shared_ptr<String> value) {
        trace("setSecureValue", key);
        return write(current(), key, value);
    }

    jboolean setSecureValueForCurrentUser(ENV*, std::shared_ptr<String> key,
                                          std::shared_ptr<String> value) {
        trace("setSecureValueForCurrentUser", key);
        return write(current(), key, value);
    }

    jboolean setSecureValueForUser(ENV*, std::shared_ptr<String> key, std::shared_ptr<String> value,
                                   jlong user) {
        trace("setSecureValueForUser", key);
        return write(user, key, value);
    }

    jboolean deleteSecureValue(ENV*, std::shared_ptr<String> key) {
        trace("deleteSecureValue", key);
        auto* o = ops();
        if (!o || !key) {
            return false;
        }
        return o->remove(current(), key->data(), key->size()) == 1;
    }

    jboolean deleteUserValues(ENV*, jlong user) {
        trace("deleteUserValues");
        auto* o = ops();
        return o && o->remove_user(user) == 1;
    }

    jboolean deleteCurrentUserValues(ENV* env) {
        trace("deleteCurrentUserValues");
        return deleteUserValues(env, current());
    }

    static void Register(ENV* env) {
        env->GetClass<PlatformLocalStorageHandler>(kHandlerClass);
        auto c = env->GetClass(kHandlerClass);
        using H = PlatformLocalStorageHandler;
        c->HookInstanceFunction(env, "getCurrentUser", &H::getCurrentUser);
        c->HookInstanceFunction(env, "setCurrentUser", &H::setCurrentUser);
        c->HookInstanceFunction(env, "getUsers", &H::getUsers);
        c->HookInstanceFunction(env, "getSecureValue", &H::getSecureValue);
        c->HookInstanceFunction(env, "getSecureValueForCurrentUser", &H::getSecureValueForCurrentUser);
        c->HookInstanceFunction(env, "getSecureValueForUser", &H::getSecureValueForUser);
        c->HookInstanceFunction(env, "setSecureValue", &H::setSecureValue);
        c->HookInstanceFunction(env, "setSecureValueForCurrentUser", &H::setSecureValueForCurrentUser);
        c->HookInstanceFunction(env, "setSecureValueForUser", &H::setSecureValueForUser);
        c->HookInstanceFunction(env, "deleteSecureValue", &H::deleteSecureValue);
        c->HookInstanceFunction(env, "deleteUserValues", &H::deleteUserValues);
        c->HookInstanceFunction(env, "deleteCurrentUserValues", &H::deleteCurrentUserValues);
    }
};

namespace {
/// The handler the engine holds, alive for the life of the process: the
/// engine keeps the reference and calls it from its own threads long after
/// the hand-over returned.
std::shared_ptr<PlatformLocalStorageHandler> g_handler;
} // namespace

/// Called from `jni_framework.cpp` during class registration.
void register_local_storage_classes(jnivm::ENV* env) {
    env->GetClass<Object>("java/lang/Object");
    WeakReference::Register(env);
    JavaSystem::Register(env);
    HashSet::Register(env);
    LocalStorageCoreProxy::Register(env);
    PlatformLocalStorageHandler::Register(env);
    // As found, for whatever registers after this.
    env->GetClass<Object>("java/lang/Object");
}

} // namespace intoxicated::secure_values

extern "C" {

/// `ILocalStorageHandlerCore.setPlatformImpl(handler)`, with the handler
/// backed by `ops`. The returned core object is discarded. A Java exception
/// the call leaves pending is described and cleared, so the next JNI call on
/// this thread does not inherit it, and reported as a failure.
int intox_secure_values_hand_over(void* fn, const IntoxSecureValueOps* ops, char* err,
                                  size_t err_len) {
    using Call = jobject (*)(JNIEnv*, jclass, jobject);
    auto* env = intoxicated::process_env();
    if (!fn || !env || !ops) {
        snprintf(err, err_len, "no JavaVM, no store, or setPlatformImpl is not exported");
        return -1;
    }
    intoxicated::secure_values::g_ops.store(ops, std::memory_order_release);
    try {
        auto cls = env->GetClass(intoxicated::secure_values::kCoreClass);
        auto handler = std::make_shared<intoxicated::secure_values::PlatformLocalStorageHandler>();
        // Parked before the call: the engine may call back from inside it.
        intoxicated::secure_values::g_handler = handler;
        JNIEnv* jni = env->GetJNIEnv();
        jobject core = reinterpret_cast<Call>(fn)(jni, (jclass)intoxicated::secure_values::to_jni(env, cls),
                                                  (jobject)intoxicated::secure_values::to_jni(env, handler));
        if (intoxicated::secure_values::tracing()) {
            fprintf(stderr, "[local-storage] setPlatformImpl returned %s\n", core ? "an object" : "null");
        }
        if (jni->ExceptionCheck()) {
            jni->ExceptionDescribe();
            jni->ExceptionClear();
            snprintf(err, err_len, "the call left a Java exception pending; described and cleared");
            return -1;
        }
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeInputInterface.nativeUpdateScreenOrientation(I)V`: 2 (landscape)
/// when the window is at least as wide as it is tall, else 1 (portrait).
int intox_update_screen_orientation(void* fn, int width, int height, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jint);
    auto* env = intoxicated::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeUpdateScreenOrientation is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jclass)intoxicated::secure_values::to_jni(env, cls),
                                   width >= height ? 2 : 1);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

} // extern "C"
