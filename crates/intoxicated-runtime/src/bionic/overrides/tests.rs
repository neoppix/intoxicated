use super::*;

/// The errno the engine reads, through the `__errno` the table gives it.
fn engine_errno() -> c_int {
    let addr = crate::bionic::function_overrides().into_iter().rev().find(|(n, _)| *n == "__errno").unwrap().1;
    let f: unsafe extern "C" fn() -> *mut c_int = unsafe { std::mem::transmute(addr) };
    unsafe { *f() }
}

#[test]
fn sysconf_answers_bionic_selectors_with_freebsds_values() {
    unsafe {
        assert_eq!(b_sysconf(39), 4096, "_SC_PAGESIZE");
        assert_eq!(b_sysconf(40), 4096, "_SC_PAGE_SIZE");
        assert_eq!(b_sysconf(96), libc::sysconf(57), "_SC_NPROCESSORS_CONF");
        assert_eq!(b_sysconf(97), libc::sysconf(58), "_SC_NPROCESSORS_ONLN");
        assert_eq!(b_sysconf(98), libc::sysconf(121), "_SC_PHYS_PAGES");
        assert_eq!(b_sysconf(11), libc::sysconf(5), "_SC_OPEN_MAX, asked for in game");
        assert_eq!(b_sysconf(6), 100, "_SC_CLK_TCK, Linux's USER_HZ");
        assert_eq!(b_sysconf(0x94), 0, "_SC_LEVEL1_DCACHE_LINESIZE: unknown");
        assert_eq!(b_sysconf(75), libc::sysconf(86), "_SC_THREAD_KEYS_MAX");
    }
    // _SC_AVPHYS_PAGES used to be FreeBSD's 122, _SC_CPUSET_SIZE: 128 bytes.
    let avail = unsafe { b_sysconf(99) };
    let phys = unsafe { b_sysconf(98) };
    assert!(avail > 0 && avail <= phys, "avail {avail} of {phys} pages");
    assert_ne!(avail, unsafe { libc::sysconf(122) }, "answered _SC_CPUSET_SIZE");
    // An unknown selector: -1 with EINVAL, as the engine reads errno.
    assert_eq!(unsafe { b_sysconf(12345) }, -1);
    assert_eq!(engine_errno(), 22);
}

#[test]
fn properties_answer_the_device_and_nothing_else() {
    let mut buf = [0x55 as c_char; PROP_VALUE_MAX];
    unsafe {
        assert_eq!(b_system_property_get(c"ro.build.version.sdk".as_ptr(), buf.as_mut_ptr()), 2);
        assert_eq!(CStr::from_ptr(buf.as_ptr()).to_str().unwrap(), "33");
        assert_eq!(b_system_property_get(c"ro.nonexistent".as_ptr(), buf.as_mut_ptr()), 0);
        assert_eq!(buf[0], 0);
        assert_eq!(b_system_property_get(c"ro.build.version.sdk".as_ptr(), std::ptr::null_mut()), 0);
    }
    assert_eq!(property("ro.product.model"), "SM-G991B");
    assert_eq!(property("ro.board.platform"), "exynos2100");
    assert!(PROPERTIES.iter().all(|(_, v)| !v.to_ascii_lowercase().contains("intoxicated")));
}

#[test]
fn data_symbols_are_real_objects() {
    let d: std::collections::BTreeMap<&str, *mut c_void> = data().into_iter().collect();
    unsafe {
        // bionic's FILE* variables are the host's own stream pointers.
        for (name, fd) in [("stdin", 0), ("stdout", 1), ("stderr", 2)] {
            let f = *(d[name] as *const *mut libc::FILE);
            assert!(!f.is_null(), "{name}");
            assert_eq!(libc::fileno(f), fd, "{name}");
        }
        assert_eq!(d["stdout"], std::ptr::addr_of_mut!(__stdoutp).cast());
        let (tz, dl) = zone_facts();
        assert_eq!(*(d["timezone"] as *const i64), tz);
        assert!(tz.abs() <= 14 * 3600, "timezone {tz}");
        assert_eq!(*(d["daylight"] as *const c_int), dl);
        assert!(dl == 0 || dl == 1);
        assert_eq!(*(d["__stack_chk_guard"] as *const u64) & 0xff, 0);
        assert_eq!(d["__sF"] as *const c_void, intoxicated_legacy_sf_base());
        assert_eq!(intoxicated_legacy_sf_stride(), 152);
        assert!(std::slice::from_raw_parts(d["__sF"] as *const u8, 456).iter().all(|&b| b == 0));
    }
    let (name, env) = environ();
    assert_eq!(name, "environ");
    let first = unsafe { *(*(env as *const *const *const c_char)) };
    assert!(!first.is_null(), "environ is empty");
}

// ------------------------------------------------------------- long double

// Rust has no stable binary128 type, so these are called the way the engine
// calls them, by hand: arguments in xmm0..xmm2 (or rdi, rsi, rdx), the
// result in xmm0, on a 16-byte aligned stack clear of the red zone.
extern "C" {
    fn bionic_powl();
    fn bionic_fmal();
    fn bionic_strtold_l();
    fn bionic_roundl();
}

/// `f(args...)` for a function of up to three binary128 arguments.
fn call_q(f: unsafe extern "C" fn(), args: &[u128]) -> u128 {
    let mut a = [0u128; 3];
    a[..args.len()].copy_from_slice(args);
    let mut out = 0u128;
    // SAFETY: `f` takes its arguments in xmm0..xmm2 and returns in xmm0; the
    // stack is realigned and restored from r13, which is declared clobbered.
    unsafe {
        std::arch::asm!(
            "mov r13, rsp",
            "and rsp, -16",
            "sub rsp, 128",
            "movdqu xmm0, [{a}]",
            "movdqu xmm1, [{a} + 16]",
            "movdqu xmm2, [{a} + 32]",
            "call {f}",
            "mov rsp, r13",
            "movdqu [r12], xmm0",
            a = in(reg) a.as_ptr(),
            f = in(reg) f as usize,
            in("r12") &mut out as *mut u128,
            out("r13") _,
            clobber_abi("C"),
        );
    }
    out
}

/// `bionic_strtold_l(s, end, loc)`.
fn strtold(s: *const c_char, end: *mut *mut c_char, loc: libc::locale_t) -> u128 {
    let mut out = 0u128;
    // SAFETY: as above, with the three pointer arguments in rdi, rsi, rdx.
    unsafe {
        std::arch::asm!(
            "mov r13, rsp",
            "and rsp, -16",
            "sub rsp, 128",
            "call {f}",
            "mov rsp, r13",
            "movdqu [r12], xmm0",
            f = in(reg) bionic_strtold_l as *const () as usize,
            in("rdi") s,
            in("rsi") end,
            in("rdx") loc,
            in("r12") &mut out as *mut u128,
            out("r13") _,
            clobber_abi("C"),
        );
    }
    out
}

/// The identity: the tests below spell binary128 values as bit patterns.
fn q(bits: u128) -> u128 {
    bits
}

fn bits(v: u128) -> u128 {
    v
}

unsafe fn bionic_powl_q(x: u128, y: u128) -> u128 {
    call_q(bionic_powl, &[x, y])
}

unsafe fn bionic_fmal_q(a: u128, b: u128, c: u128) -> u128 {
    call_q(bionic_fmal, &[a, b, c])
}

unsafe fn bionic_roundl_q(x: u128) -> u128 {
    call_q(bionic_roundl, &[x])
}

/// The binary128 of a positive integer below 2^113 (exact).
fn int(n: u128) -> u128 {
    let e = 127 - n.leading_zeros() as u128;
    let mantissa = (n << (112 - e)) & ((1u128 << 112) - 1);
    ((e + 16383) << 112) | mantissa
}

const ONE: u128 = 0x3fff_0000_0000_0000_0000_0000_0000_0000;
const NEG: u128 = 1 << 127;

#[test]
fn binary128_helpers_agree_with_known_patterns() {
    assert_eq!(int(1), ONE);
    assert_eq!(int(2), 0x4000_0000_0000_0000_0000_0000_0000_0000);
    assert_eq!(int(7), 0x4001_c000_0000_0000_0000_0000_0000_0000);
    assert_eq!(bits(unsafe { bionic_roundl_q(q(0x4000_4000_0000_0000_0000_0000_0000_0000)) }), int(3), "roundl(2.5)");
}

#[test]
fn powl_takes_and_returns_binary128() {
    unsafe {
        assert_eq!(bits(bionic_powl_q(q(int(2)), q(int(10)))), int(1024));
        assert_eq!(bits(bionic_powl_q(q(int(10)), q(int(30)))), int(10u128.pow(30)), "10^30 exactly");
        assert_eq!(bits(bionic_powl_q(q(int(2)), q(NEG | int(3)))), 0x3ffc_0000_0000_0000_0000_0000_0000_0000, "2^-3");
        assert_eq!(bits(bionic_powl_q(q(int(5)), q(0))), ONE);
        // A fractional power goes through the host at x87 precision: sqrt(2)
        // to 63 fraction bits of binary128's 112.
        let sqrt2: u128 = 0x3fff_6a09_e667_f3bc_c908_b2fb_1366_ea95;
        let half = 0x3ffe_0000_0000_0000_0000_0000_0000_0000;
        let got = bits(bionic_powl_q(q(int(2)), q(half)));
        assert!((got >> 50).abs_diff(sqrt2 >> 50) <= 1, "powl(2, 0.5) = {got:#x}");
    }
}

#[test]
fn fmal_rounds_once() {
    unsafe {
        assert_eq!(bits(bionic_fmal_q(q(int(2)), q(int(3)), q(ONE))), int(7));
        // (1 + 2^-112)^2 - (1 + 2^-111) = 2^-224 exactly. A product rounded
        // to binary128 first loses it and answers 0.
        let a = ONE | 1;
        let c = NEG | ONE | 2;
        let r = bits(bionic_fmal_q(q(a), q(a), q(c)));
        assert_eq!(r, (16383u128 - 224) << 112, "{r:#x}");
    }
}

#[test]
fn strtold_l_parses_to_binary128() {
    let c_locale = unsafe { libc::newlocale(libc::LC_ALL_MASK, c"C".as_ptr(), std::ptr::null_mut()) };
    assert!(!c_locale.is_null());
    let parse = |s: &CStr| {
        let mut end: *mut c_char = std::ptr::null_mut();
        let v = bits(strtold(s.as_ptr(), &mut end, c_locale));
        (v, end as usize - s.as_ptr() as usize)
    };
    // 0.1 rounded to 113 bits, not x87's 64.
    assert_eq!(parse(c"0.1"), (0x3ffb_9999_9999_9999_9999_9999_9999_999a, 3));
    assert_eq!(parse(c"  1e30xyz"), (int(10u128.pow(30)), 6));
    assert_eq!(parse(c"-1.5"), (NEG | 0x3fff_8000_0000_0000_0000_0000_0000_0000, 4));
    assert_eq!(parse(c"100.250"), (0x4005_9100_0000_0000_0000_0000_0000_0000, 7));
    assert_eq!(parse(c"-0"), (NEG, 2));
    // Outside the exact path: the host's x87 parse, widened.
    let (big, n) = parse(c"1e400");
    assert_eq!((big >> 112, n), (16383 + 1328, 5));
    assert_eq!(parse(c"inf").0, 0x7fff_0000_0000_0000_0000_0000_0000_0000);
    let (nan, _) = parse(c"nan");
    assert_eq!(nan >> 112, 0x7fff);
    assert_ne!(nan & ((1u128 << 112) - 1), 0);
    assert_eq!(parse(c"x").1, 0, "nothing parsed, end at the start");
    unsafe { libc::freelocale(c_locale) };
}

// ------------------------------------------------------------ translations

#[test]
fn flag_and_mask_translations() {
    assert_eq!(msync_flags(4), Some(0), "MS_SYNC is 0 on FreeBSD");
    assert_eq!(msync_flags(1 | 2), Some(libc::MS_ASYNC | libc::MS_INVALIDATE));
    assert_eq!(msync_flags(1 | 4), None);
    assert_eq!(msync_flags(8), None);
    assert_eq!(madvise_advice(8), Ok(Some(5)), "MADV_FREE");
    assert_eq!(madvise_advice(4), Ok(Some(4)));
    assert_eq!(madvise_advice(16), Ok(Some(libc::MADV_NOCORE)));
    assert_eq!(madvise_advice(14), Ok(None));
    assert_eq!(madvise_advice(99), Err(()));
    assert_eq!(locale_mask(0x1fbf), 0x3f, "LC_ALL_MASK");
    assert_eq!(locale_mask(1), libc::LC_CTYPE_MASK);
    assert_eq!(locale_mask(1 << 3), libc::LC_COLLATE_MASK);
    let l = unsafe { b_newlocale(0x1fbf, c"C".as_ptr(), std::ptr::null_mut()) };
    assert!(!l.is_null());
    unsafe { libc::freelocale(l) };
}

#[test]
fn clock_counts_microseconds() {
    let t0 = unsafe { b_clock() };
    let start = std::time::Instant::now();
    let mut x = 0u64;
    while start.elapsed() < std::time::Duration::from_millis(60) {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    std::hint::black_box(x);
    let used = unsafe { b_clock() } - t0;
    assert!((30_000..5_000_000).contains(&used), "{used} us for ~60 ms of CPU");
}

#[test]
fn thread_names_ids_and_fork_handlers() {
    unsafe {
        let me = libc::pthread_self();
        assert_eq!(b_pthread_setname_np(me, c"intox-test".as_ptr()), 0);
        assert_eq!(b_pthread_setname_np(me, c"a-name-longer-than-15".as_ptr()), ERANGE);
        assert_eq!(b_gettid(), pthread_getthreadid_np());
        assert_eq!(b_register_atfork(None, None, None, std::ptr::null_mut()), 0);
        assert_eq!(b_mremap(std::ptr::null_mut(), 0, 4096, 0), libc::MAP_FAILED);
    }
}

#[test]
fn cmsg_nxthdr_walks_linux_control_messages() {
    // Two messages: 20 bytes (aligned to 24) and 16 bytes, in a 40-byte buffer.
    let mut control = [0u64; 5];
    let base = control.as_mut_ptr().cast::<u8>();
    unsafe {
        *(base as *mut usize) = 20;
        *(base.add(24) as *mut usize) = 16;
    }
    let mut msg = [0u8; 56];
    msg[32..40].copy_from_slice(&(base as usize).to_ne_bytes());
    msg[40..48].copy_from_slice(&40usize.to_ne_bytes());
    unsafe {
        assert_eq!(b_cmsg_nxthdr(msg.as_ptr(), base), base.add(24) as *const u8);
        assert!(b_cmsg_nxthdr(msg.as_ptr(), base.add(24)).is_null());
    }
}
