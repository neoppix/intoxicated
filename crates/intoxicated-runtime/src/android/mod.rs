//! The Android side of the engine's world, answered in this process.
//!
//! `libroblox.so` imports four groups of NDK functions from `libandroid.so`,
//! and every one is implemented:
//!
//! | Group | Imports | State |
//! |---|---|---|
//! | `AAsset*` | 6, and 2 more for `dlsym` | implemented, read from the APK; see [`assets`] |
//! | `ANativeWindow_*` | 9 | implemented over the X11 window; see [`native_window`] and [`x11_window`] |
//! | `ALooper_*` | 7, and `ALooper_wake` | implemented over kqueue; see [`looper`] |
//! | `AConfiguration_*` | 9 | implemented as a desktop window's configuration; see [`config`] |
//!
//! Around them sit the pieces an Android device would provide some other way:
//! the window's input ([`input`]), the Vulkan layer between the engine and the
//! host driver ([`vk_layer`]), the clipboard, and the `/system` tree the engine
//! looks for fonts in ([`system`]).

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

pub mod assets;
pub mod clipboard;
pub mod config;
pub mod depth_capture;
pub mod etc2;
pub mod frame_capture;
pub mod gl_probe;
pub use gl_probe as gl;
pub mod glcount;
pub mod image_reader;
pub mod input;
pub mod looper;
pub mod native_window;
pub mod physical_size;
pub mod present_stats;
pub mod ssr;
pub mod system;
pub mod vk_layer;
pub mod vulkan_etc;
pub mod x11_clipboard;
pub mod x11_window;

// ------------------------------------------------------------ trace switch

/// Whether Android API calls are logged. Off until `set_trace` turns it on.
static TRACE: AtomicBool = AtomicBool::new(false);

/// Turn the Android trace on or off for the whole process. `intoxicated-run`
/// calls this once, from `INTOXICATED_ANDROID_TRACE`.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

/// One trace line, `[android] <text>` on stderr, while the trace is on.
/// Formatting is skipped entirely while it is off.
pub(crate) fn trace(text: std::fmt::Arguments<'_>) {
    if TRACE.load(Ordering::Relaxed) {
        eprintln!("[android] {text}");
    }
}

// ------------------------------------------------------------ display window
//
// X11 (`x11_window.rs`) is the only display backend. These are the entry points
// the rest of the crate uses for the one window.

/// Open the host window.
pub fn open_window(width: u32, height: u32, title: &str) -> Result<&'static x11_window::HostWindow, String> {
    x11_window::open(width, height, title)
}

/// The canvas in pixels, and `(0, 0)` before there is a window.
///
/// For a caller that needs the window's size where a
/// scripted touch has no host event to read it off. Zero is what "there is no
/// window" honestly looks like: it is also a handle of 0, so nothing reaches
/// the engine on that path anyway and there is no wrong size to be believed.
pub fn canvas_size() -> (i32, i32) {
    x11_window::current().map(|w| w.geometry()).map(|(w, h, _)| (w, h)).unwrap_or((0, 0))
}

/// The window's input connection descriptor, for the looper to watch
/// alongside the engine's own descriptors; `None` before there is a window.
pub fn connection_fd() -> Option<c_int> {
    x11_window::current().map(|w| w.connection_fd())
}

/// Fullscreen on or off, from a scripted run. See
/// `x11_window::HostWindow::set_fullscreen`.
/// devctl's `fullscreen` and `windowed` verbs come through here.
pub fn backend_set_fullscreen(on: bool) {
    if let Some(w) = x11_window::current() {
        w.set_fullscreen(on);
    }
}

/// Whether the user has closed the engine's window.
///
/// `x11_window.rs` advertises `WM_DELETE_WINDOW` and records the window manager's
/// close request in its own `WINDOW_CLOSED` flag (see its `CLIENT_MESSAGE`
/// handler); that is what makes the close button end the process.
///
/// **This used to answer `false` unconditionally on X11.** That holds only
/// while a `--run` timer is set. The `intoxicated` launcher runs `--run 0` (no
/// timer, run until the window closes), so the close button did nothing and
/// the process stayed alive with no window -- the reported bug.
/// `INTOXICATED_NO_CLOSE_EXIT` still gates whether it ends the run.
pub fn window_closed() -> bool {
    x11_window::window_closed()
}

/// Drain and deliver whatever host input is queued.
pub fn pump_input_events(handle: i64) {
    x11_window::pump_input_events(handle)
}

/// Whether host input is already queued in this process, where the looper's
/// wait on the display connection cannot see it.
pub fn input_backlog() -> bool {
    x11_window::input_backlog()
}

/// Every NDK function this module answers, for the symbol table: assets,
/// configuration, looper, image reader, native window, in that order.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    let mut v = assets::overrides();
    v.extend(config::overrides());
    v.extend(looper::overrides());
    v.extend(image_reader::overrides());
    v.extend(native_window::overrides());
    v
}
