//! Signals across the bionic/FreeBSD boundary.
//!
//! The engine speaks Linux: signal numbers (SIGUSR1 is 10, SIGBUS 7, SIGCHLD
//! 17, real-time signals from 32), `how` values (SIG_BLOCK 0), `sa_flags` bits
//! (SA_SIGINFO 4, SA_RESTART 0x10000000), bionic's 8-byte `sigset_t` (bit n-1
//! is signal n) and bionic's 32-byte `struct sigaction` (flags at 0, handler
//! at 8, mask at 16, restorer at 24). FreeBSD numbers all of those
//! differently, and its `sigset_t` is 16 bytes. Everything here translates in
//! both directions; the tables are in [`lx_to_fb_signal`] and
//! [`lx_flags_to_fb`].
//!
//! # Handlers
//!
//! The kernel calls a handler with FreeBSD's signal number, `siginfo_t` and
//! `ucontext_t`. An engine handler is therefore never installed directly: the
//! host gets [`dispatch`], which looks up the engine's action for the Linux
//! number and calls it with the Linux number and, for `SA_SIGINFO` handlers,
//! a Linux-layout `siginfo_t` and `ucontext_t` built from the kernel's.
//! Registers, the FPU image and the signal mask the handler changes in that
//! context are written back before the kernel resumes the thread, so a handler
//! that skips an instruction works. bionic's `SA_RESTORER` and `sa_restorer`
//! are kept for the engine to read back and never reach the kernel, which
//! returns through its own trampoline.
//!
//! # Signals the runtime owns
//!
//! SIGILL is the raw-syscall trap's (`native/syscall_trap.c`): the 16
//! neutralised `syscall` sites of `libroblox.so` raise it on every call, and
//! the trap must stay the handler that runs for them. SIGINT and SIGTERM end
//! the run through the pump's handlers (`android/looper/pump.rs`). For these
//! three, once a handler this module did not install is in place, the
//! engine's action is recorded and reported back to it but not installed; and
//! they are taken out of every mask the engine applies, because a blocked
//! SIGILL at a trap site kills the process and a blocked SIGTERM could never
//! end it. [`prepare_for_raw_syscall_trap`] puts [`dispatch`] on SIGILL before
//! the trap is installed, so the trap's chain to "the previous handler" for a
//! SIGILL that is not one of its sites reaches whatever the engine installs,
//! before or after.

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;
const SIG_ERR: usize = usize::MAX;

/// Linux errno values set here; all agree with FreeBSD's.
const EINVAL: c_int = 22;

// ------------------------------------------------------------------ numbers

/// Linux `sa_flags` bits (bionic's uapi `asm/signal.h`).
pub mod lx {
    pub const SA_NOCLDSTOP: u32 = 0x1;
    pub const SA_NOCLDWAIT: u32 = 0x2;
    pub const SA_SIGINFO: u32 = 0x4;
    pub const SA_RESTORER: u32 = 0x0400_0000;
    pub const SA_ONSTACK: u32 = 0x0800_0000;
    pub const SA_RESTART: u32 = 0x1000_0000;
    pub const SA_NODEFER: u32 = 0x4000_0000;
    pub const SA_RESETHAND: u32 = 0x8000_0000;
    pub const SIG_BLOCK: i32 = 0;
    pub const SIG_UNBLOCK: i32 = 1;
    pub const SIG_SETMASK: i32 = 2;
    pub const SS_ONSTACK: i32 = 1;
    pub const SS_DISABLE: i32 = 2;
    pub const SS_AUTODISARM: i32 = 1 << 31;
    pub const SIGILL: i32 = 4;
}

/// FreeBSD values (`<sys/signal.h>`, measured with a native probe).
pub mod fb {
    pub const SA_ONSTACK: i32 = 0x1;
    pub const SA_RESTART: i32 = 0x2;
    pub const SA_RESETHAND: i32 = 0x4;
    pub const SA_NOCLDSTOP: i32 = 0x8;
    pub const SA_NODEFER: i32 = 0x10;
    pub const SA_NOCLDWAIT: i32 = 0x20;
    pub const SA_SIGINFO: i32 = 0x40;
    pub const SIG_BLOCK: i32 = 1;
    pub const SIG_UNBLOCK: i32 = 2;
    pub const SIG_SETMASK: i32 = 3;
    pub const SS_ONSTACK: i32 = 1;
    pub const SS_DISABLE: i32 = 4;
    pub const SIGINT: i32 = 2;
    pub const SIGILL: i32 = 4;
    pub const SIGFPE: i32 = 8;
    pub const SIGBUS: i32 = 10;
    pub const SIGSEGV: i32 = 11;
    pub const SIGTRAP: i32 = 5;
    pub const SIGTERM: i32 = 15;
    pub const SIGCHLD: i32 = 20;
    pub const SIGIO: i32 = 23;
    pub const SIGRTMIN: i32 = 65;
    pub const SI_USER: i32 = 0x10001;
    pub const SI_QUEUE: i32 = 0x10002;
    pub const SI_TIMER: i32 = 0x10003;
    pub const SI_ASYNCIO: i32 = 0x10004;
    pub const SI_MESGQ: i32 = 0x10005;
    pub const SI_KERNEL: i32 = 0x10006;
    pub const SI_LWP: i32 = 0x10007;
    pub const FPE_INTOVF: i32 = 1;
    pub const FPE_INTDIV: i32 = 2;
    pub const MC_FPFMT_XMM: i64 = 0x10002;
    pub const MC_FPOWNED_NONE: i64 = 0x20000;
}

/// The FreeBSD number of Linux signal `n`; `None` for SIGSTKFLT (16) and
/// SIGPWR (30), which FreeBSD does not have, and for anything outside 1..=64.
/// Linux real-time signals 32..=64 are FreeBSD's 65..=97.
pub fn lx_to_fb_signal(n: c_int) -> Option<c_int> {
    const LOW: [i32; 32] = [
        0, 1, 2, 3, 4, 5, 6, 10, 8, 9, 30, 11, 31, 13, 14, 15, // 0..=15
        0, 20, 19, 17, 18, 21, 22, 16, 24, 25, 26, 27, 28, 23, 0, 12, // 16..=31
    ];
    match n {
        1..=31 => Some(LOW[n as usize]).filter(|&f| f != 0),
        32..=64 => Some(n + 33),
        _ => None,
    }
}

/// The Linux number of FreeBSD signal `n`; `None` for SIGEMT (7), SIGINFO
/// (29), SIGTHR (32), SIGLIBRT (33) and real-time signals past Linux's 64.
pub fn fb_to_lx_signal(n: c_int) -> Option<c_int> {
    match n {
        1..=31 => (1..=31).find(|&l| lx_to_fb_signal(l) == Some(n)),
        65..=97 => Some(n - 33),
        _ => None,
    }
}

/// Linux `sa_flags` to FreeBSD's; `SA_RESTORER` and unknown bits are dropped.
pub fn lx_flags_to_fb(flags: u32) -> c_int {
    const MAP: [(u32, i32); 7] = [
        (lx::SA_NOCLDSTOP, fb::SA_NOCLDSTOP),
        (lx::SA_NOCLDWAIT, fb::SA_NOCLDWAIT),
        (lx::SA_SIGINFO, fb::SA_SIGINFO),
        (lx::SA_ONSTACK, fb::SA_ONSTACK),
        (lx::SA_RESTART, fb::SA_RESTART),
        (lx::SA_NODEFER, fb::SA_NODEFER),
        (lx::SA_RESETHAND, fb::SA_RESETHAND),
    ];
    MAP.iter().filter(|(l, _)| flags & l != 0).fold(0, |acc, (_, f)| acc | f)
}

/// FreeBSD `sa_flags` to Linux's.
pub fn fb_flags_to_lx(flags: c_int) -> u32 {
    const MAP: [(i32, u32); 7] = [
        (fb::SA_NOCLDSTOP, lx::SA_NOCLDSTOP),
        (fb::SA_NOCLDWAIT, lx::SA_NOCLDWAIT),
        (fb::SA_SIGINFO, lx::SA_SIGINFO),
        (fb::SA_ONSTACK, lx::SA_ONSTACK),
        (fb::SA_RESTART, lx::SA_RESTART),
        (fb::SA_NODEFER, lx::SA_NODEFER),
        (fb::SA_RESETHAND, lx::SA_RESETHAND),
    ];
    MAP.iter().filter(|(f, _)| flags & f != 0).fold(0, |acc, (_, l)| acc | l)
}

/// Linux `how` to FreeBSD's.
pub fn lx_how_to_fb(how: c_int) -> Option<c_int> {
    match how {
        lx::SIG_BLOCK => Some(fb::SIG_BLOCK),
        lx::SIG_UNBLOCK => Some(fb::SIG_UNBLOCK),
        lx::SIG_SETMASK => Some(fb::SIG_SETMASK),
        _ => None,
    }
}

/// The FreeBSD signals the runtime keeps for itself; see the module doc.
const RUNTIME_OWNED: [c_int; 3] = [fb::SIGILL, fb::SIGINT, fb::SIGTERM];

/// A bionic set as a FreeBSD one. With `for_blocking`, the runtime's own
/// signals are left out.
pub fn lx_set_to_fb(set: u64, for_blocking: bool) -> libc::sigset_t {
    // SAFETY: sigemptyset initialises the whole set.
    let mut out: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe { libc::sigemptyset(&mut out) };
    for n in 1..=64 {
        if set & (1u64 << (n - 1)) == 0 {
            continue;
        }
        if let Some(f) = lx_to_fb_signal(n) {
            if for_blocking && RUNTIME_OWNED.contains(&f) {
                continue;
            }
            // SAFETY: `out` is initialised and `f` is a valid signal number.
            unsafe { libc::sigaddset(&mut out, f) };
        }
    }
    out
}

/// A FreeBSD set as a bionic one; signals Linux has no number for are lost.
pub fn fb_set_to_lx(set: &libc::sigset_t) -> u64 {
    let mut out = 0u64;
    for f in 1..=127 {
        // SAFETY: `set` is an initialised set and `f` is in range.
        if unsafe { libc::sigismember(set, f) } == 1 {
            if let Some(l) = fb_to_lx_signal(f) {
                out |= 1u64 << (l - 1);
            }
        }
    }
    out
}

// -------------------------------------------------------- the bionic structs

/// bionic's `struct sigaction` on x86-64: 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BionicSigaction {
    pub sa_flags: c_int,
    pub sa_handler: usize,
    pub sa_mask: u64,
    pub sa_restorer: usize,
}

/// bionic's `stack_t`: 24 bytes, flags before size.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BionicStack {
    pub ss_sp: *mut c_void,
    pub ss_flags: c_int,
    pub ss_size: usize,
}

/// bionic's `siginfo_t`: 128 bytes; the union starts at 16.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LxSiginfo {
    pub si_signo: c_int,
    pub si_errno: c_int,
    pub si_code: c_int,
    _pad: c_int,
    pub fields: [u64; 14],
}

/// The registers of bionic's `mcontext_t`, in its `REG_*` order.
pub mod reg {
    pub const R8: usize = 0;
    pub const R9: usize = 1;
    pub const R10: usize = 2;
    pub const R11: usize = 3;
    pub const R12: usize = 4;
    pub const R13: usize = 5;
    pub const R14: usize = 6;
    pub const R15: usize = 7;
    pub const RDI: usize = 8;
    pub const RSI: usize = 9;
    pub const RBP: usize = 10;
    pub const RBX: usize = 11;
    pub const RDX: usize = 12;
    pub const RAX: usize = 13;
    pub const RCX: usize = 14;
    pub const RSP: usize = 15;
    pub const RIP: usize = 16;
    pub const EFL: usize = 17;
    pub const CSGSFS: usize = 18;
    pub const ERR: usize = 19;
    pub const TRAPNO: usize = 20;
    pub const OLDMASK: usize = 21;
    pub const CR2: usize = 22;
    pub const NGREG: usize = 23;
}

/// bionic's `ucontext_t` on x86-64: 816 bytes.
#[repr(C)]
pub struct LxUcontext {
    pub uc_flags: u64,
    pub uc_link: usize,
    pub uc_stack: BionicStack,
    pub gregs: [i64; reg::NGREG],
    pub fpregs: *mut u8,
    _reserved1: [u64; 8],
    pub uc_sigmask: u64,
    pub fpregs_mem: [u8; 512],
}

/// FreeBSD amd64 `mcontext_t` (800 bytes), by the offsets `<machine/ucontext.h>` gives.
#[repr(C, align(16))]
struct FbMcontext {
    onstack: i64,
    rdi: i64,
    rsi: i64,
    rdx: i64,
    rcx: i64,
    r8: i64,
    r9: i64,
    rax: i64,
    rbx: i64,
    rbp: i64,
    r10: i64,
    r11: i64,
    r12: i64,
    r13: i64,
    r14: i64,
    r15: i64,
    trapno: u32,
    fs: u16,
    gs: u16,
    addr: i64,
    flags: u32,
    es: u16,
    ds: u16,
    err: i64,
    rip: i64,
    cs: i64,
    rflags: i64,
    rsp: i64,
    ss: i64,
    len: i64,
    fpformat: i64,
    ownedfp: i64,
    fpstate: [u8; 512],
    fsbase: i64,
    gsbase: i64,
    xfpustate: i64,
    xfpustate_len: i64,
    spare: [i64; 4],
}

/// FreeBSD `stack_t`: size before flags.
#[repr(C)]
#[derive(Clone, Copy)]
struct FbStack {
    ss_sp: *mut c_void,
    ss_size: usize,
    ss_flags: c_int,
}

/// FreeBSD amd64 `ucontext_t` (880 bytes).
#[repr(C, align(16))]
struct FbUcontext {
    uc_sigmask: libc::sigset_t,
    uc_mcontext: FbMcontext,
    uc_link: usize,
    uc_stack: FbStack,
    uc_flags: c_int,
    spare: [c_int; 4],
}

// ---------------------------------------------------------- the engine's view

/// One signal's action as the engine set it.
struct Slot {
    set: AtomicBool,
    handler: AtomicUsize,
    flags: AtomicU64,
    mask: AtomicU64,
    restorer: AtomicUsize,
}

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY_SLOT: Slot = Slot {
    set: AtomicBool::new(false),
    handler: AtomicUsize::new(SIG_DFL),
    flags: AtomicU64::new(0),
    mask: AtomicU64::new(0),
    restorer: AtomicUsize::new(0),
};

/// Indexed by Linux signal number.
static ENGINE: [Slot; 65] = [EMPTY_SLOT; 65];

fn engine_action(lsig: c_int) -> Option<BionicSigaction> {
    let s = &ENGINE[lsig as usize];
    if !s.set.load(Ordering::Acquire) {
        return None;
    }
    Some(BionicSigaction {
        sa_flags: s.flags.load(Ordering::Relaxed) as u32 as c_int,
        sa_handler: s.handler.load(Ordering::Relaxed),
        sa_mask: s.mask.load(Ordering::Relaxed),
        sa_restorer: s.restorer.load(Ordering::Relaxed),
    })
}

fn store_engine_action(lsig: c_int, a: &BionicSigaction) {
    let s = &ENGINE[lsig as usize];
    s.flags.store(a.sa_flags as u32 as u64, Ordering::Relaxed);
    s.mask.store(a.sa_mask, Ordering::Relaxed);
    s.restorer.store(a.sa_restorer, Ordering::Relaxed);
    s.handler.store(a.sa_handler, Ordering::Release);
    s.set.store(true, Ordering::Release);
}

fn host_action(fsig: c_int) -> Option<libc::sigaction> {
    // SAFETY: a query: no new action, a valid out-parameter.
    let mut cur: libc::sigaction = unsafe { std::mem::zeroed() };
    (unsafe { libc::sigaction(fsig, std::ptr::null(), &mut cur) } == 0).then_some(cur)
}

fn is_dispatch(handler: usize) -> bool {
    handler == dispatch as extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) as usize
}

/// A handler someone other than this module put on a runtime-owned signal.
fn runtime_holds(fsig: c_int) -> bool {
    RUNTIME_OWNED.contains(&fsig)
        && host_action(fsig).is_some_and(|a| {
            let h = a.sa_sigaction;
            h != SIG_DFL && h != SIG_IGN && !is_dispatch(h)
        })
}

fn trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("INTOXICATED_TRACE_SIGNALS").is_some())
}

/// What the engine reads back for a signal it never set: the host's action,
/// translated, or the default for a handler the runtime installed.
fn reported_host_action(fsig: Option<c_int>) -> BionicSigaction {
    let Some(a) = fsig.and_then(host_action) else {
        return BionicSigaction::default();
    };
    match a.sa_sigaction {
        h @ (SIG_DFL | SIG_IGN) => BionicSigaction {
            sa_flags: fb_flags_to_lx(a.sa_flags) as c_int,
            sa_handler: h,
            sa_mask: fb_set_to_lx(&a.sa_mask),
            sa_restorer: 0,
        },
        _ => BionicSigaction::default(),
    }
}

fn set_errno(e: c_int) {
    // SAFETY: this thread's errno slot.
    unsafe { *libc::__error() = e };
}

/// Install `act` for Linux signal `lsig`; see the module doc for which
/// signals only have their action recorded.
fn install(lsig: c_int, act: &BionicSigaction) -> Result<(), c_int> {
    let fsig = lx_to_fb_signal(lsig);
    let virtual_only = match fsig {
        None => true,
        Some(f) => runtime_holds(f),
    };
    if trace_on() {
        eprintln!(
            "[signals] sigaction(linux {lsig} = freebsd {fsig:?}, handler {:#x}, flags {:#x}, mask {:#x}){}",
            act.sa_handler,
            act.sa_flags as u32,
            act.sa_mask,
            if virtual_only { ": recorded, not installed" } else { "" }
        );
    }
    if let (Some(f), false) = (fsig, virtual_only) {
        // SAFETY: zeroed is a valid sigaction; every field is set below.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        let flags = act.sa_flags as u32;
        if act.sa_handler == SIG_DFL || act.sa_handler == SIG_IGN {
            sa.sa_sigaction = act.sa_handler;
            sa.sa_flags = lx_flags_to_fb(flags & !lx::SA_SIGINFO);
        } else {
            sa.sa_sigaction = dispatch as extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) as usize;
            sa.sa_flags = lx_flags_to_fb(flags) | fb::SA_SIGINFO;
        }
        sa.sa_mask = lx_set_to_fb(act.sa_mask, true);
        // The engine's action has to be in place before the kernel can call
        // `dispatch` for it.
        let previous = engine_action(lsig);
        store_engine_action(lsig, act);
        // SAFETY: a fully initialised action for a valid FreeBSD signal.
        if unsafe { libc::sigaction(f, &sa, std::ptr::null_mut()) } != 0 {
            let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(EINVAL);
            match previous {
                Some(p) => store_engine_action(lsig, &p),
                None => ENGINE[lsig as usize].set.store(false, Ordering::Release),
            }
            return Err(e);
        }
        return Ok(());
    }
    store_engine_action(lsig, act);
    Ok(())
}

/// bionic `sigaction`.
unsafe extern "C" fn b_sigaction(lsig: c_int, act: *const BionicSigaction, old: *mut BionicSigaction) -> c_int {
    // SIGKILL (9) and SIGSTOP (19) can be queried but not changed.
    if !(1..=64).contains(&lsig) || (!act.is_null() && (lsig == 9 || lsig == 19)) {
        set_errno(EINVAL);
        return -1;
    }
    let before = engine_action(lsig).unwrap_or_else(|| reported_host_action(lx_to_fb_signal(lsig)));
    if !act.is_null() {
        // SAFETY: the caller passes a readable bionic sigaction.
        let a = unsafe { std::ptr::read_unaligned(act) };
        if let Err(e) = install(lsig, &a) {
            set_errno(e);
            return -1;
        }
    }
    if !old.is_null() {
        // SAFETY: the caller passes a writable bionic sigaction.
        unsafe { std::ptr::write_unaligned(old, before) };
    }
    0
}

/// bionic `signal` (BSD semantics: the handler stays, calls restart).
unsafe extern "C" fn b_signal(lsig: c_int, handler: usize) -> usize {
    let act = BionicSigaction { sa_flags: lx::SA_RESTART as c_int, sa_handler: handler, sa_mask: 0, sa_restorer: 0 };
    let mut old = BionicSigaction::default();
    // SAFETY: both are valid structs.
    if unsafe { b_sigaction(lsig, &act, &mut old) } != 0 {
        return SIG_ERR;
    }
    old.sa_handler
}

/// bionic `raise`.
unsafe extern "C" fn b_raise(lsig: c_int) -> c_int {
    if lsig == 0 {
        return 0;
    }
    let Some(f) = lx_to_fb_signal(lsig) else {
        set_errno(EINVAL);
        return -1;
    };
    // SAFETY: a valid FreeBSD signal number.
    unsafe { libc::raise(f) }
}

fn mask_call(how: c_int, set: *const u64, old: *mut u64) -> Result<(), c_int> {
    // With no new set, `how` is not looked at (as on Linux).
    let fb_how = if set.is_null() { fb::SIG_BLOCK } else { lx_how_to_fb(how).ok_or(EINVAL)? };
    let fb_set: Option<libc::sigset_t> = (!set.is_null()).then(|| {
        // SAFETY: the caller passes a readable 8-byte set.
        let s = unsafe { std::ptr::read_unaligned(set) };
        lx_set_to_fb(s, how != lx::SIG_UNBLOCK)
    });
    let set_ptr = fb_set.as_ref().map_or(std::ptr::null(), |s| s as *const libc::sigset_t);
    // SAFETY: zeroed is a valid sigset to receive into.
    let mut fb_old: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: valid pointers or null for the set.
    let r = unsafe { libc::pthread_sigmask(fb_how, set_ptr, &mut fb_old) };
    if r != 0 {
        return Err(r);
    }
    if !old.is_null() {
        // SAFETY: the caller passes a writable 8-byte set.
        unsafe { std::ptr::write_unaligned(old, fb_set_to_lx(&fb_old)) };
    }
    Ok(())
}

/// bionic `sigprocmask`: -1 and errno on failure.
unsafe extern "C" fn b_sigprocmask(how: c_int, set: *const u64, old: *mut u64) -> c_int {
    match mask_call(how, set, old) {
        Ok(()) => 0,
        Err(e) => {
            set_errno(e);
            -1
        }
    }
}

/// bionic `pthread_sigmask`: the error number is the return value.
unsafe extern "C" fn b_pthread_sigmask(how: c_int, set: *const u64, old: *mut u64) -> c_int {
    match mask_call(how, set, old) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// bionic `sigaltstack`: the same three fields in another order, and
/// SS_DISABLE is 2 on Linux and 4 on FreeBSD.
unsafe extern "C" fn b_sigaltstack(ss: *const BionicStack, old: *mut BionicStack) -> c_int {
    let mut fb_ss = FbStack { ss_sp: std::ptr::null_mut(), ss_size: 0, ss_flags: 0 };
    let ss_ptr = if ss.is_null() {
        std::ptr::null()
    } else {
        // SAFETY: the caller passes a readable bionic stack_t.
        let s = unsafe { std::ptr::read_unaligned(ss) };
        if s.ss_flags & !(lx::SS_DISABLE | lx::SS_AUTODISARM) != 0 {
            set_errno(EINVAL);
            return -1;
        }
        fb_ss.ss_sp = s.ss_sp;
        fb_ss.ss_size = s.ss_size;
        fb_ss.ss_flags = if s.ss_flags & lx::SS_DISABLE != 0 { fb::SS_DISABLE } else { 0 };
        &fb_ss as *const FbStack
    };
    let mut fb_old = FbStack { ss_sp: std::ptr::null_mut(), ss_size: 0, ss_flags: 0 };
    // SAFETY: FbStack is FreeBSD's stack_t; both pointers are valid or null.
    let r = unsafe { libc::sigaltstack(ss_ptr.cast(), (&mut fb_old as *mut FbStack).cast()) };
    if r != 0 {
        return -1;
    }
    if !old.is_null() {
        let mut flags = 0;
        if fb_old.ss_flags & fb::SS_ONSTACK != 0 {
            flags |= lx::SS_ONSTACK;
        }
        if fb_old.ss_flags & fb::SS_DISABLE != 0 {
            flags |= lx::SS_DISABLE;
        }
        // SAFETY: the caller passes a writable bionic stack_t.
        unsafe { std::ptr::write_unaligned(old, BionicStack { ss_sp: fb_old.ss_sp, ss_flags: flags, ss_size: fb_old.ss_size }) };
    }
    0
}

// The five set operations work on the caller's 8 bytes only.
fn bit(n: c_int) -> Option<u64> {
    (1..=64).contains(&n).then(|| 1u64 << (n - 1))
}

unsafe extern "C" fn b_sigemptyset(set: *mut u64) -> c_int {
    if set.is_null() {
        return -1;
    }
    // SAFETY: the caller passes a writable 8-byte set.
    unsafe { std::ptr::write_unaligned(set, 0) };
    0
}

unsafe extern "C" fn b_sigfillset(set: *mut u64) -> c_int {
    if set.is_null() {
        return -1;
    }
    // SAFETY: as above.
    unsafe { std::ptr::write_unaligned(set, u64::MAX) };
    0
}

unsafe extern "C" fn b_sigaddset(set: *mut u64, n: c_int) -> c_int {
    match (set.is_null(), bit(n)) {
        (false, Some(b)) => {
            // SAFETY: as above.
            unsafe { std::ptr::write_unaligned(set, std::ptr::read_unaligned(set) | b) };
            0
        }
        _ => -1,
    }
}

unsafe extern "C" fn b_sigdelset(set: *mut u64, n: c_int) -> c_int {
    match (set.is_null(), bit(n)) {
        (false, Some(b)) => {
            // SAFETY: as above.
            unsafe { std::ptr::write_unaligned(set, std::ptr::read_unaligned(set) & !b) };
            0
        }
        _ => -1,
    }
}

unsafe extern "C" fn b_sigismember(set: *const u64, n: c_int) -> c_int {
    match (set.is_null(), bit(n)) {
        // SAFETY: as above.
        (false, Some(b)) => c_int::from(unsafe { std::ptr::read_unaligned(set) } & b != 0),
        _ => -1,
    }
}

// --------------------------------------------------------------- delivery

fn lx_code(fsig: c_int, code: c_int) -> c_int {
    match code {
        fb::SI_USER => 0,
        fb::SI_KERNEL | 0 => 0x80,
        fb::SI_QUEUE => -1,
        fb::SI_TIMER => -2,
        fb::SI_MESGQ => -3,
        fb::SI_ASYNCIO => -4,
        fb::SI_LWP => -6,
        // FreeBSD and Linux swap the two integer FPE codes.
        fb::FPE_INTOVF if fsig == fb::SIGFPE => 2,
        fb::FPE_INTDIV if fsig == fb::SIGFPE => 1,
        c => c,
    }
}

/// The Linux `siginfo_t` for a FreeBSD one.
fn lx_siginfo(lsig: c_int, fsig: c_int, si: *const libc::siginfo_t) -> LxSiginfo {
    let mut out = LxSiginfo { si_signo: lsig, si_errno: 0, si_code: 0x80, _pad: 0, fields: [0; 14] };
    if si.is_null() {
        return out;
    }
    let raw = si.cast::<u8>();
    // SAFETY: the kernel's siginfo_t, 80 bytes, read at its documented offsets.
    let (errno, code, pid, uid, status, addr, value, reason) = unsafe {
        (
            std::ptr::read_unaligned(raw.add(4).cast::<c_int>()),
            std::ptr::read_unaligned(raw.add(8).cast::<c_int>()),
            std::ptr::read_unaligned(raw.add(12).cast::<c_int>()),
            std::ptr::read_unaligned(raw.add(16).cast::<u32>()),
            std::ptr::read_unaligned(raw.add(20).cast::<c_int>()),
            std::ptr::read_unaligned(raw.add(24).cast::<u64>()),
            std::ptr::read_unaligned(raw.add(32).cast::<u64>()),
            std::ptr::read_unaligned(raw.add(40).cast::<u64>()),
        )
    };
    out.si_errno = errno;
    out.si_code = lx_code(fsig, code);
    let fault = code > 0 && code < 0x10000 && matches!(fsig, fb::SIGSEGV | fb::SIGBUS | fb::SIGILL | fb::SIGFPE | fb::SIGTRAP);
    if fault {
        out.fields[0] = addr; // si_addr
    } else if fsig == fb::SIGCHLD {
        out.fields[0] = u64::from(pid as u32) | (u64::from(uid) << 32);
        out.fields[1] = status as u32 as u64;
    } else if fsig == fb::SIGIO && code > 0 && code < 0x10000 {
        out.fields[0] = reason; // si_band
    } else if code == fb::SI_TIMER {
        out.fields[0] = reason & 0xffff_ffff; // timer id, overrun
        out.fields[1] = value;
    } else {
        out.fields[0] = u64::from(pid as u32) | (u64::from(uid) << 32);
        out.fields[1] = value;
    }
    out
}

/// The Linux register set for a FreeBSD machine context.
fn lx_gregs(m: &FbMcontext) -> [i64; reg::NGREG] {
    let mut g = [0i64; reg::NGREG];
    g[reg::R8] = m.r8;
    g[reg::R9] = m.r9;
    g[reg::R10] = m.r10;
    g[reg::R11] = m.r11;
    g[reg::R12] = m.r12;
    g[reg::R13] = m.r13;
    g[reg::R14] = m.r14;
    g[reg::R15] = m.r15;
    g[reg::RDI] = m.rdi;
    g[reg::RSI] = m.rsi;
    g[reg::RBP] = m.rbp;
    g[reg::RBX] = m.rbx;
    g[reg::RDX] = m.rdx;
    g[reg::RAX] = m.rax;
    g[reg::RCX] = m.rcx;
    g[reg::RSP] = m.rsp;
    g[reg::RIP] = m.rip;
    g[reg::EFL] = m.rflags;
    g[reg::CSGSFS] = (m.cs & 0xffff) | (i64::from(m.gs) << 16) | (i64::from(m.fs) << 32);
    g[reg::ERR] = m.err;
    g[reg::TRAPNO] = i64::from(m.trapno);
    g[reg::CR2] = m.addr;
    g
}

/// Write back what an engine handler changed in its Linux context.
fn apply_gregs(m: &mut FbMcontext, before: &[i64; reg::NGREG], after: &[i64; reg::NGREG]) {
    let changed = |i: usize| (before[i] != after[i]).then_some(after[i]);
    let pairs: [(usize, &mut i64); 18] = [
        (reg::R8, &mut m.r8),
        (reg::R9, &mut m.r9),
        (reg::R10, &mut m.r10),
        (reg::R11, &mut m.r11),
        (reg::R12, &mut m.r12),
        (reg::R13, &mut m.r13),
        (reg::R14, &mut m.r14),
        (reg::R15, &mut m.r15),
        (reg::RDI, &mut m.rdi),
        (reg::RSI, &mut m.rsi),
        (reg::RBP, &mut m.rbp),
        (reg::RBX, &mut m.rbx),
        (reg::RDX, &mut m.rdx),
        (reg::RAX, &mut m.rax),
        (reg::RCX, &mut m.rcx),
        (reg::RSP, &mut m.rsp),
        (reg::RIP, &mut m.rip),
        (reg::EFL, &mut m.rflags),
    ];
    for (i, slot) in pairs {
        if let Some(v) = changed(i) {
            *slot = v;
        }
    }
}

/// Emulate the default action of `fsig` for a handler that turned out to be
/// SIG_DFL: put the default back and let a fault re-fault; resend anything else.
fn take_default(fsig: c_int, code: c_int) {
    // SAFETY: plain signal-safe system calls.
    unsafe {
        libc::signal(fsig, libc::SIG_DFL);
        let fault = code > 0 && code < 0x10000;
        if !fault {
            libc::pthread_kill(libc::pthread_self(), fsig);
        }
    }
}

/// The host handler for every signal the engine handles. Async-signal-safe:
/// atomics, stack memory and system calls only.
extern "C" fn dispatch(fsig: c_int, si: *mut libc::siginfo_t, ctx: *mut c_void) {
    let Some(lsig) = fb_to_lx_signal(fsig) else {
        return;
    };
    let slot = &ENGINE[lsig as usize];
    let handler = if slot.set.load(Ordering::Acquire) { slot.handler.load(Ordering::Acquire) } else { SIG_DFL };
    let flags = slot.flags.load(Ordering::Relaxed) as u32;
    let code = if si.is_null() {
        0
    } else {
        // SAFETY: the kernel's siginfo_t.
        unsafe { (*si).si_code }
    };
    match handler {
        SIG_IGN => return,
        SIG_DFL => return take_default(fsig, code),
        _ => {}
    }
    if flags & lx::SA_RESETHAND != 0 {
        slot.handler.store(SIG_DFL, Ordering::Release);
        slot.flags.store(0, Ordering::Relaxed);
    }
    if flags & lx::SA_SIGINFO == 0 {
        // SAFETY: the engine installed a plain handler for this signal.
        let f: extern "C" fn(c_int) = unsafe { std::mem::transmute(handler) };
        f(lsig);
        return;
    }
    let mut info = lx_siginfo(lsig, fsig, si);
    if ctx.is_null() {
        // SAFETY: the engine installed an SA_SIGINFO handler.
        let f: extern "C" fn(c_int, *mut LxSiginfo, *mut c_void) = unsafe { std::mem::transmute(handler) };
        f(lsig, &mut info, std::ptr::null_mut());
        return;
    }
    // SAFETY: the kernel passes its ucontext_t for an SA_SIGINFO handler.
    let uc = unsafe { &mut *ctx.cast::<FbUcontext>() };
    let m = &mut uc.uc_mcontext;
    let before = lx_gregs(m);
    let has_fp = m.fpformat == fb::MC_FPFMT_XMM && m.ownedfp != fb::MC_FPOWNED_NONE;
    let mut luc = LxUcontext {
        uc_flags: 0,
        uc_link: 0,
        uc_stack: BionicStack {
            ss_sp: uc.uc_stack.ss_sp,
            ss_flags: (if uc.uc_stack.ss_flags & fb::SS_ONSTACK != 0 { lx::SS_ONSTACK } else { 0 })
                | (if uc.uc_stack.ss_flags & fb::SS_DISABLE != 0 { lx::SS_DISABLE } else { 0 }),
            ss_size: uc.uc_stack.ss_size,
        },
        gregs: before,
        fpregs: std::ptr::null_mut(),
        _reserved1: [0; 8],
        uc_sigmask: fb_set_to_lx(&uc.uc_sigmask),
        fpregs_mem: [0; 512],
    };
    if has_fp {
        luc.fpregs_mem = m.fpstate;
        luc.fpregs = luc.fpregs_mem.as_mut_ptr();
    }
    let mask_before = luc.uc_sigmask;
    // SAFETY: the engine installed an SA_SIGINFO handler for this signal.
    let f: extern "C" fn(c_int, *mut LxSiginfo, *mut LxUcontext) = unsafe { std::mem::transmute(handler) };
    f(lsig, &mut info, &mut luc);
    apply_gregs(m, &before, &luc.gregs);
    if has_fp && luc.fpregs_mem != m.fpstate {
        m.fpstate = luc.fpregs_mem;
    }
    if luc.uc_sigmask != mask_before {
        uc.uc_sigmask = lx_set_to_fb(luc.uc_sigmask, true);
    }
}

/// Put [`dispatch`] on SIGILL if nothing handles it yet. The loader calls
/// this right before `intoxicated_install_raw_syscall_trap`, which records
/// the handler it replaces and chains every SIGILL that is not one of its
/// sites to it; with this, that chain reaches the engine's SIGILL handler
/// whenever the engine installs one, and the default action until then.
pub fn prepare_for_raw_syscall_trap() {
    let Some(cur) = host_action(fb::SIGILL) else {
        return;
    };
    if cur.sa_sigaction != SIG_DFL {
        return;
    }
    // SAFETY: a fully initialised action for SIGILL.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = dispatch as extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) as usize;
        sa.sa_flags = fb::SA_SIGINFO | fb::SA_NODEFER;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(fb::SIGILL, &sa, std::ptr::null_mut());
    }
}

/// The registered signal set.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    vec![
        ("sigemptyset", b_sigemptyset as *mut c_void),
        ("sigfillset", b_sigfillset as *mut c_void),
        ("sigaddset", b_sigaddset as *mut c_void),
        ("sigdelset", b_sigdelset as *mut c_void),
        ("sigismember", b_sigismember as *mut c_void),
        ("sigprocmask", b_sigprocmask as *mut c_void),
        ("pthread_sigmask", b_pthread_sigmask as *mut c_void),
        ("sigaction", b_sigaction as *mut c_void),
        ("signal", b_signal as *mut c_void),
        ("bsd_signal", b_signal as *mut c_void),
        ("raise", b_raise as *mut c_void),
        ("sigaltstack", b_sigaltstack as *mut c_void),
    ]
}

#[cfg(test)]
mod tests;
