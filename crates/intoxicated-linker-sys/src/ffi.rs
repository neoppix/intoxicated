//! Every C entry point this crate calls, in one place.
//!
//! The definitions are in `native/`: the `intoxicated_linker_*` names in
//! `linker_surface.cpp` (archive `intoxicated_linker_shim`), everything else in
//! the files of `intoxicated_jni_shim` named beside each group. The C side
//! spells handles as `long` or `long long` and counts as `int`; on x86-64
//! FreeBSD those are i64 and i32, which is what is written here.
//!
//! Error-reporting calls share one tail: `err` is a caller-owned buffer of
//! `err_len` bytes that the C side fills with `snprintf` on failure.

use std::ffi::{c_char, c_int, c_void};

use crate::game_activity::battery::BatteryWire;
use crate::game_activity::{RawTextBoxInfo, TouchContact};

extern "C" {
    // linker_surface.cpp: the bionic linker.
    pub fn intoxicated_linker_init();
    pub fn intoxicated_linker_load_library(
        soname: *const c_char,
        names: *const *const c_char,
        addrs: *const *mut c_void,
        count: usize,
    ) -> *mut c_void;
    pub fn intoxicated_linker_update_ld_library_path(path: *const c_char);
    pub fn intoxicated_linker_dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    pub fn intoxicated_linker_dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    pub fn intoxicated_linker_dlerror() -> *const c_char;
    pub fn intoxicated_linker_get_library_base(handle: *mut c_void) -> usize;
    pub fn intoxicated_linker_get_library_code_region(handle: *mut c_void, start: *mut usize, len: *mut usize);

    // java_vm.cpp: the process JavaVM.
    pub fn intoxicated_jni_create_vm() -> *mut c_void;
    pub fn intoxicated_jni_call_onload(on_load: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_jni_dump_classes(path: *const c_char) -> c_int;

    // activity.cpp: AGDK GameActivity, by engine handle (the bootstrap hook
    // itself is kept in jni_values.cpp).
    pub fn intoxicated_set_bootstrap(hook: Option<extern "C" fn()>);
    pub fn intoxicated_game_activity_init(
        native: *mut c_void,
        internal_path: *const c_char,
        obb_path: *const c_char,
        external_path: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> i64;
    pub fn intoxicated_game_activity_start(
        handle: i64,
        width: c_int,
        height: c_int,
        format: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_lifecycle(
        handle: i64,
        native_name: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_window_focus(
        handle: i64,
        focused: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_surface_resized(
        handle: i64,
        format: c_int,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_surface_redraw_needed(handle: i64, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_game_activity_set_input_connection(handle: i64, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_game_activity_text_input(
        handle: i64,
        text: *const c_char,
        sel_start: c_int,
        sel_end: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_touch(
        handle: i64,
        action: c_int,
        x: f32,
        y: f32,
        button_state: c_int,
        action_button: c_int,
        event_time_ms: i64,
        down_time_ms: i64,
        consumed: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_touch_multi(
        handle: i64,
        action: c_int,
        contacts: *const TouchContact,
        count: c_int,
        event_time_ms: i64,
        down_time_ms: i64,
        consumed: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_scroll(
        handle: i64,
        x: f32,
        y: f32,
        hscroll: f32,
        vscroll: f32,
        event_time_ms: i64,
        consumed: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_game_activity_key(
        handle: i64,
        down: c_int,
        key_code: c_int,
        scan_code: c_int,
        meta_state: c_int,
        repeat_count: c_int,
        unicode_char: c_int,
        event_time_ms: i64,
        down_time_ms: i64,
        consumed: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;

    // activity.cpp: Roblox's own input natives, by raw native address.
    pub fn intoxicated_input_key_event(
        native: *mut c_void,
        down: c_int,
        key_code: c_int,
        modifiers: c_int,
        is_repeat: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_input_pass_text(
        native: *mut c_void,
        which: i64,
        text: *const c_char,
        flag: c_int,
        cursor: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_input_sync_textbox(
        native: *mut c_void,
        text: *const c_char,
        cursor: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_input_update_keyboard_size(
        native: *mut c_void,
        visible: c_int,
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_input_mouse_move(
        native: *mut c_void,
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_input_mouse_button(
        native: *mut c_void,
        x: f32,
        y: f32,
        down: c_int,
        button: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_input_mouse_wheel(
        native: *mut c_void,
        x: f32,
        y: f32,
        delta: f32,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_input_pass_input(
        native: *mut c_void,
        pointer_id: c_int,
        x: f32,
        y: f32,
        action: c_int,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;

    // jni_values.cpp and activity.cpp: native-side state, no engine call.
    pub fn intoxicated_ime_state_generation() -> u32;
    pub fn intoxicated_ime_state_text(buf: *mut c_char, len: c_int) -> c_int;
    pub fn intoxicated_ime_state_selection(start: *mut c_int, end: *mut c_int);
    pub fn intoxicated_registered_natives(class_name: *const c_char, out: *mut c_char, out_len: usize) -> c_int;

    // jni_params.cpp: settings, flags and the app bridge.
    pub fn intoxicated_set_init_params(
        native: *mut c_void,
        assets: *const c_char,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_asset_manager_init(native: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_storage_init(
        native: *mut c_void,
        a: *const c_char,
        b: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_init_storage_manager(
        native: *mut c_void,
        a: *const c_char,
        b: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_set_device_info(
        native: *mut c_void,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_init_flags(
        native: *mut c_void,
        settings: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_read_local_flags(native: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_init_client_settings(
        native: *mut c_void,
        a: *const c_char,
        b: *const c_char,
        c: *const c_char,
        out_result: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_post_client_settings_loaded(native: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_get_fint(
        native: *mut c_void,
        name: *const c_char,
        fallback: c_int,
        out_result: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_preload_flag_overrides(
        native: *mut c_void,
        json: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_appbridge_init(
        native: *mut c_void,
        assets: *const c_char,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_appbridge_start_app(
        native: *mut c_void,
        assets: *const c_char,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_appbridge_update_surface_app(
        native: *mut c_void,
        assets: *const c_char,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_appbridge_update_surface_game(
        native: *mut c_void,
        assets: *const c_char,
        width: c_int,
        height: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_appbridge_call_bare(native: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_appbridge_app_start(
        native: *mut c_void,
        a: *const c_char,
        b: *const c_char,
        flag: c_int,
        c: *const c_char,
        d: *const c_char,
        e: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_call_bare(native: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_appbridge_call_bare_cls(
        native: *mut c_void,
        class_name: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_activity_lifecycle(
        native: *mut c_void,
        activity: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_call_static_strings(
        native: *mut c_void,
        class_name: *const c_char,
        args: *const *const c_char,
        count: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_call_static_bool_string(
        native: *mut c_void,
        class_name: *const c_char,
        flag: c_int,
        text: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_call_static_bare_bool(
        native: *mut c_void,
        class_name: *const c_char,
        out_result: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;

    // jni_values.cpp: display and platform setters.
    pub fn intoxicated_set_ui_mode_night(night: c_int);
    pub fn intoxicated_set_display_size(width: c_int, height: c_int);
    pub fn intoxicated_set_display_physical_mm(width_mm: c_int, height_mm: c_int);
    pub fn intoxicated_set_touchscreen_present(present: c_int);

    // deeplink.cpp: string-returning static calls on a named class.
    pub fn intoxicated_deeplink_protocol_string(
        native: *mut c_void,
        class_name: *const c_char,
        out: *mut c_char,
        out_len: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_deeplink_two_strings_ret_string(
        native: *mut c_void,
        class_name: *const c_char,
        a: *const c_char,
        b: *const c_char,
        out: *mut c_char,
        out_len: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_deeplink_string_ret_string(
        native: *mut c_void,
        class_name: *const c_char,
        arg: *const c_char,
        out: *mut c_char,
        out_len: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_deeplink_cold_start(
        native: *mut c_void,
        class_name: *const c_char,
        url: *const c_char,
        out_handled: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_deeplink_protocol_init(
        native: *mut c_void,
        class_name: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;

    // jni_values.cpp: notification sinks, identity, text boxes, loads.
    pub fn intoxicated_app_ready_set_sink(sink: Option<extern "C" fn(*const c_char)>);
    pub fn intoxicated_home_interactive_set_sink(sink: Option<extern "C" fn()>);
    pub fn intoxicated_identity_set_sinks(
        on_login: Option<extern "C" fn(*const c_char)>,
        on_logout: Option<extern "C" fn()>,
    );
    pub fn intoxicated_identity_publish(
        user_id: i64,
        username: *const c_char,
        display_name: *const c_char,
        membership_type: i64,
        is_under13: c_int,
        has_subscription: c_int,
    );
    pub fn intoxicated_identity_clear();
    pub fn intoxicated_textbox_handle() -> i64;
    pub fn intoxicated_textbox_generation() -> u32;
    pub fn intoxicated_textbox_info(out: *mut RawTextBoxInfo) -> c_int;
    pub fn intoxicated_textbox_text(buf: *mut c_char, len: c_int) -> c_int;
    pub fn intoxicated_games_loaded() -> u32;

    // jni_cookies.cpp: the engine's cookie protocol.
    pub fn intoxicated_cookies_set_host_sink(sink: Option<extern "C" fn(*const c_char)>);
    pub fn intoxicated_cookies_register_handler(native: *mut c_void, err: *mut c_char, err_len: usize) -> c_int;
    pub fn intoxicated_cookies_get_for_domain(
        native: *mut c_void,
        class_name: *const c_char,
        domain: *const c_char,
        out: *mut c_char,
        out_len: usize,
        needed: *mut usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;

    // battery_status.cpp: the two battery reports.
    pub fn intoxicated_report_battery_state_changed(
        native: *mut c_void,
        status: c_int,
        plugged: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn intoxicated_report_battery_status(
        native: *mut c_void,
        status: *const BatteryWire,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}
