//! Settings, flags, storage and app-bridge natives, by raw native address.
//!
//! Static natives on the engine's Java bridge classes. The C side
//! (`native/jni_params.cpp`) builds whatever Java objects each one takes and
//! passes the class as receiver; where a row takes a class name, the caller
//! supplies it slash-separated, as JNI spells it
//! (`com/roblox/engine/jni/NativeSettingsInterface`).

use std::ffi::{c_char, c_int, c_void};

use crate::call::{c_text, status};
use crate::ffi;

/// `MainGameActivity.nativeAppBridgeSetInitParams`, with an `InitParams`
/// built from the arguments.
pub fn set_init_params(native: *mut c_void, assets: &str, width: i32, height: i32) -> Result<(), String> {
    let assets = c_text(assets)?;
    // SAFETY: `assets` outlives the call; `native` is checked by the C side.
    status(|err, len| unsafe { ffi::intoxicated_set_init_params(native, assets.as_ptr(), width, height, err, len) })
}

/// `JNIAAssetManagerSetup.initNative`. A plain `fn`, so the runtime can put
/// it in a list of boxed initialisers beside closures.
pub fn asset_manager_init(native: *mut c_void) -> Result<(), String> {
    // SAFETY: the helper's buffer only.
    status(|err, len| unsafe { ffi::intoxicated_asset_manager_init(native, err, len) })
}

/// `LocalStorageManager`'s storage native, with a plain placeholder object
/// and two strings.
pub fn storage_init(native: *mut c_void, a: &str, b: &str) -> Result<(), String> {
    let (a, b) = (c_text(a)?, c_text(b)?);
    // SAFETY: both strings outlive the call.
    status(|err, len| unsafe { ffi::intoxicated_storage_init(native, a.as_ptr(), b.as_ptr(), err, len) })
}

/// `LocalStorageManager.initStorageManagerNativeV3(AssetManager, String,
/// String)`, with an `android/content/res/AssetManager` as the object. A
/// different native from [`storage_init`]'s, and both are called.
pub fn init_storage_manager(native: *mut c_void, a: &str, b: &str) -> Result<(), String> {
    let (a, b) = (c_text(a)?, c_text(b)?);
    // SAFETY: both strings outlive the call.
    status(|err, len| unsafe { ffi::intoxicated_init_storage_manager(native, a.as_ptr(), b.as_ptr(), err, len) })
}

/// `NativeSettingsInterface.nativeSetDeviceInfo(DeviceParams)`.
pub fn set_device_info(native: *mut c_void, width: i32, height: i32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe { ffi::intoxicated_set_device_info(native, width, height, err, len) })
}

/// `FlagJniInterface.nativeInitializeNativeFlags`.
pub fn init_flags(native: *mut c_void, settings_json: &str) -> Result<(), String> {
    let json = c_text(settings_json)?;
    // SAFETY: `json` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_init_flags(native, json.as_ptr(), err, len) })
}

/// `NativeGLInterface.readLocalFlags()`.
pub fn read_local_flags(native: *mut c_void) -> Result<(), String> {
    // SAFETY: the helper's buffer only.
    status(|err, len| unsafe { ffi::intoxicated_read_local_flags(native, err, len) })
}

/// `NativeGLInterface.nativeInitClientSettings(String, String, String)I`.
/// `Ok` carries the engine's own int.
pub fn init_client_settings(native: *mut c_void, a: &str, b: &str, c: &str) -> Result<i32, String> {
    let (a, b, c) = (c_text(a)?, c_text(b)?, c_text(c)?);
    let mut result: c_int = 0;
    // SAFETY: the strings and `result` outlive the call.
    status(|err, len| unsafe {
        ffi::intoxicated_init_client_settings(native, a.as_ptr(), b.as_ptr(), c.as_ptr(), &mut result, err, len)
    })?;
    Ok(result)
}

/// `nativePostClientSettingsLoadedInitialization3`, with an empty list.
pub fn post_client_settings_loaded(native: *mut c_void) -> Result<(), String> {
    // SAFETY: the helper's buffer only.
    status(|err, len| unsafe { ffi::intoxicated_post_client_settings_loaded(native, err, len) })
}

/// `FlagJniInterface.nativeGetFInt(String, int)I`. The engine answers
/// `fallback` for a flag it does not know.
pub fn get_fint(native: *mut c_void, name: &str, fallback: i32) -> Result<i32, String> {
    let name = c_text(name)?;
    let mut result: c_int = fallback;
    // SAFETY: `name` and `result` outlive the call.
    status(|err, len| unsafe { ffi::intoxicated_get_fint(native, name.as_ptr(), fallback, &mut result, err, len) })?;
    Ok(result)
}

/// `MainGameActivity.nativePreloadFlagOverrides(String)`, with the text
/// passed through untouched.
pub fn preload_flag_overrides(native: *mut c_void, json: &str) -> Result<(), String> {
    let json = c_text(json)?;
    // SAFETY: `json` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_preload_flag_overrides(native, json.as_ptr(), err, len) })
}

/// `NativeGLInterface.nativeAppBridgeV2InitWithParams`.
pub fn appbridge_init(native: *mut c_void, assets: &str, width: i32, height: i32) -> Result<(), String> {
    let assets = c_text(assets)?;
    // SAFETY: `assets` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_appbridge_init(native, assets.as_ptr(), width, height, err, len) })
}

/// `nativeAppBridgeV2StartAppWithParams`.
pub fn appbridge_start_app(native: *mut c_void, assets: &str, width: i32, height: i32) -> Result<(), String> {
    let assets = c_text(assets)?;
    // SAFETY: `assets` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_appbridge_start_app(native, assets.as_ptr(), width, height, err, len) })
}

/// One of the two `...UpdateSurface...WithPlatformParams` natives: the game
/// surface's when `game`, the app shell's otherwise. Their C signatures are
/// identical; only the native the caller resolved differs.
pub fn appbridge_update_surface(
    native: *mut c_void,
    assets: &str,
    width: i32,
    height: i32,
    game: bool,
) -> Result<(), String> {
    let assets = c_text(assets)?;
    // SAFETY: `assets` outlives the call.
    status(|err, len| unsafe {
        if game {
            ffi::intoxicated_appbridge_update_surface_game(native, assets.as_ptr(), width, height, err, len)
        } else {
            ffi::intoxicated_appbridge_update_surface_app(native, assets.as_ptr(), width, height, err, len)
        }
    })
}

/// A no-argument static native with `NativeGLInterface` as receiver.
pub fn appbridge_call_bare(native: *mut c_void) -> Result<(), String> {
    // SAFETY: the helper's buffer only.
    status(|err, len| unsafe { ffi::intoxicated_appbridge_call_bare(native, err, len) })
}

/// `NativeAppBridgeInterface.nativeAppBridgeAppStart`, with five empty
/// strings and `false`. What the call is for is its side effect: it is where
/// the engine constructs its start-up controller.
pub fn app_start(native: *mut c_void) -> Result<(), String> {
    let empty: *const c_char = b"\0".as_ptr().cast();
    // SAFETY: `empty` is a static NUL-terminated string.
    status(|err, len| unsafe {
        ffi::intoxicated_appbridge_app_start(native, empty, empty, 0, empty, empty, empty, err, len)
    })
}

/// A native that takes only the JNI environment and receiver, such as
/// `nativeRetryInit`.
pub fn call_bare(native: *mut c_void) -> Result<(), String> {
    // SAFETY: the helper's buffer only.
    status(|err, len| unsafe { ffi::intoxicated_call_bare(native, err, len) })
}

/// A no-argument static native on a named class.
pub fn call_bare_on(native: *mut c_void, class_name: &str) -> Result<(), String> {
    let class = c_text(class_name)?;
    // SAFETY: `class` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_appbridge_call_bare_cls(native, class.as_ptr(), err, len) })
}

/// One of `JNIActivityLifecycleCallbacks`' natives, given an activity name.
pub fn activity_lifecycle(native: *mut c_void, activity: &str) -> Result<(), String> {
    let activity = c_text(activity)?;
    // SAFETY: `activity` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_activity_lifecycle(native, activity.as_ptr(), err, len) })
}

/// A static native on a named class taking zero to three `String`s.
///
/// The limit is the C side's, which has one call shape per count; more than
/// three comes back as its error rather than being refused here, so the
/// limit lives in one place.
pub fn call_static_strings(native: *mut c_void, class_name: &str, args: &[&str]) -> Result<(), String> {
    let class = c_text(class_name)?;
    let args = args.iter().map(|a| c_text(a)).collect::<Result<Vec<_>, _>>()?;
    let pointers: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    // SAFETY: `pointers` holds `pointers.len()` strings that outlive the call.
    status(|err, len| unsafe {
        ffi::intoxicated_call_static_strings(native, class.as_ptr(), pointers.as_ptr(), pointers.len(), err, len)
    })
}

/// A static `(boolean, String)` native on a named class.
pub fn call_static_bool_string(native: *mut c_void, class_name: &str, flag: bool, text: &str) -> Result<(), String> {
    let (class, text) = (c_text(class_name)?, c_text(text)?);
    // SAFETY: both strings outlive the call.
    status(|err, len| unsafe {
        ffi::intoxicated_call_static_bool_string(native, class.as_ptr(), c_int::from(flag), text.as_ptr(), err, len)
    })
}

/// A zero-argument static native on a named class that returns a boolean.
pub fn call_static_bare_bool(native: *mut c_void, class_name: &str) -> Result<bool, String> {
    let class = c_text(class_name)?;
    // -1 is a value the C side never writes, so a success that somehow left
    // it alone shows up as itself in a debugger instead of as a plausible
    // `false`.
    let mut result: c_int = -1;
    // SAFETY: `class` and `result` outlive the call.
    status(|err, len| unsafe {
        ffi::intoxicated_call_static_bare_bool(native, class.as_ptr(), &mut result, err, len)
    })?;
    Ok(result != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Without a VM every one of these is refused on the C side before it
    // could call anything, which is the null handling the runtime leans on
    // when a native it looked up turned out not to exist.

    #[test]
    fn a_null_native_is_an_error_not_a_crash() {
        let null = std::ptr::null_mut();
        for result in [
            appbridge_call_bare(null),
            call_bare(null),
            asset_manager_init(null),
            read_local_flags(null),
            init_flags(null, "{}"),
            call_static_strings(null, "com/example/Nothing", &["a", "b"]),
            call_bare_on(null, "com/example/Nothing"),
        ] {
            let e = result.unwrap_err();
            assert!(!e.is_empty(), "the C side explains a refusal");
        }
        assert!(init_client_settings(null, "", "", "").is_err());
        assert!(get_fint(null, "SomeFlag", 7).is_err());
        assert!(call_static_bare_bool(null, "com/example/Nothing").is_err());
    }

    #[test]
    fn an_interior_nul_never_reaches_c() {
        let null = std::ptr::null_mut();
        assert!(init_flags(null, "{\0}").unwrap_err().contains("nul byte"));
        assert!(call_static_strings(null, "com/example/Nothing", &["ok", "b\0ad"]).unwrap_err().contains("nul byte"));
        assert!(call_bare_on(null, "com/\0example").unwrap_err().contains("nul byte"));
    }
}
