//! Rust bindings to Intoxicated's native layer.
//!
//! `build.rs` builds `native/` (the AOSP bionic linker as retargeted by
//! mcpelauncher-linker, libjnivm, and the Android framework shims written for
//! this project) and links it into whatever depends on this crate. This file
//! is the linker half of the surface: create virtual libraries, load the
//! engine against them, and look symbols up in the result. [`jni`] owns the
//! process JavaVM and [`game_activity`] every call that drives the engine
//! through it.
//!
//! The order the runtime follows, and the C side assumes:
//! [`init`], then [`register`] once per virtual library, [`set_library_path`],
//! [`dlopen`] of the engine with [`RTLD_NOW`], then [`Library::code_region`]
//! and [`Library::base`] for the text protection and the syscall trap, and
//! only then the JNI and GameActivity bring-up.
//!
//! Nothing here closes or unloads. A handle, and every address looked up
//! through it, stays valid until the process exits, which is the only
//! lifetime the engine has ever had.

use std::ffi::{c_char, c_int, c_void, CStr, CString, NulError};
use std::fmt;
use std::ptr::NonNull;

mod call;
mod ffi;
pub mod game_activity;
pub mod jni;

/// `dlopen` flag for eager binding. The only flag the runtime passes: the
/// engine's text is made read-only and executable straight after the load,
/// so nothing may be left to bind lazily into it.
pub const RTLD_NOW: c_int = 2;

/// A library the bionic linker knows about: a virtual one from [`register`]
/// or a real ELF object from [`dlopen`].
///
/// This is bionic's own handle, never null, and never dereferenced on the
/// Rust side. Two handles compare equal when they name the same library;
/// opening an soname that is already loaded hands back the handle it already
/// has, which is how the runtime gets at `libroblox.so` again from places the
/// original handle was never passed to.
///
/// `Send` because the runtime keeps one in a `static Mutex`. Every lookup
/// through it takes bionic's loader lock on the C side, so which thread asks
/// does not matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Library(NonNull<c_void>);

// SAFETY: the handle is an opaque token into bionic's solist. Rust never reads
// through it, and the C functions that take it lock the loader themselves.
unsafe impl Send for Library {}

impl Library {
    fn from_raw(handle: *mut c_void) -> Result<Self, Error> {
        NonNull::new(handle).map(Library).ok_or_else(|| Error::Linker(linker_message()))
    }

    /// The address `name` resolves to through this library, or `None`.
    ///
    /// The shim consults its system-symbol override table first: for the libc
    /// names in that table it returns Intoxicated's own implementation whatever
    /// handle is passed, so the engine's run-time lookups of raw libc functions
    /// land on the same hooked versions its import table got. Anything else is
    /// bionic's `dlsym`, which searches the library and then what it depends
    /// on. A name with an interior NUL names nothing and is not looked up.
    pub fn symbol(self, name: &str) -> Option<*mut c_void> {
        let name = CString::new(name).ok()?;
        // SAFETY: the handle came from bionic and is never freed; `name`
        // outlives the call.
        let address = unsafe { ffi::intoxicated_linker_dlsym(self.0.as_ptr(), name.as_ptr()) };
        (!address.is_null()).then_some(address)
    }

    /// The load bias the library was mapped at.
    pub fn base(self) -> usize {
        // SAFETY: the handle came from bionic and is never freed.
        unsafe { ffi::intoxicated_linker_get_library_base(self.0.as_ptr()) }
    }

    /// Start and length of the library's executable segment: for the last
    /// `PT_LOAD` with `PF_X`, the load address plus `p_vaddr` and `p_memsz`.
    /// The start is not page aligned. `(0, 0)` when there is no such segment,
    /// which is the case for every virtual library.
    pub fn code_region(self) -> (usize, usize) {
        let (mut start, mut len) = (0usize, 0usize);
        // SAFETY: the handle came from bionic and is never freed; the two
        // out-pointers are locals.
        unsafe { ffi::intoxicated_linker_get_library_code_region(self.0.as_ptr(), &mut start, &mut len) };
        (start, len)
    }
}

/// Why a linker call failed.
#[derive(Debug)]
pub enum Error {
    /// The linker refused, in its own words (bionic's `dlerror()`).
    Linker(String),
    /// A name had an interior NUL, so it could not be handed to C at all.
    InteriorNul(NulError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Linker(message) => f.write_str(message),
            Error::InteriorNul(e) => write!(f, "invalid name: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Linker(_) => None,
            Error::InteriorNul(e) => Some(e),
        }
    }
}

impl From<NulError> for Error {
    fn from(e: NulError) -> Self {
        Error::InteriorNul(e)
    }
}

/// bionic's last error, copied out before anything else can replace it.
fn linker_message() -> String {
    // SAFETY: plain call; the result is null or a NUL-terminated string the
    // linker owns until its next error on this thread.
    let message = unsafe { ffi::intoxicated_linker_dlerror() };
    if message.is_null() {
        return "unknown linker error".to_string();
    }
    // SAFETY: non-null, NUL-terminated, and read before any other linker call.
    unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned()
}

/// Sets up bionic's library list and registers its built-in `libdl.so`.
///
/// Has to be the first linker call and has to happen exactly once. Nothing
/// guards either: a second call replaces bionic's list with a fresh one and
/// forgets every library already in it.
pub fn init() {
    // SAFETY: no arguments; see above for the once-only contract.
    unsafe { ffi::intoxicated_linker_init() }
}

/// Creates a virtual library: an soname whose whole content is `symbols`.
///
/// A `DT_NEEDED` entry naming `soname` in anything loaded later resolves
/// against this table. The names and addresses are copied during the call.
///
/// The soname is not. bionic stores the pointer it is given as the library's
/// soname and reads it again on every later lookup by name, so the string has
/// to outlive the process's use of the linker. It is leaked on purpose, once
/// per virtual library (about a dozen a run); freeing it would turn the next
/// soname comparison into a read of freed memory.
pub fn register(soname: &str, symbols: &[(String, *mut c_void)]) -> Result<Library, Error> {
    let soname = CString::new(soname)?;
    let names = symbols.iter().map(|(name, _)| CString::new(name.as_str())).collect::<Result<Vec<_>, _>>()?;
    let name_ptrs: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
    let addresses: Vec<*mut c_void> = symbols.iter().map(|&(_, address)| address).collect();

    // Leaked only once every name has been checked, so a refusal leaks nothing.
    let soname: &'static CStr = Box::leak(soname.into_boxed_c_str());
    // SAFETY: the two arrays are parallel, `symbols.len()` long, and outlive
    // the call; the soname is 'static, as bionic requires.
    let handle = unsafe {
        ffi::intoxicated_linker_load_library(soname.as_ptr(), name_ptrs.as_ptr(), addresses.as_ptr(), symbols.len())
    };
    Library::from_raw(handle)
}

/// Sets the directory list the linker searches for real objects. Copied.
pub fn set_library_path(path: &str) -> Result<(), Error> {
    let path = CString::new(path)?;
    // SAFETY: `path` outlives the call and the linker keeps its own copy.
    unsafe { ffi::intoxicated_linker_update_ld_library_path(path.as_ptr()) };
    Ok(())
}

/// Maps, relocates and runs the constructors of an ELF object, resolving its
/// imports against the registered virtual libraries.
///
/// An soname that is already loaded returns its existing handle. On failure
/// the error is the linker's own message. `INTOXICATED_TRACE_DLOPEN` in the
/// environment makes the shim print each request and how long it took.
pub fn dlopen(name: &str, flags: c_int) -> Result<Library, Error> {
    let name = CString::new(name)?;
    // SAFETY: `name` outlives the call.
    let handle = unsafe { ffi::intoxicated_linker_dlopen(name.as_ptr(), flags) };
    Library::from_raw(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Symbols a virtual library can point at. Their addresses are all the
    // linker sees; nothing calls through them.
    static ALPHA: u64 = 0xa1fa;
    static BETA: u64 = 0xbe7a;

    fn address_of(value: &'static u64) -> *mut c_void {
        value as *const u64 as *mut c_void
    }

    fn assert_send<T: Send>() {}

    /// One function on purpose: [`init`] may run once per process and the
    /// linker's list is process-global, while tests run on parallel threads.
    #[test]
    fn handles_and_symbol_lookup_without_the_engine() {
        assert_send::<Library>();
        init();

        let a = register("libintoxicated-test-a.so", &[("intoxicated_test_alpha".to_string(), address_of(&ALPHA))])
            .expect("a virtual library registers");
        let b = register("libintoxicated-test-b.so", &[("intoxicated_test_beta".to_string(), address_of(&BETA))])
            .expect("a second virtual library registers");
        assert_ne!(a, b, "two sonames, two handles");

        // A registered name resolves to exactly the address it was given.
        assert_eq!(a.symbol("intoxicated_test_alpha"), Some(address_of(&ALPHA)));
        assert_eq!(b.symbol("intoxicated_test_beta"), Some(address_of(&BETA)));
        // Null from the linker is None, not a null pointer in a Some.
        assert_eq!(a.symbol("intoxicated_test_nowhere"), None);
        // An interior NUL is refused before C is reached, even when the part
        // before it would have matched.
        assert_eq!(a.symbol("intoxicated_test_alpha\0tail"), None);

        // The override table answers for any handle: the same shim comes back
        // through a library that defines nothing of the sort.
        let via_a = a.symbol("openat").expect("openat is in the system-symbol table");
        assert_eq!(b.symbol("openat"), Some(via_a), "the override ignores which handle asked");

        // Opening an soname that is already loaded returns the same handle,
        // which is what the clipboard relies on to find libroblox.so again.
        let again = dlopen("libintoxicated-test-a.so", RTLD_NOW).expect("a loaded soname opens");
        assert_eq!(again, a);

        // A library with no program headers has no code region.
        assert_eq!(a.code_region(), (0, 0));
        assert_eq!(a.base(), 0);

        // A failed open carries the linker's message, never an empty one.
        let missing = dlopen("libintoxicated-test-absent.so", RTLD_NOW).expect_err("nothing by that name");
        let text = missing.to_string();
        assert!(matches!(missing, Error::Linker(_)), "{missing:?}");
        assert!(text.contains("libintoxicated-test-absent.so"), "the linker names the library: {text}");

        // Interior NULs anywhere in a name are refused, not truncated.
        let bad_soname = register("libbad\0.so", &[]).expect_err("NUL in the soname");
        assert!(matches!(bad_soname, Error::InteriorNul(_)));
        assert!(bad_soname.to_string().starts_with("invalid name: "), "{bad_soname}");
        let bad_symbol = register("libintoxicated-test-c.so", &[("bad\0name".to_string(), address_of(&ALPHA))])
            .expect_err("NUL in a symbol name");
        assert!(matches!(bad_symbol, Error::InteriorNul(_)));
        // ... and the refused library was never created.
        assert!(dlopen("libintoxicated-test-c.so", RTLD_NOW).is_err());
        assert!(matches!(set_library_path("/a\0/b"), Err(Error::InteriorNul(_))));
        assert!(matches!(dlopen("lib\0.so", RTLD_NOW), Err(Error::InteriorNul(_))));
    }

    #[test]
    fn the_linker_error_prints_as_is() {
        assert_eq!(
            Error::Linker("dlopen failed: library \"x\" not found".into()).to_string(),
            "dlopen failed: library \"x\" not found"
        );
        let nul = CString::new("a\0b").unwrap_err();
        let e = Error::from(nul);
        assert!(std::error::Error::source(&e).is_some());
    }
}
