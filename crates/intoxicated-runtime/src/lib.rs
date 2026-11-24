//! Intoxicated's runtime: everything `intoxicated-run` needs to run Roblox's
//! Android x86-64 engine as a native FreeBSD process.
//!
//! The engine is loaded by the bionic linker in `intoxicated-linker-sys` and
//! linked against the tables built here: [`bionic`] answers its libc imports
//! with FreeBSD's own functions, translated where the two disagree, and
//! [`android`] answers the NDK (assets, the window, the looper, the
//! configuration) over the APK, X11, Vulkan and kqueue. The rest is the
//! client around it: the profile and its session, flags, links, the web view
//! hand-off and the development control socket. `src/bin/intoxicated_run/` puts it
//! together.

/// The title of a window this process opens, naming the build and the
/// `backend` drawing into it.
pub fn window_title(backend: &str) -> String {
    format!("Intoxicated {} ({backend})", env!("CARGO_PKG_VERSION"))
}

pub mod account;
pub mod android;
pub mod bionic;
pub mod bus;
pub mod cookie_jar;
pub mod deeplink;
pub mod desktop;
pub mod devctl;
pub mod effects_hotkey;
pub mod engine;
pub mod engine_log;
pub mod flag_file;
pub mod flag_layers;
pub mod graphics;
pub mod linking;
pub mod macros;
pub mod net;
pub mod power;
pub mod profile;
pub mod profile_lock;
pub mod roblox_settings;
pub mod secrets;
pub mod secure_values;
pub mod storage;
pub mod webview;

pub mod stub_table;
pub use stub_table as stubs;
pub mod resolve;
pub use resolve as symtab;
pub mod syscall_sites;
pub mod gap_log;
pub use gap_log as unimplemented;
