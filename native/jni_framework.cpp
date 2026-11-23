// The engine's Java framework: the Roblox bridge classes it calls back into
// (NativeGLJavaInterface, NativeHelper, NativeUserJavaInterface and the
// small ones), the Android framework pieces it reaches for (Build, the class
// loader, activities, resources, shared preferences) and the first
// registration entry point, which also brings up the other native modules'
// classes in the order they depend on.
//
// What is refused rather than faked: purchases (no billing), session reports
// and analytics (inert), video codecs (none), HEVC encoding (no), the web
// view's own User-Agent request (printed, not answered: the host hands the
// User-Agent over through setWebviewUserAgent), web overlays (printed with
// the query elided).

#include "jni_world.h"

#include <sys/statvfs.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <map>
#include <mutex>
#include <sstream>
#include <string>
#include <vector>

// The other native modules' registrars, called from here in order.
namespace intoxicated::secure_values {
void register_local_storage_classes(jnivm::ENV* env);
} // namespace intoxicated::secure_values
namespace intoxicated {
void register_cookie_classes(jnivm::ENV* env);
void register_platform_classes(jnivm::ENV* env);
void register_fmod_classes(jnivm::ENV* env);
void register_message_bus_classes(jnivm::ENV* env);
void register_battery_classes(jnivm::ENV* env);
} // namespace intoxicated

namespace intoxicated {
namespace java {
namespace {

constexpr const char* kGLJava = "com/roblox/engine/jni/NativeGLJavaInterface";
constexpr const char* kNativeHelper = "com/roblox/client/startup/NativeHelper";
constexpr const char* kUser = "com/roblox/engine/jni/user/NativeUserJavaInterface";
constexpr const char* kLocaleInterface = "com/roblox/engine/jni/locale/NativeLocaleJavaInterface";
constexpr const char* kSessionReporter = "com/roblox/engine/jni/reporter/SessionReporterJavaInterface";
constexpr const char* kLogging = "com/roblox/universalapp/logging/LoggingProtocol";
constexpr const char* kStorageManager = "com/roblox/client/LocalStorageManager";
constexpr const char* kCodecUtils = "com/roblox/engine/jni/video/MediaCodecInfoUtils";
constexpr const char* kCodecCapability = "com/roblox/engine/jni/video/VideoCodecCapability";
constexpr const char* kBuild = "android/os/Build";
constexpr const char* kBuildVersion = "android/os/Build$VERSION";
constexpr const char* kDebug = "android/os/Debug";
constexpr const char* kString = "java/lang/String";
constexpr const char* kInsetsType = "androidx/core/view/WindowInsetsCompat$Type";
constexpr const char* kJavaClass = "java/lang/Class";
constexpr const char* kClassLoader = "java/lang/ClassLoader";
constexpr const char* kResources = "android/content/res/Resources";
constexpr const char* kContext = "android/content/Context";
constexpr const char* kApplication = "android/app/Application";
constexpr const char* kPrefs = "android/content/SharedPreferences";
constexpr const char* kEditor = "android/content/SharedPreferences$Editor";

const char* text_or_null(const std::shared_ptr<String>& s) {
    return s ? s->c_str() : "(null)";
}

// ------------------------------------------------------------ classes

class NativeHelper final : public Object {};
class Resources final : public Object {
public:
    std::shared_ptr<DisplayMetrics> getDisplayMetrics(ENV* env);
};
class VideoCodecCapability final : public Object {};

std::shared_ptr<DisplayMetrics> display_metrics(ENV* env) {
    auto m = make<DisplayMetrics>(env);
    display_size(&m->widthPixels, &m->heightPixels);
    return m;
}

std::shared_ptr<DisplayMetrics> Resources::getDisplayMetrics(ENV* env) {
    return display_metrics(env);
}

/// The one class loader, created on first use. The engine's protocol classes
/// are loaded through it, and its Android integrity check treats a null
/// loader as untrusted.
class ClassLoader final : public Object {};

std::shared_ptr<ClassLoader> class_loader() {
    static const std::shared_ptr<ClassLoader> loader = std::make_shared<ClassLoader>();
    return loader;
}

std::shared_ptr<Class> load_class(ENV* env, const std::shared_ptr<String>& name) {
    if (!name) {
        return nullptr;
    }
    std::string slashed = *name;
    for (auto& c : slashed) {
        if (c == '.') {
            c = '/';
        }
    }
    if (trace_class_loader()) {
        fprintf(stderr, "[intoxicated][classloader] loadClass/findClass: %s\n", slashed.c_str());
    }
    return jnivm::JNITypes<std::shared_ptr<Class>>::JNICast(env, find_class(env, slashed.c_str()));
}

std::shared_ptr<DeviceStaticParams> device_static_params(ENV* env) {
    auto p = make<DeviceStaticParams>(env);
    p->osVersion = jstr("13");
    p->deviceName = jstr("SM-G991B");
    p->manufacturer = jstr("samsung");
    p->deviceSku = jstr("SM-G991B");
    p->socModel = jstr("exynos2100");
    p->appBuildVariant = jstr("release");
    p->appVersion = jstr("");
    p->cpu64Bit = JNI_TRUE;
    return p;
}

// ------------------------------------------------------------ shared preferences

class Editor;

/// One named store, alive for the process. The engine asks for it by the
/// full path of an XML file Android would have used; it lives in
/// `shared_prefs/` under the working directory instead, named so the same
/// name always finds the same file.
class Preferences final : public Object {
public:
    std::string file;
    std::mutex mutex;
    std::map<std::string, std::string> values;

    void load() {
        std::ifstream in(file, std::ios::binary);
        if (!in) {
            return;
        }
        std::stringstream text;
        text << in.rdbuf();
        values = prefs_parse(text.str());
    }

    /// Called with `mutex` held.
    void save() {
        mkdir("shared_prefs", 0700);
        const std::string tmp = file + ".tmp";
        {
            std::ofstream out(tmp, std::ios::binary | std::ios::trunc);
            out << prefs_serialize(values);
            if (!out) {
                fprintf(stderr, "[prefs] could not write %s\n", tmp.c_str());
                return;
            }
        }
        if (rename(tmp.c_str(), file.c_str()) != 0) {
            fprintf(stderr, "[prefs] could not replace %s\n", file.c_str());
        }
    }

    bool lookup(const std::shared_ptr<String>& key, std::string* out) {
        if (!key) {
            return false;
        }
        std::lock_guard<std::mutex> lock(mutex);
        auto it = values.find(*key);
        if (it == values.end()) {
            return false;
        }
        *out = it->second;
        return true;
    }

    std::shared_ptr<Editor> edit();

    std::shared_ptr<String> getString(std::shared_ptr<String> key, std::shared_ptr<String> fallback) {
        std::string text;
        return lookup(key, &text) ? jstr(text) : fallback;
    }
    jint getInt(std::shared_ptr<String> key, jint fallback) {
        std::string text;
        int value = 0;
        return lookup(key, &text) && leading_int(text, &value) ? value : fallback;
    }
    jlong getLong(std::shared_ptr<String> key, jlong fallback) {
        std::string text;
        long long value = 0;
        return lookup(key, &text) && leading_long(text, &value) ? static_cast<jlong>(value) : fallback;
    }
    jfloat getFloat(std::shared_ptr<String> key, jfloat fallback) {
        std::string text;
        float value = 0;
        return lookup(key, &text) && leading_float(text, &value) ? value : fallback;
    }
    jboolean getBoolean(std::shared_ptr<String> key, jboolean fallback) {
        std::string text;
        if (!lookup(key, &text)) {
            return fallback;
        }
        return text == "true" ? JNI_TRUE : JNI_FALSE;
    }
    jboolean contains(std::shared_ptr<String> key) {
        std::string text;
        return lookup(key, &text) ? JNI_TRUE : JNI_FALSE;
    }
};

/// `SharedPreferences.Editor`: changes staged until `apply` or `commit`.
class Editor final : public Object {
public:
    std::shared_ptr<Preferences> store;
    std::mutex mutex;
    bool clear_all = false;
    std::vector<std::string> removals;
    std::map<std::string, std::string> puts;

    std::shared_ptr<Editor> self() {
        return std::static_pointer_cast<Editor>(shared_from_this());
    }
    std::shared_ptr<Editor> stage(const std::shared_ptr<String>& key, const std::string& text) {
        if (key) {
            std::lock_guard<std::mutex> lock(mutex);
            puts[*key] = text;
        }
        return self();
    }

    std::shared_ptr<Editor> putString(std::shared_ptr<String> key, std::shared_ptr<String> value) {
        return stage(key, text_of(value));
    }
    std::shared_ptr<Editor> putInt(std::shared_ptr<String> key, jint value) {
        return stage(key, std::to_string(value));
    }
    std::shared_ptr<Editor> putLong(std::shared_ptr<String> key, jlong value) {
        return stage(key, std::to_string(static_cast<long long>(value)));
    }
    std::shared_ptr<Editor> putFloat(std::shared_ptr<String> key, jfloat value) {
        return stage(key, float_text(value));
    }
    std::shared_ptr<Editor> putBoolean(std::shared_ptr<String> key, jboolean value) {
        return stage(key, value ? "true" : "false");
    }
    std::shared_ptr<Editor> remove(std::shared_ptr<String> key) {
        if (key) {
            std::lock_guard<std::mutex> lock(mutex);
            removals.push_back(*key);
        }
        return self();
    }
    std::shared_ptr<Editor> clear() {
        std::lock_guard<std::mutex> lock(mutex);
        clear_all = true;
        return self();
    }

    /// Clear if staged, then the removals, then the puts, then save; the
    /// staging starts over.
    bool flush() {
        if (!store) {
            return false;
        }
        std::lock_guard<std::mutex> staged(mutex);
        std::lock_guard<std::mutex> lock(store->mutex);
        if (clear_all) {
            store->values.clear();
        }
        for (const auto& key : removals) {
            store->values.erase(key);
        }
        for (const auto& [key, text] : puts) {
            store->values[key] = text;
        }
        store->save();
        clear_all = false;
        removals.clear();
        puts.clear();
        return true;
    }
    void apply() {
        flush();
    }
    jboolean commit() {
        return flush() ? JNI_TRUE : JNI_FALSE;
    }
};

std::shared_ptr<Editor> Preferences::edit() {
    auto e = std::make_shared<Editor>();
    e->store = std::static_pointer_cast<Preferences>(shared_from_this());
    return e;
}

std::mutex g_prefs_mutex;
std::map<std::string, std::shared_ptr<Preferences>> g_prefs;

std::shared_ptr<Preferences> preferences(const std::shared_ptr<String>& name) {
    const std::string key = name ? std::string(*name) : std::string("default");
    std::lock_guard<std::mutex> lock(g_prefs_mutex);
    auto& store = g_prefs[key];
    if (!store) {
        store = std::make_shared<Preferences>();
        store->file = prefs_relative_path(key);
        std::lock_guard<std::mutex> inner(store->mutex);
        store->load();
    }
    return store;
}

// ------------------------------------------------------------ NativeGLJavaInterface

void on_data_model_notification(const std::shared_ptr<String>& type, const std::shared_ptr<String>& data) {
    const std::string kind = type ? std::string(*type) : std::string("(null)");
    const std::string payload = text_of(data);
    if (kind == "DID_LOG_IN" || kind == "DID_SIGN_UP" || kind == "DID_SWITCH_ACCOUNT") {
        // The payload is the user's identity: its size is all that is shown.
        fprintf(stderr, "[roblox] datamodel notification: %s <identity elided, %zu bytes>\n", kind.c_str(),
                payload.size());
        if (TextSink sink = login_sink()) {
            sink(payload.c_str());
        }
        return;
    }
    if (kind == "DID_LOG_OUT" || kind == "LUA_UNAUTHORIZED_LOG_OUT") {
        fprintf(stderr, "[roblox] datamodel notification: %s\n", kind.c_str());
        if (BareSink sink = logout_sink()) {
            sink();
        }
        return;
    }
    fprintf(stderr, "[roblox] datamodel notification: %s %s\n", kind.c_str(), payload.c_str());
    if (kind == "APP_READY") {
        if (TextSink sink = app_ready_sink()) {
            sink(payload.c_str());
        }
    } else if (kind == "HOME_PAGE_INTERACTIVE") {
        if (BareSink sink = home_interactive_sink()) {
            sink();
        }
    }
}

void game_loaded(const char* via, jlong place) {
    fprintf(stderr, "[roblox] %s: place %lld\n", via, static_cast<long long>(place));
    note_game_loaded(place);
}

void register_gl_java_interface(ENV* env) {
    auto c = env->GetClass(kGLJava);
    c->Hook(env, "gameLoadedCallback", [](jlong place) { game_loaded("gameLoadedCallback", place); });
    c->Hook(env, "getDeviceStaticParams", [](ENV* env) { return device_static_params(env); });
    c->Hook(env, "showKeyboard",
            [](jlong handle, jboolean, std::shared_ptr<jnivm::Array<jbyte>> bytes,
               Lenient<NativeTextBoxInfo> info) {
                std::string text;
                if (bytes && bytes->getSize() > 0) {
                    const char* data = reinterpret_cast<const char*>(bytes->getArray());
                    const size_t size = static_cast<size_t>(bytes->getSize());
                    text.assign(data, strnlen(data, size));
                }
                const bool known = info.object && info.object->known;
                if (trace_text()) {
                    fprintf(stderr, "[text] showKeyboard: info %s, spec %s\n", info.object ? "present" : "null",
                            known ? "known" : "not known");
                }
                textbox_focus(handle, text, known ? &info.object->spec : nullptr);
            });
    c->Hook(env, "hideKeyboard", [] { textbox_blur(); });
    // No billing on this client: a purchase prompt does nothing.
    c->Hook(env, "promptNativePurchase", [](jlong, std::shared_ptr<String>, std::shared_ptr<String>) {});
    c->Hook(env, "promptNativePurchaseWithPayload", [](jlong, std::shared_ptr<String>, std::shared_ptr<String>) {});
    c->Hook(env, "exitGameWithError",
            [](jint code) { fprintf(stderr, "[roblox] the engine asked to exit the game with error %d\n", code); });
    c->Hook(env, "gameDidLeave", [] { fprintf(stderr, "[roblox] game left\n"); });
    c->Hook(env, "onAppShellReloadNeeded", [] {});
    c->Hook(env, "onLuaTextBoxChangedCallback", [](std::shared_ptr<String>) {});
    c->Hook(env, "onLuaTextBoxPropertyChangedCallback", [] {});
    c->Hook(env, "listenToMotionEvents", [](std::shared_ptr<String>) {});
    c->Hook(env, "screenOrientationChanged", [](jint) {});
    c->Hook(env, "openNativeOverlay", [](std::shared_ptr<String> url, std::shared_ptr<String> title) {
        fprintf(stderr, "[roblox] web view requested, and Intoxicated has none: %s (title: %s)\n",
                url_without_query(url ? url->c_str() : nullptr).c_str(), text_or_null(title));
    });
    c->Hook(env, "onAppBridgeNotification", [](std::shared_ptr<String> type, std::shared_ptr<String> data) {
        fprintf(stderr, "[roblox] app bridge: %s %s\n", text_or_null(type), text_of(data).c_str());
    });
    c->Hook(env, "onDataModelNotificationCallback", [](std::shared_ptr<String> type, std::shared_ptr<String> data) {
        on_data_model_notification(type, data);
    });
    c->Hook(env, "getWebViewUserAgent", [] {
        fprintf(stderr, "[roblox] the engine asked for the web view's User-Agent; unanswered (no web view; "
                        "the host sets it through setWebviewUserAgent)\n");
    });
    expect(c, Member::StaticMethod, "showKeyboard", "(JZ[BLcom/roblox/engine/jni/model/NativeTextBoxInfo;)V");
    expect(c, Member::StaticMethod, "getDeviceStaticParams", "()Lcom/roblox/engine/jni/model/DeviceStaticParams;");
}

// ------------------------------------------------------------ NativeHelper

void register_native_helper(ENV* env) {
    env->GetClass<NativeHelper>(kNativeHelper);
    auto c = env->GetClass(kNativeHelper);
    // Every one of these only reports; game-loaded also feeds the counter the
    // host's join watchdog reads. Receivers are generic: the helper object is
    // ours, but nothing makes the engine call these on it.
    c->HookInstanceFunction(env, "gameActivity_onFlagsFailed", [](ENV*, Object*) {
        fprintf(stderr, "[roblox] the engine reported onFlagsFailed: its flags did not load\n");
    });
    c->HookInstanceFunction(env, "gameActivity_onFlagsLoaded", [](ENV*, Object*, std::shared_ptr<jnivm::ByteBuffer> b) {
        fprintf(stderr, "[roblox] flags loaded (%lld bytes)\n", b ? static_cast<long long>(b->capacity) : -1LL);
    });
    c->HookInstanceFunction(env, "gameActivity_onAppReady", [](ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] app ready: %s\n", text_of(s).c_str());
    });
    c->HookInstanceFunction(env, "gameActivity_onExperienceStart",
                            [](ENV*, Object*) { fprintf(stderr, "[roblox] experience started\n"); });
    c->HookInstanceFunction(env, "gameActivity_onGameLoaded",
                            [](ENV*, Object*, jlong place) { game_loaded("game loaded", place); });
    c->HookInstanceFunction(env, "gameActivity_onDidLogInReceived", [](ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] logged in (%zu bytes, not shown)\n", text_of(s).size());
    });
    c->HookInstanceFunction(env, "gameActivity_onScreenOrientationChanged", [](ENV*, Object*, jint o, jboolean locked) {
        fprintf(stderr, "[roblox] orientation %d (locked: %s)\n", o, locked ? "yes" : "no");
    });
    c->HookInstanceFunction(env, "gameActivity_onEngineInitialized",
                            [](ENV*, Object*) { fprintf(stderr, "[roblox] engine initialised\n"); });
    c->HookInstanceFunction(env, "gameActivity_onDidLogOutReceived",
                            [](ENV*, Object*) { fprintf(stderr, "[roblox] logged out\n"); });
    c->HookInstanceFunction(env, "gameActivity_onDidSwitchAccountReceived",
                            [](ENV*, Object*) { fprintf(stderr, "[roblox] account switched\n"); });
    c->HookInstanceFunction(env, "gameActivity_onLuaAppDidReturn",
                            [](ENV*, Object*) { fprintf(stderr, "[roblox] Lua app returned\n"); });
    c->HookInstanceFunction(env, "gameActivity_onRestartLuaApp",
                            [](ENV*, Object*) { fprintf(stderr, "[roblox] the engine asked to restart the Lua app\n"); });
    c->HookInstanceFunction(env, "gameActivity_onScanQrCode", [](ENV*, Object*) {
        fprintf(stderr, "[roblox] the engine asked to scan a QR code; Intoxicated has no camera path\n");
    });
    c->HookInstanceFunction(env, "gameActivity_onDidSignUp", [](ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] signed up (%zu bytes, not shown)\n", text_of(s).size());
    });
    c->HookInstanceFunction(env, "gameActivity_onGameStreamingStatusChanged",
                            [](ENV*, Object*, std::shared_ptr<String> s) {
                                fprintf(stderr, "[roblox] game streaming status: %s\n", text_of(s).c_str());
                            });
    c->HookInstanceFunction(env, "gameActivity_onScreenshotReady", [](ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] screenshot ready: %s\n", text_of(s).c_str());
    });
    c->HookInstanceFunction(env, "gameActivity_onMotionEventListening", [](ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] motion event listening: %s\n", text_of(s).c_str());
    });
    c->HookInstanceFunction(env, "gameActivity_onExperienceStop", [](ENV*, Object*, jdouble seconds) {
        fprintf(stderr, "[roblox] experience stopped after %.3f s\n", seconds);
    });
    c->HookInstanceFunction(env, "gameActivity_setAppUpgradeStatus",
                            [](ENV*, Object*, jint a, jint b, std::shared_ptr<String> s1, std::shared_ptr<String> s2) {
                                fprintf(stderr, "[roblox] app upgrade status %d/%d %s %s\n", a, b, text_of(s1).c_str(),
                                        text_of(s2).c_str());
                            });
    expect(c, Member::Method, "gameActivity_onFlagsLoaded", "(Ljava/nio/ByteBuffer;)V");
}

// ------------------------------------------------------------ identity and locale

std::shared_ptr<String> platform_name() {
    static const std::string name = env_or("INTOXICATED_PLATFORM_NAME", "Linux");
    return jstr(name);
}

Identity asked(const char* getter) {
    Identity who = identity();
    if (trace_identity()) {
        fprintf(stderr, "[intoxicated] identity asked: %s (%s)\n", getter, who.known ? "signed in" : "nobody");
    }
    return who;
}

void register_user_interface(ENV* env) {
    auto c = env->GetClass(kUser);
    c->Hook(env, "getUserId", []() -> jlong {
        const Identity who = asked("getUserId");
        return who.known ? who.user_id : 0;
    });
    c->Hook(env, "getIsUnder13", []() -> jboolean {
        const Identity who = asked("getIsUnder13");
        return who.known && who.under13 ? JNI_TRUE : JNI_FALSE;
    });
    c->Hook(env, "getUsername", []() -> std::shared_ptr<String> {
        const Identity who = asked("getUsername");
        return jstr(who.known ? who.username : "");
    });
    c->Hook(env, "getDisplayName", []() -> std::shared_ptr<String> {
        const Identity who = asked("getDisplayName");
        if (!who.known) {
            return jstr("");
        }
        return jstr(who.display_name.empty() ? who.username : who.display_name);
    });
    c->Hook(env, "getAlternateName", []() -> std::shared_ptr<String> { return jstr(""); });
    c->Hook(env, "getPlatformName", []() -> std::shared_ptr<String> {
        asked("getPlatformName");
        return platform_name();
    });
    c->Hook(env, "getMembershipType", []() -> jint {
        const Identity who = asked("getMembershipType");
        return who.known ? static_cast<jint>(who.membership_type) : 0;
    });
    c->Hook(env, "getHasRobloxSubscription", []() -> jboolean {
        const Identity who = asked("getHasRobloxSubscription");
        return who.known && who.subscription ? JNI_TRUE : JNI_FALSE;
    });
    c->Hook(env, "getTheme", []() -> std::shared_ptr<String> { return jstr("Dark"); });

    auto l = env->GetClass(kLocaleInterface);
    for (const char* name : {"getLocale", "getRobloxLocale", "getGameLocale"}) {
        l->Hook(env, name, []() -> std::shared_ptr<String> {
            return jstr(locale_tag(getenv("LC_ALL"), getenv("LC_MESSAGES"), getenv("LANG")));
        });
    }
}

// ------------------------------------------------------------ small classes

/// Milliseconds on a monotonic clock since the first call, which answers 0.
jlong process_timestamp() {
    static const auto start = std::chrono::steady_clock::now();
    return std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - start).count();
}

jlong allocatable_bytes() {
    struct statvfs fs {};
    if (statvfs(".", &fs) != 0) {
        fprintf(stderr, "[storage] getAllocatableBytes: statvfs of the working directory failed (%s)\n",
                strerror(errno));
        return 0;
    }
    return static_cast<jlong>(fs.f_bavail) * static_cast<jlong>(fs.f_frsize);
}

void register_small_classes(ENV* env) {
    {
        auto c = env->GetClass(kSessionReporter);
        c->Hook(env, "getFilesDir", []() -> std::shared_ptr<String> { return jstr(files_dir()); });
        c->Hook(env, "getAppVersion", []() -> std::shared_ptr<String> { return jstr(""); });
        c->Hook(env, "getLastLoggedInUser", []() -> std::shared_ptr<String> { return jstr(""); });
        c->Hook(env, "getLastLoggedInUserId", []() -> std::shared_ptr<String> { return jstr(""); });
        // Deliberately inert: nothing is reported anywhere.
        c->Hook(env, "sendSessionReport", [](std::shared_ptr<String>, std::shared_ptr<String>) {});
        c->Hook(env, "setEventTrackingGoogleAnalytics",
                [](std::shared_ptr<String>, std::shared_ptr<String>, std::shared_ptr<String>, jlong) {});
    }
    env->GetClass(kLogging)->Hook(env, "getProcessTimestamp", [] { return process_timestamp(); });
    env->GetClass(kStorageManager)
        ->HookInstanceFunction(env, "getAllocatableBytes", [](ENV*, Object*) { return allocatable_bytes(); });
    env->GetClass<VideoCodecCapability>(kCodecCapability);
    {
        auto c = env->GetClass(kCodecUtils);
        c->Hook(env, "getVideoCodecs",
                [] { return std::make_shared<jnivm::Array<VideoCodecCapability>>(0); });
        c->Hook(env, "hevcHardwareEncodingSupported", [](jint, jint, jint) -> jboolean { return JNI_FALSE; });
    }
    env->GetClass(kBuildVersion)->HookGetterFunction(env, "SDK_INT", []() -> jint { return kApiLevel; });
    {
        // A Galaxy S21 on Android 13, for the engine's integrity path. These
        // name a real phone and are not branding.
        auto c = env->GetClass(kBuild);
        static const std::pair<const char*, const char*> fields[] = {
            {"MANUFACTURER", "samsung"},
            {"MODEL", "SM-G991B"},
            {"BOARD", "exynos2100"},
            {"BOOTLOADER", "G991BXXU5DVK1"},
            {"BRAND", "samsung"},
            {"DEVICE", "o1s"},
            {"FINGERPRINT", "samsung/o1sxxx/o1s:13/TP1A.220624.014/G991BXXU5DVK1:user/release-keys"},
            {"HARDWARE", "exynos2100"},
            {"PRODUCT", "o1sxxx"},
            {"TAGS", "release-keys"},
            {"TYPE", "user"},
            {"ID", "TP1A.220624.014"},
            {"DISPLAY", "TP1A.220624.014.G991BXXU5DVK1"},
            {"HOST", "SWDG6707"},
            {"USER", "dpi"},
        };
        for (const auto& [name, value] : fields) {
            const char* v = value;
            c->HookGetterFunction(env, name, [v]() -> std::shared_ptr<String> { return jstr(v); });
        }
    }
    env->GetClass(kDebug)->Hook(env, "isDebuggerConnected", []() -> jboolean { return JNI_FALSE; });
    env->GetClass(kString)->HookInstanceFunction(
        env, "getBytes",
        [](ENV*, String* self, std::shared_ptr<String>) -> std::shared_ptr<jnivm::Array<jbyte>> {
            // The bytes as stored, which is UTF-8; the charset is not asked.
            const std::string bytes = self ? std::string(*self) : std::string();
            auto array = std::make_shared<jnivm::Array<jbyte>>(static_cast<jsize>(bytes.size()));
            if (!bytes.empty()) {
                memcpy(array->getArray(), bytes.data(), bytes.size());
            }
            return array;
        });
    {
        // Distinct masks; the only consumer, getWindowInsets, answers zero
        // insets for every one of them.
        auto c = env->GetClass(kInsetsType);
        c->Hook(env, "statusBars", []() -> jint { return 1; });
        c->Hook(env, "navigationBars", []() -> jint { return 2; });
        c->Hook(env, "captionBar", []() -> jint { return 4; });
        c->Hook(env, "ime", []() -> jint { return 8; });
        c->Hook(env, "systemGestures", []() -> jint { return 16; });
        c->Hook(env, "mandatorySystemGestures", []() -> jint { return 32; });
        c->Hook(env, "tappableElement", []() -> jint { return 64; });
        c->Hook(env, "displayCutout", []() -> jint { return 128; });
        c->Hook(env, "systemBars", []() -> jint { return 1 | 2 | 4; });
    }
}

void register_class_loading(ENV* env) {
    env->GetClass<ClassLoader>(kClassLoader);
    env->GetClass(kJavaClass)->HookInstanceFunction(env, "getClassLoader", [](ENV*, Object*) {
        if (trace_class_loader()) {
            fprintf(stderr, "[intoxicated][classloader] Class.getClassLoader() -> singleton\n");
        }
        return class_loader();
    });
    auto c = env->GetClass(kClassLoader);
    c->HookInstanceFunction(env, "loadClass",
                            [](ENV* env, Object*, std::shared_ptr<String> name) { return load_class(env, name); });
    c->HookInstanceFunction(env, "findClass",
                            [](ENV* env, Object*, std::shared_ptr<String> name) { return load_class(env, name); });
    expect(env->GetClass(kJavaClass), Member::Method, "getClassLoader", "()Ljava/lang/ClassLoader;");
    expect(c, Member::Method, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;");
}

void register_activities(ENV* env) {
    env->GetClass<Resources>(kResources);
    env->GetClass(kResources)->HookInstanceFunction(env, "getDisplayMetrics", &Resources::getDisplayMetrics);
    env->GetClass(kActivity)->HookInstanceFunction(
        env, "getDisplayMetrics", [](ENV* env, Object*) { return display_metrics(env); });
    // Whatever the receiver: the engine resolves these against one name and
    // calls them on whichever activity object it holds.
    for (const char* name : {kActivity, kGameActivity, kMainGameActivity}) {
        auto c = env->GetClass(name);
        c->HookInstanceFunction(env, "getResources", [](ENV* env, Object*) { return make<Resources>(env); });
        c->HookInstanceFunction(env, "getNativeHelper", [](ENV* env, Object*) { return make<NativeHelper>(env); });
        expect(c, Member::Method, "getResources", "()Landroid/content/res/Resources;");
        expect(c, Member::Method, "getNativeHelper", "()Lcom/roblox/client/startup/NativeHelper;");
    }
}

void register_preferences_and_permissions(ENV* env) {
    env->GetClass<Preferences>(kPrefs);
    env->GetClass<Editor>(kEditor);
    {
        auto c = env->GetClass(kPrefs);
        c->HookInstanceFunction(env, "edit", &Preferences::edit);
        c->HookInstanceFunction(env, "getString", &Preferences::getString);
        c->HookInstanceFunction(env, "getInt", &Preferences::getInt);
        c->HookInstanceFunction(env, "getLong", &Preferences::getLong);
        c->HookInstanceFunction(env, "getFloat", &Preferences::getFloat);
        c->HookInstanceFunction(env, "getBoolean", &Preferences::getBoolean);
        c->HookInstanceFunction(env, "contains", &Preferences::contains);
    }
    {
        auto c = env->GetClass(kEditor);
        c->HookInstanceFunction(env, "putString", &Editor::putString);
        c->HookInstanceFunction(env, "putInt", &Editor::putInt);
        c->HookInstanceFunction(env, "putLong", &Editor::putLong);
        c->HookInstanceFunction(env, "putFloat", &Editor::putFloat);
        c->HookInstanceFunction(env, "putBoolean", &Editor::putBoolean);
        c->HookInstanceFunction(env, "remove", &Editor::remove);
        c->HookInstanceFunction(env, "clear", &Editor::clear);
        c->HookInstanceFunction(env, "apply", &Editor::apply);
        c->HookInstanceFunction(env, "commit", &Editor::commit);
    }
    for (const char* name : {kContext, kActivity, kApplication}) {
        auto c = env->GetClass(name);
        c->HookInstanceFunction(env, "getSharedPreferences",
                                [](ENV*, Object*, std::shared_ptr<String> file, jint) { return preferences(file); });
        // Every permission is granted here; the audio module is what keeps
        // the microphone honest.
        c->HookInstanceFunction(env, "checkSelfPermission",
                                [](ENV*, Object*, std::shared_ptr<String>) -> jint { return 0; });
        expect(c, Member::Method, "getSharedPreferences",
               "(Ljava/lang/String;I)Landroid/content/SharedPreferences;");
    }
}

/// libjnivm spells every `jnivm::Object` in a descriptor from the class it is
/// bound to, so binding it anywhere but java/lang/Object would quietly change
/// the descriptor of every generic hook registered after that.
void check_object_binding(ENV* env, const char* after) {
    auto* vm = env->GetVM();
    std::lock_guard<std::mutex> lock(vm->mtx);
    auto it = vm->typecheck.find(typeid(Object));
    const char* bound = it == vm->typecheck.end() || !it->second ? "(nothing)" : it->second->nativeprefix.c_str();
    if (strcmp(bound, "java/lang/Object") != 0) {
        fprintf(stderr, "[classes] after %s, jnivm::Object is bound to %s instead of java/lang/Object\n", after,
                bound);
    }
}

} // namespace

void register_framework_classes(ENV* env) {
    declare_shared_classes(env);
    register_gl_java_interface(env);
    register_native_helper(env);
    register_user_interface(env);
    register_small_classes(env);
    register_class_loading(env);
    register_activities(env);
    check_object_binding(env, "the framework classes");

    intoxicated::register_cookie_classes(env);
    register_fmod_classes(env);
    register_message_bus_classes(env);
    register_preferences_and_permissions(env);
    intoxicated::secure_values::register_local_storage_classes(env);
    intoxicated::register_platform_classes(env);
    register_battery_classes(env);
    check_object_binding(env, "the other modules' classes");

    if (getenv("INTOXICATED_JNI_TRACE")) {
        fprintf(stderr, "[classes] framework classes registered\n");
    }
}

} // namespace java
} // namespace intoxicated
