//! bionic's condition variables, semaphores, `pthread_once` and thread keys
//! over FreeBSD's.
//!
//! bionic's `pthread_cond_t` is 48 bytes and its `sem_t` 16, both used in
//! place; FreeBSD's condvar is a pointer to a libthr object and its `sem_t`
//! has its own layout. So each engine object carries a small overlay: its
//! first 8 bytes are a state word (0 never initialised, [`CREATING`] while one
//! thread builds the backing, [`READY`] once it is published) and the next 8
//! the address of a 128-byte heap block holding the real FreeBSD object. The
//! rest of the caller's bytes are never touched. bionic's static initialisers
//! are all zero, so a zero object is created on first use; a state word that
//! is none of the three is somebody else's bytes and gets EINVAL.
//!
//! Mutexes are not here: `native/freebsd_libc_compat.c` keeps a real FreeBSD
//! mutex per bionic mutex address, and a wait here uses that same mutex
//! (`bionic_mutex_real`).

use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicU64, Ordering};

/// The state word of an object that has never been initialised.
const UNSET: u64 = 0;
/// One thread is building the backing; others wait for [`READY`].
const CREATING: u64 = 0x494e_5458_0000_0001;
/// The backing is published. Zero-initialisation cannot produce either.
const READY: u64 = 0x494e_5458_5245_4459;
/// Bytes of the heap block behind each object.
const BACKING: usize = 128;

const EINVAL: c_int = 22;

extern "C" {
    fn bionic_mutex_real(m: *mut c_void) -> *mut c_void;
    fn bionic_cond_init_monotonic(cond: *mut c_void) -> c_int;
    fn bionic_pthread_once(control: *mut c_int, init: Option<extern "C" fn()>) -> c_int;
    fn pthread_cond_init(cond: *mut c_void, attr: *const c_void) -> c_int;
    fn pthread_cond_destroy(cond: *mut c_void) -> c_int;
    fn pthread_cond_signal(cond: *mut c_void) -> c_int;
    fn pthread_cond_broadcast(cond: *mut c_void) -> c_int;
    fn pthread_cond_wait(cond: *mut c_void, mutex: *mut c_void) -> c_int;
    fn pthread_cond_timedwait(cond: *mut c_void, mutex: *mut c_void, abstime: *const c_void) -> c_int;
    fn sem_init(sem: *mut c_void, pshared: c_int, value: c_uint) -> c_int;
    fn sem_destroy(sem: *mut c_void) -> c_int;
    fn sem_post(sem: *mut c_void) -> c_int;
    fn sem_wait(sem: *mut c_void) -> c_int;
    fn sem_trywait(sem: *mut c_void) -> c_int;
    fn pthread_key_create(key: *mut c_uint, dtor: Option<unsafe extern "C" fn(*mut c_void)>) -> c_int;
    fn pthread_key_delete(key: c_uint) -> c_int;
    fn pthread_getspecific(key: c_uint) -> *mut c_void;
    fn pthread_setspecific(key: c_uint, value: *const c_void) -> c_int;
}

// ---------------------------------------------------------------- overlay

/// The two words at the start of a bionic object.
struct Overlay {
    state: &'static AtomicU64,
    backing: *mut u64,
}

impl Overlay {
    /// # Safety
    /// `p` points at a bionic object of at least 16 bytes, 8-aligned.
    unsafe fn at(p: *mut c_void) -> Overlay {
        Overlay { state: unsafe { &*(p as *const AtomicU64) }, backing: unsafe { (p as *mut u64).add(1) } }
    }

    fn get(&self) -> *mut c_void {
        // SAFETY: the second word of the caller's object.
        unsafe { std::ptr::read_volatile(self.backing) as *mut c_void }
    }

    fn publish(&self, b: *mut c_void) {
        // SAFETY: as above.
        unsafe { std::ptr::write_volatile(self.backing, b as u64) };
        self.state.store(READY, Ordering::Release);
    }

    /// The backing, creating it with `make` if the object is still zero.
    /// `Err` for bytes that are not an overlay.
    fn backing_or_create(&self, make: impl FnOnce(*mut c_void) -> c_int) -> Result<*mut c_void, c_int> {
        loop {
            match self.state.load(Ordering::Acquire) {
                READY => return Ok(self.get()),
                UNSET => {
                    if self.state.compare_exchange(UNSET, CREATING, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                        let b = new_backing();
                        let r = make(b);
                        if r != 0 {
                            free_backing(b);
                            self.state.store(UNSET, Ordering::Release);
                            return Err(r);
                        }
                        self.publish(b);
                        return Ok(b);
                    }
                }
                CREATING => std::thread::yield_now(),
                _ => return Err(EINVAL),
            }
        }
    }
}

fn new_backing() -> *mut c_void {
    Box::into_raw(Box::new([0u64; BACKING / 8])).cast()
}

fn free_backing(b: *mut c_void) {
    if !b.is_null() {
        // SAFETY: made by `new_backing`.
        drop(unsafe { Box::from_raw(b.cast::<[u64; BACKING / 8]>()) });
    }
}

// ------------------------------------------------------- condition variables

fn force_monotonic() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("INTOXICATED_COND_FORCE_MONOTONIC").is_some())
}

/// Initialise a backing condition variable.
///
/// Forcing CLOCK_MONOTONIC on every condvar was right for boost, which asks
/// for it through the attribute, and wrong for everything that does not:
/// libc++'s condition_variable and RakNet build *realtime* absolute
/// deadlines (~1.79e9 s), which on a monotonic condvar are decades away. A
/// game join's RakNet update thread entered such a wait and never came back,
/// so no handshake was ever sent. The attribute now decides; its clock id is
/// translated in `pthread_condattr_setclock` (freebsd_abi.c).
/// `INTOXICATED_COND_FORCE_MONOTONIC=1` restores the old behaviour as a control.
fn make_cond(p: *mut c_void, attr: *const c_void) -> c_int {
    // The attribute is a bionic `long` holding a FreeBSD clock id, owned by
    // freebsd_abi.c's condattr functions; 4 is CLOCK_MONOTONIC.
    // SAFETY: a non-null attr is the engine's 8-byte condattr.
    let monotonic = force_monotonic() || (!attr.is_null() && unsafe { *(attr as *const i64) } == 4);
    // SAFETY: `p` is a fresh backing block, larger than FreeBSD's condvar.
    unsafe {
        if monotonic {
            bionic_cond_init_monotonic(p)
        } else {
            pthread_cond_init(p, std::ptr::null())
        }
    }
}

/// The backing condvar, created as a default one for a zero object.
unsafe fn cond_backing(c: *mut c_void) -> Result<*mut c_void, c_int> {
    if c.is_null() {
        return Err(EINVAL);
    }
    // SAFETY: the engine's 48-byte condvar.
    unsafe { Overlay::at(c) }.backing_or_create(|b| make_cond(b, std::ptr::null()))
}

unsafe extern "C" fn b_cond_init(c: *mut c_void, attr: *const c_void) -> c_int {
    if c.is_null() {
        return EINVAL;
    }
    // SAFETY: the engine's condvar.
    let o = unsafe { Overlay::at(c) };
    if o.state.load(Ordering::Acquire) == READY {
        let old = o.get();
        // SAFETY: our backing, holding an initialised host condvar.
        unsafe { pthread_cond_destroy(old) };
        free_backing(old);
    }
    let b = new_backing();
    let r = make_cond(b, attr);
    if r != 0 {
        free_backing(b);
        o.state.store(UNSET, Ordering::Release);
        return r;
    }
    o.publish(b);
    0
}

unsafe extern "C" fn b_cond_destroy(c: *mut c_void) -> c_int {
    if c.is_null() {
        return EINVAL;
    }
    // SAFETY: the engine's condvar.
    let o = unsafe { Overlay::at(c) };
    match o.state.load(Ordering::Acquire) {
        UNSET => 0,
        READY => {
            let b = o.get();
            // SAFETY: our backing.
            unsafe { pthread_cond_destroy(b) };
            free_backing(b);
            // SAFETY: the second word of the caller's object.
            unsafe { std::ptr::write_volatile(o.backing, 0) };
            o.state.store(UNSET, Ordering::Release);
            0
        }
        _ => EINVAL,
    }
}

unsafe extern "C" fn b_cond_signal(c: *mut c_void) -> c_int {
    // SAFETY: the engine's condvar; the backing is a live host condvar.
    match unsafe { cond_backing(c) } {
        Ok(b) => unsafe { pthread_cond_signal(b) },
        Err(e) => e,
    }
}

unsafe extern "C" fn b_cond_broadcast(c: *mut c_void) -> c_int {
    // SAFETY: as above.
    match unsafe { cond_backing(c) } {
        Ok(b) => unsafe { pthread_cond_broadcast(b) },
        Err(e) => e,
    }
}

/// The engine's bionic mutexes are backed by a side table of real FreeBSD
/// mutexes (bionic's layout is not FreeBSD's), so a wait must operate on that
/// same real mutex, the one mutex_lock/unlock use, not on the bionic object.
/// Handing it the bionic bytes makes the wait never truly block or signal,
/// which livelocks the engine.
unsafe extern "C" fn b_cond_wait(c: *mut c_void, m: *mut c_void) -> c_int {
    // SAFETY: as above; `m` is the engine's mutex.
    match unsafe { cond_backing(c) } {
        Ok(b) => unsafe { pthread_cond_wait(b, bionic_mutex_real(m)) },
        Err(e) => e,
    }
}

unsafe extern "C" fn b_cond_timedwait(c: *mut c_void, m: *mut c_void, abstime: *const c_void) -> c_int {
    // `struct timespec` is the same 16 bytes on both sides.
    // SAFETY: as above.
    match unsafe { cond_backing(c) } {
        Ok(b) => timedwait_result_to_linux(unsafe { pthread_cond_timedwait(b, bionic_mutex_real(m), abstime) }),
        Err(e) => e,
    }
}

/// FreeBSD's ETIMEDOUT is 60 and Linux's is 110, and this is the one pthread
/// entry point whose host return value reaches the engine unmodified: it is the
/// only timed wait `libroblox.so` imports. boost's `do_wait_until` treats any
/// code other than 0 or its own ETIMEDOUT as a failure and throws
/// `condition_variable timed_wait failed`, so every *genuine* timeout on
/// FreeBSD became an exception in whatever engine task was waiting. EINVAL,
/// EPERM and EBUSY share values between the two, so only the timeout needs
/// mapping. `INTOXICATED_COND_ERRNO_RAW=1` restores the old passthrough as a
/// control; `INTOXICATED_TRACE_COND=1` reports how many timeouts were translated.
fn timedwait_result_to_linux(r: c_int) -> c_int {
    const HOST_ETIMEDOUT: c_int = 60;
    const LINUX_ETIMEDOUT: c_int = 110;
    static RAW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static TIMEOUTS: AtomicU64 = AtomicU64::new(0);
    if r != HOST_ETIMEDOUT {
        return r;
    }
    let n = TIMEOUTS.fetch_add(1, Ordering::Relaxed) + 1;
    if *TRACE.get_or_init(|| std::env::var_os("INTOXICATED_TRACE_COND").is_some()) && n.is_power_of_two() {
        eprintln!("[cond] pthread_cond_timedwait timeouts so far: {n}");
    }
    if *RAW.get_or_init(|| std::env::var_os("INTOXICATED_COND_ERRNO_RAW").is_some()) {
        r
    } else {
        LINUX_ETIMEDOUT
    }
}

// ------------------------------------------------------------- semaphores

/// The backing semaphore, created with a count of zero for a zero object.
unsafe fn sem_backing(s: *mut c_void) -> Result<*mut c_void, c_int> {
    if s.is_null() {
        return Err(EINVAL);
    }
    // SAFETY: the engine's 16-byte sem_t.
    unsafe { Overlay::at(s) }.backing_or_create(|b| unsafe { sem_init(b, 0, 0) })
}

unsafe extern "C" fn b_sem_init(s: *mut c_void, pshared: c_int, value: c_uint) -> c_int {
    if s.is_null() {
        return EINVAL;
    }
    // SAFETY: the engine's sem_t.
    let o = unsafe { Overlay::at(s) };
    if o.state.load(Ordering::Acquire) == READY {
        let old = o.get();
        // SAFETY: our backing.
        unsafe { sem_destroy(old) };
        free_backing(old);
    }
    let b = new_backing();
    // SAFETY: a fresh block, larger than FreeBSD's sem_t.
    let r = unsafe { sem_init(b, pshared, value) };
    if r != 0 {
        free_backing(b);
        o.state.store(UNSET, Ordering::Release);
        return r;
    }
    o.publish(b);
    0
}

unsafe extern "C" fn b_sem_destroy(s: *mut c_void) -> c_int {
    if s.is_null() {
        return EINVAL;
    }
    // SAFETY: the engine's sem_t.
    let o = unsafe { Overlay::at(s) };
    match o.state.load(Ordering::Acquire) {
        UNSET => 0,
        READY => {
            let b = o.get();
            // SAFETY: our backing.
            let r = unsafe { sem_destroy(b) };
            free_backing(b);
            // SAFETY: the second word of the caller's object.
            unsafe { std::ptr::write_volatile(o.backing, 0) };
            o.state.store(UNSET, Ordering::Release);
            r
        }
        _ => EINVAL,
    }
}

macro_rules! sem_op {
    ($name:ident, $host:ident) => {
        unsafe extern "C" fn $name(s: *mut c_void) -> c_int {
            // SAFETY: the engine's sem_t; the backing is a live host semaphore.
            match unsafe { sem_backing(s) } {
                Ok(b) => unsafe { $host(b) },
                Err(e) => e,
            }
        }
    };
}
sem_op!(b_sem_post, sem_post);
sem_op!(b_sem_wait, sem_wait);
sem_op!(b_sem_trywait, sem_trywait);

// ------------------------------------------------------------- once and keys

/// FreeBSD's `pthread_once_t` is a struct (int state + mutex pointer) with
/// different init semantics than bionic's 4-byte atomic int: forwarding
/// bionic's zero-initialised control makes FreeBSD's pthread_once treat it as
/// already run and SKIP the init routine. That silently breaks every
/// lazily-initialised subsystem (memory-category tracking, TSD keys). The
/// once protocol runs on the 4-byte slot itself (freebsd_libc_compat.c), with
/// no global guard: a single lock held across the init routine deadlocks when
/// an init routine nests another pthread_once (OpenSSL and the reflection
/// init do).
unsafe extern "C" fn b_pthread_once(control: *mut c_int, routine: Option<extern "C" fn()>) -> c_int {
    if control.is_null() {
        return EINVAL;
    }
    // SAFETY: the engine's 4-byte once control.
    unsafe { bionic_pthread_once(control, routine) }
}

unsafe extern "C" fn b_key_create(key: *mut c_int, dtor: Option<unsafe extern "C" fn(*mut c_void)>) -> c_int {
    if key.is_null() {
        return EINVAL;
    }
    let mut k: c_uint = 0;
    // SAFETY: a local out-parameter.
    let r = unsafe { pthread_key_create(&mut k, dtor) };
    if r == 0 {
        // SAFETY: the engine's 4-byte key.
        unsafe { *key = k as c_int };
    }
    r
}

unsafe extern "C" fn b_key_delete(key: c_int) -> c_int {
    // SAFETY: a key the engine got from `b_key_create`.
    unsafe { pthread_key_delete(key as c_uint) }
}

unsafe extern "C" fn b_getspecific(key: c_int) -> *mut c_void {
    // SAFETY: as above.
    unsafe { pthread_getspecific(key as c_uint) }
}

unsafe extern "C" fn b_setspecific(key: c_int, value: *const c_void) -> c_int {
    // SAFETY: as above.
    unsafe { pthread_setspecific(key as c_uint, value) }
}

/// The registered set.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    vec![
        ("pthread_cond_init", b_cond_init as *mut c_void),
        ("pthread_cond_destroy", b_cond_destroy as *mut c_void),
        ("pthread_cond_signal", b_cond_signal as *mut c_void),
        ("pthread_cond_broadcast", b_cond_broadcast as *mut c_void),
        ("pthread_cond_wait", b_cond_wait as *mut c_void),
        ("pthread_cond_timedwait", b_cond_timedwait as *mut c_void),
        ("sem_init", b_sem_init as *mut c_void),
        ("sem_destroy", b_sem_destroy as *mut c_void),
        ("sem_post", b_sem_post as *mut c_void),
        ("sem_wait", b_sem_wait as *mut c_void),
        ("sem_trywait", b_sem_trywait as *mut c_void),
        ("pthread_once", b_pthread_once as *mut c_void),
        ("pthread_key_create", b_key_create as *mut c_void),
        ("pthread_key_delete", b_key_delete as *mut c_void),
        ("pthread_getspecific", b_getspecific as *mut c_void),
        ("pthread_setspecific", b_setspecific as *mut c_void),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    /// A bionic object's bytes, 8-aligned.
    #[repr(C, align(8))]
    struct Obj<const N: usize>([u8; N]);

    fn fn_of(name: &str) -> *mut c_void {
        crate::bionic::function_overrides().into_iter().rev().find(|(n, _)| *n == name).unwrap().1
    }

    #[test]
    fn the_overlay_uses_sixteen_bytes_and_leaves_the_rest() {
        let mut c = Obj::<48>([0xab; 48]);
        c.0[..16].fill(0);
        let p = c.0.as_mut_ptr().cast::<c_void>();
        unsafe {
            assert_eq!(b_cond_signal(p), 0);
            assert_eq!(b_cond_broadcast(p), 0);
            assert!(c.0[16..].iter().all(|&b| b == 0xab), "touched bytes past the overlay");
            assert_eq!(b_cond_destroy(p), 0);
            assert_eq!(b_cond_init(p, std::ptr::null()), 0);
            assert_eq!(b_cond_destroy(p), 0);
            assert_eq!(b_cond_init(p, std::ptr::null()), 0);
            assert_eq!(b_cond_init(p, std::ptr::null()), 0, "re-init frees the old backing");
            assert_eq!(b_cond_destroy(p), 0);
            assert!(c.0[..8].iter().all(|&b| b == 0), "destroy leaves the zero state");
            assert_eq!(b_cond_init(std::ptr::null_mut(), std::ptr::null()), EINVAL);
            assert_eq!(b_cond_signal(std::ptr::null_mut()), EINVAL);
        }
        // Someone else's bytes are refused, not adopted.
        let mut foreign = Obj::<48>([0x5a; 48]);
        assert_eq!(unsafe { b_cond_signal(foreign.0.as_mut_ptr().cast()) }, EINVAL);
    }

    #[test]
    fn a_mutex_cond_sequence_hands_off_between_threads() {
        // The engine's objects: a zeroed bionic mutex (40 bytes) and condvar
        // (48 bytes), used through the registered entry points only.
        type MutexFn = unsafe extern "C" fn(*mut c_void) -> c_int;
        let lock: MutexFn = unsafe { std::mem::transmute(fn_of("pthread_mutex_lock")) };
        let unlock: MutexFn = unsafe { std::mem::transmute(fn_of("pthread_mutex_unlock")) };
        let mutex = Box::leak(Box::new(Obj::<40>([0; 40]))).0.as_mut_ptr() as usize;
        let cond = Box::leak(Box::new(Obj::<48>([0; 48]))).0.as_mut_ptr() as usize;
        let flag = Arc::new(AtomicU32::new(0));
        let f2 = flag.clone();
        let waiter = std::thread::spawn(move || unsafe {
            assert_eq!(lock(mutex as *mut c_void), 0);
            while f2.load(Ordering::SeqCst) == 0 {
                assert_eq!(b_cond_wait(cond as *mut c_void, mutex as *mut c_void), 0);
            }
            f2.store(2, Ordering::SeqCst);
            assert_eq!(unlock(mutex as *mut c_void), 0);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        unsafe {
            assert_eq!(lock(mutex as *mut c_void), 0);
            flag.store(1, Ordering::SeqCst);
            assert_eq!(b_cond_signal(cond as *mut c_void), 0);
            assert_eq!(unlock(mutex as *mut c_void), 0);
        }
        waiter.join().unwrap();
        assert_eq!(flag.load(Ordering::SeqCst), 2);
        // A timed wait that expires reports Linux's ETIMEDOUT, 110.
        let past = [0i64, 1];
        unsafe {
            assert_eq!(lock(mutex as *mut c_void), 0);
            assert_eq!(b_cond_timedwait(cond as *mut c_void, mutex as *mut c_void, past.as_ptr().cast()), 110);
            assert_eq!(unlock(mutex as *mut c_void), 0);
        }
        // The mutex is recursive and its 40 bytes are never written.
        let m = unsafe { std::slice::from_raw_parts(mutex as *const u8, 40) };
        assert!(m.iter().all(|&b| b == 0));
    }

    static ONCE_RUNS: AtomicU32 = AtomicU32::new(0);
    extern "C" fn once_routine() {
        ONCE_RUNS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn once_runs_the_routine_once_per_control() {
        let mut a: c_int = 0;
        let mut b: c_int = 0;
        unsafe {
            assert_eq!(b_pthread_once(&mut a, Some(once_routine)), 0);
            assert_eq!(b_pthread_once(&mut a, Some(once_routine)), 0);
            assert_eq!(ONCE_RUNS.load(Ordering::SeqCst), 1);
            assert_eq!(a, 2, "bionic's done state");
            assert_eq!(b_pthread_once(&mut b, Some(once_routine)), 0);
            assert_eq!(ONCE_RUNS.load(Ordering::SeqCst), 2);
            assert_eq!(b_pthread_once(std::ptr::null_mut(), Some(once_routine)), EINVAL);
        }
    }

    #[test]
    fn semaphores_count() {
        let mut s = Obj::<16>([0; 16]);
        let p = s.0.as_mut_ptr().cast::<c_void>();
        unsafe {
            assert_eq!(b_sem_init(p, 0, 1), 0);
            assert_eq!(b_sem_wait(p), 0);
            assert_ne!(b_sem_trywait(p), 0);
            assert_eq!(b_sem_post(p), 0);
            assert_eq!(b_sem_trywait(p), 0);
            assert_eq!(b_sem_destroy(p), 0);
            assert_eq!(b_sem_init(std::ptr::null_mut(), 0, 0), EINVAL);
        }
    }

    #[test]
    fn keys_are_per_thread() {
        let mut key: c_int = -1;
        unsafe {
            assert_eq!(b_key_create(&mut key, None), 0);
            assert!(key >= 0);
            assert!(b_getspecific(key).is_null());
            assert_eq!(b_setspecific(key, 0x1234 as *const c_void), 0);
            assert_eq!(b_getspecific(key) as usize, 0x1234);
            let k = key;
            let other = std::thread::spawn(move || b_getspecific(k) as usize).join().unwrap();
            assert_eq!(other, 0);
            assert_eq!(b_key_delete(key), 0);
            assert_eq!(b_key_create(std::ptr::null_mut(), None), EINVAL);
        }
    }
}
