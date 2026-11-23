//! Roblox's own input natives, by raw native address.
//!
//! These bypass AGDK: `NativeGLInterface` and `NativeInputInterface` are the
//! engine's own Java bridge, and it registers a static native on them for
//! each kind of input. The C side (`native/activity.cpp`) supplies the
//! class as receiver and passes booleans as 0 or 1.

use std::ffi::{c_int, c_void};

use crate::call::{c_text, status};
use crate::ffi;

/// `NativeGLInterface.nativePassKeyEvent`.
pub fn pass_key_event(
    native: *mut c_void,
    down: bool,
    key_code: i32,
    modifiers: i32,
    is_repeat: bool,
) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer; `native` is checked by C.
    status(|err, len| unsafe {
        ffi::intoxicated_input_key_event(
            native,
            c_int::from(down),
            key_code,
            modifiers,
            c_int::from(is_repeat),
            err,
            len,
        )
    })
}

/// `nativePassText` into the box whose handle is `which`.
pub fn pass_text(native: *mut c_void, which: i64, text: &str, flag: bool, cursor: i32) -> Result<(), String> {
    let text = c_text(text)?;
    // SAFETY: `text` outlives the call.
    status(|err, len| unsafe {
        ffi::intoxicated_input_pass_text(native, which, text.as_ptr(), c_int::from(flag), cursor, err, len)
    })
}

/// `syncTextboxTextAndCursorPosition2`, which applies to whatever box has
/// focus.
pub fn sync_textbox(native: *mut c_void, text: &str, cursor: i32) -> Result<(), String> {
    let text = c_text(text)?;
    // SAFETY: `text` outlives the call.
    status(|err, len| unsafe { ffi::intoxicated_input_sync_textbox(native, text.as_ptr(), cursor, err, len) })
}

/// `updateKeyboardSize`.
pub fn update_keyboard_size(native: *mut c_void, visible: bool, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe {
        ffi::intoxicated_input_update_keyboard_size(native, c_int::from(visible), x, y, w, h, err, len)
    })
}

/// `NativeInputInterface.nativePassMouseMove`.
pub fn pass_mouse_move(native: *mut c_void, x: f32, y: f32, dx: f32, dy: f32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe { ffi::intoxicated_input_mouse_move(native, x, y, dx, dy, err, len) })
}

/// `nativePassMouseButton`.
pub fn pass_mouse_button(native: *mut c_void, x: f32, y: f32, down: bool, button: i32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe { ffi::intoxicated_input_mouse_button(native, x, y, c_int::from(down), button, err, len) })
}

/// `nativePassMouseWheel`, `delta` in detents.
pub fn pass_mouse_wheel(native: *mut c_void, x: f32, y: f32, delta: f32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe { ffi::intoxicated_input_mouse_wheel(native, x, y, delta, err, len) })
}

/// `nativePassInput`: one finger per call.
pub fn pass_input(
    native: *mut c_void,
    pointer_id: i32,
    x: f32,
    y: f32,
    action: i32,
    width: i32,
    height: i32,
) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe {
        ffi::intoxicated_input_pass_input(native, pointer_id, x, y, action, width, height, err, len)
    })
}
