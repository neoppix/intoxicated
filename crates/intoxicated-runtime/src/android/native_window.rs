//! `ANativeWindow_*`, and the two EGL entry points that take one.
//!
//! There is exactly one window, the X11 one in [`super::x11_window`], so an
//! `ANativeWindow*` here is simply the address of that window object: every
//! non-null pointer the engine hands back is taken to mean it, and null means
//! no window. Nothing is reference counted, and no software buffer is ever
//! handed out.
//!
//! Who imports what (`docs/analysis/undefined-symbols.tsv`): `libroblox.so`
//! takes `fromSurface`, `acquire`, `release`, `getWidth` and `getHeight`, and
//! `eglCreateWindowSurface` and `eglSwapInterval`; the CameraX libraries add
//! `getFormat`, `setBuffersGeometry`, `lock` and `unlockAndPost`, and were
//! never seen loaded.

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use super::x11_window;

/// `WINDOW_FORMAT_RGBA_8888`, the format a new window reports.
pub const WINDOW_FORMAT_RGBA_8888: i32 = 1;

const EINVAL: i32 = 22;
const ENOSYS: i32 = 38;

/// The size and format the engine is told, shared by the `ANativeWindow_*`
/// answers, the pointer-lock centre, the keyboard report and the canvas-size
/// query. Written by window creation, `setBuffersGeometry` and a resize.
#[derive(Debug)]
pub struct SurfaceGeometry(Mutex<(i32, i32, i32)>);

impl SurfaceGeometry {
    pub fn new(width: i32, height: i32) -> Self {
        SurfaceGeometry(Mutex::new((width, height, WINDOW_FORMAT_RGBA_8888)))
    }

    /// `(width, height, format)`.
    pub fn get(&self) -> (i32, i32, i32) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `ANativeWindow_setBuffersGeometry`: each value above zero replaces the
    /// stored one, and zero or less keeps it. Android's "0 means the window's
    /// natural size" is not reproduced; only CameraX imports this.
    pub fn set_buffers(&self, width: i32, height: i32, format: i32) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if width > 0 {
            g.0 = width;
        }
        if height > 0 {
            g.1 = height;
        }
        if format > 0 {
            g.2 = format;
        }
    }

    /// A window-manager resize: `Some(format)` when the size really changed
    /// and the engine has to hear of it, `None` for a non-positive size or
    /// the size it already has (moves, and repeated configures in a drag).
    pub fn resize(&self, width: i32, height: i32) -> Option<i32> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if (g.0, g.1) == (width, height) {
            return None;
        }
        g.0 = width;
        g.1 = height;
        Some(g.2)
    }
}

/// The geometry behind an `ANativeWindow*`: the window's, for any non-null
/// pointer once a window exists.
fn geometry_of(w: *mut c_void) -> Option<&'static SurfaceGeometry> {
    if w.is_null() {
        return None;
    }
    x11_window::current().map(|win| win.surface_geometry())
}

fn width_of(g: Option<&SurfaceGeometry>) -> i32 {
    g.map_or(0, |g| g.get().0)
}

fn height_of(g: Option<&SurfaceGeometry>) -> i32 {
    g.map_or(0, |g| g.get().1)
}

fn format_of(g: Option<&SurfaceGeometry>) -> i32 {
    g.map_or(0, |g| g.get().2)
}

fn set_buffers_geometry_of(g: Option<&SurfaceGeometry>, width: i32, height: i32, format: i32) -> i32 {
    match g {
        Some(g) => {
            g.set_buffers(width, height, format);
            0
        }
        None => -EINVAL,
    }
}

/// The handle `fromSurface` answers: the window object, or null before one
/// exists. The arguments are not read; there is one window.
fn window_handle() -> *mut c_void {
    x11_window::current().map_or(std::ptr::null_mut(), |w| w as *const x11_window::HostWindow as *mut c_void)
}

extern "C" fn native_window_from_surface(_env: *mut c_void, _surface: *mut c_void) -> *mut c_void {
    let w = window_handle();
    crate::android::trace(format_args!("ANativeWindow_fromSurface -> {w:p}"));
    w
}

extern "C" fn native_window_acquire(_window: *mut c_void) {}

extern "C" fn native_window_release(_window: *mut c_void) {}

extern "C" fn native_window_get_width(window: *mut c_void) -> i32 {
    width_of(geometry_of(window))
}

extern "C" fn native_window_get_height(window: *mut c_void) -> i32 {
    height_of(geometry_of(window))
}

extern "C" fn native_window_get_format(window: *mut c_void) -> i32 {
    format_of(geometry_of(window))
}

extern "C" fn native_window_set_buffers_geometry(window: *mut c_void, width: i32, height: i32, format: i32) -> i32 {
    set_buffers_geometry_of(geometry_of(window), width, height, format)
}

extern "C" fn native_window_lock(_window: *mut c_void, _buffer: *mut c_void, _dirty: *mut c_void) -> i32 {
    -ENOSYS
}

extern "C" fn native_window_unlock_and_post(_window: *mut c_void) -> i32 {
    -ENOSYS
}

// ------------------------------------------------------------------ EGL
//
// Android's EGL takes the `ANativeWindow*`, X11's takes the window's XID, so
// `eglCreateWindowSurface` swaps one for the other and forwards to the host.
//
// **The host entry is looked up in the host's libEGL, by handle.** It used to
// be `dlsym(NULL, ...)`, which on FreeBSD searches only the object making the
// call and its own dependencies, not the process: it returned null, so the
// wrapper answered `EGL_NO_SURFACE` without the host EGL ever running, and the
// GLES fallback could not draw at all (the engine logged
// `eglCreateWindowSurface 3000`, EGL_SUCCESS, for a call that never happened).
// `dlopen("libEGL.so.1")` hands back the handle `symtab` already opened with
// `RTLD_GLOBAL`; `RTLD_DEFAULT` is the fallback for an EGL known by another
// soname. Neither can find these wrappers, which are not exported.

const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;
/// FreeBSD's `RTLD_DEFAULT`, `((void *)-2)`.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// The host EGL's `name`, or null.
fn host_egl_symbol(name: &CStr) -> *mut c_void {
    static LIB: OnceLock<usize> = OnceLock::new();
    // SAFETY: a literal soname, never closed.
    let lib = *LIB.get_or_init(|| unsafe { dlopen(c"libEGL.so.1".as_ptr(), RTLD_NOW | RTLD_GLOBAL) } as usize);
    if lib != 0 {
        // SAFETY: an open handle and a NUL-terminated name.
        let p = unsafe { dlsym(lib as *mut c_void, name.as_ptr()) };
        if !p.is_null() {
            return p;
        }
    }
    // SAFETY: as above, with the process-wide search handle.
    unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
}

type CreateWindowSurface = unsafe extern "C" fn(*mut c_void, *mut c_void, c_ulong, *const i32) -> *mut c_void;
type SwapInterval = unsafe extern "C" fn(*mut c_void, i32) -> u32;

fn host_create_window_surface() -> Option<CreateWindowSurface> {
    static F: OnceLock<usize> = OnceLock::new();
    let p = *F.get_or_init(|| host_egl_symbol(c"eglCreateWindowSurface") as usize);
    // SAFETY: the host's `eglCreateWindowSurface`, whose X11 prototype this is.
    (p != 0).then(|| unsafe { std::mem::transmute::<usize, CreateWindowSurface>(p) })
}

fn host_swap_interval() -> Option<SwapInterval> {
    static F: OnceLock<usize> = OnceLock::new();
    let p = *F.get_or_init(|| host_egl_symbol(c"eglSwapInterval") as usize);
    // SAFETY: the host's `eglSwapInterval`.
    (p != 0).then(|| unsafe { std::mem::transmute::<usize, SwapInterval>(p) })
}

/// `eglCreateWindowSurface`: the same display, config and attributes, with
/// the client window's XID (0 before there is one) in place of the
/// `ANativeWindow*`. `EGL_NO_SURFACE` when the host has no EGL.
extern "C" fn egl_create_window_surface(
    display: *mut c_void,
    config: *mut c_void,
    _native_window: *mut c_void,
    attribs: *const i32,
) -> *mut c_void {
    super::glcount::CREATE_WINDOW_SURFACE.fetch_add(1, Ordering::Relaxed);
    let Some(f) = host_create_window_surface() else {
        return std::ptr::null_mut();
    };
    let xid = x11_window::current().map_or(0, |w| w.egl_native_window());
    // SAFETY: the host entry, with the engine's own display, config and
    // attribute list.
    unsafe { f(display, config, xid, attribs) }
}

/// `eglSwapInterval`: always 0, whatever was asked. Honouring 1 gave 1 Hz
/// swaps on an Xwayland host (INHERITED, not re-measured), and vblank-paced
/// presentation runs at about 1 Hz on this machine's Xorg for Vulkan too.
/// `EGL_FALSE` when the host has no EGL.
extern "C" fn egl_swap_interval(display: *mut c_void, _interval: i32) -> u32 {
    match host_swap_interval() {
        // SAFETY: the host entry with the engine's display.
        Some(f) => unsafe { f(display, 0) },
        None => 0,
    }
}

/// The eleven entry points this module answers, for the symbol table.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    vec![
        ("ANativeWindow_fromSurface", native_window_from_surface as *mut c_void),
        ("ANativeWindow_acquire", native_window_acquire as *mut c_void),
        ("ANativeWindow_release", native_window_release as *mut c_void),
        ("ANativeWindow_getWidth", native_window_get_width as *mut c_void),
        ("ANativeWindow_getHeight", native_window_get_height as *mut c_void),
        ("ANativeWindow_getFormat", native_window_get_format as *mut c_void),
        ("ANativeWindow_setBuffersGeometry", native_window_set_buffers_geometry as *mut c_void),
        ("ANativeWindow_lock", native_window_lock as *mut c_void),
        ("ANativeWindow_unlockAndPost", native_window_unlock_and_post as *mut c_void),
        ("eglCreateWindowSurface", egl_create_window_surface as *mut c_void),
        ("eglSwapInterval", egl_swap_interval as *mut c_void),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_answers_from_its_geometry_and_null_is_no_window() {
        let g = SurfaceGeometry::new(1667, 1349);
        let w = Some(&g);
        assert_eq!((width_of(w), height_of(w), format_of(w)), (1667, 1349, 1));

        // Zero keeps the stored value: only the height changes.
        assert_eq!(set_buffers_geometry_of(w, 0, 720, 0), 0);
        assert_eq!((width_of(w), height_of(w), format_of(w)), (1667, 720, 1));
        assert_eq!(set_buffers_geometry_of(w, 800, 600, 4), 0);
        assert_eq!((width_of(w), height_of(w), format_of(w)), (800, 600, 4));
        assert_eq!(set_buffers_geometry_of(w, -5, -5, -1), 0);
        assert_eq!(g.get(), (800, 600, 4), "negative values keep what is there");

        assert_eq!((width_of(None), height_of(None), format_of(None)), (0, 0, 0));
        assert_eq!(set_buffers_geometry_of(None, 800, 600, 4), -22);
    }

    #[test]
    fn no_software_buffer_is_ever_handed_out() {
        let g = SurfaceGeometry::new(64, 64);
        let p = &g as *const SurfaceGeometry as *mut c_void;
        assert_eq!(native_window_lock(p, std::ptr::null_mut(), std::ptr::null_mut()), -38);
        assert_eq!(native_window_unlock_and_post(p), -38);
        assert_eq!(native_window_lock(std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), -38);
    }

    #[test]
    fn before_a_window_exists_there_is_nothing_to_hand_out() {
        // No test in this crate opens a window without an X display and an
        // explicit opt-in, so the singleton is empty here.
        if x11_window::current().is_some() {
            return;
        }
        assert!(native_window_from_surface(std::ptr::null_mut(), std::ptr::null_mut()).is_null());
        let any = 0x1000 as *mut c_void;
        assert_eq!(native_window_get_width(any), 0);
        assert_eq!(native_window_set_buffers_geometry(any, 1, 1, 1), -22);
    }

    #[test]
    fn a_resize_reports_only_a_real_change_and_keeps_the_format() {
        let g = SurfaceGeometry::new(1280, 720);
        g.set_buffers(0, 0, 4);
        assert_eq!(g.resize(1280, 720), None, "the same size is no change");
        assert_eq!(g.resize(0, 720), None);
        assert_eq!(g.resize(1667, -1), None);
        assert_eq!(g.get(), (1280, 720, 4));
        assert_eq!(g.resize(1667, 1349), Some(4), "a new size, with the format kept");
        assert_eq!(g.get(), (1667, 1349, 4));
        assert_eq!(g.resize(1667, 1349), None, "and only once");
    }

    #[test]
    fn the_host_egl_entry_is_found_through_the_library_handle() {
        // What `symtab` does at start-up.
        // SAFETY: a literal soname.
        let lib = unsafe { dlopen(c"libEGL.so.1".as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        if lib.is_null() {
            eprintln!("no libEGL.so.1 on this machine; nothing to resolve");
            return;
        }
        assert!(!host_egl_symbol(c"eglCreateWindowSurface").is_null());
        assert!(host_create_window_surface().is_some(), "the wrapper's host entry");
        assert!(host_swap_interval().is_some());
        // The defect this replaces: a null handle is "the calling object" on
        // FreeBSD, and this test binary does not define EGL.
        // SAFETY: a NUL-terminated name.
        assert!(unsafe { dlsym(std::ptr::null_mut(), c"eglCreateWindowSurface".as_ptr()) }.is_null());
        // And the process-wide search finds the same function.
        // SAFETY: as above.
        let default = unsafe { dlsym(RTLD_DEFAULT, c"eglCreateWindowSurface".as_ptr()) };
        assert_eq!(default, host_egl_symbol(c"eglCreateWindowSurface"));
    }

    #[test]
    fn eleven_overrides_under_their_public_names() {
        let names: Vec<&str> = overrides().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names.len(), 11);
        assert_eq!(names.iter().filter(|n| n.starts_with("ANativeWindow_")).count(), 9);
        assert!(names.contains(&"eglCreateWindowSurface") && names.contains(&"eglSwapInterval"));
    }
}
