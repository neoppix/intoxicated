//! Diagnostics around the engine's libc: what it says when it gives up, and
//! an optional trace of the calls it makes.
//!
//! Three wrappers are always installed, for the ways the engine ends itself:
//! `abort`, `__stack_chk_fail` and `__android_log_assert`. Each says on stderr
//! which one it was, prints the stub report (an unimplemented call is the
//! usual reason a well-behaved engine aborts), and then aborts for real, so a
//! core and the exit status are what they would have been.
//!
//! The verbose wrappers forward a call and print one line about it.
//! Enabled with `INTOXICATED_TRACE=1`. Off by default, and **not merely because it
//! is noisy**: every traced call passes through one more frame between the
//! engine and the host. `bionic::function_overrides` therefore installs a
//! verbose wrapper only for a name nothing earlier translates, and with the
//! tables as they are that leaves two: `getenv` and `open64`. Neither is
//! variadic in a way that matters here: `getenv` is not variadic at all, and
//! `open64` forwards its mode only when its flags say one was passed.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

/// Whether the verbose wrappers print.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Lines printed so far; the next line's number.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Turn the verbose trace on for the whole process.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// A C string for a message: `(null)` for null, invalid UTF-8 replaced.
///
/// # Safety
/// `p` is null or a NUL-terminated string that outlives the result.
unsafe fn text<'a>(p: *const c_char) -> std::borrow::Cow<'a, str> {
    if p.is_null() {
        "(null)".into()
    } else {
        // SAFETY: the caller's contract.
        unsafe { CStr::from_ptr(p) }.to_string_lossy()
    }
}

/// One trace line as printed: the sequence number right-aligned in six, the
/// FreeBSD thread id, the call.
fn format_line(sequence: u64, tid: c_int, call: std::fmt::Arguments<'_>) -> String {
    format!("[{sequence:>6}] tid={tid} {call}\n")
}

/// Print one trace line, while the trace is on. A failed write is dropped:
/// nothing here may stop the engine.
fn line(call: std::fmt::Arguments<'_>) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    // SAFETY: no arguments; returns the calling thread's id.
    let tid = unsafe { libc::pthread_getthreadid_np() };
    let _ = std::io::stderr().lock().write_all(format_line(sequence, tid, call).as_bytes());
}

// ------------------------------------------------------------ always on

/// Say what happened, print the stub report, and abort through the host.
fn report_and_abort(what: std::fmt::Arguments<'_>) -> ! {
    {
        let mut err = std::io::stderr().lock();
        let _ = err.write_fmt(what);
        let _ = err.flush();
    }
    crate::stub_table::report();
    // SAFETY: ends the process with SIGABRT, as the engine asked.
    unsafe { libc::abort() }
}

extern "C" fn trace_abort() -> ! {
    report_and_abort(format_args!("\n*** Roblox called abort() ***\n"))
}

extern "C" fn trace_stack_chk_fail() -> ! {
    report_and_abort(format_args!(
        "\n*** stack protector tripped inside Roblox ***\n\
         \x20   A callee wrote past its frame. If Intoxicated provides that callee,\n\
         \x20   its signature or a struct layout is wrong (see bionic::threads).\n"
    ))
}

/// Android liblog's `__android_log_assert(cond, tag, fmt, ...)`. The format
/// is printed as it is; its arguments are never read, which is why the
/// variadic tail can be left out of this signature.
extern "C" fn trace_android_log_assert(cond: *const c_char, tag: *const c_char, fmt: *const c_char) -> ! {
    // SAFETY: liblog's contract: each is null or a NUL-terminated string.
    let (cond, tag, fmt) = unsafe { (text(cond), text(tag), text(fmt)) };
    report_and_abort(format_args!(
        "\n*** Roblox log-assert ***\n    tag:       {tag}\n    condition: {cond}\n    message:   {fmt}\n"
    ))
}

/// The three wrappers that are always installed. They come late in the
/// override list, so they win over any earlier entry for the same name.
pub fn always_on() -> Vec<(&'static str, *mut c_void)> {
    vec![
        ("abort", trace_abort as *const () as *mut c_void),
        ("__stack_chk_fail", trace_stack_chk_fail as *const () as *mut c_void),
        ("__android_log_assert", trace_android_log_assert as *const () as *mut c_void),
    ]
}

// ------------------------------------------------------------ verbose

extern "C" fn trace_getenv(name: *const c_char) -> *mut c_char {
    // SAFETY: forwarded as the engine passed it.
    let value = unsafe { libc::getenv(name) };
    if ENABLED.load(Ordering::Relaxed) {
        // SAFETY: `name` is the engine's string; `value` points into the
        // environment and stays valid for the line.
        let (n, v) = unsafe { (text(name), text(value)) };
        line(format_args!("getenv({n:?}) = {v:?}"));
    }
    value
}

/// Bionic's `O_CREAT` and `O_TMPFILE`: the flags arrive in Linux's numbering.
const LINUX_O_CREAT: c_int = 0o100;
const LINUX_O_TMPFILE: c_int = 0o20200000;

/// Whether an open with `flags` passed a mode, by bionic's rule.
fn takes_mode(flags: c_int) -> bool {
    flags & LINUX_O_CREAT != 0 || flags & LINUX_O_TMPFILE == LINUX_O_TMPFILE
}

type OpenFn = unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int;

/// The `open` the engine itself is given: FreeBSD has no `open64`, and the
/// flags need the same translation and the path the same rewrite and hiding.
fn engine_open() -> Option<OpenFn> {
    static OPEN: OnceLock<usize> = OnceLock::new();
    let addr = *OPEN.get_or_init(|| {
        super::function_overrides()
            .into_iter()
            .filter(|(name, _)| *name == "open")
            .last()
            .map_or(0, |(_, addr)| addr as usize)
    });
    // SAFETY: the table's `open` is `s_open`-shaped: variadic, path and flags first.
    (addr != 0).then(|| unsafe { std::mem::transmute::<usize, OpenFn>(addr) })
}

/// Bionic's `open64(path, flags, ...)`. The mode is read from where a third
/// argument would be and forwarded only when the flags say one was passed.
extern "C" fn trace_open64(path: *const c_char, flags: c_int, mode: c_uint) -> c_int {
    let fd = match engine_open() {
        // SAFETY: forwarded as the engine passed it.
        Some(open) if takes_mode(flags) => unsafe { open(path, flags, mode) },
        // SAFETY: as above.
        Some(open) => unsafe { open(path, flags) },
        None => -1,
    };
    if ENABLED.load(Ordering::Relaxed) {
        // SAFETY: the engine's path string.
        let p = unsafe { text(path) };
        line(format_args!("open64({p:?}, {flags:#x}) = {fd}"));
    }
    fd
}

/// The trace wrappers. `bionic::function_overrides` adds each only where no
/// earlier table has the name.
pub fn verbose() -> Vec<(&'static str, *mut c_void)> {
    vec![
        ("getenv", trace_getenv as *const () as *mut c_void),
        ("open64", trace_open64 as *const () as *mut c_void),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_always_on_wrappers_are_the_three_ways_out() {
        let names: Vec<&str> = always_on().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["abort", "__stack_chk_fail", "__android_log_assert"]);
        assert!(always_on().iter().all(|(_, a)| !a.is_null()));
    }

    #[test]
    fn the_verbose_wrappers_are_distinct() {
        let v = verbose();
        let mut names: Vec<&str> = v.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), v.len());
        assert!(v.iter().all(|(_, a)| !a.is_null()));
    }

    #[test]
    fn a_line_is_numbered_in_six_and_names_its_thread() {
        assert_eq!(
            format_line(7, 100123, format_args!("getenv({:?}) = {:?}", "HOME", "/home/x")),
            "[     7] tid=100123 getenv(\"HOME\") = \"/home/x\"\n"
        );
        assert_eq!(format_line(1234567, 1, format_args!("x")), "[1234567] tid=1 x\n");
    }

    #[test]
    fn getenv_answers_what_the_host_does() {
        assert_eq!(trace_getenv(c"PATH".as_ptr()), unsafe { libc::getenv(c"PATH".as_ptr()) });
        assert!(trace_getenv(c"INTOXICATED_SURELY_UNSET_FOR_THIS_TEST".as_ptr()).is_null());
    }

    #[test]
    fn open64_translates_the_flags_and_passes_a_mode_only_when_asked() {
        // Linux O_CLOEXEC (0x80000): FreeBSD's is another bit.
        let fd = trace_open64(c"/etc/hosts".as_ptr(), 0x80000, 0xdead);
        assert!(fd >= 0);
        // SAFETY: our descriptor.
        assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
        unsafe { libc::close(fd) };

        let dir = std::env::temp_dir().join(format!("intoxicated-trace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::ffi::CString::new(dir.join("created").into_os_string().into_encoded_bytes()).unwrap();
        // Linux O_WRONLY | O_CREAT, mode 0600.
        let fd = trace_open64(file.as_ptr(), 0o1 | LINUX_O_CREAT, 0o600);
        assert!(fd >= 0);
        unsafe { libc::close(fd) };
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join("created")).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(takes_mode(LINUX_O_CREAT) && takes_mode(LINUX_O_TMPFILE));
        assert!(!takes_mode(0o200000), "O_DIRECTORY alone is not O_TMPFILE");
    }

    /// The wrapper ends the process, so it runs in a child copy of this test
    /// binary, with core dumps off so nothing is left in the tree.
    #[test]
    fn a_log_assert_reports_and_aborts() {
        if std::env::var_os("INTOXICATED_ASSERT_CHILD").is_some() {
            let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            // SAFETY: a valid rlimit for this process.
            unsafe { libc::setrlimit(libc::RLIMIT_CORE, &none) };
            trace_android_log_assert(c"x > 0".as_ptr(), c"tag".as_ptr(), std::ptr::null());
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "bionic::trace::tests::a_log_assert_reports_and_aborts", "--nocapture"])
            .env("INTOXICATED_ASSERT_CHILD", "1")
            .output()
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(libc::SIGABRT), "{:?}", out.status);
        let err = String::from_utf8_lossy(&out.stderr);
        let at = err.find("*** Roblox log-assert ***").unwrap_or_else(|| panic!("{err}"));
        assert!(
            err[at..].starts_with(
                "*** Roblox log-assert ***\n    tag:       tag\n    condition: x > 0\n    message:   (null)\n"
            ),
            "{err}"
        );
        assert!(err.contains("=== no stubs were called ===") || err.contains("=== stubs called:"), "{err}");
    }
}
