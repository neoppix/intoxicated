//! Keyboard, mouse, wheel and text input, from the host window to the engine.
//!
//! Written clean-room from a behavioural spec of the engine-facing contract
//! (which natives exist and what they take, the mapping tables, the edge cases
//! and the defects found in the inherited path), the Android `MotionEvent`,
//! `KeyEvent` and GameActivity documentation and the X11, Xlib and XInput2
//! documentation. The module it replaces was not read.
//!
//! **Shape.** The X11 mechanics (reading events, the pointer grab, raw motion,
//! the gesture rules for buttons, the input method) live in `x11_window.rs`. This
//! module turns what it observed into engine calls, in two halves:
//!
//! - [`Core`]: state and rules with no I/O. The held-key table, the locked
//!   motion waiting for delivery, the reference point for unlocked deltas and
//!   the focused text field. Each rule appends the engine calls it wants, in
//!   order, as [`Call`]s, so every ordering promise below is checked by a unit
//!   test rather than by reading the code.
//! - The executor: it makes those calls through `intoxicated-linker-sys`, with
//!   a drop counter per native the engine did not export, and the optional
//!   latency trace (`INTOXICATED_TRACE_INPUT_LATENCY=1`).
//!
//! **One ordering rule covers all input.** Locked camera motion is summed and
//! sent once per drain of the X queue, because a 1000 Hz mouse would otherwise
//! be a thousand JNI calls a second for a delta the engine reads once a frame.
//! Anything else the engine is told (a button, a key, the wheel, a text edit)
//! first sends whatever motion is still waiting, so a flick and the click that
//! ends it arrive as flick then click. [`Core::emit`] is the only way a call
//! leaves the core and it does that flush itself.
//!
//! **Keys are physical.** `nativePassKeyEvent` takes a Linux evdev code, which
//! is the X keycode minus 8 on every X server with the evdev keycodes, and the
//! code reaches the engine whatever the modifiers and the layout make of the
//! key: Shift plus the number row, Shift+Tab, the keypad, Super and the keys a
//! German or French layout prints differently all arrive. The held-key table
//! is keyed by that physical code, so a key released after Shift changed
//! releases exactly the code that was pressed. The layout still decides the
//! text a key types, which goes to the focused text box separately.

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use intoxicated_linker_sys::game_activity as ga;

// ------------------------------------------------------------ Android values

/// `MotionEvent.ACTION_*`.
pub const ACTION_DOWN: i32 = 0;
pub const ACTION_UP: i32 = 1;
pub const ACTION_MOVE: i32 = 2;
pub const ACTION_HOVER_MOVE: i32 = 7;
pub const ACTION_SCROLL: i32 = 8;
pub const ACTION_BUTTON_PRESS: i32 = 11;
pub const ACTION_BUTTON_RELEASE: i32 = 12;

/// `MotionEvent.BUTTON_*`.
pub const BUTTON_PRIMARY: i32 = 1;
pub const BUTTON_SECONDARY: i32 = 2;
pub const BUTTON_TERTIARY: i32 = 4;
pub const BUTTON_BACK: i32 = 8;
pub const BUTTON_FORWARD: i32 = 16;

/// `KeyEvent.META_*`, the four this client reports. The left and right
/// variants, Num Lock and Meta are not set: the engine has only ever been
/// measured with these four (shift lock reads `META_SHIFT_ON`).
pub const META_SHIFT_ON: i32 = 0x1;
pub const META_ALT_ON: i32 = 0x2;
pub const META_CTRL_ON: i32 = 0x1000;
pub const META_CAPS_LOCK_ON: i32 = 0x10_0000;

// X modifier masks, from X.h.
const X_SHIFT_MASK: u32 = 1 << 0;
const X_LOCK_MASK: u32 = 1 << 1;
const X_CONTROL_MASK: u32 = 1 << 2;
const X_MOD1_MASK: u32 = 1 << 3;

const INPUT_CLASS: &str = "com/roblox/engine/jni/NativeInputInterface";

// ----------------------------------------------------------------- switches

/// A switch of ours: on when set to anything but empty or `0`.
fn flag(name: &str) -> bool {
    matches!(std::env::var_os(name), Some(v) if !v.is_empty() && v != "0")
}

/// Every environment switch this module reads, read once.
///
/// `INTOXICATED_TRACE_TEXT` is read by `native/` too, with `getenv != NULL`,
/// so it is read the same way here.
struct Switches {
    agdk_mouse: bool,
    agdk_key: bool,
    pass_key: bool,
    keys_while_typing: bool,
    pass_text_on_key: bool,
    pass_text_flag: bool,
    keyboard_report: bool,
    focus_report: bool,
    wheel_scale: f32,
    trace_mouse: bool,
    trace_wheel: bool,
    trace_text: bool,
    show_text: bool,
    trace_latency: bool,
}

fn switches() -> &'static Switches {
    static S: OnceLock<Switches> = OnceLock::new();
    S.get_or_init(|| {
        let scale = std::env::var("INTOXICATED_WHEEL_SCALE").ok();
        let wheel_scale = match scale.as_deref().map(parse_wheel_scale) {
            None => 1.0,
            Some(Some(s)) => s,
            Some(None) => {
                eprintln!(
                    "[input] INTOXICATED_WHEEL_SCALE={:?} is not a finite non-zero number; using 1",
                    scale.unwrap_or_default()
                );
                1.0
            }
        };
        Switches {
            agdk_mouse: !flag("INTOXICATED_NO_AGDK_TOUCH"),
            agdk_key: flag("INTOXICATED_AGDK_KEY"),
            pass_key: !flag("INTOXICATED_NO_PASS_KEY"),
            keys_while_typing: flag("INTOXICATED_KEYS_TO_GAME_WHILE_TYPING"),
            pass_text_on_key: flag("INTOXICATED_PASS_TEXT_ON_KEY"),
            pass_text_flag: flag("INTOXICATED_PASSTEXT_FLAG"),
            keyboard_report: flag("INTOXICATED_KEYBOARD_REPORT"),
            focus_report: !flag("INTOXICATED_NO_FOCUS_REPORT"),
            wheel_scale,
            trace_mouse: flag("INTOXICATED_TRACE_MOUSE"),
            trace_wheel: flag("INTOXICATED_TRACE_WHEEL"),
            trace_text: std::env::var_os("INTOXICATED_TRACE_TEXT").is_some(),
            show_text: flag("INTOXICATED_TRACE_TEXT_SHOW_PASSWORDS"),
            trace_latency: flag("INTOXICATED_TRACE_INPUT_LATENCY"),
        }
    })
}

/// `INTOXICATED_WHEEL_SCALE`, or `None` when it is not a finite non-zero
/// number. Negative inverts the wheel.
fn parse_wheel_scale(v: &str) -> Option<f32> {
    v.trim().parse::<f32>().ok().filter(|s| s.is_finite() && *s != 0.0)
}

/// `INTOXICATED_TRACE_MOUSE=1`: every pointer call and the lock getter's
/// answer when it changes.
pub fn trace_mouse() -> bool {
    switches().trace_mouse
}

/// `INTOXICATED_TRACE_TEXT=1`: key and text-field activity, redacted.
pub fn trace_text() -> bool {
    switches().trace_text
}

/// `INTOXICATED_KEYBOARD_REPORT=1`: acknowledge each text-box focus with
/// `updateKeyboardSize`. Off: it was a control with no measured benefit.
pub fn keyboard_report_enabled() -> bool {
    switches().keyboard_report
}

/// What the core needs to know of the switches. A value, so tests can build
/// a core with any of them without touching the environment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Config {
    /// AGDK mouse and wheel events alongside the Roblox natives.
    pub agdk_mouse: bool,
    /// AGDK key events alongside `nativePassKeyEvent`.
    pub agdk_key: bool,
    /// `nativePassKeyEvent` at all.
    pub pass_key: bool,
    /// Do not withhold character keys from the game while a box is focused.
    pub keys_while_typing: bool,
    pub wheel_scale: f32,
}

impl Config {
    fn from_switches() -> Self {
        let s = switches();
        Config {
            agdk_mouse: s.agdk_mouse,
            agdk_key: s.agdk_key,
            pass_key: s.pass_key,
            keys_while_typing: s.keys_while_typing,
            wheel_scale: s.wheel_scale,
        }
    }
}

// -------------------------------------------------------------------- calls

/// One call into the engine, as the core decided it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Call {
    /// `nativePassMouseMove(x, y, dx, dy)`.
    MouseMove { x: f32, y: f32, dx: f32, dy: f32 },
    /// `nativePassMouseButton(x, y, down, index)`.
    MouseButton { x: f32, y: f32, down: bool, index: i32 },
    /// `nativePassMouseWheel(x, y, delta)`.
    MouseWheel { x: f32, y: f32, delta: f32 },
    /// `nativePassKeyEvent(down, evdev, meta, false)`.
    Key { down: bool, code: i32, meta: i32 },
    /// One AGDK mouse `MotionEvent` through `onTouchEventNative`.
    Motion { action: i32, x: f32, y: f32, buttons: i32, action_button: i32, time: i64, down_time: i64 },
    /// AGDK `ACTION_SCROLL`, axes in detents.
    Scroll { x: f32, y: f32, h: f32, v: f32, time: i64 },
    /// AGDK `onKeyDownNative` / `onKeyUpNative`.
    AgdkKey { down: bool, code: i32, scan: i32, meta: i32, unicode: i32, time: i64, down_time: i64 },
    /// The focused field's whole state: `onTextInputEventNative`, then
    /// `syncTextboxTextAndCursorPosition2`, then `nativePassText` when that
    /// switch is on, then `onSurfaceRedrawNeededNative` when `redraw`.
    Field { which: i64, text: String, sel_start: i32, sel_end: i32, caret: i32, redraw: bool },
    /// `onSurfaceRedrawNeededNative`.
    Redraw,
    /// `updateKeyboardSize(false, 0, height, width, 0)`.
    KeyboardSize { width: i32, height: i32 },
    /// `onWindowFocusChangedNative`.
    WindowFocus(bool),
}

impl Call {
    /// The latency trace's name for the call.
    fn kind(&self) -> &'static str {
        match self {
            Call::MouseMove { .. } => "move",
            Call::MouseButton { .. } => "button",
            Call::MouseWheel { .. } => "wheel",
            Call::Key { .. } => "key",
            Call::Motion { .. } | Call::Scroll { .. } | Call::AgdkKey { .. } => "agdk",
            Call::Field { .. } => "text",
            Call::Redraw | Call::KeyboardSize { .. } | Call::WindowFocus(_) => "other",
        }
    }
}

/// Where an input entered this process, for the latency trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// Read off the X connection by `XNextEvent`.
    X11,
    /// Read off the devctl socket.
    Devctl,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Origin {
    at: Instant,
    source: Source,
}

/// A call, and when the input that caused it entered the process.
#[derive(Debug, Clone)]
pub(crate) struct Emitted {
    pub call: Call,
    origin: Option<Origin>,
}

// ------------------------------------------------------------ locked motion

/// Locked camera movement read and not yet delivered.
///
/// The raw (XInput2) and warp-path halves are kept apart only so the warp
/// half can be dropped when a lock's first raw sample shows the raw path is in
/// charge; they are delivered as one sum. Doubles, so the fractions a
/// high-resolution mouse reports add up instead of rounding away per event.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct PendingMotion {
    raw: (f64, f64),
    warp: (f64, f64),
    events: u32,
}

impl PendingMotion {
    fn add(&mut self, d: (f64, f64), raw: bool) {
        let half = if raw { &mut self.raw } else { &mut self.warp };
        half.0 += d.0;
        half.1 += d.1;
        self.events += 1;
    }

    fn is_empty(&self) -> bool {
        self.events == 0
    }

    /// The total, leaving it empty. `None` when it sums to nothing, so a
    /// drain that moved out and back wakes nobody.
    fn take(&mut self) -> Option<(f64, f64)> {
        let sum = (self.raw.0 + self.warp.0, self.raw.1 + self.warp.1);
        *self = Self::default();
        (sum != (0.0, 0.0)).then_some(sum)
    }
}

/// What a pending delivery is sent with: the anchor the engine has been told
/// the pointer is at, the gesture state for the AGDK half, and when the oldest
/// of the movements was read.
#[derive(Debug, Clone, Copy, Default)]
struct PendingContext {
    anchor: (i32, i32),
    buttons: i32,
    down_time: i64,
    time: i64,
    origin: Option<Origin>,
    raw: bool,
}

// --------------------------------------------------------------------- keys

/// A key the engine has been told is down (or that went down while a text box
/// withheld it), by physical code.
#[derive(Debug, Clone, Copy, PartialEq)]
struct HeldKey {
    evdev: i32,
    /// The Android keycode the AGDK press carried, 0 when none was sent.
    akey: i32,
    /// Whether `nativePassKeyEvent` heard the press. Its release follows
    /// the press, not the text-box state at release time.
    forwarded: bool,
    /// Whether AGDK heard the press.
    agdk: bool,
    down_time: i64,
}

/// One X `KeyPress` or `KeyRelease`, as `x11_window.rs` read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostKey {
    pub down: bool,
    /// The X keycode, which is the physical key. Below 8 is not a key (the
    /// input method puts composed text back as keycode 0).
    pub x_keycode: u32,
    /// The X modifier state from before the event.
    pub state: u32,
    /// The key's first keysym (group 1, level 1): what the layout prints on
    /// the unshifted key. Picks the AGDK keycode and the modifier self-bit.
    pub keysym0: u64,
    /// The character the key typed, for AGDK's `unicodeChar`; 0 for none.
    pub unicode: i32,
    /// Milliseconds on the window's input clock.
    pub time: i64,
}

/// Physical keys that type a character, by evdev code. While a text box has
/// focus their presses are withheld from the game, so typing "w" into chat
/// does not also walk.
///
/// The letter and number rows, punctuation and space, plus three the spec
/// found missing: BACKSLASH (43), the ISO key beside left Shift (86, `<>|` on
/// a German keyboard) and the keypad's digits and operators.
pub fn is_character_key(evdev: i32) -> bool {
    matches!(evdev, 2..=13 | 16..=27 | 30..=41 | 43..=53 | 55 | 57 | 71..=83 | 86 | 98)
}

/// Whether a key event is kept from `nativePassKeyEvent` because a text box
/// is typing it.
fn withheld(box_focused: bool, evdev: i32, keys_while_typing: bool) -> bool {
    box_focused && !keys_while_typing && is_character_key(evdev)
}

/// The META bits for an X modifier state.
pub fn android_meta_state(x_state: u32) -> i32 {
    let mut m = 0;
    if x_state & X_SHIFT_MASK != 0 {
        m |= META_SHIFT_ON;
    }
    if x_state & X_LOCK_MASK != 0 {
        m |= META_CAPS_LOCK_ON;
    }
    if x_state & X_CONTROL_MASK != 0 {
        m |= META_CTRL_ON;
    }
    if x_state & X_MOD1_MASK != 0 {
        m |= META_ALT_ON;
    }
    m
}

/// The META bit a modifier key's own event has to have corrected: X reports
/// the state from before the event, so Shift's press lacks Shift and its
/// release still has it. Caps Lock is left as X reports it.
fn modifier_self_bit(keysym0: u64) -> i32 {
    match keysym0 {
        0xffe1 | 0xffe2 => META_SHIFT_ON, // Shift_L, Shift_R
        0xffe3 | 0xffe4 => META_CTRL_ON,  // Control_L, Control_R
        0xffe9 | 0xffea => META_ALT_ON,   // Alt_L, Alt_R
        _ => 0,
    }
}

/// The META state for one key event, with the key's own bit corrected.
pub fn meta_for_key(x_state: u32, keysym0: u64, down: bool) -> i32 {
    let m = android_meta_state(x_state);
    let own = modifier_self_bit(keysym0);
    if down {
        m | own
    } else {
        m & !own
    }
}

/// Keysym to Android keycode: the 108 entries of the spec's table.
///
/// Used for the AGDK keycode (from the key's unshifted keysym, so it follows
/// the layout the way Android's own keycodes do) and nowhere as a gate: a
/// keysym outside it no longer stops a key reaching the engine.
pub fn keysym_to_android(keysym: u64) -> Option<i32> {
    let k = keysym;
    Some(match k {
        0x30..=0x39 => 7 + (k - 0x30) as i32,   // 0..9
        0x61..=0x7a => 29 + (k - 0x61) as i32,  // a..z
        0x41..=0x5a => 29 + (k - 0x41) as i32,  // A..Z
        0xffbe..=0xffc9 => 131 + (k - 0xffbe) as i32, // F1..F12
        0x20 => 62,             // space
        0xff0d | 0xff8d => 66,  // Return, KP_Enter
        0xff08 => 67,           // BackSpace -> DEL
        0xff09 => 61,           // Tab
        0xff1b => 111,          // Escape
        0xff51 => 21,           // Left
        0xff52 => 19,           // Up
        0xff53 => 22,           // Right
        0xff54 => 20,           // Down
        0xffe1 => 59,           // Shift_L
        0xffe2 => 60,           // Shift_R
        0xffe3 => 113,          // Control_L
        0xffe4 => 114,          // Control_R
        0xffe9 => 57,           // Alt_L
        0xffea => 58,           // Alt_R
        0xffe5 => 115,          // Caps_Lock
        0xffff => 112,          // Delete -> FORWARD_DEL
        0xff50 => 122,          // Home
        0xff57 => 123,          // End
        0xff55 => 92,           // Prior
        0xff56 => 93,           // Next
        0xff63 => 124,          // Insert
        0x2c => 55,             // comma
        0x2e => 56,             // period
        0x2f => 76,             // slash
        0x3b => 74,             // semicolon
        0x27 => 75,             // apostrophe
        0x60 => 68,             // grave
        0x2d => 69,             // minus
        0x3d => 70,             // equal
        0x5b => 71,             // bracketleft
        0x5d => 72,             // bracketright
        0x5c => 73,             // backslash
        _ => return None,
    })
}

/// Evdev code to Android keycode by position, after AOSP's `Generic.kl`. The
/// AGDK keycode for a key whose unshifted keysym is not in
/// [`keysym_to_android`]: the keypad, Super, a German `ü` (which Android's
/// own German layout also reports as LEFT_BRACKET).
pub fn evdev_to_android(evdev: i32) -> Option<i32> {
    Some(match evdev {
        1 => 111,                       // ESC
        2..=10 => 8 + (evdev - 2),      // 1..9
        11 => 7,                        // 0
        12 => 69,
        13 => 70,
        14 => 67,
        15 => 61,
        16 => 45, 17 => 51, 18 => 33, 19 => 46, 20 => 48, 21 => 53, 22 => 49, 23 => 37, 24 => 43, 25 => 44,
        26 => 71,
        27 => 72,
        28 => 66,
        29 => 113,
        30 => 29, 31 => 47, 32 => 32, 33 => 34, 34 => 35, 35 => 36, 36 => 38, 37 => 39, 38 => 40,
        39 => 74,
        40 => 75,
        41 => 68,
        42 => 59,
        43 => 73,
        44 => 54, 45 => 52, 46 => 31, 47 => 50, 48 => 30, 49 => 42, 50 => 41,
        51 => 55,
        52 => 56,
        53 => 76,
        54 => 60,
        55 => 155,                      // KPASTERISK -> NUMPAD_MULTIPLY
        56 => 57,
        57 => 62,
        58 => 115,
        59..=68 => 131 + (evdev - 59),  // F1..F10
        69 => 143,                      // NUMLOCK
        70 => 116,                      // SCROLLLOCK
        71 => 151, 72 => 152, 73 => 153,
        74 => 156,
        75 => 148, 76 => 149, 77 => 150,
        78 => 157,
        79 => 145, 80 => 146, 81 => 147,
        82 => 144,
        83 => 158,
        86 => 73,                       // 102ND, as Generic.kl
        87 => 141,
        88 => 142,
        96 => 160,                      // KPENTER -> NUMPAD_ENTER
        97 => 114,
        98 => 154,
        99 => 120,                      // SYSRQ
        100 => 58,
        102 => 122,
        103 => 19,
        104 => 92,
        105 => 21,
        106 => 22,
        107 => 123,
        108 => 20,
        109 => 93,
        110 => 124,
        111 => 112,
        113 => 164,                     // MUTE
        114 => 25,
        115 => 24,
        117 => 161,                     // KPEQUAL
        119 => 121,                     // PAUSE -> BREAK
        121 => 159,                     // KPCOMMA
        125 => 117,                     // LEFTMETA
        126 => 118,
        127 => 82,                      // COMPOSE -> MENU
        _ => return None,
    })
}

/// The AGDK keycode for a physical key: by its unshifted keysym where the
/// table has one, else by position, else 0 (`KEYCODE_UNKNOWN`).
pub fn android_keycode(keysym0: u64, evdev: i32) -> i32 {
    keysym_to_android(keysym0).or_else(|| evdev_to_android(evdev)).unwrap_or(0)
}

/// The evdev code for an ASCII character, for devctl's `text`: letters,
/// digits and space. Everything else (capitals, punctuation) has none and
/// gets text only.
pub fn ascii_to_evdev(c: char) -> Option<i32> {
    const LETTERS: [i32; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
    ];
    match c {
        'a'..='z' => Some(LETTERS[(c as u8 - b'a') as usize]),
        '1'..='9' => Some(2 + (c as u8 - b'1') as i32),
        '0' => Some(11),
        ' ' => Some(57),
        _ => None,
    }
}

/// The character a keysym names, or `None`.
///
/// Latin-1 and Unicode keysyms directly; anything else (the legacy Cyrillic,
/// Greek and Latin-2 ranges, keypad digits) through libxkbcommon's
/// `xkb_keysym_to_utf32`, loaded on first use.
pub fn keysym_to_char(keysym: u64) -> Option<char> {
    match keysym {
        0 => None,
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(keysym as u32),
        0x0100_0100..=0x0110_ffff => char::from_u32((keysym - 0x0100_0000) as u32),
        _ => xkb_keysym_to_utf32(keysym).and_then(char::from_u32).filter(|&c| c != '\0'),
    }
}

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

fn xkb_keysym_to_utf32(keysym: u64) -> Option<u32> {
    type F = unsafe extern "C" fn(u32) -> u32;
    static F_: OnceLock<Option<F>> = OnceLock::new();
    let f = F_.get_or_init(|| {
        // SAFETY: a literal soname and a documented export with this
        // signature; the handle is never closed.
        unsafe {
            let lib = dlopen(c"libxkbcommon.so.0".as_ptr(), 2 /* RTLD_NOW */);
            if lib.is_null() {
                return None;
            }
            let p = dlsym(lib, c"xkb_keysym_to_utf32".as_ptr());
            (!p.is_null()).then(|| std::mem::transmute::<*mut c_void, F>(p))
        }
    });
    let k = u32::try_from(keysym).ok()?;
    // SAFETY: a pure function of its argument.
    let c = unsafe { (f.as_ref()?)(k) };
    (c != 0).then_some(c)
}

// --------------------------------------------------------------- text field

/// Where a caret move or a selection extension goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caret {
    Left,
    Right,
    Home,
    End,
}

/// One edit of the focused field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit<'a> {
    /// Replace the selection (if any) with this text. Refused when empty or
    /// when it holds a control character.
    Insert(&'a str),
    Backspace,
    Delete,
    /// Move the caret; with a selection, collapse it.
    Move(Caret),
    /// Move the caret and extend the selection (Shift + arrow).
    Extend(Caret),
    SelectAll,
    DeleteSelection,
}

/// The field as the engine is told it: the whole text, and the selection as
/// (anchor, caret) unsorted, or (caret, caret) with none. Positions are in
/// characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldState {
    pub text: String,
    pub sel_start: i32,
    pub sel_end: i32,
    pub caret: i32,
}

/// The editor this client is for a focused text box: on X11 there is no
/// widget over the engine's surface, so this keeps the text, caret and
/// selection and the engine is told the whole state after every change.
#[derive(Debug, Clone, Default)]
pub(crate) struct TextField {
    chars: Vec<char>,
    caret: usize,
    anchor: Option<usize>,
    revision: u64,
    /// The focus generation the field was last filled for.
    seeded: Option<u32>,
}

impl TextField {
    /// Refill for a new focus. Drops the selection and counts as a change.
    fn seed(&mut self, generation: u32, text: &str, caret: Option<usize>) {
        self.chars = text.chars().collect();
        self.caret = caret.map_or(self.chars.len(), |c| c.min(self.chars.len()));
        self.anchor = None;
        self.revision += 1;
        self.seeded = Some(generation);
    }

    fn selection(&self) -> Option<(usize, usize)> {
        self.anchor
            .filter(|&a| a != self.caret)
            .map(|a| (a.min(self.caret), a.max(self.caret)))
    }

    fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection() else {
            return false;
        };
        self.chars.drain(s..e);
        self.caret = s;
        self.anchor = None;
        true
    }

    fn target(&self, from: usize, to: Caret) -> usize {
        match to {
            Caret::Left => from.saturating_sub(1),
            Caret::Right => (from + 1).min(self.chars.len()),
            Caret::Home => 0,
            Caret::End => self.chars.len(),
        }
    }

    /// Apply one edit. True when something changed.
    fn apply(&mut self, edit: Edit<'_>) -> bool {
        let changed = match edit {
            Edit::Insert(s) => {
                if s.is_empty() || s.chars().any(char::is_control) {
                    false
                } else {
                    self.delete_selection();
                    let new: Vec<char> = s.chars().collect();
                    let n = new.len();
                    self.chars.splice(self.caret..self.caret, new);
                    self.caret += n;
                    self.anchor = None;
                    true
                }
            }
            Edit::Backspace => {
                if self.delete_selection() {
                    true
                } else if self.caret == 0 {
                    false
                } else {
                    self.caret -= 1;
                    self.chars.remove(self.caret);
                    self.anchor = None;
                    true
                }
            }
            Edit::Delete => {
                if self.delete_selection() {
                    true
                } else if self.caret >= self.chars.len() {
                    false
                } else {
                    self.chars.remove(self.caret);
                    self.anchor = None;
                    true
                }
            }
            Edit::Move(to) => match self.selection() {
                Some((s, e)) => {
                    self.caret = match to {
                        Caret::Left => s,
                        Caret::Right => e,
                        Caret::Home => 0,
                        Caret::End => self.chars.len(),
                    };
                    self.anchor = None;
                    true
                }
                None => {
                    let t = self.target(self.caret, to);
                    self.anchor = None;
                    let moved = t != self.caret;
                    self.caret = t;
                    moved
                }
            },
            Edit::Extend(to) => {
                let anchor = self.anchor.unwrap_or(self.caret);
                let t = self.target(self.caret, to);
                let moved = t != self.caret;
                self.caret = t;
                self.anchor = (t != anchor).then_some(anchor);
                moved
            }
            Edit::SelectAll => {
                let len = self.chars.len();
                if len == 0 || (self.anchor == Some(0) && self.caret == len) {
                    false
                } else {
                    self.anchor = Some(0);
                    self.caret = len;
                    true
                }
            }
            Edit::DeleteSelection => self.delete_selection(),
        };
        if changed {
            self.revision += 1;
        }
        changed
    }

    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| self.chars[s..e].iter().collect())
    }

    fn state(&self) -> FieldState {
        let caret = self.caret as i32;
        let start = match self.selection() {
            Some(_) => self.anchor.unwrap_or(self.caret) as i32,
            None => caret,
        };
        FieldState { text: self.text(), sel_start: start, sel_end: caret, caret }
    }
}

/// Ctrl+A/C/X/V and the Insert/Delete forms, for a focused box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextShortcut {
    SelectAll,
    Copy,
    Cut,
    Paste,
}

/// Which shortcut a key press is, matched by keysym so it follows the layout
/// and ignores Caps Lock. Nothing with Alt, nothing with Ctrl+Shift.
pub fn text_shortcut(keysym: u64, meta: i32) -> Option<TextShortcut> {
    let ctrl = meta & META_CTRL_ON != 0;
    let shift = meta & META_SHIFT_ON != 0;
    if meta & META_ALT_ON != 0 {
        return None;
    }
    let k = if (0x41..=0x5a).contains(&keysym) { keysym + 0x20 } else { keysym };
    const INSERT: u64 = 0xff63;
    const KP_INSERT: u64 = 0xff9e;
    const DELETE: u64 = 0xffff;
    const KP_DELETE: u64 = 0xff9f;
    match (ctrl, shift, k) {
        (true, false, 0x61) => Some(TextShortcut::SelectAll),
        (true, false, 0x63) => Some(TextShortcut::Copy),
        (true, false, 0x78) => Some(TextShortcut::Cut),
        (true, false, 0x76) => Some(TextShortcut::Paste),
        (true, false, INSERT | KP_INSERT) => Some(TextShortcut::Copy),
        (false, true, INSERT | KP_INSERT) => Some(TextShortcut::Paste),
        (false, true, DELETE | KP_DELETE) => Some(TextShortcut::Cut),
        _ => None,
    }
}

/// The edit a key press makes in a focused box, or `None`. `typed` is the
/// text the key committed (the input method's, or the keysym's character).
/// Caret keys first, then BackSpace and Delete, then the text. Ctrl or Alt
/// held types nothing: Ctrl+letter is a shortcut or nothing, and Alt+letter
/// typing the letter was a defect.
pub fn edit_for_key(keysym: u64, meta: i32, typed: &str) -> Option<Edit<'_>> {
    let caret = match keysym {
        0xff51 | 0xff96 => Some(Caret::Left),  // Left, KP_Left
        0xff53 | 0xff98 => Some(Caret::Right), // Right, KP_Right
        0xff50 | 0xff95 => Some(Caret::Home),  // Home, KP_Home
        0xff57 | 0xff9c => Some(Caret::End),   // End, KP_End
        _ => None,
    };
    if let Some(to) = caret {
        return Some(if meta & META_SHIFT_ON != 0 { Edit::Extend(to) } else { Edit::Move(to) });
    }
    match keysym {
        0xff08 => Some(Edit::Backspace),
        0xffff | 0xff9f => Some(Edit::Delete),
        _ if meta & (META_CTRL_ON | META_ALT_ON) != 0 || typed.is_empty() => None,
        _ => Some(Edit::Insert(typed)),
    }
}

/// `textInputType` values that mask their text (password boxes).
pub fn is_masked_input_type(t: i32) -> bool {
    matches!(t, 5 | 9 | 10)
}

/// A field's text as trace lines and devctl print it: a size, unless
/// `INTOXICATED_TRACE_TEXT_SHOW_PASSWORDS=1`.
pub fn redacted(text: &str) -> String {
    redact(text, switches().show_text)
}

fn redact(text: &str, show: bool) -> String {
    if show {
        format!("{text:?}")
    } else {
        format!("<{} bytes, {} chars>", text.len(), text.chars().count())
    }
}

// --------------------------------------------------------------------- core

/// Whether the engine has been told the window has focus, and what the window
/// last observed. The engine starts out told "focused" (`start` sends it).
#[derive(Debug, Clone, Copy)]
struct FocusReport {
    reported: bool,
    observed: Option<bool>,
}

impl FocusReport {
    /// Record an observation; true when the engine needs telling.
    fn observe(&mut self, focused: bool) -> bool {
        self.observed = Some(focused);
        if self.reported == focused {
            return false;
        }
        self.reported = focused;
        true
    }
}

/// All input state, and the rules. See the module doc.
pub(crate) struct Core {
    cfg: Config,
    /// When the input being handled entered the process; stamped on every
    /// call made for it.
    origin: Option<Origin>,
    out: Vec<Emitted>,
    pending: PendingMotion,
    pending_ctx: PendingContext,
    /// The last absolute position `nativePassMouseMove` reported unlocked:
    /// the reference for the next delta.
    last_pos: Option<(f32, f32)>,
    /// While the pointer is locked, the anchor the engine is told.
    lock_anchor: Option<(f32, f32)>,
    /// Locked deliveries, and how many of them carried raw motion.
    deliveries: u64,
    raw_deliveries: u64,
    keys: Vec<HeldKey>,
    field: TextField,
    focus: FocusReport,
    /// The focus generation `updateKeyboardSize` was last sent for.
    keyboard_reported: Option<u32>,
}

/// The Roblox button index for an Android button bit: primary 0, secondary 1,
/// tertiary 2. Back and forward have no established index and are not sent.
pub fn button_index(bit: i32) -> Option<i32> {
    match bit {
        BUTTON_PRIMARY => Some(0),
        BUTTON_SECONDARY => Some(1),
        BUTTON_TERTIARY => Some(2),
        _ => None,
    }
}

impl Core {
    pub(crate) fn new(cfg: Config) -> Self {
        Core {
            cfg,
            origin: None,
            out: Vec::new(),
            pending: PendingMotion::default(),
            pending_ctx: PendingContext::default(),
            last_pos: None,
            lock_anchor: None,
            deliveries: 0,
            raw_deliveries: 0,
            keys: Vec::new(),
            field: TextField::default(),
            focus: FocusReport { reported: true, observed: None },
            keyboard_reported: None,
        }
    }

    /// The calls decided so far, leaving none.
    pub(crate) fn take_calls(&mut self) -> Vec<Emitted> {
        std::mem::take(&mut self.out)
    }

    /// Queue one call. **Waiting locked motion goes first**: this is the
    /// ordering rule in the module doc, and the only way a call is queued.
    fn emit(&mut self, call: Call) {
        self.flush_motion();
        self.out.push(Emitted { call, origin: self.origin });
    }

    // ---- motion

    /// Locked movement read at `anchor`, raw (XInput2) or by the warp path.
    pub(crate) fn add_locked_motion(&mut self, anchor: (i32, i32), d: (f64, f64), raw: bool, buttons: i32, down_time: i64, time: i64) {
        if !self.pending.is_empty() && self.pending_ctx.anchor != anchor {
            // A recentre between two movements: what was read at the old
            // anchor is delivered there.
            self.flush_motion();
        }
        if self.pending.is_empty() {
            self.pending_ctx.origin = self.origin;
            self.pending_ctx.raw = false;
        }
        self.pending_ctx.anchor = anchor;
        self.pending_ctx.buttons = buttons;
        self.pending_ctx.down_time = down_time;
        self.pending_ctx.time = time;
        self.pending_ctx.raw |= raw;
        self.pending.add(d, raw);
    }

    /// Forget the warp-path half of what is waiting: the lock's first raw
    /// sample has shown the raw path measures this movement, and core motion
    /// read the warp way before it would count it a second time.
    pub(crate) fn drop_pending_warp(&mut self) {
        self.pending.warp = (0.0, 0.0);
    }

    /// Deliver waiting locked motion, if it sums to anything: one AGDK move
    /// at the anchor and one `nativePassMouseMove(anchor, dx, dy)`.
    pub(crate) fn flush_motion(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let ctx = self.pending_ctx;
        let Some((dx, dy)) = self.pending.take() else {
            return;
        };
        let (x, y) = (ctx.anchor.0 as f32, ctx.anchor.1 as f32);
        if self.cfg.agdk_mouse {
            let action = if ctx.buttons != 0 { ACTION_MOVE } else { ACTION_HOVER_MOVE };
            self.out.push(Emitted {
                call: Call::Motion { action, x, y, buttons: ctx.buttons, action_button: 0, time: ctx.time, down_time: ctx.down_time },
                origin: ctx.origin,
            });
        }
        self.out.push(Emitted { call: Call::MouseMove { x, y, dx: dx as f32, dy: dy as f32 }, origin: ctx.origin });
        self.deliveries += 1;
        if ctx.raw {
            self.raw_deliveries += 1;
        }
    }

    /// Drop waiting locked motion undelivered: the lock it belonged to is gone.
    pub(crate) fn discard_motion(&mut self) {
        self.pending = PendingMotion::default();
    }

    /// The delta from the last reported position, which becomes `(x, y)`.
    fn delta_to(&mut self, x: f32, y: f32) -> (f32, f32) {
        let d = match self.last_pos {
            Some((lx, ly)) => (x - lx, y - ly),
            None => (0.0, 0.0),
        };
        self.last_pos = Some((x, y));
        d
    }

    /// Forget the reference point; the next move reports a zero delta.
    pub(crate) fn reset_delta(&mut self) {
        self.last_pos = None;
    }

    pub(crate) fn set_lock_anchor(&mut self, anchor: Option<(i32, i32)>) {
        self.lock_anchor = anchor.map(|(x, y)| (x as f32, y as f32));
    }

    /// One unlocked core motion event.
    pub(crate) fn unlocked_motion(&mut self, x: f32, y: f32, buttons: i32, down_time: i64, time: i64) {
        if self.cfg.agdk_mouse {
            let action = if buttons != 0 { ACTION_MOVE } else { ACTION_HOVER_MOVE };
            self.emit(Call::Motion { action, x, y, buttons, action_button: 0, time, down_time });
        }
        let (dx, dy) = self.delta_to(x, y);
        self.emit(Call::MouseMove { x, y, dx, dy });
    }

    /// A move with no AGDK half: devctl `move`, the scripts.
    pub(crate) fn plain_move(&mut self, x: f32, y: f32) {
        let (dx, dy) = self.delta_to(x, y);
        self.emit(Call::MouseMove { x, y, dx, dy });
    }

    /// One accepted button edge. `actions` are the AGDK `(action,
    /// buttonState, actionButton)` triples the gesture rules chose.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn button(&mut self, actions: &[(i32, i32, i32)], x: f32, y: f32, press: bool, bit: i32, time: i64, down_time: i64) {
        if self.cfg.agdk_mouse {
            for &(action, buttons, action_button) in actions {
                self.emit(Call::Motion { action, x, y, buttons, action_button, time, down_time });
            }
        }
        self.plain_button(x, y, press, bit);
    }

    /// `nativePassMouseButton` alone (devctl `click`, `down`, `up`).
    pub(crate) fn plain_button(&mut self, x: f32, y: f32, press: bool, bit: i32) {
        match button_index(bit) {
            Some(index) => self.emit(Call::MouseButton { x, y, down: press, index }),
            // Nothing for the engine, but the order still holds for what
            // follows.
            None => self.flush_motion(),
        }
    }

    /// One wheel detent (or a devctl scroll), axes positive away from the
    /// user and to the right. Horizontal reaches AGDK only.
    pub(crate) fn wheel(&mut self, x: f32, y: f32, h: f32, v: f32, time: i64) {
        let (h, v) = (h * self.cfg.wheel_scale, v * self.cfg.wheel_scale);
        if self.cfg.agdk_mouse {
            self.emit(Call::Scroll { x, y, h, v, time });
        }
        if v != 0.0 {
            self.emit(Call::MouseWheel { x, y, delta: v });
        } else {
            self.flush_motion();
        }
    }

    /// The idle-throttle keepalive: a zero move where the engine already
    /// believes the pointer is, while any key is held. While locked that is
    /// the anchor, so holding W in first person keeps the engine awake too.
    pub(crate) fn keepalive(&mut self) {
        if self.keys.is_empty() {
            return;
        }
        if let Some((x, y)) = self.last_pos.or(self.lock_anchor) {
            self.emit(Call::MouseMove { x, y, dx: 0.0, dy: 0.0 });
        }
    }

    // ---- keys

    /// One X key event. See [`HostKey`].
    pub(crate) fn host_key(&mut self, k: &HostKey, box_focused: bool) {
        if !(8..=255).contains(&k.x_keycode) {
            return;
        }
        let evdev = (k.x_keycode - 8) as i32;
        let meta = meta_for_key(k.state, k.keysym0, k.down);
        if k.down {
            if self.keys.iter().any(|h| h.evdev == evdev) {
                // Auto-repeat (detectable auto-repeat sends bare presses), or
                // a key devctl already holds: the engine was told once.
                return;
            }
            let forwarded = self.cfg.pass_key && !withheld(box_focused, evdev, self.cfg.keys_while_typing);
            let akey = android_keycode(k.keysym0, evdev);
            if self.cfg.agdk_key {
                self.emit(Call::AgdkKey { down: true, code: akey, scan: evdev, meta, unicode: k.unicode, time: k.time, down_time: k.time });
            }
            if forwarded {
                self.emit(Call::Key { down: true, code: evdev, meta });
            } else {
                self.flush_motion();
            }
            self.keys.push(HeldKey { evdev, akey, forwarded, agdk: self.cfg.agdk_key, down_time: k.time });
        } else {
            match self.keys.iter().position(|h| h.evdev == evdev) {
                Some(i) => {
                    let h = self.keys.remove(i);
                    self.release(&h, meta, k.unicode, k.time);
                }
                None => {
                    // Pressed before this window had the keyboard. A release
                    // the engine never saw a press for is harmless.
                    let forwarded = self.cfg.pass_key && !withheld(box_focused, evdev, self.cfg.keys_while_typing);
                    if self.cfg.agdk_key {
                        let akey = android_keycode(k.keysym0, evdev);
                        self.emit(Call::AgdkKey { down: false, code: akey, scan: evdev, meta, unicode: k.unicode, time: k.time, down_time: k.time });
                    }
                    if forwarded {
                        self.emit(Call::Key { down: false, code: evdev, meta });
                    }
                }
            }
        }
    }

    fn release(&mut self, h: &HeldKey, meta: i32, unicode: i32, time: i64) {
        if h.agdk {
            self.emit(Call::AgdkKey { down: false, code: h.akey, scan: h.evdev, meta, unicode, time, down_time: h.down_time });
        }
        if h.forwarded {
            self.emit(Call::Key { down: false, code: h.evdev, meta });
        } else {
            self.flush_motion();
        }
    }

    /// devctl `key` and the scripts: an evdev code and META bits, through the
    /// text-box rule and the held table. A second press of a held key is sent
    /// again but recorded once, so it releases once.
    pub(crate) fn plain_key(&mut self, down: bool, evdev: i32, meta: i32, box_focused: bool) {
        if down {
            let forwarded = self.cfg.pass_key && !withheld(box_focused, evdev, self.cfg.keys_while_typing);
            if forwarded {
                self.emit(Call::Key { down: true, code: evdev, meta });
            } else {
                self.flush_motion();
            }
            match self.keys.iter_mut().find(|h| h.evdev == evdev) {
                Some(h) => h.forwarded |= forwarded,
                None => self.keys.push(HeldKey { evdev, akey: 0, forwarded, agdk: false, down_time: 0 }),
            }
        } else {
            match self.keys.iter().position(|h| h.evdev == evdev) {
                Some(i) => {
                    let h = self.keys.remove(i);
                    self.release(&h, meta, 0, h.down_time);
                }
                None => {
                    if self.cfg.pass_key && !withheld(box_focused, evdev, self.cfg.keys_while_typing) {
                        self.emit(Call::Key { down: false, code: evdev, meta });
                    }
                }
            }
        }
    }

    /// Release every held key (a real focus loss), with META 0, and forget
    /// them all.
    pub(crate) fn release_all_keys(&mut self, time: i64) {
        for h in std::mem::take(&mut self.keys) {
            self.release(&h, 0, 0, time);
        }
    }

    /// Release the held keys with these X keycodes (released while another
    /// client's grab had the keyboard).
    pub(crate) fn release_x_keycodes(&mut self, x_keycodes: &[i32], time: i64) {
        let (up, kept): (Vec<HeldKey>, Vec<HeldKey>) =
            std::mem::take(&mut self.keys).into_iter().partition(|h| x_keycodes.contains(&(h.evdev + 8)));
        self.keys = kept;
        for h in up {
            self.release(&h, 0, 0, time);
        }
    }

    /// The held keys as (Android keycode, X keycode).
    pub(crate) fn held_keys(&self) -> Vec<(i32, i32)> {
        self.keys.iter().map(|h| (h.akey, h.evdev + 8)).collect()
    }

    // ---- text

    /// Refill the field when the focus generation moved on.
    pub(crate) fn reseed(&mut self, generation: u32, seed: impl FnOnce() -> (String, Option<usize>)) {
        if self.field.seeded != Some(generation) {
            let (text, caret) = seed();
            self.field.seed(generation, &text, caret);
        }
    }

    /// Apply an edit; the new state when it changed something.
    pub(crate) fn edit(&mut self, edit: Edit<'_>) -> Option<FieldState> {
        self.field.apply(edit).then(|| self.field.state())
    }

    /// The selected text, if anything is selected.
    pub(crate) fn selected_text(&self) -> Option<String> {
        self.field.selected_text()
    }

    pub(crate) fn field_state_call(&mut self, which: i64, state: &FieldState, redraw: bool) {
        self.emit(Call::Field {
            which,
            text: state.text.clone(),
            sel_start: state.sel_start,
            sel_end: state.sel_end,
            caret: state.caret,
            redraw,
        });
    }

    // ---- focus

    pub(crate) fn window_focus(&mut self, focused: bool, report: bool) {
        if self.focus.observe(focused) && report {
            self.emit(Call::WindowFocus(focused));
        }
    }
}

// ----------------------------------------------------------------- natives

/// One engine native resolved at load, with its own drop counter.
pub(crate) struct Native {
    name: &'static str,
    ptr: AtomicPtr<c_void>,
    drops: AtomicU64,
}

impl Native {
    const fn new(name: &'static str) -> Self {
        Native { name, ptr: AtomicPtr::new(std::ptr::null_mut()), drops: AtomicU64::new(0) }
    }

    fn set(&self, p: *mut c_void) {
        self.ptr.store(p, Ordering::Release);
    }

    /// The address, or `None` with the drop counted.
    fn get(&self) -> Option<*mut c_void> {
        let p = self.ptr.load(Ordering::Acquire);
        if p.is_null() {
            self.dropped();
            None
        } else {
            Some(p)
        }
    }

    fn dropped(&self) {
        report_unregistered(self.name, &self.drops);
    }
}

static MOUSE_MOVE: Native = Native::new("nativePassMouseMove");
static MOUSE_BUTTON: Native = Native::new("nativePassMouseButton");
static MOUSE_WHEEL: Native = Native::new("nativePassMouseWheel");
static KEY_EVENT: Native = Native::new("nativePassKeyEvent");
static PASS_TEXT: Native = Native::new("nativePassText");
static SYNC_TEXTBOX: Native = Native::new("syncTextboxTextAndCursorPosition2");
static KEYBOARD_SIZE: Native = Native::new("updateKeyboardSize");
static MOUSE_LOCKED: Native = Native::new("nativeGetMainWindowIsMouseLockedCenter");
// AGDK natives are registered by the engine at run time and called by
// handle; these carry only their drop counters.
static AGDK_TOUCH: Native = Native::new("onTouchEventNative");
static AGDK_KEY: Native = Native::new("onKeyDownNative/onKeyUpNative");
static AGDK_REDRAW: Native = Native::new("onSurfaceRedrawNeededNative");
static AGDK_FOCUS: Native = Native::new("onWindowFocusChangedNative");

/// Store the Roblox input natives `intoxicated-run` resolved. Any may be null.
pub fn set_input_natives(
    mouse_move: *mut c_void,
    mouse_button: *mut c_void,
    mouse_wheel: *mut c_void,
    key_event: *mut c_void,
    pass_text: *mut c_void,
    sync_textbox: *mut c_void,
    keyboard_size: *mut c_void,
) {
    MOUSE_MOVE.set(mouse_move);
    MOUSE_BUTTON.set(mouse_button);
    MOUSE_WHEEL.set(mouse_wheel);
    KEY_EVENT.set(key_event);
    PASS_TEXT.set(pass_text);
    SYNC_TEXTBOX.set(sync_textbox);
    KEYBOARD_SIZE.set(keyboard_size);
}

/// Store `nativeGetMainWindowIsMouseLockedCenter`.
pub fn set_mouse_lock_native(p: *mut c_void) {
    MOUSE_LOCKED.set(p);
}

/// Whether a count is 1 or a power of ten, the reporting cadence for drops.
fn first_or_power_of_ten(n: u64) -> bool {
    let mut p = 1u64;
    while p < n {
        match p.checked_mul(10) {
            Some(next) => p = next,
            None => return false,
        }
    }
    p == n
}

/// Count one call dropped because `name` is not there, print at the first and
/// at every power of ten (one line would read as a transient, one per call
/// would bury the log), and record it in the crate's gap registry. Returns
/// whether it printed.
pub(crate) fn report_unregistered(name: &str, drops: &AtomicU64) -> bool {
    let n = drops.fetch_add(1, Ordering::Relaxed) + 1;
    crate::unimplemented::record(crate::unimplemented::Kind::NativeNotRegistered, name);
    let print = first_or_power_of_ten(n);
    if print {
        eprintln!("[input] {name} is not available; {n} call(s) to it dropped so far");
    }
    print
}

// ------------------------------------------------------------- the executor

/// Make the calls, in order. `handle` is the GameActivity handle; 0 sends
/// nothing to AGDK.
fn exec(handle: i64, out: Vec<Emitted>) {
    if out.is_empty() {
        return;
    }
    let s = switches();
    for e in out {
        let started = s.trace_latency.then(Instant::now);
        run_call(handle, &e.call, s);
        if let (Some(t0), Some(origin)) = (started, e.origin) {
            latency::record(origin.source, e.call.kind(), t0.saturating_duration_since(origin.at), t0.elapsed());
        }
    }
}

fn agdk_result(r: Result<Option<bool>, String>, native: &Native, trace: bool) {
    match r {
        Ok(Some(_)) => {}
        Ok(None) => native.dropped(),
        Err(e) => {
            if trace {
                eprintln!("[input] {} failed: {e}", native.name);
            }
        }
    }
}

fn run_call(handle: i64, call: &Call, s: &Switches) {
    match *call {
        Call::MouseMove { x, y, dx, dy } => {
            if s.trace_mouse {
                eprintln!("[input] nativePassMouseMove({x:.1}, {y:.1}, {dx:.3}, {dy:.3})");
            }
            if let Some(f) = MOUSE_MOVE.get() {
                if let Err(e) = ga::pass_mouse_move(f, x, y, dx, dy) {
                    if s.trace_mouse {
                        eprintln!("[input] nativePassMouseMove failed: {e}");
                    }
                }
            }
        }
        Call::MouseButton { x, y, down, index } => {
            if s.trace_mouse {
                eprintln!("[input] nativePassMouseButton({x:.1}, {y:.1}, {down}, {index})");
            }
            if let Some(f) = MOUSE_BUTTON.get() {
                if let Err(e) = ga::pass_mouse_button(f, x, y, down, index) {
                    if s.trace_mouse {
                        eprintln!("[input] nativePassMouseButton failed: {e}");
                    }
                }
            }
        }
        Call::MouseWheel { x, y, delta } => {
            if s.trace_wheel || s.trace_mouse {
                eprintln!("[input] nativePassMouseWheel({x:.1}, {y:.1}, {delta})");
            }
            if let Some(f) = MOUSE_WHEEL.get() {
                if let Err(e) = ga::pass_mouse_wheel(f, x, y, delta) {
                    if s.trace_wheel || s.trace_mouse {
                        eprintln!("[input] nativePassMouseWheel failed: {e}");
                    }
                }
            }
        }
        Call::Key { down, code, meta } => {
            if s.trace_text {
                eprintln!("[input] nativePassKeyEvent({down}, {code}, {meta:#x}, false)");
            }
            if let Some(f) = KEY_EVENT.get() {
                if let Err(e) = ga::pass_key_event(f, down, code, meta, false) {
                    if s.trace_text {
                        eprintln!("[input] nativePassKeyEvent failed: {e}");
                    }
                }
            }
        }
        Call::Motion { action, x, y, buttons, action_button, time, down_time } => {
            if handle != 0 {
                agdk_result(ga::touch(handle, action, x, y, buttons, action_button, time, down_time), &AGDK_TOUCH, s.trace_mouse);
            }
        }
        Call::Scroll { x, y, h, v, time } => {
            if s.trace_wheel {
                eprintln!("[input] AGDK scroll at ({x:.1}, {y:.1}) h={h} v={v}");
            }
            if handle != 0 {
                agdk_result(ga::scroll(handle, x, y, h, v, time), &AGDK_TOUCH, s.trace_wheel);
            }
        }
        Call::AgdkKey { down, code, scan, meta, unicode, time, down_time } => {
            if handle != 0 {
                agdk_result(ga::key(handle, down, code, scan, meta, 0, unicode, time, down_time), &AGDK_KEY, s.trace_text);
            }
        }
        Call::Field { which, ref text, sel_start, sel_end, caret, redraw } => {
            if s.trace_text {
                eprintln!("[input] text -> {} selection=({sel_start}, {sel_end}) box={which}", redact(text, s.show_text));
            }
            if handle != 0 {
                if let Err(e) = ga::text_input(handle, text, sel_start, sel_end) {
                    if s.trace_text {
                        eprintln!("[input] onTextInputEventNative failed: {e}");
                    }
                }
            }
            if let Some(f) = SYNC_TEXTBOX.get() {
                if let Err(e) = ga::sync_textbox(f, text, caret) {
                    if s.trace_text {
                        eprintln!("[input] syncTextboxTextAndCursorPosition2 failed: {e}");
                    }
                }
            }
            if s.pass_text_on_key {
                if let Some(f) = PASS_TEXT.get() {
                    if let Err(e) = ga::pass_text(f, which, text, s.pass_text_flag, caret) {
                        if s.trace_text {
                            eprintln!("[input] nativePassText failed: {e}");
                        }
                    }
                }
            }
            if redraw {
                run_call(handle, &Call::Redraw, s);
            }
        }
        Call::Redraw => {
            if handle != 0 {
                match ga::surface_redraw_needed(handle) {
                    Ok(Some(())) => {}
                    Ok(None) => AGDK_REDRAW.dropped(),
                    Err(e) => super::trace(format_args!("onSurfaceRedrawNeededNative failed: {e}")),
                }
            }
        }
        Call::KeyboardSize { width, height } => {
            if let Some(f) = KEYBOARD_SIZE.get() {
                if let Err(e) = ga::update_keyboard_size(f, false, 0, height, width, 0) {
                    if s.trace_text {
                        eprintln!("[input] updateKeyboardSize failed: {e}");
                    }
                }
            }
        }
        Call::WindowFocus(focused) => {
            if handle != 0 {
                match ga::window_focus(handle, focused) {
                    Ok(Some(())) => {}
                    Ok(None) => AGDK_FOCUS.dropped(),
                    Err(e) => eprintln!("[input] onWindowFocusChangedNative({focused}) failed: {e}"),
                }
            }
        }
    }
}

// ----------------------------------------------------------- global entry

static CORE: Mutex<Option<Core>> = Mutex::new(None);

/// The GameActivity handle last passed in, for the entry points that take
/// none (devctl `key`, the scripts).
static LAST_HANDLE: AtomicI64 = AtomicI64::new(0);

thread_local! {
    /// When the input being dispatched on this thread entered the process.
    static ORIGIN: Cell<Option<Origin>> = const { Cell::new(None) };
}

/// Run `f` on the core, then make the calls it decided, outside the lock (an
/// engine call that came back into this module must not find it held).
fn with_core<R>(handle: Option<i64>, f: impl FnOnce(&mut Core) -> R) -> R {
    if let Some(h) = handle {
        LAST_HANDLE.store(h, Ordering::Relaxed);
    }
    let (r, out) = {
        let mut guard = CORE.lock().unwrap_or_else(|e| e.into_inner());
        let core = guard.get_or_insert_with(|| Core::new(Config::from_switches()));
        core.origin = ORIGIN.with(Cell::get);
        let r = f(core);
        (r, core.take_calls())
    };
    exec(handle.unwrap_or_else(|| LAST_HANDLE.load(Ordering::Relaxed)), out);
    r
}

/// Mark the X event just read, for the latency trace. Free when the trace is
/// off.
pub fn note_x_event_read() {
    if switches().trace_latency {
        ORIGIN.with(|o| o.set(Some(Origin { at: Instant::now(), source: Source::X11 })));
    }
}

/// Mark the devctl command about to be applied, read off the socket at `at`.
pub fn note_devctl_command(at: Instant) {
    if switches().trace_latency {
        ORIGIN.with(|o| o.set(Some(Origin { at, source: Source::Devctl })));
    }
}

/// Nothing being dispatched any more.
pub fn clear_input_origin() {
    ORIGIN.with(|o| o.set(None));
}

// ---- pointer

/// Locked movement read at `anchor`; delivered at the end of the drain or
/// before whatever reaches the engine next, whichever is first.
pub fn add_locked_motion(anchor: (i32, i32), dx: f64, dy: f64, raw: bool, buttons: i32, down_time: i64, time: i64) {
    with_core(None, |c| c.add_locked_motion(anchor, (dx, dy), raw, buttons, down_time, time));
}

/// See [`Core::drop_pending_warp`].
pub fn drop_pending_warp() {
    with_core(None, Core::drop_pending_warp);
}

/// Deliver waiting locked motion now (the end of a drain, or a focus loss).
pub fn flush_locked_motion(handle: i64) {
    with_core(Some(handle), Core::flush_motion);
}

/// Drop waiting locked motion (the lock went).
pub fn discard_locked_motion() {
    with_core(None, Core::discard_motion);
}

/// Locked deliveries since start, and how many carried raw motion.
pub fn locked_deliveries() -> (u64, u64) {
    with_core(None, |c| (c.deliveries, c.raw_deliveries))
}

/// Tell the keepalive where the locked pointer is held, or that it is free.
pub fn set_lock_anchor(anchor: Option<(i32, i32)>) {
    with_core(None, |c| c.set_lock_anchor(anchor));
}

/// Forget the unlocked delta's reference point (a lock taken or released).
pub fn reset_mouse_delta() {
    with_core(None, Core::reset_delta);
}

/// One unlocked core motion event.
pub fn unlocked_motion(handle: i64, x: f32, y: f32, buttons: i32, down_time: i64, time: i64) {
    with_core(Some(handle), |c| c.unlocked_motion(x, y, buttons, down_time, time));
}

/// One accepted button edge from the window's gesture rules.
#[allow(clippy::too_many_arguments)]
pub fn mouse_button(handle: i64, actions: &[(i32, i32, i32)], x: f32, y: f32, press: bool, bit: i32, time: i64, down_time: i64) {
    with_core(Some(handle), |c| c.button(actions, x, y, press, bit, time, down_time));
}

/// The wheel, in detents. Scaled by `INTOXICATED_WHEEL_SCALE`.
pub fn wheel(handle: i64, x: f32, y: f32, hscroll: f32, vscroll: f32, now_ms: i64) {
    with_core(Some(handle), |c| c.wheel(x, y, hscroll, vscroll, now_ms));
}

/// `nativePassMouseMove` with the delta from the last position (scripts).
pub fn pass_mouse_move(x: f32, y: f32) {
    with_core(None, |c| c.plain_move(x, y));
}

/// devctl `move`.
pub fn script_move(handle: i64, x: f32, y: f32, _now_ms: i64) {
    with_core(Some(handle), |c| c.plain_move(x, y));
}

/// devctl `click`, `down` and `up`: `nativePassMouseButton` alone, without
/// touching the window's gesture state. `button` is an Android button bit.
pub fn script_button(handle: i64, x: f32, y: f32, down: bool, button: i32, _now_ms: i64) {
    with_core(Some(handle), |c| c.plain_button(x, y, down, button));
}

/// The instrumentation script's `click:XxY`: hover, move, AGDK down and
/// press, the button down, and the release 40 ms later on the event clock.
pub fn script_click(handle: i64, x: f32, y: f32, now_ms: i64) {
    with_core(Some(handle), |c| {
        if c.cfg.agdk_mouse {
            c.emit(Call::Motion { action: ACTION_HOVER_MOVE, x, y, buttons: 0, action_button: 0, time: now_ms, down_time: 0 });
        }
        c.plain_move(x, y);
        c.button(&[(ACTION_DOWN, BUTTON_PRIMARY, 0), (ACTION_BUTTON_PRESS, BUTTON_PRIMARY, BUTTON_PRIMARY)], x, y, true, BUTTON_PRIMARY, now_ms, now_ms);
        c.button(&[(ACTION_BUTTON_RELEASE, 0, BUTTON_PRIMARY), (ACTION_UP, 0, 0)], x, y, false, BUTTON_PRIMARY, now_ms + 40, now_ms);
    });
}

/// One AGDK mouse event (scripts).
#[allow(clippy::too_many_arguments)]
pub fn deliver_mouse(handle: i64, action: i32, x: f32, y: f32, buttons: i32, action_button: i32, time: i64, down_time: i64) {
    with_core(Some(handle), |c| {
        if c.cfg.agdk_mouse {
            c.emit(Call::Motion { action, x, y, buttons, action_button, time, down_time });
        }
    });
}

/// `onSurfaceRedrawNeededNative`.
pub fn deliver_surface_redraw(handle: i64) {
    with_core(Some(handle), |c| c.emit(Call::Redraw));
}

/// The keepalive against the engine's idle throttle. See [`Core::keepalive`].
pub fn idle_keepalive() {
    with_core(None, Core::keepalive);
}

// ---- pointer lock getter

/// devctl's `fakeenginelock`: 0 ask the engine, 1 false, 2 true.
pub static FAKE_ENGINE_LOCK: AtomicU8 = AtomicU8::new(0);

static LOCK_GETTER_DEAD: AtomicBool = AtomicBool::new(false);
/// The last answer, for the trace: 0 none yet, 1 false, 2 true.
static LOCK_LAST: AtomicU8 = AtomicU8::new(0);

/// Whether the engine wants the pointer locked to the window centre (first
/// person, shift lock), or `None` when it cannot say. The first failure is
/// printed and the getter is not called again this session.
pub fn engine_wants_pointer_lock() -> Option<bool> {
    match FAKE_ENGINE_LOCK.load(Ordering::Relaxed) {
        2 => return Some(true),
        1 => return Some(false),
        _ => {}
    }
    if LOCK_GETTER_DEAD.load(Ordering::Relaxed) {
        return None;
    }
    let f = MOUSE_LOCKED.get()?;
    match ga::call_static_bare_bool(f, INPUT_CLASS) {
        Ok(v) => {
            let code = if v { 2 } else { 1 };
            if LOCK_LAST.swap(code, Ordering::Relaxed) != code && trace_mouse() {
                eprintln!("[input] the engine {} the pointer locked", if v { "wants" } else { "does not want" });
            }
            Some(v)
        }
        Err(e) => {
            if !LOCK_GETTER_DEAD.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[input] nativeGetMainWindowIsMouseLockedCenter failed ({e}); not asking again, \
                     so the lock follows the camera buttons only"
                );
            }
            None
        }
    }
}

// ---- keys

/// One X key event from the window.
pub fn host_key(handle: i64, key: &HostKey) {
    let focused = ga::focused_textbox().is_some();
    // A macro trigger is matched here, on the real key the window delivered, so
    // it only ever fires with the Roblox window focused. A swallowing macro's
    // trigger (its press, its auto-repeat and its release) is kept from the
    // engine; every other key goes on as before.
    if crate::macros::intercept(key.down, key.x_keycode, key.state, focused) {
        return;
    }
    with_core(Some(handle), |c| c.host_key(key, focused));
}

/// devctl `key`/`tap` and the scripts: `nativePassKeyEvent(down, evdev,
/// modifiers)` through the text-box rule and the held table.
pub fn pass_key_event(down: bool, evdev: i32, modifiers: i32) {
    let focused = ga::focused_textbox().is_some();
    with_core(None, |c| c.plain_key(down, evdev, modifiers, focused));
}

/// One AGDK key event (scripts), when `INTOXICATED_AGDK_KEY` is on.
#[allow(clippy::too_many_arguments)]
pub fn deliver_key(handle: i64, down: bool, keycode: i32, scan: i32, meta: i32, _repeat: i32, unicode: i32, time: i64, down_time: i64) {
    with_core(Some(handle), |c| {
        if c.cfg.agdk_key {
            c.emit(Call::AgdkKey { down, code: keycode, scan, meta, unicode, time, down_time });
        }
    });
}

/// Release every key the engine believes is held (a real focus loss).
pub fn release_all_keys(handle: i64, time: i64) {
    with_core(Some(handle), |c| c.release_all_keys(time));
}

/// Release the held keys with these X keycodes.
pub fn release_keys(handle: i64, x_keycodes: &[i32], time: i64) {
    with_core(Some(handle), |c| c.release_x_keycodes(x_keycodes, time));
}

/// The held keys as (Android keycode, X keycode).
pub fn held_keys() -> Vec<(i32, i32)> {
    with_core(None, |c| c.held_keys())
}

// ---- focus

/// The window really gained or lost the keyboard. Tells the engine through
/// `onWindowFocusChangedNative` on a change (`INTOXICATED_NO_FOCUS_REPORT=1`
/// observes without telling).
pub fn report_window_focus(handle: i64, focused: bool) {
    let report = switches().focus_report;
    with_core(Some(handle), |c| c.window_focus(focused, report));
    if trace_mouse() || trace_text() {
        eprintln!("[input] window focus {}", if focused { "in" } else { "out" });
    }
}

/// The window focus as last observed, `None` before any real focus event.
pub fn observed_window_focus() -> Option<bool> {
    with_core(None, |c| c.focus.observed)
}

// ---- text

/// The focused box's starting contents for a new focus generation: the IME
/// state if the engine has ever sent one this session, else the text it
/// handed `showKeyboard`, caret at the end.
fn read_seed() -> (String, Option<usize>) {
    if ga::ime_state_generation() != 0 {
        let text = ga::ime_state_text();
        let (_, end) = ga::ime_state_selection();
        (text, Some(end.max(0) as usize))
    } else {
        (ga::textbox_text(), None)
    }
}

fn reseeded(c: &mut Core) {
    let generation = ga::textbox_generation();
    c.reseed(generation, read_seed);
}

/// Apply an edit to the focused field; its new state when it changed.
pub fn edit_text_field(edit: Edit<'_>) -> Option<FieldState> {
    with_core(None, |c| {
        reseeded(c);
        c.edit(edit)
    })
}

/// Send a field state to the engine: text input, sync, then the redraw.
pub fn deliver_field_state(handle: i64, which: i64, state: &FieldState) {
    with_core(Some(handle), |c| c.field_state_call(which, state, true));
}

/// The selected text of the focused field, if any.
pub fn text_buffer_selected_text() -> Option<String> {
    with_core(None, |c| {
        reseeded(c);
        c.selected_text()
    })
}

/// The field's text and caret as they are, without refilling it.
pub fn text_buffer_peek() -> (String, usize) {
    with_core(None, |c| (c.field.text(), c.field.caret))
}

/// Bumped on every change and every refill.
pub fn text_buffer_revision() -> u64 {
    with_core(None, |c| c.field.revision)
}

/// A key press for the focused box `which`: a shortcut, a caret move, an
/// erase or the committed text. `typed` is what the input method committed.
pub fn host_text(handle: i64, which: i64, keysym: u64, meta: i32, typed: &str) {
    if let Some(shortcut) = text_shortcut(keysym, meta) {
        super::clipboard::run_text_shortcut(handle, which, shortcut);
        return;
    }
    let Some(edit) = edit_for_key(keysym, meta, typed) else {
        return;
    };
    with_core(Some(handle), |c| {
        reseeded(c);
        if let Some(state) = c.edit(edit) {
            c.field_state_call(which, &state, true);
        }
    });
}

/// devctl `text`: per character, a key press for letters, digits and space
/// (withheld while a box is focused), the character into the focused box
/// (state and sync, no redraw), then the release. Returns how many went into
/// a box.
pub fn script_type(handle: i64, text: &str, now_ms: i64) -> usize {
    let mut typed = 0;
    for (i, ch) in text.chars().enumerate() {
        let t = now_ms + 60 * i as i64;
        let code = ascii_to_evdev(ch);
        if let Some(code) = code {
            deliver_key(handle, true, evdev_to_android(code).unwrap_or(0), code, 0, 0, ch as i32, t, t);
            pass_key_event(true, code, 0);
        }
        if let Some(which) = ga::focused_textbox() {
            let mut buf = [0u8; 4];
            let s: &str = ch.encode_utf8(&mut buf);
            let went_in = with_core(Some(handle), |c| {
                reseeded(c);
                match c.edit(Edit::Insert(s)) {
                    Some(state) => {
                        c.field_state_call(which, &state, false);
                        true
                    }
                    None => false,
                }
            });
            typed += usize::from(went_in);
        }
        if let Some(code) = code {
            deliver_key(handle, false, evdev_to_android(code).unwrap_or(0), code, 0, 0, ch as i32, t + 30, t);
            pass_key_event(false, code, 0);
        }
    }
    typed
}

/// `updateKeyboardSize` once per focus generation, when enabled.
pub fn report_keyboard_state(size: (i32, i32)) {
    let generation = ga::textbox_generation();
    with_core(None, |c| {
        if c.keyboard_reported != Some(generation) {
            c.keyboard_reported = Some(generation);
            c.emit(Call::KeyboardSize { width: size.0, height: size.1 });
        }
    });
}

// ---- touchscreen declaration

/// What `PlatformParams.isTouchDevice` is told, and why:
/// `INTOXICATED_NO_TOUCH` forces no; else `INTOXICATED_INPUT_TOUCH` when set
/// to something (`0`, `false`, `no`, `off` in any case are no, anything else
/// yes); else the display backend's own answer.
pub fn touchscreen_reported(no_touch: bool, forced: Option<&str>, backend: bool) -> (bool, &'static str) {
    if no_touch {
        return (false, "INTOXICATED_NO_TOUCH");
    }
    if let Some(v) = forced.map(str::trim).filter(|v| !v.is_empty()) {
        let off = ["0", "false", "no", "off"].iter().any(|o| v.eq_ignore_ascii_case(o));
        return (!off, "INTOXICATED_INPUT_TOUCH");
    }
    (backend, "the display backend")
}

/// Tell the native side whether there is a touchscreen, once, at window open.
pub fn report_touchscreen(backend_has_one: bool) {
    let forced = std::env::var("INTOXICATED_INPUT_TOUCH").ok();
    let (present, why) = touchscreen_reported(flag("INTOXICATED_NO_TOUCH"), forced.as_deref(), backend_has_one);
    println!("[android] touchscreen: {} (from {why})", if present { "yes" } else { "no" });
    ga::set_touchscreen_present(present);
}

// ---- idle throttle

/// When the keepalive stands down and lets the engine's own idle throttle
/// work: `INTOXICATED_THROTTLE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleWhen {
    /// `visible` (the default): when the window is known hidden and not known
    /// focused.
    Visible,
    /// `unfocused`: when the window is known to have lost focus.
    Unfocused,
    /// `off`: never; the keepalive always runs. The launcher sets this.
    Off,
}

pub fn parse_throttle(v: Option<&str>) -> ThrottleWhen {
    match v.map(str::trim) {
        Some("off") => ThrottleWhen::Off,
        Some("unfocused") => ThrottleWhen::Unfocused,
        _ => ThrottleWhen::Visible,
    }
}

pub fn throttle_policy() -> ThrottleWhen {
    parse_throttle(std::env::var("INTOXICATED_THROTTLE").ok().as_deref())
}

/// Whether the keepalive should run this iteration.
pub fn keepalive_wanted(policy: ThrottleWhen, focused: Option<bool>, visible: Option<bool>) -> bool {
    match policy {
        ThrottleWhen::Off => true,
        ThrottleWhen::Unfocused => focused != Some(false),
        ThrottleWhen::Visible => !(visible == Some(false) && focused != Some(true)),
    }
}

// ------------------------------------------------------------ latency trace

/// `INTOXICATED_TRACE_INPUT_LATENCY=1`: for every engine call made for an
/// input, the time from the input entering the process (an X event coming
/// out of `XNextEvent`, a devctl line off the socket) to the call starting,
/// and how long the call took. Kept in memory and summarised: on stderr every
/// ten seconds while samples arrive, and on devctl's `latency` verb.
mod latency {
    use super::Source;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Samples kept per series; the trace stops adding beyond it.
    const CAP: usize = 1 << 20;

    struct Series {
        source: Source,
        kind: &'static str,
        lat_us: Vec<u32>,
        cost_us: Vec<u32>,
    }

    struct Stats {
        series: Vec<Series>,
        printed: Option<Instant>,
    }

    static STATS: Mutex<Stats> = Mutex::new(Stats { series: Vec::new(), printed: None });

    fn us(d: Duration) -> u32 {
        u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
    }

    pub(super) fn record(source: Source, kind: &'static str, lat: Duration, cost: Duration) {
        let print = {
            let mut s = STATS.lock().unwrap_or_else(|e| e.into_inner());
            let i = match s.series.iter().position(|x| x.source == source && x.kind == kind) {
                Some(i) => i,
                None => {
                    s.series.push(Series { source, kind, lat_us: Vec::new(), cost_us: Vec::new() });
                    s.series.len() - 1
                }
            };
            let series = &mut s.series[i];
            if series.lat_us.len() < CAP {
                series.lat_us.push(us(lat));
                series.cost_us.push(us(cost));
            }
            let due = s.printed.is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
            if due {
                s.printed = Some(Instant::now());
            }
            due
        };
        if print {
            eprintln!("[input] latency {}", summary());
        }
    }

    /// `p`-th percentile (0..=100) of `v`, nearest rank.
    pub(super) fn percentile(sorted: &[u32], p: f64) -> u32 {
        if sorted.is_empty() {
            return 0;
        }
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[rank.clamp(1, sorted.len()) - 1]
    }

    pub(super) fn summary() -> String {
        let s = STATS.lock().unwrap_or_else(|e| e.into_inner());
        if s.series.is_empty() {
            return "no samples (is INTOXICATED_TRACE_INPUT_LATENCY=1 set?)".into();
        }
        let mut parts = Vec::new();
        for x in &s.series {
            let mut lat = x.lat_us.clone();
            let mut cost = x.cost_us.clone();
            lat.sort_unstable();
            cost.sort_unstable();
            parts.push(format!(
                "{}.{} n={} p50={}us p99={}us max={}us call_p50={}us call_max={}us",
                match x.source {
                    Source::X11 => "x11",
                    Source::Devctl => "devctl",
                },
                x.kind,
                lat.len(),
                percentile(&lat, 50.0),
                percentile(&lat, 99.0),
                lat.last().copied().unwrap_or(0),
                percentile(&cost, 50.0),
                cost.last().copied().unwrap_or(0),
            ));
        }
        parts.join(" | ")
    }

    pub(super) fn reset() {
        let mut s = STATS.lock().unwrap_or_else(|e| e.into_inner());
        s.series.clear();
        s.printed = None;
    }
}

/// The latency trace's summary, for devctl.
pub fn latency_summary() -> String {
    latency::summary()
}

/// Forget the latency samples, for devctl.
pub fn latency_reset() {
    latency::reset();
}

#[cfg(test)]
mod tests;
