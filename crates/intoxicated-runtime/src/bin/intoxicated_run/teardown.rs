//! The end of a run: the reports, then `_exit`.
//!
//! Once `libroblox.so` is loaded the process never returns from `main`. The
//! engine's `atexit` handlers and `DT_FINI_ARRAY` destructors expect a live
//! Android process and crash while it is torn down, so every exit after the
//! load, the normal one and the failures alike, goes through [`leave`]. Rust
//! destructors of `main`'s locals do not run on that path; nothing in them
//! has to (the profile lock belongs to the open file, and the kernel drops
//! it on any exit).

use std::io::Write;
use std::path::Path;

/// What the end-of-run reports need to know.
pub struct Ending<'a> {
    /// `--dump-classes`, when the JNI block ran.
    pub dump_classes: Option<&'a str>,
    /// `<data root>`, whose `files/` holds the engine's storage.
    pub data_root: &'a Path,
}

/// The reports at the end of every run that loaded the engine, then exit 0.
pub fn finish(ending: Ending<'_>) -> ! {
    if let Some(path) = ending.dump_classes {
        match intoxicated_linker_sys::jni::dump_classes(path) {
            Ok(()) => println!("  Java classes Roblox reached for -> {path}"),
            Err(e) => eprintln!("  class dump failed: {e}"),
        }
    }
    intoxicated_runtime::storage::report(&ending.data_root.join("files"));
    intoxicated_runtime::stubs::report();
    intoxicated_runtime::unimplemented::report();
    leave(0)
}

/// Flush what was printed and end the process with `code`, without running
/// `atexit` handlers or destructors.
pub fn leave(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` takes a status and does not return.
    unsafe { libc::_exit(code) }
}
