// `com.roblox.protocols.localstorageplatforminterface.generated.*`, and the
// screen-orientation call docs/analysis/flag-init.md §16 found missing.
//
// Two unrelated gaps share this file because both are small and both are new:
// the task that added them could only create one C++ file, and CMakeLists.txt
// lists sources by name rather than by glob (see the comment there on why —
// a `local_storage.cpp` that a build did not know to compile links with
// "undefined symbol" and looks like a mistake in the calling code instead).
//
// **What `localstorageplatforminterface` is, and what it is not.**
// `docs/analysis/flag-init.md` §12 already separates two things the dex and
// the engine both call "storage": `RbxStorage`, the content cache that gates
// on `LocalStorageManager.getAllocatableBytes` and that Cordial has never
// found a way to construct directly, and *this* -- `ILocalStorageHandlerCore`
// and `IPlatformLocalStorageHandler` -- which is a small per-user key/value
// store the engine asks the platform to hold on its behalf. The two are
// unrelated exports of the same class name prefix and nothing here bears on
// the `RbxStorage::init` question; §12 already established that nothing in
// the dex or the exports constructs `RbxStorage`, and adding this does not
// change that.
//
// **The direction of the call, worked out from `readelf`, not assumed.**
// `Java_..._ILocalStorageHandlerCore_setPlatformImpl` and the `nativeDestroy`/
// `native_*` family under `IPlatformLocalStorageHandler$CppProxy` are all
// *exported by* `libroblox.so` -- i.e. they are native methods the engine's
// own compiled code answers, the same shape as `LocalStorageManager`'s
// `initStorageManagerNativeV3` a few classes over. `setPlatformImpl` is the
// entry point: on real Android, application code builds a Java object
// implementing `IPlatformLocalStorageHandler` and hands it to this static, and
// the engine then calls straight back into that object's own methods --
// `getSecureValue`, `setCurrentUser` and the rest -- whenever it wants to read
// or write something. Cordial has no Java side to build that object, so this
// file is it: `PlatformLocalStorageHandler` below is a libjnivm `Object`
// subclass whose hooked methods answer exactly the calls the interface
// declares, in the same idiom `init_params.cpp`'s `AndroidActivity` and
// `Resources` already use for the classes the engine expects to call methods
// on rather than construct.
//
// The `$CppProxy` classes and their `native_*` exports are the reverse
// direction of the same djinni scaffolding -- a Java handle onto a *C++*
// implementation living inside the engine itself, used somewhere else in
// `libroblox.so` -- and are not this file's concern; nothing here constructs
// one or calls into it.
//
// **Reference, not source.** The shape of the call --
// `IPlatformLocalStorageHandler` built and handed to `setPlatformImpl` right
// after the asset manager is set up -- was confirmed against
// `mocktail/src/legacy/legacy_runtime.cc`'s `ConfigureLocalStorage`
// (`third_party/mocktail-webview/` is a different, unrelated vendor tree; the
// checkout this was read from lives outside this repository entirely, per the
// task that added this file). Mocktail is Apache-2.0; this is a GPL-3.0
// project, and nothing below is transcribed from it -- mocktail backs its
// handler with a generic `SharedPreferences` object and an XML file with no
// attempt at the secrecy the method names claim, which is a design this
// project has an explicit rule against repeating (see `secrets.rs`'s own
// header, and the note below). What is reused is the fact of the call and its
// argument shapes; the storage behind it is Cordial's own.
//
// **This handles secrets, named as such.** `getSecureValue`/`setSecureValue`
// and their `ForUser`/`ForCurrentUser` twins carry per-account credentials --
// the dex's own vocabulary, not a guess. AGENTS.md's rule and
// `crates/cordial-runtime/src/secrets.rs`'s own header apply here exactly as
// they do to the cookie jar and the identity mirror: nothing below prints a
// value or a user id at any verbosity. The user id is logged nowhere, not even
// at trace level; only key names and byte counts are, the same restraint
// `secrets.rs` documents for itself.
//
// **The store itself lives in Rust, not here, and not in `secrets.rs`.**
// `secrets.rs` already is this project's model for where a secret belongs --
// the desktop Secret Service first, a `0600` file second, told plainly either
// way -- and the right thing was to call it, not rebuild it. Its `Kind` enum
// only has two members, `Cookies` and `Identity`, each holding exactly one
// document per profile; a per-user, arbitrary-key store does not fit that
// shape, and the task that added this file left `secrets.rs` off limits to
// edit. So `crates/cordial-runtime/src/bin/load.rs` carries a second, small
// implementation of the same *reasoning* -- keyring first, honest fallback,
// nothing ever printed -- under its own schema, reachable from here only
// through the four `cordial_local_storage_*` externs below. That module's own
// header says why it is not simply a third `Kind`.
//
// **Never make a stub lie.** A value that was never stored, or that the
// keyring would not hand back, comes back to the engine as `null`, not `""`.
// An empty string here is indistinguishable from "the value is empty" and the
// engine has no way to tell the two apart; AGENTS.md's rule about
// `opensles.cpp` reporting failure honestly instead of a dead success applies
// exactly as much to a credential as it does to an audio engine.

#include <jnivm.h>

#include <cstdio>
#include <cstdlib>
#include <cstdint>
#include <cstring>
#include <memory>
#include <mutex>
#include <set>
#include <string>
#include <vector>

namespace cordial {

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

jnivm::ENV* process_env();

// Every other file in this directory carries its own copy of this rather than
// sharing a header -- see the comment on `to_jni` in `init_params.cpp` for
// what it does that a raw `jnivm::JNITypes<...>::ToJNIType` call would skip
// silently (the class pointer, and keeping the object alive in the local
// frame). Kept identical here on purpose.
template <class T>
static auto to_jni(jnivm::ENV* env, const std::shared_ptr<T>& p) {
    return jnivm::JNITypes<std::shared_ptr<T>>::ToJNIType(env, p);
}

namespace {
std::shared_ptr<String> S(const char* v) {
    return std::make_shared<String>(std::string(v ? v : ""));
}
} // namespace

/// `CORDIAL_TRACE_LOCAL_STORAGE=1` -- did the engine ever call this interface?
///
/// `docs/analysis/flag-init.md` §40 established that handing the platform
/// implementation over stops the engine's "Not available on the current
/// platform" warning, and read that as the engine having accepted it. That is
/// evidence about the *registration*, not about any later call: an interface
/// can be accepted and then never used, and nothing in this file could tell
/// the two apart. This is the probe that can. Off by default, because the
/// ordinary client should not print a line every time the engine reads a
/// platform token.
///
/// **Never a value, never a user id**, per this file's header and
/// `secrets.rs`'s. Key names and byte counts only, which is the same line the
/// Rust side draws.
static bool trace_local_storage() {
    static const bool on = getenv("CORDIAL_TRACE_LOCAL_STORAGE") != nullptr;
    return on;
}

static void note_call(const char* method, const char* key) {
    if (!trace_local_storage()) {
        return;
    }
    if (key) {
        fprintf(stderr, "[intoxicated] local storage: %s key=%s\n", method, key);
    } else {
        fprintf(stderr, "[intoxicated] local storage: %s\n", method);
    }
}

// Defined in `android_classes.cpp`, beside `NativeUserJavaInterface` --
// `init_params.cpp` already forward-declares the same four for
// `StartAppParams`, with the comment explaining why this project has one
// source for "who is signed in" rather than a header. Only the two needed
// here are repeated.
jlong identity_user_id();
bool identity_known();

// The Rust side of the secure store. Declared here rather than pulled from a
// generated header for the reason every cross-language boundary in this
// directory is: `cordial-linker-sys`, which owns the usual generated bindings
// for `native/`, was off limits to the task that added this file, so these
// four symbols are declared directly against what
// `crates/cordial-runtime/src/bin/load.rs` exports with `#[no_mangle]`.
//
// Return codes, shared by all four: `0` succeeded (a "not found" read is a
// success that found nothing, not a failure); anything negative is a reason
// to answer the engine honestly rather than to guess. `cordial_local_storage_get`
// additionally distinguishes "absent" (`1` not set) from "present" (`1` set,
// `*out_len` gives the length written) so the caller never has to infer one
// from the other.
extern "C" {
int cordial_local_storage_get(long long user_id, const char* key, char* out, size_t out_cap,
                               int* found, size_t* out_len);
int cordial_local_storage_set(long long user_id, const char* key, const char* value,
                              size_t value_len);
int cordial_local_storage_delete(long long user_id, const char* key);
int cordial_local_storage_delete_user(long long user_id);
} // extern "C"

// ---------------------------------------------------------------------------
// Which user "current" means.
//
// The interface has three families of method -- plain, `...ForUser(long)` and
// `...ForCurrentUser` -- plus `deleteCurrentUserValues()`, which is not even
// given a user id to work with. All four have to agree on one answer, kept
// here rather than re-derived per call.
// ---------------------------------------------------------------------------

namespace {
std::mutex g_users_mutex;
jlong g_current_user = 0;
bool g_current_user_set = false;
std::set<jlong> g_known_users;

/// Seeds from the signed-in identity the first time anything asks, so local
/// storage's notion of "current user" and `NativeUserJavaInterface`'s cannot
/// name two different accounts. Caller holds `g_users_mutex`.
jlong current_user_locked() {
    if (!g_current_user_set) {
        g_current_user = identity_known() ? identity_user_id() : 0;
        g_current_user_set = true;
        if (g_current_user != 0) {
            g_known_users.insert(g_current_user);
        }
    }
    return g_current_user;
}
} // namespace

/// `java.lang.ref.WeakReference` and `java.lang.System.identityHashCode`.
///
/// **These are djinni's, not local storage's**, and they are the reason
/// `setPlatformImpl` crashed the process for as long as it was enabled.
/// `docs/analysis/flag-init.md` §39 inferred that libjnivm's own
/// `NewWeakGlobalRef` was returning null and djinni was asserting on that.
/// **That inference is wrong and §40 retracts it.** A `CORDIAL_JNI_TRACE=ON`
/// run with a print added to `NewWeakGlobalRef` shows it is never called at
/// all -- not once, in a run that throws `djinni (djinni_support.cpp:529):
/// weakRef` thirteen times. What the trace shows instead, in the last dozen
/// lines before the first throw:
///
///     FindClass java/lang/ref/WeakReference
///     Constructed Unresolved symbol, Class=`java/lang/ref/WeakReference`,
///       StaticMethod=`<init>`, Signature=`(Ljava/lang/Object;)Ljava/lang/ref/WeakReference;`
///     Constructed Unresolved symbol, Class=`java/lang/ref/WeakReference`,
///       Method=`get`, Signature=`()Ljava/lang/Object;`
///     Call Unknown Static Function Class=`java/lang/ref/WeakReference` Method=`<init>`
///     FindClass java/lang/Error
///
/// djinni does not use JNI weak global references for this. It builds a real
/// `java.lang.ref.WeakReference` **object** and keeps that, which is a class
/// libjnivm does not implement -- so its `<init>` was an invented stub, the
/// stub returned null, and `DJINNI_ASSERT(weakRef, ...)` failed on every
/// subsequent call into the interface. `FindClass java/lang/Error` on the very
/// next line is djinni fetching the class it is about to throw.
///
/// `System.identityHashCode` is in the same window and unresolved for the same
/// reason; djinni keys its proxy cache on it, and a stub returning 0 for every
/// object collapses that cache onto one bucket. Both are answered here.
///
/// **Why the reference is genuinely weak.** libjnivm has no collector, so a
/// `WeakReference` that held a strong pointer would never report its referent
/// gone and would pin every object djinni ever wrapped for the life of the
/// process. `std::weak_ptr` is the honest mapping: `get()` returns null exactly
/// when nothing else holds the object, which is what the Java class promises.
///
/// The pair lives in this file because this is where the engine first reached
/// for them and where the failure was measured. They belong to no interface in
/// particular -- any other djinni-generated surface will want them -- and if a
/// second one appears they should move somewhere neutral rather than be
/// duplicated.
class WeakReference : public Object {
public:
    std::weak_ptr<Object> referent;

    /// libjnivm rewrites an instance `<init>` lookup into a static one with
    /// the return type folded into the signature -- `NativeFlagsInitResult` in
    /// `init_params.cpp` carries the full explanation, and the trace line
    /// above shows the rewritten signature this has to match exactly.
    static std::shared_ptr<WeakReference> ctor(ENV* env, Class*, std::shared_ptr<Object> o) {
        auto p = std::make_shared<WeakReference>();
        p->referent = o;
        to_jni(env, p);
        return p;
    }

    std::shared_ptr<Object> get(ENV* env) {
        auto o = referent.lock();
        if (o) {
            to_jni(env, o);
        }
        return o;
    }

    static void Register(ENV* env) {
        env->GetClass<WeakReference>("java/lang/ref/WeakReference");
        auto c = env->GetClass("java/lang/ref/WeakReference");
        c->Hook(env, "<init>", &WeakReference::ctor);
        c->HookInstanceFunction(env, "get", &WeakReference::get);
    }
};

/// `java.lang.System`, only `identityHashCode`.
///
/// Java's contract is "distinct for distinct objects, stable for the life of
/// the object, 0 for null". The object address satisfies all three here
/// because libjnivm objects do not move. Shifted right by four because glibc
/// aligns every allocation to sixteen bytes, so the bottom four bits are
/// always zero and fifteen sixteenths of djinni's buckets would never be used
/// without the shift.
class SystemClass : public Object {
public:
    static jint identityHashCode(ENV*, Class*, std::shared_ptr<Object> o) {
        if (!o) {
            return 0;
        }
        auto bits = reinterpret_cast<uintptr_t>(o.get()) >> 4;
        return static_cast<jint>(bits ^ (bits >> 32));
    }

    static void Register(ENV* env) {
        env->GetClass<SystemClass>("java/lang/System");
        auto c = env->GetClass("java/lang/System");
        c->Hook(env, "identityHashCode", &SystemClass::identityHashCode);
    }
};

/// `java.util.HashSet`, enough of it for `getUsers()`'s return value.
///
/// **What this is not**: a real enumeration of every user this profile has
/// ever stored a value for. Building that would mean listing every item the
/// desktop keyring holds for this profile and reading each one's attributes
/// back, which is a second, heavier D-Bus round trip this call has never been
/// measured to need. What it reports instead is honest on its own narrower
/// terms -- the users Cordial has actually seen this run, seeded from the
/// signed-in identity and grown by `setCurrentUser`/`setSecureValueForUser` --
/// which for the account-per-profile shape this project runs is the same set
/// either way. `size()`/`isEmpty()` only, matching `JavaMap`'s and
/// `JavaList`'s "enough of it" in `init_params.cpp`; no iterator, because
/// nothing here has needed one to observe.
class UserSet : public Object {
public:
    std::set<jlong> ids;

    jint size(ENV*) { return static_cast<jint>(ids.size()); }
    jboolean isEmpty(ENV*) { return ids.empty(); }

    static void Register(ENV* env) {
        env->GetClass<UserSet>("java/util/HashSet");
        auto c = env->GetClass("java/util/HashSet");
        c->HookInstanceFunction(env, "size", &UserSet::size);
        c->HookInstanceFunction(env, "isEmpty", &UserSet::isEmpty);
    }
};

/// `com.roblox.protocols.localstorageplatforminterface.generated.IPlatformLocalStorageHandler`
///
/// One instance, constructed once by `cordial_local_storage_set_platform_impl`
/// below and held by the engine for the life of the process -- the same
/// lifetime `cookies.cpp`'s `g_handler` documents for the same reason: the
/// engine calls back into this from its own thread, long after the call that
/// registered it returned, so nothing here may be a local.
class PlatformLocalStorageHandler : public Object {
public:
    jlong getCurrentUser(ENV*) {
        note_call("getCurrentUser", nullptr);
        std::lock_guard<std::mutex> lock(g_users_mutex);
        return current_user_locked();
    }

    jboolean setCurrentUser(ENV*, jlong userId) {
        note_call("setCurrentUser", nullptr);
        std::lock_guard<std::mutex> lock(g_users_mutex);
        g_current_user = userId;
        g_current_user_set = true;
        g_known_users.insert(userId);
        return JNI_TRUE;
    }

    std::shared_ptr<UserSet> getUsers(ENV* env) {
        note_call("getUsers", nullptr);
        std::lock_guard<std::mutex> lock(g_users_mutex);
        current_user_locked();
        auto set = std::make_shared<UserSet>();
        set->ids = g_known_users;
        to_jni(env, set);
        return set;
    }

    std::shared_ptr<String> getSecureValue(ENV* env, std::shared_ptr<String> key) {
        note_call("getSecureValue", key ? std::string(*key).c_str() : nullptr);
        return read(env, current_user(), key);
    }
    std::shared_ptr<String> getSecureValueForCurrentUser(ENV* env, std::shared_ptr<String> key) {
        note_call("getSecureValueForCurrentUser", key ? std::string(*key).c_str() : nullptr);
        return read(env, current_user(), key);
    }
    std::shared_ptr<String> getSecureValueForUser(ENV* env, std::shared_ptr<String> key,
                                                  jlong userId) {
        note_call("getSecureValueForUser", key ? std::string(*key).c_str() : nullptr);
        return read(env, userId, key);
    }

    jboolean setSecureValue(ENV*, std::shared_ptr<String> key, std::shared_ptr<String> value) {
        note_call("setSecureValue", key ? std::string(*key).c_str() : nullptr);
        return write(current_user(), key, value);
    }
    jboolean setSecureValueForCurrentUser(ENV*, std::shared_ptr<String> key,
                                          std::shared_ptr<String> value) {
        note_call("setSecureValueForCurrentUser", key ? std::string(*key).c_str() : nullptr);
        return write(current_user(), key, value);
    }
    jboolean setSecureValueForUser(ENV*, std::shared_ptr<String> key,
                                   std::shared_ptr<String> value, jlong userId) {
        note_call("setSecureValueForUser", key ? std::string(*key).c_str() : nullptr);
        return write(userId, key, value);
    }

    jboolean deleteSecureValue(ENV*, std::shared_ptr<String> key) {
        note_call("deleteSecureValue", key ? std::string(*key).c_str() : nullptr);
        if (!key) {
            return JNI_FALSE;
        }
        jlong user = current_user();
        std::string k(*key);
        int rc = cordial_local_storage_delete(static_cast<long long>(user), k.c_str());
        return rc == 0 ? JNI_TRUE : JNI_FALSE;
    }

    jboolean deleteUserValues(ENV*, jlong userId) {
        note_call("deleteUserValues", nullptr);
        int rc = cordial_local_storage_delete_user(static_cast<long long>(userId));
        std::lock_guard<std::mutex> lock(g_users_mutex);
        g_known_users.erase(userId);
        return rc == 0 ? JNI_TRUE : JNI_FALSE;
    }

    jboolean deleteCurrentUserValues(ENV* env) {
        note_call("deleteCurrentUserValues", nullptr);
        return deleteUserValues(env, current_user());
    }

    static void Register(ENV* env) {
        const char* name =
            "com/roblox/protocols/localstorageplatforminterface/generated/"
            "IPlatformLocalStorageHandler";
        env->GetClass<PlatformLocalStorageHandler>(name);
        auto c = env->GetClass(name);
        c->HookInstanceFunction(env, "getCurrentUser", &PlatformLocalStorageHandler::getCurrentUser);
        c->HookInstanceFunction(env, "setCurrentUser", &PlatformLocalStorageHandler::setCurrentUser);
        c->HookInstanceFunction(env, "getUsers", &PlatformLocalStorageHandler::getUsers);
        c->HookInstanceFunction(env, "getSecureValue", &PlatformLocalStorageHandler::getSecureValue);
        c->HookInstanceFunction(env, "getSecureValueForCurrentUser",
                                &PlatformLocalStorageHandler::getSecureValueForCurrentUser);
        c->HookInstanceFunction(env, "getSecureValueForUser",
                                &PlatformLocalStorageHandler::getSecureValueForUser);
        c->HookInstanceFunction(env, "setSecureValue", &PlatformLocalStorageHandler::setSecureValue);
        c->HookInstanceFunction(env, "setSecureValueForCurrentUser",
                                &PlatformLocalStorageHandler::setSecureValueForCurrentUser);
        c->HookInstanceFunction(env, "setSecureValueForUser",
                                &PlatformLocalStorageHandler::setSecureValueForUser);
        c->HookInstanceFunction(env, "deleteSecureValue",
                                &PlatformLocalStorageHandler::deleteSecureValue);
        c->HookInstanceFunction(env, "deleteUserValues",
                                &PlatformLocalStorageHandler::deleteUserValues);
        c->HookInstanceFunction(env, "deleteCurrentUserValues",
                                &PlatformLocalStorageHandler::deleteCurrentUserValues);
    }

private:
    static jlong current_user() {
        std::lock_guard<std::mutex> lock(g_users_mutex);
        return current_user_locked();
    }

    /// A single fixed-size buffer rather than the usual libjnivm two-call
    /// growth protocol: these are credentials and small platform tokens, not
    /// documents, and `secrets.rs`'s own bodies (a cookie jar, a whole
    /// identity record) are smaller than this in ordinary operation. A value
    /// that does not fit is refused rather than truncated -- a truncated
    /// credential is a corrupt one, and corrupt is a worse answer than absent.
    static constexpr size_t kMaxValue = 8192;

    static std::shared_ptr<String> read(ENV*, jlong userId, const std::shared_ptr<String>& key) {
        if (!key) {
            return nullptr;
        }
        std::string k(*key);
        std::vector<char> buf(kMaxValue);
        int found = 0;
        size_t len = 0;
        int rc = cordial_local_storage_get(static_cast<long long>(userId), k.c_str(), buf.data(),
                                           buf.size(), &found, &len);
        if (rc != 0 || !found) {
            // Absent, a locked keyring, a value too large for `kMaxValue`, or
            // any other reason the store could not answer -- all of them are
            // "nothing stored" to the engine. `crates/cordial-runtime/src/bin/
            // load.rs` is where the distinction is logged; nothing here lies
            // about which one happened by returning "" for any of them.
            return nullptr;
        }
        if (len > buf.size()) {
            len = buf.size();
        }
        return std::make_shared<String>(std::string(buf.data(), len));
    }

    static jboolean write(jlong userId, const std::shared_ptr<String>& key,
                          const std::shared_ptr<String>& value) {
        if (!key || !value) {
            return JNI_FALSE;
        }
        std::string k(*key);
        std::string v(*value);
        if (v.size() > kMaxValue) {
            return JNI_FALSE;
        }
        int rc = cordial_local_storage_set(static_cast<long long>(userId), k.c_str(), v.c_str(),
                                           v.size());
        return rc == 0 ? JNI_TRUE : JNI_FALSE;
    }
};

/// The descriptors this file actually registered, printed under
/// `CORDIAL_TRACE_LOCAL_STORAGE=1`.
///
/// Not decoration. libjnivm derives every hook's signature from the C++ types
/// of the function it is handed, by way of a `typeid`-keyed map that any other
/// registration can move out from under it -- which is exactly the bug §40
/// found, where `jnivm::Object` was left pointing at `android/app/Application`
/// and every `Object`-taking hook here registered under a descriptor the
/// engine would never ask for. **That failure is silent in both directions**:
/// the JNI trace prints the descriptor the *engine* asked for, which is always
/// the right one, and nothing prints what the registered side spelled. This
/// does. Compare it against `tools/dex_method.py --class
/// localstorageplatforminterface`, which is the ground truth.
static void dump_registered(ENV* env, const char* name) {
    if (!trace_local_storage()) {
        return;
    }
    auto c = env->GetClass(name);
    if (!c) {
        fprintf(stderr, "[intoxicated] local storage: no class %s\n", name);
        return;
    }
    for (auto& m : c->methods) {
        fprintf(stderr, "[intoxicated] local storage: registered %s.%s%s%s\n", name, m->name.data(),
                m->signature.data(), m->_static ? "  (static)" : "");
    }
}

/// `com.roblox.protocols.localstorageplatforminterface.generated.ILocalStorageHandlerCore$CppProxy`
///
/// **The reverse direction of the djinni scaffolding, registered only so a
/// null answer can be read.** `setPlatformImpl` returns an
/// `ILocalStorageHandlerCore`, and under Cordial it returns null every run --
/// measured, with the return value printed rather than discarded as it was
/// until now. Two things produce that null and they mean opposite things:
/// djinni's `fromCpp` hands back `nullptr` without touching JNI when the C++
/// side gave it no core, and libjnivm hands back whatever it invents when
/// `NewObject` names a class nobody implemented. This class separates them.
/// If the constructor below is reached, the engine built a core and the null
/// was libjnivm's; if it is never reached, the engine built none.
///
/// `nativeRef` exists because djinni's `JniInterface` constructor looks the
/// field up on the proxy class and asserts on a null id. libjnivm invents
/// field ids rather than failing, so the assert never fired -- but a real
/// field is the honest shape and costs nothing.
class LocalStorageHandlerCoreProxy : public Object {
public:
    jlong nativeRef = 0;

    static std::shared_ptr<LocalStorageHandlerCoreProxy> ctor(ENV* env, Class*, jlong ref) {
        auto p = std::make_shared<LocalStorageHandlerCoreProxy>();
        p->nativeRef = ref;
        to_jni(env, p);
        if (trace_local_storage()) {
            fprintf(stderr, "[intoxicated] local storage: ILocalStorageHandlerCore$CppProxy built\n");
        }
        return p;
    }

    static void Register(ENV* env) {
        const char* name =
            "com/roblox/protocols/localstorageplatforminterface/generated/"
            "ILocalStorageHandlerCore$CppProxy";
        env->GetClass<LocalStorageHandlerCoreProxy>(name);
        auto c = env->GetClass(name);
        c->Hook(env, "<init>", &LocalStorageHandlerCoreProxy::ctor);
        c->HookInstanceProperty(env, "nativeRef", &LocalStorageHandlerCoreProxy::nativeRef);
    }
};

void register_local_storage_classes(ENV* env) {
    WeakReference::Register(env);
    SystemClass::Register(env);
    UserSet::Register(env);
    PlatformLocalStorageHandler::Register(env);
    LocalStorageHandlerCoreProxy::Register(env);
    dump_registered(env,
                    "com/roblox/protocols/localstorageplatforminterface/generated/"
                    "IPlatformLocalStorageHandler");
    dump_registered(env, "java/lang/ref/WeakReference");
    dump_registered(env, "java/lang/System");
    dump_registered(env, "java/util/HashSet");
}

} // namespace cordial

extern "C" {

/// `ILocalStorageHandlerCore.setPlatformImpl(IPlatformLocalStorageHandler)
///   -> ILocalStorageHandlerCore`
///
/// A static native -- confirmed by mocktail's own call, which passes the
/// *class* object as the receiver rather than constructing a core instance
/// first (`ConfigureLocalStorage` in `legacy_runtime.cc`; see this file's
/// header for what "confirmed" means here). Cordial builds the one argument
/// the engine actually reads from and discards the `ILocalStorageHandlerCore`
/// this returns -- nothing here has found a reason to call anything on it.
///
/// **This crashed the process until the `WeakReference` and
/// `System.identityHashCode` classes above existed, and it no longer does.**
/// The old text here, and `docs/analysis/flag-init.md` §39, both blamed
/// libjnivm's `NewWeakGlobalRef` returning null. Both were wrong: a trace run
/// with a print inside that function shows it is never called, once, in a run
/// that throws `djinni (djinni_support.cpp:529): weakRef` thirteen times. The
/// class comment on `WeakReference` above has the trace lines that say what
/// actually happened. Three runs with this call made and three with it skipped,
/// same build and separate profile roots, now exit 0 with no djinni exception
/// either way, and the engine's `FLog::LocalStorageHandler` `Not available on
/// the current platform` warning appears twice a run when it is skipped and
/// not at all when it is made -- so the implementation is reaching the engine
/// rather than merely failing quietly. §40 records it.
///
/// It does not produce an `rbx-storage.db`; see §40's closing note.
int cordial_local_storage_set_platform_impl(void* fn, char* err, size_t err_len) {
    using Call = jobject (*)(JNIEnv*, jclass, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or setPlatformImpl is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass(
            "com/roblox/protocols/localstorageplatforminterface/generated/ILocalStorageHandlerCore");
        auto handler = std::make_shared<cordial::PlatformLocalStorageHandler>();
        cordial::to_jni(env, handler);
        jobject core = reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                                  (jclass)cordial::to_jni(env, cls),
                                                  (jobject)cordial::to_jni(env, handler));
        // The return was discarded until now and the discard hid a fact worth
        // having: djinni's `setPlatformImpl` returns the core it built, so a
        // null here means the engine declined to build one and everything
        // downstream is moot. Reported rather than acted on -- nothing has
        // found a use for the object itself.
        // A Java exception left pending here would be djinni's, thrown out of
        // the call and never looked at by anybody -- which is how §40's
        // `weakRef` failure went unnoticed for as long as it did. Cleared as
        // well as reported: libjnivm keeps it on the ENV, and the next thing
        // to make a JNI call on this thread would otherwise inherit it.
        auto* jni = env->GetJNIEnv();
        bool pending = jni->ExceptionCheck() == JNI_TRUE;
        if (pending) {
            jni->ExceptionDescribe();
            jni->ExceptionClear();
        }
        if (getenv("CORDIAL_TRACE_LOCAL_STORAGE")) {
            fprintf(stderr, "[intoxicated] local storage: setPlatformImpl returned %s%s\n",
                    core ? "a core" : "null", pending ? ", exception pending" : "");
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

/// `NativeInputInterface.nativeUpdateScreenOrientation(I)V`
///
/// The descriptor is read from the dex with `dexproto.py`, not guessed --
/// `docs/analysis/flag-init.md` §16 records the class as the one call
/// mocktail makes between `initializeNativeCode` and the settings handshake
/// that Cordial did not. It is otherwise unrelated to the storage classes
/// above; both live here because the task that added them could only create
/// one new `.cpp` file (see the header).
///
/// The two orientation values are Android's own `Configuration.ORIENTATION_*`
/// constants -- `init_params.cpp`'s `Configuration` class already reports the
/// same two numbers through `getConfiguration().orientation`, derived from the
/// same width/height comparison. Duplicated here rather than shared through a
/// header, which is this directory's usual reason: one header for two
/// constants would be the only one in `native/`.
int cordial_update_screen_orientation(void* fn, int width, int height, char* err, size_t err_len) {
    constexpr jint kOrientationPortrait = 1;
    constexpr jint kOrientationLandscape = 2;
    using Call = void (*)(JNIEnv*, jclass, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeUpdateScreenOrientation is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        jint orientation = width >= height ? kOrientationLandscape : kOrientationPortrait;
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jclass)cordial::to_jni(env, cls), orientation);
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
