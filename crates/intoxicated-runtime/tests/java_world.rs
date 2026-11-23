//! The Java world the engine sees, checked member by member.
//!
//! libjnivm finds a method or field only by exact name, descriptor and static
//! flag, and anything it does not find still resolves, to a placeholder that
//! answers zero or null. A hook registered under the wrong descriptor
//! therefore fails silently, as two did for months
//! (`GameActivity.getWindowInsets` and
//! `DeviceUtils.getScreenPhysicalSizeInMillimeters`). This creates the
//! process's JavaVM, which registers every class, and asks for each member
//! the engine was measured using (the class dump of a signed-in launch of
//! engine 2.748), with the descriptor it asks for.
//!
//! One test in its own binary: the VM is process-wide and created once.

use std::ffi::{c_char, c_int, CString};

use intoxicated_linker_sys::jni;

// The native archives call back into the runtime (libjnivm's logger reports
// unresolved lookups to its `unimplemented` table), so the runtime has to be
// linked in although nothing here names it.
extern crate intoxicated_runtime;

extern "C" {
    fn intoxicated_java_member_answered(
        class_name: *const c_char,
        kind: c_int,
        name: *const c_char,
        descriptor: *const c_char,
    ) -> c_int;
}

#[derive(Clone, Copy, Debug)]
enum Member {
    Method = 0,
    StaticMethod = 1,
    Field = 2,
    StaticField = 3,
}
use Member::*;

/// Whether the member answers, looked up the way libjnivm looks it up: a
/// constructor `<init>(ARGS)V` is rewritten into a static factory returning
/// its class, `<init>(ARGS)L<class>;`.
fn answered(class: &str, kind: Member, name: &str, descriptor: &str) -> bool {
    let descriptor = match (name, descriptor.strip_suffix('V')) {
        ("<init>", Some(args)) => format!("{args}L{class};"),
        _ => descriptor.to_string(),
    };
    let (class, name, descriptor) =
        (CString::new(class).unwrap(), CString::new(name).unwrap(), CString::new(descriptor).unwrap());
    // SAFETY: three NUL-terminated strings that outlive the call; the C side
    // only looks.
    let r = unsafe {
        intoxicated_java_member_answered(class.as_ptr(), kind as c_int, name.as_ptr(), descriptor.as_ptr())
    };
    assert!(r >= 0, "no JavaVM");
    r == 1
}

/// Every member the engine was measured reaching for on the classes this
/// client answers, minus the lookups nobody answers on purpose (the purchase
/// variants with no billing behind them, `InitParams.pioneerPlaceId`,
/// `GameActivity.finish`, `getAppUpgradeKey` and `syncCookiesFromEngine`).
const ANSWERED: &[(&str, &[(Member, &str, &str)])] = &[
    (
        "java/lang/ClassLoader",
        &[
            (Method, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;"),
            (Method, "findClass", "(Ljava/lang/String;)Ljava/lang/Class;"),
        ],
    ),
    (
        "java/util/Locale",
        &[
            (Method, "getLanguage", "()Ljava/lang/String;"),
            (Method, "getCountry", "()Ljava/lang/String;"),
            (Method, "getScript", "()Ljava/lang/String;"),
            (Method, "getVariant", "()Ljava/lang/String;"),
            (Method, "toString", "()Ljava/lang/String;"),
        ],
    ),
    (
        "java/util/Map",
        &[
            (Method, "size", "()I"),
            (Method, "isEmpty", "()Z"),
        ],
    ),
    (
        "java/util/ArrayList",
        &[
            (StaticMethod, "<init>", "()V"),
            (Method, "size", "()I"),
            (Method, "isEmpty", "()Z"),
            (Method, "get", "(I)Ljava/lang/Object;"),
        ],
    ),
    (
        "java/util/List",
        &[
            (Method, "size", "()I"),
            (Method, "isEmpty", "()Z"),
            (Method, "get", "(I)Ljava/lang/Object;"),
        ],
    ),
    (
        "com/roblox/engine/jni/NativeGLJavaInterface",
        &[
            (StaticMethod, "gameLoadedCallback", "(J)V"),
            (StaticMethod, "getDeviceStaticParams", "()Lcom/roblox/engine/jni/model/DeviceStaticParams;"),
            (StaticMethod, "showKeyboard", "(JZ[BLcom/roblox/engine/jni/model/NativeTextBoxInfo;)V"),
            (StaticMethod, "hideKeyboard", "()V"),
            (StaticMethod, "promptNativePurchase", "(JLjava/lang/String;Ljava/lang/String;)V"),
            (StaticMethod, "promptNativePurchaseWithPayload", "(JLjava/lang/String;Ljava/lang/String;)V"),
            (StaticMethod, "exitGameWithError", "(I)V"),
            (StaticMethod, "gameDidLeave", "()V"),
            (StaticMethod, "onAppShellReloadNeeded", "()V"),
            (StaticMethod, "onLuaTextBoxChangedCallback", "(Ljava/lang/String;)V"),
            (StaticMethod, "onLuaTextBoxPropertyChangedCallback", "()V"),
            (StaticMethod, "listenToMotionEvents", "(Ljava/lang/String;)V"),
            (StaticMethod, "screenOrientationChanged", "(I)V"),
            (StaticMethod, "openNativeOverlay", "(Ljava/lang/String;Ljava/lang/String;)V"),
            (StaticMethod, "onAppBridgeNotification", "(Ljava/lang/String;Ljava/lang/String;)V"),
            (StaticMethod, "onDataModelNotificationCallback", "(Ljava/lang/String;Ljava/lang/String;)V"),
            (StaticMethod, "getWebViewUserAgent", "()V"),
        ],
    ),
    (
        "com/roblox/engine/jni/model/DeviceStaticParams",
        &[
            (Field, "osVersion", "Ljava/lang/String;"),
            (Field, "deviceName", "Ljava/lang/String;"),
            (Field, "appVersion", "Ljava/lang/String;"),
            (Field, "manufacturer", "Ljava/lang/String;"),
            (Field, "deviceSku", "Ljava/lang/String;"),
            (Field, "appBuildVariant", "Ljava/lang/String;"),
            (Field, "socModel", "Ljava/lang/String;"),
            (Field, "cpu64Bit", "Z"),
        ],
    ),
    (
        "com/roblox/engine/jni/model/NativeTextBoxInfo",
        &[
            (StaticMethod, "<init>", "(FFFFFZIIIIIIZZZ)V"),
        ],
    ),
    (
        "com/roblox/engine/jni/model/ClientLocalFlags",
        &[
            (StaticMethod, "<init>", "()V"),
            (Method, "add", "(Ljava/lang/String;Ljava/lang/String;)V"),
            (Method, "isEmpty", "()Z"),
            (Method, "size", "()I"),
            (Method, "getAll", "()Lorg/json/JSONObject;"),
        ],
    ),
    (
        "com/roblox/engine/jni/model/DeviceParams",
        &[
            (Field, "deviceName", "Ljava/lang/String;"),
            (Field, "osVersion", "Ljava/lang/String;"),
            (Field, "appBuildVariant", "Ljava/lang/String;"),
            (Field, "appVersion", "Ljava/lang/String;"),
            (Field, "country", "Ljava/lang/String;"),
            (Field, "deviceSku", "Ljava/lang/String;"),
            (Field, "displayResolution", "Ljava/lang/String;"),
            (Field, "manufacturer", "Ljava/lang/String;"),
            (Field, "networkType", "Ljava/lang/String;"),
            (Field, "socModel", "Ljava/lang/String;"),
            (Field, "testDeviceName", "Ljava/lang/String;"),
            (Field, "cpu64Bit", "Z"),
            (Field, "isChrome", "Z"),
            (Field, "isLowRamDevice", "Z"),
            (Field, "deviceTotalMemoryMB", "I"),
            (Field, "displayPhysicalWidthPixels", "I"),
            (Field, "displayPhysicalHeightPixels", "I"),
            (Field, "largeMemoryClass", "I"),
            (Field, "memoryClass", "I"),
            (Field, "lowMemoryKillerBackgroundAppThreshold", "J"),
            (Field, "lowMemoryKillerForegroundAppThreshold", "J"),
        ],
    ),
    (
        "com/roblox/engine/jni/model/PlatformParams",
        &[
            (Field, "isKeyboardDevice", "Z"),
            (Field, "isMouseDevice", "Z"),
            (Field, "isTouchDevice", "Z"),
            (Field, "dpiScale", "F"),
            (Field, "assetFolderPath", "Ljava/lang/String;"),
            (Field, "viewportWidthMm", "I"),
            (Field, "viewportHeightMm", "I"),
        ],
    ),
    (
        "com/roblox/engine/jni/locale/NativeLocaleJavaInterface",
        &[
            (StaticMethod, "getLocale", "()Ljava/lang/String;"),
            (StaticMethod, "getRobloxLocale", "()Ljava/lang/String;"),
            (StaticMethod, "getGameLocale", "()Ljava/lang/String;"),
        ],
    ),
    (
        "com/roblox/engine/jni/user/NativeUserJavaInterface",
        &[
            (StaticMethod, "getUserId", "()J"),
            (StaticMethod, "getIsUnder13", "()Z"),
            (StaticMethod, "getUsername", "()Ljava/lang/String;"),
            (StaticMethod, "getDisplayName", "()Ljava/lang/String;"),
            (StaticMethod, "getAlternateName", "()Ljava/lang/String;"),
            (StaticMethod, "getPlatformName", "()Ljava/lang/String;"),
            (StaticMethod, "getMembershipType", "()I"),
            (StaticMethod, "getHasRobloxSubscription", "()Z"),
            (StaticMethod, "getTheme", "()Ljava/lang/String;"),
        ],
    ),
    (
        "com/roblox/engine/jni/reporter/SessionReporterJavaInterface",
        &[
            (StaticMethod, "getFilesDir", "()Ljava/lang/String;"),
            (StaticMethod, "getAppVersion", "()Ljava/lang/String;"),
            (StaticMethod, "getLastLoggedInUser", "()Ljava/lang/String;"),
            (StaticMethod, "getLastLoggedInUserId", "()Ljava/lang/String;"),
            (StaticMethod, "sendSessionReport", "(Ljava/lang/String;Ljava/lang/String;)V"),
            (StaticMethod, "setEventTrackingGoogleAnalytics", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;J)V"),
        ],
    ),
    (
        "com/roblox/engine/jni/video/MediaCodecInfoUtils",
        &[
            (StaticMethod, "getVideoCodecs", "()[Lcom/roblox/engine/jni/video/VideoCodecCapability;"),
            (StaticMethod, "hevcHardwareEncodingSupported", "(III)Z"),
        ],
    ),
    (
        "com/roblox/engine/jni/autovalue/InitParams",
        &[
            (Field, "baseURL", "Ljava/lang/String;"),
            (Field, "buildVariant", "Ljava/lang/String;"),
            (Field, "userAgent", "Ljava/lang/String;"),
            (Field, "deviceParams", "Lcom/roblox/engine/jni/model/DeviceParams;"),
            (Field, "platformParams", "Lcom/roblox/engine/jni/model/PlatformParams;"),
            (Field, "vrContext", "Landroid/app/Activity;"),
            (Field, "isPotato", "Z"),
            (Field, "isTablet", "Z"),
            (Field, "isVrDevice", "Z"),
            (Method, "baseURL", "()Ljava/lang/String;"),
            (Method, "buildVariant", "()Ljava/lang/String;"),
            (Method, "userAgent", "()Ljava/lang/String;"),
            (Method, "deviceParams", "()Lcom/roblox/engine/jni/model/DeviceParams;"),
            (Method, "platformParams", "()Lcom/roblox/engine/jni/model/PlatformParams;"),
            (Method, "isPotato", "()Z"),
            (Method, "isTablet", "()Z"),
            (Method, "isVrDevice", "()Z"),
            (Method, "vrContext", "()Landroid/app/Activity;"),
        ],
    ),
    (
        "com/roblox/engine/jni/autovalue/StartAppParams",
        &[
            (Field, "appStarterPlace", "Ljava/lang/String;"),
            (Field, "appStarterScript", "Ljava/lang/String;"),
            (Field, "selectedTheme", "Ljava/lang/String;"),
            (Field, "username", "Ljava/lang/String;"),
            (Field, "platformParams", "Lcom/roblox/engine/jni/model/PlatformParams;"),
            (Field, "surface", "Landroid/view/Surface;"),
            (Field, "vrContext", "Landroid/app/Activity;"),
            (Field, "appUserId", "J"),
            (Field, "isUnder13", "Z"),
            (Field, "membershipType", "I"),
            (Method, "appStarterPlace", "()Ljava/lang/String;"),
            (Method, "appStarterScript", "()Ljava/lang/String;"),
            (Method, "selectedTheme", "()Ljava/lang/String;"),
            (Method, "username", "()Ljava/lang/String;"),
            (Method, "platformParams", "()Lcom/roblox/engine/jni/model/PlatformParams;"),
            (Method, "surface", "()Landroid/view/Surface;"),
            (Method, "vrContext", "()Landroid/app/Activity;"),
            (Method, "appUserId", "()J"),
            (Method, "isUnder13", "()Z"),
            (Method, "membershipType", "()I"),
        ],
    ),
    (
        "com/roblox/universalapp/logging/LoggingProtocol",
        &[
            (StaticMethod, "getProcessTimestamp", "()J"),
        ],
    ),
    (
        "com/roblox/client/LocalStorageManager",
        &[
            (Method, "getAllocatableBytes", "()J"),
        ],
    ),
    (
        "com/roblox/client/flags/NativeFlagsInitResult",
        &[
            (StaticMethod, "<init>", "(I)V"),
            (Method, "addBoolean", "(Ljava/lang/String;ZZ)V"),
            (Method, "getNativeFlagProviderId", "()I"),
            (Method, "getBooleanCachedMap", "()Ljava/util/Map;"),
            (Method, "resolveFlagValue", "(Ljava/lang/String;)Z"),
        ],
    ),
    (
        "com/roblox/client/startup/NativeHelper",
        &[
            (Method, "gameActivity_onFlagsFailed", "()V"),
            (Method, "gameActivity_onFlagsLoaded", "(Ljava/nio/ByteBuffer;)V"),
            (Method, "gameActivity_onAppReady", "(Ljava/lang/String;)V"),
            (Method, "gameActivity_onExperienceStart", "()V"),
            (Method, "gameActivity_onGameLoaded", "(J)V"),
            (Method, "gameActivity_onDidLogInReceived", "(Ljava/lang/String;)V"),
            (Method, "gameActivity_onScreenOrientationChanged", "(IZ)V"),
            (Method, "gameActivity_onEngineInitialized", "()V"),
            (Method, "gameActivity_onDidLogOutReceived", "()V"),
            (Method, "gameActivity_onDidSwitchAccountReceived", "()V"),
            (Method, "gameActivity_onLuaAppDidReturn", "()V"),
            (Method, "gameActivity_onRestartLuaApp", "()V"),
            (Method, "gameActivity_onScanQrCode", "()V"),
            (Method, "gameActivity_onDidSignUp", "(Ljava/lang/String;)V"),
            (Method, "gameActivity_onGameStreamingStatusChanged", "(Ljava/lang/String;)V"),
            (Method, "gameActivity_onScreenshotReady", "(Ljava/lang/String;)V"),
            (Method, "gameActivity_onMotionEventListening", "(Ljava/lang/String;)V"),
            (Method, "gameActivity_onExperienceStop", "(D)V"),
            (Method, "gameActivity_setAppUpgradeStatus", "(IILjava/lang/String;Ljava/lang/String;)V"),
        ],
    ),
    (
        "com/roblox/client/startup/MainGameActivity",
        &[
            (Method, "getResources", "()Landroid/content/res/Resources;"),
            (Method, "getNativeHelper", "()Lcom/roblox/client/startup/NativeHelper;"),
        ],
    ),
    (
        "com/roblox/platform/util/DeviceUtils",
        &[
            (StaticMethod, "getScreenPhysicalSizeInMillimeters", "(Landroid/content/Context;)Landroid/graphics/Point;"),
        ],
    ),
    (
        "com/google/androidgamesdk/GameActivity",
        &[
            (Method, "setImeEditorInfoFields", "(III)V"),
            (Method, "setWindowFlags", "(II)V"),
            (Method, "getWindowInsets", "(I)Landroidx/core/graphics/Insets;"),
            (Method, "getWaterfallInsets", "()Landroidx/core/graphics/Insets;"),
            (Method, "bootstrapTheApp", "()V"),
            (Method, "getResources", "()Landroid/content/res/Resources;"),
            (Method, "getNativeHelper", "()Lcom/roblox/client/startup/NativeHelper;"),
        ],
    ),
    (
        "com/google/androidgamesdk/gametextinput/State",
        &[
            (Field, "text", "Ljava/lang/String;"),
            (Field, "selectionStart", "I"),
            (Field, "selectionEnd", "I"),
            (Field, "composingRegionStart", "I"),
            (Field, "composingRegionEnd", "I"),
        ],
    ),
    (
        "com/google/androidgamesdk/gametextinput/InputConnection",
        &[
            (Method, "setState", "(Lcom/google/androidgamesdk/gametextinput/State;)V"),
            (Method, "setSoftKeyboardActive", "(ZI)V"),
            (Method, "restartInput", "()V"),
        ],
    ),
    (
        "android/os/Build",
        &[
            (StaticField, "MANUFACTURER", "Ljava/lang/String;"),
            (StaticField, "MODEL", "Ljava/lang/String;"),
            (StaticField, "BOARD", "Ljava/lang/String;"),
            (StaticField, "BOOTLOADER", "Ljava/lang/String;"),
            (StaticField, "BRAND", "Ljava/lang/String;"),
            (StaticField, "DEVICE", "Ljava/lang/String;"),
            (StaticField, "FINGERPRINT", "Ljava/lang/String;"),
            (StaticField, "HARDWARE", "Ljava/lang/String;"),
            (StaticField, "PRODUCT", "Ljava/lang/String;"),
            (StaticField, "TAGS", "Ljava/lang/String;"),
            (StaticField, "TYPE", "Ljava/lang/String;"),
            (StaticField, "ID", "Ljava/lang/String;"),
            (StaticField, "DISPLAY", "Ljava/lang/String;"),
            (StaticField, "HOST", "Ljava/lang/String;"),
            (StaticField, "USER", "Ljava/lang/String;"),
        ],
    ),
    (
        "android/os/Build$VERSION",
        &[
            (StaticField, "SDK_INT", "I"),
        ],
    ),
    (
        "android/os/Debug",
        &[
            (StaticMethod, "isDebuggerConnected", "()Z"),
        ],
    ),
    (
        "android/os/LocaleList",
        &[
            (Method, "size", "()I"),
            (Method, "isEmpty", "()Z"),
            (Method, "get", "(I)Ljava/util/Locale;"),
        ],
    ),
    (
        "android/graphics/Rect",
        &[
            (Field, "left", "I"),
            (Field, "top", "I"),
            (Field, "right", "I"),
            (Field, "bottom", "I"),
            (StaticMethod, "<init>", "()V"),
            (StaticMethod, "<init>", "(IIII)V"),
        ],
    ),
    (
        "android/graphics/Point",
        &[
            (Field, "x", "I"),
            (Field, "y", "I"),
        ],
    ),
    (
        "android/content/SharedPreferences",
        &[
            (Method, "edit", "()Landroid/content/SharedPreferences$Editor;"),
            (Method, "getString", "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"),
            (Method, "getInt", "(Ljava/lang/String;I)I"),
            (Method, "getLong", "(Ljava/lang/String;J)J"),
            (Method, "getFloat", "(Ljava/lang/String;F)F"),
            (Method, "getBoolean", "(Ljava/lang/String;Z)Z"),
            (Method, "contains", "(Ljava/lang/String;)Z"),
        ],
    ),
    (
        "android/content/SharedPreferences$Editor",
        &[
            (Method, "putString", "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/SharedPreferences$Editor;"),
            (Method, "putInt", "(Ljava/lang/String;I)Landroid/content/SharedPreferences$Editor;"),
            (Method, "putLong", "(Ljava/lang/String;J)Landroid/content/SharedPreferences$Editor;"),
            (Method, "putFloat", "(Ljava/lang/String;F)Landroid/content/SharedPreferences$Editor;"),
            (Method, "putBoolean", "(Ljava/lang/String;Z)Landroid/content/SharedPreferences$Editor;"),
            (Method, "remove", "(Ljava/lang/String;)Landroid/content/SharedPreferences$Editor;"),
            (Method, "clear", "()Landroid/content/SharedPreferences$Editor;"),
            (Method, "apply", "()V"),
            (Method, "commit", "()Z"),
        ],
    ),
    (
        "android/content/Context",
        &[
            (Method, "getSharedPreferences", "(Ljava/lang/String;I)Landroid/content/SharedPreferences;"),
            (Method, "checkSelfPermission", "(Ljava/lang/String;)I"),
        ],
    ),
    (
        "android/content/res/Configuration",
        &[
            (Field, "fontScale", "F"),
            (Field, "densityDpi", "I"),
            (Field, "screenWidthDp", "I"),
            (Field, "screenHeightDp", "I"),
            (Field, "smallestScreenWidthDp", "I"),
            (Field, "orientation", "I"),
            (Field, "screenLayout", "I"),
            (Field, "uiMode", "I"),
            (Field, "colorMode", "I"),
            (Field, "touchscreen", "I"),
            (Field, "keyboard", "I"),
            (Field, "keyboardHidden", "I"),
            (Field, "hardKeyboardHidden", "I"),
            (Field, "navigation", "I"),
            (Field, "navigationHidden", "I"),
            (Field, "mcc", "I"),
            (Field, "mnc", "I"),
            (Field, "fontWeightAdjustment", "I"),
            (Method, "getLocales", "()Landroid/os/LocaleList;"),
        ],
    ),
    (
        "android/content/res/Resources",
        &[
            (Method, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;"),
        ],
    ),
    (
        "android/app/Activity",
        &[
            (Method, "getSharedPreferences", "(Ljava/lang/String;I)Landroid/content/SharedPreferences;"),
            (Method, "checkSelfPermission", "(Ljava/lang/String;)I"),
            (Method, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;"),
            (Method, "getResources", "()Landroid/content/res/Resources;"),
            (Method, "getNativeHelper", "()Lcom/roblox/client/startup/NativeHelper;"),
        ],
    ),
    (
        "android/app/Application",
        &[
            (Method, "getSharedPreferences", "(Ljava/lang/String;I)Landroid/content/SharedPreferences;"),
            (Method, "checkSelfPermission", "(Ljava/lang/String;)I"),
        ],
    ),
    (
        "android/view/MotionEvent",
        &[
            (Method, "getPointerId", "(I)I"),
            (Method, "getToolType", "(I)I"),
            (Method, "getRawX", "(I)F"),
            (Method, "getRawY", "(I)F"),
            (Method, "getXPrecision", "()F"),
            (Method, "getYPrecision", "()F"),
            (Method, "getAxisValue", "(II)F"),
            (Method, "getHistoricalEventTime", "(I)J"),
            (Method, "getHistoricalAxisValue", "(III)F"),
            (Method, "getDeviceId", "()I"),
            (Method, "getSource", "()I"),
            (Method, "getAction", "()I"),
            (Method, "getEventTime", "()J"),
            (Method, "getDownTime", "()J"),
            (Method, "getFlags", "()I"),
            (Method, "getMetaState", "()I"),
            (Method, "getActionButton", "()I"),
            (Method, "getButtonState", "()I"),
            (Method, "getClassification", "()I"),
            (Method, "getEdgeFlags", "()I"),
            (Method, "getHistorySize", "()I"),
            (Method, "getPointerCount", "()I"),
        ],
    ),
    (
        "android/view/KeyEvent",
        &[
            (Method, "getDeviceId", "()I"),
            (Method, "getSource", "()I"),
            (Method, "getAction", "()I"),
            (Method, "getEventTime", "()J"),
            (Method, "getDownTime", "()J"),
            (Method, "getFlags", "()I"),
            (Method, "getMetaState", "()I"),
            (Method, "getModifiers", "()I"),
            (Method, "getRepeatCount", "()I"),
            (Method, "getKeyCode", "()I"),
            (Method, "getScanCode", "()I"),
            (Method, "getUnicodeChar", "()I"),
        ],
    ),
    (
        "android/util/DisplayMetrics",
        &[
            (Field, "density", "F"),
            (Field, "scaledDensity", "F"),
            (Field, "xdpi", "F"),
            (Field, "ydpi", "F"),
            (Field, "densityDpi", "I"),
            (Field, "widthPixels", "I"),
            (Field, "heightPixels", "I"),
        ],
    ),
    (
        "org/json/JSONObject",
        &[
            (StaticMethod, "<init>", "()V"),
            (Method, "length", "()I"),
        ],
    ),
    (
        "androidx/core/graphics/Insets",
        &[
            (Field, "left", "I"),
            (Field, "top", "I"),
            (Field, "right", "I"),
            (Field, "bottom", "I"),
        ],
    ),
    (
        "androidx/core/view/WindowInsetsCompat$Type",
        &[
            (StaticMethod, "statusBars", "()I"),
            (StaticMethod, "navigationBars", "()I"),
            (StaticMethod, "captionBar", "()I"),
            (StaticMethod, "ime", "()I"),
            (StaticMethod, "systemGestures", "()I"),
            (StaticMethod, "mandatorySystemGestures", "()I"),
            (StaticMethod, "tappableElement", "()I"),
            (StaticMethod, "displayCutout", "()I"),
            (StaticMethod, "systemBars", "()I"),
        ],
    ),
    (
        "java/lang/Class",
        &[
            (Method, "getClassLoader", "()Ljava/lang/ClassLoader;"),
        ],
    ),
    (
        "java/lang/String",
        &[
            (Method, "getBytes", "(Ljava/lang/String;)[B"),
        ],
    ),
];

#[test]
fn every_member_the_engine_uses_is_answered_with_the_descriptor_it_asks_for() {
    assert!(jni::create_vm().is_some(), "the JavaVM is created once, here");

    let mut missing = Vec::new();
    let mut checked = 0;
    for (class, members) in ANSWERED {
        for &(kind, name, descriptor) in *members {
            checked += 1;
            if !answered(class, kind, name, descriptor) {
                missing.push(format!("{class} {kind:?} {name} {descriptor}"));
            }
        }
    }
    assert!(missing.is_empty(), "{} of {checked} members are not answered:\n{}", missing.len(), missing.join("\n"));
    assert_eq!(checked, 308);

    // The two that used to bind where nobody asks are gone, not doubled.
    assert!(!answered("com/google/androidgamesdk/GameActivity", Method, "getWindowInsets", "(I)Ljava/lang/Object;"));
    assert!(!answered(
        "com/roblox/platform/util/DeviceUtils",
        StaticMethod,
        "getScreenPhysicalSizeInMillimeters",
        "()Landroid/graphics/Point;"
    ));
    // Nor is anything answered that the engine never looked up under that
    // kind: a static lookup does not find an instance hook.
    assert!(!answered("com/roblox/engine/jni/user/NativeUserJavaInterface", Method, "getUserId", "()J"));
    assert!(!answered("no/such/Class", Method, "x", "()V"));
}
