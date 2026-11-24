//! In-game macros: a key press, seen on the client's own input path, plays a
//! recorded sequence of input back through that same path.
//!
//! The macros live in `<profile>/macros.json`, written by Intoxicated Settings
//! and readable by hand. [`load`] reads them once at startup, next to where
//! `devctl::start` binds the dev socket, so the profile has latched by then.
//! `INTOXICATED_MACROS=0` turns the whole feature off; any other value, or
//! none, leaves it on.
//!
//! A macro has a trigger (a physical key and the Ctrl/Shift/Alt that must be
//! held with it) and a list of steps. The trigger is matched in [`intercept`],
//! called from `input::host_key` for every real key the window delivers, so a
//! macro only ever fires while the Roblox window has the keyboard: nothing is
//! read from the X server and nothing fires on the desktop. A matched press
//! hands the steps to one player thread.
//!
//! The steps are the same events the dev socket queues (`Cmd`): taps, key
//! holds, pointer moves, clicks, scrolls, typed text and waits. The player
//! thread never calls the engine itself. It sleeps out a step's wait, then
//! pushes the step into [`READY`] and wakes the pump; the pump drains READY in
//! [`apply_due`] and applies each step through [`crate::devctl::apply_cmd`], on
//! the thread that owns the engine's input, exactly as a queued dev-socket
//! command is applied.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::devctl::Cmd;

/// The switch that turns the feature off.
const ENV: &str = "INTOXICATED_MACROS";

/// The X modifier-state bits a trigger can ask for: Shift, Control, Mod1 (Alt),
/// the same three the dev socket's `xkey` state documents. A trigger matches
/// only when the state masked to these equals what it asked for, so a plain `f`
/// macro does not fire on Ctrl+f and the other way round.
const SHIFT: u32 = 1;
const CTRL: u32 = 4;
const ALT: u32 = 8;
const MOD_MASK: u32 = SHIFT | CTRL | ALT;

/// A button press, release, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Down,
    Up,
    Press,
}

/// The modifiers a key step holds down, as X state bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mods {
    ctrl: bool,
    shift: bool,
    alt: bool,
}

impl Mods {
    fn x_state(self) -> u32 {
        (if self.ctrl { CTRL } else { 0 }) | (if self.shift { SHIFT } else { 0 }) | (if self.alt { ALT } else { 0 })
    }

    /// The engine META integer these modifiers make, through the window's own
    /// X-state-to-META conversion, so a macro key carries the same modifier
    /// bits a real one would.
    fn meta(self) -> i32 {
        crate::android::input::android_meta_state(self.x_state())
    }
}

/// What a macro trigger is: a physical key (evdev code) and the modifiers that
/// must be held with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    evdev: i32,
    mods: Mods,
}

/// One step of a macro.
#[derive(Clone, Debug, PartialEq)]
enum Step {
    /// A key tapped (down then up), held or released, with its modifiers.
    Key { edge: Edge, evdev: i32, mods: Mods },
    /// The pointer moved to window coordinates.
    Move { x: f32, y: f32 },
    /// A mouse button pressed, released, or clicked at window coordinates.
    Button { edge: Edge, x: f32, y: f32, button: i32 },
    /// A wheel turn of `detents` at window coordinates.
    Scroll { x: f32, y: f32, detents: f32 },
    /// Text typed one character at a time.
    Text(String),
    /// A pause before the next step.
    Wait { ms: u64 },
}

/// One macro: its name, whether it is on, its trigger, whether the trigger key
/// is kept from the game, whether it repeats while the key is held, and its
/// steps.
#[derive(Clone, Debug, PartialEq)]
pub struct Macro {
    name: String,
    enabled: bool,
    trigger: Trigger,
    swallow: bool,
    repeat: bool,
    steps: Vec<Step>,
}

// ---------------------------------------------------------------- the file

/// Parse a macros document. Blank text is no macros, not an error. A macro that
/// cannot be read is left out with its reason in `skipped` rather than failing
/// the whole file, so one bad hand-edit does not lose the rest.
pub fn parse(text: &str, skipped: &mut Vec<String>) -> Result<Vec<Macro>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let doc: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let serde_json::Value::Object(root) = doc else {
        return Err("the document must be a JSON object".into());
    };
    let list = match root.get("macros") {
        Some(serde_json::Value::Array(a)) => a,
        Some(_) => return Err("\"macros\" must be an array".into()),
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for (i, value) in list.iter().enumerate() {
        match parse_macro(value) {
            Ok(m) => out.push(m),
            Err(why) => skipped.push(format!("macro {}: {why}", i + 1)),
        }
    }
    Ok(out)
}

fn obj_get<'a>(o: &'a serde_json::Map<String, serde_json::Value>, key: &str) -> Option<&'a serde_json::Value> {
    o.get(key)
}

fn as_bool(v: Option<&serde_json::Value>, default: bool) -> bool {
    v.and_then(serde_json::Value::as_bool).unwrap_or(default)
}

fn as_f32(v: Option<&serde_json::Value>) -> Option<f32> {
    v.and_then(serde_json::Value::as_f64).map(|n| n as f32)
}

fn as_i32(v: Option<&serde_json::Value>) -> Option<i32> {
    v.and_then(serde_json::Value::as_i64).map(|n| n as i32)
}

fn read_mods(o: &serde_json::Map<String, serde_json::Value>) -> Mods {
    Mods { ctrl: as_bool(obj_get(o, "ctrl"), false), shift: as_bool(obj_get(o, "shift"), false), alt: as_bool(obj_get(o, "alt"), false) }
}

fn parse_macro(value: &serde_json::Value) -> Result<Macro, String> {
    let serde_json::Value::Object(o) = value else {
        return Err("each macro must be a JSON object".into());
    };
    let name = obj_get(o, "name").and_then(serde_json::Value::as_str).unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err("a macro needs a name".into());
    }
    let serde_json::Value::Object(trig) = obj_get(o, "trigger").ok_or("no trigger")? else {
        return Err("the trigger must be a JSON object".into());
    };
    let evdev = as_i32(obj_get(trig, "key")).ok_or("the trigger needs a \"key\" evdev code")?;
    if !(1..=255).contains(&evdev) {
        return Err(format!("trigger key {evdev} is not an evdev code 1..255"));
    }
    let trigger = Trigger { evdev, mods: read_mods(trig) };

    let steps_val = match obj_get(o, "steps") {
        Some(serde_json::Value::Array(a)) => a,
        Some(_) => return Err("\"steps\" must be an array".into()),
        None => return Err("a macro needs steps".into()),
    };
    let mut steps = Vec::with_capacity(steps_val.len());
    for (i, s) in steps_val.iter().enumerate() {
        steps.push(parse_step(s).map_err(|e| format!("step {}: {e}", i + 1))?);
    }
    if steps.is_empty() {
        return Err("a macro needs at least one step".into());
    }
    Ok(Macro {
        name,
        enabled: as_bool(obj_get(o, "enabled"), true),
        trigger,
        swallow: as_bool(obj_get(o, "swallow"), false),
        repeat: as_bool(obj_get(o, "repeat"), false),
        steps,
    })
}

fn parse_step(value: &serde_json::Value) -> Result<Step, String> {
    let serde_json::Value::Object(o) = value else {
        return Err("a step must be a JSON object".into());
    };
    let kind = obj_get(o, "type").and_then(serde_json::Value::as_str).ok_or("a step needs a \"type\"")?;
    let evdev_step = |edge: Edge| -> Result<Step, String> {
        let evdev = as_i32(obj_get(o, "key")).ok_or("needs a \"key\" evdev code")?;
        if !(1..=255).contains(&evdev) {
            return Err(format!("key {evdev} is not an evdev code 1..255"));
        }
        Ok(Step::Key { edge, evdev, mods: read_mods(o) })
    };
    let point = || -> Result<(f32, f32), String> {
        Ok((as_f32(obj_get(o, "x")).ok_or("needs \"x\"")?, as_f32(obj_get(o, "y")).ok_or("needs \"y\"")?))
    };
    match kind {
        "tap" => evdev_step(Edge::Press),
        "keydown" => evdev_step(Edge::Down),
        "keyup" => evdev_step(Edge::Up),
        "move" => {
            let (x, y) = point()?;
            Ok(Step::Move { x, y })
        }
        "click" | "buttondown" | "buttonup" => {
            let (x, y) = point()?;
            let button = as_i32(obj_get(o, "button")).unwrap_or(1);
            let edge = match kind {
                "buttondown" => Edge::Down,
                "buttonup" => Edge::Up,
                _ => Edge::Press,
            };
            Ok(Step::Button { edge, x, y, button })
        }
        "scroll" => {
            let (x, y) = point()?;
            let detents = as_f32(obj_get(o, "detents")).ok_or("needs \"detents\"")?;
            Ok(Step::Scroll { x, y, detents })
        }
        "text" => {
            let t = obj_get(o, "text").and_then(serde_json::Value::as_str).ok_or("needs \"text\"")?;
            if t.is_empty() {
                return Err("empty text".into());
            }
            Ok(Step::Text(t.to_string()))
        }
        "wait" => {
            let ms = obj_get(o, "ms").and_then(serde_json::Value::as_u64).ok_or("needs \"ms\"")?;
            Ok(Step::Wait { ms: ms.min(3_600_000) })
        }
        other => Err(format!("unknown step type {other:?}")),
    }
}

// ----------------------------------------------------------- loaded state

static MACROS: Mutex<Vec<Macro>> = Mutex::new(Vec::new());
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether the switch leaves the feature on: unset, or set to anything but
/// empty or `0`.
fn switch_on() -> bool {
    match std::env::var(ENV) {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => true,
    }
}

/// Read `<profile>/macros.json` once, after the profile has latched. A read or
/// parse problem is one line on stdout and never stops the launch.
pub fn load() {
    if !switch_on() {
        println!("  macros: off (INTOXICATED_MACROS={})", std::env::var(ENV).unwrap_or_default());
        return;
    }
    let path = crate::profile::active().join("macros.json");
    load_from(&path);
}

fn load_from(path: &Path) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // No file is the common case: the user has not made any macros.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ENABLED.store(false, Ordering::Relaxed);
            return;
        }
        Err(e) => {
            println!("  macros: could not read {} ({e}); none loaded", path.display());
            ENABLED.store(false, Ordering::Relaxed);
            return;
        }
    };
    let mut skipped = Vec::new();
    match parse(&text, &mut skipped) {
        Ok(list) => {
            let on = list.iter().filter(|m| m.enabled).count();
            for why in &skipped {
                println!("  macros: {why} (skipped)");
            }
            *MACROS.lock().unwrap_or_else(|e| e.into_inner()) = list;
            ENABLED.store(on > 0, Ordering::Relaxed);
            println!("  macros: {on} enabled");
        }
        Err(e) => {
            println!("  macros: {} is unusable ({e}); none loaded", path.display());
            ENABLED.store(false, Ordering::Relaxed);
        }
    }
}

// --------------------------------------------------------------- triggers

/// Trigger keys currently held, and whether each was kept from the game, so a
/// release is swallowed to match its press and an auto-repeat does not fire the
/// macro again.
static HELD: Mutex<Vec<(i32, bool)>> = Mutex::new(Vec::new());

/// Match a real key event against the macros, firing one on a fresh press.
/// Returns whether the key is kept from the game (a swallowing macro's trigger,
/// press and release alike). Cheap and false when the feature is off.
///
/// `box_focused` is true when a Roblox text box has the keyboard; macros do not
/// fire then, so the trigger key can be typed.
pub fn intercept(down: bool, x_keycode: u32, x_state: u32, box_focused: bool) -> bool {
    if !ENABLED.load(Ordering::Relaxed) || x_keycode < 8 {
        return false;
    }
    let evdev = x_keycode as i32 - 8;
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if !down {
        if let Some(pos) = held.iter().position(|&(e, _)| e == evdev) {
            let (_, swallowed) = held.remove(pos);
            release(evdev);
            return swallowed;
        }
        return false;
    }
    // A press. An auto-repeat of a key already held keeps its swallow verdict
    // and does not fire again.
    if let Some(&(_, swallowed)) = held.iter().find(|&&(e, _)| e == evdev) {
        return swallowed;
    }
    if box_focused {
        return false;
    }
    let wanted = x_state & MOD_MASK;
    let found = {
        let macros = MACROS.lock().unwrap_or_else(|e| e.into_inner());
        macros
            .iter()
            .find(|m| m.enabled && m.trigger.evdev == evdev && m.trigger.mods.x_state() == wanted)
            .cloned()
    };
    let swallow = found.as_ref().is_some_and(|m| m.swallow);
    held.push((evdev, swallow));
    drop(held);
    if let Some(m) = found {
        fire(m, evdev);
    }
    swallow
}

// --------------------------------------------------------------- the player

/// A step resolved to what the player does: wait, or push a command to apply.
#[derive(Clone, Debug)]
enum Beat {
    Wait(u64),
    Emit(Cmd),
}

/// One run handed to the player thread: the resolved beats, whether to loop,
/// and a flag the trigger's release clears to stop a looping run.
struct Job {
    beats: Vec<Beat>,
    repeat: bool,
    keep_going: Arc<AtomicBool>,
}

/// Commands the player has made due; the pump drains them in [`apply_due`].
static READY: Mutex<VecDeque<Cmd>> = Mutex::new(VecDeque::new());

/// The player thread's inbox, and the loop flag of each trigger key still held,
/// so a release can stop a repeating run.
static PLAYER: OnceLock<Sender<Job>> = OnceLock::new();
static RUNNING: Mutex<Option<HashMap<i32, Arc<AtomicBool>>>> = Mutex::new(None);

fn resolve(steps: &[Step]) -> Vec<Beat> {
    let mut beats = Vec::new();
    for step in steps {
        match step {
            Step::Wait { ms } => beats.push(Beat::Wait(*ms)),
            Step::Key { edge, evdev, mods } => push_key(&mut beats, *edge, *evdev, mods.meta()),
            Step::Move { x, y } => beats.push(Beat::Emit(Cmd::Move { x: *x, y: *y })),
            Step::Button { edge, x, y, button } => push_button(&mut beats, *edge, *x, *y, *button),
            Step::Scroll { x, y, detents } => beats.push(Beat::Emit(Cmd::Scroll { x: *x, y: *y, detents: *detents })),
            Step::Text(t) => beats.push(Beat::Emit(Cmd::Text(t.clone()))),
        }
    }
    beats
}

fn push_key(beats: &mut Vec<Beat>, edge: Edge, evdev: i32, meta: i32) {
    if edge != Edge::Up {
        beats.push(Beat::Emit(Cmd::Key { down: true, evdev, modifiers: meta }));
    }
    if edge != Edge::Down {
        beats.push(Beat::Emit(Cmd::Key { down: false, evdev, modifiers: meta }));
    }
}

fn push_button(beats: &mut Vec<Beat>, edge: Edge, x: f32, y: f32, button: i32) {
    if edge == Edge::Press {
        beats.push(Beat::Emit(Cmd::Move { x, y }));
    }
    if edge != Edge::Up {
        beats.push(Beat::Emit(Cmd::Button { x, y, down: true, button }));
    }
    if edge != Edge::Down {
        beats.push(Beat::Emit(Cmd::Button { x, y, down: false, button }));
    }
}

fn fire(m: Macro, evdev: i32) {
    let keep_going = Arc::new(AtomicBool::new(true));
    {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        running.get_or_insert_with(HashMap::new).insert(evdev, keep_going.clone());
    }
    let job = Job { beats: resolve(&m.steps), repeat: m.repeat, keep_going };
    let tx = PLAYER.get_or_init(start_player);
    // A full inbox (a storm of triggers) drops the run rather than blocking the
    // input path; the key is still held, so the next press plays again.
    let _ = tx.send(job);
}

fn release(evdev: i32) {
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = running.as_mut() {
        if let Some(flag) = map.remove(&evdev) {
            flag.store(false, Ordering::Relaxed);
        }
    }
}

fn start_player() -> Sender<Job> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let _ = std::thread::Builder::new().name("intoxicated-macros".into()).spawn(move || player(rx));
    tx
}

/// The player thread: one run at a time, sleeping out each wait, pushing each
/// command to READY and waking the pump. The engine is never touched here.
fn player(rx: Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        loop {
            for beat in &job.beats {
                if !job.keep_going.load(Ordering::Relaxed) {
                    break;
                }
                match beat {
                    Beat::Wait(ms) => std::thread::sleep(Duration::from_millis(*ms)),
                    Beat::Emit(cmd) => {
                        READY.lock().unwrap_or_else(|e| e.into_inner()).push_back(cmd.clone());
                        crate::android::looper::wake_pump();
                    }
                }
            }
            if !job.repeat || !job.keep_going.load(Ordering::Relaxed) {
                break;
            }
        }
    }
}

/// Apply every command the player has made due, on the pump thread, through the
/// same path a queued dev-socket command takes. Called from the looper pump.
pub fn apply_due(handle: i64) {
    loop {
        let next = READY.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        match next {
            Some(cmd) => crate::devctl::apply_cmd(handle, cmd),
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(json: &str) -> Macro {
        let mut skipped = Vec::new();
        let list = parse(json, &mut skipped).expect("parses");
        assert!(skipped.is_empty(), "unexpected skips: {skipped:?}");
        assert_eq!(list.len(), 1, "expected one macro");
        list.into_iter().next().unwrap()
    }

    #[test]
    fn a_full_macro_round_trips_from_json() {
        let m = one(
            r#"{"macros":[{
                "name":"Rapid","enabled":true,"swallow":true,"repeat":true,
                "trigger":{"key":33,"ctrl":true},
                "steps":[
                    {"type":"tap","key":57},
                    {"type":"wait","ms":40},
                    {"type":"keydown","key":17},
                    {"type":"click","x":640,"y":360,"button":1},
                    {"type":"move","x":10,"y":20},
                    {"type":"scroll","x":1,"y":2,"detents":-1},
                    {"type":"text","text":"gg"}
                ]}]}"#,
        );
        assert_eq!(m.name, "Rapid");
        assert!(m.swallow && m.repeat && m.enabled);
        assert_eq!(m.trigger.evdev, 33);
        assert_eq!(m.trigger.mods, Mods { ctrl: true, shift: false, alt: false });
        assert_eq!(m.steps.len(), 7);
    }

    #[test]
    fn blank_and_no_array_are_empty_not_errors() {
        let mut skipped = Vec::new();
        assert!(parse("   ", &mut skipped).unwrap().is_empty());
        assert!(parse(r#"{"other":1}"#, &mut skipped).unwrap().is_empty());
        assert!(skipped.is_empty());
    }

    #[test]
    fn a_bad_macro_is_skipped_and_the_rest_kept() {
        let mut skipped = Vec::new();
        let list = parse(
            r#"{"macros":[
                {"name":"ok","trigger":{"key":30},"steps":[{"type":"tap","key":30}]},
                {"name":"no steps","trigger":{"key":31},"steps":[]},
                {"trigger":{"key":32},"steps":[{"type":"tap","key":32}]}
            ]}"#,
            &mut skipped,
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "ok");
        assert_eq!(skipped.len(), 2, "two bad macros named: {skipped:?}");
    }

    #[test]
    fn a_tap_resolves_to_a_down_then_an_up() {
        let beats = resolve(&[Step::Key { edge: Edge::Press, evdev: 17, mods: Mods::default() }]);
        assert_eq!(beats.len(), 2);
        assert!(matches!(beats[0], Beat::Emit(Cmd::Key { down: true, evdev: 17, .. })));
        assert!(matches!(beats[1], Beat::Emit(Cmd::Key { down: false, evdev: 17, .. })));
    }

    #[test]
    fn a_click_moves_then_presses_then_releases() {
        let beats = resolve(&[Step::Button { edge: Edge::Press, x: 5.0, y: 6.0, button: 1 }]);
        assert_eq!(beats.len(), 3);
        assert!(matches!(beats[0], Beat::Emit(Cmd::Move { .. })));
        assert!(matches!(beats[1], Beat::Emit(Cmd::Button { down: true, .. })));
        assert!(matches!(beats[2], Beat::Emit(Cmd::Button { down: false, .. })));
    }

    #[test]
    fn an_out_of_range_trigger_key_is_refused() {
        let mut skipped = Vec::new();
        let list = parse(r#"{"macros":[{"name":"x","trigger":{"key":999},"steps":[{"type":"tap","key":30}]}]}"#, &mut skipped)
            .unwrap();
        assert!(list.is_empty());
        assert_eq!(skipped.len(), 1);
    }
}
