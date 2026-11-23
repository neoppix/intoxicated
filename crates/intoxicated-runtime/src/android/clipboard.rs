//! The clipboard bridge: copy out of Roblox onto the X11 CLIPBOARD selection,
//! paste from it into the focused text box, and Ctrl+A/C/X/V in a box.
//!
//! Written clean-room from a behavioural spec of the engine-facing contract,
//! Android's `ClipboardManager` documentation and the ICCCM selection rules.
//! The module it replaces was not read.
//!
//! **What the engine does.** `libroblox.so` never asks for
//! `android.content.ClipboardManager` or `ClipData`; Roblox's Java does that on
//! Android, and that Java does not run here. A copy inside an experience is a
//! message-bus publish on [`MESSAGE_ID`] with a JSON payload, so this module
//! subscribes to it through [`crate::bus`] (the only subscriber path, shared
//! with join links, the web view and devctl). A paste has no engine side at
//! all: on Android an `EditText` over the surface takes it and the engine only
//! sees the field change. Here `input.rs`'s editor is that field, so a paste
//! is an insert edit followed by the same state delivery a keystroke makes.
//!
//! **Formats and limits.** Text only, UTF-8, at most [`MAX_BYTES`] either way.
//! The X side (`x11_clipboard.rs`) offers UTF8_STRING, TEXT and STRING
//! (Latin-1) and reads UTF8_STRING, then STRING. Control characters in a paste
//! become spaces, because a Roblox text box is one line and the editor refuses
//! them. Images, rich text and PRIMARY are not handled.
//!
//! **Threads.** The bus calls [`on_set_clipboard_text`] on whichever engine
//! thread published; it only parks the text in one slot (a newer copy replaces
//! one not yet taken). Everything that touches X or the editor runs on the
//! looper thread, which also drains the X connection: [`pump_pending`] takes
//! the parked text onto the clipboard, and Ctrl+V asks the selection owner and
//! returns at once, the answer arriving later through the pump's drain
//! ([`paste_arrived`]). Nothing on the Ctrl+V path waits for another client.
//!
//! **Privacy.** No clipboard text, payload, selection or request body is ever
//! printed, by any switch or in any error: lines carry byte and character
//! counts, JSON member names and JSON kinds. A value carrying text hides it
//! from `Debug`.
//!
//! Switches: [`SKIP_VARIABLE`] (any value) turns the bridge off while still
//! subscribing, as a control; [`TRACE_VARIABLE`] (any value) traces this
//! module, the X side and the bus natives together.

use std::ffi::{c_char, c_void};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use intoxicated_linker_sys as linker;
use linker::game_activity as ga;
use serde_json::Value;

use super::input::{self, Edit, FieldState, TextShortcut};
use super::x11_clipboard::{self, PasteFor, ReadStart, X11Clipboard};
use crate::bus;

/// The bus message a copy inside an experience publishes.
pub const MESSAGE_ID: &str = "ExternalContentSharing.setClipboardText";

/// The most text taken in either direction, in bytes.
pub const MAX_BYTES: usize = 65536;

/// Any value: copy and paste do nothing, but the subscription is still made.
pub const SKIP_VARIABLE: &str = "INTOXICATED_SKIP_CLIPBOARD";

/// Any value: trace lines from this module, `x11_clipboard` and the bus.
pub const TRACE_VARIABLE: &str = "INTOXICATED_TRACE_CLIPBOARD";

/// The members that may hold the text, in the order they are tried. The real
/// member name has never been seen from a copy in an experience (INFERRED):
/// `content` is first because the Android app's own error text names it.
const CANDIDATES: [&str; 5] = ["content", "text", "value", "clipboardText", "data"];

/// How long the instrumentation script's blocking `paste` waits for the owner.
const READ_TIMEOUT: Duration = Duration::from_millis(400);

/// How long a Ctrl+V waits for the owner before it is given up on.
const PASTE_TIMEOUT: Duration = Duration::from_secs(1);

fn skip() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(SKIP_VARIABLE).is_some())
}

/// Whether [`TRACE_VARIABLE`] is set.
pub fn trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(TRACE_VARIABLE).is_some())
}

/// Whether a bus message id carries clipboard text, so a generic listener
/// (devctl's `bussub`) must not print its payload.
pub fn is_private_topic(message_id: &str) -> bool {
    message_id == MESSAGE_ID
}

// --------------------------------------------------------- engine to host

/// Where in the payload the text was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The payload was a bare JSON string.
    Whole,
    /// A member of the payload object.
    Member(String),
}

impl Source {
    /// The key as the spec reports it.
    pub fn label(&self) -> &str {
        match self {
            Source::Whole => "(the whole payload)",
            Source::Member(name) => name,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Whole => f.write_str("the whole payload"),
            Source::Member(name) => write!(f, "the payload's {name:?} member"),
        }
    }
}

/// Text the engine asked to have copied.
#[derive(Clone, PartialEq, Eq)]
pub struct Copied {
    text: String,
    source: Source,
}

/// Never the text: where it came from and how long it is.
impl fmt::Debug for Copied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Copied").field("source", &self.source.label()).field("bytes", &self.text.len()).finish()
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The text in a `setClipboardText` payload, and where it was. Errors never
/// contain the payload or any value in it.
pub fn extract(payload: &str) -> Result<(Source, String), String> {
    let value: Value = serde_json::from_str(payload)
        .map_err(|e| format!("the payload is not JSON ({} bytes): {e}", payload.len()))?;
    let mut members = match value {
        Value::String(text) => return Ok((Source::Whole, text)),
        Value::Object(members) => members,
        other => return Err(format!("the payload is {} and carries no members", kind(&other))),
    };
    let chosen = CANDIDATES
        .iter()
        .find(|name| members.get(**name).is_some_and(Value::is_string))
        .map(|name| name.to_string())
        .or_else(|| {
            let mut strings = members.iter().filter(|(_, v)| v.is_string()).map(|(k, _)| k);
            match (strings.next(), strings.next()) {
                (Some(only), None) => Some(only.clone()),
                _ => None,
            }
        });
    if let Some(name) = chosen {
        if let Some(Value::String(text)) = members.remove(&name) {
            return Ok((Source::Member(name), text));
        }
    }
    if members.is_empty() {
        return Err("no member of the payload looks like the text; it has no members".into());
    }
    let list: Vec<String> = members.iter().map(|(k, v)| format!("{k} ({})", kind(v))).collect();
    Err(format!("no member of the payload looks like the text; it has {}", list.join(", ")))
}

/// What the sink makes of one payload: the text to park, or the line to print.
fn receive(payload: &[u8]) -> Result<Copied, String> {
    let payload = std::str::from_utf8(payload)
        .map_err(|_| "[clipboard] the engine published something that is not UTF-8; ignored".to_string())?;
    let (source, text) =
        extract(payload).map_err(|e| format!("[clipboard] cannot use the engine's setClipboardText payload: {e}"))?;
    if text.len() > MAX_BYTES {
        return Err(format!("[clipboard] refusing {} bytes from the engine; the limit is {MAX_BYTES}", text.len()));
    }
    Ok(Copied { text, source })
}

/// The one slot between the engine's thread and the looper.
static PARKED: Mutex<Option<Copied>> = Mutex::new(None);
/// Set after the slot is filled, so the pump's check is one atomic swap.
static HAS_PARKED: AtomicBool = AtomicBool::new(false);

fn park(copied: Copied) {
    *PARKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(copied);
    HAS_PARKED.store(true, Ordering::Release);
}

fn take_parked() -> Option<Copied> {
    if !HAS_PARKED.swap(false, Ordering::AcqRel) {
        return None;
    }
    PARKED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The bus sink for [`MESSAGE_ID`]. Engine thread: parks, never blocks,
/// never touches X, never calls the engine.
extern "C" fn on_set_clipboard_text(payload: *const c_char) {
    // SAFETY: the bus hands a NUL-terminated string valid for the call, or null.
    let bytes = unsafe { bus::payload_bytes(payload) }.unwrap_or_default();
    match receive(bytes) {
        Ok(copied) => {
            if trace() {
                eprintln!("[clipboard] engine -> host: {} bytes from {}", copied.text.len(), copied.source);
            }
            park(copied);
            // So the copy reaches X now, not after the pump's next wait.
            super::looper::wake_pump();
        }
        Err(line) => println!("{line}"),
    }
}

/// `libroblox.so`, which the linker already holds: opening a loaded soname
/// hands back its handle.
fn engine() -> Result<linker::Library, String> {
    linker::dlopen("libroblox.so", linker::RTLD_NOW).map_err(|e| e.to_string())
}

static ARMED: AtomicBool = AtomicBool::new(false);
/// `Connection.isConnected`, or 0 when this build does not export it.
static IS_CONNECTED: OnceLock<usize> = OnceLock::new();

/// Subscribe to [`MESSAGE_ID`]. Called by the looper once, before the first
/// pump tick, when there is an engine; a second call does nothing.
pub fn arm() {
    if ARMED.swap(true, Ordering::AcqRel) {
        return;
    }
    let lib = match engine() {
        Ok(lib) => lib,
        Err(e) => {
            println!("[clipboard] cannot reach the engine's symbols: {e}");
            return;
        }
    };
    let Some(do_subscribe_raw) = lib.symbol(bus::DO_SUBSCRIBE_RAW) else {
        println!("[clipboard] doSubscribeRaw is not exported; copying out of Roblox will do nothing");
        return;
    };
    let sink: Option<bus::Sink> = if skip() {
        println!("[clipboard] bridge off ({SKIP_VARIABLE}); subscribing anyway, as a control");
        None
    } else {
        Some(on_set_clipboard_text)
    };
    if let Err(e) = bus::subscribe(do_subscribe_raw, MESSAGE_ID, sink) {
        println!("[clipboard] doSubscribeRaw failed: {e}");
        return;
    }
    let is_connected = lib.symbol(bus::IS_CONNECTED);
    let _ = IS_CONNECTED.set(is_connected.map_or(0, |f| f as usize));
    let verdict = if is_connected.is_none() {
        "; nothing here can confirm it (Connection.isConnected is not exported)"
    } else if bus::connection_ptr(MESSAGE_ID) == 0 {
        "; nothing here can confirm it (no Connection came back)"
    } else {
        match bus::connected(is_connected, MESSAGE_ID) {
            Some(true) => "; the bus says it is live",
            Some(false) => ", but the bus says the connection is not live",
            None => "; nothing here can confirm it (isConnected failed)",
        }
    };
    println!("[clipboard] subscribed to {MESSAGE_ID}{verdict}");
}

/// Whether the subscription is live, per `Connection.isConnected`; `None`
/// when that cannot be asked. Looper thread: it calls the engine.
pub fn subscription_live() -> Option<bool> {
    let f = *IS_CONNECTED.get()?;
    if f == 0 {
        return None;
    }
    bus::connected(Some(f as *mut c_void), MESSAGE_ID)
}

/// The host clipboard, or why there is none.
fn host_clipboard() -> Result<&'static X11Clipboard, String> {
    match x11_clipboard::current() {
        None => Err("no window is open yet, so there is no clipboard to reach".into()),
        Some(Err(e)) => Err(e),
        Some(Ok(cb)) => Ok(cb),
    }
}

/// One pump tick, on the looper thread: give up on a paste whose owner went
/// quiet, and put text the engine copied onto the clipboard. Two atomic or
/// uncontended operations when nothing is waiting.
pub fn pump_pending() {
    x11_clipboard::expire_read();
    let Some(copied) = take_parked() else { return };
    match host_clipboard().and_then(|cb| cb.set_text(&copied.text)) {
        Ok(()) => println!("[clipboard] {} bytes onto the host clipboard (from {})", copied.text.len(), copied.source),
        Err(e) => println!("[clipboard] could not reach the host clipboard: {e}"),
    }
}

fn probe_payload(text: &str) -> String {
    serde_json::json!({ "content": text }).to_string()
}

/// A scripted copy of `text`: publish a `setClipboardText` on the engine's
/// own bus, which comes back through the real subscription.
pub fn publish_probe(text: &str) -> Result<(), String> {
    let lib = engine().map_err(|e| format!("cannot reach the engine's symbols: {e}"))?;
    let publish_raw = lib.symbol(bus::PUBLISH_RAW).ok_or_else(|| "publishRaw is not exported".to_string())?;
    bus::publish(publish_raw, MESSAGE_ID, &probe_payload(text))
}

// ------------------------------------------------------------ the text box

/// What the bridge needs of the focused box and the host clipboard. [`Live`]
/// is the real one; the tests drive the same rules with their own.
trait Desk {
    /// The focused box and its focus generation.
    fn focused(&self) -> Option<(i64, u32)>;
    fn masked(&self) -> bool;
    fn selected_text(&mut self) -> Option<String>;
    fn edit(&mut self, edit: Edit<'_>) -> Option<FieldState>;
    fn deliver(&mut self, which: i64, state: &FieldState);
    fn put_on_clipboard(&mut self, text: &str) -> Result<(), String>;
}

/// The engine's focused box through `input.rs`, and the X11 clipboard.
struct Live {
    handle: i64,
}

impl Desk for Live {
    fn focused(&self) -> Option<(i64, u32)> {
        ga::focused_textbox().map(|which| (which, ga::textbox_generation()))
    }

    /// A box whose spec did not arrive is taken as unmasked.
    fn masked(&self) -> bool {
        ga::focused_textbox_info().is_some_and(|info| input::is_masked_input_type(info.text_input_type))
    }

    fn selected_text(&mut self) -> Option<String> {
        input::text_buffer_selected_text()
    }

    fn edit(&mut self, edit: Edit<'_>) -> Option<FieldState> {
        input::edit_text_field(edit)
    }

    fn deliver(&mut self, which: i64, state: &FieldState) {
        input::deliver_field_state(self.handle, which, state);
    }

    fn put_on_clipboard(&mut self, text: &str) -> Result<(), String> {
        host_clipboard()?.set_text(text)
    }
}

/// Every control character (Unicode Cc, which includes newline, carriage
/// return and tab) as one space.
fn flatten(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// Put pasted text into the box `which` at the caret, over the selection, and
/// tell the engine. Returns the characters inserted.
fn insert_paste(desk: &mut impl Desk, which: i64, text: &str) -> Result<usize, String> {
    if text.is_empty() {
        return Err("the host clipboard holds empty text".into());
    }
    let flat = flatten(text);
    let state = desk.edit(Edit::Insert(&flat)).ok_or_else(|| "the field refused the insert".to_string())?;
    desk.deliver(which, &state);
    Ok(flat.chars().count())
}

fn over_limit(n: usize) -> String {
    format!("the host clipboard holds {n} bytes and the limit is {MAX_BYTES}")
}

/// A paste of `text` already read: 0 when no box has focus.
fn paste_text(desk: &mut impl Desk, text: &str) -> Result<usize, String> {
    if text.len() > MAX_BYTES {
        return Err(over_limit(text.len()));
    }
    let Some((which, _)) = desk.focused() else { return Ok(0) };
    insert_paste(desk, which, text)
}

/// The answer to a Ctrl+V: `Ok(None)` when the box it was for is no longer
/// the focused one (blurred, or another box focused since).
fn finish_paste(desk: &mut impl Desk, paste: PasteFor, result: Result<String, String>) -> Result<Option<usize>, String> {
    let text = result?;
    if text.len() > MAX_BYTES {
        return Err(over_limit(text.len()));
    }
    match desk.focused() {
        Some((which, generation)) if which == paste.which && generation == paste.generation => {
            insert_paste(desk, which, &text).map(Some)
        }
        _ => Ok(None),
    }
}

/// Copy or cut the selection; the one line to print.
fn copy_or_cut(desk: &mut impl Desk, which: i64, cut: bool, skip: bool) -> String {
    let verb = if cut { "cut" } else { "copy" };
    if skip {
        return format!("[clipboard] {verb} not done: the clipboard bridge is off ({SKIP_VARIABLE})");
    }
    if desk.masked() {
        return format!("[clipboard] {verb} refused: the focused box masks its text");
    }
    let Some(selection) = desk.selected_text() else {
        return format!(
            "[clipboard] {verb}: nothing is selected, so the clipboard is unchanged (Ctrl+A selects the whole box)"
        );
    };
    let n = selection.len();
    if n > MAX_BYTES {
        return format!("[clipboard] {verb} refused: {n} bytes; the limit is {MAX_BYTES}");
    }
    if let Err(e) = desk.put_on_clipboard(&selection) {
        return format!("[clipboard] {verb} failed: {e}");
    }
    if cut {
        if let Some(state) = desk.edit(Edit::DeleteSelection) {
            desk.deliver(which, &state);
        }
    }
    format!("[clipboard] {verb}: {n} bytes onto the host clipboard")
}

fn select_all(desk: &mut impl Desk, which: i64) {
    if let Some(state) = desk.edit(Edit::SelectAll) {
        desk.deliver(which, &state);
    }
}

/// Ctrl+A, Ctrl+C, Ctrl+X or Ctrl+V (and their Insert/Delete forms) in the
/// focused box `which`, from `input.rs`. Looper thread.
pub fn run_text_shortcut(handle: i64, which: i64, shortcut: TextShortcut) {
    let mut desk = Live { handle };
    match shortcut {
        TextShortcut::SelectAll => select_all(&mut desk, which),
        TextShortcut::Copy => println!("{}", copy_or_cut(&mut desk, which, false, skip())),
        TextShortcut::Cut => println!("{}", copy_or_cut(&mut desk, which, true, skip())),
        TextShortcut::Paste => begin_paste(handle, which),
    }
}

/// Ctrl+V: ask the CLIPBOARD owner and return. Our own copy is answered at
/// once; another client's arrives through the pump's drain.
fn begin_paste(handle: i64, which: i64) {
    if skip() {
        println!("[clipboard] paste failed: the clipboard bridge is off ({SKIP_VARIABLE})");
        return;
    }
    let cb = match host_clipboard() {
        Ok(cb) => cb,
        Err(e) => {
            println!("[clipboard] paste failed: {e}");
            return;
        }
    };
    let paste = PasteFor { handle, which, generation: ga::textbox_generation() };
    match cb.begin_read(MAX_BYTES, PASTE_TIMEOUT, paste) {
        Ok(ReadStart::Now(text)) => paste_arrived(paste, Ok(text)),
        Ok(ReadStart::Pending) => {
            if trace() {
                eprintln!("[clipboard] paste asked of the CLIPBOARD owner; the answer comes through the pump");
            }
        }
        Err(e) => println!("[clipboard] paste failed: {e}"),
    }
}

/// Pastes the X side handed back, in the live test, which has no engine.
#[cfg(test)]
pub(crate) static TEST_PASTES: Mutex<Vec<Result<String, String>>> = Mutex::new(Vec::new());

/// The answer to a Ctrl+V, from `x11_clipboard` (the owner's reply, our own
/// copy, a refusal or the timeout). Looper thread.
pub fn paste_arrived(paste: PasteFor, result: Result<String, String>) {
    #[cfg(test)]
    TEST_PASTES.lock().unwrap_or_else(|e| e.into_inner()).push(result.clone());
    let mut desk = Live { handle: paste.handle };
    match finish_paste(&mut desk, paste, result) {
        Ok(Some(n)) => {
            if trace() {
                eprintln!("[clipboard] pasted {n} characters");
            }
        }
        Ok(None) => println!("[clipboard] paste dropped: the box it was for lost focus before the clipboard answered"),
        Err(e) => println!("[clipboard] paste failed: {e}"),
    }
}

/// A scripted paste: read the clipboard (waiting up to 400 ms, so
/// this one does block) and put it in the focused box. 0 when none has focus.
pub fn paste_into_engine(handle: i64) -> Result<usize, String> {
    if skip() {
        return Err(format!("the clipboard bridge is off ({SKIP_VARIABLE})"));
    }
    let text = host_clipboard()?.read_text(READ_TIMEOUT, MAX_BYTES)?;
    let n = paste_text(&mut Live { handle }, &text)?;
    if n > 0 && trace() {
        eprintln!("[clipboard] host -> engine: {n} characters into the focused box");
    }
    Ok(n)
}

#[cfg(test)]
mod tests;
