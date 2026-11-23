use std::ffi::{c_int, c_uint, c_ulong, c_void};

use super::placement::{ascii_title, parse_position, place, Placement, PlacementRequest};
use super::xinput::*;
use super::xlib::*;
use super::*;

#[test]
fn the_keysym_supplies_the_text_and_xlib_bytes_only_rule_out_control_characters() {
    // Cyrillic_a on a Russian layout: no Latin-1 bytes, no modifiers.
    assert_eq!(keysym_text_fallback(&[], 0, 0x06c1), Some('а'));
    // Shift is how capitals are typed and must not suppress them.
    assert_eq!(keysym_text_fallback(&[], 1, 0x06e1), Some('А'));
    // A plain letter: the same character either way.
    assert_eq!(keysym_text_fallback(b"a", 0, 0x0061), Some('a'));
    // The defect: in the C locale Xlib hands back Latin-1 0xE9 for
    // eacute, which is not UTF-8. The keysym's character is used.
    assert_eq!(keysym_text_fallback(&[0xe9], 0, 0x00e9), Some('é'));
    assert_eq!(keysym_text_fallback(&[0xfc], 1, 0x00dc), Some('Ü'));
    // Ctrl+A / Ctrl+C and Alt+letter: the keysym is the letter, but no
    // letter was typed.
    assert_eq!(keysym_text_fallback(&[0x01], 1 << 2, 0x0061), None);
    assert_eq!(keysym_text_fallback(&[], 1 << 2, 0x0063), None);
    assert_eq!(keysym_text_fallback(&[], 1 << 3, 0x06c1), None);
    // A control byte from Xlib means no text even without the mask.
    assert_eq!(keysym_text_fallback(&[0x1b], 0, 0xff1b), None);
    // An editing key has no character to offer.
    assert_eq!(keysym_text_fallback(&[0x08], 0, 0xff08), None);
    assert_eq!(keysym_text_fallback(&[], 0, 0xff08), None);
}

#[test]
fn the_lock_follows_the_engine_a_camera_drag_and_the_override() {
    let focused = Some(true);
    // Nothing asking: no lock.
    assert_eq!(pointer_lock_decision(false, 0, false, false, focused), Some(false));
    // The engine alone.
    assert_eq!(pointer_lock_decision(true, 0, false, false, focused), Some(true));
    // A camera drag, right or middle, unless drag locks are off.
    assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, false, false, focused), Some(true));
    assert_eq!(pointer_lock_decision(false, BUTTON_TERTIARY, false, false, focused), Some(true));
    assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, true, false, focused), Some(false));
    // The left button is a click, not a camera drag.
    assert_eq!(pointer_lock_decision(false, BUTTON_PRIMARY, false, false, focused), Some(false));
    // The override alone.
    assert_eq!(pointer_lock_decision(false, 0, false, true, focused), Some(true));
}

#[test]
fn nothing_second_guesses_the_engine_while_focused() {
    // The Escape latch is gone: with the engine asking, the answer is
    // yes on every pump, with no state carried between them that could
    // make it no. (The old signature took a `previously_suppressed`
    // flag; there is nothing left to pass.)
    for _ in 0..3 {
        assert_eq!(pointer_lock_decision(true, 0, false, false, Some(true)), Some(true));
    }
}

#[test]
fn an_unfocused_window_decides_nothing_and_an_unknown_one_still_does() {
    // After a real FocusOut the lock was released; the gate is what
    // stops the next pump taking it straight back while the user is
    // typing in another window.
    assert_eq!(pointer_lock_decision(true, 0, false, false, Some(false)), None);
    assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, false, true, Some(false)), None);
    // No focus event seen yet must not freeze the lock forever.
    assert_eq!(pointer_lock_decision(true, 0, false, false, None), Some(true));
}

#[test]
fn only_real_focus_changes_count_and_grabs_do_not() {
    const NOTIFY_GRAB: c_int = 1;
    const NOTIFY_ANCESTOR: c_int = 0;
    const NOTIFY_NONLINEAR: c_int = 3;
    const NOTIFY_POINTER: c_int = 5;
    // Alt-tab, a click on another window, i3 moving focus.
    assert!(focus_change_is_real(NOTIFY_NORMAL, NOTIFY_NONLINEAR));
    assert!(focus_change_is_real(NOTIFY_NORMAL, NOTIFY_ANCESTOR));
    assert!(focus_change_is_real(NOTIFY_NORMAL, NOTIFY_POINTER));
    // A focus change that happened while a keyboard grab was active is
    // still a focus change.
    assert!(focus_change_is_real(NOTIFY_WHILE_GRABBED, NOTIFY_NONLINEAR));
    // A media key or a window-manager binding activating and releasing a
    // passive key grab: the review's "media keys jerk the camera".
    assert!(!focus_change_is_real(NOTIFY_GRAB, NOTIFY_NONLINEAR));
    assert!(!focus_change_is_real(NOTIFY_UNGRAB, NOTIFY_NONLINEAR));
    // Focus moving into a child of this window never leaves it.
    assert!(!focus_change_is_real(NOTIFY_NORMAL, NOTIFY_INFERIOR));
}

#[test]
fn a_focus_loss_releases_each_held_button_and_nothing_else() {
    assert_eq!(held_button_bits(0), Vec::<i32>::new());
    assert_eq!(held_button_bits(BUTTON_SECONDARY), vec![BUTTON_SECONDARY]);
    assert_eq!(
        held_button_bits(BUTTON_TERTIARY | BUTTON_PRIMARY | BUTTON_FORWARD),
        vec![BUTTON_PRIMARY, BUTTON_TERTIARY, BUTTON_FORWARD],
        "one release per held button, in a fixed order"
    );
}

#[test]
fn a_grab_that_swallowed_a_release_is_reconciled_against_the_server() {
    const BUTTON1: c_uint = 1 << 8;
    const BUTTON3: c_uint = 1 << 10;
    // Right still physically down after a media key: left alone, the
    // drag goes on.
    assert_eq!(buttons_released_elsewhere(BUTTON_SECONDARY, BUTTON3), Vec::<i32>::new());
    // Right let go during the grab: released here, since the real
    // release went to the grabbing client.
    assert_eq!(buttons_released_elsewhere(BUTTON_SECONDARY, 0), vec![BUTTON_SECONDARY]);
    // X's middle is Button2Mask and Android's TERTIARY; the masks must
    // not be crossed with right's.
    assert_eq!(buttons_released_elsewhere(BUTTON_TERTIARY, BUTTON3), vec![BUTTON_TERTIARY]);
    // Unheld buttons are never released, whatever the server says.
    assert_eq!(buttons_released_elsewhere(0, 0), Vec::<i32>::new());
    // Side buttons have no core mask and are not released on a guess.
    assert_eq!(buttons_released_elsewhere(BUTTON_BACK | BUTTON_PRIMARY, BUTTON1), Vec::<i32>::new());

    // W (X keycode 25) still held, Shift_L (50) released during the grab.
    let mut keymap = [0u8; 32];
    keymap[25 / 8] |= 1 << (25 % 8);
    let held = [(51, 25), (59, 50)];
    assert_eq!(keys_released_elsewhere(&held, &keymap), vec![(59, 50)]);
    keymap[50 / 8] |= 1 << (50 % 8);
    assert_eq!(keys_released_elsewhere(&held, &keymap), Vec::<(i32, i32)>::new());
}

#[test]
fn a_modifier_keys_own_event_carries_its_own_bit() {
    use super::super::input::{keysym_to_android, meta_for_key, META_CTRL_ON, META_SHIFT_ON};
    const SHIFT_MASK: c_uint = 1;
    // X reports the state from *before* the event: Shift's press has no
    // ShiftMask, its release still has it. The correction is the bit
    // belonging to the key in hand, set on down and cleared on up.
    assert_eq!(meta_for_key(0, 0xffe1, true), META_SHIFT_ON, "Shift_L down");
    assert_eq!(meta_for_key(SHIFT_MASK, 0xffe1, false), 0, "Shift_L up");
    assert_eq!(meta_for_key(0, 0xffe2, true), META_SHIFT_ON, "Shift_R down");
    assert_eq!(meta_for_key(0, 0xffe3, true), META_CTRL_ON, "Control_L down");
    // An ordinary key is untouched: W with Shift held keeps Shift.
    assert_eq!(meta_for_key(SHIFT_MASK, 0x77, true), META_SHIFT_ON);
    assert_eq!(meta_for_key(0, 0x77, false), 0);
    // Shift_L is Android's KEYCODE_SHIFT_LEFT, which is what the engine
    // reads for shift lock.
    assert_eq!(keysym_to_android(0xffe1), Some(59));
}

#[test]
fn raw_valuators_are_unpacked_by_mask_not_by_index() {
    // Both axes moved: two packed values each.
    let both = [0b11u8];
    assert_eq!(
        raw_motion_axes(&both, &[3.0, -2.0], &[1.5, -1.0]),
        ((3.0, -2.0), (1.5, -1.0))
    );
    // Purely vertical: bit 1 alone, and its value is at index 0. An
    // unpacked read would send this to the horizontal axis.
    let y_only = [0b10u8];
    assert_eq!(raw_motion_axes(&y_only, &[4.0], &[2.0]), ((0.0, 4.0), (0.0, 2.0)));
    // A third valuator (a wheel as an axis) is skipped, not read as X.
    let with_wheel = [0b101u8];
    assert_eq!(raw_motion_axes(&with_wheel, &[1.0, 9.0], &[0.5, 9.0]), ((1.0, 0.0), (0.5, 0.0)));
    // Nothing set, or arrays shorter than the mask claims: zeros, no
    // panic.
    assert_eq!(raw_motion_axes(&[0u8, 0], &[], &[]), ((0.0, 0.0), (0.0, 0.0)));
    assert_eq!(raw_motion_axes(&both, &[1.0], &[]), ((1.0, 0.0), (0.0, 0.0)));
}

#[test]
fn the_acceleration_setting_picks_the_pair() {
    // The drain's sum is `input::PendingMotion`, tested there with the
    // same vectors this test used to carry.
    let acc = (6.0, -3.0);
    let raw = (2.0, -1.0);
    assert_eq!(choose_camera_delta(true, acc, raw), acc);
    assert_eq!(choose_camera_delta(false, acc, raw), raw);
}

#[test]
fn a_focus_loss_holding_w_and_the_right_button_releases_both() {
    use super::super::input::{Call, Config, Core, HostKey};
    // The gesture rules choose the AGDK half; the core sends it and the
    // button index, and releases the key by its physical code.
    let mut core = Core::new(Config {
        agdk_mouse: true,
        agdk_key: false,
        pass_key: true,
        keys_while_typing: false,
        wheel_scale: 1.0,
    });
    core.host_key(&HostKey { down: true, x_keycode: 25, state: 0, keysym0: 0x77, unicode: 'w' as i32, time: 1 }, false);
    let pressed = ButtonStep::new(0, BUTTON_SECONDARY, true).unwrap();
    core.button(&pressed.agdk_actions(), 300.0, 200.0, true, BUTTON_SECONDARY, 2, 2);
    core.take_calls();

    // What `lose_focus` does, in its order: each held button through the
    // gesture rules at the last position, then every key.
    for b in held_button_bits(pressed.buttons) {
        let step = ButtonStep::new(pressed.buttons, b, false).unwrap();
        core.button(&step.agdk_actions(), 300.0, 200.0, false, b, 3, 2);
    }
    core.release_all_keys(3);
    let calls: Vec<Call> = core.take_calls().into_iter().map(|e| e.call).collect();
    assert_eq!(
        calls,
        vec![
            Call::Motion { action: ACTION_BUTTON_RELEASE, x: 300.0, y: 200.0, buttons: 0, action_button: BUTTON_SECONDARY, time: 3, down_time: 2 },
            Call::Motion { action: ACTION_UP, x: 300.0, y: 200.0, buttons: 0, action_button: 0, time: 3, down_time: 2 },
            Call::MouseButton { x: 300.0, y: 200.0, down: false, index: 1 },
            Call::Key { down: false, code: 17, meta: 0 },
        ]
    );
}

#[test]
fn a_raw_sample_delivered_twice_under_the_grab_is_counted_once() {
    // The measured shape: same time, device, source and values.
    let a: RawSampleKey = (114255876, 2, 4, 7.0, -3.0);
    assert!(!is_duplicate_raw(None, a), "the first sample of a lock is taken");
    assert!(is_duplicate_raw(Some(a), a), "its second delivery is dropped");
    // The next real sample, even with the same values, has a new time.
    let b: RawSampleKey = (114255931, 2, 4, 7.0, -3.0);
    assert!(!is_duplicate_raw(Some(a), b));
    // Same millisecond, different movement: two real samples.
    let c: RawSampleKey = (114255876, 2, 4, 6.0, -3.0);
    assert!(!is_duplicate_raw(Some(a), c));
    // Same millisecond and values from another device: also real.
    let d: RawSampleKey = (114255876, 2, 9, 7.0, -3.0);
    assert!(!is_duplicate_raw(Some(a), d));
}

#[test]
fn a_drag_holds_where_the_button_went_down_and_the_engine_gets_the_centre() {
    let size = (1280, 720);
    // Raw path, camera drag: the cursor stays where the player pressed.
    assert_eq!(lock_anchor(false, true, Some((100, 200)), size), (100, 200));
    // Clamped into the window if the press was reported just outside it.
    assert_eq!(lock_anchor(false, true, Some((-5, 900)), size), (0, 719));
    // The engine's own request is a *centred* lock on either path.
    assert_eq!(lock_anchor(true, true, Some((100, 200)), size), (640, 360));
    // The warp fallback measures from where it warps to and always needs
    // the centre.
    assert_eq!(lock_anchor(false, false, Some((100, 200)), size), (640, 360));
    // No pointer position: the centre.
    assert_eq!(lock_anchor(false, true, None, size), (640, 360));
}

#[test]
fn only_the_edge_band_triggers_a_raw_path_recentre() {
    let size = (1280, 720);
    assert!(!near_edge((640, 360), size));
    assert!(!near_edge((RAW_EDGE_MARGIN, RAW_EDGE_MARGIN), size));
    assert!(near_edge((RAW_EDGE_MARGIN - 1, 360), size));
    assert!(near_edge((640, 719), size));
    assert!(near_edge((1280 - RAW_EDGE_MARGIN, 360), size));
    assert!(!near_edge((1280 - RAW_EDGE_MARGIN - 1, 720 - RAW_EDGE_MARGIN - 1), size));
}

#[test]
fn the_warps_own_echo_is_not_motion() {
    // The MotionNotify this backend's own XWarpPointer produces lands on
    // the anchor and is not motion: it only moves the reference point.
    let mut m = WarpPathMotion::engaged((640, 360));
    assert_eq!(m.motion((645, 358)), (5, -2));
    assert_eq!(m.end_of_drain((640, 360), true), (Some((5, -2)), true));
    assert_eq!(m.motion((640, 360)), (0, 0), "the echo");
    assert_eq!(m.end_of_drain((640, 360), true), (None, false), "nothing owed, nothing to warp");
    // With no warp outstanding, a move that happens to end on the anchor
    // is a move like any other.
    assert_eq!(m.motion((650, 360)), (10, 0));
    assert_eq!(m.motion((640, 360)), (-10, 0));
}

/// Feed one drain's core events through the warp path and end it the way
/// `flush_locked_motion` does, returning what the camera was handed.
fn drain(m: &mut WarpPathMotion, events: &[(i32, i32)]) -> (i32, i32) {
    for &pos in events {
        m.motion(pos);
    }
    m.end_of_drain((640, 360), true).0.unwrap_or((0, 0))
}

#[test]
fn a_backlog_of_motion_is_counted_once_not_once_per_event() {
    // After a hitch, or at a high frame rate with a fast mouse, a drain
    // reads dozens of MotionNotify generated before the last warp-back
    // landed, 3 px apart along the mouse's real path. Measured from the
    // anchor, as this path used to, each counts the earlier part of the
    // flick again: 5490 px for these 60, less whatever the old echo wait
    // happened to discard. Measured from the previous event, 180.
    let mut m = WarpPathMotion::engaged((640, 360));
    let backlog: Vec<(i32, i32)> = (1..=60).map(|i| (640 + 3 * i, 360)).collect();
    assert_eq!(drain(&mut m, &backlog), (180, 0));
    // The drain ended off the anchor, so one warp is now expected; its
    // echo comes after more motion the server had already queued.
    assert_eq!(m.expected.len(), 1);
    let late: Vec<(i32, i32)> = (61..=70).map(|i| (640 + 3 * i, 360)).collect();
    assert_eq!(drain(&mut m, &late), (30, 0), "pre-warp stragglers still count from the last");
    assert_eq!(m.expected.len(), 1, "no second warp to an anchor already expected");
    assert_eq!(drain(&mut m, &[(640, 360), (644, 358)]), (4, -2), "after the echo, from the anchor");
}

#[test]
fn a_backlog_out_and_back_cancels_out() {
    let mut m = WarpPathMotion::engaged((640, 360));
    let mut events: Vec<(i32, i32)> = (1..=30).map(|i| (640 + 3 * i, 360)).collect();
    events.extend((0..30).rev().map(|i| (640 + 3 * i, 359)));
    assert_eq!(drain(&mut m, &events), (0, -1));
    // Back on the anchor's column but a pixel above it, so a warp is due.
    assert_eq!(m.expected, vec![((640, 360), 0)]);
}

#[test]
fn a_recentre_mid_flight_rebases_at_each_echo_in_turn() {
    // A drag anchor at (900, 200), the warp back to it in flight, and then
    // the engine asks for a centred lock: two warps, two targets, echoes
    // arriving in the order the warps were issued.
    let mut m = WarpPathMotion::engaged((900, 200));
    m.motion((910, 200));
    m.end_of_drain((900, 200), true);
    m.warped_to((640, 360));
    assert_eq!(m.motion((912, 201)), (2, 1), "still the pre-warp path");
    assert_eq!(m.motion((900, 200)), (0, 0), "first echo");
    assert_eq!(m.motion((640, 360)), (0, 0), "second echo");
    assert!(m.expected.is_empty());
    assert_eq!(m.motion((641, 360)), (1, 0));
}

#[test]
fn a_warp_that_never_echoes_stops_being_expected() {
    // A warp onto the pixel the pointer already occupies produces no
    // MotionNotify. If nothing retired it, `end_of_drain` would never warp
    // to that anchor again and the hidden pointer would pin against the
    // confinement's edge.
    let mut m = WarpPathMotion::engaged((640, 360));
    m.motion((700, 360));
    assert!(m.end_of_drain((640, 360), true).1);
    for _ in 0..WARP_UNSEEN_DRAINS {
        assert!(!m.end_of_drain((640, 360), true).1, "still waiting");
    }
    assert!(m.end_of_drain((640, 360), true).1, "given up on, and warped again");
}

#[test]
fn the_raw_path_never_asks_for_a_drain_warp() {
    let mut m = WarpPathMotion::engaged((640, 360));
    m.motion((700, 360));
    assert_eq!(m.end_of_drain((640, 360), false), (Some((60, 0)), false));
    assert!(m.expected.is_empty());
}

/// Walk a sequence of `(button, press)` through `ButtonStep` from nothing
/// held, the way `dispatch_button_bit` does, collecting the AGDK actions.
fn gesture(steps: &[(i32, bool)]) -> Vec<(i32, i32, i32)> {
    let mut held = 0;
    let mut out = Vec::new();
    for &(button, press) in steps {
        if let Some(step) = ButtonStep::new(held, button, press) {
            held = step.buttons;
            out.extend(step.agdk_actions());
        }
    }
    out
}

#[test]
fn a_second_button_neither_starts_nor_ends_the_gesture() {
    const L: i32 = BUTTON_PRIMARY;
    const R: i32 = BUTTON_SECONDARY;
    // Left held, right pressed and released for a camera turn, left
    // released: one ACTION_DOWN, one ACTION_UP, and the left button is
    // never reported up before it is.
    assert_eq!(
        gesture(&[(L, true), (R, true), (R, false), (L, false)]),
        vec![
            (ACTION_DOWN, L, 0),
            (ACTION_BUTTON_PRESS, L, L),
            (ACTION_BUTTON_PRESS, L | R, R),
            (ACTION_BUTTON_RELEASE, L, R),
            (ACTION_BUTTON_RELEASE, 0, L),
            (ACTION_UP, 0, 0),
        ]
    );
    // The first button let go first: the gesture still ends only with the
    // last one.
    assert_eq!(
        gesture(&[(R, true), (L, true), (R, false), (L, false)]),
        vec![
            (ACTION_DOWN, R, 0),
            (ACTION_BUTTON_PRESS, R, R),
            (ACTION_BUTTON_PRESS, L | R, L),
            (ACTION_BUTTON_RELEASE, L, R),
            (ACTION_BUTTON_RELEASE, 0, L),
            (ACTION_UP, 0, 0),
        ]
    );
    // A lone click is the pair it always was.
    assert_eq!(
        gesture(&[(L, true), (L, false)]),
        vec![(ACTION_DOWN, L, 0), (ACTION_BUTTON_PRESS, L, L), (ACTION_BUTTON_RELEASE, 0, L), (ACTION_UP, 0, 0)]
    );
}

#[test]
fn a_button_that_does_not_change_is_not_a_step() {
    let s = ButtonStep::new(BUTTON_PRIMARY, BUTTON_SECONDARY, true).unwrap();
    assert!(!s.starts_gesture && !s.ends_gesture);
    assert_eq!(ButtonStep::new(0, BUTTON_PRIMARY, false), None, "release of a button never down");
    assert_eq!(ButtonStep::new(BUTTON_PRIMARY, BUTTON_PRIMARY, true), None, "press of one already down");
    // A focus loss lets go of each held button in turn through the same
    // path; only the last of them closes the gesture.
    let held = BUTTON_PRIMARY | BUTTON_SECONDARY | BUTTON_TERTIARY;
    let steps: Vec<_> = held_button_bits(held)
        .into_iter()
        .scan(held, |h, b| {
            let s = ButtonStep::new(*h, b, false).unwrap();
            *h = s.buttons;
            Some(s.ends_gesture)
        })
        .collect();
    assert_eq!(steps, vec![false, false, true]);
}

#[test]
fn only_the_last_expose_of_a_batch_asks_for_a_redraw() {
    assert!(!is_final_expose(3));
    assert!(!is_final_expose(1));
    assert!(is_final_expose(0));
}

#[test]
fn wheel_buttons_are_detents_and_never_clicks() {
    for b in 4..=7 {
        assert!(wheel_detents(b).is_some(), "{b} is the wheel");
        assert_eq!(android_button(b), None, "{b} is not a click");
    }
    assert_eq!(wheel_detents(4), Some((0.0, 1.0)));
    assert_eq!(wheel_detents(5), Some((0.0, -1.0)));
    assert_eq!(wheel_detents(6), Some((-1.0, 0.0)));
    assert_eq!(wheel_detents(7), Some((1.0, 0.0)));
    for b in 1..=3 {
        assert_eq!(wheel_detents(b), None);
    }
    assert_eq!(android_button(1), Some(BUTTON_PRIMARY));
    assert_eq!(android_button(2), Some(BUTTON_TERTIARY));
    assert_eq!(android_button(3), Some(BUTTON_SECONDARY));
    assert_eq!(android_button(8), Some(BUTTON_BACK));
    assert_eq!(android_button(9), Some(BUTTON_FORWARD));
    assert_eq!(android_button(10), None);
}

#[test]
fn the_event_mask_selects_exposure_and_the_input_it_needs() {
    assert_eq!(EVENT_MASK, 0x0022_804F);
    assert_ne!(EVENT_MASK & 0x8000, 0, "Expose is what makes the engine redraw");
    assert_eq!(EVENT_MASK & 0x4F, 0x4F, "keys, buttons and motion");
    // Enter/leave, property and visibility changes are not selected.
    assert_eq!(EVENT_MASK & (0x10 | 0x20 | 0x10000 | 0x400000), 0);
}

#[test]
fn the_window_class_is_the_desktop_files_startup_class() {
    let desktop = include_str!("../../../../../packaging/freebsd/intoxicated.desktop");
    let class = desktop
        .lines()
        .find_map(|l| l.strip_prefix("StartupWMClass="))
        .expect("intoxicated.desktop has StartupWMClass");
    assert_eq!(class.trim(), WM_RES_CLASS);
    assert_eq!(WM_RES_NAME, "intoxicated");
}

#[test]
fn the_title_is_ascii_for_wm_name() {
    assert_eq!(ascii_title("Intoxicated 1.0 (abc)"), "Intoxicated 1.0 (abc)");
    assert_eq!(ascii_title("Intoxicat\u{e9}d"), "Intoxicat-d", "one character, one dash");
}

fn heads(list: &[(i32, i32, i32, i32)]) -> Vec<Head> {
    list.iter().map(|&(x, y, width, height)| Head { x, y, width, height }).collect()
}

fn placed(request: PlacementRequest, w: i32, h: i32, list: Option<&[(i32, i32, i32, i32)]>) -> (Placement, Vec<String>) {
    let mut warnings = Vec::new();
    let p = place(w, h, &request, || list.map(heads).ok_or(()), &mut warnings);
    (p, warnings)
}

fn at(position: &str) -> PlacementRequest {
    PlacementRequest { position: Some(position.into()), ..PlacementRequest::default() }
}

fn on_monitor(n: &str, fullscreen: bool) -> PlacementRequest {
    PlacementRequest { monitor: Some(n.into()), fullscreen, ..PlacementRequest::default() }
}

#[test]
fn placement_without_switches_is_the_origin_at_the_requested_size() {
    let (p, w) = placed(PlacementRequest::default(), 1280, 720, None);
    assert_eq!(p, Placement { x: 0, y: 0, width: 1280, height: 720, fullscreen: false, monitor: None });
    assert!(w.is_empty());
}

#[test]
fn an_explicit_position_wins_and_a_bad_one_is_reported() {
    assert_eq!(placed(at("100,200"), 1280, 720, None).0, Placement { x: 100, y: 200, width: 1280, height: 720, fullscreen: false, monitor: None });
    let p = placed(at(" 5 , -3 "), 1280, 720, None).0;
    assert_eq!((p.x, p.y), (5, -3));
    assert_eq!(parse_position("1,2,3"), Some((1, 2)), "extra parts are ignored");
    let (p, w) = placed(at("x"), 1280, 720, None);
    assert_eq!((p.x, p.y), (0, 0));
    assert_eq!(w, vec!["[android] INTOXICATED_WINDOW_POS=\"x\" is not <x>,<y>; ignoring".to_string()]);
    // A position wins over a monitor, and keeps the fullscreen flag.
    let request = PlacementRequest { position: Some("7,8".into()), monitor: Some("0".into()), fullscreen: true };
    let mut asked = false;
    let p = place(1280, 720, &request, || {
        asked = true;
        Ok(Vec::new())
    }, &mut Vec::new());
    assert_eq!((p.x, p.y, p.fullscreen, p.monitor), (7, 8, true, None));
    assert!(!asked, "nothing else is consulted");
}

#[test]
fn a_monitor_centres_the_window_or_covers_it_when_fullscreen() {
    let one = [(0, 0, 1920, 1080)];
    assert_eq!(placed(on_monitor("0", false), 1280, 720, Some(&one)).0, Placement { x: 320, y: 180, width: 1280, height: 720, fullscreen: false, monitor: Some(0) });
    let two = [(0, 0, 3440, 1440), (3440, 240, 1920, 1200)];
    let p = placed(on_monitor("1", false), 1280, 720, Some(&two)).0;
    assert_eq!((p.x, p.y), (3760, 480));
    let p = placed(on_monitor("1", false), 2000, 1500, Some(&two)).0;
    assert_eq!((p.x, p.y, p.width, p.height), (3440, 240, 2000, 1500), "larger than the head: its corner");
    let p = placed(on_monitor(" 1 ", true), 1280, 720, Some(&two)).0;
    assert_eq!(p, Placement { x: 3440, y: 240, width: 1920, height: 1200, fullscreen: true, monitor: Some(1) });
}

#[test]
fn a_bad_monitor_is_reported_and_out_of_range_uses_the_first_head_throughout() {
    let (p, w) = placed(on_monitor("second", false), 1280, 720, None);
    assert_eq!((p.x, p.y, p.monitor), (0, 0, None));
    assert_eq!(w, vec!["[android] INTOXICATED_MONITOR must be a number; ignoring".to_string()]);
    let (p, w) = placed(on_monitor("0", false), 1280, 720, None);
    assert_eq!((p.x, p.y, p.monitor), (0, 0, None));
    assert_eq!(w, vec!["[android] INTOXICATED_MONITOR needs libXinerama; ignoring".to_string()]);
    let (p, w) = placed(on_monitor("0", false), 1280, 720, Some(&[]));
    assert_eq!((p.x, p.y, p.monitor), (0, 0, None), "no heads: the origin");
    assert!(w.is_empty());
    // Out of range: the first head, and the first head is also the one
    // `_NET_WM_FULLSCREEN_MONITORS` names, not the last.
    let two = [(0, 0, 3440, 1440), (3440, 240, 1920, 1200)];
    let (p, w) = placed(on_monitor("5", true), 1280, 720, Some(&two));
    assert_eq!((p.x, p.y, p.width, p.height, p.monitor), (0, 0, 3440, 1440, Some(0)));
    assert_eq!(w, vec!["[android] INTOXICATED_MONITOR=5 but only 2 monitor(s); using the first".to_string()]);
}

#[test]
fn event_layouts_are_the_lp64_ones() {
    assert_eq!(std::mem::offset_of!(XInputEvent, time), 56);
    assert_eq!(std::mem::offset_of!(XInputEvent, x), 64);
    assert_eq!(std::mem::offset_of!(XInputEvent, state), 80);
    assert_eq!(std::mem::offset_of!(XInputEvent, detail), 84);
    assert_eq!(std::mem::offset_of!(XInputEvent, same_screen), 88);
    assert_eq!(std::mem::size_of::<XSizeHints>(), 80);
    assert_eq!(std::mem::size_of::<XWMHints>(), 56);
    assert_eq!(std::mem::size_of::<XColor>(), 16);
    let mut buf = [0u8; 256];
    buf[56..60].copy_from_slice(&1667i32.to_ne_bytes());
    buf[60..64].copy_from_slice(&1349i32.to_ne_bytes());
    assert_eq!((event_int(&buf, 56), event_int(&buf, 60)), (1667, 1349));
}

/// Defect 9.5: accented letters and dead keys were lost. Against a real
/// display's keymap, with events built in this process: nothing but the
/// (never mapped) window is sent to the server. Run with
/// `DISPLAY=... cargo test --release -p intoxicated-runtime -- --ignored input_method`.
#[test]
#[ignore = "needs an X display"]
fn the_input_method_composes_dead_keys_from_locally_built_events() {
    let xlib = Xlib::load().expect("libX11");
    // SAFETY: plain Xlib calls on the connection opened here.
    let display = unsafe { (xlib.open_display)(std::ptr::null()) };
    assert!(!display.is_null(), "no X display");
    let root = unsafe { (xlib.default_root_window)(display) };
    let window = unsafe { (xlib.create_simple_window)(display, root, 0, 0, 8, 8, 0, 0, 0) };
    let xim = Xim::open(display, window).expect("the local input method");
    // SAFETY: a documented Xlib export with this signature.
    let to_keycode: unsafe extern "C" fn(Display, c_ulong) -> u8 =
        unsafe { std::mem::transmute(dlsym(dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW), c"XKeysymToKeycode".as_ptr())) };
    let press = |keysym: c_ulong, state: c_uint| -> Option<[u8; 256]> {
        let keycode = unsafe { to_keycode(display, keysym) };
        (keycode != 0).then(|| {
            let mut buf = [0u8; 256];
            let ev = XInputEvent {
                type_: KEY_PRESS, serial: 0, send_event: 1, display, window, root, subwindow: 0,
                time: 0, x: 0, y: 0, x_root: 0, y_root: 0, state, detail: keycode as c_uint, same_screen: 1,
            };
            unsafe { std::ptr::write_unaligned(buf.as_mut_ptr() as *mut XInputEvent, ev) };
            buf
        })
    };
    let mut a = press(0x61, 0).expect("an a key");
    assert!(!xim.filter(a.as_mut_ptr().cast()));
    assert_eq!(xim.committed_text(a.as_mut_ptr().cast()), "a");
    let mut shifted = press(0x61, 1).unwrap();
    assert_eq!(xim.committed_text(shifted.as_mut_ptr().cast()), "A");
    let mut ctrl = press(0x61, 4).unwrap();
    assert_eq!(xim.committed_text(ctrl.as_mut_ptr().cast()), "\u{1}", "a control character, which inserts nothing");

    // dead_acute, then e: both taken by the method, "é" put back as a
    // keycode-0 press at the head of this client's queue.
    let (Some(mut dead), Some(mut e)) = (press(0xfe51, 0), press(0x65, 0)) else {
        println!("this keymap has no dead_acute; only the plain keys were checked");
        return;
    };
    assert!(xim.filter(dead.as_mut_ptr().cast()), "the dead key is held back");
    assert!(xim.filter(e.as_mut_ptr().cast()), "and the key that completes it");
    assert!(unsafe { (xlib.events_queued)(display, 0) } > 0, "the result is queued in-process");
    let mut back = [0u8; 256];
    unsafe { (xlib.next_event)(display, back.as_mut_ptr() as *mut c_void) };
    let ev = unsafe { std::ptr::read_unaligned(back.as_ptr() as *const XInputEvent) };
    assert_eq!((ev.type_, ev.detail), (KEY_PRESS, 0));
    assert!(!xim.filter(back.as_mut_ptr().cast()));
    let composed = xim.committed_text(back.as_mut_ptr().cast());
    println!("dead_acute + e -> {composed:?}");
    assert_eq!(composed, "é");
    unsafe { (xlib.destroy_window)(display, window) };
}
