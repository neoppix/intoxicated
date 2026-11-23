#pragma once

// The Java classes more than one of jni_framework.cpp, jni_params.cpp and
// activity.cpp hand to the engine, and the few helpers all three share.
//
// How libjnivm shapes this (third_party/libjnivm/include/jnivm):
//
// - A hook's JNI descriptor is derived from its C++ signature, and a class
//   type in it is spelled from the Java name that C++ type is bound to
//   (`ENV::GetClass<T>(name)`). So every type here is bound by
//   `declare_shared_classes` before any hook that mentions it is registered,
//   and each Java class has its own C++ type: binding a second name to a type
//   moves the binding and changes every descriptor derived from it later.
// - A typed receiver is checked with `dynamic_cast`; an object of another
//   type raises inside libjnivm. Hooks that may be called on objects this
//   code did not make (an Application, a GameActivity reached through
//   MainGameActivity) take a `jnivm::Object*` receiver instead.
// - An object handed to the engine must carry its class and live in the
//   current local frame, which `to_jni` does.

#include <jnivm.h>

#include <memory>
#include <string>

#include "jni_values.h"

/// This thread's libjnivm ENV, attaching the thread first if needed; null
/// before the JavaVM exists (java_vm.cpp).
namespace intoxicated {
jnivm::ENV* process_env();
}

namespace intoxicated {
namespace java {

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

/// Bind an object to its class and put it in the current local frame.
template <class T>
auto to_jni(ENV* env, const std::shared_ptr<T>& p) {
    return jnivm::JNITypes<std::shared_ptr<T>>::ToJNIType(env, p);
}

inline std::shared_ptr<String> jstr(const std::string& s) {
    return std::make_shared<String>(s);
}

/// The text of a Java string argument; null is empty.
inline std::string text_of(const std::shared_ptr<String>& s) {
    return s ? std::string(*s) : std::string();
}

/// A local frame for the length of a scope, popped however it ends.
class LocalFrame {
public:
    LocalFrame(ENV* env, int capacity) : jni_(env->GetJNIEnv()) {
        jni_->PushLocalFrame(capacity);
    }
    ~LocalFrame() {
        jni_->PopLocalFrame(nullptr);
    }
    LocalFrame(const LocalFrame&) = delete;
    LocalFrame& operator=(const LocalFrame&) = delete;

private:
    JNIEnv* jni_;
};

// ------------------------------------------------------------ shared classes

/// `android.app.Activity`, as `vrContext` and as the receiver of the context
/// methods. Stateless.
class Activity final : public Object {};

/// `android.view.Surface`. Stateless: the engine renders into the
/// ANativeWindow, never into this object.
class Surface final : public Object {};

/// `android.content.res.AssetManager`. Stateless: native code reaches assets
/// through the NDK asset manager.
class AssetManager final : public Object {};

/// `android.graphics.Point`.
class Point final : public Object {
public:
    jint x = 0;
    jint y = 0;
};

/// `androidx.core.graphics.Insets`. Zero on every side is the true answer for
/// a desktop window: no status bar, navigation bar, cutout or gesture area.
class Insets final : public Object {
public:
    jint left = 0;
    jint top = 0;
    jint right = 0;
    jint bottom = 0;
};

/// `android.util.DisplayMetrics`.
class DisplayMetrics final : public Object {
public:
    jfloat density = 1.0f;
    jfloat scaledDensity = 1.0f;
    jfloat xdpi = 96.0f;
    jfloat ydpi = 96.0f;
    jint densityDpi = 160;
    jint widthPixels = 0;
    jint heightPixels = 0;
};

/// `android.content.res.Configuration`. Every field reads zero: the one
/// instance the engine is given (the `config` argument of
/// `initializeNativeCode`) carries no values, as it always has. What a
/// Configuration carrying the real ones would change is an open question for
/// the owner, not a like-for-like replacement.
class Configuration final : public Object {
public:
    jfloat fontScale = 0;
    jint densityDpi = 0;
    jint screenWidthDp = 0;
    jint screenHeightDp = 0;
    jint smallestScreenWidthDp = 0;
    jint orientation = 0;
    jint screenLayout = 0;
    jint uiMode = 0;
    jint colorMode = 0;
    jint touchscreen = 0;
    jint keyboard = 0;
    jint keyboardHidden = 0;
    jint hardKeyboardHidden = 0;
    jint navigation = 0;
    jint navigationHidden = 0;
    jint mcc = 0;
    jint mnc = 0;
    jint fontWeightAdjustment = 0;
};

/// `com.roblox.engine.jni.model.DeviceStaticParams`.
class DeviceStaticParams final : public Object {
public:
    std::shared_ptr<String> osVersion, deviceName, manufacturer, deviceSku, socModel, appBuildVariant, appVersion;
    jboolean cpu64Bit = JNI_TRUE;
};

/// `com.roblox.engine.jni.model.NativeTextBoxInfo`: the fifteen constructor
/// values, kept so `showKeyboard` can tell which box it is for. `known` is
/// false for an object that did not come through the constructor.
class NativeTextBoxInfo final : public Object {
public:
    TextBoxSpec spec{};
    bool known = false;
};

/// A parameter of type `T` that does not insist on being one. Its descriptor
/// is `T`'s, but an argument of another C++ type arrives as null instead of
/// raising inside libjnivm, which would turn the whole call into a pending
/// exception. For arguments the engine may build some other way than through
/// a constructor registered here.
template <class T>
struct Lenient {
    std::shared_ptr<T> object;
};

// ------------------------------------------------------------ names

constexpr const char* kActivity = "android/app/Activity";
constexpr const char* kSurface = "android/view/Surface";
constexpr const char* kAssetManager = "android/content/res/AssetManager";
constexpr const char* kPoint = "android/graphics/Point";
constexpr const char* kInsets = "androidx/core/graphics/Insets";
constexpr const char* kDisplayMetrics = "android/util/DisplayMetrics";
constexpr const char* kConfiguration = "android/content/res/Configuration";
constexpr const char* kDeviceStaticParams = "com/roblox/engine/jni/model/DeviceStaticParams";
constexpr const char* kNativeTextBoxInfo = "com/roblox/engine/jni/model/NativeTextBoxInfo";
constexpr const char* kGameActivity = "com/google/androidgamesdk/GameActivity";
constexpr const char* kMainGameActivity = "com/roblox/client/startup/MainGameActivity";

/// Bind every class above to its Java name. Idempotent (a class is
/// get-or-create), so each registration entry point calls it before
/// deriving any descriptor that names one of them. Hooks on these classes
/// are registered once, by the file that owns each.
void declare_shared_classes(ENV* env);

/// The three registration entry points, run in this order when the JavaVM is
/// created (java_vm.cpp).
void register_framework_classes(ENV* env);
void register_activity_classes(ENV* env);
void register_param_classes(ENV* env);

/// A new object of a type with a default constructor. It is bound to its
/// class when it is handed over: by `to_jni` as an argument, or by libjnivm
/// as a hook's return value or a field's value. Binding it here as well
/// would put it in the local frame twice, and a `DeleteLocalRef` from the
/// engine removes only one entry.
template <class T>
std::shared_ptr<T> make(ENV*) {
    return std::make_shared<T>();
}

/// An object of the class `cls` with no C++ state of its own, for receivers
/// and arguments the engine only needs to be of the right class.
std::shared_ptr<Object> instance_of(ENV* env, jclass cls);

/// A class by name through JNI's own `FindClass`, which takes the VM's lock
/// (`ENV::GetClass` does not, and the engine looks classes up from its own
/// threads). Never null: libjnivm creates an unknown class.
jclass find_class(ENV* env, const char* name);

/// What kind of member `has_hooked` looks for.
enum class Member { Method, StaticMethod, Field, StaticField };

/// True when the class has that member with exactly that descriptor and a
/// hook behind it, which is what the engine's exact-match lookup will find.
bool has_hooked(const std::shared_ptr<Class>& cls, Member kind, const char* name, const char* descriptor);

/// Warn on stderr when `has_hooked` is false. A hook that binds under a
/// descriptor nobody asks for fails silently otherwise, which is how two of
/// them went unnoticed for months.
void expect(const std::shared_ptr<Class>& cls, Member kind, const char* name, const char* descriptor);

/// Write `message` into the host's error buffer.
void report(char* err, size_t err_len, const char* message);

/// Run one host-callable operation: 0 or the body's own code, and -1 with the
/// text of any C++ exception in `err`. Nothing unwinds into the host.
template <class F>
int contained(char* err, size_t err_len, F&& body) {
    try {
        return body();
    } catch (const std::exception& e) {
        report(err, err_len, e.what());
    } catch (...) {
        report(err, err_len, "non-standard C++ exception");
    }
    return -1;
}

/// The object behind a reference, through the global and weak wrappers
/// libjnivm puts around references the engine keeps, if it is a `T`.
template <class T>
std::shared_ptr<T> object_as(jobject ref) {
    auto* object = reinterpret_cast<Object*>(ref);
    for (int depth = 0; object && depth < 4; ++depth) {
        if (auto* global = dynamic_cast<jnivm::Global*>(object)) {
            object = global->wrapped.get();
        } else if (auto* weak = dynamic_cast<jnivm::Weak*>(object)) {
            auto strong = weak->wrapped.lock();
            object = strong.get();
            if (auto* t = dynamic_cast<T*>(object)) {
                return std::shared_ptr<T>(strong, t);
            }
            return nullptr;
        } else {
            break;
        }
    }
    auto* t = object ? dynamic_cast<T*>(object) : nullptr;
    if (!t) {
        return nullptr;
    }
    return std::shared_ptr<T>(t->shared_from_this(), t);
}

} // namespace java
} // namespace intoxicated

namespace jnivm {
template <class T>
struct JNITypes<intoxicated::java::Lenient<T>> {
    static std::string GetJNISignature(ENV* env) {
        return JNITypes<std::shared_ptr<T>>::GetJNISignature(env);
    }
    static intoxicated::java::Lenient<T> JNICast(ENV*, const jvalue& v) {
        return {intoxicated::java::object_as<T>(v.l)};
    }
};
} // namespace jnivm
