//! The conventions every fallible native call shares, written once.
//!
//! A call into the engine reports through a return code and, on failure, a
//! message the C side `snprintf`s into a buffer the caller owns. Getting that
//! translation slightly different in seventy places is how one function ends
//! up returning an empty error or reading past a missing NUL, so every
//! wrapper goes through the helpers here instead of doing it inline.

use std::ffi::{c_char, c_int, CString};

/// Bytes in the message buffer of every fallible call. The C side truncates
/// to fit, so this bounds the message, never the call.
pub(crate) const MESSAGE_LEN: usize = 512;

/// Return code the handle-based GameActivity calls use for "that native has
/// not been registered yet". The engine registers its GameActivity natives
/// from its own threads during start-up, so this is a race, not a fault.
pub(crate) const NOT_REGISTERED: c_int = -2;

/// The text in `bytes` up to the first NUL, or all of it when there is none,
/// with invalid UTF-8 replaced rather than refused: these are diagnostics,
/// and a message with one bad byte is still worth reading.
pub(crate) fn text_before_nul(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// A NUL-terminated copy of `s` that lives as long as the caller keeps it.
///
/// An interior NUL would silently cut the string short on the C side, so it
/// is refused here, before anything is called, with the error as text.
pub(crate) fn c_text(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|e| e.to_string())
}

/// Runs `f` with a fresh, zero-filled message buffer and hands back its
/// result together with the buffer.
pub(crate) fn with_message<R>(f: impl FnOnce(*mut c_char, usize) -> R) -> (R, Message) {
    let mut message = Message([0; MESSAGE_LEN]);
    let result = f(message.0.as_mut_ptr().cast(), MESSAGE_LEN);
    (result, message)
}

/// The message buffer after a call.
pub(crate) struct Message([u8; MESSAGE_LEN]);

impl Message {
    pub(crate) fn text(&self) -> String {
        text_before_nul(&self.0)
    }
}

/// A call that returns 0 on success and anything else on failure.
pub(crate) fn status(f: impl FnOnce(*mut c_char, usize) -> c_int) -> Result<(), String> {
    match with_message(f) {
        (0, _) => Ok(()),
        (_, message) => Err(message.text()),
    }
}

/// A handle-based GameActivity call: 0 success, [`NOT_REGISTERED`] for a
/// native the engine has not registered yet, anything else a failure.
pub(crate) fn handle_status(f: impl FnOnce(*mut c_char, usize) -> c_int) -> Result<Option<()>, String> {
    match with_message(f) {
        (0, _) => Ok(Some(())),
        (NOT_REGISTERED, _) => Ok(None),
        (_, message) => Err(message.text()),
    }
}

/// A handle-based input event, which also reports whether the engine
/// consumed it.
pub(crate) fn event_status(f: impl FnOnce(*mut c_int, *mut c_char, usize) -> c_int) -> Result<Option<bool>, String> {
    let mut consumed: c_int = 0;
    handle_status(|err, len| f(&mut consumed, err, len)).map(|seen| seen.map(|()| consumed != 0))
}

/// A call that writes a NUL-terminated answer into an output buffer of
/// `capacity` bytes. The C side refuses an answer that does not fit rather
/// than truncating it, so a successful call's text is the whole answer.
pub(crate) fn answer(
    capacity: usize,
    f: impl FnOnce(*mut c_char, usize, *mut c_char, usize) -> c_int,
) -> Result<String, String> {
    let mut out = vec![0u8; capacity];
    status(|err, len| f(out.as_mut_ptr().cast(), out.len(), err, len))?;
    Ok(text_before_nul(&out))
}

/// Reads one of the C side's copied-out text fields: `f` fills a buffer of
/// the given size and returns how many bytes it wrote, not counting the NUL.
pub(crate) fn copied_text(capacity: usize, f: impl FnOnce(*mut c_char, c_int) -> c_int) -> String {
    let mut buf = vec![0u8; capacity];
    let len = c_int::try_from(capacity).unwrap_or(c_int::MAX);
    match usize::try_from(f(buf.as_mut_ptr().cast(), len)) {
        Ok(n) if n > 0 => String::from_utf8_lossy(&buf[..n.min(capacity)]).into_owned(),
        _ => String::new(),
    }
}

/// Byte offset of a field, for the layout tests that pin the Rust mirrors to
/// the C structs. `field` must only compute the field's address
/// (`addr_of!((*p).field)`), never read through the pointer.
#[cfg(test)]
pub(crate) fn field_offset<T, F>(field: impl FnOnce(*const T) -> *const F) -> usize {
    let base = std::mem::MaybeUninit::<T>::uninit();
    field(base.as_ptr()) as usize - base.as_ptr() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_stops_at_the_first_nul() {
        assert_eq!(text_before_nul(b"no JavaVM\0stale tail"), "no JavaVM");
        assert_eq!(text_before_nul(b"\0everything after is ignored"), "");
    }

    #[test]
    fn a_message_without_a_nul_is_the_whole_buffer() {
        assert_eq!(text_before_nul(b"filled to the brim"), "filled to the brim");
    }

    #[test]
    fn a_message_with_bad_utf8_is_still_readable() {
        assert_eq!(text_before_nul(b"bad \xff byte\0"), "bad \u{fffd} byte");
    }

    #[test]
    fn an_interior_nul_is_refused_before_any_call() {
        let e = c_text("com/roblox\0/Thing").unwrap_err();
        assert!(e.contains("nul byte"), "the NUL error, as text: {e}");
        assert_eq!(c_text("").unwrap().as_bytes(), b"");
    }

    #[test]
    fn the_return_code_families_map_as_documented() {
        let fail = |rc: c_int| {
            move |err: *mut c_char, len: usize| {
                let text = b"it went wrong\0";
                assert!(len >= text.len());
                // SAFETY: `err` is the helper's own buffer of `len` bytes.
                unsafe { std::ptr::copy_nonoverlapping(text.as_ptr().cast(), err, text.len()) };
                rc
            }
        };
        assert_eq!(status(|_, _| 0), Ok(()));
        assert_eq!(status(fail(-1)), Err("it went wrong".to_string()));
        assert_eq!(status(fail(7)), Err("it went wrong".to_string()));

        assert_eq!(handle_status(|_, _| 0), Ok(Some(())));
        assert_eq!(handle_status(fail(NOT_REGISTERED)), Ok(None));
        assert_eq!(handle_status(fail(-1)), Err("it went wrong".to_string()));

        let event = |rc: c_int, consumed: c_int| {
            move |out: *mut c_int, _: *mut c_char, _: usize| {
                // SAFETY: `out` points at the helper's own flag.
                unsafe { *out = consumed };
                rc
            }
        };
        assert_eq!(event_status(event(0, 1)), Ok(Some(true)));
        assert_eq!(event_status(event(0, 0)), Ok(Some(false)));
        assert_eq!(event_status(event(NOT_REGISTERED, 1)), Ok(None));
    }

    #[test]
    fn copied_text_takes_exactly_the_count_it_is_given() {
        let fill = |n: c_int| {
            move |buf: *mut c_char, len: c_int| {
                assert_eq!(len, 16);
                // SAFETY: the helper's buffer holds 16 bytes.
                unsafe { std::ptr::copy_nonoverlapping(b"abcdef\0".as_ptr().cast(), buf, 7) };
                n
            }
        };
        assert_eq!(copied_text(16, fill(3)), "abc");
        assert_eq!(copied_text(16, fill(0)), "");
        assert_eq!(copied_text(16, fill(-1)), "");
    }
}
