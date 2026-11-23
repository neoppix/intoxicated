//! The symbol table the guest linker is given: every name the engine and its
//! sibling libraries import, each answered by Intoxicated's own code, by a host
//! library, or by a stub, and grouped under the Android soname the engine
//! asked for.
//!
//! # Classification
//!
//! Each name of the stub list (`stub_table`) is classified first: Khronos EGL
//! (`egl` and then an uppercase letter), Khronos GLES (`gl` and an uppercase
//! letter, so `glob` is not GL), one of the Android libraries by prefix
//! ([`android_soname`]), or generic (libc, libm, libz and the C++ runtime).
//!
//! # Resolution
//!
//! 1. A name the merged override map has is Intoxicated's, under its
//!    classified soname (generic names under `libc.so`).
//! 2. An Android name without an override is a stub; the host is never asked.
//! 3. A Khronos name comes from the first host library that defines it,
//!    registered under the Khronos soname whichever library answered.
//! 4. A generic name comes from the first host library that defines it, under
//!    that library's Android soname; then, unless it is on the ABI-unsafe
//!    denylist, from `libc.so.7` and `libsys.so.7`; else it is a stub.
//!
//! A host library counts as defining a name only when `dladdr` puts the
//! address in a file named like it: a lookup through a handle also searches
//! that library's dependencies, and libm would otherwise answer for libc.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};

/// The AAudio library's soname; always registered.
pub const AAUDIO_SONAME: &str = "libaaudio.so";
/// The OpenSL ES library's soname; always registered (see [`build`]).
pub const OPENSLES_SONAME: &str = "libOpenSLES.so";
/// Sonames the engine needs (`DT_NEEDED`) that contribute no imports and are
/// registered with no entries.
pub const EMPTY_LIBRARIES: [&str; 1] = ["libOpenMAXAL.so"];
/// The engine's `DT_NEEDED` (measured with `readelf -d` on 2.748.1527).
/// Every one must be a registered library except `libdl.so`, which the guest
/// linker provides itself.
pub const ENGINE_NEEDED: [&str; 10] = [
    "libandroid.so",
    "liblog.so",
    "libOpenMAXAL.so",
    "libmediandk.so",
    "libGLESv2.so",
    "libEGL.so",
    "libOpenSLES.so",
    "libdl.so",
    "libm.so",
    "libc.so",
];

/// Where an entry's address came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Intoxicated's own code (an override).
    Intoxicated,
    /// A host library.
    Host,
    /// A stub from `stub_table`.
    Stub,
}

impl Source {
    /// The word the loader prints for this source.
    pub fn label(self) -> &'static str {
        match self {
            Source::Intoxicated => "intoxicated",
            Source::Host => "host",
            Source::Stub => "stub",
        }
    }
}

/// One registered symbol.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub symbol: &'static str,
    pub address: *mut c_void,
    pub source: Source,
}

/// How many entries of each source a library has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub intoxicated: usize,
    pub host: usize,
    pub stub: usize,
}

impl Stats {
    pub fn record(&mut self, source: Source) {
        match source {
            Source::Intoxicated => self.intoxicated += 1,
            Source::Host => self.host += 1,
            Source::Stub => self.stub += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.intoxicated + self.host + self.stub
    }
}

/// The table: per soname, its entries in registration order and its counts.
#[derive(Debug, Default)]
pub struct SymbolTable {
    pub libraries: BTreeMap<&'static str, Vec<Entry>>,
    pub stats: BTreeMap<&'static str, Stats>,
    /// Host sonames that could not be opened; their names fell to stubs.
    pub missing_host_libs: Vec<&'static str>,
}

impl SymbolTable {
    /// The counts over every library.
    pub fn totals(&self) -> Stats {
        let mut t = Stats::default();
        for s in self.stats.values() {
            t.intoxicated += s.intoxicated;
            t.host += s.host;
            t.stub += s.stub;
        }
        t
    }

    fn add(&mut self, soname: &'static str, symbol: &'static str, address: *mut c_void, source: Source) {
        self.libraries.entry(soname).or_default().push(Entry { symbol, address, source });
        self.stats.entry(soname).or_default().record(source);
    }

    fn ensure(&mut self, soname: &'static str) {
        self.libraries.entry(soname).or_default();
        self.stats.entry(soname).or_default();
    }

    /// The entry for `symbol` under `soname`.
    pub fn find(&self, soname: &str, symbol: &str) -> Option<&Entry> {
        self.libraries.get(soname)?.iter().find(|e| e.symbol == symbol)
    }

    /// The first line the loader prints.
    pub fn summary(&self) -> String {
        let t = self.totals();
        format!(
            "symbol table: {} intoxicated, {} host, {} stub, {} total across {} libraries",
            t.intoxicated,
            t.host,
            t.stub,
            t.total(),
            self.libraries.len()
        )
    }
}

/// How a stub-list name is classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Khronos, under this soname.
    Khronos(&'static str),
    /// An Android NDK library, under this soname.
    Android(&'static str),
    /// libc, libm, libz, the C++ runtime.
    Generic,
}

/// `prefix` followed directly by an ASCII uppercase letter.
fn khronos_name(symbol: &str, prefix: &str) -> bool {
    symbol.strip_prefix(prefix).and_then(|rest| rest.bytes().next()).is_some_and(|b| b.is_ascii_uppercase())
}

/// The Android library a name belongs to, by prefix.
pub fn android_soname(symbol: &str) -> Option<&'static str> {
    const TABLE: &[(&[&str], &str)] = &[
        (&["AMedia", "AMEDIA", "AImage", "AIMAGE"], "libmediandk.so"),
        (&["AndroidBitmap"], "libjnigraphics.so"),
        (&["__android_log", "android_set_abort_message", "android_get_device_api_level"], "liblog.so"),
        (
            &[
                "ANative",
                "AAsset",
                "AInput",
                "AKey",
                "AMotion",
                "ALooper",
                "ASensor",
                "AChoreographer",
                "AConfiguration",
                "ATrace",
                "AHardwareBuffer",
                "ASharedMemory",
                "APerformanceHint",
                "AObb",
                "AStorageManager",
                "ASurface",
                "AFont",
                "ASystemFont",
            ],
            "libandroid.so",
        ),
        (&["SL_IID_", "slCreateEngine"], OPENSLES_SONAME),
    ];
    TABLE
        .iter()
        .find(|(prefixes, _)| prefixes.iter().any(|p| symbol.starts_with(p)))
        .map(|(_, soname)| *soname)
}

/// The class of a stub-list name, in the order the rules apply.
pub fn classify(symbol: &str) -> Class {
    if khronos_name(symbol, "egl") {
        Class::Khronos("libEGL.so")
    } else if khronos_name(symbol, "gl") {
        Class::Khronos("libGLESv2.so")
    } else if let Some(soname) = android_soname(symbol) {
        Class::Android(soname)
    } else {
        Class::Generic
    }
}

/// Generic names never answered by the `libc.so.7`/`libsys.so.7`
/// fall-through: FreeBSD's mutex and rwlock objects have another size and
/// layout than bionic's, and its `syscall(2)` numbers are not Linux's. Each
/// has a translated override on FreeBSD; without one it is a stub, never the
/// host's version.
pub fn abi_unsafe_generic(symbol: &str) -> bool {
    matches!(
        symbol,
        "syscall"
            | "pthread_mutex_init"
            | "pthread_mutex_lock"
            | "pthread_mutex_unlock"
            | "pthread_mutex_trylock"
            | "pthread_mutex_timedlock"
            | "pthread_mutex_destroy"
            | "pthread_rwlock_init"
            | "pthread_rwlock_destroy"
            | "pthread_rwlock_rdlock"
            | "pthread_rwlock_tryrdlock"
            | "pthread_rwlock_wrlock"
            | "pthread_rwlock_trywrlock"
            | "pthread_rwlock_unlock"
    )
}

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
}

#[repr(C)]
struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

/// An opened host library standing in for an Android soname.
struct HostLib {
    handle: *mut c_void,
    /// The host soname up to and including `.so`: `libm.so` for `libm.so.5`.
    stem: &'static str,
    android: &'static str,
}

impl HostLib {
    fn open(host: &'static str, android: &'static str) -> Option<HostLib> {
        let c = CString::new(host).ok()?;
        // SAFETY: a NUL-terminated soname; the handle is kept for the process.
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        if handle.is_null() {
            return None;
        }
        let stem = host.find(".so").map_or(host, |i| &host[..i + 3]);
        Some(HostLib { handle, stem, android })
    }

    /// `name`'s address if this library itself defines it.
    fn lookup(&self, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        // SAFETY: a live handle and a NUL-terminated name.
        let addr = unsafe { dlsym(self.handle, c.as_ptr()) };
        if addr.is_null() {
            return None;
        }
        let mut info = DlInfo {
            dli_fname: std::ptr::null(),
            dli_fbase: std::ptr::null_mut(),
            dli_sname: std::ptr::null(),
            dli_saddr: std::ptr::null_mut(),
        };
        // SAFETY: `info` is a valid out-parameter.
        if unsafe { dladdr(addr, &mut info) } == 0 || info.dli_fname.is_null() {
            return None;
        }
        // SAFETY: dladdr hands back a NUL-terminated path owned by rtld.
        let file = unsafe { CStr::from_ptr(info.dli_fname) }.to_string_lossy();
        let base = file.rsplit('/').next().unwrap_or(&file);
        base.starts_with(self.stem).then_some(addr)
    }
}

/// The six host libraries, in lookup order, each with the Android soname it
/// stands in for. FreeBSD spells the libc-family sonames its own way, and the
/// Itanium C++ personality lives in libcxxrt.
const HOST_LIBRARIES: [(&str, &str); 6] = [
    ("libm.so.5", "libm.so"),
    ("libz.so.6", "libz.so"),
    ("libGLESv2.so.2", "libGLESv2.so"),
    ("libEGL.so.1", "libEGL.so"),
    ("libcxxrt.so.1", "libc.so"),
    ("libgcc_s.so.1", "libc.so"),
];

/// Every override, later duplicates replacing earlier ones: the function
/// overrides, the data overrides, then the Android ones.
pub fn override_map() -> BTreeMap<&'static str, *mut c_void> {
    let mut map = BTreeMap::new();
    for (name, addr) in crate::bionic::function_overrides()
        .into_iter()
        .chain(crate::bionic::data_overrides())
        .chain(crate::android::overrides())
    {
        map.insert(name, addr);
    }
    map
}

/// Build the table. `host_libc` is the `--host-libc` switch, accepted and
/// ignored: on FreeBSD the host libc is always consulted, after the six
/// libraries and minus the denylist.
pub fn build(host_libc: bool) -> SymbolTable {
    let _ = host_libc;
    let overrides = override_map();
    let mut table = SymbolTable::default();

    let mut hosts = Vec::new();
    for (host, android) in HOST_LIBRARIES {
        match HostLib::open(host, android) {
            Some(lib) => hosts.push(lib),
            None => table.missing_host_libs.push(host),
        }
    }
    // Always, and never reported when absent: libsys.so.7 only exists from
    // FreeBSD 15, where the system-call wrappers (mmap, munmap, mprotect,
    // getpid and the rest) moved out of libc.so.7.
    let libc = HostLib::open("libc.so.7", "libc.so");
    let libsys = HostLib::open("libsys.so.7", "libc.so");

    for (symbol, stub) in crate::stub_table::entries() {
        let class = classify(symbol);
        let own_soname = match class {
            Class::Khronos(s) | Class::Android(s) => s,
            Class::Generic => "libc.so",
        };
        if let Some(&addr) = overrides.get(symbol) {
            table.add(own_soname, symbol, addr, Source::Intoxicated);
            continue;
        }
        match class {
            Class::Android(soname) => table.add(soname, symbol, stub, Source::Stub),
            Class::Khronos(soname) => match hosts.iter().find_map(|h| h.lookup(symbol)) {
                Some(addr) => table.add(soname, symbol, addr, Source::Host),
                None => table.add(soname, symbol, stub, Source::Stub),
            },
            Class::Generic => {
                if let Some((addr, soname)) = hosts.iter().find_map(|h| h.lookup(symbol).map(|a| (a, h.android))) {
                    table.add(soname, symbol, addr, Source::Host);
                } else if abi_unsafe_generic(symbol) {
                    table.add("libc.so", symbol, stub, Source::Stub);
                } else if let Some(addr) = libc
                    .as_ref()
                    .and_then(|l| l.lookup(symbol))
                    .or_else(|| libsys.as_ref().and_then(|l| l.lookup(symbol)))
                {
                    table.add("libc.so", symbol, addr, Source::Host);
                } else {
                    table.add("libc.so", symbol, stub, Source::Stub);
                }
            }
        }
    }

    for soname in EMPTY_LIBRARIES {
        table.ensure(soname);
    }

    // FMOD's Android file layer `dlsym`s `AAsset_read` and `AAsset_seek` after
    // `dlopen("libandroid.so")`; the engine itself does not import them.
    for (symbol, address) in crate::android::assets::overrides() {
        if table.find("libandroid.so", symbol).is_none() {
            table.add("libandroid.so", symbol, address, Source::Intoxicated);
        }
    }

    register_vulkan(&mut table);

    // Always registered. Whether FMOD opens it is decided by
    // `org.fmod.FMOD.supportsAAudio()` (native/fmod_java.cpp).
    table.ensure(AAUDIO_SONAME);
    for (symbol, address) in crate::bionic::aaudio_overrides() {
        table.add(AAUDIO_SONAME, symbol, address, Source::Intoxicated);
    }

    // The engine needs libOpenSLES.so (DT_NEEDED) and no longer imports any
    // of its names, so the pass above registers nothing for it. Its
    // link-time symbols (native/opensl_refusal.cpp) go in as they are.
    table.ensure(OPENSLES_SONAME);
    for (symbol, address) in crate::bionic::opensles_overrides() {
        if table.find(OPENSLES_SONAME, symbol).is_none() {
            table.add(OPENSLES_SONAME, symbol, address, Source::Intoxicated);
        }
    }

    table
}

/// The Vulkan entry point, under both sonames, when the host loader has one;
/// otherwise neither soname exists and `libvulkan.so.1` is reported missing.
/// Kept apart because `android/vulkan.rs` owns what is registered.
fn register_vulkan(table: &mut SymbolTable) {
    match crate::android::vk_layer::get_instance_proc_addr_symbol() {
        Some(addr) => {
            for soname in crate::android::vk_layer::LIBRARY_NAMES {
                table.add(soname, "vkGetInstanceProcAddr", addr, Source::Intoxicated);
            }
        }
        None => table.missing_host_libs.push("libvulkan.so.1"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_follows_the_prefix_rules() {
        assert_eq!(classify("glDrawArrays"), Class::Khronos("libGLESv2.so"));
        assert_eq!(classify("eglGetDisplay"), Class::Khronos("libEGL.so"));
        for generic in ["glob", "globfree", "memcpy", "pthread_create", "eglue", "gl"] {
            assert_eq!(classify(generic), Class::Generic, "{generic}");
        }
        assert_eq!(classify("ANativeWindow_lock"), Class::Android("libandroid.so"));
        assert_eq!(classify("__android_log_print"), Class::Android("liblog.so"));
        assert_eq!(classify("AMediaCodec_start"), Class::Android("libmediandk.so"));
        assert_eq!(classify("AndroidBitmap_getInfo"), Class::Android("libjnigraphics.so"));
        assert_eq!(classify("SL_IID_ENGINE"), Class::Android("libOpenSLES.so"));
    }

    #[test]
    fn the_denylist_is_the_fourteen_layout_and_numbering_names() {
        let n = ["syscall", "pthread_mutex_lock", "pthread_rwlock_trywrlock", "pthread_mutex_timedlock"]
            .iter()
            .filter(|s| abi_unsafe_generic(s))
            .count();
        assert_eq!(n, 4);
        assert!(!abi_unsafe_generic("pthread_cond_wait"));
        assert!(!abi_unsafe_generic("memset"));
    }

    #[test]
    fn resolution_puts_each_name_where_the_rules_say() {
        let t = build(true);
        let src = |soname: &str, symbol: &str| t.find(soname, symbol).map(|e| e.source);
        assert_eq!(src("libGLESv2.so", "glClearStencil"), Some(Source::Host));
        assert_eq!(src("libEGL.so", "eglCreateWindowSurface"), Some(Source::Intoxicated));
        assert_eq!(src("libc.so", "memset"), Some(Source::Host));
        assert_eq!(src("libc.so", "pthread_mutex_lock"), Some(Source::Intoxicated));
        for name in ["pthread_once", "pthread_key_create", "pthread_key_delete", "pthread_getspecific"] {
            assert_eq!(src("libc.so", name), Some(Source::Intoxicated), "{name}");
        }
        assert_eq!(src("libmediandk.so", "AMediaCodec_start"), Some(Source::Stub));
        assert_eq!(src("libmediandk.so", "AImageReader_acquireNextImage"), Some(Source::Intoxicated));
        // Data symbols bionic exports and FreeBSD does not: real objects now.
        for name in ["stdin", "stdout", "stderr", "daylight", "timezone"] {
            assert_eq!(src("libc.so", name), Some(Source::Intoxicated), "{name}");
        }
        // libm's own names come from libm, under libm.so.
        assert_eq!(src("libm.so", "cbrt"), Some(Source::Host));
    }

    #[test]
    fn every_needed_library_is_registered() {
        let t = build(true);
        for soname in ENGINE_NEEDED {
            if soname == "libdl.so" {
                continue;
            }
            assert!(t.libraries.contains_key(soname), "{soname} is not registered");
        }
        assert!(t.libraries.contains_key(AAUDIO_SONAME));
        let sl = &t.libraries[OPENSLES_SONAME];
        assert!(sl.iter().any(|e| e.symbol == "slCreateEngine"), "libOpenSLES.so without slCreateEngine");
        assert!(t.libraries["libandroid.so"].iter().any(|e| e.symbol == "AAsset_seek"));
    }

    #[test]
    fn counts_and_summary_agree() {
        let t = build(true);
        let total: usize = t.libraries.values().map(Vec::len).sum();
        assert_eq!(t.totals().total(), total);
        let line = t.summary();
        assert!(line.starts_with("symbol table: "), "{line}");
        assert!(line.contains(" intoxicated, "), "{line}");
    }

    #[test]
    fn section_bounds_are_left_for_the_linker() {
        let t = build(true);
        for e in t.libraries.values().flatten() {
            assert!(!e.symbol.starts_with("__start_") && !e.symbol.starts_with("__stop_"), "{}", e.symbol);
        }
    }

    #[test]
    fn a_host_library_answers_only_for_its_own_names() {
        let m = HostLib::open("libm.so.5", "libm.so").expect("libm.so.5");
        assert!(m.lookup("cbrt").is_some());
        // memset reaches libc through libm's dependencies; it must not count.
        assert!(m.lookup("memset").is_none());
    }
}
