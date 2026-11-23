// Parameter objects: the carriers the engine is handed or builds, and the
// host operations that deliver them to the engine's bridge and settings
// natives.
//
// Every carrier is built new for each delivery and holds plain values; the
// values come from jni_values.cpp. Classes registered here, in this order
// (the third registration entry point, after jni_framework.cpp and
// activity.cpp): the carriers the engine reads, the flag result and the
// collections it builds, the locale pair, DisplayMetrics, Configuration,
// Point before DeviceUtils, Rect.

#include "jni_world.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <mutex>
#include <string>
#include <vector>

namespace intoxicated {
namespace java {

// A parameter that is spelled `Landroid/content/Context;` in a descriptor but
// accepts whatever object the engine passes. A typed parameter would be
// checked with dynamic_cast, and the engine's context may be any of several
// objects (an Activity, the Application, a GameActivity) or null.
struct ContextArg {
    jobject object;
};

} // namespace java
} // namespace intoxicated

namespace jnivm {
template <>
struct JNITypes<intoxicated::java::ContextArg> {
    static std::string GetJNISignature(ENV*) {
        return "Landroid/content/Context;";
    }
    static intoxicated::java::ContextArg JNICast(ENV*, const jvalue& v) {
        return {v.l};
    }
};
} // namespace jnivm

namespace intoxicated {
namespace java {

namespace {

constexpr const char* kDeviceParams = "com/roblox/engine/jni/model/DeviceParams";
constexpr const char* kPlatformParams = "com/roblox/engine/jni/model/PlatformParams";
constexpr const char* kInitParams = "com/roblox/engine/jni/autovalue/InitParams";
constexpr const char* kStartAppParams = "com/roblox/engine/jni/autovalue/StartAppParams";
constexpr const char* kFlagsResult = "com/roblox/client/flags/NativeFlagsInitResult";
constexpr const char* kMap = "java/util/Map";
constexpr const char* kArrayList = "java/util/ArrayList";
constexpr const char* kList = "java/util/List";
constexpr const char* kLocalFlags = "com/roblox/engine/jni/model/ClientLocalFlags";
constexpr const char* kJsonObject = "org/json/JSONObject";
constexpr const char* kLocale = "java/util/Locale";
constexpr const char* kLocaleList = "android/os/LocaleList";
constexpr const char* kRect = "android/graphics/Rect";
constexpr const char* kDeviceUtils = "com/roblox/platform/util/DeviceUtils";

constexpr const char* kNativeGL = "com/roblox/engine/jni/NativeGLInterface";
constexpr const char* kNativeSettings = "com/roblox/engine/jni/NativeSettingsInterface";
constexpr const char* kAppBridge = "com/roblox/engine/jni/NativeAppBridgeInterface";
constexpr const char* kFlagJni = "com/roblox/client/flags/FlagJniInterface";
constexpr const char* kAssetSetup = "com/roblox/client/JNIAAssetManagerSetup";
constexpr const char* kStorageManager = "com/roblox/client/LocalStorageManager";
constexpr const char* kLifecycle = "com/roblox/universalapp/activitylifecyclecallbacks/JNIActivityLifecycleCallbacks";

// ------------------------------------------------------------ carriers

class DeviceParams final : public Object {
public:
    std::shared_ptr<String> deviceName, osVersion, appBuildVariant, appVersion, country, deviceSku,
        displayResolution, manufacturer, networkType, socModel, testDeviceName;
    jboolean cpu64Bit = JNI_TRUE;
    jboolean isChrome = JNI_FALSE;
    jboolean isLowRamDevice = JNI_FALSE;
    jint deviceTotalMemoryMB = 0;
    jint displayPhysicalWidthPixels = 0;
    jint displayPhysicalHeightPixels = 0;
    jint largeMemoryClass = 512;
    jint memoryClass = 256;
    jlong lowMemoryKillerBackgroundAppThreshold = 0;
    jlong lowMemoryKillerForegroundAppThreshold = 0;
};

class PlatformParams final : public Object {
public:
    std::shared_ptr<String> assetFolderPath;
    jfloat dpiScale = 1.0f;
    jboolean isTouchDevice = JNI_FALSE;
    jboolean isKeyboardDevice = JNI_TRUE;
    jboolean isMouseDevice = JNI_TRUE;
    jint viewportWidthMm = 0;
    jint viewportHeightMm = 0;
};

/// AutoValue style: the engine reads the accessors, and the fields answer the
/// same values.
class InitParams final : public Object {
public:
    std::shared_ptr<String> baseURL, buildVariant, userAgent;
    std::shared_ptr<DeviceParams> deviceParams;
    std::shared_ptr<PlatformParams> platformParams;
    std::shared_ptr<Activity> vrContext;
    jboolean isPotato = JNI_FALSE;
    jboolean isTablet = JNI_FALSE;
    jboolean isVrDevice = JNI_FALSE;

    std::shared_ptr<String> baseURL_() {
        if (trace_base_url()) {
            fprintf(stderr, "[baseurl] engine read InitParams.baseURL() = %s\n", text_of(baseURL).c_str());
        }
        return baseURL;
    }
    std::shared_ptr<String> buildVariant_() { return buildVariant; }
    std::shared_ptr<String> userAgent_() { return userAgent; }
    std::shared_ptr<DeviceParams> deviceParams_() { return deviceParams; }
    std::shared_ptr<PlatformParams> platformParams_() { return platformParams; }
    std::shared_ptr<Activity> vrContext_() { return vrContext; }
    jboolean isPotato_() { return isPotato; }
    jboolean isTablet_() { return isTablet; }
    jboolean isVrDevice_() { return isVrDevice; }
};

class StartAppParams final : public Object {
public:
    std::shared_ptr<String> appStarterPlace, appStarterScript, selectedTheme, username;
    jlong appUserId = 0;
    jboolean isUnder13 = JNI_FALSE;
    jint membershipType = 0;
    std::shared_ptr<PlatformParams> platformParams;
    std::shared_ptr<Surface> surface;
    std::shared_ptr<Activity> vrContext;

    std::shared_ptr<String> appStarterPlace_() { return appStarterPlace; }
    std::shared_ptr<String> appStarterScript_() { return appStarterScript; }
    std::shared_ptr<String> selectedTheme_() { return selectedTheme; }
    std::shared_ptr<String> username_() { return username; }
    jlong appUserId_() { return appUserId; }
    jboolean isUnder13_() { return isUnder13; }
    jint membershipType_() { return membershipType; }
    std::shared_ptr<PlatformParams> platformParams_() { return platformParams; }
    std::shared_ptr<Surface> surface_() { return surface; }
    std::shared_ptr<Activity> vrContext_() { return vrContext; }
};

std::shared_ptr<DeviceParams> build_device_params(ENV* env, int width, int height) {
    auto p = make<DeviceParams>(env);
    p->deviceName = jstr(env_or("INTOXICATED_DEVICE_NAME", "Intoxicated"));
    p->manufacturer = jstr(env_or("INTOXICATED_MANUFACTURER", "Intoxicated"));
    p->socModel = jstr(env_or("INTOXICATED_SOC_MODEL", "intoxicated"));
    p->deviceSku = jstr(env_or("INTOXICATED_DEVICE_SKU", "intoxicated"));
    p->osVersion = jstr(std::to_string(kApiLevel));
    p->appBuildVariant = jstr("release");
    p->appVersion = jstr("");
    p->country = jstr("US");
    p->networkType = jstr("WIFI");
    p->testDeviceName = jstr("");
    p->displayResolution = jstr(std::to_string(width) + "x" + std::to_string(height));
    p->displayPhysicalWidthPixels = width;
    p->displayPhysicalHeightPixels = height;
    p->deviceTotalMemoryMB = total_memory_mb(getenv("INTOXICATED_DEVICE_RAM_MB"), physmem_mb());
    fprintf(stderr, "[device] DeviceParams.deviceTotalMemoryMB = %d\n", p->deviceTotalMemoryMB);
    return p;
}

std::shared_ptr<PlatformParams> build_platform_params(ENV* env, const std::string& assets, int width, int height) {
    auto p = make<PlatformParams>(env);
    p->assetFolderPath = jstr(assets);
    std::string complaint;
    p->dpiScale = dpi_scale(getenv("INTOXICATED_DPI_SCALE"), &complaint);
    if (!complaint.empty()) {
        fprintf(stderr, "%s\n", complaint.c_str());
    }
    p->isTouchDevice = touchscreen() ? JNI_TRUE : JNI_FALSE;
    p->viewportWidthMm = millimetres_at_96dpi(width);
    p->viewportHeightMm = millimetres_at_96dpi(height);
    return p;
}

std::shared_ptr<InitParams> build_init_params(ENV* env, const std::string& assets, int width, int height) {
    auto p = make<InitParams>(env);
    const Profile profile = device_profile();
    const std::string ua = user_agent();
    p->isTablet = profile == Profile::AndroidTablet ? JNI_TRUE : JNI_FALSE;
    fprintf(stderr, "[intoxicated] device identity: %s (isTablet=%s, User-Agent: %s)\n", profile_label(profile),
            p->isTablet ? "true" : "false", ua.c_str());
    const char* base = getenv("INTOXICATED_BASE_URL");
    p->baseURL = jstr(base ? base : "https://www.roblox.com");
    p->buildVariant = jstr("release");
    p->userAgent = jstr(ua);
    p->deviceParams = build_device_params(env, width, height);
    p->platformParams = build_platform_params(env, assets, width, height);
    p->vrContext = make<Activity>(env);
    return p;
}

std::shared_ptr<StartAppParams> build_start_app_params(ENV* env, const std::string& assets, int width, int height,
                                                       const std::shared_ptr<Surface>& surface) {
    auto p = make<StartAppParams>(env);
    const Identity who = identity();
    p->appStarterPlace = jstr("");
    p->appStarterScript = jstr("");
    p->selectedTheme = jstr("Dark");
    p->username = jstr(who.known ? who.username : "");
    p->appUserId = who.known ? who.user_id : 0;
    p->isUnder13 = who.known && who.under13 ? JNI_TRUE : JNI_FALSE;
    p->membershipType = who.known ? static_cast<jint>(who.membership_type) : 0;
    p->platformParams = build_platform_params(env, assets, width, height);
    p->surface = surface;
    p->vrContext = make<Activity>(env);
    fprintf(stderr, "[intoxicated] app start as %s\n", who.known ? "a signed-in user" : "nobody signed in");
    return p;
}

// ------------------------------------------------------------ built by the engine

/// `java.util.Map`, as `getBooleanCachedMap` hands it out.
class BooleanMap final : public Object {
public:
    std::mutex mutex;
    std::map<std::string, bool> values;

    jint size() {
        std::lock_guard<std::mutex> lock(mutex);
        return static_cast<jint>(values.size());
    }
    jboolean isEmpty() {
        return size() == 0 ? JNI_TRUE : JNI_FALSE;
    }
};

/// `NativeFlagsInitResult`, which the engine constructs and fills during the
/// flag handshake. Its constructor must be a static factory: libjnivm
/// rewrites `<init>(I)V` into a static `<init>(I)L...;` lookup.
class NativeFlagsInitResult final : public Object {
public:
    jint provider = 0;
    std::shared_ptr<BooleanMap> map;

    static std::shared_ptr<NativeFlagsInitResult> create(ENV* env, jint provider_id) {
        auto r = make<NativeFlagsInitResult>(env);
        r->provider = provider_id;
        r->map = std::make_shared<BooleanMap>();
        return r;
    }
    void addBoolean(std::shared_ptr<String> name, jboolean value, jboolean /*unused*/) {
        if (!name || !map) {
            return;
        }
        std::lock_guard<std::mutex> lock(map->mutex);
        map->values[*name] = value != JNI_FALSE;
    }
    jint getNativeFlagProviderId() {
        return provider;
    }
    std::shared_ptr<BooleanMap> getBooleanCachedMap() {
        return map;
    }
    jboolean resolveFlagValue(std::shared_ptr<String> name) {
        if (!name || !map) {
            return JNI_FALSE;
        }
        std::lock_guard<std::mutex> lock(map->mutex);
        auto it = map->values.find(*name);
        return it != map->values.end() && it->second ? JNI_TRUE : JNI_FALSE;
    }
};

/// The list `nativePostClientSettingsLoadedInitialization3` takes: the
/// previous process's exit reasons. There were none to report, so it is
/// empty, which is the truthful answer.
class ArrayList final : public Object {
public:
    jint size() { return 0; }
    jboolean isEmpty() { return JNI_TRUE; }
    std::shared_ptr<Object> get(jint) { return nullptr; }
};

class JsonObject final : public Object {
public:
    jint entries = 0;
    jint length() { return entries; }
};

class ClientLocalFlags final : public Object {
public:
    std::mutex mutex;
    std::map<std::string, std::string> values;

    void add(std::shared_ptr<String> name, std::shared_ptr<String> value) {
        if (!name) {
            return;
        }
        std::lock_guard<std::mutex> lock(mutex);
        values[*name] = text_of(value);
    }
    jboolean isEmpty() {
        std::lock_guard<std::mutex> lock(mutex);
        return values.empty() ? JNI_TRUE : JNI_FALSE;
    }
    jint size() {
        std::lock_guard<std::mutex> lock(mutex);
        return static_cast<jint>(values.size());
    }
    /// A JSONObject of the same length. Its values are not carried: nothing
    /// has been seen reading them back.
    std::shared_ptr<JsonObject> getAll(ENV* env) {
        auto json = make<JsonObject>(env);
        json->entries = size();
        return json;
    }
};

// ------------------------------------------------------------ locale

/// `Configuration.getLocales().get(0)`. Fixed, unlike the engine's own locale
/// interface, which follows the host.
class Locale final : public Object {
public:
    std::shared_ptr<String> getLanguage() { return jstr("en"); }
    std::shared_ptr<String> getCountry() { return jstr("US"); }
    std::shared_ptr<String> getScript() { return jstr(""); }
    std::shared_ptr<String> getVariant() { return jstr(""); }
    std::shared_ptr<String> toString() { return jstr("en_US"); }
};

class LocaleList final : public Object {
public:
    jint size() { return 1; }
    jboolean isEmpty() { return JNI_FALSE; }
    std::shared_ptr<Locale> get(ENV* env, jint) { return make<Locale>(env); }
};

class Rect final : public Object {
public:
    jint left = 0;
    jint top = 0;
    jint right = 0;
    jint bottom = 0;
};

// ------------------------------------------------------------ registration

/// A field hook. With `INTOXICATED_TRACE_PARAM_READS` it is a getter that
/// names the field on stderr before answering the same value.
template <class T, class V>
void field(ENV* env, const std::shared_ptr<Class>& c, const char* label, const char* name, V T::*member) {
    if (trace_param_reads()) {
        c->HookInstanceGetterFunction(env, name, [label, name, member](ENV*, T* self) -> V {
            fprintf(stderr, "[intoxicated] param read: %s.%s\n", label, name);
            return self->*member;
        });
    } else {
        c->HookInstance(env, name, static_cast<V T::*>(member));
    }
}

void register_carriers(ENV* env) {
    env->GetClass<DeviceParams>(kDeviceParams);
    env->GetClass<PlatformParams>(kPlatformParams);
    env->GetClass<InitParams>(kInitParams);
    env->GetClass<StartAppParams>(kStartAppParams);

    {
        auto c = env->GetClass(kDeviceParams);
#define F(name) field(env, c, "DeviceParams", #name, &DeviceParams::name)
        F(deviceName);
        F(osVersion);
        F(appBuildVariant);
        F(appVersion);
        F(country);
        F(deviceSku);
        F(displayResolution);
        F(manufacturer);
        F(networkType);
        F(socModel);
        F(testDeviceName);
        F(cpu64Bit);
        F(isChrome);
        F(isLowRamDevice);
        F(deviceTotalMemoryMB);
        F(displayPhysicalWidthPixels);
        F(displayPhysicalHeightPixels);
        F(largeMemoryClass);
        F(memoryClass);
        F(lowMemoryKillerBackgroundAppThreshold);
        F(lowMemoryKillerForegroundAppThreshold);
#undef F
    }
    {
        auto c = env->GetClass(kPlatformParams);
#define F(name) field(env, c, "PlatformParams", #name, &PlatformParams::name)
        F(isKeyboardDevice);
        F(isMouseDevice);
        F(isTouchDevice);
        F(dpiScale);
        F(assetFolderPath);
        F(viewportWidthMm);
        F(viewportHeightMm);
#undef F
    }
    {
        auto c = env->GetClass(kInitParams);
#define F(name)                                                                                                    \
    field(env, c, "InitParams", #name, &InitParams::name);                                                         \
    c->HookInstanceFunction(env, #name, &InitParams::name##_)
        F(baseURL);
        F(buildVariant);
        F(userAgent);
        F(deviceParams);
        F(platformParams);
        F(isPotato);
        F(isTablet);
        F(isVrDevice);
        F(vrContext);
#undef F
        // `pioneerPlaceId()J` is not answered: the engine gets 0 from
        // libjnivm's placeholder, which is the no-pioneer-place answer.
        expect(c, Member::Method, "baseURL", "()Ljava/lang/String;");
        expect(c, Member::Method, "platformParams", "()Lcom/roblox/engine/jni/model/PlatformParams;");
    }
    {
        auto c = env->GetClass(kStartAppParams);
#define F(name)                                                                                                    \
    field(env, c, "StartAppParams", #name, &StartAppParams::name);                                                 \
    c->HookInstanceFunction(env, #name, &StartAppParams::name##_)
        F(appStarterPlace);
        F(appStarterScript);
        F(selectedTheme);
        F(username);
        F(platformParams);
        F(surface);
        F(vrContext);
        F(appUserId);
        F(isUnder13);
        F(membershipType);
#undef F
    }
    {
        auto c = env->GetClass(kDeviceStaticParams);
#define F(name) field(env, c, "DeviceStaticParams", #name, &DeviceStaticParams::name)
        F(osVersion);
        F(deviceName);
        F(appVersion);
        F(manufacturer);
        F(deviceSku);
        F(appBuildVariant);
        F(socModel);
        F(cpu64Bit);
#undef F
    }
    {
        // The fifteen arguments are recorded as the last built spec, and
        // nothing reads them back through fields.
        auto c = env->GetClass(kNativeTextBoxInfo);
        c->Hook(env, "<init>",
                [](ENV* env, jfloat x, jfloat y, jfloat width, jfloat height, jfloat font_size, jboolean multiline,
                   jint x_alignment, jint y_alignment, jint text_color, jint font, jint text_input_type,
                   jint return_key_type, jboolean manual_focus_release, jboolean text_wrapped,
                   jboolean fifteenth) -> std::shared_ptr<NativeTextBoxInfo> {
                    auto info = make<NativeTextBoxInfo>(env);
                    TextBoxSpec& s = info->spec;
                    s.x = x;
                    s.y = y;
                    s.width = width;
                    s.height = height;
                    s.font_size = font_size;
                    s.multiline = multiline ? 1 : 0;
                    s.x_alignment = x_alignment;
                    s.y_alignment = y_alignment;
                    s.text_color = text_color;
                    s.font = font;
                    s.text_input_type = text_input_type;
                    s.return_key_type = return_key_type;
                    s.manual_focus_release = manual_focus_release ? 1 : 0;
                    s.text_wrapped = text_wrapped ? 1 : 0;
                    s.fifteenth = fifteenth ? 1 : 0;
                    info->known = true;
                    textbox_record_built(s);
                    return info;
                });
        expect(c, Member::StaticMethod, "<init>", "(FFFFFZIIIIIIZZZ)Lcom/roblox/engine/jni/model/NativeTextBoxInfo;");
    }
}

void register_engine_built(ENV* env) {
    env->GetClass<BooleanMap>(kMap);
    {
        auto c = env->GetClass(kMap);
        c->HookInstanceFunction(env, "size", &BooleanMap::size);
        c->HookInstanceFunction(env, "isEmpty", &BooleanMap::isEmpty);
    }
    env->GetClass<NativeFlagsInitResult>(kFlagsResult);
    {
        auto c = env->GetClass(kFlagsResult);
        c->Hook(env, "<init>", &NativeFlagsInitResult::create);
        c->HookInstanceFunction(env, "addBoolean", &NativeFlagsInitResult::addBoolean);
        c->HookInstanceFunction(env, "getNativeFlagProviderId", &NativeFlagsInitResult::getNativeFlagProviderId);
        c->HookInstanceFunction(env, "getBooleanCachedMap", &NativeFlagsInitResult::getBooleanCachedMap);
        c->HookInstanceFunction(env, "resolveFlagValue", &NativeFlagsInitResult::resolveFlagValue);
        expect(c, Member::StaticMethod, "<init>", "(I)Lcom/roblox/client/flags/NativeFlagsInitResult;");
        expect(c, Member::Method, "getBooleanCachedMap", "()Ljava/util/Map;");
    }
    env->GetClass<ArrayList>(kArrayList);
    {
        auto c = env->GetClass(kArrayList);
        c->Hook(env, "<init>", [](ENV* env) { return make<ArrayList>(env); });
        // Also on the interface name: lookups do not walk to a superclass.
        for (const char* name : {kArrayList, kList}) {
            auto l = env->GetClass(name);
            l->HookInstanceFunction(env, "size", &ArrayList::size);
            l->HookInstanceFunction(env, "isEmpty", &ArrayList::isEmpty);
            l->HookInstanceFunction(env, "get", &ArrayList::get);
        }
        expect(env->GetClass(kList), Member::Method, "get", "(I)Ljava/lang/Object;");
    }
    env->GetClass<JsonObject>(kJsonObject);
    {
        auto c = env->GetClass(kJsonObject);
        c->Hook(env, "<init>", [](ENV* env) { return make<JsonObject>(env); });
        c->HookInstanceFunction(env, "length", &JsonObject::length);
    }
    env->GetClass<ClientLocalFlags>(kLocalFlags);
    {
        auto c = env->GetClass(kLocalFlags);
        c->Hook(env, "<init>", [](ENV* env) { return make<ClientLocalFlags>(env); });
        c->HookInstanceFunction(env, "add", &ClientLocalFlags::add);
        c->HookInstanceFunction(env, "isEmpty", &ClientLocalFlags::isEmpty);
        c->HookInstanceFunction(env, "size", &ClientLocalFlags::size);
        c->HookInstanceFunction(env, "getAll", &ClientLocalFlags::getAll);
    }
}

void register_display_classes(ENV* env) {
    env->GetClass<Locale>(kLocale);
    {
        auto c = env->GetClass(kLocale);
        c->HookInstanceFunction(env, "getLanguage", &Locale::getLanguage);
        c->HookInstanceFunction(env, "getCountry", &Locale::getCountry);
        c->HookInstanceFunction(env, "getScript", &Locale::getScript);
        c->HookInstanceFunction(env, "getVariant", &Locale::getVariant);
        c->HookInstanceFunction(env, "toString", &Locale::toString);
    }
    env->GetClass<LocaleList>(kLocaleList);
    {
        auto c = env->GetClass(kLocaleList);
        c->HookInstanceFunction(env, "size", &LocaleList::size);
        c->HookInstanceFunction(env, "isEmpty", &LocaleList::isEmpty);
        c->HookInstanceFunction(env, "get", &LocaleList::get);
    }
    {
        auto c = env->GetClass(kDisplayMetrics);
        c->HookInstance(env, "density", &DisplayMetrics::density);
        c->HookInstance(env, "scaledDensity", &DisplayMetrics::scaledDensity);
        c->HookInstance(env, "xdpi", &DisplayMetrics::xdpi);
        c->HookInstance(env, "ydpi", &DisplayMetrics::ydpi);
        c->HookInstance(env, "densityDpi", &DisplayMetrics::densityDpi);
        c->HookInstance(env, "widthPixels", &DisplayMetrics::widthPixels);
        c->HookInstance(env, "heightPixels", &DisplayMetrics::heightPixels);
    }
    {
        auto c = env->GetClass(kConfiguration);
#define F(name) c->HookInstance(env, #name, &Configuration::name)
        F(fontScale);
        F(densityDpi);
        F(screenWidthDp);
        F(screenHeightDp);
        F(smallestScreenWidthDp);
        F(orientation);
        F(screenLayout);
        F(uiMode);
        F(colorMode);
        F(touchscreen);
        F(keyboard);
        F(keyboardHidden);
        F(hardKeyboardHidden);
        F(navigation);
        F(navigationHidden);
        F(mcc);
        F(mnc);
        F(fontWeightAdjustment);
#undef F
        // Whatever the receiver: AGDK asks the Configuration it was given.
        c->HookInstanceFunction(env, "getLocales",
                                [](ENV* env, Object*) -> std::shared_ptr<LocaleList> { return make<LocaleList>(env); });
        expect(c, Member::Method, "getLocales", "()Landroid/os/LocaleList;");
    }
    {
        auto c = env->GetClass(kPoint);
        c->HookInstance(env, "x", &Point::x);
        c->HookInstance(env, "y", &Point::y);
    }
    {
        // `getScreenPhysicalSizeInMillimeters(Context)`, as the engine asks
        // it: the display's physical size the host measured, or null while it
        // is unknown, which the engine handles ("returned null").
        auto c = env->GetClass(kDeviceUtils);
        c->Hook(env, "getScreenPhysicalSizeInMillimeters", [](ENV* env, ContextArg) -> std::shared_ptr<Point> {
            int width_mm = 0;
            int height_mm = 0;
            if (!physical_mm(&width_mm, &height_mm)) {
                return nullptr;
            }
            auto p = make<Point>(env);
            p->x = width_mm;
            p->y = height_mm;
            return p;
        });
        expect(c, Member::StaticMethod, "getScreenPhysicalSizeInMillimeters",
               "(Landroid/content/Context;)Landroid/graphics/Point;");
    }
    env->GetClass<Rect>(kRect);
    {
        auto c = env->GetClass(kRect);
        c->Hook(env, "<init>", [](ENV* env) { return make<Rect>(env); });
        c->Hook(env, "<init>", [](ENV* env, jint left, jint top, jint right, jint bottom) {
            auto r = make<Rect>(env);
            r->left = left;
            r->top = top;
            r->right = right;
            r->bottom = bottom;
            return r;
        });
        c->HookInstance(env, "left", &Rect::left);
        c->HookInstance(env, "top", &Rect::top);
        c->HookInstance(env, "right", &Rect::right);
        c->HookInstance(env, "bottom", &Rect::bottom);
    }
}

// ------------------------------------------------------------ host operations

/// Objects the engine was handed and might hold past the call without a
/// global reference: kept until the next delivery of the same kind, so a
/// stray local reference stays valid. Each host operation pops its own local
/// frame, which is what keeps a periodic re-send of the 1.5 MB settings
/// document from accumulating.
enum Slot { kInitSlot, kStartSlot, kSurfaceApp, kSurfaceGame, kAssetObject, kStorageStatic, kStorageObjects, kSlotCount };
std::mutex g_kept_mutex;
std::vector<std::shared_ptr<Object>> g_kept[kSlotCount];

void keep(Slot slot, std::vector<std::shared_ptr<Object>> objects) {
    std::lock_guard<std::mutex> lock(g_kept_mutex);
    g_kept[slot] = std::move(objects);
}

/// The shared shape of every bridge call: a VM and a native, a local frame
/// for the objects built for the call, and no exception into the host.
template <class F>
int bridge(void* fn, char* err, size_t err_len, F&& body) {
    ENV* env = intoxicated::process_env();
    if (!fn || !env) {
        report(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    return contained(err, err_len, [&] {
        LocalFrame frame(env, 16);
        return body(env, env->GetJNIEnv());
    });
}

jstring jtext(ENV* env, const char* s) {
    return reinterpret_cast<jstring>(to_jni(env, jstr(s ? s : "")));
}

} // namespace

void register_param_classes(ENV* env) {
    declare_shared_classes(env);
    register_carriers(env);
    register_engine_built(env);
    register_display_classes(env);
    if (getenv("INTOXICATED_JNI_TRACE")) {
        fprintf(stderr, "[classes] parameter objects registered\n");
    }
}

} // namespace java
} // namespace intoxicated

using namespace intoxicated;
using namespace intoxicated::java;

extern "C" {

/// `MainGameActivity.nativeAppBridgeSetInitParams(InitParams)`, an instance
/// native, on a plain object.
int intoxicated_set_init_params(void* fn, const char* assets, int width, int height, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto self = std::make_shared<Object>();
        auto params = build_init_params(env, assets ? assets : "", width, height);
        reinterpret_cast<void (*)(JNIEnv*, jobject, jobject)>(fn)(jni, to_jni(env, self), to_jni(env, params));
        keep(kInitSlot, {params});
        return 0;
    });
}

/// `NativeGLInterface.nativeAppBridgeV2InitWithParams(InitParams)`.
int intoxicated_appbridge_init(void* fn, const char* assets, int width, int height, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto params = build_init_params(env, assets ? assets : "", width, height);
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject)>(fn)(jni, find_class(env, kNativeGL),
                                                                  to_jni(env, params));
        keep(kInitSlot, {params});
        return 0;
    });
}

/// `nativeAppBridgeV2StartAppWithParams(StartAppParams)`, with a new Surface.
int intoxicated_appbridge_start_app(void* fn, const char* assets, int width, int height, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto surface = make<Surface>(env);
        auto params = build_start_app_params(env, assets ? assets : "", width, height, surface);
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject)>(fn)(jni, find_class(env, kNativeGL),
                                                                  to_jni(env, params));
        keep(kStartSlot, {params});
        return 0;
    });
}

/// `nativeAppBridgeV2UpdateSurfaceAppWithPlatformParams(Surface, PlatformParams)`.
int intoxicated_appbridge_update_surface_app(void* fn, const char* assets, int width, int height, char* err,
                                             size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto surface = make<Surface>(env);
        auto params = build_platform_params(env, assets ? assets : "", width, height);
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject, jobject)>(fn)(jni, find_class(env, kNativeGL),
                                                                           to_jni(env, surface), to_jni(env, params));
        keep(kSurfaceApp, {surface, params});
        return 0;
    });
}

/// `nativeAppBridgeV2UpdateSurfaceGameWithPlatformParams(Surface, PlatformParams, Activity)`.
int intoxicated_appbridge_update_surface_game(void* fn, const char* assets, int width, int height, char* err,
                                              size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto surface = make<Surface>(env);
        auto params = build_platform_params(env, assets ? assets : "", width, height);
        auto activity = make<Activity>(env);
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject, jobject, jobject)>(fn)(
            jni, find_class(env, kNativeGL), to_jni(env, surface), to_jni(env, params), to_jni(env, activity));
        keep(kSurfaceGame, {surface, params, activity});
        return 0;
    });
}

/// `NativeSettingsInterface.nativeSetDeviceInfo(DeviceParams)`.
int intoxicated_set_device_info(void* fn, int width, int height, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto params = build_device_params(env, width, height);
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject)>(fn)(jni, find_class(env, kNativeSettings),
                                                                  to_jni(env, params));
        return 0;
    });
}

/// `JNIAAssetManagerSetup.initNative(AssetManager)`, with a plain object.
int intoxicated_asset_manager_init(void* fn, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto object = std::make_shared<Object>();
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject)>(fn)(jni, find_class(env, kAssetSetup),
                                                                  to_jni(env, object));
        keep(kAssetObject, {object});
        return 0;
    });
}

/// `LocalStorageManager.initStorageManagerNativeV3`, static form: the class,
/// a plain object and two strings.
int intoxicated_storage_init(void* fn, const char* a, const char* b, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto object = std::make_shared<Object>();
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject, jstring, jstring)>(fn)(
            jni, find_class(env, kStorageManager), to_jni(env, object), jtext(env, a), jtext(env, b));
        keep(kStorageStatic, {object});
        return 0;
    });
}

/// The same native, instance form: a LocalStorageManager, an AssetManager and
/// two strings.
int intoxicated_init_storage_manager(void* fn, const char* a, const char* b, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto manager = instance_of(env, find_class(env, kStorageManager));
        auto assets = make<AssetManager>(env);
        reinterpret_cast<void (*)(JNIEnv*, jobject, jobject, jstring, jstring)>(fn)(
            jni, to_jni(env, manager), to_jni(env, assets), jtext(env, a), jtext(env, b));
        keep(kStorageObjects, {manager, assets});
        return 0;
    });
}

/// `FlagJniInterface.nativeInitializeNativeFlags(String[])`: one name per
/// line of `names`, trailing CR and spaces dropped, blank lines skipped. The
/// result object is the engine's and is not read here.
int intoxicated_init_flags(void* fn, const char* names, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        std::vector<std::string> list;
        const std::string text = names ? names : "";
        size_t start = 0;
        while (start <= text.size()) {
            size_t end = text.find('\n', start);
            if (end == std::string::npos) {
                end = text.size();
            }
            std::string line = text.substr(start, end - start);
            while (!line.empty() && (line.back() == '\r' || line.back() == ' ')) {
                line.pop_back();
            }
            if (!line.empty()) {
                list.push_back(line);
            }
            start = end + 1;
        }
        auto array = std::make_shared<jnivm::Array<String>>(static_cast<jsize>(list.size()));
        for (size_t i = 0; i < list.size(); ++i) {
            (*array)[static_cast<jint>(i)] = jstr(list[i]);
        }
        reinterpret_cast<jobject (*)(JNIEnv*, jclass, jobjectArray)>(fn)(jni, find_class(env, kFlagJni),
                                                                          to_jni(env, array));
        return 0;
    });
}

/// `NativeGLInterface.readLocalFlags()`. The engine's answer is not used.
int intoxicated_read_local_flags(void* fn, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        reinterpret_cast<jobject (*)(JNIEnv*, jclass)>(fn)(jni, find_class(env, kNativeGL));
        return 0;
    });
}

/// `NativeGLInterface.nativeInitClientSettings(String, String, String)I`.
int intoxicated_init_client_settings(void* fn, const char* a, const char* b, const char* c, int* out_result, char* err,
                                     size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        const jint r = reinterpret_cast<jint (*)(JNIEnv*, jclass, jstring, jstring, jstring)>(fn)(
            jni, find_class(env, kNativeGL), jtext(env, a), jtext(env, b), jtext(env, c));
        if (out_result) {
            *out_result = r;
        }
        return 0;
    });
}

/// `nativePostClientSettingsLoadedInitialization3(List)`, with an empty list.
int intoxicated_post_client_settings_loaded(void* fn, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto list = make<ArrayList>(env);
        reinterpret_cast<void (*)(JNIEnv*, jclass, jobject)>(fn)(jni, find_class(env, kNativeGL), to_jni(env, list));
        return 0;
    });
}

/// `FlagJniInterface.nativeGetFInt(String, int)I`.
int intoxicated_get_fint(void* fn, const char* name, int fallback, int* out_result, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        const jint r = reinterpret_cast<jint (*)(JNIEnv*, jclass, jstring, jint)>(fn)(
            jni, find_class(env, kFlagJni), jtext(env, name), fallback);
        if (out_result) {
            *out_result = r;
        }
        return 0;
    });
}

/// `MainGameActivity.nativePreloadFlagOverrides(String)`, an instance native,
/// on a plain object, with the text unchanged.
int intoxicated_preload_flag_overrides(void* fn, const char* json, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto self = std::make_shared<Object>();
        reinterpret_cast<void (*)(JNIEnv*, jobject, jstring)>(fn)(jni, to_jni(env, self), jtext(env, json));
        return 0;
    });
}

/// A bare static native on `NativeGLInterface`: nativeGameGlobalInit,
/// nativeUpdateAdapterInit, nativeAppBridgeStartLuaAppDM.
int intoxicated_appbridge_call_bare(void* fn, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        reinterpret_cast<void (*)(JNIEnv*, jclass)>(fn)(jni, find_class(env, kNativeGL));
        return 0;
    });
}

/// A bare static native on a named class.
int intoxicated_appbridge_call_bare_cls(void* fn, const char* class_name, char* err, size_t err_len) {
    if (!class_name) {
        report(err, err_len, "no class name");
        return -1;
    }
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        reinterpret_cast<void (*)(JNIEnv*, jclass)>(fn)(jni, find_class(env, class_name));
        return 0;
    });
}

/// `NativeAppBridgeInterface.nativeAppBridgeAppStart(String, String, boolean,
/// String, String, String)`, which is where the engine builds its start-up
/// controller.
int intoxicated_appbridge_app_start(void* fn, const char* a, const char* b, int flag, const char* c, const char* d,
                                    const char* e, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jstring, jstring, jboolean, jstring, jstring, jstring)>(fn)(
            jni, find_class(env, kAppBridge), jtext(env, a), jtext(env, b), flag ? JNI_TRUE : JNI_FALSE,
            jtext(env, c), jtext(env, d), jtext(env, e));
        return 0;
    });
}

/// An instance native taking nothing (nativeRetryInit), on a plain object.
int intoxicated_call_bare(void* fn, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        auto self = std::make_shared<Object>();
        reinterpret_cast<void (*)(JNIEnv*, jobject)>(fn)(jni, to_jni(env, self));
        return 0;
    });
}

/// `JNIActivityLifecycleCallbacks.nativeOn*(String)`, given an activity name.
int intoxicated_activity_lifecycle(void* fn, const char* activity, char* err, size_t err_len) {
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jstring)>(fn)(jni, find_class(env, kLifecycle),
                                                                  jtext(env, activity));
        return 0;
    });
}

/// A static native on a named class taking zero to three Strings; a null
/// string is passed as empty.
int intoxicated_call_static_strings(void* fn, const char* class_name, const char* const* args, size_t count, char* err,
                                    size_t err_len) {
    if (count > 3) {
        report(err, err_len, "more than three String arguments is not a call shape this bridge has");
        return -1;
    }
    if (!class_name || (count > 0 && !args)) {
        report(err, err_len, "no class name, or no argument list");
        return -1;
    }
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        const jclass cls = find_class(env, class_name);
        jstring s[3] = {nullptr, nullptr, nullptr};
        for (size_t i = 0; i < count; ++i) {
            s[i] = jtext(env, args[i]);
        }
        switch (count) {
        case 0:
            reinterpret_cast<void (*)(JNIEnv*, jclass)>(fn)(jni, cls);
            break;
        case 1:
            reinterpret_cast<void (*)(JNIEnv*, jclass, jstring)>(fn)(jni, cls, s[0]);
            break;
        case 2:
            reinterpret_cast<void (*)(JNIEnv*, jclass, jstring, jstring)>(fn)(jni, cls, s[0], s[1]);
            break;
        default:
            reinterpret_cast<void (*)(JNIEnv*, jclass, jstring, jstring, jstring)>(fn)(jni, cls, s[0], s[1], s[2]);
            break;
        }
        return 0;
    });
}

/// A static `(boolean, String)` native on a named class.
int intoxicated_call_static_bool_string(void* fn, const char* class_name, int flag, const char* text, char* err,
                                        size_t err_len) {
    if (!class_name) {
        report(err, err_len, "no class name");
        return -1;
    }
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        reinterpret_cast<void (*)(JNIEnv*, jclass, jboolean, jstring)>(fn)(
            jni, find_class(env, class_name), flag ? JNI_TRUE : JNI_FALSE, jtext(env, text));
        return 0;
    });
}

/// A static `()Z` native on a named class; its answer in `*out_result`.
int intoxicated_call_static_bare_bool(void* fn, const char* class_name, int* out_result, char* err, size_t err_len) {
    if (!class_name) {
        report(err, err_len, "no class name");
        return -1;
    }
    return bridge(fn, err, err_len, [&](ENV* env, JNIEnv* jni) {
        const jboolean r = reinterpret_cast<jboolean (*)(JNIEnv*, jclass)>(fn)(jni, find_class(env, class_name));
        if (out_result) {
            *out_result = r ? 1 : 0;
        }
        return 0;
    });
}

} // extern "C"
