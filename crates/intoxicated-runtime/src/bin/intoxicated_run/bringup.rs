//! The GameActivity bring-up: everything between `JNI_OnLoad` and the
//! looper pump, in the order the engine needs it, on the main thread that
//! later runs the pump.
//!
//! The order is load-bearing in these places, each measured or recorded:
//!
//! - looper, early settings, the `bootstrapTheApp` hook and night mode before
//!   `initializeNativeCode`, which decides flags, the Configuration and the
//!   AGDK handle during or right after it;
//! - the early `files` and `cache` directories before it as well (storage
//!   comes up only with both), `files` only once the CA link exists;
//! - the display size and `INTOXICATED_ENGINE_VERSION` before the
//!   User-Agent is first read;
//! - the settings and flags delivery finished before the globals, waited
//!   for rather than assumed (see `bootstrap.rs`);
//! - the globals before the app bridge; the cookie restore, identity and the
//!   deep link after `nativeAppBridgeV2InitWithParams` and before
//!   `StartLuaAppDM`; `nativeAppBridgeAppStart` before StartApp;
//! - the surface handover, then the late post and retry, which recover a
//!   failed first flags verdict;
//! - the bus consumers last, once the bridge has started the bus.

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::time::Duration;

use intoxicated_linker_sys as linker;

use crate::cli::Options;
use crate::{bootstrap, disk, switches, teardown};

const SETTINGS_CLASS: &str = "com/roblox/engine/jni/NativeSettingsInterface";
const GL_CLASS: &str = "com/roblox/engine/jni/NativeGLInterface";
const MAIN_ACTIVITY_CLASS: &str = "com/roblox/client/startup/MainGameActivity";

/// The method name of a JNI symbol: what follows its last underscore.
pub fn method(jni_name: &str) -> &str {
    jni_name.rsplit('_').next().unwrap_or(jni_name)
}

/// `<root>`, `<files>`, `<cache>` and `<root>/external`.
struct Dirs {
    root: PathBuf,
    files: PathBuf,
    cache: PathBuf,
    external: PathBuf,
}

impl Dirs {
    fn new(root: &Path) -> Dirs {
        Dirs {
            root: root.to_path_buf(),
            files: root.join("files"),
            cache: root.join("cache"),
            external: root.join("external"),
        }
    }
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// A static string-taking native, with the three log forms: `  <method> ok`,
/// `  <name> not exported` and `  <name> failed: <e>`, each with `label`
/// appended to the method where it says when.
fn call_strings(lib: linker::Library, name: &str, class: &str, args: &[&str], label: &str) {
    match lib.symbol(name) {
        None => println!("  {name} not exported"),
        Some(f) => match linker::game_activity::call_static_strings(f, class, args) {
            Ok(()) => println!("  {}{label} ok", method(name)),
            Err(e) => println!("  {name} failed: {e}"),
        },
    }
}

/// `--game-activity`: bring Roblox up, hand it the window and pump until the
/// run ends. Returns when the run is over or the bring-up stopped early; an
/// unrecoverable failure ends the process here.
pub fn run(lib: linker::Library, opt: &Options, asset_root: &str, data_root: &Path) {
    let Some(init_native) = lib.symbol("Java_com_google_androidgamesdk_GameActivity_initializeNativeCode") else {
        eprintln!("  initializeNativeCode is not exported");
        return;
    };

    // The data directories.
    let dirs = Dirs::new(data_root);
    for dir in [&dirs.files, &dirs.cache] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            println!("  could not create {}: {e}", dir.display());
        }
    }

    watch_engine_log(&dirs.files);

    // initializeNativeCode gives up with a zero handle when
    // `ALooper_forThread` answers null.
    if !intoxicated_runtime::android::looper::prepare_for_current_thread() {
        eprintln!("  could not prepare a looper for this thread");
        teardown::leave(1);
    }

    // The early settings, always: without them a fresh profile aborts
    // before its flags are loaded.
    let (settings, origin) = bootstrap::document();
    bootstrap::deliver_early(lib, &settings);

    early_directories(lib, &dirs);

    // The delivery proper, run by the engine's `bootstrapTheApp`.
    let names = bootstrap::flag_names(opt.client_settings.as_deref());
    bootstrap::install(lib, &opt.lib_dir, settings, origin, names);

    // Read from `GTK_THEME` (see `desktop::prefers_dark`).
    // Logged because "dark mode does not work" is three
    // different failures and this line separates them.
    let dark = intoxicated_runtime::desktop::prefers_dark();
    println!("  uiMode: night={}", if dark { "yes" } else { "no" });
    linker::game_activity::set_ui_mode_night(i32::from(dark));

    println!();
    println!("calling GameActivity.initializeNativeCode");
    let files = text(&dirs.files);
    let handle = match linker::game_activity::initialize(init_native, &files, &files, &files) {
        Ok(handle) => {
            println!("  native handle {handle:#x}");
            handle
        }
        Err(e) => {
            println!("  failed: {e}");
            return;
        }
    };

    // The window, and the display facts the User-Agent embeds.
    let (rw, rh) = switches::requested_resolution();
    let w = match intoxicated_runtime::android::open_window(
        rw, rh, &intoxicated_runtime::desktop::title(),
    ) {
        Ok(w) => w,
        Err(e) => {
            println!("  no window: {e}");
            return;
        }
    };
    // The window's real size, which a fullscreen monitor may have changed.
    let (width, height, format) = w.geometry();
    intoxicated_runtime::android::config::set_screen(width, height);
    println!("  window {width}x{height}");
    linker::game_activity::set_display_size(width, height);
    // And the display in millimetres, for
    // `DeviceUtils.getScreenPhysicalSizeInMillimeters`:
    // the monitor the window is on, as the
    // X server reports it. `(0, 0)` when
    // nothing does, which the engine reads
    // as null and already handles.
    let (mm_w, mm_h, mm_source) = w.physical_mm();
    println!(
        "  display: {mm_w}x{mm_h} mm ({})",
        mm_source.describe()
    );
    linker::game_activity::set_display_physical_mm(mm_w, mm_h);

    directory_layout(&dirs);

    // The engine version, before the User-Agent is first read.
    let engine_ver = match engine_version(&opt.lib_dir) {
        Some(engine_ver) => {
            // Read by the User-Agent builder in
            // `native/jni_values.cpp`, which has no
            // other route to it.
            std::env::set_var("INTOXICATED_ENGINE_VERSION", &engine_ver);
            println!("  engine version {engine_ver} (read from the binary)");
            engine_ver
        }
        None => {
            println!("  engine version not readable from libroblox.so; not setting one");
            String::new()
        }
    };

    if let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetDeviceInfo") {
        match linker::game_activity::set_device_info(f, width, height) {
            Ok(()) => println!("  device info set"),
            Err(e) => println!("  nativeSetDeviceInfo failed: {e}"),
        }
    }

    report_refresh_rate_natives(lib);

    // The battery, alongside the display
    // facts just wired above; see
    // `wire_battery_reporting`.
    wire_battery_reporting(lib);

    // Before anything asks for an asset ("RbxStorage is not
    // initialized" otherwise).
    match lib.symbol("Java_com_roblox_client_LocalStorageManager_initStorageManagerNativeV3") {
        None => println!("  initStorageManagerNativeV3 not exported"),
        Some(f) => match linker::game_activity::init_storage_manager(f, &files, &text(&dirs.cache)) {
            Ok(()) => println!("  storage manager initialised"),
            Err(e) => println!("  initStorageManagerNativeV3 failed: {e}"),
        },
    }

    // Six settings, in this order. `nativeSetBaseUrl` is not one of
    // them: given the same origin twice, the engine considers itself signed
    // out and refuses joins.
    let preferences = text(&dirs.files.join("appData/GlobalBasicSettings_13.xml"));
    let channel = switches::channel();
    call_strings(
        lib,
        "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetDefaultAppPolicyFile",
        SETTINGS_CLASS,
        &["content/guac/defaultConfigs/GuacDefaultPolicy-GlobalDist.json"],
        "",
    );
    call_strings(
        lib,
        "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetPreferencesFile",
        SETTINGS_CLASS,
        &[&preferences],
        "",
    );
    call_strings(
        lib,
        "Java_com_roblox_client_startup_MainGameActivity_nativeSetAssetPath",
        MAIN_ACTIVITY_CLASS,
        &[asset_root],
        "",
    );
    call_strings(
        lib,
        "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetRobloxVersion",
        SETTINGS_CLASS,
        &[&engine_ver],
        "",
    );
    // The real app's value, read from its dex.
    call_strings(
        lib,
        "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeOverrideChannelPlatformName",
        SETTINGS_CLASS,
        &["GoogleAndroidApp"],
        "",
    );
    call_strings(
        lib,
        "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetRobloxChannel",
        SETTINGS_CLASS,
        &[&channel],
        "",
    );

    // The four directory setters, whatever was sent early.
    directory_setters(lib, &dirs, None);

    webview_user_agent(lib);

    // As early as they resolve: the change handler reports only the
    // changes made after it is registered. The restore waits for the bridge.
    intoxicated_runtime::cookie_jar::attach(
        lib.symbol("Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetMultipleCookies"),
        lib.symbol("Java_com_roblox_engine_jni_NativeSettingsInterface_nativeGetCookiesForDomain"),
        lib.symbol(
            "Java_com_roblox_universalapp_cookie_JNICookieProtocol_updateOnSetCookieHandler",
        ),
    );

    // A different call shape from the first storage call's, on the same native; both
    // are made.
    let asset_manager = "Java_com_roblox_client_JNIAAssetManagerSetup_initNative";
    match lib.symbol(asset_manager) {
        None => println!("  {asset_manager} not exported"),
        Some(f) => match linker::game_activity::asset_manager_init(f) {
            Ok(()) => println!("  {} ok", method(asset_manager)),
            Err(e) => println!("  {asset_manager} failed: {e}"),
        },
    }
    let storage = "Java_com_roblox_client_LocalStorageManager_initStorageManagerNativeV3";
    match lib.symbol(storage) {
        None => println!("  {storage} not exported"),
        Some(f) => match linker::game_activity::storage_init(f, &files, &text(&dirs.cache)) {
            Ok(()) => println!("  {} ok", method(storage)),
            Err(e) => println!("  {storage} failed: {e}"),
        },
    }

    // The engine's local storage backed by `secure_values`: with it the
    // engine's "Not available on the current platform" warning goes.
    match lib.symbol(
        "Java_com_roblox_protocols_localstorageplatforminterface_generated_ILocalStorageHandlerCore_setPlatformImpl",
    ) {
        None => println!("  setPlatformImpl not exported"),
        Some(f) => match intoxicated_runtime::secure_values::hand_over(f) {
            Ok(()) => println!("  setPlatformImpl ok"),
            Err(e) => println!("  setPlatformImpl failed: {e}"),
        },
    }

    // The asset path is the `content` directory, never the `.apk`.
    if let Some(f) = lib.symbol("Java_com_roblox_client_startup_MainGameActivity_nativeAppBridgeSetInitParams") {
        match linker::game_activity::set_init_params(f, asset_root, width, height) {
            Ok(()) => println!("  init params set"),
            Err(e) => println!("  init params failed: {e}"),
        }
    }

    screen_orientation(lib, width, height);

    if let Some(overrides) = opt.flag_overrides.as_deref() {
        match lib.symbol("Java_com_roblox_client_startup_MainGameActivity_nativePreloadFlagOverrides") {
            None => println!("  nativePreloadFlagOverrides not exported"),
            Some(f) => match linker::game_activity::preload_flag_overrides(f, overrides) {
                Ok(()) => println!("  flag overrides preloaded ({} bytes)", overrides.len()),
                Err(e) => println!("  nativePreloadFlagOverrides failed: {e}"),
            },
        }
    }

    if let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_readLocalFlags") {
        match linker::game_activity::read_local_flags(f) {
            Ok(()) => println!("  local flags read"),
            Err(e) => println!("  readLocalFlags failed: {e}"),
        }
    }

    // The web view protocol's names, read
    // from the engine's own getters and
    // reported. Reading only: each getter
    // returns a constant. `webview::arm`,
    // just before the pump, reads them
    // again and subscribes to openWindow.
    let vocabulary = intoxicated_runtime::webview::read_vocabulary(|name| lib.symbol(name));
    intoxicated_runtime::webview::report(&vocabulary);
    // The two bus natives the openWindow
    // subscription needs, resolved and
    // reported only: the app bridge, which
    // the bus lives in, does not exist yet.
    let bus = intoxicated_runtime::webview::find_bus_natives(|name| lib.symbol(name));
    intoxicated_runtime::webview::report_bus_natives(&bus);

    if let Some(f) = lib.symbol("Java_com_roblox_client_startup_MainGameActivity_nativeRetryInit") {
        match linker::game_activity::call_bare(f) {
            Ok(()) => println!("  retryInit ok"),
            Err(e) => println!("  retryInit failed: {e}"),
        }
    }

    input_natives(lib);

    activity_lifecycle(lib);

    // The settings and flags delivery has to have finished before the
    // globals: waited for, not assumed. See `bootstrap.rs`.
    bootstrap::await_delivery();

    // Before the app bridge: StartLuaAppDM crashes on a null JNIEnv
    // without what these store.
    call_globals(lib, "late");

    // Builds the engine's cookie jar, protocol machinery and user store;
    // the next three steps do nothing before it.
    if let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeV2InitWithParams") {
        match linker::game_activity::appbridge_init(f, asset_root, width, height) {
            Ok(()) => println!("  app bridge initialised"),
            Err(e) => println!("  app bridge init failed: {e}"),
        }
    }

    // The session: a marker cookie set and read straight
    // back at four
    // points of the bring-up was in the engine only after the bridge.
    intoxicated_runtime::cookie_jar::restore();

    // The engine's own copy of the signed-in user, from the
    // mirrors `account::restore` fills.
    intoxicated_runtime::account::tell_engine(
        lib.symbol("Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetUserId"),
    );

    // After the bridge built the protocol machinery and before the app
    // shell checks for a cold-start link, as on
    // Android. The publish itself waits
    // for the home page; the module doc of
    // `deeplink` says why.
    if let Some(url) = opt.join.as_ref() {
        let outcome = intoxicated_runtime::deeplink::deliver(lib, url);
        println!("[deeplink] outcome: {outcome:?}");
    }

    // The app shell and its first authenticated request.
    if let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeStartLuaAppDM") {
        match linker::game_activity::appbridge_call_bare(f) {
            Ok(()) => println!("  Lua app DataModel started"),
            Err(e) => println!("  StartLuaAppDM failed: {e}"),
        }
    }

    // Immediately before StartApp, as in the Android capture.
    if let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_setTaskSchedulerBackgroundMode") {
        match linker::game_activity::call_static_bool_string(f, GL_CLASS, false, "ASMA.start") {
            Ok(()) => println!("  task scheduler foregrounded"),
            Err(e) => println!("  setTaskSchedulerBackgroundMode failed: {e}"),
        }
    }

    // The client's FIRST bridge call, and the one nothing here
    // was making: nativeAppBridgeAppStart on
    // NativeAppBridgeInterface. It constructs the StartupController
    // lazy-static (0x70b3c20); without it StartAppWithParams derefs
    // a null controller and segfaults right after `app start`.
    if let Some(f) = lib.symbol(
        "Java_com_roblox_engine_jni_NativeAppBridgeInterface_nativeAppBridgeAppStart__Ljava_lang_String_2Ljava_lang_String_2ZLjava_lang_String_2Ljava_lang_String_2Ljava_lang_String_2",
    ) {
        match linker::game_activity::app_start(f) {
            Ok(()) => println!("  nativeAppBridgeAppStart ok (builds StartupController)"),
            Err(e) => println!("  nativeAppBridgeAppStart failed: {e}"),
        }
    } else {
        println!("  nativeAppBridgeAppStart not exported");
    }

    if let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativeAppBridgeV2StartAppWithParams") {
        match linker::game_activity::appbridge_start_app(f, asset_root, width, height) {
            Ok(()) => println!("  app started with surface"),
            Err(e) => println!("  StartApp failed: {e}"),
        }
    }

    update_surfaces(lib, asset_root, width, height);

    disk::check(&dirs.files, disk::Moment::BeforeLaunch);

    if let Err(e) = linker::game_activity::start(handle, width, height, format) {
        println!("  lifecycle failed: {e}");
        return;
    }
    println!("  surface handed to the engine");

    late_post_and_retry(lib);

    match linker::game_activity::set_input_connection(handle) {
        Ok(Some(())) => println!("  InputConnection registered with the engine"),
        Ok(None) => println!("  setInputConnectionNative not registered yet; IME state will not reach Intoxicated"),
        Err(e) => println!("  setInputConnectionNative failed: {e}"),
    }

    if opt.run == 0 {
        println!("  pumping the looper until the window is closed (no --run timer)");
    } else {
        println!("  pumping the looper for {}s", opt.run);
    }

    arm_bus_consumers(lib);

    // Returns after its own teardown, or ends the process itself with
    // 124, 130 or 143.
    intoxicated_runtime::android::looper::pump(Duration::from_secs(opt.run), Some(handle));

    // Ungated: this is the one line
    // somebody debugging a mysterious crash
    // needs, and hiding it behind a switch
    // they do not know about is how it
    // stayed unsaid.
    disk::check(&dirs.files, disk::Moment::AfterExit);
}

/// Follow the engine's own log.
fn watch_engine_log(data_root: &Path) {
    // The engine writes its own log under this same tree;
    // the looper reports joins and leaves from it.
    intoxicated_runtime::engine_log::watch(data_root.join("appData/logs"));
}

/// The directory setters `INTOXICATED_EARLY_DIRS` selects, before
/// `initializeNativeCode`. Both `files` and `cache` early is what brings up
/// `RbxStorage` (5 of 5 against 0 of 8 with either alone); `files` early
/// without the CA link would break HTTPS, so it waits for the link.
fn early_directories(lib: linker::Library, dirs: &Dirs) {
    let early = switches::early_dirs(std::env::var(switches::EARLY_DIRS).ok().as_deref());
    if !early.any() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dirs.external) {
        println!("  could not create {}: {e}", dirs.external.display());
    }
    let mut early = early;
    if early.files && !dirs.files.join("exe/cacert.pem").exists() {
        println!(
            "  nativeSetFilesDirectory NOT set early: no {}, and early would break HTTPS",
            dirs.files.join("exe/cacert.pem").display()
        );
        early.files = false;
    }
    directory_setters(lib, dirs, Some(early));
}

/// The four directory setters in their order, all of them, or with `early`
/// only those selected and with the early log forms.
fn directory_setters(lib: linker::Library, dirs: &Dirs, early: Option<switches::EarlyDirs>) {
    let (files, cache, external) = (text(&dirs.files), text(&dirs.cache), text(&dirs.external));
    let all = switches::EarlyDirs { files: true, cache: true, external: true, base: true };
    let chosen = early.unwrap_or(all);
    let setters: [(bool, &str, Vec<&str>); 4] = [
        (chosen.files, "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetFilesDirectory", vec![&files]),
        (chosen.cache, "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetCacheDirectory", vec![&cache]),
        (
            chosen.external,
            "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetExternalDirectory",
            vec![&external],
        ),
        (
            chosen.base,
            "Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetBaseDataDirectories",
            vec![&files, &cache],
        ),
    ];
    for (wanted, name, args) in setters {
        if !wanted {
            continue;
        }
        if early.is_none() {
            call_strings(lib, name, SETTINGS_CLASS, &args, "");
            continue;
        }
        let m = method(name);
        match lib.symbol(name) {
            None => println!("  {m} not exported (early)"),
            Some(f) => match linker::game_activity::call_static_strings(f, SETTINGS_CLASS, &args) {
                Ok(()) => println!("  {m} ok (early)"),
                Err(e) => println!("  {m} failed (early): {e}"),
            },
        }
    }
}

/// The directories under `<root>` and under the working directory that
/// storage `statvfs`s before it builds anything; the engine builds some
/// paths relative to the working directory.
const LAYOUT: [&str; 18] = [
    "files",
    "cache",
    "shared_prefs",
    "rbx-storage",
    "appData",
    "appData/LocalStorage",
    "appData/rbx-storage",
    "appData/ClientSettings",
    "files/appData",
    "files/appData/LocalStorage",
    "files/appData/OTAPatchBackups",
    "files/appData/rbx-storage",
    "cache/ContentProvider_2",
    "cache/rbx-storage",
    "cache/sounds",
    "sdcard/Android/data/com.roblox.client",
    "sdcard/Android/data/com.roblox.client/files",
    "sdcard/Android/data/com.roblox.client/cache",
];

/// The layout above, under both roots, and `set_files_dir` before anything
/// asks the framework for its files directory: the native side latches it.
fn directory_layout(dirs: &Dirs) {
    let _ = std::fs::create_dir_all(&dirs.external);
    intoxicated_runtime::android::system::set_files_dir(&dirs.files);
    for base in [dirs.root.as_path(), Path::new(".")] {
        for dir in LAYOUT {
            let _ = std::fs::create_dir_all(base.join(dir));
        }
    }
}

/// The version of the engine in `lib_dir`, read out of the binary, or `None`.
/// Never a guess: an engine told a version it does not carry is a bug.
///
/// The scan lives in [`intoxicated_runtime::engine`], which returns `None` when
/// the four-part shape is not unique, because skipping the call is honest and
/// inventing a version is what caused the bug above.
fn engine_version(lib_dir: &str) -> Option<String> {
    intoxicated_runtime::engine::installed_version(std::path::Path::new(lib_dir))
}

/// Say what the engine's refresh-rate natives are, and report nothing.
///
/// `nativePassSupportedRefreshRates` and `nativePassCurrentDisplayRefreshRate`
/// are exported by every build this
/// project has looked at. The monitor list that used to feed them came from
/// GDK, and GDK was never initialised on this X11 path, so the list was always
/// empty and nothing was ever passed. That is still the behaviour; it is now
/// said plainly instead of being the result of an empty enumeration. Feeding
/// these from XRandR is the way to make them do something.
fn report_refresh_rate_natives(lib: linker::Library) {
    let supported = lib.symbol(
        "Java_com_roblox_engine_jni_NativeGLInterface_nativePassSupportedRefreshRates",
    );
    let current = lib.symbol(
        "Java_com_roblox_engine_jni_NativeGLInterface_nativePassCurrentDisplayRefreshRate",
    );
    println!(
        "  refresh: nativePassSupportedRefreshRates {}, nativePassCurrentDisplayRefreshRate {}; \
         no monitor list is read, so neither is called",
        if supported.is_some() { "resolved" } else { "NOT exported" },
        if current.is_some() { "resolved" } else { "NOT exported" },
    );
}

/// Hand the battery natives to `power`.
///
/// `reportBatteryStateChanged` and `reportBatteryStatus` are
/// exported by every build this project has looked at. What they are told
/// comes from FreeBSD's ACPI sysctls; see `intoxicated_runtime::power` for the
/// mapping, for what a desktop reports, and for the re-reading that runs
/// (from the looper) only on a machine with a battery or an AC adapter.
fn wire_battery_reporting(lib: linker::Library) {
    let state_changed_native = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_reportBatteryStateChanged");
    let status_native = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_reportBatteryStatus");
    for (name, native) in [("reportBatteryStateChanged", state_changed_native), ("reportBatteryStatus", status_native)] {
        println!("  battery: {name} {}", if native.is_some() { "resolved" } else { "NOT exported" });
    }
    if let (Some(state_changed_native), Some(status_native)) = (state_changed_native, status_native) {
        intoxicated_runtime::power::start(state_changed_native, status_native);
    }
}

/// The web view is given the User-Agent read back from
/// `native/jni_values.cpp`'s
/// User-Agent builder rather than
/// recomputing it, so the engine's
/// web view and its HTTP client cannot
/// disagree about what this client
/// claims to be. `getWebViewUserAgent()V` on the framework side answers the
/// same value (`native/jni_framework.cpp`).
fn webview_user_agent(lib: linker::Library) {
    let Some(ua) = intoxicated_runtime::webview::user_agent() else {
        println!(
            "  webview: could not read the User-Agent back from \
             native/jni_values.cpp; not calling setWebviewUserAgent"
        );
        return;
    };
    let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_setWebviewUserAgent") else {
        println!("  webview: setWebviewUserAgent not exported by this build");
        return;
    };
    match linker::game_activity::call_static_strings(f, GL_CLASS, &[&ua]) {
        Ok(()) => println!("  webview: setWebviewUserAgent ok"),
        Err(e) => println!("  webview: setWebviewUserAgent failed: {e}"),
    }
}

// `native/secure_values.cpp` also answers the screen orientation native.
// Declared here rather than through `intoxicated_linker_sys::game_activity`,
// which has no `(int)` call shape; it links from the same
// `libintoxicated_jni_shim.a` that crate already tells `intoxicated-run` to link.
extern "C" {
    fn intox_update_screen_orientation(
        native: *mut c_void,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

/// The orientation the window's shape implies: landscape when it is at
/// least as wide as it is high.
fn screen_orientation(lib: linker::Library, width: i32, height: i32) {
    let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeInputInterface_nativeUpdateScreenOrientation") else {
        println!("  nativeUpdateScreenOrientation not exported");
        return;
    };
    let mut err = [0u8; 512];
    // SAFETY: `f` is the exported JNI native the caller resolved by name;
    // `err` is a live buffer for the duration of this call.
    let rc = unsafe {
        intox_update_screen_orientation(
            f,
            width,
            height,
            err.as_mut_ptr().cast(),
            err.len(),
        )
    };
    if rc == 0 {
        println!("  screen orientation set");
    } else {
        let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
        println!("  nativeUpdateScreenOrientation failed: {}", String::from_utf8_lossy(&err[..end]));
    }
}

/// The input natives `android::input` delivers through, null when one
/// is missing.
fn input_natives(lib: linker::Library) {
    let native = |name: &str| lib.symbol(name).unwrap_or(std::ptr::null_mut());
    let mouse_move = native("Java_com_roblox_engine_jni_NativeInputInterface_nativePassMouseMove");
    let mouse_button = native("Java_com_roblox_engine_jni_NativeInputInterface_nativePassMouseButton");
    let mouse_wheel = native("Java_com_roblox_engine_jni_NativeInputInterface_nativePassMouseWheel");
    // No gamepad natives, no touch
    // native and no text-box geometry
    // getter: nothing on X11 produces
    // gamepad or touch input, and the
    // field comes from `showKeyboard`.
    intoxicated_runtime::android::input::set_input_natives(
        mouse_move,
        mouse_button,
        mouse_wheel,
        native("Java_com_roblox_engine_jni_NativeGLInterface_nativePassKeyEvent"),
        native("Java_com_roblox_engine_jni_NativeGLInterface_nativePassText"),
        native("Java_com_roblox_engine_jni_NativeGLInterface_syncTextboxTextAndCursorPosition2"),
        native("Java_com_roblox_engine_jni_NativeGLInterface_updateKeyboardSize"),
    );
    if mouse_move.is_null() || mouse_button.is_null() {
        println!("  input: NativeInputInterface not fully exported; UI input will not work");
    }
    if mouse_wheel.is_null() {
        println!("  input: nativePassMouseWheel NOT exported; the scroll wheel will do nothing");
    } else {
        println!("  input: nativePassMouseWheel resolved");
    }
    // And the one native
    // the client reads rather than
    // writes: whether the engine wants
    // the pointer locked to the window
    // centre (first person, shift
    // lock). See
    // `input::engine_wants_pointer_lock`.
    let locked = native("Java_com_roblox_engine_jni_NativeInputInterface_nativeGetMainWindowIsMouseLockedCenter");
    intoxicated_runtime::android::input::set_mouse_lock_native(locked);
    if locked.is_null() {
        println!(
            "  input: nativeGetMainWindowIsMouseLockedCenter NOT exported; pointer capture falls back to the \
             mouse button alone"
        );
    } else {
        println!("  input: nativeGetMainWindowIsMouseLockedCenter resolved");
    }
}

/// The nine `JNIActivityLifecycleCallbacks` natives, in Android's order.
const LIFECYCLE: [&str; 9] = [
    "nativeOnPreCreated",
    "nativeOnCreated",
    "nativeOnPostCreated",
    "nativeOnPreStarted",
    "nativeOnStarted",
    "nativeOnPostStarted",
    "nativeOnPreResumed",
    "nativeOnResumed",
    "nativeOnPostResumed",
];

/// Without these the engine held a null JNIEnv on its game thread.
fn activity_lifecycle(lib: linker::Library) {
    let mut fired = 0;
    for stage in LIFECYCLE {
        let name =
            format!("Java_com_roblox_universalapp_activitylifecyclecallbacks_JNIActivityLifecycleCallbacks_{stage}");
        let Some(f) = lib.symbol(&name) else { continue };
        match linker::game_activity::activity_lifecycle(f, "com.roblox.client.ActivityNativeMain") {
            Ok(()) => fired += 1,
            Err(e) => println!("  {stage} failed: {e}"),
        }
    }
    println!("  activity lifecycle: {fired}/{} fired", LIFECYCLE.len());
}

/// `nativeGameGlobalInit`, then `nativeUpdateAdapterInit`, with `label`
/// saying when. The engine reaches its flags verdict around the first.
fn call_globals(lib: linker::Library, label: &str) {
    for name in [
        "Java_com_roblox_engine_jni_NativeGLInterface_nativeGameGlobalInit",
        "Java_com_roblox_engine_jni_NativeGLInterface_nativeUpdateAdapterInit",
    ] {
        match lib.symbol(name) {
            None => println!("  {name} not exported"),
            Some(f) => match linker::game_activity::appbridge_call_bare(f) {
                Ok(()) => println!("  {} ok ({label})", method(name)),
                Err(e) => println!("  {} failed ({label}): {e}", method(name)),
            },
        }
    }
}

/// The app surface, then the game surface, with their platform params.
fn update_surfaces(lib: linker::Library, asset_root: &str, width: i32, height: i32) {
    for (which, game) in [("App", false), ("Game", true)] {
        let word = which.to_ascii_lowercase();
        let native = format!("nativeAppBridgeV2UpdateSurface{which}WithPlatformParams");
        match lib.symbol(&format!("Java_com_roblox_engine_jni_NativeGLInterface_{native}")) {
            Some(f) => match linker::game_activity::appbridge_update_surface(f, asset_root, width, height, game) {
                Ok(()) => println!("  surface+platform params delivered ({word})"),
                Err(e) => println!("  UpdateSurface {word} failed: {e}"),
            },
            None => {
                println!("  UpdateSurface {word}: the engine does not export it");
                intoxicated_runtime::unimplemented::placeholder(native, "not exported by this build; not called");
            }
        }
    }
}

/// After the handover: a pause, the base URL the launcher asks for, the
/// post-settings call and the retry. On a fresh profile whose first flags
/// verdict failed, this is what turns it into `flags loaded`; with it off
/// the flags never loaded. 0 ms reached a better flags state and then
/// segfaulted.
fn late_post_and_retry(lib: linker::Library) {
    let Some(ms) = switches::late_post_delay(std::env::var(switches::LATE_POST_MS).ok().as_deref()) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(ms));

    // EXPERIMENT (INTOXICATED_SET_BASE_URL=<a>,<b>): on FreeBSD
    // the engine reads InitParams.baseURL() correctly and still
    // logs `The base url is ` empty, then aims its settings
    // fetch at `https:///v2/...`. Call the engine's own
    // setter with two explicit strings and see whether the base
    // url and settings url come alive. The second argument is
    // not known.
    if let Ok(v) = std::env::var("INTOXICATED_SET_BASE_URL") {
        let (a, b) = v.split_once(',').unwrap_or((v.as_str(), v.as_str()));
        let (a, b): (&'static str, &'static str) = (a.to_string().leak(), b.to_string().leak());
        if let Some(f) = lib.symbol("Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetBaseUrl") {
            match linker::game_activity::call_static_strings(f, "com/roblox/engine/jni/NativeSettingsInterface", &[a, b]) {
                Ok(()) => println!("  [baseurl] nativeSetBaseUrl({a:?}, {b:?}) ok"),
                Err(e) => println!("  [baseurl] nativeSetBaseUrl failed: {e}"),
            }
        }
    }

    match lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_nativePostClientSettingsLoadedInitialization3") {
        None => println!("  late post: not exported"),
        Some(f) => match linker::game_activity::post_client_settings_loaded(f) {
            Ok(()) => println!("  late post: postClientSettingsLoadedInitialization3 ok (after {ms} ms)"),
            Err(e) => println!("  late post failed: {e}"),
        },
    }

    if !switches::late_retry() {
        return;
    }
    match lib.symbol("Java_com_roblox_client_startup_MainGameActivity_nativeRetryInit") {
        None => println!("  late retry: not exported"),
        Some(f) => match linker::game_activity::appbridge_call_bare(f) {
            Ok(()) => println!("  late retry: nativeRetryInit ok"),
            Err(e) => println!("  late retry failed: {e}"),
        },
    }
}

/// What listens on the engine's message bus, which exists once the
/// app bridge has started.
fn arm_bus_consumers(lib: linker::Library) {
    // Here, just before the pump
    // starts: the message bus has
    // existed only since the app bridge started. The web view first.
    intoxicated_runtime::webview::arm(|name| lib.symbol(name));
    // Then register the engine for
    // devctl's bus verbs.
    intoxicated_runtime::deeplink::arm_live(lib);
    // Without it Terms, Privacy and external links do nothing.
    intoxicated_runtime::linking::arm(|name| lib.symbol(name));
    install_browser_presenter();
    // A test of the openWindow path that needs no
    // click in signed-in UI: one synthetic openWindow,
    // published on the engine's own bus, so the
    // subscription, the parse and the hand-off all run
    // as they would for a real one.
    if let Ok(url) = std::env::var(intoxicated_runtime::webview::TEST_VARIABLE) {
        intoxicated_runtime::webview::publish_test_window(
            |name| lib.symbol(name),
            &url,
        );
    }
}

/// Send the engine's `openWindow` to the user's browser instead of dropping it.
///
/// The engine asks for a web view for pages such as Terms and Privacy:
/// pages
/// the user is asking for by clicking. There is no in-application web view:
/// the embedded one attached to a GTK host window, and the X11 window is not
/// one.
///
/// The URL comes
/// from the engine, which got it from Lua. `desktop::open_url` refuses
/// anything that is not http or https, and the address is never logged
/// because it can carry credentials in its query string.
fn install_browser_presenter() {
    println!("  webview: openWindow will open in your browser");
    intoxicated_runtime::webview::set_presenter(|request| {
        match intoxicated_runtime::desktop::open_url(&request.url) {
            Ok(()) => println!("  webview: openWindow handed to the browser"),
            Err(e) => println!("  webview: openWindow could not be opened: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_is_what_follows_the_last_underscore() {
        assert_eq!(method("Java_com_roblox_engine_jni_NativeSettingsInterface_nativeSetRobloxVersion"), "nativeSetRobloxVersion");
        assert_eq!(method("Java_com_roblox_client_LocalStorageManager_initStorageManagerNativeV3"), "initStorageManagerNativeV3");
        assert_eq!(method("Java_com_roblox_client_JNIAAssetManagerSetup_initNative"), "initNative");
        assert_eq!(method("plain"), "plain");
    }

    #[test]
    fn the_layout_is_relative() {
        assert_eq!(LAYOUT.len(), 18);
        assert!(LAYOUT.iter().all(|d| !d.starts_with('/') && !d.contains("..")));
    }
}
