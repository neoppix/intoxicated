// AGDK's GameActivity, driven without Android: the Java methods the engine
// and AGDK call on the activity, the event objects AGDK reads back, the text
// input pair, and the host operations that call the natives the engine
// registered on `com/google/androidgamesdk/GameActivity`, plus the engine's
// own input natives on NativeGLInterface and NativeInputInterface.
//
// The second registration entry point. Of AGDK's seventeen natives only
// `initializeNativeCode` is an exported symbol; it registers the other
// sixteen with RegisterNatives, and they are found here by name in the
// class's natives table, under the class's lock.
//
// Threads: `bootstrapTheApp` arrives on one of the engine's threads, after
// `initializeNativeCode` has returned, while the host is still opening the
// window and calling into the engine from its own thread. The hook pointer is
// atomic (jni_values.cpp), the shared objects here are created once under a
// lock, and every host operation works in its own thread's JNI environment.

#include "jni_world.h"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

namespace intoxicated {
namespace java {
namespace {

constexpr const char* kMotionEvent = "android/view/MotionEvent";
constexpr const char* kKeyEvent = "android/view/KeyEvent";
constexpr const char* kState = "com/google/androidgamesdk/gametextinput/State";
constexpr const char* kInputConnection = "com/google/androidgamesdk/gametextinput/InputConnection";
constexpr const char* kNativeGL = "com/roblox/engine/jni/NativeGLInterface";
constexpr const char* kNativeInput = "com/roblox/engine/jni/NativeInputInterface";

// Android's constants, from MotionEvent, InputDevice and KeyEvent.
constexpr jint kSourceMouse = 0x2002;
constexpr jint kSourceTouchscreen = 0x1002;
constexpr jint kSourceKeyboard = 0x101;
constexpr jint kToolFinger = 1;
constexpr jint kToolMouse = 3;
constexpr jint kActionScroll = 8;
constexpr jint kAxisX = 0;
constexpr jint kAxisY = 1;
constexpr jint kAxisVScroll = 9;
constexpr jint kAxisHScroll = 10;

class GameActivity final : public Object {};

struct Contact {
    jint id;
    jfloat x;
    jfloat y;
};

/// One delivery's MotionEvent. AGDK reads every value back off it.
class MotionEvent final : public Object {
public:
    jint source = kSourceMouse;
    jint tool = kToolMouse;
    jint action = 0;
    jint button_state = 0;
    jint action_button = 0;
    jlong event_time = 0;
    jlong down_time = 0;
    jfloat vscroll = 0;
    jfloat hscroll = 0;
    std::vector<Contact> contacts;

    /// An index out of range answers for contact 0, never for nothing.
    const Contact& at(jint index) const {
        return index >= 0 && static_cast<size_t>(index) < contacts.size() ? contacts[static_cast<size_t>(index)]
                                                                           : contacts.front();
    }

    jint getPointerId(jint i) { return at(i).id; }
    jint getToolType(jint) { return tool; }
    jfloat getRawX(jint i) { return at(i).x; }
    jfloat getRawY(jint i) { return at(i).y; }
    jfloat getXPrecision() { return 1.0f; }
    jfloat getYPrecision() { return 1.0f; }
    jfloat getAxisValue(jint axis, jint i) {
        switch (axis) {
        case kAxisX:
            return at(i).x;
        case kAxisY:
            return at(i).y;
        case kAxisVScroll:
            return vscroll;
        case kAxisHScroll:
            return hscroll;
        default:
            return 0.0f;
        }
    }
    jlong getHistoricalEventTime(jint) { return event_time; }
    jfloat getHistoricalAxisValue(jint, jint, jint) { return 0.0f; }
    jint getDeviceId() { return 1; }
    jint getSource() { return source; }
    jint getAction() { return action; }
    jlong getEventTime() { return event_time; }
    jlong getDownTime() { return down_time; }
    jint getFlags() { return 0; }
    jint getMetaState() { return 0; }
    jint getActionButton() { return action_button; }
    jint getButtonState() { return button_state; }
    jint getClassification() { return 0; }
    jint getEdgeFlags() { return 0; }
    jint getHistorySize() { return 0; }
    jint getPointerCount() { return static_cast<jint>(contacts.size()); }
};

class KeyEvent final : public Object {
public:
    jint action = 0;
    jint meta_state = 0;
    jint repeat_count = 0;
    jint key_code = 0;
    jint scan_code = 0;
    jint unicode_char = 0;
    jlong event_time = 0;
    jlong down_time = 0;

    jint getDeviceId() { return 1; }
    jint getSource() { return kSourceKeyboard; }
    jint getAction() { return action; }
    jlong getEventTime() { return event_time; }
    jlong getDownTime() { return down_time; }
    jint getFlags() { return 0; }
    jint getMetaState() { return meta_state; }
    jint getModifiers() { return meta_state; }
    jint getRepeatCount() { return repeat_count; }
    jint getKeyCode() { return key_code; }
    jint getScanCode() { return scan_code; }
    jint getUnicodeChar() { return unicode_char; }
};

/// GameTextInput's State: the field's text, selection and composing region.
class State final : public Object {
public:
    std::shared_ptr<String> text = jstr("");
    jint selectionStart = 0;
    jint selectionEnd = 0;
    jint composingRegionStart = -1;
    jint composingRegionEnd = -1;
};

/// The one InputConnection, through which GameTextInput reports what the
/// engine's text field holds.
class InputConnection final : public Object {
public:
    void setState(Lenient<State> state) {
        if (!state.object) {
            return;
        }
        const State& s = *state.object;
        const std::string text = text_of(s.text);
        ime_set_state(text, s.selectionStart, s.selectionEnd, s.composingRegionStart, s.composingRegionEnd);
        if (trace_text()) {
            fprintf(stderr, "[text] InputConnection.setState: %zu bytes, selection %d..%d, composing %d..%d\n",
                    text.size(), s.selectionStart, s.selectionEnd, s.composingRegionStart, s.composingRegionEnd);
        }
    }
    void setSoftKeyboardActive(jboolean active, jint flags) {
        ime_set_active(active != JNI_FALSE);
        if (trace_text()) {
            fprintf(stderr, "[text] InputConnection.setSoftKeyboardActive: %s (flags %d)\n", active ? "on" : "off",
                    flags);
        }
    }
    void restartInput() {
        ime_restart();
        if (trace_text()) {
            fprintf(stderr, "[text] InputConnection.restartInput\n");
        }
    }
};

// ------------------------------------------------------------ shared objects

std::shared_ptr<Class> g_activity_class;

std::mutex g_objects_mutex;
std::shared_ptr<GameActivity> g_activity;
std::shared_ptr<InputConnection> g_input_connection;
/// What `initializeNativeCode` was given, kept for the life of the process.
std::vector<std::shared_ptr<Object>> g_init_objects;
/// The Surface of the current surface lifetime, from onSurfaceCreatedNative
/// to onSurfaceDestroyedNative.
std::shared_ptr<Surface> g_surface;

/// The receiver of every native but initializeNativeCode.
std::shared_ptr<GameActivity> shared_activity() {
    std::lock_guard<std::mutex> lock(g_objects_mutex);
    if (!g_activity) {
        g_activity = std::make_shared<GameActivity>();
    }
    return g_activity;
}

std::shared_ptr<InputConnection> input_connection() {
    std::lock_guard<std::mutex> lock(g_objects_mutex);
    if (!g_input_connection) {
        g_input_connection = std::make_shared<InputConnection>();
    }
    return g_input_connection;
}

std::shared_ptr<Surface> current_surface() {
    std::lock_guard<std::mutex> lock(g_objects_mutex);
    return g_surface;
}

void set_surface(std::shared_ptr<Surface> surface) {
    std::lock_guard<std::mutex> lock(g_objects_mutex);
    g_surface = std::move(surface);
}

/// A native the engine registered on GameActivity, or null.
void* activity_native(const char* name) {
    const std::shared_ptr<Class> cls = g_activity_class;
    if (!cls) {
        return nullptr;
    }
    std::lock_guard<std::mutex> lock(cls->mtx);
    auto it = cls->natives.find(name);
    return it == cls->natives.end() ? nullptr : it->second;
}

bool flag_set(const char* name) {
    return getenv(name) != nullptr;
}

/// Return codes of the handle-based operations.
constexpr int kNotRegistered = -2;

/// The checks every handle-based operation starts with. Null env means the
/// caller returns -1 with the message already written.
ENV* usable(int64_t handle, char* err, size_t err_len) {
    ENV* env = intoxicated::process_env();
    if (!env) {
        report(err, err_len, "no JavaVM");
        return nullptr;
    }
    if (handle == 0) {
        report(err, err_len, "no GameActivity handle (initializeNativeCode has not returned one)");
        return nullptr;
    }
    return env;
}

int not_registered(const char* name, char* err, size_t err_len) {
    char message[160];
    snprintf(message, sizeof(message), "%s is not registered yet", name);
    report(err, err_len, message);
    return kNotRegistered;
}

using HandleNative = void (*)(JNIEnv*, jobject, jlong);
using SurfaceNative = void (*)(JNIEnv*, jobject, jlong, jobject);
using SurfaceChangedNative = void (*)(JNIEnv*, jobject, jlong, jobject, jint, jint, jint);
using RectNative = void (*)(JNIEnv*, jobject, jlong, jint, jint, jint, jint);
using FocusNative = void (*)(JNIEnv*, jobject, jlong, jboolean);
using TouchNative = jboolean (*)(JNIEnv*, jobject, jlong, jobject, jint, jint, jint, jint, jint, jlong, jlong, jint,
                                 jint, jint, jint, jint, jint, jfloat, jfloat);
using KeyNative = jboolean (*)(JNIEnv*, jobject, jlong, jobject);
using ObjectNative = void (*)(JNIEnv*, jobject, jlong, jobject);

/// One MotionEvent through onTouchEventNative, in its own local frame, with
/// every scalar argument read back off the event as AGDK's Java side does.
int deliver_motion(int64_t handle, const std::shared_ptr<MotionEvent>& event, int* consumed, char* err,
                   size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    auto native = reinterpret_cast<TouchNative>(activity_native("onTouchEventNative"));
    if (!native) {
        return not_registered("onTouchEventNative", err, err_len);
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        MotionEvent& e = *event;
        const jboolean r = native(env->GetJNIEnv(), to_jni(env, shared_activity()), handle, to_jni(env, event),
                                  e.getPointerCount(), e.getHistorySize(), e.getDeviceId(), e.getSource(),
                                  e.getAction(), e.getEventTime(), e.getDownTime(), e.getFlags(), e.getMetaState(),
                                  e.getActionButton(), e.getButtonState(), e.getClassification(), e.getEdgeFlags(),
                                  e.getXPrecision(), e.getYPrecision());
        if (consumed) {
            *consumed = r ? 1 : 0;
        }
        return 0;
    });
}

// ------------------------------------------------------------ registration

void register_game_activity(ENV* env) {
    env->GetClass<GameActivity>(kGameActivity);
    g_activity_class = env->GetClass(kGameActivity);
    auto c = g_activity_class;
    {
        // Declared before anything returns it, fields hooked once.
        auto insets = env->GetClass(kInsets);
        insets->HookInstance(env, "left", &Insets::left);
        insets->HookInstance(env, "top", &Insets::top);
        insets->HookInstance(env, "right", &Insets::right);
        insets->HookInstance(env, "bottom", &Insets::bottom);
    }
    // Receivers are generic: AGDK calls these on the object it was given, but
    // the engine may reach them through MainGameActivity's name.
    c->HookInstanceFunction(env, "setImeEditorInfoFields", [](ENV*, Object*, jint, jint, jint) {});
    c->HookInstanceFunction(env, "setWindowFlags", [](ENV*, Object*, jint, jint) {});
    // A desktop window has nothing to inset by, whatever the mask.
    c->HookInstanceFunction(env, "getWindowInsets",
                            [](ENV* env, Object*, jint) -> std::shared_ptr<Insets> { return make<Insets>(env); });
    c->HookInstanceFunction(env, "getWaterfallInsets",
                            [](ENV* env, Object*) -> std::shared_ptr<Insets> { return make<Insets>(env); });
    c->HookInstanceFunction(env, "bootstrapTheApp", [](ENV*, Object*) {
        if (BareSink hook = bootstrap_hook()) {
            hook();
        } else {
            fprintf(stderr, "[intoxicated] bootstrapTheApp: no bootstrap is installed, so the engine's flags "
                            "are never delivered and it will report onFlagsFailed\n");
        }
    });
    expect(c, Member::Method, "getWindowInsets", "(I)Landroidx/core/graphics/Insets;");
    expect(c, Member::Method, "getWaterfallInsets", "()Landroidx/core/graphics/Insets;");
    expect(c, Member::Method, "bootstrapTheApp", "()V");
}

void register_events(ENV* env) {
    env->GetClass<MotionEvent>(kMotionEvent);
    {
        auto c = env->GetClass(kMotionEvent);
#define M(name) c->HookInstanceFunction(env, #name, &MotionEvent::name)
        M(getPointerId);
        M(getToolType);
        M(getRawX);
        M(getRawY);
        M(getXPrecision);
        M(getYPrecision);
        M(getAxisValue);
        M(getHistoricalEventTime);
        M(getHistoricalAxisValue);
        M(getDeviceId);
        M(getSource);
        M(getAction);
        M(getEventTime);
        M(getDownTime);
        M(getFlags);
        M(getMetaState);
        M(getActionButton);
        M(getButtonState);
        M(getClassification);
        M(getEdgeFlags);
        M(getHistorySize);
        M(getPointerCount);
#undef M
    }
    env->GetClass<KeyEvent>(kKeyEvent);
    {
        auto c = env->GetClass(kKeyEvent);
#define M(name) c->HookInstanceFunction(env, #name, &KeyEvent::name)
        M(getDeviceId);
        M(getSource);
        M(getAction);
        M(getEventTime);
        M(getDownTime);
        M(getFlags);
        M(getMetaState);
        M(getModifiers);
        M(getRepeatCount);
        M(getKeyCode);
        M(getScanCode);
        M(getUnicodeChar);
#undef M
    }
    env->GetClass<State>(kState);
    {
        auto c = env->GetClass(kState);
        c->HookInstance(env, "text", &State::text);
        c->HookInstance(env, "selectionStart", &State::selectionStart);
        c->HookInstance(env, "selectionEnd", &State::selectionEnd);
        c->HookInstance(env, "composingRegionStart", &State::composingRegionStart);
        c->HookInstance(env, "composingRegionEnd", &State::composingRegionEnd);
    }
    env->GetClass<InputConnection>(kInputConnection);
    {
        auto c = env->GetClass(kInputConnection);
        c->HookInstanceFunction(env, "setState", &InputConnection::setState);
        c->HookInstanceFunction(env, "setSoftKeyboardActive", &InputConnection::setSoftKeyboardActive);
        c->HookInstanceFunction(env, "restartInput", &InputConnection::restartInput);
        expect(c, Member::Method, "setState", "(Lcom/google/androidgamesdk/gametextinput/State;)V");
    }
}

// ------------------------------------------------------------ input natives

/// A class that is looked up once and then handed over by reference: the
/// input natives run for every mouse movement.
std::shared_ptr<Class> class_named(ENV* env, const char* name) {
    return jnivm::JNITypes<std::shared_ptr<Class>>::JNICast(env, find_class(env, name));
}

/// The shared shape of the engine's own input natives: static, the declaring
/// class as receiver, one local frame per call.
template <class F>
int input_call(void* fn, const char* class_name, char* err, size_t err_len, F&& body) {
    ENV* env = intoxicated::process_env();
    if (!fn || !env) {
        report(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        static std::mutex mutex;
        static std::vector<std::pair<std::string, std::shared_ptr<Class>>> cache;
        std::shared_ptr<Class> cls;
        {
            std::lock_guard<std::mutex> lock(mutex);
            for (const auto& [name, c] : cache) {
                if (name == class_name) {
                    cls = c;
                }
            }
            if (!cls) {
                cls = class_named(env, class_name);
                cache.emplace_back(class_name, cls);
            }
        }
        body(env, env->GetJNIEnv(), reinterpret_cast<jclass>(to_jni(env, cls)));
        return 0;
    });
}

jstring jtext(ENV* env, const char* s) {
    return to_jni(env, jstr(s ? s : ""));
}

} // namespace

void register_activity_classes(ENV* env) {
    declare_shared_classes(env);
    register_game_activity(env);
    register_events(env);
    if (getenv("INTOXICATED_JNI_TRACE")) {
        fprintf(stderr, "[classes] GameActivity classes registered\n");
    }
}

} // namespace java
} // namespace intoxicated

using namespace intoxicated;
using namespace intoxicated::java;

/// One finger, as the host's `TouchContact` lays it out.
struct IntoxicatedTouchContact {
    int32_t id;
    float x;
    float y;
};
static_assert(sizeof(IntoxicatedTouchContact) == 12, "TouchContact is three 32-bit slots");

extern "C" {

/// `GameActivity.initializeNativeCode` with its own GameActivity, the three
/// paths, a stateless AssetManager, no saved state and a Configuration that
/// carries no values. Returns the engine's handle, or 0 with a message.
int64_t intoxicated_game_activity_init(void* fn, const char* internal_path, const char* obb_path,
                                       const char* external_path, char* err, size_t err_len) {
    ENV* env = intoxicated::process_env();
    if (!fn || !env) {
        report(err, err_len, "no JavaVM, or initializeNativeCode is not exported");
        return 0;
    }
    using Init = jlong (*)(JNIEnv*, jobject, jstring, jstring, jstring, jobject, jbyteArray, jobject);
    int64_t handle = 0;
    const int rc = contained(err, err_len, [&] {
        LocalFrame frame(env, 16);
        auto self = std::make_shared<GameActivity>();
        auto assets = std::make_shared<AssetManager>();
        auto config = std::make_shared<Configuration>();
        {
            std::lock_guard<std::mutex> lock(g_objects_mutex);
            g_init_objects = {self, assets, config};
        }
        handle = reinterpret_cast<Init>(fn)(env->GetJNIEnv(), to_jni(env, self), jtext(env, internal_path),
                                            jtext(env, obb_path), jtext(env, external_path), to_jni(env, assets),
                                            nullptr, to_jni(env, config));
        if (handle == 0) {
            report(err, err_len, "initializeNativeCode returned a null handle");
        }
        return 0;
    });
    return rc == 0 ? handle : 0;
}

/// Start, resume, surface created and changed, the content rect, then window
/// focus. Prints which parts ran; the INTOXICATED_SKIP_AGDK_* switches skip
/// parts (each leaves a client that draws nothing, and exists to diagnose).
int intoxicated_game_activity_start(int64_t handle, int width, int height, int format, char* err, size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    const bool lifecycle = flag_set("INTOXICATED_SKIP_AGDK_LIFECYCLE");
    const bool state = !(lifecycle || flag_set("INTOXICATED_SKIP_AGDK_STATE"));
    const bool focus = !(lifecycle || flag_set("INTOXICATED_SKIP_AGDK_FOCUS"));
    const bool surface = !flag_set("INTOXICATED_SKIP_AGDK_SURFACE");
    printf("  agdk natives: state=%s focus=%s surface=%s\n", state ? "on" : "skipped", focus ? "on" : "skipped",
           surface ? "on" : "skipped");
    fflush(stdout);
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 16);
        JNIEnv* jni = env->GetJNIEnv();
        const jobject thiz = to_jni(env, shared_activity());
        if (state) {
            for (const char* name : {"onStartNative", "onResumeNative"}) {
                if (auto native = reinterpret_cast<HandleNative>(activity_native(name))) {
                    native(jni, thiz, handle);
                }
            }
        }
        if (surface) {
            auto created = reinterpret_cast<SurfaceNative>(activity_native("onSurfaceCreatedNative"));
            if (!created) {
                report(err, err_len, "onSurfaceCreatedNative is not registered, so the engine has no surface");
                return -1;
            }
            auto s = std::make_shared<Surface>();
            set_surface(s);
            const jobject js = to_jni(env, s);
            created(jni, thiz, handle, js);
            if (auto changed = reinterpret_cast<SurfaceChangedNative>(activity_native("onSurfaceChangedNative"))) {
                changed(jni, thiz, handle, js, format, width, height);
            }
            if (auto rect = reinterpret_cast<RectNative>(activity_native("onContentRectChangedNative"))) {
                rect(jni, thiz, handle, 0, 0, width, height);
            }
        }
        if (focus) {
            if (auto native = reinterpret_cast<FocusNative>(activity_native("onWindowFocusChangedNative"))) {
                native(jni, thiz, handle, JNI_TRUE);
            }
        }
        return 0;
    });
}

/// A registered `(J)V` native by name: onPauseNative, onStopNative,
/// onSurfaceDestroyedNative (which ends the surface lifetime),
/// terminateNativeCode and the like.
int intoxicated_game_activity_lifecycle(int64_t handle, const char* native_name, char* err, size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    if (!native_name) {
        report(err, err_len, "no native name");
        return -1;
    }
    auto native = reinterpret_cast<HandleNative>(activity_native(native_name));
    if (!native) {
        return not_registered(native_name, err, err_len);
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        native(env->GetJNIEnv(), to_jni(env, shared_activity()), handle);
        if (strcmp(native_name, "onSurfaceDestroyedNative") == 0) {
            set_surface(nullptr);
        }
        return 0;
    });
}

int intoxicated_game_activity_window_focus(int64_t handle, int focused, char* err, size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    auto native = reinterpret_cast<FocusNative>(activity_native("onWindowFocusChangedNative"));
    if (!native) {
        return not_registered("onWindowFocusChangedNative", err, err_len);
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        native(env->GetJNIEnv(), to_jni(env, shared_activity()), handle, focused ? JNI_TRUE : JNI_FALSE);
        return 0;
    });
}

/// After the host window resized: surface changed and content rect again,
/// with the existing Surface, each only if registered. The handle is not
/// checked here.
int intoxicated_game_activity_surface_resized(int64_t handle, int format, int width, int height, char* err,
                                              size_t err_len) {
    ENV* env = intoxicated::process_env();
    if (!env) {
        report(err, err_len, "no JavaVM");
        return -1;
    }
    auto surface = current_surface();
    if (!surface) {
        report(err, err_len, "no surface has been created yet");
        return -1;
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        JNIEnv* jni = env->GetJNIEnv();
        const jobject thiz = to_jni(env, shared_activity());
        if (auto changed = reinterpret_cast<SurfaceChangedNative>(activity_native("onSurfaceChangedNative"))) {
            changed(jni, thiz, handle, to_jni(env, surface), format, width, height);
        }
        if (auto rect = reinterpret_cast<RectNative>(activity_native("onContentRectChangedNative"))) {
            rect(jni, thiz, handle, 0, 0, width, height);
        }
        return 0;
    });
}

int intoxicated_game_activity_surface_redraw_needed(int64_t handle, char* err, size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    auto native = reinterpret_cast<SurfaceNative>(activity_native("onSurfaceRedrawNeededNative"));
    if (!native) {
        return not_registered("onSurfaceRedrawNeededNative", err, err_len);
    }
    auto surface = current_surface();
    if (!surface) {
        report(err, err_len, "no surface has been created yet");
        return -1;
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        native(env->GetJNIEnv(), to_jni(env, shared_activity()), handle, to_jni(env, surface));
        return 0;
    });
}

/// Hands the process's one InputConnection to the engine.
int intoxicated_game_activity_set_input_connection(int64_t handle, char* err, size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    auto native = reinterpret_cast<ObjectNative>(activity_native("setInputConnectionNative"));
    if (!native) {
        return not_registered("setInputConnectionNative", err, err_len);
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        native(env->GetJNIEnv(), to_jni(env, shared_activity()), handle, to_jni(env, input_connection()));
        return 0;
    });
}

/// onTextInputEventNative with the field's whole text and selection, and no
/// composing region. An unregistered native is a failure here.
int intoxicated_game_activity_text_input(int64_t handle, const char* text, int sel_start, int sel_end, char* err,
                                         size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    auto native = reinterpret_cast<ObjectNative>(activity_native("onTextInputEventNative"));
    if (!native) {
        report(err, err_len, "onTextInputEventNative is not registered yet");
        return -1;
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        auto state = std::make_shared<State>();
        state->text = jstr(text ? text : "");
        state->selectionStart = sel_start;
        state->selectionEnd = sel_end;
        native(env->GetJNIEnv(), to_jni(env, shared_activity()), handle, to_jni(env, state));
        return 0;
    });
}

/// A mouse-pointer MotionEvent: one contact, id 0.
int intoxicated_game_activity_touch(int64_t handle, int action, float x, float y, int button_state, int action_button,
                                    int64_t event_time_ms, int64_t down_time_ms, int* consumed, char* err,
                                    size_t err_len) {
    auto e = std::make_shared<MotionEvent>();
    e->source = kSourceMouse;
    e->tool = kToolMouse;
    e->action = action;
    e->button_state = button_state;
    e->action_button = action_button;
    e->event_time = event_time_ms;
    e->down_time = down_time_ms;
    e->contacts = {{0, x, y}};
    return deliver_motion(handle, e, consumed, err, err_len);
}

/// A finger MotionEvent: one contact per finger in pointer-index order, the
/// action already packed with its pointer index.
int intoxicated_game_activity_touch_multi(int64_t handle, int action, const IntoxicatedTouchContact* contacts,
                                          int count, int64_t event_time_ms, int64_t down_time_ms, int* consumed,
                                          char* err, size_t err_len) {
    if (!contacts || count <= 0) {
        report(err, err_len, "a touch event needs at least one contact; got no contacts");
        return -1;
    }
    auto e = std::make_shared<MotionEvent>();
    e->source = kSourceTouchscreen;
    e->tool = kToolFinger;
    e->action = action;
    e->event_time = event_time_ms;
    e->down_time = down_time_ms;
    for (int i = 0; i < count; ++i) {
        e->contacts.push_back({contacts[i].id, contacts[i].x, contacts[i].y});
    }
    return deliver_motion(handle, e, consumed, err, err_len);
}

/// ACTION_SCROLL at a point, with the axis values in detents.
int intoxicated_game_activity_scroll(int64_t handle, float x, float y, float hscroll, float vscroll,
                                     int64_t event_time_ms, int* consumed, char* err, size_t err_len) {
    auto e = std::make_shared<MotionEvent>();
    e->source = kSourceMouse;
    e->tool = kToolMouse;
    e->action = kActionScroll;
    e->event_time = event_time_ms;
    e->down_time = event_time_ms;
    e->hscroll = hscroll;
    e->vscroll = vscroll;
    e->contacts = {{0, x, y}};
    return deliver_motion(handle, e, consumed, err, err_len);
}

/// onKeyDownNative or onKeyUpNative.
int intoxicated_game_activity_key(int64_t handle, int down, int key_code, int scan_code, int meta_state,
                                  int repeat_count, int unicode_char, int64_t event_time_ms, int64_t down_time_ms,
                                  int* consumed, char* err, size_t err_len) {
    ENV* env = usable(handle, err, err_len);
    if (!env) {
        return -1;
    }
    const char* name = down ? "onKeyDownNative" : "onKeyUpNative";
    auto native = reinterpret_cast<KeyNative>(activity_native(name));
    if (!native) {
        return not_registered(name, err, err_len);
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 8);
        auto e = std::make_shared<KeyEvent>();
        e->action = down ? 0 : 1;
        e->key_code = key_code;
        e->scan_code = scan_code;
        e->meta_state = meta_state;
        e->repeat_count = repeat_count;
        e->unicode_char = unicode_char;
        e->event_time = event_time_ms;
        e->down_time = down_time_ms;
        const jboolean r = native(env->GetJNIEnv(), to_jni(env, shared_activity()), handle, to_jni(env, e));
        if (consumed) {
            *consumed = r ? 1 : 0;
        }
        return 0;
    });
}

/// Every native the engine registered on `class_name`, as space-separated
/// `name@address` entries into `out`; an entry that does not fit is left
/// out whole. Returns how many were written.
int intoxicated_registered_natives(const char* class_name, char* out, size_t out_len) {
    ENV* env = intoxicated::process_env();
    if (!env) {
        return -1;
    }
    if (!out || out_len == 0) {
        return 0;
    }
    out[0] = '\0';
    std::shared_ptr<Class> cls;
    {
        auto* vm = env->GetVM();
        std::lock_guard<std::mutex> lock(vm->mtx);
        auto it = class_name ? vm->classes.find(class_name) : vm->classes.end();
        if (it != vm->classes.end()) {
            cls = it->second;
        }
    }
    if (!cls) {
        snprintf(out, out_len, "class not registered");
        return 0;
    }
    std::vector<std::pair<std::string, void*>> natives;
    {
        std::lock_guard<std::mutex> lock(cls->mtx);
        natives.assign(cls->natives.begin(), cls->natives.end());
    }
    std::sort(natives.begin(), natives.end());
    std::string text;
    int count = 0;
    for (const auto& [name, address] : natives) {
        char entry[512];
        snprintf(entry, sizeof(entry), "%s%s@%p", text.empty() ? "" : " ", name.c_str(), address);
        if (text.size() + strlen(entry) + 1 > out_len) {
            break;
        }
        text += entry;
        ++count;
    }
    memcpy(out, text.c_str(), text.size() + 1);
    return count;
}

// ------------------------------------------------------------ the engine's own input natives

int intoxicated_input_key_event(void* fn, int down, int key_code, int modifiers, int is_repeat, char* err,
                                size_t err_len) {
    return input_call(fn, kNativeGL, err, err_len, [&](ENV*, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jboolean, jint, jint, jboolean)>(fn)(
            jni, cls, down ? JNI_TRUE : JNI_FALSE, key_code, modifiers, is_repeat ? JNI_TRUE : JNI_FALSE);
    });
}

int intoxicated_input_pass_text(void* fn, int64_t which, const char* text, int flag, int cursor, char* err,
                                size_t err_len) {
    return input_call(fn, kNativeGL, err, err_len, [&](ENV* env, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jlong, jstring, jboolean, jint)>(fn)(
            jni, cls, which, jtext(env, text), flag ? JNI_TRUE : JNI_FALSE, cursor);
    });
}

int intoxicated_input_sync_textbox(void* fn, const char* text, int cursor, char* err, size_t err_len) {
    return input_call(fn, kNativeGL, err, err_len, [&](ENV* env, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jstring, jint)>(fn)(jni, cls, jtext(env, text), cursor);
    });
}

int intoxicated_input_update_keyboard_size(void* fn, int visible, int x, int y, int w, int h, char* err,
                                           size_t err_len) {
    return input_call(fn, kNativeGL, err, err_len, [&](ENV*, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jboolean, jint, jint, jint, jint)>(fn)(
            jni, cls, visible ? JNI_TRUE : JNI_FALSE, x, y, w, h);
    });
}

int intoxicated_input_mouse_move(void* fn, float x, float y, float dx, float dy, char* err, size_t err_len) {
    return input_call(fn, kNativeInput, err, err_len, [&](ENV*, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jfloat, jfloat, jfloat, jfloat)>(fn)(jni, cls, x, y, dx, dy);
    });
}

int intoxicated_input_mouse_button(void* fn, float x, float y, int down, int button, char* err, size_t err_len) {
    return input_call(fn, kNativeInput, err, err_len, [&](ENV*, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jfloat, jfloat, jboolean, jint)>(fn)(
            jni, cls, x, y, down ? JNI_TRUE : JNI_FALSE, button);
    });
}

int intoxicated_input_mouse_wheel(void* fn, float x, float y, float delta, char* err, size_t err_len) {
    return input_call(fn, kNativeInput, err, err_len, [&](ENV*, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jfloat, jfloat, jfloat)>(fn)(jni, cls, x, y, delta);
    });
}

int intoxicated_input_pass_input(void* fn, int pointer_id, float x, float y, int action, int width, int height,
                                 char* err, size_t err_len) {
    return input_call(fn, kNativeInput, err, err_len, [&](ENV*, JNIEnv* jni, jclass cls) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jint, jfloat, jfloat, jint, jint, jint)>(fn)(
            jni, cls, pointer_id, x, y, action, width, height);
    });
}

} // extern "C"
