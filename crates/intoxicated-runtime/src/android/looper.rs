//! `ALooper`, the NDK's per-thread event loop, and the client's main pump.
//!
//! Written clean-room from a behavioural spec of what the engine and the rest
//! of the runtime expect, the NDK `android/looper.h` documentation and
//! FreeBSD's kqueue(2). The module it replaces was not read.
//!
//! **What the engine uses.** `libroblox.so` imports seven `ALooper_*`
//! functions (`prepare`, `forThread`, `acquire`, `release`, `addFd`,
//! `removeFd`, `pollOnce`); `ALooper_wake` is provided as well. In every
//! logged run there were exactly two registrations: AGDK's main-work pipe
//! with a callback on the main thread, and the GameActivity glue's command
//! pipe as ident 1 on the engine's app thread, which then calls `pollOnce`,
//! mostly with a zero timeout, in a tight loop.
//!
//! **Semantics.** As the NDK documents them, with two deliberate departures
//! and one choice:
//!
//! - A negative ("forever") timeout waits at most [`INFINITE_CEILING_MS`] and
//!   then reports a timeout. A lost wakeup becomes one late frame instead of
//!   a frozen client.
//! - A zero timeout, once the engine has presented [`BACKOFF_PRESENTS`] frames
//!   and this looper has answered [`BACKOFF_AFTER`] empty zero-timeout polls
//!   in a row, sleeps 250 us before the check. Answered literally the
//!   engine's loop burns a core; the check itself is never skipped.
//! - One kernel wait per call, at most [`BATCH`] events. Every ready callback
//!   in the batch runs, in kernel order, on the calling thread with no lock
//!   held; ready ident registrations are queued and handed back one per call,
//!   before the next wait. That is the NDK's own dispatch order.
//!
//! **Kernel side.** One native kqueue per looper, level-triggered, with an
//! `EVFILT_USER` trigger for `ALooper_wake`; see [`kqueue`] for why not
//! epoll-shim. A looper is never freed: every pointer handed out stays valid
//! for the life of the process, whatever the reference count says.
//!
//! **The pump** (the loop on the process's main thread that keeps the
//! engine's main-thread work, host input, housekeeping and the health line
//! going, then shuts the engine down) is in [`pump`].

use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

mod kqueue;
mod pump;
#[cfg(test)]
mod tests;

pub use pump::{note_join_requested, pump, request_quit};

use kqueue::{KEvent, WaitSet, EVFILT_READ, EVFILT_WRITE, EV_EOF, EV_ERROR};

// ------------------------------------------------------------ NDK constants

pub const ALOOPER_POLL_WAKE: c_int = -1;
pub const ALOOPER_POLL_CALLBACK: c_int = -2;
pub const ALOOPER_POLL_TIMEOUT: c_int = -3;
pub const ALOOPER_POLL_ERROR: c_int = -4;

pub const ALOOPER_EVENT_INPUT: c_int = 1;
pub const ALOOPER_EVENT_OUTPUT: c_int = 2;
pub const ALOOPER_EVENT_ERROR: c_int = 4;
pub const ALOOPER_EVENT_HANGUP: c_int = 8;
/// Never produced here: a descriptor closed while registered simply stops
/// being reported, because the kernel drops its filters on close.
pub const ALOOPER_EVENT_INVALID: c_int = 16;

pub const ALOOPER_PREPARE_ALLOW_NON_CALLBACKS: c_int = 1;

/// `ALooper_callbackFunc`: return 1 to stay registered, 0 to be removed.
pub type Callback = unsafe extern "C" fn(fd: c_int, events: c_int, data: *mut c_void) -> c_int;

// ------------------------------------------------------------ tuning

/// Most kernel events taken in one wait.
pub const BATCH: usize = 16;
/// The longest a negative timeout waits. FreeBSD lets a 50 ms timer run to
/// 50.7 to 54.1 ms (spec measurement), so an idle engine thread wakes about
/// 19 times a second.
pub const INFINITE_CEILING_MS: c_int = 50;
/// Empty zero-timeout polls in a row before the backoff sleeps.
pub const BACKOFF_AFTER: u32 = 64;
/// The default backoff sleep. `INTOXICATED_POLL_COALESCE_US` overrides it in
/// microseconds, and 0 turns the backoff off.
pub const BACKOFF_SLEEP_US: u64 = 250;
/// Presents before the backoff may engage. The startup freeze race happens
/// before the first frames, and nothing may slow the engine there.
pub const BACKOFF_PRESENTS: u64 = 120;
pub const COALESCE_VARIABLE: &str = "INTOXICATED_POLL_COALESCE_US";

/// The backoff sleep in microseconds: [`BACKOFF_SLEEP_US`] unless the variable
/// holds a whole number.
pub fn parse_coalesce(v: Option<&str>) -> u64 {
    v.and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(BACKOFF_SLEEP_US)
}

fn backoff_sleep_us() -> u64 {
    static US: OnceLock<u64> = OnceLock::new();
    *US.get_or_init(|| parse_coalesce(std::env::var(COALESCE_VARIABLE).ok().as_deref()))
}

#[cfg(test)]
thread_local! {
    /// The present count this thread's polls see, for the backoff tests.
    static TEST_PRESENTS: Cell<Option<u64>> = const { Cell::new(None) };
}

fn presents() -> u64 {
    #[cfg(test)]
    if let Some(n) = TEST_PRESENTS.with(Cell::get) {
        return n;
    }
    super::glcount::QUEUE_PRESENT.load(Ordering::Relaxed)
}

// ------------------------------------------------------------ the clock

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the looper clock was first used, which is normally
/// when the first looper was created. devctl's `loopers` reads it.
pub fn clock_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

// ------------------------------------------------------------ one looper

#[derive(Clone, Copy)]
struct Registration {
    fd: c_int,
    /// As stored: `ALOOPER_POLL_CALLBACK` for a callback registration.
    ident: c_int,
    callback: Option<Callback>,
    data: usize,
    /// Tells this registration from a later one on the same descriptor.
    seq: u64,
}

/// A ready ident registration, waiting to be handed back by `pollOnce`.
#[derive(Clone, Copy)]
struct Response {
    fd: c_int,
    ident: c_int,
    events: c_int,
    data: usize,
}

#[derive(Default)]
struct Table {
    regs: Vec<Registration>,
    responses: VecDeque<Response>,
    next_seq: u64,
}

impl Table {
    fn find(&self, fd: c_int) -> Option<&Registration> {
        self.regs.iter().find(|r| r.fd == fd)
    }

    fn drop_fd(&mut self, fd: c_int) -> bool {
        let before = self.regs.len();
        self.regs.retain(|r| r.fd != fd);
        self.responses.retain(|r| r.fd != fd);
        self.regs.len() != before
    }
}

/// The out parameters of `pollOnce`; each may be null.
#[derive(Clone, Copy)]
pub(crate) struct Outs {
    fd: *mut c_int,
    events: *mut c_int,
    data: *mut *mut c_void,
}

impl Outs {
    pub(crate) const NONE: Outs =
        Outs { fd: std::ptr::null_mut(), events: std::ptr::null_mut(), data: std::ptr::null_mut() };

    fn write(&self, r: &Response) {
        // SAFETY: the caller of `pollOnce` passes either null or a pointer it
        // owns for the call, per the NDK contract.
        unsafe {
            if !self.fd.is_null() {
                *self.fd = r.fd;
            }
            if !self.events.is_null() {
                *self.events = r.events;
            }
            if !self.data.is_null() {
                *self.data = r.data as *mut c_void;
            }
        }
    }
}

/// One thread's looper. Shared (`&'static`) with any thread that is handed
/// its pointer; everything that changes is behind the table lock or atomic.
pub struct Looper {
    waits: WaitSet,
    allow_non_callbacks: bool,
    /// The owning thread, as `procstat -t` and lldb number it.
    tid: i32,
    refs: AtomicI64,
    table: Mutex<Table>,
    /// `table.responses.len()`, so an empty poll never takes the lock.
    queued: AtomicUsize,
    /// Consecutive zero-timeout polls that found nothing.
    empty_zero_polls: AtomicU32,
    /// Negative-timeout polls that ran into the ceiling.
    ceiling_expired: AtomicU64,
    /// Polls that slept for the backoff.
    backoff_sleeps: AtomicU64,
    registered: AtomicUsize,
    polls: AtomicU64,
    events: AtomicU64,
    wakes: AtomicU64,
    last_event_ms: AtomicU64,
    last_wake_ms: AtomicU64,
}

/// Every looper ever created, in creation order. Never shrinks.
static LOOPERS: Mutex<Vec<&'static Looper>> = Mutex::new(Vec::new());

/// The main thread's looper once the pump runs, for [`wake_pump`].
static PUMP_LOOPER: AtomicPtr<Looper> = AtomicPtr::new(std::ptr::null_mut());

thread_local! {
    /// This thread's looper, or null.
    static CURRENT: Cell<*const Looper> = const { Cell::new(std::ptr::null()) };
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn current() -> Option<&'static Looper> {
    let p = CURRENT.with(Cell::get);
    // SAFETY: only ever set to a leaked `Looper`, which is never freed.
    unsafe { p.as_ref() }
}

fn from_handle(p: *mut c_void) -> Option<&'static Looper> {
    // SAFETY: a non-null handle is one `ALooper_prepare` or `forThread`
    // returned, which points at a leaked `Looper` that is never freed.
    unsafe { (p as *const Looper).as_ref() }
}

fn as_handle(l: &'static Looper) -> *mut c_void {
    l as *const Looper as *mut c_void
}

fn thread_id() -> i32 {
    // SAFETY: no arguments; FreeBSD's own thread id call.
    unsafe { libc::pthread_getthreadid_np() }
}

/// The calling thread's looper, creating it on the first call. `None` only
/// if the kernel refuses a kqueue.
fn prepare(opts: c_int) -> Option<&'static Looper> {
    if let Some(l) = current() {
        return Some(l);
    }
    let created_ms = clock_ms();
    let waits = match WaitSet::new() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[android] ALooper_prepare: no kqueue for this thread ({e})");
            return None;
        }
    };
    let l: &'static Looper = Box::leak(Box::new(Looper {
        waits,
        allow_non_callbacks: opts & ALOOPER_PREPARE_ALLOW_NON_CALLBACKS != 0,
        tid: thread_id(),
        refs: AtomicI64::new(0),
        table: Mutex::new(Table::default()),
        queued: AtomicUsize::new(0),
        empty_zero_polls: AtomicU32::new(0),
        ceiling_expired: AtomicU64::new(0),
        backoff_sleeps: AtomicU64::new(0),
        registered: AtomicUsize::new(0),
        polls: AtomicU64::new(0),
        events: AtomicU64::new(0),
        wakes: AtomicU64::new(0),
        last_event_ms: AtomicU64::new(created_ms),
        last_wake_ms: AtomicU64::new(0),
    }));
    lock(&LOOPERS).push(l);
    CURRENT.with(|c| c.set(l));
    Some(l)
}

/// The kernel's report on one filter, as `ALOOPER_EVENT_*` bits.
fn event_bits(ev: &KEvent) -> c_int {
    let eof = ev.flags & EV_EOF != 0;
    let mut bits = match ev.filter {
        // A pipe whose writer closed with nothing unread is a hang-up and
        // not readable, as epoll reports it.
        EVFILT_READ if eof => ALOOPER_EVENT_HANGUP | if ev.data > 0 { ALOOPER_EVENT_INPUT } else { 0 },
        EVFILT_READ => ALOOPER_EVENT_INPUT,
        EVFILT_WRITE if eof => ALOOPER_EVENT_OUTPUT | ALOOPER_EVENT_HANGUP,
        EVFILT_WRITE => ALOOPER_EVENT_OUTPUT,
        _ => 0,
    };
    // On EOF, `fflags` carries a socket's pending error.
    if ev.flags & EV_ERROR != 0 || (eof && ev.fflags != 0) {
        bits |= ALOOPER_EVENT_ERROR;
    }
    bits
}

/// A ready callback, copied out of the table so it can run unlocked.
#[derive(Clone, Copy)]
struct Call {
    callback: Callback,
    fd: c_int,
    events: c_int,
    data: usize,
    seq: u64,
}

impl Looper {
    /// `ALooper_addFd` after its argument checks. Returns 1 or -1.
    fn add(&self, fd: c_int, ident: c_int, events: c_int, callback: Option<Callback>, data: usize) -> c_int {
        // SAFETY: F_GETFD only reads the descriptor's flags. A registration
        // with no interest bits makes no kernel change below, so this is
        // what refuses a descriptor that is not open, as epoll would.
        if fd < 0 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            eprintln!("[android] ALooper_addFd(fd={fd}): not an open descriptor");
            return -1;
        }
        let ident = if callback.is_some() { ALOOPER_POLL_CALLBACK } else { ident };
        let read = events & ALOOPER_EVENT_INPUT != 0;
        let write = events & ALOOPER_EVENT_OUTPUT != 0;
        let mut t = lock(&self.table);
        let replacing = t.find(fd).is_some();
        // A registration that asks for neither direction is kept in the
        // table and never reported, as epoll does for it.
        if read || write || replacing {
            if let Err(e) = self.waits.watch(fd, read, write) {
                if replacing {
                    // The old filters were removed with the failed change.
                    t.drop_fd(fd);
                    self.queued.store(t.responses.len(), Ordering::Release);
                    self.registered.store(t.regs.len(), Ordering::Relaxed);
                }
                drop(t);
                eprintln!("[android] ALooper_addFd(fd={fd}): the kernel refused it ({e})");
                return -1;
            }
        }
        t.next_seq += 1;
        let reg = Registration { fd, ident, callback, data, seq: t.next_seq };
        if replacing {
            // The NDK replaces an existing registration. Queued results
            // belong to the old one.
            t.responses.retain(|r| r.fd != fd);
            if let Some(slot) = t.regs.iter_mut().find(|r| r.fd == fd) {
                *slot = reg;
            }
        } else {
            t.regs.push(reg);
        }
        self.queued.store(t.responses.len(), Ordering::Release);
        self.registered.store(t.regs.len(), Ordering::Relaxed);
        drop(t);
        self.empty_zero_polls.store(0, Ordering::Release);
        1
    }

    /// `ALooper_removeFd`. Returns 1 if a registration was removed, else 0.
    fn remove(&self, fd: c_int) -> c_int {
        let mut t = lock(&self.table);
        self.waits.forget(fd);
        let removed = t.drop_fd(fd);
        self.queued.store(t.responses.len(), Ordering::Release);
        self.registered.store(t.regs.len(), Ordering::Relaxed);
        c_int::from(removed)
    }

    /// Removes `fd` only if it is still the registration `seq`, for a
    /// callback that returned 0 and may have re-registered its own fd.
    fn remove_if(&self, fd: c_int, seq: u64) {
        let mut t = lock(&self.table);
        if t.find(fd).is_some_and(|r| r.seq == seq) {
            self.waits.forget(fd);
            t.drop_fd(fd);
            self.queued.store(t.responses.len(), Ordering::Release);
            self.registered.store(t.regs.len(), Ordering::Relaxed);
        }
    }

    fn is_current(&self, fd: c_int, seq: u64) -> bool {
        lock(&self.table).find(fd).is_some_and(|r| r.seq == seq)
    }

    /// `ALooper_wake`. The backoff is told first, so there is no moment when
    /// a wake is pending and the backoff still thinks the looper is idle.
    fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::Relaxed);
        self.last_wake_ms.store(clock_ms(), Ordering::Relaxed);
        self.empty_zero_polls.store(0, Ordering::SeqCst);
        self.waits.wake();
    }

    fn next_response(&self) -> Option<Response> {
        let mut t = lock(&self.table);
        let r = t.responses.pop_front();
        self.queued.store(t.responses.len(), Ordering::Release);
        r
    }

    /// `ALooper_pollOnce` on this looper. Only its owning thread calls it.
    fn poll(&self, timeout: c_int, outs: Outs) -> c_int {
        self.polls.fetch_add(1, Ordering::Relaxed);

        // Results from the last batch come before another wait.
        if self.queued.load(Ordering::Acquire) != 0 {
            if let Some(r) = self.next_response() {
                if timeout == 0 {
                    self.empty_zero_polls.store(0, Ordering::Relaxed);
                }
                outs.write(&r);
                return r.ident;
            }
        }

        let wait_ms = if timeout < 0 { INFINITE_CEILING_MS } else { timeout };
        if timeout == 0 && self.empty_zero_polls.load(Ordering::Acquire) >= BACKOFF_AFTER {
            let us = backoff_sleep_us();
            if us > 0 && presents() >= BACKOFF_PRESENTS {
                self.backoff_sleeps.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_micros(us));
            }
        }

        let mut buf = [MaybeUninit::<KEvent>::uninit(); BATCH];
        let batch = match self.waits.wait(&mut buf, wait_ms) {
            Ok(b) => b,
            // A caught signal interrupts the wait (FreeBSD does not restart
            // it). The NDK reports that as a wake.
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => return ALOOPER_POLL_WAKE,
            Err(_) => return ALOOPER_POLL_ERROR,
        };
        if batch.is_empty() {
            if timeout == 0 {
                self.empty_zero_polls.fetch_add(1, Ordering::AcqRel);
            } else if timeout < 0 {
                self.ceiling_expired.fetch_add(1, Ordering::Relaxed);
            }
            return ALOOPER_POLL_TIMEOUT;
        }
        self.events.fetch_add(1, Ordering::Relaxed);
        self.last_event_ms.store(clock_ms(), Ordering::Relaxed);
        if timeout == 0 {
            self.empty_zero_polls.store(0, Ordering::Release);
        }

        // One entry per descriptor: a descriptor watched both ways comes back
        // as two kernel events.
        let mut woke = false;
        let mut ready: [(c_int, c_int); BATCH] = [(0, 0); BATCH];
        let mut nready = 0;
        for ev in batch {
            if kqueue::is_wake(ev) {
                woke = true;
                continue;
            }
            let fd = ev.ident as c_int;
            let bits = event_bits(ev);
            match ready[..nready].iter_mut().find(|(f, _)| *f == fd) {
                Some(entry) => entry.1 |= bits,
                None => {
                    ready[nready] = (fd, bits);
                    nready += 1;
                }
            }
        }

        let mut calls: [Option<Call>; BATCH] = [None; BATCH];
        let mut ncalls = 0;
        {
            let mut t = lock(&self.table);
            for &(fd, events) in &ready[..nready] {
                let Some(reg) = t.find(fd).copied() else { continue };
                match reg.callback {
                    Some(callback) => {
                        calls[ncalls] = Some(Call { callback, fd, events, data: reg.data, seq: reg.seq });
                        ncalls += 1;
                    }
                    None => t.responses.push_back(Response { fd, ident: reg.ident, events, data: reg.data }),
                }
            }
            self.queued.store(t.responses.len(), Ordering::Release);
        }

        let mut result = if woke { ALOOPER_POLL_WAKE } else { ALOOPER_POLL_TIMEOUT };
        for call in calls[..ncalls].iter().flatten() {
            // An earlier callback in this batch may have removed it.
            if !self.is_current(call.fd, call.seq) {
                continue;
            }
            // SAFETY: the engine registered this function for this data; it
            // runs on the polling thread, as the NDK promises, with no lock
            // held, so it may call back into this looper.
            let keep = unsafe { (call.callback)(call.fd, call.events, call.data as *mut c_void) };
            if keep == 0 {
                self.remove_if(call.fd, call.seq);
            }
            result = ALOOPER_POLL_CALLBACK;
        }
        if self.queued.load(Ordering::Acquire) != 0 {
            if let Some(r) = self.next_response() {
                outs.write(&r);
                return r.ident;
            }
        }
        result
    }

    /// The registrations as `fd:ident:flag`, comma-separated, in the order
    /// they were made, and how many there are.
    fn registrations(&self) -> (usize, String) {
        let t = lock(&self.table);
        let list = t
            .regs
            .iter()
            .map(|r| format!("{}:{}:{}", r.fd, r.ident, if r.callback.is_some() { "cb" } else { "-" }))
            .collect::<Vec<_>>()
            .join(",");
        (t.regs.len(), list)
    }
}

// ------------------------------------------------------------ the NDK entry points

/// The line every `ALooper_addFd` call prints first, valid looper or not:
/// the arguments as the engine passed them.
pub fn add_fd_line(fd: c_int, ident: c_int, events: c_int, callback: bool) -> String {
    format!(
        "[android] ALooper_addFd(fd={fd}, ident={ident}, events={events}, callback={})",
        if callback { "yes" } else { "no" }
    )
}

extern "C" fn alooper_prepare(opts: c_int) -> *mut c_void {
    super::trace(format_args!("ALooper_prepare({opts})"));
    prepare(opts).map(as_handle).unwrap_or(std::ptr::null_mut())
}

extern "C" fn alooper_for_thread() -> *mut c_void {
    let l = current();
    super::trace(format_args!("ALooper_forThread -> {}", if l.is_some() { "this thread's looper" } else { "null" }));
    l.map(as_handle).unwrap_or(std::ptr::null_mut())
}

extern "C" fn alooper_acquire(looper: *mut c_void) {
    if let Some(l) = from_handle(looper) {
        l.refs.fetch_add(1, Ordering::Relaxed);
    }
}

/// Counted, never acted on: loopers are never freed.
extern "C" fn alooper_release(looper: *mut c_void) {
    if let Some(l) = from_handle(looper) {
        l.refs.fetch_sub(1, Ordering::Relaxed);
    }
}

extern "C" fn alooper_add_fd(
    looper: *mut c_void,
    fd: c_int,
    ident: c_int,
    events: c_int,
    callback: Option<Callback>,
    data: *mut c_void,
) -> c_int {
    println!("{}", add_fd_line(fd, ident, events, callback.is_some()));
    let Some(l) = from_handle(looper) else { return -1 };
    if callback.is_none() {
        if !l.allow_non_callbacks {
            eprintln!(
                "[android] ALooper_addFd(fd={fd}): refused, no callback on a looper prepared without \
                 ALOOPER_PREPARE_ALLOW_NON_CALLBACKS"
            );
            return -1;
        }
        if ident < 0 {
            eprintln!("[android] ALooper_addFd(fd={fd}): refused, no callback and a negative ident");
            return -1;
        }
    }
    l.add(fd, ident, events, callback, data as usize)
}

extern "C" fn alooper_remove_fd(looper: *mut c_void, fd: c_int) -> c_int {
    match from_handle(looper) {
        Some(l) => l.remove(fd),
        None => -1,
    }
}

extern "C" fn alooper_poll_once(
    timeout_millis: c_int,
    out_fd: *mut c_int,
    out_events: *mut c_int,
    out_data: *mut *mut c_void,
) -> c_int {
    match current() {
        Some(l) => l.poll(timeout_millis, Outs { fd: out_fd, events: out_events, data: out_data }),
        None => ALOOPER_POLL_ERROR,
    }
}

extern "C" fn alooper_wake(looper: *mut c_void) {
    if let Some(l) = from_handle(looper) {
        l.wake();
    }
}

/// The NDK names and this module's entry points, for the Android override
/// table (`android::overrides`).
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("ALooper_prepare", alooper_prepare),
        f!("ALooper_forThread", alooper_for_thread),
        f!("ALooper_acquire", alooper_acquire),
        f!("ALooper_release", alooper_release),
        f!("ALooper_addFd", alooper_add_fd),
        f!("ALooper_removeFd", alooper_remove_fd),
        f!("ALooper_pollOnce", alooper_poll_once),
        f!("ALooper_wake", alooper_wake),
    ]
}

// ------------------------------------------------------------ the runtime's side

/// Prepares this thread's looper, as Android's framework does for the UI
/// thread before any app code runs: AGDK's `initializeNativeCode` gives up
/// with a zero handle when `ALooper_forThread` answers null. Prepared to
/// accept non-callback registrations, because the pump registers the display
/// connection on it that way. False only if the kernel refused a kqueue.
pub fn prepare_for_current_thread() -> bool {
    prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS).is_some()
}

/// Wakes the pump's wait, so something queued for it (devctl input, a stop
/// request) runs now rather than after up to 50 ms. Before the pump starts it
/// does nothing.
pub fn wake_pump() {
    let p = PUMP_LOOPER.load(Ordering::Acquire);
    // SAFETY: only ever set to a leaked `Looper`.
    if let Some(l) = unsafe { p.as_ref() } {
        l.wake();
    }
}

/// One looper's statistics, for devctl's `loopers`.
#[derive(Debug, Clone)]
pub struct LooperStats {
    /// The owning thread's FreeBSD thread id.
    pub tid: i32,
    /// Current registrations, the pump's display connection included.
    pub fds: usize,
    /// The registrations as `fd:ident:flag`, comma-separated.
    pub list: String,
    /// `pollOnce` calls, the pump's own included.
    pub polls: u64,
    /// Polls in which the kernel reported anything ready.
    pub events: u64,
    pub wakes: u64,
    /// Looper clock of the last poll that found something (creation time
    /// before that) and of the last wake (0 if never).
    pub last_event_ms: u64,
    pub last_wake_ms: u64,
}

/// The looper clock now, and every looper ever created, in creation order.
/// Safe from any thread at any time.
pub fn snapshot() -> (u64, Vec<LooperStats>) {
    let now = clock_ms();
    let loopers: Vec<&'static Looper> = lock(&LOOPERS).clone();
    let stats = loopers
        .into_iter()
        .map(|l| {
            let (fds, list) = l.registrations();
            LooperStats {
                tid: l.tid,
                fds,
                list,
                polls: l.polls.load(Ordering::Relaxed),
                events: l.events.load(Ordering::Relaxed),
                wakes: l.wakes.load(Ordering::Relaxed),
                last_event_ms: l.last_event_ms.load(Ordering::Relaxed),
                last_wake_ms: l.last_wake_ms.load(Ordering::Relaxed),
            }
        })
        .collect();
    (now, stats)
}
