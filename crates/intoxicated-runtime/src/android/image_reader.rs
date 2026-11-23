//! `AImageReader_*`, `AImage_*`, two `AHardwareBuffer_*` reference counts and
//! `AMediaCodec_createCodecByName`: imports this process has nothing behind.
//!
//! Roblox 2.746.0.1266 is the first build whose `libroblox.so` imports the
//! image reader family, and 2.748.0.1527 adds `AMediaCodec_createCodecByName`.
//! An import nobody answers stops `dlopen` on the first missing name (it was
//! `AImageReader_delete`) before any engine code runs, so all ten have to
//! exist.
//!
//! INFERRED from the names alone, and not yet reached in any run: the caller
//! is hardware video decoding into a hardware buffer, next to the engine's
//! other `AMediaCodec_*` imports and its HardwareBuffer video flags.
//!
//! There is no image reader and no media codec here, and every answer says
//! so. Creating something fails, and anything that takes an object rejects
//! it, because no object is ever handed out. A blanket stub returning 0 would
//! be wrong in a way the engine could not notice: 0 is `AMEDIA_OK`, and the
//! caller would go on to use an out-pointer nobody wrote. So every
//! out-pointer the NDK documents as filled on success is set to null.

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};

/// `AMEDIA_ERROR_BASE` from `media/NdkMediaError.h`.
const MEDIA_ERROR_BASE: i32 = -10000;
/// `AMEDIA_ERROR_UNSUPPORTED`: the answer to a request for a new reader.
pub const MEDIA_ERROR_UNSUPPORTED: i32 = MEDIA_ERROR_BASE - 2;
/// `AMEDIA_ERROR_INVALID_OBJECT`: the answer for any reader or image passed
/// in, since none of them can be one this module made.
pub const MEDIA_ERROR_INVALID_OBJECT: i32 = MEDIA_ERROR_BASE - 3;

/// Set once the refused reader has been reported.
static READER_REPORTED: AtomicBool = AtomicBool::new(false);
/// Set once a refused codec name has been reported.
static CODEC_REPORTED: AtomicBool = AtomicBool::new(false);

/// True for the first caller only, for a report that is worth one line per
/// process and no more.
fn first_time(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::Relaxed)
}

/// Write a null pointer through `out`, unless `out` is itself null.
fn clear(out: *mut *mut c_void) {
    if !out.is_null() {
        // SAFETY: a non-null out-pointer from the caller points at one
        // writable pointer, as every NDK out-parameter of this shape does.
        unsafe { out.write(std::ptr::null_mut()) };
    }
}

extern "C" fn reader_new_with_usage(
    width: i32,
    height: i32,
    format: i32,
    usage: u64,
    max_images: i32,
    reader: *mut *mut c_void,
) -> i32 {
    clear(reader);
    if first_time(&READER_REPORTED) {
        eprintln!(
            "[media] AImageReader_newWithUsage({width}x{height}, format {format:#x}, \
             usage {usage:#x}, max images {max_images}) refused with \
             AMEDIA_ERROR_UNSUPPORTED: Intoxicated has no image reader"
        );
    }
    MEDIA_ERROR_UNSUPPORTED
}

extern "C" fn reader_delete(_reader: *mut c_void) {}

extern "C" fn reader_get_window(_reader: *mut c_void, window: *mut *mut c_void) -> i32 {
    clear(window);
    MEDIA_ERROR_INVALID_OBJECT
}

extern "C" fn reader_acquire_next_image(_reader: *mut c_void, image: *mut *mut c_void) -> i32 {
    clear(image);
    MEDIA_ERROR_INVALID_OBJECT
}

extern "C" fn image_delete(_image: *mut c_void) {}

extern "C" fn image_get_hardware_buffer(_image: *const c_void, buffer: *mut *mut c_void) -> i32 {
    clear(buffer);
    MEDIA_ERROR_INVALID_OBJECT
}

/// The timestamp is a plain value, not a pointer, so there is nothing safe
/// to put in it; it is left untouched.
extern "C" fn image_get_timestamp(_image: *const c_void, _timestamp_ns: *mut i64) -> i32 {
    MEDIA_ERROR_INVALID_OBJECT
}

/// A buffer could only have come out of an `AImage`, and there are none.
extern "C" fn hardware_buffer_acquire(_buffer: *mut c_void) {}

/// As [`hardware_buffer_acquire`].
extern "C" fn hardware_buffer_release(_buffer: *mut c_void) {}

extern "C" fn media_codec_create_by_name(name: *const c_char) -> *mut c_void {
    if !name.is_null() && first_time(&CODEC_REPORTED) {
        // SAFETY: a non-null codec name is a NUL-terminated string, per the
        // NDK's contract for this call.
        let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
        eprintln!(
            "[media] AMediaCodec_createCodecByName(\"{name}\") returned null: \
             Intoxicated has no media codec"
        );
    }
    std::ptr::null_mut()
}

/// The ten names and their entry points, for `android::overrides`.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("AImageReader_newWithUsage", reader_new_with_usage),
        f!("AImageReader_delete", reader_delete),
        f!("AImageReader_getWindow", reader_get_window),
        f!("AImageReader_acquireNextImage", reader_acquire_next_image),
        f!("AImage_delete", image_delete),
        f!("AImage_getHardwareBuffer", image_get_hardware_buffer),
        f!("AImage_getTimestamp", image_get_timestamp),
        f!("AHardwareBuffer_acquire", hardware_buffer_acquire),
        f!("AHardwareBuffer_release", hardware_buffer_release),
        f!("AMediaCodec_createCodecByName", media_codec_create_by_name),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::{null, null_mut, NonNull};

    /// A non-null pointer that no call here may keep.
    fn stale() -> *mut c_void {
        NonNull::<c_void>::dangling().as_ptr()
    }

    #[test]
    fn the_status_codes_are_the_ndk_values() {
        assert_eq!(MEDIA_ERROR_UNSUPPORTED, -10002);
        assert_eq!(MEDIA_ERROR_INVALID_OBJECT, -10003);
    }

    #[test]
    fn a_new_reader_is_refused_and_the_out_pointer_is_cleared() {
        let mut reader = stale();
        let status = reader_new_with_usage(1280, 720, 0x22, 0x100, 4, &mut reader);
        assert_eq!(status, MEDIA_ERROR_UNSUPPORTED);
        assert!(reader.is_null());
        // Asked again, and with no out-pointer at all, the answer is the same.
        let mut reader = stale();
        assert_eq!(reader_new_with_usage(64, 64, 0x23, 0, 1, &mut reader), MEDIA_ERROR_UNSUPPORTED);
        assert!(reader.is_null());
        assert_eq!(reader_new_with_usage(64, 64, 0x23, 0, 1, null_mut()), MEDIA_ERROR_UNSUPPORTED);
    }

    #[test]
    fn calls_on_an_object_reject_it_and_clear_what_they_would_fill() {
        let mut window = stale();
        assert_eq!(reader_get_window(stale(), &mut window), MEDIA_ERROR_INVALID_OBJECT);
        assert!(window.is_null());

        let mut image = stale();
        assert_eq!(reader_acquire_next_image(stale(), &mut image), MEDIA_ERROR_INVALID_OBJECT);
        assert!(image.is_null());

        let mut buffer = stale();
        assert_eq!(image_get_hardware_buffer(stale(), &mut buffer), MEDIA_ERROR_INVALID_OBJECT);
        assert!(buffer.is_null());

        // A null out-pointer is not written through.
        assert_eq!(reader_get_window(null_mut(), null_mut()), MEDIA_ERROR_INVALID_OBJECT);
        assert_eq!(reader_acquire_next_image(null_mut(), null_mut()), MEDIA_ERROR_INVALID_OBJECT);
        assert_eq!(image_get_hardware_buffer(null(), null_mut()), MEDIA_ERROR_INVALID_OBJECT);
    }

    #[test]
    fn the_timestamp_is_rejected_and_left_alone() {
        let mut ns: i64 = 0x5eed;
        assert_eq!(image_get_timestamp(stale(), &mut ns), MEDIA_ERROR_INVALID_OBJECT);
        assert_eq!(ns, 0x5eed);
        assert_eq!(image_get_timestamp(null(), null_mut()), MEDIA_ERROR_INVALID_OBJECT);
    }

    #[test]
    fn the_void_calls_accept_anything() {
        for p in [null_mut(), stale()] {
            reader_delete(p);
            image_delete(p);
            hardware_buffer_acquire(p);
            hardware_buffer_release(p);
        }
    }

    #[test]
    fn no_codec_is_made_for_a_name_or_for_null() {
        let name = c"c2.android.avc.decoder";
        assert!(media_codec_create_by_name(name.as_ptr()).is_null());
        assert!(media_codec_create_by_name(name.as_ptr()).is_null());
        assert!(media_codec_create_by_name(null()).is_null());
    }

    #[test]
    fn a_report_flag_answers_true_once() {
        let flag = AtomicBool::new(false);
        assert!(first_time(&flag));
        assert!(!first_time(&flag));
        assert!(!first_time(&flag));
    }

    #[test]
    fn exactly_the_ten_imports_are_exported() {
        let table = overrides();
        let mut names: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
        for name in [
            "AImageReader_newWithUsage",
            "AImageReader_delete",
            "AImageReader_getWindow",
            "AImageReader_acquireNextImage",
            "AImage_delete",
            "AImage_getHardwareBuffer",
            "AImage_getTimestamp",
            "AHardwareBuffer_acquire",
            "AHardwareBuffer_release",
            "AMediaCodec_createCodecByName",
        ] {
            assert!(names.contains(&name), "{name} is missing");
        }
        assert!(table.iter().all(|(_, addr)| !addr.is_null()));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 10, "{names:?}");
        assert_eq!(table.len(), 10);
    }
}
