//! `AConfiguration_*`: the device configuration as the NDK hands it out.
//!
//! Two views of one desktop window exist. The Java side the framework layer answers
//! (see `native/jni_framework.cpp`) is what the engine's own code consults;
//! these nine functions are the native view, and they say the same thing: a
//! large, tablet-class screen, English, United States, navigation not hidden.
//!
//! The engine makes one handle and refreshes it once, during
//! `GameActivity.initializeNativeCode`, before the window opens (traced with
//! `INTOXICATED_ANDROID_TRACE=1`). A handle is a snapshot, as on Android: the
//! window's size reaches handles made or refreshed after [`set_screen`], not
//! the one the engine already holds. Exactly the nine functions
//! `libroblox.so` imports are here; any other `AConfiguration_*` would get
//! the stub table's answer.

use std::ffi::{c_char, c_void};
use std::sync::Mutex;

/// `ACONFIGURATION_SCREENSIZE_XLARGE`.
pub const SCREENSIZE_XLARGE: i32 = 0x04;
/// `ACONFIGURATION_NAVHIDDEN_NO`.
pub const NAVHIDDEN_NO: i32 = 1;

/// What one handle holds. Sizes are in dp; a desktop's density is close
/// enough to Android's 160 dpi baseline that window pixels are stored as
/// they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Configuration {
    width_dp: i32,
    height_dp: i32,
    screen_size: i32,
    language: [u8; 2],
    country: [u8; 2],
}

const DEFAULT: Configuration = Configuration {
    width_dp: 1280,
    height_dp: 720,
    screen_size: SCREENSIZE_XLARGE,
    language: *b"en",
    country: *b"US",
};

/// The process-wide configuration new and refreshed handles copy.
static CURRENT: Mutex<Configuration> = Mutex::new(DEFAULT);

fn current() -> Configuration {
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner())
}

/// The window's size, in pixels. A positive value replaces the stored one;
/// zero or less leaves that dimension as it was.
pub fn set_screen(width: i32, height: i32) {
    let mut c = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if width > 0 {
        c.width_dp = width;
    }
    if height > 0 {
        c.height_dp = height;
    }
}

/// The handle behind an `AConfiguration*`, or `None` for null.
///
/// # Safety
/// `handle` is null or came from [`new`] and has not been deleted.
unsafe fn read<'a>(handle: *mut c_void) -> Option<&'a Configuration> {
    // SAFETY: the caller's contract.
    unsafe { handle.cast::<Configuration>().as_ref() }
}

extern "C" fn new() -> *mut c_void {
    super::trace(format_args!("AConfiguration_new"));
    Box::into_raw(Box::new(current())).cast()
}

extern "C" fn delete(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: every handle the engine holds came from `new`.
        drop(unsafe { Box::from_raw(handle.cast::<Configuration>()) });
    }
}

extern "C" fn from_asset_manager(handle: *mut c_void, _assets: *mut c_void) {
    super::trace(format_args!("AConfiguration_fromAssetManager"));
    // SAFETY: as in `delete`.
    if let Some(c) = unsafe { handle.cast::<Configuration>().as_mut() } {
        *c = current();
    }
}

/// Two bytes, no terminator, which is the NDK's contract for both codes.
fn write_code(code: [u8; 2], out: *mut c_char) {
    if !out.is_null() {
        // SAFETY: the NDK's out-buffer holds at least two bytes.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), out.cast::<u8>(), 2) };
    }
}

extern "C" fn get_language(handle: *mut c_void, out: *mut c_char) {
    // SAFETY: as in `read`.
    if let Some(c) = unsafe { read(handle) } {
        write_code(c.language, out);
    }
}

extern "C" fn get_country(handle: *mut c_void, out: *mut c_char) {
    // SAFETY: as in `read`.
    if let Some(c) = unsafe { read(handle) } {
        write_code(c.country, out);
    }
}

extern "C" fn get_screen_width_dp(handle: *mut c_void) -> i32 {
    // SAFETY: as in `read`.
    unsafe { read(handle) }.map_or(0, |c| c.width_dp)
}

extern "C" fn get_screen_height_dp(handle: *mut c_void) -> i32 {
    // SAFETY: as in `read`.
    unsafe { read(handle) }.map_or(0, |c| c.height_dp)
}

extern "C" fn get_screen_size(handle: *mut c_void) -> i32 {
    // SAFETY: as in `read`.
    unsafe { read(handle) }.map_or(0, |c| c.screen_size)
}

extern "C" fn get_nav_hidden(_handle: *mut c_void) -> i32 {
    NAVHIDDEN_NO
}

/// The nine names and their entry points, for `android::overrides`.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("AConfiguration_new", new),
        f!("AConfiguration_delete", delete),
        f!("AConfiguration_fromAssetManager", from_asset_manager),
        f!("AConfiguration_getLanguage", get_language),
        f!("AConfiguration_getCountry", get_country),
        f!("AConfiguration_getScreenWidthDp", get_screen_width_dp),
        f!("AConfiguration_getScreenHeightDp", get_screen_height_dp),
        f!("AConfiguration_getScreenSize", get_screen_size),
        f!("AConfiguration_getNavHidden", get_nav_hidden),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `set_screen` changes process-wide state; the tests that call it run
    /// one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn the_codes_are_two_bytes_and_nothing_more() {
        let h = new();
        let mut buf = [0x7f as c_char; 3];
        get_language(h, buf.as_mut_ptr());
        assert_eq!([buf[0] as u8, buf[1] as u8, buf[2] as u8], [b'e', b'n', 0x7f]);
        let mut buf = [0x7f as c_char; 3];
        get_country(h, buf.as_mut_ptr());
        assert_eq!([buf[0] as u8, buf[1] as u8, buf[2] as u8], [b'U', b'S', 0x7f]);
        // A null out-buffer is not written through.
        get_language(h, std::ptr::null_mut());
        delete(h);
    }

    #[test]
    fn a_null_handle_reads_as_zero_and_is_never_written() {
        let null = std::ptr::null_mut();
        let mut buf = [0x7f as c_char; 2];
        get_language(null, buf.as_mut_ptr());
        get_country(null, buf.as_mut_ptr());
        assert_eq!(buf, [0x7f as c_char; 2]);
        assert_eq!(get_screen_width_dp(null), 0);
        assert_eq!(get_screen_height_dp(null), 0);
        assert_eq!(get_screen_size(null), 0);
        assert_eq!(get_nav_hidden(null), NAVHIDDEN_NO);
        delete(null);
        from_asset_manager(null, null);
    }

    #[test]
    fn handles_are_snapshots_of_the_window_size() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_screen(1280, 720);
        let h = new();
        assert_eq!((get_screen_width_dp(h), get_screen_height_dp(h)), (1280, 720));
        assert_eq!(get_screen_size(h), SCREENSIZE_XLARGE);
        assert_eq!(get_nav_hidden(h), 1);

        // Zero leaves the width alone; the old handle keeps its copy until
        // it is refreshed.
        set_screen(0, 900);
        assert_eq!(get_screen_height_dp(h), 720);
        let later = new();
        assert_eq!((get_screen_width_dp(later), get_screen_height_dp(later)), (1280, 900));
        from_asset_manager(h, std::ptr::null_mut());
        assert_eq!((get_screen_width_dp(h), get_screen_height_dp(h)), (1280, 900));

        set_screen(-5, 720);
        assert_eq!(current().width_dp, 1280);
        delete(h);
        delete(later);
    }

    #[test]
    fn exactly_the_nine_imports_are_exported() {
        let names: Vec<&str> = overrides().iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), 9);
        for (name, addr) in overrides() {
            assert!(name.starts_with("AConfiguration_") && !addr.is_null(), "{name}");
        }
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 9, "{names:?}");
    }
}
