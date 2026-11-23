//! `Linking.openURL`: the engine asking the host to open a link.
//!
//! Terms, Privacy and an experience's external links do not go through an
//! Android `Intent`. The engine sends a message bus request, protocol
//! `Linking`, method `openURL`, carrying `{"url": ...}`, and waits for
//! `{"success": <bool>}`, which decides what it shows the user next. Nothing
//! answered that request before this module, so every such link silently did
//! nothing.
//!
//! The vocabulary is what the engine's own `Linking` protocol getters
//! (`getProtocolName`, `getOpenURLId`, `getUrlKey`, `getSuccessKey`) returned
//! in the notes this project started from; it is fixed here rather than read
//! back from the engine.
//!
//! The URL is never printed, at any verbosity: a link from the app can carry
//! a sign-in ticket.

use std::ffi::{c_char, c_int, c_void, CStr};

/// The bus protocol the engine sends link requests on.
const PROTOCOL: &str = "Linking";
/// The method of an open request.
const METHOD: &str = "openURL";

/// What to tell the engine about one request's text. True only when the
/// desktop took the link: the engine acts on the answer, so a success that
/// did not happen would mislead the user.
fn handle(request: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(request) else {
        eprintln!("[linking] openURL request was not JSON");
        return false;
    };
    let Some(url) = value.get("url").and_then(serde_json::Value::as_str) else {
        eprintln!("[linking] openURL request carried no \"url\"");
        return false;
    };
    // A game script can put anything in the request, so the scheme is checked,
    // and it is checked once, the same way for every caller that opens links
    // that way: `desktop::open_url` refuses anything that is not http or
    // https before it spawns anything, which covers `file://`, `javascript:` and
    // custom handlers. It also reaps the browser on a thread of its own, so the
    // engine thread that asked is never held up.
    match crate::desktop::open_url(url) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("[linking] openURL refused: {e}");
            false
        }
    }
}

/// The answer for `success`, NUL-terminated, into `out`. False, writing
/// nothing, when it does not fit.
fn write_answer(success: bool, out: &mut [u8]) -> bool {
    let answer: &[u8] = if success { b"{\"success\":true}\0" } else { b"{\"success\":false}\0" };
    match out.get_mut(..answer.len()) {
        Some(dst) => {
            dst.copy_from_slice(answer);
            true
        }
        None => false,
    }
}

/// The bus's request sink for `Linking.openURL`, called on whichever engine
/// thread sends the request. Returns non-zero after writing an answer; zero
/// lets the bus layer answer `{"success":false}` itself.
extern "C" fn on_open_url(request: *const c_char, out: *mut c_char, len: usize) -> c_int {
    if request.is_null() || out.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the bus layer hands a NUL-terminated request and a zeroed
    // buffer of `len` bytes, both live for the call.
    let (text, buf) = unsafe {
        (CStr::from_ptr(request).to_string_lossy(), std::slice::from_raw_parts_mut(out.cast::<u8>(), len))
    };
    c_int::from(write_answer(handle(&text), buf))
}

/// Answer the engine's `Linking.openURL` from now on. `symbol` resolves a
/// name in the loaded engine. Call once, before the looper starts; nothing
/// here can stop the launch.
pub fn arm(symbol: impl Fn(&str) -> Option<*mut c_void>) {
    let Some(set) = symbol(crate::bus::SET_REQUEST_HANDLER_RAW) else {
        println!("  linking: setRequestHandlerRaw is not exported; external links will not open");
        return;
    };
    match crate::bus::set_request_handler(set, PROTOCOL, METHOD, on_open_url) {
        Ok(()) => println!("  linking: {PROTOCOL}.{METHOD} handler bound"),
        Err(e) => println!("  linking: could not bind {PROTOCOL}.{METHOD}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sink as the bus layer calls it, with a zeroed buffer of `len`.
    fn ask(request: &str, len: usize) -> (c_int, Vec<u8>) {
        let request = std::ffi::CString::new(request).unwrap();
        let mut buf = vec![0u8; len];
        let rc = on_open_url(request.as_ptr(), buf.as_mut_ptr().cast(), buf.len());
        (rc, buf)
    }

    fn answer_of(buf: &[u8]) -> &str {
        CStr::from_bytes_until_nul(buf).unwrap().to_str().unwrap()
    }

    // No vector here may be an http or https URL: that would start a browser.
    #[test]
    fn a_request_that_cannot_be_opened_is_answered_false() {
        for request in [
            "{}",
            r#"{"notaurl":"https://example.com"}"#,
            "not json at all",
            "",
            r#"{"url":"file:///etc/passwd"}"#,
            r#"{"url":"javascript:alert(1)"}"#,
            r#"{"url":"mailto:nobody"}"#,
            r#"{"url":"example.com"}"#,
            r#"{"url":5}"#,
            "5",
        ] {
            assert!(!handle(request), "{request}");
            let (rc, buf) = ask(request, 64);
            assert_eq!(rc, 1, "{request}");
            assert_eq!(answer_of(&buf), r#"{"success":false}"#, "{request}");
        }
    }

    #[test]
    fn an_answer_that_does_not_fit_is_not_written() {
        // `{"success":false}` and its NUL are 18 bytes.
        let (rc, buf) = ask("{}", 16);
        assert_eq!(rc, 0);
        assert!(buf.iter().all(|&b| b == 0));
        let (rc, buf) = ask("{}", 18);
        assert_eq!(rc, 1);
        assert_eq!(answer_of(&buf), r#"{"success":false}"#);
        let mut exact = [0u8; 17];
        assert!(write_answer(true, &mut exact));
        assert_eq!(&exact, b"{\"success\":true}\0");
    }

    #[test]
    fn null_arguments_are_left_to_the_bus_layer() {
        let mut buf = [0u8; 64];
        assert_eq!(on_open_url(std::ptr::null(), buf.as_mut_ptr().cast(), buf.len()), 0);
        assert_eq!(on_open_url(c"{}".as_ptr(), std::ptr::null_mut(), 64), 0);
        assert_eq!(on_open_url(c"{}".as_ptr(), buf.as_mut_ptr().cast(), 0), 0);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn invalid_utf8_is_read_lossily() {
        let mut buf = [0u8; 64];
        let request = b"{\"url\":\"\xff\"}\0";
        assert_eq!(on_open_url(request.as_ptr().cast(), buf.as_mut_ptr().cast(), buf.len()), 1);
        assert_eq!(answer_of(&buf), r#"{"success":false}"#);
    }

    #[test]
    fn a_missing_native_is_reported_not_fatal() {
        arm(|_| None);
    }
}
