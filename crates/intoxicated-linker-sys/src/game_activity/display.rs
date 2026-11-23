//! What the native side reports about the display and the device.
//!
//! No engine call: these set values `native/jni_values.cpp` holds for the
//! Java world to hand out later, when the engine asks for DisplayMetrics,
//! the User-Agent or the screen's physical size. They cannot fail.

use std::ffi::c_int;

use crate::ffi;

/// Night mode for the Configuration the engine reads during
/// [`super::initialize`], so set it before: above zero is night, 0 is day,
/// -1 keeps the default.
pub fn set_ui_mode_night(night: i32) {
    // SAFETY: plain value.
    unsafe { ffi::intoxicated_set_ui_mode_night(night) }
}

/// The window's real size, for DisplayMetrics, the User-Agent and
/// AConfiguration. Call it once the size is known and again when it changes.
pub fn set_display_size(width: i32, height: i32) {
    // SAFETY: plain values.
    unsafe { ffi::intoxicated_set_display_size(width, height) }
}

/// The display's physical size in millimetres. 0 means unknown and is a
/// legitimate answer.
pub fn set_display_physical_mm(width_mm: i32, height_mm: i32) {
    // SAFETY: plain values.
    unsafe { ffi::intoxicated_set_display_physical_mm(width_mm, height_mm) }
}

/// Whether to advertise a touchscreen. The C side reads it only during
/// start-up.
pub fn set_touchscreen_present(present: bool) {
    // SAFETY: plain value.
    unsafe { ffi::intoxicated_set_touchscreen_present(c_int::from(present)) }
}
