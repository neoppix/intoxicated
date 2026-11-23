//! The input core's rules, without an X server or an engine. Each test feeds
//! the core what `x11_window.rs` would and checks the engine calls it decided, in
//! order.

use super::*;

const SHIFT: u32 = 1;
const LOCK: u32 = 2;
const CTRL: u32 = 4;
const ALT: u32 = 8;

fn cfg() -> Config {
    Config { agdk_mouse: true, agdk_key: false, pass_key: true, keys_while_typing: false, wheel_scale: 1.0 }
}

fn core() -> Core {
    Core::new(cfg())
}

fn calls(c: &mut Core) -> Vec<Call> {
    c.take_calls().into_iter().map(|e| e.call).collect()
}

/// Only the `nativePassKeyEvent` calls.
fn keys(c: &mut Core) -> Vec<(bool, i32, i32)> {
    calls(c)
        .into_iter()
        .filter_map(|call| match call {
            Call::Key { down, code, meta } => Some((down, code, meta)),
            _ => None,
        })
        .collect()
}

fn key(down: bool, x_keycode: u32, keysym0: u64, state: u32) -> HostKey {
    HostKey { down, x_keycode, state, keysym0, unicode: 0, time: 10 }
}

// ------------------------------------------------------------------ ordering

/// Defect 1.8/9.3: a flick then a click in one drain reached the engine as
/// click first, camera second. Waiting motion now goes out before the click.
#[test]
fn locked_motion_read_before_a_click_reaches_the_engine_before_it() {
    let mut c = core();
    let anchor = (640, 360);
    c.add_locked_motion(anchor, (3.0, -1.0), true, 0, 0, 5);
    c.add_locked_motion(anchor, (2.5, 0.5), true, 0, 0, 6);
    c.button(&[(ACTION_DOWN, BUTTON_PRIMARY, 0), (ACTION_BUTTON_PRESS, BUTTON_PRIMARY, BUTTON_PRIMARY)], 640.0, 360.0, true, BUTTON_PRIMARY, 7, 7);
    assert_eq!(
        calls(&mut c),
        vec![
            Call::Motion { action: ACTION_HOVER_MOVE, x: 640.0, y: 360.0, buttons: 0, action_button: 0, time: 6, down_time: 0 },
            Call::MouseMove { x: 640.0, y: 360.0, dx: 5.5, dy: -0.5 },
            Call::Motion { action: ACTION_DOWN, x: 640.0, y: 360.0, buttons: 1, action_button: 0, time: 7, down_time: 7 },
            Call::Motion { action: ACTION_BUTTON_PRESS, x: 640.0, y: 360.0, buttons: 1, action_button: 1, time: 7, down_time: 7 },
            Call::MouseButton { x: 640.0, y: 360.0, down: true, index: 0 },
        ]
    );
}

#[test]
fn motion_click_motion_click_keeps_every_movement_on_its_side_of_its_click() {
    let mut c = Core::new(Config { agdk_mouse: false, ..cfg() });
    let a = (640, 360);
    c.add_locked_motion(a, (1.0, 0.0), true, 0, 0, 1);
    c.add_locked_motion(a, (1.0, 0.0), true, 0, 0, 1);
    c.plain_button(640.0, 360.0, true, BUTTON_PRIMARY);
    c.add_locked_motion(a, (0.0, 4.0), true, 1, 1, 2);
    c.plain_button(640.0, 360.0, false, BUTTON_PRIMARY);
    c.flush_motion();
    assert_eq!(
        calls(&mut c),
        vec![
            Call::MouseMove { x: 640.0, y: 360.0, dx: 2.0, dy: 0.0 },
            Call::MouseButton { x: 640.0, y: 360.0, down: true, index: 0 },
            Call::MouseMove { x: 640.0, y: 360.0, dx: 0.0, dy: 4.0 },
            Call::MouseButton { x: 640.0, y: 360.0, down: false, index: 0 },
        ]
    );
}

#[test]
fn keys_wheel_and_text_also_flush_waiting_motion_first() {
    let mut c = Core::new(Config { agdk_mouse: false, ..cfg() });
    let a = (100, 100);
    c.add_locked_motion(a, (7.0, 0.0), true, 0, 0, 1);
    c.host_key(&key(true, 10, 0x31, 0), false);
    c.add_locked_motion(a, (0.0, 2.0), false, 0, 0, 2);
    c.wheel(5.0, 5.0, 0.0, 1.0, 3);
    c.add_locked_motion(a, (1.0, 1.0), true, 0, 0, 4);
    c.field_state_call(9, &FieldState { text: "x".into(), sel_start: 1, sel_end: 1, caret: 1 }, true);
    let got = calls(&mut c);
    let kinds: Vec<&str> = got.iter().map(Call::kind).collect();
    assert_eq!(kinds, vec!["move", "key", "move", "wheel", "move", "text"], "{got:?}");
}

#[test]
fn a_drain_that_moves_out_and_back_sends_nothing() {
    let mut c = core();
    c.add_locked_motion((640, 360), (5.0, 3.0), true, 0, 0, 1);
    c.add_locked_motion((640, 360), (-5.0, -3.0), true, 0, 0, 1);
    c.flush_motion();
    assert_eq!(calls(&mut c), vec![]);
}

#[test]
fn the_pending_sum_keeps_fractions_and_empties_when_taken() {
    // The vectors x11_window.rs's accumulator test carried.
    let mut sum = PendingMotion::default();
    assert_eq!(sum.take(), None, "an empty drain wakes nobody");
    sum.add((0.25, 1.0), true);
    sum.add((0.5, -3.0), true);
    sum.add((0.25, 0.0), true);
    assert_eq!(sum.events, 3);
    assert_eq!(sum.take(), Some((1.0, -2.0)));
    assert_eq!(sum.take(), None, "taking empties it");
    sum.add((1.0, 1.0), true);
    sum.add((-1.0, -1.0), true);
    assert_eq!(sum.take(), None);
}

#[test]
fn the_first_raw_sample_drops_the_warp_paths_count_of_the_same_movement() {
    let mut c = Core::new(Config { agdk_mouse: false, ..cfg() });
    // Core motion read the warp way before the lock's first raw sample...
    c.add_locked_motion((640, 360), (4.0, 0.0), false, 0, 0, 1);
    // ...then the raw sample of the same movement.
    c.drop_pending_warp();
    c.add_locked_motion((640, 360), (4.0, 0.0), true, 0, 0, 1);
    c.flush_motion();
    assert_eq!(calls(&mut c), vec![Call::MouseMove { x: 640.0, y: 360.0, dx: 4.0, dy: 0.0 }]);
    assert_eq!((c.deliveries, c.raw_deliveries), (1, 1));
}

#[test]
fn a_recentre_delivers_what_was_read_at_the_old_anchor_there() {
    let mut c = Core::new(Config { agdk_mouse: false, ..cfg() });
    c.add_locked_motion((100, 200), (1.0, 0.0), true, 2, 0, 1);
    c.add_locked_motion((640, 360), (0.0, 1.0), true, 2, 0, 2);
    c.flush_motion();
    assert_eq!(
        calls(&mut c),
        vec![
            Call::MouseMove { x: 100.0, y: 200.0, dx: 1.0, dy: 0.0 },
            Call::MouseMove { x: 640.0, y: 360.0, dx: 0.0, dy: 1.0 },
        ]
    );
}

#[test]
fn locked_motion_with_a_button_held_is_an_agdk_move_not_a_hover() {
    let mut c = core();
    c.add_locked_motion((10, 10), (1.0, 1.0), true, BUTTON_SECONDARY, 40, 50);
    c.flush_motion();
    assert_eq!(
        calls(&mut c)[0],
        Call::Motion { action: ACTION_MOVE, x: 10.0, y: 10.0, buttons: BUTTON_SECONDARY, action_button: 0, time: 50, down_time: 40 }
    );
}

#[test]
fn discarded_motion_is_never_delivered() {
    let mut c = core();
    c.add_locked_motion((10, 10), (9.0, 9.0), true, 0, 0, 1);
    c.discard_motion();
    c.plain_button(10.0, 10.0, true, BUTTON_PRIMARY);
    assert_eq!(calls(&mut c), vec![Call::MouseButton { x: 10.0, y: 10.0, down: true, index: 0 }]);
}

// ---------------------------------------------------------------- keys: 9.1

/// Defect 9.1: with Shift held the number row's keysyms (`exclam`, ...) were
/// outside the table and nothing reached the engine; sprint plus a hotbar key
/// did nothing.
#[test]
fn shift_then_number_row_reaches_the_engine_as_key_1_with_shift() {
    let mut c = Core::new(Config { agdk_key: true, ..cfg() });
    c.host_key(&key(true, 50, 0xffe1, 0), false); // Shift_L down
    c.host_key(&key(true, 10, 0x31, SHIFT), false); // the 1 key, Shift held
    c.host_key(&key(false, 10, 0x31, SHIFT), false);
    let got = calls(&mut c);
    assert!(got.contains(&Call::Key { down: true, code: 42, meta: META_SHIFT_ON }), "{got:?}");
    assert!(got.contains(&Call::Key { down: true, code: 2, meta: META_SHIFT_ON }), "{got:?}");
    assert!(got.contains(&Call::Key { down: false, code: 2, meta: META_SHIFT_ON }), "{got:?}");
    // AGDK's keycode is KEYCODE_1 (8), from the unshifted keysym.
    assert!(got.iter().any(|c| matches!(c, Call::AgdkKey { down: true, code: 8, meta: 1, .. })), "{got:?}");
    // The same for 2: evdev 3, KEYCODE_2.
    c.host_key(&key(true, 11, 0x32, SHIFT), false);
    assert!(calls(&mut c).contains(&Call::Key { down: true, code: 3, meta: META_SHIFT_ON }));
}

#[test]
fn shift_tab_reaches_the_engine_as_tab_with_shift() {
    let mut c = Core::new(Config { agdk_key: true, ..cfg() });
    c.host_key(&key(true, 23, 0xff09, SHIFT), false);
    let got = calls(&mut c);
    assert!(got.contains(&Call::Key { down: true, code: 15, meta: META_SHIFT_ON }), "{got:?}");
    assert!(got.iter().any(|c| matches!(c, Call::AgdkKey { down: true, code: 61, meta: 1, .. })), "{got:?}");
}

#[test]
fn keypad_super_menu_and_layout_keys_all_reach_the_engine_by_position() {
    let mut c = core();
    for (kc, keysym0) in [
        (87u32, 0xff9c_u64), // KP_1 position with Num Lock off (KP_End)
        (90, 0xff9e),        // KP_0 (KP_Insert)
        (104, 0xff8d),       // KP_Enter
        (133, 0xffeb),       // Super_L
        (135, 0xff67),       // Menu
        (34, 0xfc),          // German u-umlaut, evdev 26
        (20, 0xdf),          // German sharp s, evdev 12
        (94, 0x3c),          // ISO 102nd key, evdev 86
        (49, 0xfe52),        // dead_circumflex, evdev 41
        (108, 0xfe03),       // AltGr as ISO_Level3_Shift, evdev 100
    ] {
        c.host_key(&key(true, kc, keysym0, 0), false);
        c.host_key(&key(false, kc, keysym0, 0), false);
    }
    let codes: Vec<i32> = keys(&mut c).into_iter().filter(|k| k.0).map(|k| k.1).collect();
    assert_eq!(codes, vec![79, 82, 96, 125, 127, 26, 12, 86, 41, 100]);
}

#[test]
fn the_spec_key_vectors_hold() {
    let mut c = core();
    c.host_key(&key(true, 25, 0x77, 0), false);
    assert_eq!(keys(&mut c), vec![(true, 17, 0)], "w");
    c.host_key(&key(true, 25, 0x77, 0), false);
    assert_eq!(keys(&mut c), vec![], "a repeat press is nothing");
    c.host_key(&key(false, 25, 0x77, 0), false);
    assert_eq!(keys(&mut c), vec![(false, 17, 0)]);
    c.host_key(&key(true, 50, 0xffe1, 0), false);
    assert_eq!(keys(&mut c), vec![(true, 42, 0x1)]);
    c.host_key(&key(false, 50, 0xffe1, SHIFT), false);
    assert_eq!(keys(&mut c), vec![(false, 42, 0x0)]);
    c.host_key(&key(true, 38, 0x61, SHIFT), false);
    assert_eq!(keys(&mut c), vec![(true, 30, 0x1)]);
    c.host_key(&key(false, 38, 0x61, 0), false);
    keys(&mut c);
    c.host_key(&key(true, 38, 0x61, LOCK), false);
    assert_eq!(keys(&mut c), vec![(true, 30, 0x10_0000)]);
    c.host_key(&key(false, 38, 0x61, 0), false);
    keys(&mut c);
    c.host_key(&key(true, 38, 0x61, CTRL), false);
    assert_eq!(keys(&mut c), vec![(true, 30, 0x1000)]);
    c.host_key(&key(true, 25, 0x77, ALT), false);
    assert_eq!(keys(&mut c), vec![(true, 17, 0x2)]);
    // The one the spec listed as "nothing (DEFECT)": exclam with Shift.
    c.host_key(&key(true, 10, 0x31, SHIFT), false);
    assert_eq!(keys(&mut c), vec![(true, 2, 0x1)]);
    c.host_key(&key(true, 9, 0xff1b, 0), false);
    assert_eq!(keys(&mut c), vec![(true, 1, 0)], "Escape reaches Roblox");
    c.host_key(&key(true, 104, 0xff8d, 0), false);
    assert_eq!(keys(&mut c), vec![(true, 96, 0)], "KP_Enter is KEY_KPENTER");
}

#[test]
fn keycodes_outside_8_to_255_are_not_keys() {
    let mut c = core();
    c.host_key(&key(true, 0, 0, 0), false); // the input method's put-back
    c.host_key(&key(true, 7, 0, 0), false);
    assert_eq!(calls(&mut c), vec![]);
    assert!(c.held_keys().is_empty());
}

// -------------------------------------------------------------- keys: 9.2

/// Defect 9.2: press 1, then Shift, then release 1: the release's keysym was
/// `exclam`, so no release was sent, 1 stayed down in the engine and its next
/// press was taken for a repeat and swallowed.
#[test]
fn a_key_released_after_shift_changes_releases_what_was_pressed() {
    let mut c = core();
    c.host_key(&key(true, 10, 0x31, 0), false);
    c.host_key(&key(true, 50, 0xffe1, 0), false);
    c.host_key(&key(false, 10, 0x31, SHIFT), false);
    c.host_key(&key(false, 50, 0xffe1, SHIFT), false);
    assert_eq!(keys(&mut c), vec![(true, 2, 0), (true, 42, 1), (false, 2, 1), (false, 42, 0)]);
    assert!(c.held_keys().is_empty(), "nothing is left held");
    // And the next press of 1 is a press, not a repeat.
    c.host_key(&key(true, 10, 0x31, 0), false);
    assert_eq!(keys(&mut c), vec![(true, 2, 0)]);
}

#[test]
fn tab_then_shift_and_punctuation_then_shift_release_cleanly() {
    let mut c = core();
    for (kc, keysym0) in [(23u32, 0xff09u64), (61, 0x2f), (48, 0x27), (20, 0x2d)] {
        c.host_key(&key(true, kc, keysym0, 0), false);
        c.host_key(&key(false, kc, keysym0, SHIFT), false);
    }
    let k = keys(&mut c);
    assert_eq!(k.iter().filter(|k| k.0).count(), 4);
    assert_eq!(k.iter().filter(|k| !k.0).count(), 4);
    assert!(c.held_keys().is_empty());
}

#[test]
fn auto_repeat_presses_reach_the_engine_once_and_release_once() {
    let mut c = core();
    for _ in 0..5 {
        c.host_key(&key(true, 25, 0x77, 0), false);
    }
    c.host_key(&key(false, 25, 0x77, 0), false);
    assert_eq!(keys(&mut c), vec![(true, 17, 0), (false, 17, 0)]);
}

// ---------------------------------------------------- text-box withholding

#[test]
fn a_focused_box_withholds_character_keys_and_forwards_the_rest() {
    let mut c = core();
    c.host_key(&key(true, 25, 0x77, 0), true); // w
    c.host_key(&key(true, 61, 0x2f, 0), true); // slash
    c.host_key(&key(true, 51, 0x23, 0), true); // backslash position (# on de)
    c.host_key(&key(true, 94, 0x3c, 0), true); // 102nd
    c.host_key(&key(true, 79, 0xff95, 0), true); // KP_7 position
    assert_eq!(keys(&mut c), vec![], "character keys are typed, not played");
    c.host_key(&key(true, 36, 0xff0d, 0), true); // Return
    c.host_key(&key(true, 37, 0xffe3, 0), true); // Control_L
    c.host_key(&key(true, 9, 0xff1b, 0), true); // Escape
    c.host_key(&key(true, 22, 0xff08, 0), true); // BackSpace
    c.host_key(&key(true, 111, 0xff52, 0), true); // Up
    assert_eq!(keys(&mut c), vec![(true, 28, 0), (true, 29, 0x1000), (true, 1, 0), (true, 14, 0), (true, 103, 0)]);
}

#[test]
fn a_release_follows_its_press_across_a_box_focus_change() {
    // "/" opens chat: pressed with no box (forwarded), the engine focuses
    // the chat box, and the release must still reach it.
    let mut c = core();
    c.host_key(&key(true, 61, 0x2f, 0), false);
    c.host_key(&key(false, 61, 0x2f, 0), true);
    assert_eq!(keys(&mut c), vec![(true, 53, 0), (false, 53, 0)]);
    // Pressed while typing (withheld), released still typing: withheld.
    c.host_key(&key(true, 25, 0x77, 0), true);
    c.host_key(&key(false, 25, 0x77, 0), true);
    assert_eq!(keys(&mut c), vec![]);
    // Pressed while typing, the box closes, released: still withheld, the
    // engine never saw the press.
    c.host_key(&key(true, 25, 0x77, 0), true);
    c.host_key(&key(false, 25, 0x77, 0), false);
    assert_eq!(keys(&mut c), vec![]);
}

#[test]
fn devctl_keys_record_once_and_release_once() {
    let mut c = core();
    c.plain_key(true, 17, 0, false);
    c.plain_key(true, 17, 0, false);
    assert_eq!(c.held_keys().len(), 1, "two presses, one record");
    c.plain_key(false, 17, 0, false);
    assert_eq!(keys(&mut c), vec![(true, 17, 0), (true, 17, 0), (false, 17, 0)]);
    assert!(c.held_keys().is_empty(), "one release consumed it");
    // The pairing for devctl too: "/" pressed, box focused, released.
    c.plain_key(true, 53, 0, false);
    c.plain_key(false, 53, 0, true);
    assert_eq!(keys(&mut c), vec![(true, 53, 0), (false, 53, 0)]);
    // A tap while typing: withheld both ways.
    c.plain_key(true, 30, 0, true);
    c.plain_key(false, 30, 0, true);
    assert_eq!(keys(&mut c), vec![]);
}

#[test]
fn the_switches_override_the_withholding_and_the_key_native() {
    let mut c = Core::new(Config { keys_while_typing: true, ..cfg() });
    c.host_key(&key(true, 25, 0x77, 0), true);
    assert_eq!(keys(&mut c), vec![(true, 17, 0)]);
    let mut c = Core::new(Config { pass_key: false, ..cfg() });
    c.host_key(&key(true, 25, 0x77, 0), false);
    assert_eq!(keys(&mut c), vec![]);
    assert_eq!(c.held_keys(), vec![(51, 25)], "still held, for the keepalive");
}

#[test]
fn the_character_key_set_is_the_spec_list_plus_backslash_102nd_and_keypad() {
    let spec: Vec<i32> = (2..=13).chain(16..=27).chain(30..=41).chain(44..=53).chain([57]).collect();
    for code in &spec {
        assert!(is_character_key(*code), "{code}");
    }
    for code in [43, 86, 55, 71, 79, 82, 83, 98] {
        assert!(is_character_key(code), "{code}");
    }
    for code in [1, 14, 15, 28, 29, 42, 54, 56, 58, 59, 87, 96, 97, 100, 103, 105, 125] {
        assert!(!is_character_key(code), "{code}");
    }
}

// ------------------------------------------------------------- focus: 9.6

#[test]
fn a_focus_loss_releases_every_held_key_once_by_its_physical_code() {
    let mut c = Core::new(Config { agdk_key: true, ..cfg() });
    c.host_key(&key(true, 25, 0x77, 0), false); // W
    c.host_key(&key(true, 50, 0xffe1, 0), false); // Shift
    c.host_key(&key(true, 10, 0x31, SHIFT), false); // 1, under Shift
    c.plain_key(true, 32, 0, false); // devctl D
    c.take_calls();
    c.release_all_keys(99);
    let got = calls(&mut c);
    let k: Vec<(bool, i32, i32)> = got
        .iter()
        .filter_map(|c| match *c {
            Call::Key { down, code, meta } => Some((down, code, meta)),
            _ => None,
        })
        .collect();
    assert_eq!(k, vec![(false, 17, 0), (false, 42, 0), (false, 2, 0), (false, 32, 0)]);
    let agdk: Vec<i32> = got
        .iter()
        .filter_map(|c| match *c {
            Call::AgdkKey { down: false, code, meta: 0, .. } => Some(code),
            _ => None,
        })
        .collect();
    assert_eq!(agdk, vec![51, 59, 8], "AGDK hears the releases of what it heard pressed");
    assert!(c.held_keys().is_empty());
    c.release_all_keys(100);
    assert_eq!(calls(&mut c), vec![], "nothing is released twice");
}

#[test]
fn an_ungrab_releases_only_the_keys_that_came_up() {
    let mut c = core();
    c.host_key(&key(true, 25, 0x77, 0), false);
    c.host_key(&key(true, 50, 0xffe1, 0), false);
    c.take_calls();
    assert_eq!(c.held_keys(), vec![(51, 25), (59, 50)]);
    c.release_x_keycodes(&[50], 5);
    assert_eq!(keys(&mut c), vec![(false, 42, 0)]);
    assert_eq!(c.held_keys(), vec![(51, 25)]);
}

#[test]
fn the_engine_is_told_focus_changes_once_each() {
    let mut c = core();
    c.window_focus(true, true);
    assert_eq!(calls(&mut c), vec![], "it starts out told it has focus");
    c.window_focus(false, true);
    c.window_focus(false, true);
    c.window_focus(true, true);
    assert_eq!(calls(&mut c), vec![Call::WindowFocus(false), Call::WindowFocus(true)]);
    assert_eq!(c.focus.observed, Some(true));
    // Observed but not told, with the report switched off.
    let mut c = core();
    c.window_focus(false, false);
    assert_eq!(calls(&mut c), vec![]);
    assert_eq!(c.focus.observed, Some(false));
}

// ------------------------------------------------------------------- meta

#[test]
fn x_modifiers_map_to_the_four_meta_bits() {
    assert_eq!(android_meta_state(0), 0);
    assert_eq!(android_meta_state(SHIFT), META_SHIFT_ON);
    assert_eq!(android_meta_state(LOCK), META_CAPS_LOCK_ON);
    assert_eq!(android_meta_state(CTRL), META_CTRL_ON);
    assert_eq!(android_meta_state(ALT), META_ALT_ON);
    assert_eq!(android_meta_state(SHIFT | CTRL | ALT | LOCK), 0x10_1003);
    // Num Lock (Mod2), Super (Mod4) and AltGr (Mod5) set nothing.
    assert_eq!(android_meta_state(0x10 | 0x40 | 0x80), 0);
    // The self-bit, for each modifier key; Caps Lock is not corrected.
    assert_eq!(meta_for_key(0, 0xffe1, true), META_SHIFT_ON);
    assert_eq!(meta_for_key(SHIFT, 0xffe2, false), 0);
    assert_eq!(meta_for_key(0, 0xffe4, true), META_CTRL_ON);
    assert_eq!(meta_for_key(CTRL, 0xffe3, false), 0);
    assert_eq!(meta_for_key(0, 0xffe9, true), META_ALT_ON);
    assert_eq!(meta_for_key(ALT, 0xffea, false), 0);
    assert_eq!(meta_for_key(0, 0xffe5, true), 0);
    assert_eq!(meta_for_key(LOCK, 0xffe5, false), META_CAPS_LOCK_ON);
}

// ------------------------------------------------------------ the tables

/// All 108 entries of the spec's keysym table.
#[test]
fn the_keysym_table_has_the_spec_s_108_entries() {
    let mut expected: Vec<(u64, i32)> = Vec::new();
    for i in 0..10 {
        expected.push((0x30 + i, 7 + i as i32));
    }
    for i in 0..26 {
        expected.push((0x61 + i, 29 + i as i32));
        expected.push((0x41 + i, 29 + i as i32));
    }
    for i in 0..12 {
        expected.push((0xffbe + i, 131 + i as i32));
    }
    expected.extend([
        (0x20, 62),
        (0xff0d, 66),
        (0xff8d, 66),
        (0xff08, 67),
        (0xff09, 61),
        (0xff1b, 111),
        (0xff51, 21),
        (0xff52, 19),
        (0xff53, 22),
        (0xff54, 20),
        (0xffe1, 59),
        (0xffe2, 60),
        (0xffe3, 113),
        (0xffe4, 114),
        (0xffe9, 57),
        (0xffea, 58),
        (0xffe5, 115),
        (0xffff, 112),
        (0xff50, 122),
        (0xff57, 123),
        (0xff55, 92),
        (0xff56, 93),
        (0xff63, 124),
        (0x2c, 55),
        (0x2e, 56),
        (0x2f, 76),
        (0x3b, 74),
        (0x27, 75),
        (0x60, 68),
        (0x2d, 69),
        (0x3d, 70),
        (0x5b, 71),
        (0x5d, 72),
        (0x5c, 73),
    ]);
    assert_eq!(expected.len(), 108);
    for &(keysym, code) in &expected {
        assert_eq!(keysym_to_android(keysym), Some(code), "keysym {keysym:#x}");
    }
    // And nothing else in the ranges a keyboard produces.
    let mapped = (0..=0xffffu64).filter(|k| keysym_to_android(*k).is_some()).count();
    assert_eq!(mapped, 108);
    for k in [0x21u64, 0x40, 0x7e, 0xfe20, 0xffb1, 0xffeb, 0xfc, 0x06c1] {
        assert_eq!(keysym_to_android(k), None, "{k:#x}");
    }
}

#[test]
fn the_agdk_keycode_follows_the_layout_and_falls_back_to_position() {
    // QWERTZ: the key at evdev 21 (Y on US) prints z.
    assert_eq!(android_keycode(0x7a, 21), 54);
    // u-umlaut has no keycode; Android's German layout reports LEFT_BRACKET.
    assert_eq!(android_keycode(0xfc, 26), 71);
    assert_eq!(android_keycode(0xff9c, 79), 145, "keypad 1 by position");
    assert_eq!(android_keycode(0xffeb, 125), 117, "Super_L is META_LEFT");
    assert_eq!(android_keycode(0, 250), 0, "unknown");
    // Position table spot checks against Generic.kl.
    assert_eq!(evdev_to_android(1), Some(111));
    assert_eq!(evdev_to_android(2), Some(8));
    assert_eq!(evdev_to_android(11), Some(7));
    assert_eq!(evdev_to_android(17), Some(51));
    assert_eq!(evdev_to_android(57), Some(62));
    assert_eq!(evdev_to_android(96), Some(160));
    assert_eq!(evdev_to_android(103), Some(19));
}

#[test]
fn ascii_letters_digits_and_space_have_evdev_codes() {
    let letters = "abcdefghijklmnopqrstuvwxyz";
    let codes = [30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44];
    for (c, code) in letters.chars().zip(codes) {
        assert_eq!(ascii_to_evdev(c), Some(code), "{c}");
    }
    for (i, c) in "123456789".chars().enumerate() {
        assert_eq!(ascii_to_evdev(c), Some(2 + i as i32));
    }
    assert_eq!(ascii_to_evdev('0'), Some(11));
    assert_eq!(ascii_to_evdev(' '), Some(57));
    for c in ['A', '!', '/', 'é'] {
        assert_eq!(ascii_to_evdev(c), None);
    }
}

#[test]
fn keysyms_name_their_characters() {
    assert_eq!(keysym_to_char(0x61), Some('a'));
    assert_eq!(keysym_to_char(0xe9), Some('é'));
    assert_eq!(keysym_to_char(0xdf), Some('ß'));
    assert_eq!(keysym_to_char(0x0100_20ac), Some('€'));
    assert_eq!(keysym_to_char(0x06c1), Some('а'), "Cyrillic through libxkbcommon");
    assert_eq!(keysym_to_char(0), None);
    assert_eq!(keysym_to_char(0xff08).filter(|c| !c.is_control()), None);
}

// ------------------------------------------------------- mouse, wheel

#[test]
fn buttons_map_to_the_engine_s_indices_and_side_buttons_reach_agdk_only() {
    assert_eq!(button_index(BUTTON_PRIMARY), Some(0));
    assert_eq!(button_index(BUTTON_SECONDARY), Some(1));
    assert_eq!(button_index(BUTTON_TERTIARY), Some(2));
    assert_eq!(button_index(BUTTON_BACK), None);
    assert_eq!(button_index(BUTTON_FORWARD), None);
    let mut c = core();
    c.button(&[(ACTION_DOWN, BUTTON_BACK, 0), (ACTION_BUTTON_PRESS, BUTTON_BACK, BUTTON_BACK)], 1.0, 2.0, true, BUTTON_BACK, 3, 3);
    let got = calls(&mut c);
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|c| matches!(c, Call::Motion { .. })));
}

#[test]
fn unlocked_deltas_follow_the_spec_vectors() {
    let mut c = Core::new(Config { agdk_mouse: false, ..cfg() });
    c.unlocked_motion(100.0, 50.0, 0, 0, 1);
    c.unlocked_motion(103.0, 47.0, 0, 0, 2);
    c.unlocked_motion(103.0, 47.0, 0, 0, 3);
    c.reset_delta();
    c.unlocked_motion(900.0, 47.0, 0, 0, 4);
    let d: Vec<(f32, f32)> = calls(&mut c)
        .into_iter()
        .filter_map(|c| match c {
            Call::MouseMove { dx, dy, .. } => Some((dx, dy)),
            _ => None,
        })
        .collect();
    assert_eq!(d, vec![(0.0, 0.0), (3.0, -3.0), (0.0, 0.0), (0.0, 0.0)]);
}

#[test]
fn unlocked_motion_sends_agdk_first_then_the_move() {
    let mut c = core();
    c.unlocked_motion(10.0, 20.0, BUTTON_PRIMARY, 4, 5);
    assert_eq!(
        calls(&mut c),
        vec![
            Call::Motion { action: ACTION_MOVE, x: 10.0, y: 20.0, buttons: 1, action_button: 0, time: 5, down_time: 4 },
            Call::MouseMove { x: 10.0, y: 20.0, dx: 0.0, dy: 0.0 },
        ]
    );
}

#[test]
fn the_wheel_sends_detents_and_horizontal_reaches_agdk_only() {
    let mut c = core();
    c.wheel(10.0, 20.0, 0.0, 1.0, 7);
    assert_eq!(
        calls(&mut c),
        vec![Call::Scroll { x: 10.0, y: 20.0, h: 0.0, v: 1.0, time: 7 }, Call::MouseWheel { x: 10.0, y: 20.0, delta: 1.0 }]
    );
    c.wheel(10.0, 20.0, -1.0, 0.0, 8);
    assert_eq!(calls(&mut c), vec![Call::Scroll { x: 10.0, y: 20.0, h: -1.0, v: 0.0, time: 8 }]);
    let mut c = Core::new(Config { wheel_scale: -2.0, ..cfg() });
    c.wheel(0.0, 0.0, 1.0, -1.0, 9);
    assert_eq!(
        calls(&mut c),
        vec![Call::Scroll { x: 0.0, y: 0.0, h: -2.0, v: 2.0, time: 9 }, Call::MouseWheel { x: 0.0, y: 0.0, delta: 2.0 }]
    );
    assert_eq!(parse_wheel_scale("1.5"), Some(1.5));
    assert_eq!(parse_wheel_scale(" -1 "), Some(-1.0));
    for bad in ["0", "nan", "inf", "x", ""] {
        assert_eq!(parse_wheel_scale(bad), None, "{bad:?}");
    }
}

#[test]
fn the_keepalive_runs_while_a_key_is_held_unlocked_or_locked() {
    let mut c = Core::new(Config { agdk_mouse: false, ..cfg() });
    c.keepalive();
    assert_eq!(calls(&mut c), vec![], "no key held");
    c.unlocked_motion(5.0, 6.0, 0, 0, 1);
    c.host_key(&key(true, 25, 0x77, 0), false);
    c.take_calls();
    c.keepalive();
    assert_eq!(calls(&mut c), vec![Call::MouseMove { x: 5.0, y: 6.0, dx: 0.0, dy: 0.0 }]);
    // Locked: the last position is gone, the anchor stands in.
    c.reset_delta();
    c.set_lock_anchor(Some((640, 360)));
    c.keepalive();
    assert_eq!(calls(&mut c), vec![Call::MouseMove { x: 640.0, y: 360.0, dx: 0.0, dy: 0.0 }]);
    c.set_lock_anchor(None);
    c.keepalive();
    assert_eq!(calls(&mut c), vec![], "released and nothing reported since");
}

#[test]
fn the_throttle_policy_decides_the_keepalive() {
    assert_eq!(parse_throttle(None), ThrottleWhen::Visible);
    assert_eq!(parse_throttle(Some("off")), ThrottleWhen::Off);
    assert_eq!(parse_throttle(Some("unfocused")), ThrottleWhen::Unfocused);
    assert_eq!(parse_throttle(Some("bogus")), ThrottleWhen::Visible);
    for f in [None, Some(true), Some(false)] {
        for v in [None, Some(true), Some(false)] {
            assert!(keepalive_wanted(ThrottleWhen::Off, f, v));
            assert_eq!(keepalive_wanted(ThrottleWhen::Unfocused, f, v), f != Some(false));
            assert_eq!(keepalive_wanted(ThrottleWhen::Visible, f, v), !(v == Some(false) && f != Some(true)));
        }
    }
}

#[test]
fn the_touchscreen_declaration_resolves_in_order() {
    assert_eq!(touchscreen_reported(true, Some("1"), true).0, false);
    assert_eq!(touchscreen_reported(false, Some("1"), false).0, true);
    for off in ["0", "false", "NO", "Off"] {
        assert_eq!(touchscreen_reported(false, Some(off), true).0, false, "{off}");
    }
    assert_eq!(touchscreen_reported(false, Some("  "), true), (true, "the display backend"));
    assert_eq!(touchscreen_reported(false, None, false), (false, "the display backend"));
}

#[test]
fn a_missing_native_prints_at_one_and_each_power_of_ten_with_its_own_count() {
    let a = AtomicU64::new(0);
    let b = AtomicU64::new(0);
    let printed = (0..25).filter(|_| report_unregistered("test native a", &a)).count();
    assert_eq!(printed, 2, "at 1 and at 10");
    assert!(report_unregistered("test native b", &b), "another native starts its own count");
    assert!(first_or_power_of_ten(1) && first_or_power_of_ten(100) && first_or_power_of_ten(1000));
    assert!(!first_or_power_of_ten(0) && !first_or_power_of_ten(2) && !first_or_power_of_ten(99));
}

// -------------------------------------------------------------- text field

fn field(text: &str, caret: usize) -> TextField {
    let mut f = TextField::default();
    f.seed(1, text, Some(caret));
    f
}

#[test]
fn the_field_vectors_hold() {
    let mut f = field("hello", 2);
    assert!(f.apply(Edit::SelectAll));
    assert_eq!(f.selected_text().as_deref(), Some("hello"));
    assert!(!f.apply(Edit::SelectAll), "again is no change");
    assert!(f.apply(Edit::Insert("é")));
    assert_eq!((f.text(), f.caret, f.selection()), ("é".to_string(), 1, None));

    assert!(!field("", 0).apply(Edit::SelectAll));

    let mut f = field("abcdef", 1);
    f.apply(Edit::Extend(Caret::Right));
    f.apply(Edit::Extend(Caret::Right));
    assert_eq!(f.selected_text().as_deref(), Some("bc"));
    assert!(f.apply(Edit::Backspace));
    assert_eq!((f.text().as_str(), f.caret), ("adef", 1));

    let mut f = field("abcdef", 4);
    f.apply(Edit::Extend(Caret::Left));
    assert!(f.apply(Edit::Delete));
    assert_eq!((f.text().as_str(), f.caret), ("abcef", 3));

    let mut f = field("hello world", 5);
    f.apply(Edit::Extend(Caret::End));
    assert_eq!(f.selected_text().as_deref(), Some(" world"));
    f.apply(Edit::Extend(Caret::Home));
    assert_eq!(f.selected_text().as_deref(), Some("hello"));
    let s = f.state();
    assert_eq!((s.sel_start, s.sel_end), (5, 0), "anchor then caret, unsorted");

    let mut f = field("abc", 1);
    f.apply(Edit::Extend(Caret::Right));
    f.apply(Edit::Extend(Caret::Left));
    assert_eq!(f.selection(), None);
    assert_eq!((f.state().sel_start, f.state().sel_end), (1, 1));

    let mut f = field("abcdef", 2);
    f.apply(Edit::Extend(Caret::Right));
    f.apply(Edit::Extend(Caret::Right));
    assert!(f.apply(Edit::Move(Caret::Left)));
    assert_eq!((f.caret, f.selection()), (2, None));
    let mut f = field("abcdef", 4);
    f.apply(Edit::Extend(Caret::Left));
    f.apply(Edit::Move(Caret::Right));
    assert_eq!(f.caret, 4);

    let mut f = field("abc", 1);
    assert!(!f.apply(Edit::DeleteSelection));
    f.apply(Edit::SelectAll);
    assert!(f.apply(Edit::DeleteSelection));
    assert_eq!((f.text().as_str(), f.caret), ("", 0));

    let mut f = field("añb€c", 1);
    for _ in 0..3 {
        f.apply(Edit::Extend(Caret::Right));
    }
    assert_eq!(f.selected_text().as_deref(), Some("ñb€"));
    f.apply(Edit::Insert("-"));
    assert_eq!((f.text().as_str(), f.caret), ("a-c", 2));

    let mut f = field("abc", 0);
    f.apply(Edit::SelectAll);
    f.seed(2, "abc", None);
    assert_eq!(f.selection(), None, "a reseed drops the selection");

    let mut f = field("abcd", 3);
    assert_eq!((f.state().sel_start, f.state().sel_end), (3, 3));
    f.apply(Edit::Extend(Caret::Left));
    f.apply(Edit::Extend(Caret::Left));
    assert_eq!((f.caret, f.state().sel_start, f.state().sel_end), (1, 3, 1));

    let mut f = TextField::default();
    f.seed(1, "hello", None);
    assert_eq!(f.caret, 5, "a seed without a caret puts it at the end");
    f.apply(Edit::Move(Caret::Home));
    f.apply(Edit::Insert("say "));
    assert_eq!((f.text().as_str(), f.caret), ("say hello", 4));

    let mut f = field("abc", 0);
    assert!(!f.apply(Edit::Backspace));
    assert!(f.apply(Edit::Delete));
    assert_eq!((f.text().as_str(), f.caret), ("bc", 0));
    f.apply(Edit::Move(Caret::End));
    f.apply(Edit::Backspace);
    assert_eq!(f.text(), "b");

    let mut f = field("héllo", 5);
    f.apply(Edit::Move(Caret::Home));
    f.apply(Edit::Move(Caret::Right));
    f.apply(Edit::Move(Caret::Right));
    assert_eq!(f.caret, 2);
    f.apply(Edit::Insert("X"));
    assert_eq!(f.text(), "héXllo");

    let mut f = field("ab", 2);
    f.apply(Edit::Move(Caret::Home));
    assert!(!f.apply(Edit::Move(Caret::Left)));
    assert!(f.apply(Edit::Move(Caret::Right)));
    f.apply(Edit::Move(Caret::End));
    assert!(!f.apply(Edit::Move(Caret::Right)));

    let mut f = field("ab", 1);
    assert!(!f.apply(Edit::Insert("a\nb")));
    assert!(!f.apply(Edit::Insert("")));
}

#[test]
fn the_revision_counts_changes_and_refills() {
    let mut f = field("ab", 0);
    let r0 = f.revision;
    f.apply(Edit::Move(Caret::Left));
    assert_eq!(f.revision, r0, "no change, no revision");
    f.apply(Edit::Insert("x"));
    assert_eq!(f.revision, r0 + 1);
    f.seed(9, "new", None);
    assert_eq!(f.revision, r0 + 2);
}

#[test]
fn a_new_focus_generation_refills_the_field_and_the_same_one_does_not() {
    let mut c = core();
    c.reseed(4, || ("user".to_string(), None));
    c.edit(Edit::Insert("name"));
    c.reseed(4, || unreachable!("the same generation is not refilled"));
    assert_eq!(c.field.text(), "username");
    // The password box: a new generation, so the username does not carry.
    c.reseed(5, || (String::new(), None));
    assert_eq!(c.field.text(), "");
    // An IME caret beyond the text is clamped.
    c.reseed(6, || ("abc".to_string(), Some(99)));
    assert_eq!(c.field.caret, 3);
}

#[test]
fn the_shortcut_vectors_hold() {
    let m = android_meta_state;
    assert_eq!(text_shortcut(0x61, m(CTRL)), Some(TextShortcut::SelectAll));
    assert_eq!(text_shortcut(0x41, m(CTRL | LOCK)), Some(TextShortcut::SelectAll));
    assert_eq!(text_shortcut(0x63, m(CTRL)), Some(TextShortcut::Copy));
    assert_eq!(text_shortcut(0x78, m(CTRL)), Some(TextShortcut::Cut));
    assert_eq!(text_shortcut(0x76, m(CTRL)), Some(TextShortcut::Paste));
    assert_eq!(text_shortcut(0xff63, m(CTRL)), Some(TextShortcut::Copy));
    assert_eq!(text_shortcut(0xff63, m(SHIFT)), Some(TextShortcut::Paste));
    assert_eq!(text_shortcut(0xffff, m(SHIFT)), Some(TextShortcut::Cut));
    assert_eq!(text_shortcut(0x61, 0), None);
    assert_eq!(text_shortcut(0x56, m(CTRL | SHIFT)), None);
    assert_eq!(text_shortcut(0x63, m(CTRL | ALT)), None);
    assert_eq!(text_shortcut(0xffff, 0), None);
    assert_eq!(text_shortcut(0xff63, 0), None);
    assert_eq!(text_shortcut(0x7a, m(CTRL)), None);
}

#[test]
fn a_key_press_in_a_box_edits_by_keysym_and_types_its_committed_text() {
    let m = android_meta_state;
    assert_eq!(edit_for_key(0xff51, 0, ""), Some(Edit::Move(Caret::Left)));
    assert_eq!(edit_for_key(0xff53, m(SHIFT), ""), Some(Edit::Extend(Caret::Right)));
    assert_eq!(edit_for_key(0xff50, 0, ""), Some(Edit::Move(Caret::Home)));
    assert_eq!(edit_for_key(0xff9c, m(SHIFT), ""), Some(Edit::Extend(Caret::End)), "KP_End");
    assert_eq!(edit_for_key(0xff08, 0, "\u{8}"), Some(Edit::Backspace));
    assert_eq!(edit_for_key(0xffff, 0, ""), Some(Edit::Delete));
    assert_eq!(edit_for_key(0x61, 0, "a"), Some(Edit::Insert("a")));
    // The input method's composed result arrives on keycode 0, keysym 0.
    assert_eq!(edit_for_key(0, 0, "é"), Some(Edit::Insert("é")));
    assert_eq!(edit_for_key(0x40, 0, "@"), Some(Edit::Insert("@")), "AltGr+Q is Mod5, not Alt");
    // Alt+letter types nothing (it used to type the letter).
    assert_eq!(edit_for_key(0x61, m(ALT), "a"), None);
    assert_eq!(edit_for_key(0x7a, m(CTRL), "\u{1a}"), None);
    // Up, Down, Tab, Return, Escape make no edit.
    for k in [0xff52u64, 0xff54, 0xff09, 0xff0d, 0xff1b] {
        assert_eq!(edit_for_key(k, 0, ""), None, "{k:#x}");
    }
}

#[test]
fn field_text_is_redacted_unless_asked() {
    assert_eq!(redact("hunter2", false), "<7 bytes, 7 chars>");
    assert_eq!(redact("é", false), "<2 bytes, 1 chars>");
    assert_eq!(redact("hunter2", true), "\"hunter2\"");
}

#[test]
fn masked_input_types_are_5_9_and_10() {
    for t in 0..16 {
        assert_eq!(is_masked_input_type(t), matches!(t, 5 | 9 | 10), "{t}");
    }
}

#[test]
fn percentiles_are_nearest_rank() {
    let v: Vec<u32> = (1..=100).collect();
    assert_eq!(latency::percentile(&v, 50.0), 50);
    assert_eq!(latency::percentile(&v, 99.0), 99);
    assert_eq!(latency::percentile(&v, 100.0), 100);
    assert_eq!(latency::percentile(&[7], 99.0), 7);
    assert_eq!(latency::percentile(&[], 50.0), 0);
}
