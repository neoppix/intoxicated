//! Counters other modules bump on the graphics path.
//!
//! `QUEUE_PRESENT` is the one read on the live path: the looper's health
//! monitor, frame pacing and the devctl `info` line all read it. The ETC
//! counters are kept beside it because `vulkan_etc`'s tests read them.
//!
//! This module used to wrap the host's EGL and GLES entry points with counting
//! forwarders behind a switch; the switch and the wrappers have been removed.

use std::sync::atomic::AtomicU64;

/// `eglCreateWindowSurface` calls, bumped by `native_window.rs`'s own wrapper.
pub static CREATE_WINDOW_SURFACE: AtomicU64 = AtomicU64::new(0);
/// `vkQueuePresentKHR` calls. Without it a Vulkan session has no frame count.
pub static QUEUE_PRESENT: AtomicU64 = AtomicU64::new(0);
/// ETC2/EAC images served on a driver without it; see `vulkan_etc`.
pub static ETC_IMAGES: AtomicU64 = AtomicU64::new(0);
/// ETC2/EAC upload regions decoded at submit.
pub static ETC_REGIONS: AtomicU64 = AtomicU64::new(0);
/// ETC2/EAC requests not served. Non-zero after joining a game is the reading
/// to report.
pub static ETC_UNHANDLED: AtomicU64 = AtomicU64::new(0);
