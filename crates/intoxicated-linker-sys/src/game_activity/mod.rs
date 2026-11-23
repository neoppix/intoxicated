//! Driving the engine through its JNI natives.
//!
//! Most functions take, first, the raw address of one of `libroblox.so`'s
//! exported or registered JNI natives (from [`crate::Library::symbol`]) and
//! call it on the C side with the JNI environment and receiver it expects.
//! The address is not checked here; the C side reports a null one as an
//! error rather than calling it. The handle-based AGDK calls take instead the
//! engine handle [`initialize`] returned.
//!
//! What every call shares:
//!
//! Errors are the C side's own message, written into a 512-byte buffer. The
//! handle-based calls distinguish a native the engine has not registered yet
//! (`Ok(None)`, an ordinary start-up race) from a failure (`Err`).
//!
//! Strings are copied to NUL-terminated buffers that live for the call; an
//! interior NUL is refused before anything is called.
//!
//! Every C entry that reaches the engine catches C++ exceptions and returns
//! them as errors, so nothing unwinds into Rust. The callbacks Rust installs
//! are `extern "C"` functions and must not unwind into C either.
//!
//! Every C entry that reaches the engine attaches the calling thread to the
//! VM first, so once [`crate::jni::create_vm`] has run these may be called
//! from any thread; they run synchronously on it.
//!
//! The bring-up order the runtime follows and the C side assumes:
//! [`set_bootstrap`] and [`set_ui_mode_night`] before [`initialize`];
//! [`initialize`] before any handle-based call; [`set_display_size`] and
//! [`set_display_physical_mm`] straight after it; [`start`] once a window
//! exists; [`set_input_connection`] once, early.

mod agdk;
pub(crate) mod battery;
mod bridge;
mod display;
mod input;
mod linking;
mod session;
mod textbox;

pub use agdk::{
    initialize, key, lifecycle, scroll, set_bootstrap, set_input_connection, start, surface_redraw_needed,
    surface_resized, text_input, touch, touch_multi, window_focus, TouchContact,
};
pub use battery::{report_battery_state_changed, report_battery_status, BatteryStatusFields};
pub use bridge::{
    activity_lifecycle, app_start, appbridge_call_bare, appbridge_init, appbridge_start_app, appbridge_update_surface,
    asset_manager_init, call_bare, call_bare_on, call_static_bare_bool, call_static_bool_string, call_static_strings,
    get_fint, init_client_settings, init_flags, init_storage_manager, post_client_settings_loaded,
    preload_flag_overrides, read_local_flags, set_device_info, set_init_params, storage_init,
};
pub use display::{set_display_physical_mm, set_display_size, set_touchscreen_present, set_ui_mode_night};
pub use input::{
    pass_input, pass_key_event, pass_mouse_button, pass_mouse_move, pass_mouse_wheel, pass_text, sync_textbox,
    update_keyboard_size,
};
pub use linking::{
    call_static_ret_string, call_static_string_ret_string, call_static_two_strings_ret_string,
    cold_start_protocol_launch, protocol_init,
};
pub use session::{
    app_ready_set_sink, cookies_for_domain, cookies_register_handler, home_interactive_set_sink, identity_clear,
    identity_publish, identity_set_sinks, Jar,
};
pub use textbox::{
    focused_textbox, focused_textbox_info, games_loaded, ime_state_generation, ime_state_selection, ime_state_text,
    registered_natives, textbox_generation, textbox_text, RawTextBoxInfo,
};
