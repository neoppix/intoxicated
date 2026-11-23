//! Reads of native-side state: the focused text box, the IME, place loads and
//! the engine's registered natives. None of these calls the engine, and none
//! can fail; "nothing" has its own answer in each.

use std::ffi::CString;

use crate::call::{copied_text, text_before_nul};
use crate::ffi;

/// Room for a text field's contents. The C side truncates to one byte less.
const TEXT_CAPACITY: usize = 4096;
/// Room for the `registered_natives` listing.
const NATIVES_CAPACITY: usize = 8192;

/// The focused text box's spec as the engine handed it over at focus time:
/// the fifteen arguments of `NativeTextBoxInfo.<init>`, in order, mirroring
/// the C side's `TextBoxSpec` (`native/jni_values.h`) slot for slot. The four
/// boolean slots are `int` on both sides, so there is no one-byte member to
/// pad around.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawTextBoxInfo {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub font_size: f32,
    pub multiline: i32,
    /// `Enum.TextXAlignment`: Left 0, Center 1, Right 2.
    pub x_alignment: i32,
    /// `Enum.TextYAlignment`: Top 0, Center 1, Bottom 2.
    pub y_alignment: i32,
    pub text_color: i32,
    pub font: i32,
    pub text_input_type: i32,
    pub return_key_type: i32,
    pub manual_focus_release: i32,
    pub text_wrapped: i32,
    pub z14: i32,
}

/// Handle of the box the engine focused through `showKeyboard`. `None` means
/// nothing has focus, and then no text may be sent at all: handle 0 is not a
/// box.
pub fn focused_textbox() -> Option<i64> {
    // SAFETY: an atomic load on the C side.
    let handle = unsafe { ffi::intoxicated_textbox_handle() };
    (handle != 0).then_some(handle)
}

/// Bumped on every focus and blur, so a reader can tell "the same box" from
/// "a different box that happens to have the same handle". Wraps.
pub fn textbox_generation() -> u32 {
    // SAFETY: an atomic load on the C side.
    unsafe { ffi::intoxicated_textbox_generation() }
}

/// The focused box's spec, or `None` when nothing is focused or no spec
/// arrived with the focus. Never a zeroed placeholder: a box at (0, 0) sized
/// 0 by 0 cannot be told from one nobody described.
pub fn focused_textbox_info() -> Option<RawTextBoxInfo> {
    let mut info = RawTextBoxInfo::default();
    // SAFETY: `info` is a repr(C) mirror of the struct the C side copies.
    let known = unsafe { ffi::intoxicated_textbox_info(&mut info) };
    (known != 0).then_some(info)
}

/// The focused box's contents as of focus time, truncated by the C side to
/// 4095 bytes.
pub fn textbox_text() -> String {
    // SAFETY: the helper's buffer, of the length it passes.
    copied_text(TEXT_CAPACITY, |buf, len| unsafe { ffi::intoxicated_textbox_text(buf, len) })
}

/// Bumped on every `InputConnection.setState` and `restartInput`.
pub fn ime_state_generation() -> u32 {
    // SAFETY: an atomic load on the C side.
    unsafe { ffi::intoxicated_ime_state_generation() }
}

/// The text the engine last reported through `InputConnection.setState`.
pub fn ime_state_text() -> String {
    // SAFETY: the helper's buffer, of the length it passes.
    copied_text(TEXT_CAPACITY, |buf, len| unsafe { ffi::intoxicated_ime_state_text(buf, len) })
}

/// Selection start and end from the last `setState`.
pub fn ime_state_selection() -> (i32, i32) {
    let (mut start, mut end) = (0, 0);
    // SAFETY: two locals as out-pointers.
    unsafe { ffi::intoxicated_ime_state_selection(&mut start, &mut end) };
    (start, end)
}

/// How many times the engine has reported a place finished loading.
pub fn games_loaded() -> u32 {
    // SAFETY: an atomic load on the C side.
    unsafe { ffi::intoxicated_games_loaded() }
}

/// Every native the engine registered on `class_name`, as space-separated
/// `name@address` entries; entries that do not fit are left out whole.
///
/// An unregistered class answers "class not registered". Before there is a
/// JNI environment, or for a name with an interior NUL, the answer is empty.
pub fn registered_natives(class_name: &str) -> String {
    let Ok(class) = CString::new(class_name) else {
        return String::new();
    };
    let mut out = vec![0u8; NATIVES_CAPACITY];
    // SAFETY: `class` and `out` outlive the call.
    let count = unsafe { ffi::intoxicated_registered_natives(class.as_ptr(), out.as_mut_ptr().cast(), out.len()) };
    if count < 0 {
        return String::new();
    }
    text_before_nul(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::field_offset;
    use std::ffi::{c_char, c_int};

    // Test seams in native/jni_values.cpp. Declared here only: the
    // runtime has no business focusing a text box.
    extern "C" {
        fn intoxicated_textbox_test_focus(
            handle: i64,
            text: *const c_char,
            s0: f32,
            s1: f32,
            s2: f32,
            s3: f32,
            s4: f32,
            s5: c_int,
            s6: c_int,
            s7: c_int,
            s8: c_int,
            s9: c_int,
            s10: c_int,
            s11: c_int,
            s12: c_int,
            s13: c_int,
            s14: c_int,
        );
        fn intoxicated_textbox_focused(handle: i64, text: *const c_char, info: *const RawTextBoxInfo);
        fn intoxicated_textbox_blurred();
    }

    /// The C side fills its struct by member name from fifteen separate
    /// scalars, and Rust reads the result back by field name. Handing a
    /// Rust-built struct to C and reading it back would prove only that
    /// `memcpy` works, and would pass with the mirror shifted by a slot.
    ///
    /// One function, because the focus state is process-global and the test
    /// harness runs tests on parallel threads.
    #[test]
    fn raw_textbox_info_matches_the_c_struct_slot_for_slot() {
        let text = CString::new("hello").unwrap();
        let before = textbox_generation();
        // SAFETY: `text` outlives the call; the C side copies it.
        unsafe {
            intoxicated_textbox_test_focus(42, text.as_ptr(), 1.5, 2.5, 3.5, 4.5, 5.5, 1, 6, 7, 8, 9, 10, 11, 0, 1, 1)
        };
        assert_eq!(focused_textbox(), Some(42));
        assert_ne!(textbox_generation(), before, "a focus bumps the generation");
        assert_eq!(textbox_text(), "hello");
        // Every value distinct where a one-slot shift could land, so a
        // misaligned mirror cannot pass by accident.
        let expected = RawTextBoxInfo {
            x: 1.5,
            y: 2.5,
            width: 3.5,
            height: 4.5,
            font_size: 5.5,
            multiline: 1,
            x_alignment: 6,
            y_alignment: 7,
            text_color: 8,
            font: 9,
            text_input_type: 10,
            return_key_type: 11,
            manual_focus_release: 0,
            text_wrapped: 1,
            z14: 1,
        };
        assert_eq!(focused_textbox_info(), Some(expected));

        // SAFETY: no arguments.
        unsafe { intoxicated_textbox_blurred() };
        assert_eq!(focused_textbox(), None);
        assert_eq!(focused_textbox_info(), None);

        // A focus without a spec is a box Rust may send text to but must not
        // style: there is no fallback spec in a test process.
        // SAFETY: `text` outlives the call; a null spec is allowed.
        unsafe { intoxicated_textbox_focused(7, text.as_ptr(), std::ptr::null()) };
        assert_eq!(focused_textbox(), Some(7));
        assert_eq!(focused_textbox_info(), None);

        // SAFETY: no arguments.
        unsafe { intoxicated_textbox_blurred() };
        assert_eq!(focused_textbox(), None);
    }

    #[test]
    fn raw_textbox_info_has_the_c_size_and_slots() {
        assert_eq!(std::mem::size_of::<RawTextBoxInfo>(), 60);
        assert_eq!(std::mem::align_of::<RawTextBoxInfo>(), 4);
        macro_rules! at {
            ($field:ident) => {
                field_offset(|i: *const RawTextBoxInfo| unsafe { std::ptr::addr_of!((*i).$field) })
            };
        }
        let slots = [
            at!(x),
            at!(y),
            at!(width),
            at!(height),
            at!(font_size),
            at!(multiline),
            at!(x_alignment),
            at!(y_alignment),
            at!(text_color),
            at!(font),
            at!(text_input_type),
            at!(return_key_type),
            at!(manual_focus_release),
            at!(text_wrapped),
            at!(z14),
        ];
        for (k, at) in slots.into_iter().enumerate() {
            assert_eq!(at, 4 * k, "slot {k}");
        }
    }

    #[test]
    fn registered_natives_before_there_is_a_vm_is_empty() {
        assert_eq!(registered_natives("com/google/androidgamesdk/GameActivity"), "");
        assert_eq!(registered_natives("com/\0google"), "");
    }
}
