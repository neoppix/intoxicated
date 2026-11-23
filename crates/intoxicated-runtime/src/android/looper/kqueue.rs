//! The kernel wait set behind each looper: one native kqueue.
//!
//! Why not epoll-shim, which the binary links anyway: an epoll-shim descriptor
//! is a kqueue with a poll(2) in front of it, so every wait that finds
//! something costs two system calls instead of one, and a wake through an
//! eventfd costs a write, the wait and a read where an `EVFILT_USER` trigger
//! costs the trigger and the wait. The engine never sees this descriptor, so
//! nothing it does needs epoll's semantics here. The engine's own `epoll_*`
//! imports still go through `native/freebsd_abi.c` and epoll-shim, unchanged.
//!
//! Every filter is level-triggered (no `EV_CLEAR`), which is what the ALooper
//! contract promises: a descriptor that is not drained is reported again. The
//! wake trigger is the exception: it is `EV_CLEAR`, so it fires once however
//! many wakes came before the wait and needs no draining.

use std::ffi::{c_int, c_void};
use std::io;
use std::mem::MaybeUninit;

pub(super) const EVFILT_READ: i16 = -1;
pub(super) const EVFILT_WRITE: i16 = -2;
pub(super) const EVFILT_SIGNAL: i16 = -6;
pub(super) const EVFILT_USER: i16 = -11;

const EV_ADD: u16 = 0x0001;
const EV_DELETE: u16 = 0x0002;
const EV_CLEAR: u16 = 0x0020;
const EV_RECEIPT: u16 = 0x0040;
pub(super) const EV_ERROR: u16 = 0x4000;
pub(super) const EV_EOF: u16 = 0x8000;

const NOTE_TRIGGER: u32 = 0x0100_0000;

/// The wake trigger's identifier. `EVFILT_USER` has its own namespace, so it
/// cannot collide with a descriptor number.
const WAKE_IDENT: usize = 0;

/// FreeBSD 12 and later `struct kevent` (64 bytes on amd64). Declared here
/// rather than taken from the `libc` crate, whose layout follows whichever
/// FreeBSD ABI it was configured for; the plain `kevent` symbol this links
/// against is the current one, which takes this layout.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct KEvent {
    pub ident: usize,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: i64,
    pub udata: *mut c_void,
    pub ext: [u64; 4],
}

impl KEvent {
    pub(super) const ZERO: KEvent =
        KEvent { ident: 0, filter: 0, flags: 0, fflags: 0, data: 0, udata: std::ptr::null_mut(), ext: [0; 4] };

    fn change(ident: usize, filter: i16, flags: u16, fflags: u32) -> KEvent {
        KEvent { ident, filter, flags, fflags, ..KEvent::ZERO }
    }
}

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

extern "C" {
    fn kqueue() -> c_int;
    fn kevent(
        kq: c_int,
        changelist: *const KEvent,
        nchanges: c_int,
        eventlist: *mut KEvent,
        nevents: c_int,
        timeout: *const Timespec,
    ) -> c_int;
}

/// One kqueue. Never closed: a looper lives for the process.
pub(super) struct WaitSet {
    kq: c_int,
}

impl WaitSet {
    /// A new kqueue with the wake trigger already in it.
    pub(super) fn new() -> io::Result<WaitSet> {
        // SAFETY: no arguments.
        let kq = unsafe { kqueue() };
        if kq < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `kq` is a descriptor this call just created. A failure only
        // leaves it inheritable across exec, which nothing here relies on.
        unsafe { libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC) };
        let set = WaitSet { kq };
        let errors = set.apply(&[KEvent::change(WAKE_IDENT, EVFILT_USER, EV_ADD | EV_CLEAR, 0)]);
        if let Some(&e) = errors.first().filter(|&&e| e != 0) {
            // SAFETY: closing the native kqueue created above; nothing else
            // has seen it.
            unsafe { libc::close(kq) };
            return Err(io::Error::from_raw_os_error(e));
        }
        Ok(set)
    }

    /// Applies `changes` without collecting pending events, and returns one
    /// errno per change (0 for success). `EV_RECEIPT` makes the kernel answer
    /// every change, so a failure in one does not hide the others.
    fn apply(&self, changes: &[KEvent]) -> Vec<c_int> {
        let mut list: Vec<KEvent> = changes.iter().map(|c| KEvent { flags: c.flags | EV_RECEIPT, ..*c }).collect();
        let mut receipts = vec![KEvent::ZERO; list.len()];
        let zero = Timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: both arrays are live and sized as passed.
        let n = unsafe {
            kevent(
                self.kq,
                list.as_mut_ptr(),
                list.len() as c_int,
                receipts.as_mut_ptr(),
                receipts.len() as c_int,
                &zero,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error().raw_os_error().unwrap_or(libc::EINVAL);
            return vec![e; changes.len()];
        }
        let mut out = vec![0; changes.len()];
        for r in &receipts[..n as usize] {
            if r.flags & EV_ERROR != 0 && r.data != 0 {
                if let Some(i) = list.iter().position(|c| c.ident == r.ident && c.filter == r.filter) {
                    out[i] = r.data as c_int;
                }
            }
        }
        out
    }

    /// Makes the kernel watch `fd` for exactly `read` and `write`, whatever it
    /// was watching before. Adding a filter that is already there updates it,
    /// so a descriptor number that was closed and reused is registered again.
    /// Removing one that is not there is not an error.
    pub(super) fn watch(&self, fd: c_int, read: bool, write: bool) -> io::Result<()> {
        let ident = fd as usize;
        let op = |on: bool| if on { EV_ADD } else { EV_DELETE };
        let changes = [
            KEvent::change(ident, EVFILT_READ, op(read), 0),
            KEvent::change(ident, EVFILT_WRITE, op(write), 0),
        ];
        let errors = self.apply(&changes);
        let failed = changes
            .iter()
            .zip(&errors)
            .find(|(c, &e)| c.flags & EV_ADD != 0 && e != 0)
            .map(|(_, &e)| e);
        match failed {
            None => Ok(()),
            Some(e) => {
                // Leave nothing half-registered.
                self.forget(fd);
                Err(io::Error::from_raw_os_error(e))
            }
        }
    }

    /// Drops both filters for `fd`. Errors are ignored: a closed descriptor's
    /// filters are already gone (EBADF), and one never watched has none.
    pub(super) fn forget(&self, fd: c_int) {
        let ident = fd as usize;
        self.apply(&[KEvent::change(ident, EVFILT_READ, EV_DELETE, 0), KEvent::change(ident, EVFILT_WRITE, EV_DELETE, 0)]);
    }

    /// Reports `sig` as a wake. This is the kernel's own record of a delivery
    /// to the process (whichever thread took it) and does not replace or call
    /// the signal's handler.
    pub(super) fn watch_signal(&self, sig: c_int) -> io::Result<()> {
        match self.apply(&[KEvent::change(sig as usize, EVFILT_SIGNAL, EV_ADD, 0)]).first() {
            Some(&0) | None => Ok(()),
            Some(&e) => Err(io::Error::from_raw_os_error(e)),
        }
    }

    /// Fires the wake trigger. Any thread may call it; triggers before the
    /// next wait coalesce into one event.
    pub(super) fn wake(&self) {
        let trigger = KEvent::change(WAKE_IDENT, EVFILT_USER, 0, NOTE_TRIGGER);
        // SAFETY: one live change, no event list.
        unsafe { kevent(self.kq, &trigger, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    }

    /// One wait of at most `timeout_ms` (0 returns at once). Returns the
    /// events the kernel wrote into the front of `out`. `out` is not
    /// initialised first: the engine's zero-timeout loop calls this millions
    /// of times a second, mostly to hear nothing.
    pub(super) fn wait<'a>(&self, out: &'a mut [MaybeUninit<KEvent>], timeout_ms: c_int) -> io::Result<&'a [KEvent]> {
        let ms = timeout_ms.max(0) as i64;
        let timeout = Timespec { tv_sec: ms / 1000, tv_nsec: (ms % 1000) * 1_000_000 };
        // SAFETY: `out` is live for `out.len()` entries; the kernel writes at
        // most that many.
        let n = unsafe { kevent(self.kq, std::ptr::null(), 0, out.as_mut_ptr().cast(), out.len() as c_int, &timeout) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the kernel initialised the first `n` entries.
        Ok(unsafe { std::slice::from_raw_parts(out.as_ptr().cast::<KEvent>(), n as usize) })
    }
}

/// Whether a returned event is the wake trigger, a watched signal, or neither.
pub(super) fn is_wake(ev: &KEvent) -> bool {
    ev.filter == EVFILT_USER || ev.filter == EVFILT_SIGNAL
}
