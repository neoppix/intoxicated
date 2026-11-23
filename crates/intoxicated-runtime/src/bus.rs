//! The engine's message bus: one native subscription per message id, shared by
//! every listener in this process, and the request handlers.
//!
//! `native/message_bus.cpp`'s `intoxicated_messagebus_subscribe` keeps one
//! callback per message id, so a second subscribe on an id replaces the first
//! callback and the first listener goes deaf without being told. The join
//! link's `Game.launch` subscription and devctl's `bussub Game.launch` would
//! collide exactly that way, and so would `bussub` on the clipboard's topic.
//!
//! So nothing in Rust calls that native directly except [`subscribe`] here. It
//! subscribes each message id once, with a trampoline that knows its own slot,
//! and the trampoline hands every payload to all the sinks registered for that
//! id. Adding a sink to an id that is already subscribed makes no engine call.
//! Request handlers (`Linking.openURL`) are bound through
//! [`set_request_handler`], so every bus native has its one declaration here.
//!
//! Delivery takes no lock. The bus calls back on whichever thread published,
//! sometimes from inside a `publishRaw` or `doSubscribeRaw` made by the very
//! thread that holds the registration lock, so the delivery path reads only
//! atomics. A sink must be quick, must not block and must not unwind.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Receives one payload: a NUL-terminated JSON text the bus owns for the
/// duration of the call, or null.
pub type Sink = extern "C" fn(*const c_char);

/// Answers one request: the request (NUL-terminated, valid for the call), a
/// zeroed buffer for the answer and its length. Returns non-zero when it wrote
/// a NUL-terminated answer; otherwise the engine is told `{"success":false}`.
pub type RequestSink = extern "C" fn(*const c_char, *mut c_char, usize) -> c_int;

/// The JNI class every bus native is called on.
pub const MESSAGE_BUS_CLASS: &str = "com/roblox/universalapp/messagebus/MessageBus";
pub const PUBLISH_RAW: &str = "Java_com_roblox_universalapp_messagebus_MessageBus_publishRaw";
pub const DO_SUBSCRIBE_RAW: &str = "Java_com_roblox_universalapp_messagebus_MessageBus_doSubscribeRaw";
pub const GET_MESSAGE_ID: &str = "Java_com_roblox_universalapp_messagebus_MessageBus_getMessageId";
pub const GET_LAST_RAW: &str = "Java_com_roblox_universalapp_messagebus_MessageBus_getLastRaw";
pub const IS_CONNECTED: &str = "Java_com_roblox_universalapp_messagebus_Connection_isConnected";
pub const SET_REQUEST_HANDLER_RAW: &str = "Java_com_roblox_universalapp_messagebus_MessageBus_setRequestHandlerRaw";

/// Message ids this process can subscribe to. The client itself uses three
/// (`Game.launch`, `WebView.openWindow`, the clipboard's); the rest are room
/// for devctl's `bussub`.
pub const TOPICS: usize = 16;

/// Sinks one message id can have.
pub const SINKS_PER_TOPIC: usize = 8;

// `native/message_bus.cpp`.
unsafe extern "C" {
    fn intoxicated_messagebus_subscribe(
        f: *mut c_void,
        message_id: *const c_char,
        sink: Option<Sink>,
        err: *mut c_char,
        n: usize,
    ) -> c_int;
    fn intoxicated_messagebus_connection_value(message_id: *const c_char) -> i64;
    fn intoxicated_messagebus_is_connected(
        f: *mut c_void,
        value: i64,
        out_connected: *mut c_int,
        err: *mut c_char,
        n: usize,
    ) -> c_int;
    fn intoxicated_messagebus_set_request_handler(
        f: *mut c_void,
        protocol: *const c_char,
        method: *const c_char,
        sink: RequestSink,
        err: *mut c_char,
        n: usize,
    ) -> c_int;
}

/// One message id's sinks, as function addresses; 0 is an empty place.
struct Slot {
    sinks: [AtomicUsize; SINKS_PER_TOPIC],
}

impl Slot {
    const fn new() -> Self {
        Slot { sinks: [const { AtomicUsize::new(0) }; SINKS_PER_TOPIC] }
    }
}

static SLOTS: [Slot; TOPICS] = [const { Slot::new() }; TOPICS];

/// Which message id owns which slot, and whether the engine has it. Index is
/// the slot. Never touched on the delivery path.
struct Topic {
    id: String,
    subscribed: bool,
}

static REGISTRY: Mutex<Vec<Topic>> = Mutex::new(Vec::new());

fn fan_out(slot: usize, payload: *const c_char) {
    for place in &SLOTS[slot].sinks {
        let address = place.load(Ordering::Acquire);
        if address != 0 {
            // SAFETY: only `Sink` addresses are ever stored in a slot.
            let sink = unsafe { std::mem::transmute::<usize, Sink>(address) };
            sink(payload);
        }
    }
}

macro_rules! trampolines {
    ($($slot:literal)*) => {
        [$({
            extern "C" fn trampoline(payload: *const c_char) {
                fan_out($slot, payload)
            }
            trampoline as Sink
        },)*]
    };
}

/// The callback the engine is given for each slot.
static TRAMPOLINES: [Sink; TOPICS] = trampolines!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15);

/// What a [`subscribe`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Joined {
    /// The engine was asked to subscribe this message id, and agreed.
    Subscribed,
    /// The id was already subscribed; the sink was added beside the others.
    Added,
    /// The id was already subscribed and this sink was already on it.
    AlreadyListening,
}

/// Adds `sink` to `slot`: `Ok(true)` when it was added, `Ok(false)` when it
/// was already there.
fn add_sink(slot: usize, sink: Sink) -> Result<bool, String> {
    let address = sink as usize;
    let places = &SLOTS[slot].sinks;
    if places.iter().any(|p| p.load(Ordering::Acquire) == address) {
        return Ok(false);
    }
    // Only ever called with the registry lock held, so no other thread is
    // filling a place at the same time.
    match places.iter().find(|p| p.load(Ordering::Acquire) == 0) {
        Some(place) => {
            place.store(address, Ordering::Release);
            Ok(true)
        }
        None => Err(format!("this message id already has {SINKS_PER_TOPIC} listeners, the most there is room for")),
    }
}

fn remove_sink(slot: usize, sink: Sink) {
    let address = sink as usize;
    for place in &SLOTS[slot].sinks {
        let _ = place.compare_exchange(address, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// Subscribes `sink` to `message_id` through `doSubscribeRaw` (the native at
/// `do_subscribe_raw`), or adds it beside the sinks already there.
///
/// `sink` may be `None`: the id is then subscribed with nobody listening,
/// which is what the clipboard's control run asks for.
pub fn subscribe(do_subscribe_raw: *mut c_void, message_id: &str, sink: Option<Sink>) -> Result<Joined, String> {
    subscribe_with(message_id, sink, |trampoline| native_subscribe(do_subscribe_raw, message_id, trampoline))
}

/// [`subscribe`] with the engine call supplied by the caller, so the fan-out
/// can be tested without an engine.
pub(crate) fn subscribe_with(
    message_id: &str,
    sink: Option<Sink>,
    engine_subscribe: impl FnOnce(Sink) -> Result<(), String>,
) -> Result<Joined, String> {
    if message_id.is_empty() {
        return Err("the message id is empty".into());
    }
    if message_id.contains('\0') {
        return Err("the message id has a NUL in it".into());
    }
    let mut registry = REGISTRY.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let slot = match registry.iter().position(|t| t.id == message_id) {
        Some(slot) => slot,
        None if registry.len() == TOPICS => {
            return Err(format!("{TOPICS} message ids are subscribed already, the most there is room for"));
        }
        None => {
            registry.push(Topic { id: message_id.to_string(), subscribed: false });
            registry.len() - 1
        }
    };
    let added = match sink {
        Some(sink) => add_sink(slot, sink)?,
        None => false,
    };
    if registry[slot].subscribed {
        return Ok(if added { Joined::Added } else { Joined::AlreadyListening });
    }
    // The sink is in place before the engine is asked, because the bus may
    // deliver from inside the subscribing call. The lock stays held so a second
    // caller for the same id cannot subscribe it twice; delivery never takes it.
    match engine_subscribe(TRAMPOLINES[slot]) {
        Ok(()) => {
            registry[slot].subscribed = true;
            Ok(Joined::Subscribed)
        }
        Err(e) => {
            if let (true, Some(sink)) = (added, sink) {
                remove_sink(slot, sink);
            }
            Err(e)
        }
    }
}

fn native_subscribe(do_subscribe_raw: *mut c_void, message_id: &str, trampoline: Sink) -> Result<(), String> {
    let id = CString::new(message_id).map_err(|_| "the message id has a NUL in it".to_string())?;
    let mut err = [0u8; 512];
    // SAFETY: `id` and `err` outlive the call; the C side checks `f` and keeps
    // the callback object alive for the rest of the process.
    let rc = unsafe {
        intoxicated_messagebus_subscribe(do_subscribe_raw, id.as_ptr(), Some(trampoline), err.as_mut_ptr().cast(), err.len())
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(text_before_nul(&err))
    }
}

/// The address inside the `Connection` the engine returned for `message_id`,
/// or 0 when none came back.
pub fn connection_ptr(message_id: &str) -> i64 {
    let Ok(id) = CString::new(message_id) else { return 0 };
    // SAFETY: reads a value the C side stored under this id.
    unsafe { intoxicated_messagebus_connection_value(id.as_ptr()) }
}

/// What `Connection.isConnected` says about `message_id`'s subscription:
/// `None` when there is nothing to ask (the native is missing, no
/// `Connection` came back, or the call failed).
pub fn connected(is_connected: Option<*mut c_void>, message_id: &str) -> Option<bool> {
    let f = is_connected?;
    let ptr = connection_ptr(message_id);
    if ptr == 0 {
        return None;
    }
    let mut out: c_int = -1;
    let mut err = [0u8; 256];
    // SAFETY: the native resolved under its own name; the buffers outlive the call.
    let rc = unsafe { intoxicated_messagebus_is_connected(f, ptr, &mut out, err.as_mut_ptr().cast(), err.len()) };
    (rc == 0).then_some(out != 0)
}

/// `setRequestHandlerRaw(protocol, method, handler)` through the native at
/// `set_request_handler_raw`: from then on the engine's requests for
/// `<protocol>.<method>` are answered by `sink`, on whichever engine thread
/// asks. Handlers are never replaced or unbound.
pub fn set_request_handler(
    set_request_handler_raw: *mut c_void,
    protocol: &str,
    method: &str,
    sink: RequestSink,
) -> Result<(), String> {
    let protocol = CString::new(protocol).map_err(|_| "the protocol has a NUL in it".to_string())?;
    let method = CString::new(method).map_err(|_| "the method has a NUL in it".to_string())?;
    let mut err = [0u8; 512];
    // SAFETY: the strings and `err` outlive the call; the C side checks the
    // native and keeps the handler object alive for the rest of the process.
    let rc = unsafe {
        intoxicated_messagebus_set_request_handler(
            set_request_handler_raw,
            protocol.as_ptr(),
            method.as_ptr(),
            sink,
            err.as_mut_ptr().cast(),
            err.len(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(text_before_nul(&err))
    }
}

/// `publishRaw(messageId, json)`. Success means the bus took the message,
/// nothing more.
pub fn publish(publish_raw: *mut c_void, message_id: &str, json: &str) -> Result<(), String> {
    intoxicated_linker_sys::game_activity::call_static_strings(publish_raw, MESSAGE_BUS_CLASS, &[message_id, json])
}

/// The text of a payload, or `None` for a null pointer.
///
/// # Safety
///
/// `payload` is null or a NUL-terminated string valid for the call, which is
/// what the bus hands a sink.
pub unsafe fn payload_bytes<'a>(payload: *const c_char) -> Option<&'a [u8]> {
    if payload.is_null() {
        return None;
    }
    // SAFETY: the caller's contract.
    Some(unsafe { std::ffi::CStr::from_ptr(payload) }.to_bytes())
}

fn text_before_nul(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static FIRST: AtomicU32 = AtomicU32::new(0);
    static SECOND: AtomicU32 = AtomicU32::new(0);
    static LENGTHS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn first(payload: *const c_char) {
        // SAFETY: the test hands a valid C string.
        LENGTHS.fetch_add(unsafe { payload_bytes(payload) }.map_or(0, |b| b.len()), Ordering::SeqCst);
        FIRST.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn second(_: *const c_char) {
        SECOND.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_second_listener_joins_the_first_instead_of_replacing_it() {
        let mut engine_calls = 0;
        let mut given = None;
        let joined = subscribe_with("test.TwoListeners", Some(first), |t| {
            engine_calls += 1;
            given = Some(t);
            Ok(())
        })
        .unwrap();
        assert_eq!(joined, Joined::Subscribed);
        let joined = subscribe_with("test.TwoListeners", Some(second), |_| {
            engine_calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(joined, Joined::Added);
        assert_eq!(engine_calls, 1, "the engine is asked once per message id");

        let payload = CString::new("{\"placeId\":1}").unwrap();
        (given.unwrap())(payload.as_ptr());
        assert_eq!(FIRST.load(Ordering::SeqCst), 1);
        assert_eq!(SECOND.load(Ordering::SeqCst), 1);
        assert_eq!(LENGTHS.load(Ordering::SeqCst), 13);
    }

    #[test]
    fn the_same_sink_twice_is_one_listener() {
        static ONCE: AtomicU32 = AtomicU32::new(0);
        extern "C" fn once(_: *const c_char) {
            ONCE.fetch_add(1, Ordering::SeqCst);
        }
        let mut given = None;
        subscribe_with("test.Twice", Some(once), |t| {
            given = Some(t);
            Ok(())
        })
        .unwrap();
        assert_eq!(subscribe_with("test.Twice", Some(once), |_| Ok(())).unwrap(), Joined::AlreadyListening);
        (given.unwrap())(std::ptr::null());
        assert_eq!(ONCE.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_subscribe_leaves_nobody_listening_and_can_be_retried() {
        static HEARD: AtomicU32 = AtomicU32::new(0);
        extern "C" fn heard(_: *const c_char) {
            HEARD.fetch_add(1, Ordering::SeqCst);
        }
        let mut given = None;
        let e = subscribe_with("test.Retry", Some(heard), |t| {
            given = Some(t);
            Err("no".into())
        })
        .unwrap_err();
        assert_eq!(e, "no");
        (given.unwrap())(std::ptr::null());
        assert_eq!(HEARD.load(Ordering::SeqCst), 0, "a sink whose subscribe failed is removed");
        assert_eq!(subscribe_with("test.Retry", Some(heard), |_| Ok(())).unwrap(), Joined::Subscribed);
    }

    #[test]
    fn ids_are_checked_before_anything_is_called() {
        assert!(subscribe_with("", Some(first), |_| panic!("called")).is_err());
        assert!(subscribe_with("a\0b", Some(first), |_| panic!("called")).is_err());
    }

    #[test]
    fn nothing_is_connected_that_was_never_subscribed() {
        assert_eq!(connection_ptr("test.NeverSubscribed"), 0);
        assert_eq!(connected(None, "test.NeverSubscribed"), None);
    }

    /// The natives in `native/message_bus.cpp`, with no engine and no JavaVM:
    /// each one refuses with its own reason, and a refused subscribe leaves
    /// the id free to subscribe later.
    #[test]
    fn the_bus_natives_refuse_cleanly_without_an_engine() {
        static NEVER: AtomicU32 = AtomicU32::new(0);
        extern "C" fn never(_: *const c_char) {
            NEVER.fetch_add(1, Ordering::SeqCst);
        }
        extern "C" fn no_answer(_: *const c_char, _: *mut c_char, _: usize) -> c_int {
            0
        }
        // Stand-ins with the natives' own shapes, so that even in a process
        // that had a JavaVM the call would be well defined. Never reached
        // here: the missing JavaVM is reported first.
        static REACHED: AtomicU32 = AtomicU32::new(0);
        extern "C" fn fake_subscribe(_: *mut c_void, _: *mut c_void, _: *mut c_void, _: *mut c_void, _: u8) -> *mut c_void {
            REACHED.fetch_add(1, Ordering::SeqCst);
            std::ptr::null_mut()
        }
        extern "C" fn fake_set_handler(_: *mut c_void, _: *mut c_void, _: *mut c_void, _: *mut c_void, _: *mut c_void) {
            REACHED.fetch_add(1, Ordering::SeqCst);
        }
        let e = subscribe(std::ptr::null_mut(), "test.NoEngine", Some(never)).unwrap_err();
        assert!(e.contains("doSubscribeRaw is not exported"), "{e}");
        assert_eq!(connection_ptr("test.NoEngine"), 0);
        let e = subscribe(fake_subscribe as *mut c_void, "test.NoEngine", Some(never)).unwrap_err();
        assert!(e.contains("no JavaVM"), "{e}");
        let e = set_request_handler(std::ptr::null_mut(), "Linking", "openURL", no_answer).unwrap_err();
        assert!(e.contains("setRequestHandlerRaw is not exported"), "{e}");
        let fake = fake_set_handler as *mut c_void;
        let e = set_request_handler(fake, "Linking", "openURL", no_answer).unwrap_err();
        assert!(e.contains("no JavaVM"), "{e}");
        assert!(set_request_handler(fake, "Link\0ing", "openURL", no_answer).is_err());
        assert_eq!(REACHED.load(Ordering::SeqCst), 0);
        // isConnected with nothing stored is not a question worth asking.
        assert_eq!(connected(Some(fake), "test.NoEngine"), None);
        assert_eq!(NEVER.load(Ordering::SeqCst), 0);
        assert_eq!(subscribe_with("test.NoEngine", Some(never), |_| Ok(())).unwrap(), Joined::Subscribed);
    }
}
