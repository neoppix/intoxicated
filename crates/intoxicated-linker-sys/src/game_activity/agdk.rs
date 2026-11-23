//! AGDK GameActivity: bring-up, lifecycle and input events, by engine handle.
//!
//! On Android the Java `GameActivity` calls these natives as the platform
//! drives the activity. Intoxicated has no platform to do that, so the C side
//! (`native/activity.cpp`) builds the objects they expect and calls the
//! natives the engine registered on `com/google/androidgamesdk/GameActivity`
//! itself.

use std::ffi::{c_int, c_void};

use crate::call::{c_text, event_status, handle_status, status, with_message};
use crate::ffi;

/// One finger in a multi-touch event, laid out as the C side's
/// `IntoxicatedTouchContact`: `id` is the Android pointer id, `x` and `y` are
/// window pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchContact {
    pub id: i32,
    pub x: f32,
    pub y: f32,
}

/// Installs, or clears with `None`, the hook the Java-side
/// `GameActivity.bootstrapTheApp()` runs.
///
/// The engine reads the flags it depends on as soon as `initializeNativeCode`
/// has set itself up, so the hook has to be in place before [`initialize`].
/// Measured on 2.748: the engine calls it after [`initialize`] has returned,
/// from one of its own threads, while the host goes on with the bring-up, so
/// the hook must be safe to run concurrently with the host's own calls. The
/// C side keeps it in an atomic; the engine calls it twice a launch and the
/// hook is expected to deliver once.
pub fn set_bootstrap(hook: Option<extern "C" fn()>) {
    // SAFETY: stores a function pointer with static lifetime.
    unsafe { ffi::intoxicated_set_bootstrap(hook) }
}

/// `GameActivity.initializeNativeCode`, with fresh Activity, AssetManager and
/// Configuration objects, the three paths, and no saved state. Returns the
/// engine's native handle, which every later handle-based call takes.
pub fn initialize(native: *mut c_void, internal: &str, obb: &str, external: &str) -> Result<i64, String> {
    let (internal, obb, external) = (c_text(internal)?, c_text(obb)?, c_text(external)?);
    // SAFETY: the strings outlive the call; `native` is checked by the C side.
    let (handle, message) = with_message(|err, len| unsafe {
        ffi::intoxicated_game_activity_init(native, internal.as_ptr(), obb.as_ptr(), external.as_ptr(), err, len)
    });
    if handle != 0 {
        return Ok(handle);
    }
    // The engine itself returning 0 leaves the buffer empty, and an empty
    // error is the one nobody can act on.
    let text = message.text();
    Err(if text.is_empty() { "initializeNativeCode returned a null handle".to_string() } else { text })
}

/// Drives start, resume, surface created and changed, the content rect and
/// window focus through the registered GameActivity natives.
///
/// A missing native here is a failure, not `None`. The native-side variables
/// `INTOXICATED_SKIP_AGDK_SURFACE`, `_LIFECYCLE`, `_STATE` and `_FOCUS` skip
/// parts of it.
pub fn start(handle: i64, width: i32, height: i32, format: i32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe { ffi::intoxicated_game_activity_start(handle, width, height, format, err, len) })
}

/// Calls one registered `(J)V` GameActivity native by name, such as
/// `onPauseNative` or `terminateNativeCode`.
pub fn lifecycle(handle: i64, native_name: &str) -> Result<Option<()>, String> {
    let name = c_text(native_name)?;
    // SAFETY: `name` outlives the call.
    handle_status(|err, len| unsafe { ffi::intoxicated_game_activity_lifecycle(handle, name.as_ptr(), err, len) })
}

/// `onWindowFocusChangedNative`.
pub fn window_focus(handle: i64, focused: bool) -> Result<Option<()>, String> {
    // SAFETY: plain values and the helper's buffer.
    handle_status(|err, len| unsafe {
        ffi::intoxicated_game_activity_window_focus(handle, c_int::from(focused), err, len)
    })
}

/// Surface changed and content rect again, after the host window resized.
/// Returns 0 or -1 only: the C side does not check the handle here, and a
/// native it cannot find is a failure rather than `None`.
pub fn surface_resized(handle: i64, format: i32, width: i32, height: i32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe {
        ffi::intoxicated_game_activity_surface_resized(handle, format, width, height, err, len)
    })
}

/// The repaint nudge, `onSurfaceRedrawNeededNative`.
pub fn surface_redraw_needed(handle: i64) -> Result<Option<()>, String> {
    // SAFETY: plain values and the helper's buffer.
    handle_status(|err, len| unsafe { ffi::intoxicated_game_activity_surface_redraw_needed(handle, err, len) })
}

/// Builds the process's one `InputConnection` and hands it to the engine
/// through `setInputConnectionNative`, as Android would from
/// `onCreateInputConnection`. Meant to be called once, early.
pub fn set_input_connection(handle: i64) -> Result<Option<()>, String> {
    // SAFETY: plain values and the helper's buffer.
    handle_status(|err, len| unsafe { ffi::intoxicated_game_activity_set_input_connection(handle, err, len) })
}

/// `onTextInputEventNative` with the field's whole contents and selection.
/// An unregistered native is a failure here, not `None`.
pub fn text_input(handle: i64, text: &str, sel_start: i32, sel_end: i32) -> Result<(), String> {
    let text = c_text(text)?;
    // SAFETY: `text` outlives the call.
    status(|err, len| unsafe {
        ffi::intoxicated_game_activity_text_input(handle, text.as_ptr(), sel_start, sel_end, err, len)
    })
}

/// One mouse-pointer `MotionEvent` through `onTouchEventNative`. `action` is
/// an Android `ACTION_*` value, coordinates are window pixels and times are
/// milliseconds. `Ok(Some(consumed))` is the engine's answer.
///
/// The C side wraps each event in its own JNI local frame, so calling this
/// once per event for a whole session does not grow the local reference
/// table.
#[allow(clippy::too_many_arguments)]
pub fn touch(
    handle: i64,
    action: i32,
    x: f32,
    y: f32,
    button_state: i32,
    action_button: i32,
    event_time_ms: i64,
    down_time_ms: i64,
) -> Result<Option<bool>, String> {
    // SAFETY: plain values, a local flag and the helper's buffer.
    event_status(|consumed, err, len| unsafe {
        ffi::intoxicated_game_activity_touch(
            handle,
            action,
            x,
            y,
            button_state,
            action_button,
            event_time_ms,
            down_time_ms,
            consumed,
            err,
            len,
        )
    })
}

/// A multi-finger `MotionEvent`. Slice order is the pointer index and
/// [`TouchContact::id`] the pointer id; `action` arrives already packed with
/// the index it applies to. The slice is only borrowed for the call.
pub fn touch_multi(
    handle: i64,
    action: i32,
    contacts: &[TouchContact],
    event_time_ms: i64,
    down_time_ms: i64,
) -> Result<Option<bool>, String> {
    if contacts.is_empty() {
        return Err("a touch event needs at least one contact; got no contacts".to_string());
    }
    let count = c_int::try_from(contacts.len()).map_err(|_| format!("{} contacts is too many", contacts.len()))?;
    // SAFETY: `contacts` is `count` repr(C) elements that outlive the call.
    event_status(|consumed, err, len| unsafe {
        ffi::intoxicated_game_activity_touch_multi(
            handle,
            action,
            contacts.as_ptr(),
            count,
            event_time_ms,
            down_time_ms,
            consumed,
            err,
            len,
        )
    })
}

/// `ACTION_SCROLL` at a point, with the axis values in detents.
pub fn scroll(
    handle: i64,
    x: f32,
    y: f32,
    hscroll: f32,
    vscroll: f32,
    event_time_ms: i64,
) -> Result<Option<bool>, String> {
    // SAFETY: plain values, a local flag and the helper's buffer.
    event_status(|consumed, err, len| unsafe {
        ffi::intoxicated_game_activity_scroll(handle, x, y, hscroll, vscroll, event_time_ms, consumed, err, len)
    })
}

/// `onKeyDownNative` or `onKeyUpNative`, chosen by `down`.
#[allow(clippy::too_many_arguments)]
pub fn key(
    handle: i64,
    down: bool,
    key_code: i32,
    scan_code: i32,
    meta_state: i32,
    repeat_count: i32,
    unicode_char: i32,
    event_time_ms: i64,
    down_time_ms: i64,
) -> Result<Option<bool>, String> {
    // SAFETY: plain values, a local flag and the helper's buffer.
    event_status(|consumed, err, len| unsafe {
        ffi::intoxicated_game_activity_key(
            handle,
            c_int::from(down),
            key_code,
            scan_code,
            meta_state,
            repeat_count,
            unicode_char,
            event_time_ms,
            down_time_ms,
            consumed,
            err,
            len,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::field_offset as offset;

    #[test]
    fn touch_contact_matches_the_c_layout() {
        assert_eq!(std::mem::size_of::<TouchContact>(), 12);
        assert_eq!(std::mem::align_of::<TouchContact>(), 4);
        assert_eq!(offset(|c: *const TouchContact| unsafe { std::ptr::addr_of!((*c).id) }), 0);
        assert_eq!(offset(|c: *const TouchContact| unsafe { std::ptr::addr_of!((*c).x) }), 4);
        assert_eq!(offset(|c: *const TouchContact| unsafe { std::ptr::addr_of!((*c).y) }), 8);
    }

    #[test]
    fn an_empty_touch_is_refused_before_the_call() {
        // Handle 0 is not an engine; the refusal has to come first.
        let e = touch_multi(0, 0, &[], 0, 0).unwrap_err();
        assert!(e.contains("no contacts"), "{e}");
    }

    #[test]
    fn initialize_without_a_vm_is_an_error_with_a_message() {
        let e = initialize(std::ptr::null_mut(), "/files", "/obb", "/external").unwrap_err();
        assert!(!e.is_empty());
        assert!(initialize(std::ptr::null_mut(), "/fi\0les", "", "").unwrap_err().contains("nul byte"));
    }
}
