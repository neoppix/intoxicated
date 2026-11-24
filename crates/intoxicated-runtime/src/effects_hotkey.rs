//! The effects hotkey: one key, F9 unless the switch names another, that
//! turns every visual effect off and back on while the game runs.
//!
//! Off unless `INTOXICATED_EFFECTS_HOTKEY` is set: `1` takes F9, and a key
//! name (`F1` to `F10` or `F12`, in either case) takes that key. F11 is not
//! offered, as the window keeps it for fullscreen. Unset, empty or `0` is
//! off, and then the key goes to Roblox untouched.
//!
//! The key is matched in [`intercept`], called from `input::host_key` for
//! every real key the window delivers, after the macros have had it
//! (`macros::intercept`), so it only ever works while the Roblox window has
//! the keyboard. A fresh press, with no Shift, Ctrl or Alt held and no Roblox
//! text box focused, toggles the effects through
//! `android::ssr::toggle_all`; that press, its auto-repeats and its release
//! are kept from the engine, so Roblox does not also open its developer
//! console. While a text box has focus the key goes on to Roblox as any
//! other, and so does a press with a modifier held.
//!
//! What "off" and "back on" mean is `android::ssr`'s: every effect's switch
//! saved and cleared, so nothing is recorded into the frame, then the saved
//! switches put back with the looks they had. A client built without the
//! visual effects has nothing to toggle: the switch is then said once in the
//! log to be ignored, and the key passes through.
//!
//! The matching is covered by the unit tests below; a real F9 in a running
//! game, and Roblox's console staying shut, are INFERRED: not tried in game
//! when this was added.

use std::sync::atomic::{AtomicI32, Ordering::Relaxed};
use std::sync::Mutex;

/// The switch.
pub const ENV: &str = "INTOXICATED_EFFECTS_HOTKEY";

/// The key `1` takes: F9, by evdev code.
const F9: i32 = 67;

/// The X modifier-state bits that make a press something other than the
/// plain key: Shift, Control and Mod1 (Alt), as `macros` reads them.
const MOD_MASK: u32 = 1 | 4 | 8;

/// The evdev code of function key `n`, F1 to F12.
fn function_key(n: u32) -> Option<i32> {
    match n {
        1..=10 => Some(58 + n as i32),
        11 => Some(87),
        12 => Some(88),
        _ => None,
    }
}

/// The name of an evdev code [`function_key`] gives.
pub(crate) fn key_name(evdev: i32) -> String {
    match evdev {
        59..=68 => format!("F{}", evdev - 58),
        87 => "F11".into(),
        88 => "F12".into(),
        other => format!("evdev {other}"),
    }
}

/// What the switch's value asks for: `Ok(None)` for off, the key's evdev code,
/// or why the value is not understood.
pub(crate) fn parse_switch(value: Option<&str>) -> Result<Option<i32>, String> {
    let v = match value.map(str::trim) {
        None | Some("" | "0") => return Ok(None),
        Some(v) => v,
    };
    if v == "1" {
        return Ok(Some(F9));
    }
    let n = v.strip_prefix(['F', 'f']).and_then(|n| n.parse::<u32>().ok());
    match n.and_then(function_key) {
        // The window keeps F11 for fullscreen; the engine never sees it.
        Some(87) => Err(format!("{ENV}={v:?}: F11 is the fullscreen key; use 1 for F9, or F1 to F10 or F12")),
        Some(evdev) => Ok(Some(evdev)),
        None => Err(format!("{ENV}={v:?} is not understood; use 1 for F9, or F1 to F10 or F12")),
    }
}

/// What a key event is to the hotkey.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Not ours: on to the game as before.
    Pass,
    /// Ours, kept from the game: an auto-repeat or the release of a press
    /// that toggled.
    Swallow,
    /// A fresh press: toggle the effects, and keep the key from the game.
    Toggle,
}

/// The key the hotkey listens for, and whether it is held and how its press
/// went: `Some(true)` held after a press that toggled (so its repeats and
/// release are swallowed), `Some(false)` held after a press that went to the
/// game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Matcher {
    pub key: Option<i32>,
    held: Option<bool>,
}

impl Matcher {
    pub(crate) const fn new(key: Option<i32>) -> Self {
        Matcher { key, held: None }
    }

    /// One real key event: `x_keycode` and `x_state` as X gave them,
    /// `box_focused` when a Roblox text box has the keyboard.
    pub(crate) fn key_event(&mut self, down: bool, x_keycode: u32, x_state: u32, box_focused: bool) -> Verdict {
        let Some(key) = self.key else { return Verdict::Pass };
        if x_keycode < 8 || x_keycode as i32 - 8 != key {
            return Verdict::Pass;
        }
        if !down {
            return match self.held.take() {
                Some(true) => Verdict::Swallow,
                _ => Verdict::Pass,
            };
        }
        // Detectable auto-repeat sends bare presses while the key is held:
        // each goes where the first press went.
        if let Some(toggled) = self.held {
            return if toggled { Verdict::Swallow } else { Verdict::Pass };
        }
        let ours = !box_focused && x_state & MOD_MASK == 0;
        self.held = Some(ours);
        if ours {
            Verdict::Toggle
        } else {
            Verdict::Pass
        }
    }

    /// The window lost or regained the keyboard: a release that went to
    /// another window must not make the next press look like a repeat.
    pub(crate) fn forget_held(&mut self) {
        self.held = None;
    }
}

static MATCHER: Mutex<Matcher> = Mutex::new(Matcher::new(None));

/// The key as an evdev code, or -1 when the hotkey is off; read without the
/// lock on every key event.
static KEY: AtomicI32 = AtomicI32::new(-1);

/// Read the switch once, next to `macros::load`, and say what it does.
pub fn load() {
    let value = std::env::var(ENV).ok();
    let key = match parse_switch(value.as_deref()) {
        Ok(None) => return,
        Ok(Some(key)) => key,
        Err(why) => {
            println!("  effects hotkey: {why}; off");
            return;
        }
    };
    if !crate::android::ssr::AVAILABLE {
        println!("  effects hotkey: {ENV} ignored: built without visual effects; {} passes through to Roblox", key_name(key));
        return;
    }
    MATCHER.lock().unwrap_or_else(|e| e.into_inner()).key = Some(key);
    KEY.store(key, Relaxed);
    println!(
        "  effects hotkey: {} turns the visual effects off and back on in game ({ENV}); Roblox no longer sees it",
        key_name(key)
    );
}

/// The key's name, or `None` when the hotkey is off.
pub fn active_key() -> Option<String> {
    let key = KEY.load(Relaxed);
    (key >= 0).then(|| key_name(key))
}

/// Match a real key event (see the module doc). Returns whether it is kept
/// from the game. Cheap and false when the hotkey is off.
pub fn intercept(down: bool, x_keycode: u32, x_state: u32, box_focused: bool) -> bool {
    let key = KEY.load(Relaxed);
    if key < 0 || x_keycode as i32 - 8 != key {
        return false;
    }
    let verdict = MATCHER.lock().unwrap_or_else(|e| e.into_inner()).key_event(down, x_keycode, x_state, box_focused);
    match verdict {
        Verdict::Pass => false,
        Verdict::Swallow => true,
        Verdict::Toggle => {
            crate::android::ssr::toggle_all(&key_name(key));
            true
        }
    }
}

/// A real focus change of the window.
pub fn focus_changed() {
    if KEY.load(Relaxed) >= 0 {
        MATCHER.lock().unwrap_or_else(|e| e.into_inner()).forget_held();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F9's X keycode.
    const X_F9: u32 = 75;

    #[test]
    fn the_switch_is_off_unless_set_and_1_means_f9() {
        assert_eq!(parse_switch(None), Ok(None));
        assert_eq!(parse_switch(Some("")), Ok(None));
        assert_eq!(parse_switch(Some(" 0 ")), Ok(None));
        assert_eq!(parse_switch(Some("1")), Ok(Some(F9)));
        assert_eq!(parse_switch(Some("F9")), Ok(Some(67)));
        assert_eq!(parse_switch(Some("f1")), Ok(Some(59)));
        assert_eq!(parse_switch(Some("F10")), Ok(Some(68)));
        assert_eq!(parse_switch(Some("F12")), Ok(Some(88)));
        assert!(parse_switch(Some("F11")).unwrap_err().contains("fullscreen"));
        for bad in ["2", "F0", "F13", "yes", "F", "G9"] {
            assert!(parse_switch(Some(bad)).is_err(), "{bad}");
        }
        for evdev in [59, 67, 68, 88] {
            assert_eq!(parse_switch(Some(&key_name(evdev))), Ok(Some(evdev)));
        }
    }

    #[test]
    fn a_fresh_f9_press_toggles_and_its_release_is_swallowed() {
        let mut m = Matcher::new(Some(F9));
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Toggle);
        assert_eq!(m.key_event(false, X_F9, 0, false), Verdict::Swallow);
        // The next press toggles again.
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Toggle);
        assert_eq!(m.key_event(false, X_F9, 0, false), Verdict::Swallow);
    }

    #[test]
    fn an_auto_repeat_is_swallowed_and_does_not_toggle_again() {
        let mut m = Matcher::new(Some(F9));
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Toggle);
        for _ in 0..5 {
            assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Swallow);
        }
        assert_eq!(m.key_event(false, X_F9, 0, false), Verdict::Swallow);
    }

    #[test]
    fn with_a_text_box_focused_f9_goes_to_the_game() {
        let mut m = Matcher::new(Some(F9));
        assert_eq!(m.key_event(true, X_F9, 0, true), Verdict::Pass);
        // Its repeats and release too, even if the box lost focus meanwhile.
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Pass);
        assert_eq!(m.key_event(false, X_F9, 0, false), Verdict::Pass);
        // And the next press with the box gone toggles.
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Toggle);
    }

    #[test]
    fn a_modifier_or_another_key_goes_to_the_game() {
        let mut m = Matcher::new(Some(F9));
        for state in [1, 4, 8, 4 | 1] {
            assert_eq!(m.key_event(true, X_F9, state, false), Verdict::Pass, "state {state}");
            assert_eq!(m.key_event(false, X_F9, state, false), Verdict::Pass);
        }
        // Num Lock (Mod2) is not a modifier that counts.
        assert_eq!(m.key_event(true, X_F9, 16, false), Verdict::Toggle);
        assert_eq!(m.key_event(false, X_F9, 16, false), Verdict::Swallow);
        // F10, W and keycode 0 (composed text) are not F9.
        for other in [76, 25, 0] {
            assert_eq!(m.key_event(true, other, 0, false), Verdict::Pass);
            assert_eq!(m.key_event(false, other, 0, false), Verdict::Pass);
        }
    }

    #[test]
    fn with_the_hotkey_off_every_key_passes_through() {
        let mut m = Matcher::new(None);
        for (down, code) in [(true, X_F9), (true, X_F9), (false, X_F9), (true, 25), (false, 25)] {
            assert_eq!(m.key_event(down, code, 0, false), Verdict::Pass);
        }
        // And the cheap path in front of it says the same.
        assert!(!intercept(true, X_F9, 0, false));
        assert!(!intercept(false, X_F9, 0, false));
        assert_eq!(active_key(), None);
    }

    #[test]
    fn another_key_can_be_the_hotkey() {
        let mut m = Matcher::new(Some(88));
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Pass);
        assert_eq!(m.key_event(false, X_F9, 0, false), Verdict::Pass);
        assert_eq!(m.key_event(true, 96, 0, false), Verdict::Toggle);
        assert_eq!(m.key_event(false, 96, 0, false), Verdict::Swallow);
    }

    #[test]
    fn a_focus_change_forgets_a_held_key() {
        let mut m = Matcher::new(Some(F9));
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Toggle);
        // The release went to another window.
        m.forget_held();
        assert_eq!(m.key_event(true, X_F9, 0, false), Verdict::Toggle);
    }
}
