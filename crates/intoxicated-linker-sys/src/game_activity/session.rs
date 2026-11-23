//! The signed-in session: notification sinks, identity, and cookies.
//!
//! A sink is an `extern "C"` function with static lifetime that the C side
//! stores (in an atomic, except the cookie host sink) and calls on one of the
//! engine's threads, from inside the engine's own callback. A string handed to
//! a sink is valid only for that call, and a sink must not unwind.

use std::ffi::{c_char, c_int, c_void, CString};
use std::fmt;

use crate::call::{c_text, status};
use crate::ffi;

/// Room for one domain's cookie jar.
const JAR_CAPACITY: usize = 256 * 1024;

/// A cookie jar as the engine reported it, or as it was stored.
///
/// This is a live session credential, which is why it is a type at all:
/// `Debug` prints the length and nothing else, there is no `Display`, and
/// the text is reached only through [`Jar::expose`], a name chosen to be
/// noticed in review. The leak this prevents is a one-line diagnostic
/// somebody adds while chasing something unrelated.
pub struct Jar(String);

impl Jar {
    /// Wraps a jar read back from storage.
    pub fn from_stored(text: String) -> Self {
        Jar(text)
    }

    /// The jar's text, `name=value` pairs joined by `"; "`.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Jar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Jar({} bytes)", self.0.len())
    }
}

/// Installs, or clears with `None`, the receiver of the DataModel
/// `APP_READY` notification. Its argument is the app shell's state name.
pub fn app_ready_set_sink(sink: Option<extern "C" fn(*const c_char)>) {
    // SAFETY: stores a function pointer with static lifetime.
    unsafe { ffi::intoxicated_app_ready_set_sink(sink) }
}

/// Installs, or clears with `None`, the receiver of `HOME_PAGE_INTERACTIVE`.
pub fn home_interactive_set_sink(sink: Option<extern "C" fn()>) {
    // SAFETY: stores a function pointer with static lifetime.
    unsafe { ffi::intoxicated_home_interactive_set_sink(sink) }
}

/// Installs the identity sinks.
///
/// `on_login` receives the payload of `DID_LOG_IN`, `DID_SIGN_UP` or
/// `DID_SWITCH_ACCOUNT`: identity JSON, which is personal data and must never
/// be logged. `on_logout` runs on `DID_LOG_OUT` and
/// `LUA_UNAUTHORIZED_LOG_OUT`.
pub fn identity_set_sinks(on_login: extern "C" fn(*const c_char), on_logout: extern "C" fn()) {
    // SAFETY: stores two function pointers with static lifetime.
    unsafe { ffi::intoxicated_identity_set_sinks(Some(on_login), Some(on_logout)) }
}

/// Gives the native Java mirrors the signed-in identity. The C side copies it
/// under its own lock, so this may run at any time relative to the engine.
///
/// A name with an interior NUL skips the whole publish: a username cut short
/// at the NUL would be a different, wrong identity, and publishing half of
/// one is worse than leaving the previous one in place.
pub fn identity_publish(
    user_id: i64,
    username: &str,
    display_name: &str,
    membership_type: i64,
    is_under13: bool,
    has_subscription: bool,
) {
    let (Ok(username), Ok(display_name)) = (CString::new(username), CString::new(display_name)) else {
        return;
    };
    // SAFETY: both strings outlive the call, and the C side copies them.
    unsafe {
        ffi::intoxicated_identity_publish(
            user_id,
            username.as_ptr(),
            display_name.as_ptr(),
            membership_type,
            c_int::from(is_under13),
            c_int::from(has_subscription),
        )
    }
}

/// Resets the identity mirrors to nobody signed in.
pub fn identity_clear() {
    // SAFETY: no arguments.
    unsafe { ffi::intoxicated_identity_clear() }
}

/// Installs `sink`, then registers a handler with
/// `JNICookieProtocol.updateOnSetCookieHandler`.
///
/// The sink goes in first and stays even if the registration fails. It later
/// receives, on the engine's HTTP thread, only the host a `Set-Cookie`
/// response came from: never a cookie value, never the URL.
pub fn cookies_register_handler(native: *mut c_void, sink: extern "C" fn(*const c_char)) -> Result<(), String> {
    // SAFETY: a plain store of a function pointer with static lifetime; this
    // runs during start-up, before the engine has an HTTP thread to read it.
    unsafe { ffi::intoxicated_cookies_set_host_sink(Some(sink)) };
    // SAFETY: the helper's buffer only; `native` is checked by the C side.
    status(|err, len| unsafe { ffi::intoxicated_cookies_register_handler(native, err, len) })
}

/// `nativeGetCookiesForDomain(String)` on a named class: the engine's whole
/// jar for `domain`.
///
/// A jar that does not fit is an error naming its size, never a shorter jar:
/// a jar cut off mid-value still parses as cookies, and handing that back to
/// the engine on the next launch looks like an expired session rather than
/// like the bug it is. For the same reason bytes that are not UTF-8 are an
/// error, not replaced.
pub fn cookies_for_domain(native: *mut c_void, class_name: &str, domain: &str) -> Result<Jar, String> {
    let (class, domain) = (c_text(class_name)?, c_text(domain)?);
    let mut out = vec![0u8; JAR_CAPACITY];
    let mut needed: usize = 0;
    // SAFETY: the strings, `out` and `needed` outlive the call.
    status(|err, len| unsafe {
        ffi::intoxicated_cookies_get_for_domain(
            native,
            class.as_ptr(),
            domain.as_ptr(),
            out.as_mut_ptr().cast(),
            out.len(),
            &mut needed,
            err,
            len,
        )
    })?;
    // The C side already refuses this; checked again because `truncate`
    // past the end would quietly keep the whole buffer.
    if needed >= out.len() {
        return Err(format!("the jar for this domain is {needed} bytes and the buffer is {}", out.len()));
    }
    out.truncate(needed);
    String::from_utf8(out)
        .map(Jar)
        .map_err(|e| format!("the jar for this domain is not UTF-8 (at byte {})", e.utf8_error().valid_up_to()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jar_shows_its_length_and_nothing_else() {
        let jar = Jar::from_stored("SESSION=do-not-print-me".into());
        let shown = format!("{jar:?}");
        assert_eq!(shown, "Jar(23 bytes)");
        assert_eq!(jar.len(), 23);
        assert!(!jar.is_empty());
        assert_eq!(jar.expose(), "SESSION=do-not-print-me");
        assert!(Jar::from_stored(String::new()).is_empty());
    }

    #[test]
    fn cookie_calls_refuse_a_null_native() {
        let e = cookies_for_domain(std::ptr::null_mut(), "com/example/Cookies", "example.com").unwrap_err();
        assert!(!e.is_empty());
        assert!(cookies_for_domain(std::ptr::null_mut(), "com/example/Cookies", "exa\0mple.com")
            .unwrap_err()
            .contains("nul byte"));
    }
}
