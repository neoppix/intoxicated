//! The X11 CLIPBOARD selection, on the connection `window.rs` already holds.
//!
//! **Why this exists rather than GDK.** `clipboard.rs` reached the host
//! clipboard through `gdk::Display::default()`, and on the X11 backend GTK is
//! never initialised -- `window.rs` opens a raw Xlib toplevel and nothing else
//! (ADR-024). So every paste and every copy failed with "GTK has no display
//! open", and on X11 that error went to a trace that is off by default: Ctrl+V
//! did nothing and said nothing, and text the engine asked to copy never left
//! the process. Opening GTK on X11 just for its clipboard would be a second
//! display connection and a second main loop for what ICCCM spells as a
//! handful of requests, so this speaks ICCCM directly.
//!
//! **Owning.** A copy stores the text here and claims `CLIPBOARD` with
//! `XSetSelectionOwner`; another client asking for it arrives as a
//! `SelectionRequest` in `window.rs`'s pump, answered from [`handle_event`].
//! Targets offered: `TARGETS`, `TIMESTAMP`, `UTF8_STRING`, `TEXT` (answered as
//! UTF-8) and `STRING` (Latin-1, refused for text that has no Latin-1 form
//! rather than sent lossy). `MULTIPLE` is not offered. Nothing larger than
//! `clipboard::MAX_BYTES` (64 KiB) is ever owned, and that fits a single
//! `ChangeProperty` request on any server (the core protocol's ceiling is
//! 256 KiB), so `INCR` is never needed on the sending side.
//!
//! **The copy dies with the process.** X has no clipboard storage of its own:
//! the text lives in whichever client owns the selection, and when Cordial
//! exits the clipboard is empty unless a clipboard manager took a copy. That is
//! how every X11 client behaves, and an i3 session usually runs no manager.
//!
//! **Reading.** `XConvertSelection(CLIPBOARD, UTF8_STRING)` onto a property on
//! Cordial's own window, then a bounded wait for the `SelectionNotify`, falling
//! back to `STRING` once if the owner refuses UTF-8. The wait is the same 400 ms
//! the GDK path uses and has the same shape: non-blocking checks
//! (`XCheckTypedWindowEvent`, which removes only the one event and leaves every
//! key and motion event queued for the pump in order) with `poll(2)` on the
//! connection between them. An owner that sends `INCR` is refused by size: an
//! owner only does that for a transfer far above the 64 KiB this will accept.
//!
//! Portable by construction, because the user this was written for runs a
//! native FreeBSD build: only Xlib (`libX11.so.6`, which is the soname on both
//! systems) and `poll(2)`.
//!
//! **Nothing here logs a clipboard value**, for the reasons `clipboard.rs`
//! gives.

use std::ffi::{c_char, c_int, c_long, c_uchar, c_ulong, c_void, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

type Display = *mut c_void;
type Window = c_ulong;
type Atom = c_ulong;

// X11 event `type` values, from X.h. Selection events are sent whatever the
// window's event mask, so `INPUT_EVENT_MASK` in `window.rs` needs no new bit.
pub const SELECTION_CLEAR: c_int = 29;
pub const SELECTION_REQUEST: c_int = 30;
pub const SELECTION_NOTIFY: c_int = 31;

// Predefined atoms, from Xatom.h. These never need interning.
const XA_ATOM: Atom = 4;
const XA_INTEGER: Atom = 19;
const XA_STRING: Atom = 31;

const CURRENT_TIME: c_ulong = 0;
const PROP_MODE_REPLACE: c_int = 0;
const ANY_PROPERTY_TYPE: Atom = 0;
const NONE: c_ulong = 0;

/// The atoms that have to be interned, once.
#[derive(Clone, Copy, Debug)]
pub struct Atoms {
    pub clipboard: Atom,
    pub targets: Atom,
    pub timestamp: Atom,
    pub utf8_string: Atom,
    pub text: Atom,
    pub incr: Atom,
    /// The property on Cordial's own window that a conversion is delivered
    /// into. Any name works; ICCCM asks only that it be one the requestor owns.
    pub transfer: Atom,
}

// ------------------------------------------------------------- pure helpers
//
// Target negotiation and property decoding take atoms as plain numbers, so they
// can be tested without an X server.

/// What to put in the requestor's property for one `SelectionRequest`.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// A format-32 property of type `ATOM`.
    Atoms(Vec<Atom>),
    /// A format-32 property of type `INTEGER`.
    Integer(c_ulong),
    /// A format-8 property of the given type.
    Bytes { kind: Atom, data: Vec<u8> },
    /// Send `SelectionNotify` with property `None`: this target is not served.
    Refuse(&'static str),
}

/// Text as ICCCM's `STRING`, which is Latin-1. `None` when some character has
/// no Latin-1 form: a requestor that asked for `STRING` gets a refusal and can
/// ask for `UTF8_STRING`, which is better than a silent `?` in its place.
pub fn to_latin1(text: &str) -> Option<Vec<u8>> {
    text.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect()
}

/// Decide the answer to a request for `target` while owning `text`, claimed
/// at `owned_at`.
pub fn answer_request(target: Atom, atoms: &Atoms, text: &str, owned_at: c_ulong) -> Answer {
    if target == atoms.targets {
        Answer::Atoms(vec![atoms.targets, atoms.timestamp, atoms.utf8_string, atoms.text, XA_STRING])
    } else if target == atoms.timestamp {
        Answer::Integer(owned_at)
    } else if target == atoms.utf8_string || target == atoms.text {
        // `TEXT` lets the owner pick the encoding; UTF-8 is the one that
        // carries everything, and the property's type says which was chosen.
        Answer::Bytes { kind: atoms.utf8_string, data: text.as_bytes().to_vec() }
    } else if target == XA_STRING {
        match to_latin1(text) {
            Some(data) => Answer::Bytes { kind: XA_STRING, data },
            None => Answer::Refuse("the text has characters STRING (Latin-1) cannot carry"),
        }
    } else {
        Answer::Refuse("target not offered")
    }
}

/// Turn what `XGetWindowProperty` returned for a conversion into text.
///
/// `bytes_after` is what the server still held past what was read; anything
/// left over means the value is over `max_bytes`, since the read asked for
/// slightly more than that.
pub fn decode_property(
    actual_type: Atom,
    format: c_int,
    data: &[u8],
    bytes_after: c_ulong,
    atoms: &Atoms,
    max_bytes: usize,
) -> Result<String, String> {
    if actual_type == NONE {
        return Err("the clipboard owner answered but left no data".into());
    }
    if actual_type == atoms.incr {
        return Err(format!(
            "the clipboard owner chose an incremental (INCR) transfer, which owners use only \
             for large values; the limit here is {max_bytes} bytes"
        ));
    }
    if format != 8 {
        return Err(format!("the clipboard owner answered in {format}-bit units, not text"));
    }
    let total = data.len() as u64 + bytes_after as u64;
    if total > max_bytes as u64 || bytes_after > 0 {
        return Err(format!("the host clipboard holds {total} bytes and the limit is {max_bytes}"));
    }
    if actual_type == atoms.utf8_string {
        String::from_utf8(data.to_vec())
            .map_err(|_| "the clipboard owner said UTF-8 and sent something that is not".into())
    } else if actual_type == XA_STRING {
        Ok(data.iter().map(|&b| char::from(b)).collect())
    } else {
        Err("the clipboard owner answered with a text type this does not read".into())
    }
}

// --------------------------------------------------------------------- Xlib

struct Fns {
    intern_atom: unsafe extern "C" fn(Display, *const c_char, c_int) -> Atom,
    set_selection_owner: unsafe extern "C" fn(Display, Atom, Window, c_ulong) -> c_int,
    get_selection_owner: unsafe extern "C" fn(Display, Atom) -> Window,
    convert_selection: unsafe extern "C" fn(Display, Atom, Atom, Atom, Window, c_ulong) -> c_int,
    get_window_property: unsafe extern "C" fn(
        Display, Window, Atom, c_long, c_long, c_int, Atom,
        *mut Atom, *mut c_int, *mut c_ulong, *mut c_ulong, *mut *mut c_uchar,
    ) -> c_int,
    change_property:
        unsafe extern "C" fn(Display, Window, Atom, Atom, c_int, c_int, *const c_uchar, c_int) -> c_int,
    delete_property: unsafe extern "C" fn(Display, Window, Atom) -> c_int,
    send_event: unsafe extern "C" fn(Display, Window, c_int, c_long, *mut c_void) -> c_int,
    check_typed_window_event: unsafe extern "C" fn(Display, Window, c_int, *mut c_void) -> c_int,
    flush: unsafe extern "C" fn(Display) -> c_int,
    free: unsafe extern "C" fn(*mut c_void) -> c_int,
}

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    // Same signature as `window.rs`'s declaration of the same symbol, which
    // `clashing_extern_declarations` requires.
    fn poll(fds: *mut c_void, nfds: c_ulong, timeout_ms: c_int) -> c_int;
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
const POLLIN: i16 = 0x001;

impl Fns {
    fn load() -> Result<Self, String> {
        // The same soname `window.rs` opens; the dynamic loader hands back the
        // object it already has, so this is not a second copy of Xlib.
        // SAFETY: a literal soname; the handle is never closed.
        let lib = unsafe { dlopen(c"libX11.so.6".as_ptr(), 2 /* RTLD_NOW */) };
        if lib.is_null() {
            return Err("libX11.so.6 is not available".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                let name = CString::new($name).unwrap();
                // SAFETY: the handle is open and these are Xlib's documented
                // exports, with the signatures declared above.
                let p = unsafe { dlsym(lib, name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("libX11 has no {}", $name));
                }
                unsafe { std::mem::transmute(p) }
            }};
        }
        Ok(Fns {
            intern_atom: sym!("XInternAtom"),
            set_selection_owner: sym!("XSetSelectionOwner"),
            get_selection_owner: sym!("XGetSelectionOwner"),
            convert_selection: sym!("XConvertSelection"),
            get_window_property: sym!("XGetWindowProperty"),
            change_property: sym!("XChangeProperty"),
            delete_property: sym!("XDeleteProperty"),
            send_event: sym!("XSendEvent"),
            check_typed_window_event: sym!("XCheckTypedWindowEvent"),
            flush: sym!("XFlush"),
            free: sym!("XFree"),
        })
    }
}

/// `XSelectionRequestEvent`.
#[repr(C)]
struct XSelectionRequestEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: Display,
    owner: Window,
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: c_ulong,
}

/// `XSelectionEvent`, which is what `SelectionNotify` carries.
#[repr(C)]
struct XSelectionEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: Display,
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: c_ulong,
}

/// `XSelectionClearEvent`.
#[repr(C)]
struct XSelectionClearEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: Display,
    window: Window,
    selection: Atom,
    time: c_ulong,
}

/// Room for any `XEvent`: the union is 24 longs, and this is aligned for the
/// structs above to be written into it.
type EventBuf = [c_long; 32];

struct Owned {
    text: String,
    time: c_ulong,
}

pub struct X11Clipboard {
    x: Fns,
    display: Display,
    window: Window,
    conn_fd: c_int,
    atoms: Atoms,
    owned: Mutex<Option<Owned>>,
}

// Only ever touched from the looper thread, the one that also drains the X
// connection in `window.rs`; the same contract `HostWindow` states.
unsafe impl Send for X11Clipboard {}
unsafe impl Sync for X11Clipboard {}

static CLIPBOARD: OnceLock<Result<X11Clipboard, String>> = OnceLock::new();

/// The X server time of the last key press Cordial saw.
///
/// ICCCM asks for a real timestamp on `SetSelectionOwner` and
/// `ConvertSelection` rather than `CurrentTime`, so that a slow request cannot
/// take a selection somebody else claimed after the person's own action. The
/// key press that caused the copy is that action. Copies the engine makes after
/// a click have no key to point at, so [`X11Clipboard::set_text`] retries with
/// `CurrentTime` when the dated claim is refused.
static USER_TIME: AtomicU64 = AtomicU64::new(0);

pub fn note_user_time(time: c_ulong) {
    if time != 0 {
        USER_TIME.store(time as u64, Ordering::Relaxed);
    }
}

fn user_time() -> c_ulong {
    USER_TIME.load(Ordering::Relaxed) as c_ulong
}

/// The X11 clipboard, when the X11 backend has a window open.
///
/// `None` means this is not the X11 backend (or no window exists yet), and the
/// caller should use GDK. `Some(Err)` means it is X11 and the clipboard cannot
/// work, which the caller must report rather than fall through to GDK -- GDK
/// has no display on this backend, and its error would name the wrong cause.
pub fn current() -> Option<Result<&'static X11Clipboard, String>> {
    let window = super::window::current()?;
    let made = CLIPBOARD.get_or_init(|| {
        let x = Fns::load()?;
        let display = window.egl_native_display();
        let intern = |name: &str| -> Atom {
            let c = CString::new(name).unwrap_or_default();
            // SAFETY: `display` is the open connection `window.rs` owns.
            unsafe { (x.intern_atom)(display, c.as_ptr(), 0) }
        };
        let atoms = Atoms {
            clipboard: intern("CLIPBOARD"),
            targets: intern("TARGETS"),
            timestamp: intern("TIMESTAMP"),
            utf8_string: intern("UTF8_STRING"),
            text: intern("TEXT"),
            incr: intern("INCR"),
            transfer: intern("CORDIAL_CLIPBOARD"),
        };
        if atoms.clipboard == NONE || atoms.utf8_string == NONE || atoms.transfer == NONE {
            return Err("the X server would not intern the clipboard atoms".into());
        }
        Ok(X11Clipboard {
            x,
            display,
            window: window.egl_native_window(),
            conn_fd: window.connection_fd(),
            atoms,
            owned: Mutex::new(None),
        })
    });
    Some(made.as_ref().map_err(Clone::clone))
}

/// Answer a selection event the pump drained. `buf` is the raw `XEvent`.
pub fn handle_event(buf: &[u8; 256]) {
    let Some(Ok(cb)) = current() else { return };
    let kind = c_int::from_ne_bytes(buf[..4].try_into().expect("four bytes"));
    // SAFETY: the event type says which member of Xlib's `XEvent` union
    // `XNextEvent` filled; the structs above are those members' layouts, and
    // an unaligned read copies them out of the byte buffer safely.
    match kind {
        SELECTION_REQUEST => {
            let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const XSelectionRequestEvent) };
            cb.answer(&ev);
        }
        SELECTION_CLEAR => {
            let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const XSelectionClearEvent) };
            if ev.selection == cb.atoms.clipboard {
                // Somebody else copied. What this process held is no longer
                // the clipboard, and answering for it later would be wrong.
                *cb.owned.lock().unwrap_or_else(|e| e.into_inner()) = None;
                if super::input::trace_text() || trace() {
                    eprintln!("[clipboard] X11: another client took the CLIPBOARD");
                }
            }
        }
        _ => {}
    }
}

fn trace() -> bool {
    std::env::var_os("CORDIAL_TRACE_CLIPBOARD").is_some()
}

impl X11Clipboard {
    /// Claim `CLIPBOARD` with `text`. Fails, rather than reporting success,
    /// when the server did not make Cordial the owner.
    pub fn set_text(&self, text: &str) -> Result<(), String> {
        let dated = user_time();
        let times: &[c_ulong] = if dated != 0 { &[dated, CURRENT_TIME] } else { &[CURRENT_TIME] };
        for &time in times {
            *self.owned.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Owned { text: text.to_owned(), time });
            // SAFETY: the display and window are the open ones `window.rs`
            // owns; every call here is on the looper thread.
            let owner = unsafe {
                (self.x.set_selection_owner)(self.display, self.atoms.clipboard, self.window, time);
                (self.x.get_selection_owner)(self.display, self.atoms.clipboard)
            };
            if owner == self.window {
                return Ok(());
            }
        }
        *self.owned.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Err("the X server did not make Cordial the CLIPBOARD owner".into())
    }

    /// Read `CLIPBOARD` as text, waiting at most `timeout` in total.
    pub fn read_text(&self, timeout: std::time::Duration, max_bytes: usize) -> Result<String, String> {
        // SAFETY: as in `set_text`.
        let owner = unsafe { (self.x.get_selection_owner)(self.display, self.atoms.clipboard) };
        if owner == NONE {
            return Err(
                "nothing owns the X11 CLIPBOARD: nothing was copied, or the program that \
                 copied it has exited"
                    .into(),
            );
        }
        if owner == self.window {
            // Cordial's own copy. A conversion would be a request to itself,
            // which this thread could only answer by returning to the pump.
            return match &*self.owned.lock().unwrap_or_else(|e| e.into_inner()) {
                Some(o) => Ok(o.text.clone()),
                None => Err("Cordial owns the CLIPBOARD but holds no text".into()),
            };
        }

        let deadline = std::time::Instant::now() + timeout;
        for target in [self.atoms.utf8_string, XA_STRING] {
            if let Some(text) = self.convert(target, deadline, timeout, max_bytes)? {
                return Ok(text);
            }
        }
        Err("the clipboard owner refused both UTF8_STRING and STRING; it holds no text".into())
    }

    /// One `ConvertSelection` and its answer. `Ok(None)` is a refusal of this
    /// target, which the caller may follow with another; `Err` ends the paste.
    fn convert(
        &self,
        target: Atom,
        deadline: std::time::Instant,
        budget: std::time::Duration,
        max_bytes: usize,
    ) -> Result<Option<String>, String> {
        // SAFETY: as in `set_text`.
        unsafe {
            (self.x.delete_property)(self.display, self.window, self.atoms.transfer);
            (self.x.convert_selection)(
                self.display,
                self.atoms.clipboard,
                target,
                self.atoms.transfer,
                self.window,
                user_time(),
            );
            (self.x.flush)(self.display);
        }
        let notify = loop {
            let mut buf: EventBuf = [0; 32];
            // SAFETY: `buf` is large enough for any `XEvent`. This removes the
            // one matching event and leaves the rest queued for the pump.
            let got = unsafe {
                (self.x.check_typed_window_event)(
                    self.display,
                    self.window,
                    SELECTION_NOTIFY,
                    buf.as_mut_ptr() as *mut c_void,
                )
            };
            if got != 0 {
                // SAFETY: `SelectionNotify` fills the `XSelectionEvent` member.
                let ev = unsafe { std::ptr::read(buf.as_ptr() as *const XSelectionEvent) };
                // A late answer to an earlier request that timed out is not
                // this one's.
                if ev.selection == self.atoms.clipboard && ev.target == target {
                    break ev;
                }
                continue;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "the clipboard owner did not answer within {} ms",
                    budget.as_millis()
                ));
            }
            let mut pfd = PollFd { fd: self.conn_fd, events: POLLIN, revents: 0 };
            let wait = left.as_millis().clamp(1, 10) as c_int;
            // SAFETY: one live `PollFd`.
            unsafe { poll(&mut pfd as *mut PollFd as *mut c_void, 1, wait) };
        };
        if notify.property == NONE {
            return Ok(None);
        }

        let mut actual_type: Atom = 0;
        let mut format: c_int = 0;
        let mut nitems: c_ulong = 0;
        let mut after: c_ulong = 0;
        let mut data: *mut c_uchar = std::ptr::null_mut();
        // Ask for one long more than the limit, so an oversized value shows up
        // as `bytes_after` rather than being silently cut short.
        let longs = (max_bytes / 4 + 1) as c_long;
        // SAFETY: every out-pointer is live; `data` is freed below.
        let rc = unsafe {
            (self.x.get_window_property)(
                self.display,
                self.window,
                self.atoms.transfer,
                0,
                longs,
                1, // delete: the property is the transfer, not storage
                ANY_PROPERTY_TYPE,
                &mut actual_type,
                &mut format,
                &mut nitems,
                &mut after,
                &mut data,
            )
        };
        if rc != 0 {
            return Err(format!("XGetWindowProperty failed ({rc})"));
        }
        let bytes = if data.is_null() || format != 8 {
            Vec::new()
        } else {
            // SAFETY: Xlib returned `nitems` format-8 items at `data`.
            unsafe { std::slice::from_raw_parts(data, nitems as usize) }.to_vec()
        };
        if !data.is_null() {
            // SAFETY: allocated by Xlib for this call.
            unsafe { (self.x.free)(data as *mut c_void) };
        }
        decode_property(actual_type, format, &bytes, after, &self.atoms, max_bytes).map(Some)
    }

    fn answer(&self, req: &XSelectionRequestEvent) {
        // Obsolete clients send property `None` and mean "use the target".
        let property = if req.property == NONE { req.target } else { req.property };
        let answer = if req.selection != self.atoms.clipboard {
            Answer::Refuse("not the CLIPBOARD")
        } else {
            match &*self.owned.lock().unwrap_or_else(|e| e.into_inner()) {
                Some(o) => answer_request(req.target, &self.atoms, &o.text, o.time),
                None => Answer::Refuse("Cordial holds no text"),
            }
        };
        // SAFETY: the requestor's window id and atoms came from the server;
        // a bad window raises an asynchronous X error, not memory unsafety.
        let served = unsafe {
            match &answer {
                Answer::Atoms(list) => {
                    (self.x.change_property)(
                        self.display, req.requestor, property, XA_ATOM, 32, PROP_MODE_REPLACE,
                        list.as_ptr() as *const c_uchar, list.len() as c_int,
                    );
                    true
                }
                Answer::Integer(v) => {
                    let v: c_ulong = *v;
                    (self.x.change_property)(
                        self.display, req.requestor, property, XA_INTEGER, 32, PROP_MODE_REPLACE,
                        &v as *const c_ulong as *const c_uchar, 1,
                    );
                    true
                }
                Answer::Bytes { kind, data } => {
                    (self.x.change_property)(
                        self.display, req.requestor, property, *kind, 8, PROP_MODE_REPLACE,
                        data.as_ptr(), data.len() as c_int,
                    );
                    true
                }
                Answer::Refuse(_) => false,
            }
        };
        if trace() {
            match &answer {
                Answer::Refuse(why) => eprintln!("[clipboard] X11: refused a request: {why}"),
                Answer::Bytes { data, .. } => {
                    eprintln!("[clipboard] X11: served {} bytes to another client", data.len())
                }
                _ => {}
            }
        }
        let notify = XSelectionEvent {
            type_: SELECTION_NOTIFY,
            serial: 0,
            send_event: 1,
            display: self.display,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: if served { property } else { NONE },
            time: req.time,
        };
        let mut buf: EventBuf = [0; 32];
        // SAFETY: `buf` is larger than and aligned for `XSelectionEvent`; Xlib
        // reads a whole `XEvent` from it.
        unsafe {
            std::ptr::write(buf.as_mut_ptr() as *mut XSelectionEvent, notify);
            (self.x.send_event)(self.display, req.requestor, 0, 0, buf.as_mut_ptr() as *mut c_void);
            (self.x.flush)(self.display);
        }
    }
}

#[cfg(test)]
impl X11Clipboard {
    /// Answer whatever selection events are queued, the way `window.rs`'s pump
    /// would, for the live test below, which has no engine and so no pump.
    fn serve_queued(&self) {
        for kind in [SELECTION_REQUEST, SELECTION_CLEAR] {
            loop {
                let mut buf = [0u8; 256];
                // SAFETY: as in `convert`.
                let got = unsafe {
                    (self.x.check_typed_window_event)(
                        self.display, self.window, kind, buf.as_mut_ptr() as *mut c_void,
                    )
                };
                if got == 0 {
                    break;
                }
                handle_event(&buf);
            }
        }
    }

    fn holds_text(&self) -> bool {
        self.owned.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms() -> Atoms {
        Atoms {
            clipboard: 100,
            targets: 101,
            timestamp: 102,
            utf8_string: 103,
            text: 104,
            incr: 105,
            transfer: 106,
        }
    }

    #[test]
    fn targets_lists_what_is_served() {
        let a = atoms();
        assert_eq!(
            answer_request(a.targets, &a, "hi", 7),
            Answer::Atoms(vec![a.targets, a.timestamp, a.utf8_string, a.text, XA_STRING])
        );
        assert_eq!(answer_request(a.timestamp, &a, "hi", 7), Answer::Integer(7));
    }

    #[test]
    fn utf8_and_text_are_served_as_utf8() {
        let a = atoms();
        let want = Answer::Bytes { kind: a.utf8_string, data: "naïve ✓".as_bytes().to_vec() };
        assert_eq!(answer_request(a.utf8_string, &a, "naïve ✓", 0), want);
        assert_eq!(answer_request(a.text, &a, "naïve ✓", 0), want);
    }

    /// STRING is Latin-1. A character outside it is a refusal, not a `?`.
    #[test]
    fn string_is_latin1_or_refused() {
        let a = atoms();
        assert_eq!(
            answer_request(XA_STRING, &a, "café", 0),
            Answer::Bytes { kind: XA_STRING, data: vec![b'c', b'a', b'f', 0xe9] }
        );
        assert!(matches!(answer_request(XA_STRING, &a, "✓", 0), Answer::Refuse(_)));
    }

    #[test]
    fn unknown_targets_are_refused() {
        let a = atoms();
        assert!(matches!(answer_request(999, &a, "x", 0), Answer::Refuse(_)));
    }

    #[test]
    fn utf8_property_decodes() {
        let a = atoms();
        let t = decode_property(a.utf8_string, 8, "日本".as_bytes(), 0, &a, 64).unwrap();
        assert_eq!(t, "日本");
    }

    #[test]
    fn string_property_decodes_as_latin1() {
        let a = atoms();
        assert_eq!(decode_property(XA_STRING, 8, &[b'c', 0xe9], 0, &a, 64).unwrap(), "cé");
    }

    #[test]
    fn incr_and_oversize_are_refused_with_the_limit_named() {
        let a = atoms();
        let e = decode_property(a.incr, 32, &[], 0, &a, 64).unwrap_err();
        assert!(e.contains("INCR") && e.contains("64"), "{e}");
        let e = decode_property(a.utf8_string, 8, &[b'x'; 64], 4, &a, 64).unwrap_err();
        assert!(e.contains("68 bytes"), "{e}");
    }

    #[test]
    fn bad_utf8_and_odd_formats_fail_without_quoting_the_data() {
        let a = atoms();
        let e = decode_property(a.utf8_string, 8, &[0xff, 0xfe], 0, &a, 64).unwrap_err();
        assert!(e.contains("not"), "{e}");
        assert!(decode_property(a.utf8_string, 32, &[], 0, &a, 64).is_err());
        assert!(decode_property(NONE, 0, &[], 0, &a, 64).is_err());
        assert!(decode_property(a.targets, 8, b"hunter2", 0, &a, 64)
            .unwrap_err()
            .find("hunter2")
            .is_none());
    }

    /// Both directions against a real X server and a real other client.
    ///
    /// Ignored, and refuses to run unless `CORDIAL_CLIPBOARD_LIVE_TEST=1` is set
    /// and `DISPLAY` is not `:0`, because it takes the display's clipboard and
    /// would throw away whatever the person at that desk had copied. Point it
    /// at a private server:
    ///
    /// ```text
    /// Xvfb :97 -nolisten tcp &
    /// DISPLAY=:97 CORDIAL_CLIPBOARD_LIVE_TEST=1 cargo test -p cordial-runtime \
    ///   --release -- --ignored --test-threads=1 x11_clipboard_round_trips
    /// ```
    ///
    /// Needs `xclip`. Unlike the GDK test in `clipboard.rs`, nothing here can
    /// pass by answering itself: every read is made by another process.
    #[test]
    #[ignore = "takes an X display's clipboard"]
    fn x11_clipboard_round_trips_with_another_client() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if std::env::var_os("CORDIAL_CLIPBOARD_LIVE_TEST").is_none() {
            panic!("refusing to take a clipboard without CORDIAL_CLIPBOARD_LIVE_TEST=1");
        }
        let display = std::env::var("DISPLAY").unwrap_or_default();
        assert!(display != ":0" && !display.is_empty(), "refusing DISPLAY={display:?}");

        super::super::window::open(64, 64, "clipboard probe").expect("an X display");
        let cb = current().expect("the X11 backend").expect("a clipboard");

        // Cordial -> another client, as UTF8_STRING.
        let sent = "cordial→xclip ✓ naïve";
        cb.set_text(sent).expect("ownership");
        let read = |target: &str| {
            let mut child = Command::new("xclip")
                .args(["-o", "-selection", "clipboard", "-t", target])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("xclip");
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while child.try_wait().unwrap().is_none() && std::time::Instant::now() < until {
                cb.serve_queued();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let out = child.wait_with_output().unwrap();
            (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let (ok, got) = read("UTF8_STRING");
        println!("xclip read UTF8_STRING: ok={ok} {} bytes", got.len());
        assert!(ok && got == sent, "xclip read {got:?}");

        let (ok, got) = read("TARGETS");
        println!("xclip read TARGETS: {:?}", got.split_whitespace().collect::<Vec<_>>());
        assert!(ok && got.contains("UTF8_STRING") && got.contains("STRING"), "{got:?}");

        // STRING for text Latin-1 cannot carry is refused, not mangled.
        let (ok, got) = read("STRING");
        println!("xclip read STRING of non-Latin-1 text: ok={ok} {} bytes", got.len());
        assert!(!ok || got.is_empty(), "a lossy STRING was served: {got:?}");

        // Another client -> Cordial. `xclip -i` forks and owns the selection
        // until somebody else takes it, which the last step does.
        let pasted = "from xclip: Grüße ✓";
        let mut child = Command::new("xclip")
            .args(["-i", "-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("xclip");
        child.stdin.take().unwrap().write_all(pasted.as_bytes()).unwrap();
        child.wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        cb.serve_queued();
        assert!(!cb.holds_text(), "SelectionClear did not drop Cordial's copy");
        let t0 = std::time::Instant::now();
        let got = cb
            .read_text(std::time::Duration::from_millis(400), 64 * 1024)
            .expect("a paste from xclip");
        println!("read from xclip: {} bytes in {:?}", got.len(), t0.elapsed());
        assert_eq!(got, pasted);

        // Over the limit: refused with the limit named, never truncated.
        let mut child = Command::new("xclip")
            .args(["-i", "-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("xclip");
        child.stdin.take().unwrap().write_all(&vec![b'x'; 70 * 1024]).unwrap();
        child.wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let e = cb.read_text(std::time::Duration::from_millis(400), 64 * 1024).unwrap_err();
        println!("70 KiB paste: {e}");
        assert!(e.contains("65536"), "{e}");

        // Take it back, which also lets the forked xclip exit, and read our own
        // copy without a round trip to ourselves.
        cb.set_text("back").expect("ownership");
        assert_eq!(cb.read_text(std::time::Duration::from_millis(400), 64).unwrap(), "back");
    }
}
