//! Static natives that answer with a string: deep links, the MessageBus and
//! the web view's linking protocol (`native/deeplink.cpp`).
//!
//! Each answer goes into a fixed buffer. The C side refuses one that does not
//! fit, naming both sizes, rather than truncating it: a cut-off URL or bus
//! payload is still a well-formed string, and nothing downstream could tell.

use std::ffi::{c_int, c_void};

use crate::call::{answer, c_text, status};
use crate::ffi;

/// Room for a protocol name or URL.
const SHORT_ANSWER: usize = 512;
/// Room for a MessageBus payload, which is JSON.
const BUS_ANSWER: usize = 8192;

/// A zero-argument static native on a named class that returns a `String`.
pub fn call_static_ret_string(native: *mut c_void, class_name: &str) -> Result<String, String> {
    let class = c_text(class_name)?;
    // SAFETY: `class` and the helper's buffers outlive the call.
    answer(SHORT_ANSWER, |out, out_len, err, len| unsafe {
        ffi::intoxicated_deeplink_protocol_string(native, class.as_ptr(), out, out_len, err, len)
    })
}

/// A static `(String, String)` native on a named class that returns a
/// `String`.
pub fn call_static_two_strings_ret_string(
    native: *mut c_void,
    class_name: &str,
    a: &str,
    b: &str,
) -> Result<String, String> {
    let (class, a, b) = (c_text(class_name)?, c_text(a)?, c_text(b)?);
    // SAFETY: the strings and the helper's buffers outlive the call.
    answer(SHORT_ANSWER, |out, out_len, err, len| unsafe {
        ffi::intoxicated_deeplink_two_strings_ret_string(
            native,
            class.as_ptr(),
            a.as_ptr(),
            b.as_ptr(),
            out,
            out_len,
            err,
            len,
        )
    })
}

/// A static `(String)` native on a named class that returns a `String`; the
/// MessageBus's, so the answer buffer is sized for a JSON payload.
pub fn call_static_string_ret_string(native: *mut c_void, class_name: &str, arg: &str) -> Result<String, String> {
    let (class, arg) = (c_text(class_name)?, c_text(arg)?);
    // SAFETY: the strings and the helper's buffers outlive the call.
    answer(BUS_ANSWER, |out, out_len, err, len| unsafe {
        ffi::intoxicated_deeplink_string_ret_string(native, class.as_ptr(), arg.as_ptr(), out, out_len, err, len)
    })
}

/// Hands a URL to a linking protocol's cold-start entry. `Ok(true)` is the
/// engine saying it took the URL.
pub fn cold_start_protocol_launch(native: *mut c_void, class_name: &str, url: &str) -> Result<bool, String> {
    let (class, url) = (c_text(class_name)?, c_text(url)?);
    // Not 0 or 1, so a success that never wrote it is visible as such.
    let mut handled: c_int = -1;
    // SAFETY: the strings and `handled` outlive the call.
    status(|err, len| unsafe {
        ffi::intoxicated_deeplink_cold_start(native, class.as_ptr(), url.as_ptr(), &mut handled, err, len)
    })?;
    Ok(handled != 0)
}

/// `init(Context)` on a linking-protocol class.
pub fn protocol_init(native: *mut c_void, class_name: &str) -> Result<(), String> {
    let class = c_text(class_name)?;
    // SAFETY: `class` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_deeplink_protocol_init(native, class.as_ptr(), err, len) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_calls_refuse_a_null_native() {
        let null = std::ptr::null_mut();
        assert!(call_static_ret_string(null, "com/example/Linking").is_err());
        assert!(call_static_two_strings_ret_string(null, "com/example/Bus", "a", "b").is_err());
        assert!(call_static_string_ret_string(null, "com/example/Bus", "{}").is_err());
        assert!(cold_start_protocol_launch(null, "com/example/Linking", "roblox://").is_err());
        assert!(protocol_init(null, "com/example/Linking").is_err());
    }
}
