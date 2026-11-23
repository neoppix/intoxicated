//! The libc layer the engine is linked against: every bionic function or
//! variable whose FreeBSD namesake would misread the engine's arguments, or
//! that FreeBSD does not have.
//!
//! [`function_overrides`] is the list the symbol table (`resolve`) consults
//! first, built from these sets in this order, a later name replacing an
//! earlier one:
//!
//! 1. [`overrides::generic`]: FORTIFY names, bionic-only entry points,
//!    `sysconf`, the legacy `__sF` streams and the numbering translations;
//! 2. [`overrides::freebsd`] and `environ`: the registrations of
//!    `native/freebsd_libc_compat.c` and `native/fortify_chk.c`;
//! 3. the fixed ABI table (`fbsd_abi`), after removing its names from 1 and 2;
//! 4. signals, 5. condition variables, semaphores, once and keys;
//! 6. liblog, 7. the system path table and its `__open_2`, 8. netdb,
//!    9. OpenSL ES, 10. `pthread_create`;
//! 11. the always-on diagnostics of `trace`, and with `INTOXICATED_TRACE` set
//!     its verbose wrappers for names nothing above translates.
//!
//! Building the list has three effects and no others:
//! `INTOXICATED_TRACE_PATHS` turns the path trace on,
//! `INTOXICATED_TRACE_THREADS` the thread-creation trace, and
//! `INTOXICATED_TRACE` the trace module.

use std::ffi::{c_char, c_int, c_void, CStr};

pub mod fbsd_abi;
pub mod trace;

mod audio;
pub mod overrides;
pub mod signals;
pub mod threads;

pub use audio::{aaudio_overrides, announce_audio_backend, opensles_overrides};
/// The names the rest of the crate knew these modules by.
pub use signals as signal;
pub use threads as pthread;

#[repr(C)]
struct Symbol {
    name: *const c_char,
    addr: *mut c_void,
}

/// A C `*_symbols(size_t* count)` table; a null table contributes nothing.
fn c_table(get: unsafe extern "C" fn(*mut usize) -> *const Symbol) -> Vec<(&'static str, *mut c_void)> {
    let mut count = 0usize;
    // SAFETY: each table is a static array in its C file.
    let table = unsafe { get(&mut count) };
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `count` initialised entries with static names.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .filter(|e| !e.name.is_null())
        .map(|e| {
            // SAFETY: the names are string literals in the C files.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
}

extern "C" {
    fn intoxicated_liblog_symbols(count: *mut usize) -> *const Symbol;
    fn intoxicated_system_symbols(count: *mut usize) -> *const Symbol;
    fn intoxicated_netdb_symbols(count: *mut usize) -> *const Symbol;
    fn intoxicated_thread_symbols(count: *mut usize) -> *const Symbol;
    fn intoxicated_set_path_trace(on: c_int);
    fn intoxicated_set_thread_trace(on: c_int);
    fn intoxicated_open_2(path: *const c_char, flags: c_int) -> c_int;
    fn intoxicated_set_power_supply_reader(read: extern "C" fn(*const c_char, *mut c_char, usize) -> isize);
}

/// `native/android_log.cpp`.
pub fn liblog_overrides() -> Vec<(&'static str, *mut c_void)> {
    c_table(intoxicated_liblog_symbols)
}

/// `native/system_paths.cpp`: the path rewrite, root-file hiding, `/proc`
/// and `/sys` synthesis and the `stat`/`dirent`/`statfs` layouts. Its
/// `/sys/class/power_supply` answers come from `power.rs`, handed over here.
pub fn system_path_overrides() -> Vec<(&'static str, *mut c_void)> {
    // SAFETY: stores a function pointer with the declared signature.
    unsafe { intoxicated_set_power_supply_reader(crate::power::intoxicated_power_supply_text) };
    c_table(intoxicated_system_symbols)
}

/// `native/netdb_linux.cpp`.
pub fn netdb_overrides() -> Vec<(&'static str, *mut c_void)> {
    c_table(intoxicated_netdb_symbols)
}

/// `native/thread_create.cpp`: `pthread_create`.
pub fn thread_overrides() -> Vec<(&'static str, *mut c_void)> {
    c_table(intoxicated_thread_symbols)
}

/// The data symbols; each value is the address the engine reads through.
pub fn data_overrides() -> Vec<(&'static str, *mut c_void)> {
    overrides::data()
}

/// Add the trace module's verbose wrappers, but only for names nothing
/// already translates: they forward to the host untranslated, and replacing
/// `sysconf`, `syscall`, `openat` or `access` with that would break them.
fn add_verbose(v: &mut Vec<(&'static str, *mut c_void)>, wrappers: Vec<(&'static str, *mut c_void)>) {
    for (name, addr) in wrappers {
        if !v.iter().any(|(n, _)| *n == name) {
            v.push((name, addr));
        }
    }
}

/// Every function override, in precedence order; see the module doc.
pub fn function_overrides() -> Vec<(&'static str, *mut c_void)> {
    if std::env::var_os("INTOXICATED_TRACE_PATHS").is_some() {
        // SAFETY: sets a flag.
        unsafe { intoxicated_set_path_trace(1) };
    }
    if std::env::var_os("INTOXICATED_TRACE_THREADS").is_some() {
        // SAFETY: sets a flag.
        unsafe { intoxicated_set_thread_trace(1) };
    }
    let verbose = std::env::var_os("INTOXICATED_TRACE").is_some();
    if verbose {
        trace::enable();
    }

    let mut v = overrides::generic();
    v.extend(overrides::freebsd());
    v.push(overrides::environ());
    // The engine's Linux flag words, sockaddrs and errno numbers, translated
    // for FreeBSD -- `native/freebsd_abi.c`. Names it has are dropped from the
    // sets above rather than left to whichever duplicate wins.
    let abi = fbsd_abi::overrides();
    v.retain(|(name, _)| !abi.iter().any(|(n, _)| n == name));
    v.extend(abi);
    v.extend(signals::overrides());
    v.extend(threads::overrides());
    v.extend(liblog_overrides());
    v.extend(system_path_overrides());
    // FORTIFY's `open`: the same hiding, flag translation, /proc synthesis and
    // path rewrite as `open`, replacing the fixed ABI table's, which only
    // rewrote the path.
    v.push(("__open_2", intoxicated_open_2 as *mut c_void));
    v.extend(netdb_overrides());
    v.extend(opensles_overrides());
    v.extend(thread_overrides());
    v.extend(trace::always_on());
    if verbose {
        add_verbose(&mut v, trace::verbose());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::CString;

    fn map() -> BTreeMap<&'static str, *mut c_void> {
        function_overrides().into_iter().collect()
    }

    #[test]
    fn diagnostics_and_fixed_tables_win_where_they_should() {
        let m = map();
        let always: BTreeMap<_, _> = trace::always_on().into_iter().collect();
        assert_eq!(m["__android_log_assert"], always["__android_log_assert"]);
        let abi: BTreeMap<_, _> = fbsd_abi::overrides().into_iter().collect();
        assert_eq!(m["__errno"], abi["__errno"]);
        assert_eq!(m["__open_2"], intoxicated_open_2 as *mut c_void);
        assert_ne!(m["__open_2"], abi["__open_2"]);
    }

    #[test]
    fn verbose_wrappers_never_replace_a_translation() {
        let mut v = vec![("sysconf", 1usize as *mut c_void)];
        add_verbose(&mut v, vec![("sysconf", 2usize as *mut c_void), ("getenv", 3usize as *mut c_void)]);
        assert_eq!(v, vec![("sysconf", 1usize as *mut c_void), ("getenv", 3usize as *mut c_void)]);
    }

    #[test]
    fn the_engines_open_2_hides_root_tools_and_opens_the_rest() {
        type Open2 = unsafe extern "C" fn(*const c_char, c_int) -> c_int;
        let open_2: Open2 = unsafe { std::mem::transmute(map()["__open_2"]) };
        let errno_fn: unsafe extern "C" fn() -> *mut c_int = unsafe { std::mem::transmute(map()["__errno"]) };
        // FreeBSD ships /usr/bin/su; the device the engine believes in does not.
        assert!(std::path::Path::new("/usr/bin/su").exists());
        let su = CString::new("/usr/bin/su").unwrap();
        assert_eq!(unsafe { open_2(su.as_ptr(), 0) }, -1);
        assert_eq!(unsafe { *errno_fn() }, 2, "ENOENT");
        let fd = unsafe { open_2(c"/etc/hosts".as_ptr(), 0x80000) }; // Linux O_CLOEXEC
        assert!(fd >= 0);
        // SAFETY: our descriptor.
        assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC, 0, "Linux O_CLOEXEC not translated");
        unsafe { libc::close(fd) };
    }

    /// The synthesis reads `INTOXICATED_FAKE_PROC` on every call, and setting
    /// it here would race every other test, so the checks run in a child copy
    /// of this test binary with it set.
    #[test]
    fn proc_and_sys_are_synthesised_without_a_linux_procfs() {
        if std::env::var_os("INTOXICATED_SYNTH_CHILD").is_some() {
            return synthesis_checks();
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "bionic::tests::proc_and_sys_are_synthesised_without_a_linux_procfs", "--test-threads=1"])
            .env("INTOXICATED_SYNTH_CHILD", "1")
            .env("INTOXICATED_FAKE_PROC", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn read_fd(fd: c_int) -> String {
        let mut text = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: our descriptor and buffer.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            text.extend_from_slice(&buf[..n as usize]);
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    fn synthesis_checks() {
        type Open = unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int;
        type Open2 = unsafe extern "C" fn(*const c_char, c_int) -> c_int;
        type Fopen = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut libc::FILE;
        type Fstatfs = unsafe extern "C" fn(c_int, *mut u64) -> c_int;
        let m = map();
        let open: Open = unsafe { std::mem::transmute(m["open"]) };
        let open_2: Open2 = unsafe { std::mem::transmute(m["__open_2"]) };
        let fopen: Fopen = unsafe { std::mem::transmute(system_path_overrides().into_iter().find(|(n, _)| *n == "fopen").unwrap().1) };
        let fstatfs: Fstatfs = unsafe { std::mem::transmute(system_path_overrides().into_iter().find(|(n, _)| *n == "fstatfs").unwrap().1) };
        let pid = std::process::id();

        // /proc/self/stat through `__open_2`, the way the engine reads it.
        let fd = unsafe { open_2(c"/proc/self/stat".as_ptr(), 0) };
        assert!(fd >= 0, "/proc/self/stat");
        let stat = read_fd(fd);
        unsafe { libc::close(fd) };
        assert!(stat.starts_with(&format!("{pid} (com.roblox.client) R ")), "{stat}");
        let fields: Vec<&str> = stat.split_whitespace().collect();
        assert_eq!(fields.len(), 52, "{stat}");
        let threads: i32 = fields[19].parse().unwrap();
        assert!(threads >= 1);

        // /proc/self/statm: seven page counts, the resident one real.
        let fd = unsafe { open(c"/proc/self/statm".as_ptr(), 0) };
        assert!(fd >= 0, "/proc/self/statm");
        let statm = read_fd(fd);
        unsafe { libc::close(fd) };
        let pages: Vec<u64> = statm.split_whitespace().map(|v| v.parse().unwrap()).collect();
        assert_eq!(pages.len(), 7, "{statm}");
        assert!(pages[1] > 0 && pages[1] <= pages[0], "{statm}");
        let fd = unsafe { open(c"/proc/sys/vm/overcommit_memory".as_ptr(), 0) };
        assert_eq!(read_fd(fd), "0\n");
        unsafe { libc::close(fd) };

        // /proc/self/status through `fopen`, real thread count.
        let f = unsafe { fopen(c"/proc/self/status".as_ptr(), c"r".as_ptr()) };
        assert!(!f.is_null());
        let mut buf = vec![0u8; 4096];
        let n = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), f) };
        unsafe { libc::fclose(f) };
        let status = String::from_utf8_lossy(&buf[..n]).into_owned();
        assert!(status.contains("TracerPid:\t0\n") && status.contains(&format!("Pid:\t{pid}\n")), "{status}");
        assert!(status.contains("Threads:\t"), "{status}");

        // procfs's statfs magic on a synthetic descriptor.
        let fd = unsafe { open(c"/proc/self/status".as_ptr(), 0) };
        let mut sfs = [0u64; 15];
        assert_eq!(unsafe { fstatfs(fd, sfs.as_mut_ptr()) }, 0);
        assert_eq!(sfs[0], 0x9fa0);
        unsafe { libc::close(fd) };

        // The map, built from kern.proc.vmmap: Linux's columns, nothing under /compat.
        let fd = unsafe { open(c"/proc/self/maps".as_ptr(), 0) };
        assert!(fd >= 0, "/proc/self/maps");
        let maps = read_fd(fd);
        unsafe { libc::close(fd) };
        assert!(maps.lines().count() > 10, "{maps}");
        for line in maps.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            assert!(cols.len() >= 5, "{line}");
            assert!(cols[0].contains('-') && cols[1].len() == 4 && cols[3].contains(':'), "{line}");
        }
        assert!(!maps.contains("compat"), "{maps}");
        assert!(maps.contains("[stack]"), "no stack line");

        let fd = unsafe { open(c"/proc/meminfo".as_ptr(), 0) };
        assert!(read_fd(fd).starts_with("MemTotal:"));
        unsafe { libc::close(fd) };

        // A /proc file the synthesis does not serve is absent, not FreeBSD's
        // (its procfs, mounted here, has /proc/curproc/map).
        assert_eq!(unsafe { open(c"/proc/curproc/map".as_ptr(), 0) }, -1);
        assert!(unsafe { fopen(c"/proc/stat".as_ptr(), c"r".as_ptr()) }.is_null());

        // The battery answers what power.rs tells the engine.
        let fd = unsafe { open(c"/sys/class/power_supply/battery/present".as_ptr(), 0) };
        match crate::power::power_supply_text(&crate::power::report(&crate::power::read()), "/sys/class/power_supply/battery/present") {
            Some(text) => {
                assert!(fd >= 0);
                assert_eq!(read_fd(fd), text);
                unsafe { libc::close(fd) };
            }
            None => assert_eq!(fd, -1),
        }
    }

    #[test]
    fn every_override_names_a_real_address() {
        for (name, addr) in function_overrides().into_iter().chain(data_overrides()) {
            assert!(!addr.is_null(), "{name}");
        }
    }
}

#[cfg(test)]
mod foreign_allocation_tests {
    //! The allocating libc calls `system_paths.cpp` refuses rather than let the
    //! host libc hand the engine a block its own allocator cannot free. Every
    //! call goes through the address the override table registers -- the one
    //! the engine's GOT receives -- so a table that loses an entry fails here
    //! instead of quietly sending the engine back to libc.so.

    use super::*;

    fn registered(name: &str) -> *mut c_void {
        let map: std::collections::BTreeMap<&str, *mut c_void> =
            function_overrides().into_iter().collect();
        *map.get(name).unwrap_or_else(|| panic!("{name} has no override"))
    }

    /// The errno as the engine reads it: through the `__errno` it was given,
    /// which on FreeBSD translates the host's numbering to Linux's.
    fn engine_errno() -> c_int {
        let f: unsafe extern "C" fn() -> *mut c_int =
            unsafe { std::mem::transmute(registered("__errno")) };
        // SAFETY: returns this thread's errno slot.
        unsafe { *f() }
    }

    fn clear_errno() {
        let f: unsafe extern "C" fn() -> *mut c_int =
            unsafe { std::mem::transmute(registered("__errno")) };
        // SAFETY: this thread's errno slot.
        unsafe { *f() = 0 };
    }

    const LX_ENOMEM: c_int = 12;
    const LX_ERANGE: c_int = 34;

    /// The point of the change, read from the table `symtab::build` consults:
    /// none of the three resolves to the host any more.
    #[test]
    fn allocating_imports_resolve_to_intoxicated_not_the_host() {
        let table = crate::symtab::build(true);
        let libc = &table.libraries["libc.so"];
        for name in ["getcwd", "vasprintf", "realpath"] {
            let e = libc
                .iter()
                .find(|e| e.symbol == name)
                .unwrap_or_else(|| panic!("{name} is not an import the table resolves"));
            assert_eq!(e.source, crate::symtab::Source::Intoxicated, "{name}");
            assert_eq!(e.address, registered(name), "{name}");
        }
    }

    #[test]
    fn getcwd_without_a_buffer_is_refused_with_enomem() {
        type Getcwd = unsafe extern "C" fn(*mut c_char, usize) -> *mut c_char;
        // SAFETY: the registered address is `s_getcwd`, which has this type.
        let getcwd: Getcwd = unsafe { std::mem::transmute(registered("getcwd")) };
        for size in [0usize, 1, 4096] {
            clear_errno();
            // SAFETY: a null buffer is the case under test; nothing is written.
            assert!(unsafe { getcwd(std::ptr::null_mut(), size) }.is_null(), "size {size}");
            assert_eq!(engine_errno(), LX_ENOMEM, "size {size}");
        }
    }

    #[test]
    fn getcwd_with_a_buffer_is_forwarded_with_its_own_errors() {
        type Getcwd = unsafe extern "C" fn(*mut c_char, usize) -> *mut c_char;
        // SAFETY: as above.
        let getcwd: Getcwd = unsafe { std::mem::transmute(registered("getcwd")) };
        let mut buf = [0 as c_char; 4096];
        // SAFETY: `buf` is live and its length is passed.
        let got = unsafe { getcwd(buf.as_mut_ptr(), buf.len()) };
        assert_eq!(got, buf.as_mut_ptr());
        // SAFETY: getcwd succeeded, so `buf` holds a terminated string.
        let path = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap().to_owned();
        assert_eq!(std::path::Path::new(&path), std::env::current_dir().unwrap());

        let mut tiny = [0 as c_char; 1];
        clear_errno();
        // SAFETY: `tiny` is live and its length is passed.
        assert!(unsafe { getcwd(tiny.as_mut_ptr(), tiny.len()) }.is_null());
        assert_eq!(engine_errno(), LX_ERANGE);
    }

    #[test]
    fn vasprintf_is_refused_and_leaves_nothing_to_free() {
        // `va_list` is a pointer to the register save area on x86-64, and the
        // refusal never reads it, so null stands in for an argument list here.
        type Vasprintf = unsafe extern "C" fn(*mut *mut c_char, *const c_char, *mut c_void) -> c_int;
        // SAFETY: the registered address is `s_vasprintf`; see above for `ap`.
        let vasprintf: Vasprintf = unsafe { std::mem::transmute(registered("vasprintf")) };
        for _ in 0..3 {
            let mut out = 0xdead as *mut c_char;
            clear_errno();
            // SAFETY: `out` is a live slot; the format is a terminated literal.
            let r = unsafe { vasprintf(&mut out, c"%s=%d".as_ptr(), std::ptr::null_mut()) };
            assert_eq!(r, -1);
            assert_eq!(engine_errno(), LX_ENOMEM);
            assert!(out.is_null(), "the out-parameter must not be left for a caller to free");
        }
        clear_errno();
        // SAFETY: a null out-parameter is refused without being written.
        assert_eq!(unsafe { vasprintf(std::ptr::null_mut(), c"x".as_ptr(), std::ptr::null_mut()) }, -1);
        assert_eq!(engine_errno(), LX_ENOMEM);
    }
}
