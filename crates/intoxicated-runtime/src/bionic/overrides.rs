//! The libc functions and data the engine imports that FreeBSD's libc either
//! lacks or answers with another ABI: FORTIFY names, bionic-only entry
//! points, selector and flag numbering, `long double`, and the data symbols
//! bionic exports and FreeBSD does not.

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr};
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

macro_rules! f {
    ($name:literal, $sym:expr) => {
        ($name, $sym as *mut c_void)
    };
}

const EINVAL: c_int = 22;
const ENOMEM: c_int = 12;
const ERANGE: c_int = 34;

fn set_errno(e: c_int) {
    // SAFETY: this thread's errno slot.
    unsafe { *libc::__error() = e };
}

fn text(p: *const c_char) -> std::borrow::Cow<'static, str> {
    if p.is_null() {
        "(null)".into()
    } else {
        // SAFETY: the engine passes NUL-terminated strings.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned().into()
    }
}

// ------------------------------------------------------------ diagnostics

unsafe extern "C" fn b_assert(file: *const c_char, line: c_int, msg: *const c_char) -> ! {
    eprintln!("\n*** Roblox assertion failed ***\n    {}:{line}: {}", text(file), text(msg));
    crate::stub_table::report();
    // SAFETY: ends the process.
    unsafe { libc::abort() }
}

unsafe extern "C" fn b_assert2(file: *const c_char, line: c_int, func: *const c_char, expr: *const c_char) -> ! {
    eprintln!(
        "\n*** Roblox assertion failed ***\n    {}:{line} in {}\n    assertion: {}",
        text(file),
        text(func),
        text(expr)
    );
    crate::stub_table::report();
    // SAFETY: ends the process.
    unsafe { libc::abort() }
}

// ---------------------------------------------------------------- FORTIFY
// FreeBSD's libc has none of these; each forwards with the bound ignored.

unsafe extern "C" fn b_strlen_chk(s: *const c_char, _bound: usize) -> usize {
    // SAFETY: as the caller's strlen.
    unsafe { libc::strlen(s) }
}

unsafe extern "C" fn b_strchr_chk(s: *const c_char, c: c_int, _bound: usize) -> *mut c_char {
    // SAFETY: as the caller's strchr.
    unsafe { libc::strchr(s, c) }
}

unsafe extern "C" fn b_strncpy_chk2(dst: *mut c_char, src: *const c_char, n: usize, _d: usize, _s: usize) -> *mut c_char {
    // SAFETY: as the caller's strncpy.
    unsafe { libc::strncpy(dst, src, n) }
}

unsafe extern "C" fn b_strlcpy_chk(dst: *mut c_char, src: *const c_char, n: usize, _d: usize) -> usize {
    extern "C" {
        fn strlcpy(dst: *mut c_char, src: *const c_char, n: usize) -> usize;
    }
    // SAFETY: as the caller's strlcpy.
    unsafe { strlcpy(dst, src, n) }
}

unsafe extern "C" fn b_umask_chk(mode: libc::mode_t) -> libc::mode_t {
    // SAFETY: plain umask.
    unsafe { libc::umask(mode) }
}

unsafe extern "C" fn b_readlink_chk(path: *const c_char, buf: *mut c_char, size: usize, _bound: usize) -> isize {
    // SAFETY: as the caller's readlink.
    unsafe { libc::readlink(path, buf, size) }
}

unsafe extern "C" fn b_fread_chk(p: *mut c_void, size: usize, n: usize, f: *mut libc::FILE, _bound: usize) -> usize {
    // SAFETY: as the caller's fread.
    unsafe { libc::fread(p, size, n, f) }
}

// ------------------------------------------------------------- properties

/// Galaxy S21 (SM-G991B), kept identical to the Java `android.os.Build`
/// fields in `android_classes.cpp` and `DeviceStaticParams`. The emulator
/// check reads the device identity across all three surfaces; a mismatch
/// between them (e.g. Build.MANUFACTURER=samsung while
/// ro.product.manufacturer said otherwise) is itself a tell, so every surface
/// must agree. `ro.board.platform` is the S21's own, like `ro.hardware`.
const PROPERTIES: &[(&str, &str)] = &[
    ("ro.build.version.sdk", "33"),
    ("ro.build.version.release", "13"),
    ("ro.product.model", "SM-G991B"),
    ("ro.product.manufacturer", "samsung"),
    ("ro.product.brand", "samsung"),
    ("ro.product.device", "o1s"),
    ("ro.product.name", "o1sxxx"),
    ("ro.hardware", "exynos2100"),
    ("ro.product.board", "exynos2100"),
    ("ro.soc.manufacturer", "Samsung"),
    ("ro.soc.model", "exynos2100"),
    ("ro.build.fingerprint", "samsung/o1sxxx/o1s:13/TP1A.220624.014/G991BXXU5DVK1:user/release-keys"),
    ("ro.build.tags", "release-keys"),
    ("ro.build.type", "user"),
    ("ro.board.platform", "exynos2100"),
    ("ro.debuggable", "0"),
    ("ro.secure", "1"),
];

/// bionic's PROP_VALUE_MAX.
const PROP_VALUE_MAX: usize = 92;

/// The answer for a property name, empty when the table has none.
pub fn property(name: &str) -> &'static str {
    PROPERTIES.iter().find(|(k, _)| *k == name).map_or("", |(_, v)| *v)
}

unsafe extern "C" fn b_system_property_get(name: *const c_char, value: *mut c_char) -> c_int {
    let key = text(name);
    let answer = property(&key);
    if std::env::var_os("INTOXICATED_TRACE_PROPS").is_some() {
        if answer.is_empty() {
            eprintln!("[props] {key} = <empty, not in table>");
        } else {
            eprintln!("[props] {key} = {answer}");
        }
    }
    if value.is_null() {
        return 0;
    }
    let bytes = &answer.as_bytes()[..answer.len().min(PROP_VALUE_MAX - 1)];
    // SAFETY: bionic callers pass a PROP_VALUE_MAX buffer.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), value.cast::<u8>(), bytes.len());
        *value.add(bytes.len()) = 0;
    }
    bytes.len() as c_int
}

// ------------------------------------------------------------------ sysconf

/// bionic `_SC_*` to FreeBSD `_SC_*`, by name (bionic's `bits/sysconf.h`,
/// FreeBSD's `<unistd.h>`). The XBS5 and V7 programming environments are
/// FreeBSD's V6 ones, which describe the same models.
const SYSCONF_MAP: &[(c_int, c_int)] = &[
    (0, 1),     // ARG_MAX
    (1, 9),     // BC_BASE_MAX
    (2, 10),    // BC_DIM_MAX
    (3, 11),    // BC_SCALE_MAX
    (4, 12),    // BC_STRING_MAX
    (5, 2),     // CHILD_MAX
    (7, 13),    // COLL_WEIGHTS_MAX
    (8, 14),    // EXPR_NEST_MAX
    (9, 15),    // LINE_MAX
    (10, 4),    // NGROUPS_MAX
    (11, 5),    // OPEN_MAX
    (13, 18),   // 2_C_BIND
    (14, 19),   // 2_C_DEV
    (16, 20),   // 2_CHAR_TERM
    (17, 21),   // 2_FORT_DEV
    (18, 22),   // 2_FORT_RUN
    (19, 23),   // 2_LOCALEDEF
    (20, 24),   // 2_SW_DEV
    (21, 25),   // 2_UPE
    (22, 17),   // 2_VERSION
    (23, 6),    // JOB_CONTROL
    (24, 7),    // SAVED_IDS
    (25, 8),    // VERSION
    (26, 16),   // RE_DUP_MAX
    (27, 26),   // STREAM_MAX
    (28, 27),   // TZNAME_MAX
    (29, 108),  // XOPEN_CRYPT
    (30, 109),  // XOPEN_ENH_I18N
    (31, 113),  // XOPEN_SHM
    (32, 116),  // XOPEN_VERSION
    (33, 117),  // XOPEN_XCU_VERSION
    (34, 111),  // XOPEN_REALTIME
    (35, 112),  // XOPEN_REALTIME_THREADS
    (36, 110),  // XOPEN_LEGACY
    (37, 107),  // ATEXIT_MAX
    (38, 56),   // IOV_MAX
    (39, 47),   // PAGESIZE
    (40, 47),   // PAGE_SIZE
    (41, 115),  // XOPEN_UNIX
    (42, 103),  // XBS5_ILP32_OFF32
    (43, 104),  // XBS5_ILP32_OFFBIG
    (44, 105),  // XBS5_LP64_OFF64
    (45, 106),  // XBS5_LPBIG_OFFBIG
    (46, 42),   // AIO_LISTIO_MAX
    (47, 43),   // AIO_MAX
    (48, 44),   // AIO_PRIO_DELTA_MAX
    (49, 45),   // DELAYTIMER_MAX
    (50, 46),   // MQ_OPEN_MAX
    (51, 75),   // MQ_PRIO_MAX
    (52, 48),   // RTSIG_MAX
    (53, 49),   // SEM_NSEMS_MAX
    (54, 50),   // SEM_VALUE_MAX
    (55, 51),   // SIGQUEUE_MAX
    (56, 52),   // TIMER_MAX
    (57, 28),   // ASYNCHRONOUS_IO
    (58, 38),   // FSYNC
    (59, 29),   // MAPPED_FILES
    (60, 30),   // MEMLOCK
    (61, 31),   // MEMLOCK_RANGE
    (62, 32),   // MEMORY_PROTECTION
    (63, 33),   // MESSAGE_PASSING
    (64, 34),   // PRIORITIZED_IO
    (65, 35),   // PRIORITY_SCHEDULING
    (66, 36),   // REALTIME_SIGNALS
    (67, 37),   // SEMAPHORES
    (68, 39),   // SHARED_MEMORY_OBJECTS
    (69, 40),   // SYNCHRONIZED_IO
    (70, 41),   // TIMERS
    (71, 70),   // GETGR_R_SIZE_MAX
    (72, 71),   // GETPW_R_SIZE_MAX
    (73, 73),   // LOGIN_NAME_MAX
    (74, 85),   // THREAD_DESTRUCTOR_ITERATIONS
    (75, 86),   // THREAD_KEYS_MAX
    (76, 93),   // THREAD_STACK_MIN
    (77, 94),   // THREAD_THREADS_MAX
    (78, 101),  // TTY_NAME_MAX
    (79, 96),   // THREADS
    (80, 82),   // THREAD_ATTR_STACKADDR
    (81, 83),   // THREAD_ATTR_STACKSIZE
    (82, 89),   // THREAD_PRIORITY_SCHEDULING
    (83, 87),   // THREAD_PRIO_INHERIT
    (84, 88),   // THREAD_PRIO_PROTECT
    (85, 91),   // THREAD_SAFE_FUNCTIONS
    (96, 57),   // NPROCESSORS_CONF
    (97, 58),   // NPROCESSORS_ONLN
    (98, 121),  // PHYS_PAGES
    (100, 74),  // MONOTONIC_CLOCK
    (101, 59),  // 2_PBS
    (102, 60),  // 2_PBS_ACCOUNTING
    (103, 61),  // 2_PBS_CHECKPOINT
    (104, 62),  // 2_PBS_LOCATE
    (105, 63),  // 2_PBS_MESSAGE
    (106, 64),  // 2_PBS_TRACK
    (107, 65),  // ADVISORY_INFO
    (108, 66),  // BARRIERS
    (109, 67),  // CLOCK_SELECTION
    (110, 68),  // CPUTIME
    (111, 72),  // HOST_NAME_MAX
    (112, 118), // IPV6
    (113, 119), // RAW_SOCKETS
    (114, 76),  // READER_WRITER_LOCKS
    (115, 77),  // REGEXP
    (116, 78),  // SHELL
    (117, 79),  // SPAWN
    (118, 80),  // SPIN_LOCKS
    (119, 81),  // SPORADIC_SERVER
    (121, 120), // SYMLOOP_MAX
    (122, 84),  // THREAD_CPUTIME
    (123, 90),  // THREAD_PROCESS_SHARED
    (126, 92),  // THREAD_SPORADIC_SERVER
    (127, 95),  // TIMEOUTS
    (128, 97),  // TRACE
    (129, 98),  // TRACE_EVENT_FILTER
    (131, 99),  // TRACE_INHERIT
    (132, 100), // TRACE_LOG
    (136, 102), // TYPED_MEMORY_OBJECTS
    (137, 103), // V7_ILP32_OFF32
    (138, 104), // V7_ILP32_OFFBIG
    (139, 105), // V7_LP64_OFF64
    (140, 106), // V7_LPBIG_OFFBIG
    (141, 114), // XOPEN_STREAMS
];

/// bionic `_SC_CLK_TCK`, `_SC_AVPHYS_PAGES` and the cache selectors.
const SC_CLK_TCK: c_int = 6;
const SC_AVPHYS_PAGES: c_int = 99;
const SC_LEVEL1_ICACHE_SIZE: c_int = 0x8f;
const SC_LEVEL4_CACHE_LINESIZE: c_int = 0x9d;

/// The tick every count the engine can read is in: Linux's USER_HZ, the
/// AT_CLKTCK of the synthetic `/proc/self/auxv` and the unit of the
/// synthetic `/proc/self/stat`. The engine imports no `times()`, the one
/// host call that counts in FreeBSD's 128 Hz.
const USER_HZ: c_long = 100;

fn free_pages() -> c_long {
    let mut v: c_uint = 0;
    let mut len = std::mem::size_of::<c_uint>();
    // SAFETY: an int-sized sysctl into an int.
    let r = unsafe {
        libc::sysctlbyname(c"vm.stats.vm.v_free_count".as_ptr(), (&mut v as *mut c_uint).cast(), &mut len, std::ptr::null(), 0)
    };
    if r == 0 {
        c_long::from(v)
    } else {
        -1
    }
}

/// The value for a bionic selector, or `None` for one FreeBSD has no answer to.
pub fn sysconf_value(name: c_int) -> Option<c_long> {
    match name {
        SC_CLK_TCK => Some(USER_HZ),
        SC_AVPHYS_PAGES => Some(free_pages()).filter(|&v| v >= 0),
        // bionic answers 0 for a cache it cannot describe; so does this.
        SC_LEVEL1_ICACHE_SIZE..=SC_LEVEL4_CACHE_LINESIZE => Some(0),
        _ => {
            let (_, fb) = SYSCONF_MAP.iter().find(|(b, _)| *b == name)?;
            // SAFETY: a valid FreeBSD selector.
            Some(unsafe { libc::sysconf(*fb) })
        }
    }
}

unsafe extern "C" fn b_sysconf(name: c_int) -> c_long {
    if let Some(v) = sysconf_value(name) {
        return v;
    }
    // Said once per selector, so a caller that polls does not flood the log.
    static SAID: std::sync::Mutex<Vec<c_int>> = std::sync::Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if !said.contains(&name) {
        said.push(name);
        eprintln!("[bionic] sysconf({name}) has no FreeBSD answer; returning -1 (EINVAL)");
    }
    set_errno(EINVAL);
    -1
}

// --------------------------------------------------- numbering translations

/// bionic `clock()`: CLOCKS_PER_SEC is 1000000 there and 128 on FreeBSD.
unsafe extern "C" fn b_clock() -> c_long {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid out-parameter.
    if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) } != 0 {
        return -1;
    }
    ts.tv_sec * 1_000_000 + ts.tv_nsec / 1_000
}

/// Linux `madvise` advice to FreeBSD's. `Ok(None)` is advice FreeBSD has no
/// use for, which Linux also treats as a hint (success, nothing done).
fn madvise_advice(lx: c_int) -> Result<Option<c_int>, ()> {
    match lx {
        0..=4 => Ok(Some(lx)),            // NORMAL RANDOM SEQUENTIAL WILLNEED DONTNEED
        8 => Ok(Some(libc::MADV_FREE)),   // FREE (FreeBSD 5)
        16 => Ok(Some(libc::MADV_NOCORE)), // DONTDUMP
        17 => Ok(Some(libc::MADV_CORE)),   // DODUMP
        12 | 13 | 14 | 15 | 20 | 21 => Ok(None), // MERGEABLE UNMERGEABLE HUGEPAGE NOHUGEPAGE COLD PAGEOUT
        _ => Err(()),
    }
}

unsafe extern "C" fn b_madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int {
    // FreeBSD `<sys/mman.h>` INHERIT_COPY, INHERIT_NONE, INHERIT_ZERO.
    const INHERIT_COPY: c_int = 1;
    const INHERIT_NONE: c_int = 2;
    const INHERIT_ZERO: c_int = 3;
    // DONTFORK/DOFORK and WIPEONFORK/KEEPONFORK are FreeBSD's minherit.
    let inherit = match advice {
        10 => Some(INHERIT_NONE),
        11 | 19 => Some(INHERIT_COPY),
        18 => Some(INHERIT_ZERO),
        _ => None,
    };
    if let Some(i) = inherit {
        extern "C" {
            fn minherit(addr: *mut c_void, len: usize, inherit: c_int) -> c_int;
        }
        // SAFETY: the caller's range.
        return unsafe { minherit(addr, len, i) };
    }
    match madvise_advice(advice) {
        // SAFETY: the caller's range.
        Ok(Some(a)) => unsafe { libc::madvise(addr, len, a) },
        Ok(None) => 0,
        Err(()) => {
            set_errno(EINVAL);
            -1
        }
    }
}

/// Linux `msync` flags: MS_ASYNC 1, MS_INVALIDATE 2, MS_SYNC 4. FreeBSD's
/// MS_SYNC is 0.
pub fn msync_flags(lx: c_int) -> Option<c_int> {
    if lx & !7 != 0 || (lx & 1 != 0 && lx & 4 != 0) {
        return None;
    }
    let mut f = 0;
    if lx & 1 != 0 {
        f |= libc::MS_ASYNC;
    }
    if lx & 2 != 0 {
        f |= libc::MS_INVALIDATE;
    }
    Some(f)
}

unsafe extern "C" fn b_msync(addr: *mut c_void, len: usize, flags: c_int) -> c_int {
    match msync_flags(flags) {
        // SAFETY: the caller's range.
        Some(f) => unsafe { libc::msync(addr, len, f) },
        None => {
            set_errno(EINVAL);
            -1
        }
    }
}

/// bionic's `LC_*_MASK` bits (1 << category, LC_CTYPE being 0) to FreeBSD's.
/// glibc's extra categories (LC_PAPER and on) have no FreeBSD counterpart.
pub fn locale_mask(lx: c_int) -> c_int {
    const MAP: [(c_int, c_int); 6] = [
        (1 << 0, libc::LC_CTYPE_MASK),
        (1 << 1, libc::LC_NUMERIC_MASK),
        (1 << 2, libc::LC_TIME_MASK),
        (1 << 3, libc::LC_COLLATE_MASK),
        (1 << 4, libc::LC_MONETARY_MASK),
        (1 << 5, libc::LC_MESSAGES_MASK),
    ];
    MAP.iter().filter(|(l, _)| lx & l != 0).fold(0, |acc, (_, f)| acc | f)
}

unsafe extern "C" fn b_newlocale(mask: c_int, name: *const c_char, base: libc::locale_t) -> libc::locale_t {
    // SAFETY: as the caller's newlocale, with the mask in FreeBSD's bits.
    unsafe { libc::newlocale(locale_mask(mask), name, base) }
}

extern "C" {
    fn pthread_setname_np(thread: libc::pthread_t, name: *const c_char) -> c_int;
    fn pthread_getthreadid_np() -> c_int;
    fn pthread_atfork(
        prepare: Option<unsafe extern "C" fn()>,
        parent: Option<unsafe extern "C" fn()>,
        child: Option<unsafe extern "C" fn()>,
    ) -> c_int;
}

/// bionic refuses a name longer than the kernel's 15 characters with ERANGE.
unsafe extern "C" fn b_pthread_setname_np(thread: libc::pthread_t, name: *const c_char) -> c_int {
    if name.is_null() {
        return EINVAL;
    }
    // SAFETY: a NUL-terminated name.
    if unsafe { libc::strlen(name) } > 15 {
        return ERANGE;
    }
    // SAFETY: a thread handle the host gave the engine.
    unsafe { pthread_setname_np(thread, name) }
}

unsafe extern "C" fn b_gettid() -> c_int {
    // SAFETY: no arguments.
    unsafe { pthread_getthreadid_np() }
}

/// bionic's fork-handler registration, with the owning DSO bionic tracks
/// for unloading (nothing here unloads the engine).
unsafe extern "C" fn b_register_atfork(
    prepare: Option<unsafe extern "C" fn()>,
    parent: Option<unsafe extern "C" fn()>,
    child: Option<unsafe extern "C" fn()>,
    _dso: *mut c_void,
) -> c_int {
    // SAFETY: plain registration.
    unsafe { pthread_atfork(prepare, parent, child) }
}

/// FreeBSD has no mremap; Linux's answer when it cannot remap.
unsafe extern "C" fn b_mremap(_old: *mut c_void, _old_len: usize, _new_len: usize, _flags: c_int) -> *mut c_void {
    set_errno(ENOMEM);
    libc::MAP_FAILED
}

/// Linux `CMSG_NXTHDR` over the engine's Linux-layout `msghdr` (msg_control
/// at 32, msg_controllen at 40) and `cmsghdr` (16 bytes, length first).
unsafe extern "C" fn b_cmsg_nxthdr(msg: *const u8, cmsg: *const u8) -> *const u8 {
    if msg.is_null() || cmsg.is_null() {
        return std::ptr::null();
    }
    let align = |n: usize| (n + 7) & !7;
    // SAFETY: the engine's msghdr and one of its control messages.
    unsafe {
        let control = std::ptr::read_unaligned(msg.add(32).cast::<usize>());
        let controllen = std::ptr::read_unaligned(msg.add(40).cast::<usize>());
        let len = std::ptr::read_unaligned(cmsg.cast::<usize>());
        if len < 16 {
            return std::ptr::null();
        }
        let next = cmsg.add(align(len));
        let end = control + controllen;
        if (next as usize) + 16 > end {
            return std::ptr::null();
        }
        next
    }
}

// --------------------------------------------------------------- misc

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Mallinfo([usize; 10]);

/// bionic's ten-`size_t` `mallinfo`, from the host's `mallinfo2` if it has
/// one; FreeBSD does not, so the engine gets zeros.
unsafe extern "C" fn b_mallinfo() -> Mallinfo {
    // SAFETY: a lookup through the default handle.
    let f = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"mallinfo2".as_ptr()) };
    if f.is_null() {
        return Mallinfo::default();
    }
    // SAFETY: glibc's mallinfo2 returns the same ten size_t.
    let f: extern "C" fn() -> Mallinfo = unsafe { std::mem::transmute(f) };
    f()
}

extern "C" fn b_android_get_device_api_level() -> c_int {
    33
}

// ------------------------------------------------------------ data symbols

/// One word, low byte zero, constant for the process.
static STACK_CHK_GUARD: u64 = 0x4f7c_29d3_86ab_e100;

/// The pre-API-23 `__sF`: three 152-byte `FILE` slots (stdin, stdout,
/// stderr). Never written; `legacy_streams.cpp` maps a pointer into it to the
/// host stream of its slot.
static LEGACY_SF: [u8; 3 * LEGACY_SF_STRIDE] = [0; 3 * LEGACY_SF_STRIDE];
const LEGACY_SF_STRIDE: usize = 152;

#[no_mangle]
pub extern "C" fn intoxicated_legacy_sf_base() -> *const c_void {
    LEGACY_SF.as_ptr().cast()
}

#[no_mangle]
pub extern "C" fn intoxicated_legacy_sf_stride() -> usize {
    LEGACY_SF_STRIDE
}

extern "C" {
    // FreeBSD's spellings of bionic's `stdin`, `stdout` and `stderr`: each a
    // `FILE*` variable, which is exactly what bionic exports.
    static mut __stdinp: *mut libc::FILE;
    static mut __stdoutp: *mut libc::FILE;
    static mut __stderrp: *mut libc::FILE;
}

/// bionic's `long timezone` (seconds west of UTC, standard time) and `int
/// daylight` (whether the zone has summer time). FreeBSD has neither as data:
/// its `timezone` is a function. Filled from the host's zone rules when the
/// table is built and again on every `tzset`.
static TIMEZONE: AtomicI64 = AtomicI64::new(0);
static DAYLIGHT: AtomicI32 = AtomicI32::new(0);

/// (seconds west of UTC in standard time, whether summer time exists), from
/// the local zone's offsets on January 1 and July 1 of this year.
pub fn zone_facts() -> (i64, i32) {
    extern "C" {
        fn tzset();
    }
    // SAFETY: tzset, time, mktime and localtime_r on local structs.
    unsafe {
        tzset();
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        let probe = |mon: c_int| {
            let mut t: libc::tm = std::mem::zeroed();
            t.tm_year = tm.tm_year;
            t.tm_mon = mon;
            t.tm_mday = 1;
            t.tm_hour = 12;
            t.tm_isdst = -1;
            let at = libc::mktime(&mut t);
            let mut out: libc::tm = std::mem::zeroed();
            libc::localtime_r(&at, &mut out);
            (out.tm_gmtoff, out.tm_isdst > 0)
        };
        let (jan_off, jan_dst) = probe(0);
        let (jul_off, jul_dst) = probe(6);
        let standard = if jan_dst && !jul_dst { jul_off } else if jul_dst && !jan_dst { jan_off } else { jan_off };
        (-standard, i32::from(jan_dst || jul_dst))
    }
}

fn refresh_zone() {
    let (tz, dl) = zone_facts();
    TIMEZONE.store(tz, Ordering::Relaxed);
    DAYLIGHT.store(dl, Ordering::Relaxed);
}

unsafe extern "C" fn b_tzset() {
    refresh_zone();
}

/// The data symbols: each value is the address the engine reads through.
pub fn data() -> Vec<(&'static str, *mut c_void)> {
    refresh_zone();
    // Only the addresses of the host's variables are taken.
    let (i, o, e) = (
        std::ptr::addr_of_mut!(__stdinp).cast::<c_void>(),
        std::ptr::addr_of_mut!(__stdoutp).cast::<c_void>(),
        std::ptr::addr_of_mut!(__stderrp).cast::<c_void>(),
    );
    vec![
        ("__stack_chk_guard", std::ptr::addr_of!(STACK_CHK_GUARD) as *mut c_void),
        ("__sF", LEGACY_SF.as_ptr() as *mut c_void),
        ("stdin", i),
        ("stdout", o),
        ("stderr", e),
        ("timezone", TIMEZONE.as_ptr().cast()),
        ("daylight", DAYLIGHT.as_ptr().cast()),
    ]
}

// --------------------------------------------------------------- the sets

extern "C" {
    fn intoxicated_legacy_fflush();
    fn intoxicated_legacy_fclose();
    fn intoxicated_legacy_fseek();
    fn intoxicated_legacy_ftell();
    fn intoxicated_legacy_fputs();
    fn intoxicated_legacy_setvbuf();
    fn intoxicated_legacy_fread();
    fn intoxicated_legacy_fwrite();
    fn intoxicated_legacy_fprintf();
    fn intoxicated_legacy_vfprintf();
    fn intoxicated_legacy_fputc();
    fn intoxicated_legacy_fgets();
    fn intoxicated_legacy_ferror();
    fn intoxicated_legacy_feof();
    fn intoxicated_legacy_fileno();
    fn __cxa_thread_atexit_impl();
}

/// What a FreeBSD libc either lacks or answers in another ABI, not counting
/// the fixed tables registered by the aggregator.
pub fn generic() -> Vec<(&'static str, *mut c_void)> {
    vec![
        f!("__assert", b_assert),
        f!("__assert2", b_assert2),
        f!("__strlen_chk", b_strlen_chk),
        f!("__strchr_chk", b_strchr_chk),
        f!("__strncpy_chk2", b_strncpy_chk2),
        f!("__strlcpy_chk", b_strlcpy_chk),
        f!("__umask_chk", b_umask_chk),
        f!("__readlink_chk", b_readlink_chk),
        f!("__fread_chk", b_fread_chk),
        f!("__system_property_get", b_system_property_get),
        f!("mallinfo", b_mallinfo),
        f!("__cxa_thread_atexit_impl", __cxa_thread_atexit_impl),
        f!("android_get_device_api_level", b_android_get_device_api_level),
        f!("sysconf", b_sysconf),
        f!("fflush", intoxicated_legacy_fflush),
        f!("fclose", intoxicated_legacy_fclose),
        f!("fseek", intoxicated_legacy_fseek),
        f!("ftell", intoxicated_legacy_ftell),
        f!("fputs", intoxicated_legacy_fputs),
        f!("setvbuf", intoxicated_legacy_setvbuf),
        f!("fread", intoxicated_legacy_fread),
        f!("fwrite", intoxicated_legacy_fwrite),
        f!("fprintf", intoxicated_legacy_fprintf),
        f!("vfprintf", intoxicated_legacy_vfprintf),
        f!("fputc", intoxicated_legacy_fputc),
        f!("fgets", intoxicated_legacy_fgets),
        f!("ferror", intoxicated_legacy_ferror),
        f!("feof", intoxicated_legacy_feof),
        f!("fileno", intoxicated_legacy_fileno),
        f!("clock", b_clock),
        f!("madvise", b_madvise),
        f!("msync", b_msync),
        f!("newlocale", b_newlocale),
        f!("pthread_setname_np", b_pthread_setname_np),
        f!("gettid", b_gettid),
        f!("__register_atfork", b_register_atfork),
        f!("mremap", b_mremap),
        f!("__cmsg_nxthdr", b_cmsg_nxthdr),
        f!("tzset", b_tzset),
    ]
}

/// The FreeBSD set: registrations of `native/freebsd_libc_compat.c` and
/// `native/fortify_chk.c`, which translate bionic's ABI for FreeBSD.
pub fn freebsd() -> Vec<(&'static str, *mut c_void)> {
    extern "C" {
        fn __memcpy_chk();
        fn __memmove_chk();
        fn __memset_chk();
        fn __strcpy_chk();
        fn __strcat_chk();
        fn __strncpy_chk();
        fn __pread64_chk();
        fn __pread_chk();
        fn bionic_roundl();
        fn bionic_llroundl();
        fn bionic_powl();
        fn bionic_fmal();
        fn bionic_strtold_l();
        fn __fwrite_chk();
        fn __vsnprintf_chk();
        fn __vsprintf_chk();
        fn __FD_SET_chk();
        fn __FD_CLR_chk();
        fn __FD_ISSET_chk();
        fn __ctype_get_mb_cur_max();
        fn intoxicated_posix_fallocate();
        fn bionic_syscall();
        fn bionic_pthread_mutex_init();
        fn bionic_pthread_mutex_lock();
        fn bionic_pthread_mutex_unlock();
        fn bionic_pthread_mutex_trylock();
        fn bionic_pthread_mutex_destroy();
        fn bionic_pthread_rwlock_init();
        fn bionic_pthread_rwlock_rdlock();
        fn bionic_pthread_rwlock_wrlock();
        fn bionic_pthread_rwlock_tryrdlock();
        fn bionic_pthread_rwlock_trywrlock();
        fn bionic_pthread_rwlock_unlock();
        fn bionic_pthread_rwlock_destroy();
        fn prctl(option: c_int, ...) -> c_int;
        fn uname();
        fn bionic_sysinfo();
        fn bionic_mmap();
        fn getauxval(kind: c_ulong) -> c_ulong;
        fn bionic_clock_gettime();
        fn bionic_clock_getres();
        fn bionic_timerfd_create();
        fn bionic_timerfd_settime();
        fn bionic_pthread_attr_init();
        fn bionic_pthread_attr_destroy();
        fn bionic_pthread_getattr_np();
        fn bionic_pthread_attr_getstack();
        fn bionic_pthread_attr_setstacksize();
        fn bionic_pthread_attr_setdetachstate();
        fn bionic_pthread_attr_setschedparam();
        fn sched_setscheduler();
        fn sched_setparam();
        fn pipe(fds: *mut c_int) -> c_int;
    }
    vec![
        f!("__ctype_get_mb_cur_max", __ctype_get_mb_cur_max),
        // FreeBSD posix_fallocate fails (EINVAL) on ZFS, which breaks every
        // mmap-backed content cache and gets the session kicked as "corrupted
        // files" (reason 304). The shim falls back to ftruncate on ZFS.
        f!("posix_fallocate", intoxicated_posix_fallocate),
        f!("syscall", bionic_syscall),
        f!("pthread_mutex_init", bionic_pthread_mutex_init),
        f!("pthread_mutex_lock", bionic_pthread_mutex_lock),
        f!("pthread_mutex_unlock", bionic_pthread_mutex_unlock),
        f!("pthread_mutex_trylock", bionic_pthread_mutex_trylock),
        f!("pthread_mutex_destroy", bionic_pthread_mutex_destroy),
        f!("pthread_rwlock_init", bionic_pthread_rwlock_init),
        f!("pthread_rwlock_rdlock", bionic_pthread_rwlock_rdlock),
        f!("pthread_rwlock_wrlock", bionic_pthread_rwlock_wrlock),
        f!("pthread_rwlock_tryrdlock", bionic_pthread_rwlock_tryrdlock),
        f!("pthread_rwlock_trywrlock", bionic_pthread_rwlock_trywrlock),
        f!("pthread_rwlock_unlock", bionic_pthread_rwlock_unlock),
        f!("pthread_rwlock_destroy", bionic_pthread_rwlock_destroy),
        f!("prctl", prctl),
        // libroblox imports `uname`; without this it binds the host FreeBSD
        // one, which answers sysname="FreeBSD" machine="amd64" (a kernel tell)
        // through a struct of the wrong field size. Shim answers Linux/x86_64.
        f!("uname", uname),
        f!("sysinfo", bionic_sysinfo),
        f!("mmap", bionic_mmap),
        f!("mmap64", bionic_mmap),
        f!("getauxval", getauxval),
        f!("clock_gettime", bionic_clock_gettime),
        f!("clock_getres", bionic_clock_getres),
        // The engine paces its frame loop with a timerfd; bionic passes Linux
        // clockids/flags that FreeBSD misreads (clockid 1 = CLOCK_VIRTUAL, not
        // MONOTONIC), so the frame timer never fires and the TaskScheduler
        // clock never steps. These translate both.
        f!("timerfd_create", bionic_timerfd_create),
        f!("timerfd_settime", bionic_timerfd_settime),
        // bionic pthread_attr_t is a by-value 56-byte struct; FreeBSD's is an
        // opaque pointer. Own the whole family so no bionic attr ever reaches
        // host libthr (which would free() its first qword as a pointer).
        f!("pthread_attr_init", bionic_pthread_attr_init),
        f!("pthread_attr_destroy", bionic_pthread_attr_destroy),
        f!("pthread_getattr_np", bionic_pthread_getattr_np),
        f!("pthread_attr_getstack", bionic_pthread_attr_getstack),
        f!("pthread_attr_setstacksize", bionic_pthread_attr_setstacksize),
        f!("pthread_attr_setdetachstate", bionic_pthread_attr_setdetachstate),
        f!("pthread_attr_setschedparam", bionic_pthread_attr_setschedparam),
        f!("sched_setscheduler", sched_setscheduler),
        f!("sched_setparam", sched_setparam),
        // Track pipe fd pairs so the runtime can find the GameActivity command
        // pipe's write end (it only sees the read end via ALooper_addFd).
        // `pipe2` is registered by `fbsd_abi`, which translates the engine's
        // Linux flags and then calls the same recording function.
        f!("pipe", pipe),
        f!("__memcpy_chk", __memcpy_chk),
        f!("__memmove_chk", __memmove_chk),
        f!("__memset_chk", __memset_chk),
        f!("__strcpy_chk", __strcpy_chk),
        f!("__strcat_chk", __strcat_chk),
        f!("__strncpy_chk", __strncpy_chk),
        f!("__pread64_chk", __pread64_chk),
        f!("__pread_chk", __pread_chk),
        // bionic's long double is binary128; see native/freebsd_libc_compat.c.
        f!("roundl", bionic_roundl),
        f!("llroundl", bionic_llroundl),
        f!("powl", bionic_powl),
        f!("fmal", bionic_fmal),
        f!("strtold_l", bionic_strtold_l),
        f!("__fwrite_chk", __fwrite_chk),
        f!("__vsnprintf_chk", __vsnprintf_chk),
        f!("__vsprintf_chk", __vsprintf_chk),
        f!("__FD_SET_chk", __FD_SET_chk),
        f!("__FD_CLR_chk", __FD_CLR_chk),
        f!("__FD_ISSET_chk", __FD_ISSET_chk),
    ]
}

/// `environ` is a *data* symbol defined in crt startup, not in libc.so, so
/// the host-libc lookup rejects it and it would fall to a function stub; the
/// engine, reading `environ` as `char**`, would dereference that stub and
/// SIGBUS. A data override's value is the symbol's address, so this is the
/// address of the real environ variable.
pub fn environ() -> (&'static str, *mut c_void) {
    extern "C" {
        static mut environ: *mut *mut c_char;
    }
    ("environ", std::ptr::addr_of_mut!(environ) as *mut c_void)
}

#[cfg(test)]
mod tests;
