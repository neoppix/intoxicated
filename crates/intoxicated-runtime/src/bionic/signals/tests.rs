use super::*;
use std::mem::{offset_of, size_of};
use std::sync::atomic::{AtomicI64, AtomicU32};
use std::sync::Mutex;

/// Signal dispositions are process-wide; the tests that change them take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn bionic_and_freebsd_layouts_match_their_headers() {
    // bionic, measured by compiling against its headers for x86_64-linux-android33.
    assert_eq!(size_of::<BionicSigaction>(), 32);
    assert_eq!(offset_of!(BionicSigaction, sa_flags), 0);
    assert_eq!(offset_of!(BionicSigaction, sa_handler), 8);
    assert_eq!(offset_of!(BionicSigaction, sa_mask), 16);
    assert_eq!(offset_of!(BionicSigaction, sa_restorer), 24);
    assert_eq!(size_of::<BionicStack>(), 24);
    assert_eq!(offset_of!(BionicStack, ss_flags), 8);
    assert_eq!(offset_of!(BionicStack, ss_size), 16);
    assert_eq!(size_of::<LxSiginfo>(), 128);
    assert_eq!(size_of::<LxUcontext>(), 816);
    assert_eq!(offset_of!(LxUcontext, uc_stack), 16);
    assert_eq!(offset_of!(LxUcontext, gregs), 40);
    assert_eq!(offset_of!(LxUcontext, fpregs), 224);
    assert_eq!(offset_of!(LxUcontext, uc_sigmask), 296);
    assert_eq!(offset_of!(LxUcontext, fpregs_mem), 304);
    // FreeBSD 14.4 amd64, measured with a native probe.
    assert_eq!(size_of::<libc::sigaction>(), 32);
    assert_eq!(size_of::<FbMcontext>(), 800);
    assert_eq!(offset_of!(FbMcontext, trapno), 128);
    assert_eq!(offset_of!(FbMcontext, addr), 136);
    assert_eq!(offset_of!(FbMcontext, err), 152);
    assert_eq!(offset_of!(FbMcontext, rip), 160);
    assert_eq!(offset_of!(FbMcontext, rsp), 184);
    assert_eq!(offset_of!(FbMcontext, fpformat), 208);
    assert_eq!(offset_of!(FbMcontext, fpstate), 224);
    assert_eq!(offset_of!(FbMcontext, fsbase), 736);
    assert_eq!(size_of::<FbUcontext>(), 880);
    assert_eq!(offset_of!(FbUcontext, uc_mcontext), 16);
    assert_eq!(offset_of!(FbUcontext, uc_link), 816);
    assert_eq!(offset_of!(FbUcontext, uc_stack), 824);
    assert_eq!(offset_of!(FbUcontext, uc_flags), 848);
    assert_eq!(offset_of!(FbStack, ss_size), 8);
    assert_eq!(offset_of!(FbStack, ss_flags), 16);
}

#[test]
fn signal_numbers_translate_both_ways() {
    let pairs = [(10, 30), (12, 31), (7, 10), (17, 20), (18, 19), (19, 17), (20, 18), (23, 16), (29, 23), (31, 12)];
    for (l, f) in pairs {
        assert_eq!(lx_to_fb_signal(l), Some(f), "linux {l}");
        assert_eq!(fb_to_lx_signal(f), Some(l), "freebsd {f}");
    }
    for same in [1, 2, 3, 4, 5, 6, 8, 9, 11, 13, 14, 15, 21, 22, 24, 25, 26, 27, 28] {
        assert_eq!(lx_to_fb_signal(same), Some(same));
    }
    assert_eq!(lx_to_fb_signal(16), None, "SIGSTKFLT");
    assert_eq!(lx_to_fb_signal(30), None, "SIGPWR");
    assert_eq!(lx_to_fb_signal(32), Some(65));
    assert_eq!(lx_to_fb_signal(64), Some(97));
    assert_eq!(lx_to_fb_signal(0), None);
    assert_eq!(lx_to_fb_signal(65), None);
    for f in [7, 29, 32, 33] {
        assert_eq!(fb_to_lx_signal(f), None, "freebsd {f} has no Linux number");
    }
    for l in 1..=64 {
        if let Some(f) = lx_to_fb_signal(l) {
            assert_eq!(fb_to_lx_signal(f), Some(l));
        }
    }
}

#[test]
fn flags_and_how_translate() {
    assert_eq!(lx_flags_to_fb(lx::SA_SIGINFO), 0x40);
    assert_eq!(lx_flags_to_fb(lx::SA_SIGINFO | lx::SA_ONSTACK | lx::SA_RESTART), 0x43);
    assert_eq!(lx_flags_to_fb(lx::SA_NODEFER | lx::SA_RESETHAND), 0x14);
    assert_eq!(lx_flags_to_fb(lx::SA_NOCLDSTOP | lx::SA_NOCLDWAIT), 0x28);
    assert_eq!(lx_flags_to_fb(lx::SA_RESTORER), 0, "SA_RESTORER never reaches the kernel");
    assert_eq!(fb_flags_to_lx(0x43), lx::SA_SIGINFO | lx::SA_ONSTACK | lx::SA_RESTART);
    assert_eq!(lx_how_to_fb(0), Some(1));
    assert_eq!(lx_how_to_fb(1), Some(2));
    assert_eq!(lx_how_to_fb(2), Some(3));
    assert_eq!(lx_how_to_fb(3), None);
}

#[test]
fn set_operations_stay_in_eight_bytes() {
    let mut words = [0u64, 0xfeed_face_cafe_beef];
    unsafe {
        assert_eq!(b_sigfillset(words.as_mut_ptr()), 0);
        assert_eq!(words[0], u64::MAX);
        assert_eq!(b_sigemptyset(words.as_mut_ptr()), 0);
        assert_eq!(b_sigaddset(words.as_mut_ptr(), 9), 0);
        assert_eq!(words[0], 0x100);
        assert_eq!(b_sigismember(words.as_ptr(), 9), 1);
        assert_eq!(b_sigismember(words.as_ptr(), 10), 0);
        assert_eq!(b_sigaddset(words.as_mut_ptr(), 64), 0);
        assert_eq!(b_sigdelset(words.as_mut_ptr(), 9), 0);
        assert_eq!(words[0], 1 << 63);
        assert_eq!(b_sigaddset(words.as_mut_ptr(), 65), -1);
        assert_eq!(b_sigaddset(words.as_mut_ptr(), 0), -1);
        assert_eq!(b_sigismember(std::ptr::null(), 1), -1);
    }
    assert_eq!(words[1], 0xfeed_face_cafe_beef, "wrote past the bionic set");
}

#[test]
fn masks_carry_the_right_signals() {
    // {SIGINT 2, SIGUSR1 10, SIGTERM 15} in bionic's bits.
    let set = (1 << 1) | (1 << 9) | (1 << 14);
    let f = lx_set_to_fb(set, false);
    let has = |s: &libc::sigset_t, n| unsafe { libc::sigismember(s, n) } == 1;
    assert!(has(&f, 2) && has(&f, 30) && has(&f, 15));
    assert!(!has(&f, 10), "Linux SIGUSR1 must not become FreeBSD SIGBUS");
    assert_eq!(fb_set_to_lx(&f), set);
    // Applied as a mask, the runtime's own signals are left out.
    let b = lx_set_to_fb(set | (1 << 3), true);
    assert!(has(&b, 30));
    assert!(!has(&b, 2) && !has(&b, 15) && !has(&b, 4));
}

static USR1_SIG: AtomicI64 = AtomicI64::new(0);
static USR1_SIGNO: AtomicI64 = AtomicI64::new(0);
static USR1_MASK: AtomicU64 = AtomicU64::new(0);
static USR1_CODE: AtomicI64 = AtomicI64::new(0);

extern "C" fn usr1_handler(sig: c_int, info: *mut LxSiginfo, _uc: *mut LxUcontext) {
    USR1_SIG.store(sig.into(), Ordering::SeqCst);
    unsafe {
        USR1_SIGNO.store((*info).si_signo.into(), Ordering::SeqCst);
        USR1_CODE.store((*info).si_code.into(), Ordering::SeqCst);
        let mut cur: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(fb::SIG_BLOCK, std::ptr::null(), &mut cur);
        USR1_MASK.store(fb_set_to_lx(&cur), Ordering::SeqCst);
    }
}

#[test]
fn sigaction_installs_a_translated_action_and_delivers_linux_numbers() {
    let _g = serial();
    let act = BionicSigaction {
        sa_flags: (lx::SA_SIGINFO | lx::SA_RESTART | lx::SA_ONSTACK | lx::SA_RESTORER) as c_int,
        sa_handler: usr1_handler as extern "C" fn(c_int, *mut LxSiginfo, *mut LxUcontext) as usize,
        sa_mask: (1 << 6) | (1 << 16), // SIGBUS 7, SIGCHLD 17
        sa_restorer: 0x1234,
    };
    let mut old = BionicSigaction { sa_flags: -1, sa_handler: 7, sa_mask: 7, sa_restorer: 7 };
    assert_eq!(unsafe { b_sigaction(10, &act, &mut old) }, 0);
    assert_eq!(old.sa_handler, SIG_DFL);

    // What FreeBSD got: SIGUSR1 is 30 there.
    let host = host_action(30).unwrap();
    assert!(is_dispatch(host.sa_sigaction));
    assert_eq!(host.sa_flags & 0x43, 0x43, "flags {:#x}", host.sa_flags);
    let has = |n| unsafe { libc::sigismember(&host.sa_mask, n) } == 1;
    assert!(has(10) && has(20), "mask lost FreeBSD SIGBUS or SIGCHLD");
    assert!(!has(7), "Linux 7 read as FreeBSD SIGEMT");

    // Delivered with the Linux number, in Linux layout.
    assert_eq!(unsafe { b_raise(10) }, 0);
    assert_eq!(USR1_SIG.load(Ordering::SeqCst), 10);
    assert_eq!(USR1_SIGNO.load(Ordering::SeqCst), 10);
    assert_eq!(USR1_CODE.load(Ordering::SeqCst), -6, "raise is SI_TKILL on Linux");
    // While it ran, its own signal and its mask were blocked: 10, 7 and 17.
    let m = USR1_MASK.load(Ordering::SeqCst);
    let want = (1 << 9) | (1 << 6) | (1 << 16);
    assert_eq!(m & want, want, "mask during the handler {m:#x}");

    // Read back exactly as set, restorer included.
    let mut now = BionicSigaction::default();
    assert_eq!(unsafe { b_sigaction(10, std::ptr::null(), &mut now) }, 0);
    assert_eq!(now, act);

    let dfl = BionicSigaction::default();
    assert_eq!(unsafe { b_sigaction(10, &dfl, std::ptr::null_mut()) }, 0);
    assert_eq!(host_action(30).unwrap().sa_sigaction, SIG_DFL);
    // SIGKILL cannot be changed; out-of-range numbers are refused.
    assert_eq!(unsafe { b_sigaction(9, &dfl, std::ptr::null_mut()) }, -1);
    assert_eq!(unsafe { b_sigaction(65, std::ptr::null(), &mut now) }, -1);
}

#[test]
fn ignoring_sigpipe_and_restoring_it_round_trips() {
    let _g = serial();
    let ign = BionicSigaction { sa_handler: SIG_IGN, ..Default::default() };
    let mut saved = BionicSigaction::default();
    assert_eq!(unsafe { b_sigaction(13, &ign, &mut saved) }, 0);
    assert_eq!(host_action(13).unwrap().sa_sigaction, SIG_IGN);
    assert_eq!(unsafe { b_sigaction(13, &saved, std::ptr::null_mut()) }, 0);
    assert_eq!(host_action(13).unwrap().sa_sigaction, saved.sa_handler);
}

#[test]
fn masks_use_linux_how_values_and_never_block_sigill() {
    let _g = serial();
    // Linux SIG_BLOCK (0) of SIGUSR2 (12) and SIGILL (4).
    let set: u64 = (1 << 11) | (1 << 3);
    let mut old = 0u64;
    assert_eq!(unsafe { b_pthread_sigmask(0, &set, &mut old) }, 0);
    let mut host: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe { libc::pthread_sigmask(fb::SIG_BLOCK, std::ptr::null(), &mut host) };
    assert_eq!(unsafe { libc::sigismember(&host, 31) }, 1, "FreeBSD SIGUSR2 not blocked");
    assert_eq!(unsafe { libc::sigismember(&host, 4) }, 0, "SIGILL blocked");
    let mut now = 0u64;
    assert_eq!(unsafe { b_sigprocmask(1, std::ptr::null(), &mut now) }, 0);
    assert_ne!(now & (1 << 11), 0);
    // Linux SIG_SETMASK (2) puts the old mask back; SIG_UNBLOCK is 1.
    assert_eq!(unsafe { b_pthread_sigmask(2, &old, std::ptr::null_mut()) }, 0);
    assert_eq!(unsafe { b_pthread_sigmask(7, &set, std::ptr::null_mut()) }, EINVAL);
    assert_eq!(unsafe { b_sigprocmask(7, &set, std::ptr::null_mut()) }, -1);
}

#[test]
fn sigaltstack_swaps_the_fields_and_flags() {
    let _g = serial();
    let mut mem = vec![0u8; 64 * 1024];
    let ss = BionicStack { ss_sp: mem.as_mut_ptr().cast(), ss_flags: 0, ss_size: mem.len() };
    let mut old = BionicStack { ss_sp: std::ptr::null_mut(), ss_flags: 0, ss_size: 0 };
    assert_eq!(unsafe { b_sigaltstack(&ss, &mut old) }, 0);
    let mut host = FbStack { ss_sp: std::ptr::null_mut(), ss_size: 0, ss_flags: 0 };
    unsafe { libc::sigaltstack(std::ptr::null(), (&mut host as *mut FbStack).cast()) };
    assert_eq!(host.ss_sp, ss.ss_sp);
    assert_eq!(host.ss_size, mem.len());
    let mut back = BionicStack { ss_sp: std::ptr::null_mut(), ss_flags: -1, ss_size: 0 };
    assert_eq!(unsafe { b_sigaltstack(std::ptr::null(), &mut back) }, 0);
    assert_eq!((back.ss_sp, back.ss_size, back.ss_flags), (ss.ss_sp, mem.len(), 0));
    // Put back what the thread had (Rust's own alternate stack, or none).
    let restore = if old.ss_flags & lx::SS_DISABLE != 0 {
        BionicStack { ss_sp: std::ptr::null_mut(), ss_flags: lx::SS_DISABLE, ss_size: 0 }
    } else {
        old
    };
    assert_eq!(unsafe { b_sigaltstack(&restore, std::ptr::null_mut()) }, 0);
}

// ------------------------------------------------------- the raw-syscall trap

extern "C" {
    fn intoxicated_install_raw_syscall_trap(base: usize, vaddrs: *const std::ffi::c_ulong, count: usize) -> c_int;
}

/// Code for the trap test. At 0: `mov eax, 39` (Linux getpid); `syscall`,
/// the site the trap rewrites to `ud2`; `ret`. At 16: `0x06`, an opcode that
/// is undefined in 64-bit mode, so a SIGILL that is not a trap site; then
/// `mov eax, 42; ret`.
const CODE: [u8; 23] = [
    0xb8, 0x27, 0, 0, 0, 0x0f, 0x05, 0xc3, 0, 0, 0, 0, 0, 0, 0, 0, //
    0x06, 0xb8, 0x2a, 0, 0, 0, 0xc3,
];
const SITE: std::ffi::c_ulong = 5;
const NOT_A_SITE: usize = 16;

struct Trap {
    page: usize,
    trap_handler: usize,
}

fn trap() -> &'static Trap {
    static TRAP: std::sync::OnceLock<Trap> = std::sync::OnceLock::new();
    TRAP.get_or_init(|| unsafe {
        let page = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        );
        assert_ne!(page, libc::MAP_FAILED);
        std::ptr::copy_nonoverlapping(CODE.as_ptr(), page.cast::<u8>(), CODE.len());
        // In the loader's order: this module first, then the trap.
        prepare_for_raw_syscall_trap();
        assert!(is_dispatch(host_action(fb::SIGILL).unwrap().sa_sigaction));
        let sites = [SITE];
        assert_eq!(intoxicated_install_raw_syscall_trap(page as usize, sites.as_ptr(), 1), 1);
        assert_eq!(*page.cast::<u8>().add(6), 0x0b, "the site was not rewritten to ud2");
        let trap_handler = host_action(fb::SIGILL).unwrap().sa_sigaction;
        assert!(!is_dispatch(trap_handler) && trap_handler > 1);
        Trap { page: page as usize, trap_handler }
    })
}

static ENGINE_SIGILL_CALLS: AtomicU32 = AtomicU32::new(0);
static ENGINE_SIGILL_SIG: AtomicI64 = AtomicI64::new(0);
static ENGINE_SIGILL_RIP: AtomicI64 = AtomicI64::new(0);

/// What an engine crash handler would do for a SIGILL it can step over:
/// read the Linux context and advance RIP past the one-byte instruction.
extern "C" fn engine_sigill(sig: c_int, info: *mut LxSiginfo, uc: *mut LxUcontext) {
    ENGINE_SIGILL_CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe {
        ENGINE_SIGILL_SIG.store(i64::from(sig) * 100 + i64::from((*info).si_signo), Ordering::SeqCst);
        ENGINE_SIGILL_RIP.store((*uc).gregs[reg::RIP], Ordering::SeqCst);
        (*uc).gregs[reg::RIP] += 1;
    }
}

#[test]
fn the_raw_syscall_trap_keeps_sigill_after_the_engine_installs_its_own() {
    let _g = serial();
    let t = trap();
    // The engine installs a SIGILL handler the way a crash reporter does:
    // SA_SIGINFO, on the alternate stack, every signal masked.
    let act = BionicSigaction {
        sa_flags: (lx::SA_SIGINFO | lx::SA_ONSTACK | lx::SA_RESTORER) as c_int,
        sa_handler: engine_sigill as extern "C" fn(c_int, *mut LxSiginfo, *mut LxUcontext) as usize,
        sa_mask: u64::MAX,
        sa_restorer: 0,
    };
    assert_eq!(unsafe { b_sigaction(lx::SIGILL, &act, std::ptr::null_mut()) }, 0);
    // The engine reads back what it set ...
    let mut back = BionicSigaction::default();
    assert_eq!(unsafe { b_sigaction(lx::SIGILL, std::ptr::null(), &mut back) }, 0);
    assert_eq!(back, act);
    // ... and the kernel still runs the trap's handler.
    assert_eq!(host_action(fb::SIGILL).unwrap().sa_sigaction, t.trap_handler, "the trap's SIGILL handler was replaced");

    // A neutralised site still answers through the trap: Linux getpid.
    let raw_getpid: extern "C" fn() -> i64 = unsafe { std::mem::transmute(t.page) };
    let calls_before = ENGINE_SIGILL_CALLS.load(Ordering::SeqCst);
    assert_eq!(raw_getpid(), i64::from(std::process::id()));
    assert_eq!(ENGINE_SIGILL_CALLS.load(Ordering::SeqCst), calls_before, "the engine's handler ran for a trap site");

    // Even with every signal blocked through the bionic layer.
    let all = u64::MAX;
    let mut saved = 0u64;
    assert_eq!(unsafe { b_pthread_sigmask(lx::SIG_BLOCK, &all, &mut saved) }, 0);
    assert_eq!(raw_getpid(), i64::from(std::process::id()));
    assert_eq!(unsafe { b_pthread_sigmask(lx::SIG_SETMASK, &saved, std::ptr::null_mut()) }, 0);

    // Any other SIGILL is chained by the trap to the engine's handler, which
    // gets Linux's number and context and can step over the instruction.
    let other: extern "C" fn() -> i64 = unsafe { std::mem::transmute(t.page + NOT_A_SITE) };
    assert_eq!(other(), 42);
    assert_eq!(ENGINE_SIGILL_CALLS.load(Ordering::SeqCst), calls_before + 1);
    assert_eq!(ENGINE_SIGILL_SIG.load(Ordering::SeqCst), 404, "signal number or si_signo not Linux's");
    assert_eq!(ENGINE_SIGILL_RIP.load(Ordering::SeqCst) as usize, t.page + NOT_A_SITE);
    assert_eq!(host_action(fb::SIGILL).unwrap().sa_sigaction, t.trap_handler);
}
