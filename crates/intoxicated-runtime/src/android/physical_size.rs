//! The physical size of the screen the window is on, from the X server.
//!
//! The engine asks `DeviceUtils.getScreenPhysicalSizeInMillimeters` for it
//! and lays out against a null answer otherwise ("Call to
//! getScreenPhysicalSizeInMillimeters returned null"). Android answers for
//! the device's one display; the desktop equivalent is the monitor showing
//! the window.
//!
//! RandR first: each connected output reports the size its EDID gives, and
//! the window is matched to the output whose area it overlaps most, so a
//! second monitor answers for itself. Without RandR (or with no output that
//! reports a size) the core protocol's screen size, `XDisplayWidthMM` and
//! `XDisplayHeightMM`, which on a multi-monitor screen covers all of them.
//! Libraries are opened with `dlopen` like libX11 in `window.rs`, so a
//! missing libXrandr costs the per-monitor answer and nothing else.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_ushort, c_void};

type Display = *mut c_void;
type Window = c_ulong;

/// One monitor: where it sits in root coordinates and its size in pixels and
/// millimetres, millimetres as the output reports them (unrotated).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub mm_width: i32,
    pub mm_height: i32,
    /// Rotated by 90 or 270 degrees: the pixel area is turned, the panel's
    /// millimetres are not.
    pub quarter_turn: bool,
}

/// Where the answer came from, for the start-up line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A RandR output's own size.
    Output,
    /// The core protocol's size of the whole X screen.
    Screen,
    /// Nothing reported a size.
    Unknown,
}

impl Source {
    pub fn describe(self) -> &'static str {
        match self {
            Source::Output => "the monitor's RandR output",
            Source::Screen => "the X screen",
            Source::Unknown => "unknown",
        }
    }
}

/// The millimetres of the monitor `window` (x, y, width, height) overlaps
/// most, oriented as the monitor is turned. Monitors that report no size
/// are passed over. A window on none of them takes the first that has one.
pub(crate) fn monitor_mm(monitors: &[Monitor], window: (i32, i32, i32, i32)) -> Option<(i32, i32)> {
    let (wx, wy, ww, wh) = window;
    let sized = || monitors.iter().filter(|m| m.mm_width > 0 && m.mm_height > 0);
    let overlap = |m: &Monitor| -> i64 {
        let left = wx.max(m.x) as i64;
        let right = (wx as i64 + ww as i64).min(m.x as i64 + m.width as i64);
        let top = wy.max(m.y) as i64;
        let bottom = (wy as i64 + wh as i64).min(m.y as i64 + m.height as i64);
        (right - left).max(0) * (bottom - top).max(0)
    };
    // The first of the monitors with the largest overlap, so ties go to the
    // one RandR lists first.
    let mut best: Option<(&Monitor, i64)> = None;
    for m in sized() {
        let area = overlap(m);
        if best.map_or(true, |(_, a)| area > a) {
            best = Some((m, area));
        }
    }
    let m = best.map(|(m, _)| m)?;
    Some(if m.quarter_turn { (m.mm_height, m.mm_width) } else { (m.mm_width, m.mm_height) })
}

// ------------------------------------------------------------ X

/// `XRRScreenResources`, from Xrandr.h, up to the outputs.
#[repr(C)]
struct XrrScreenResources {
    timestamp: c_ulong,
    config_timestamp: c_ulong,
    ncrtc: c_int,
    crtcs: *mut c_ulong,
    noutput: c_int,
    outputs: *mut c_ulong,
}

/// `XRROutputInfo`, up to the connection state.
#[repr(C)]
struct XrrOutputInfo {
    timestamp: c_ulong,
    crtc: c_ulong,
    name: *mut c_char,
    name_len: c_int,
    mm_width: c_ulong,
    mm_height: c_ulong,
    connection: c_ushort,
}

/// `XRRCrtcInfo`, up to the rotation.
#[repr(C)]
struct XrrCrtcInfo {
    timestamp: c_ulong,
    x: c_int,
    y: c_int,
    width: c_uint,
    height: c_uint,
    mode: c_ulong,
    rotation: c_ushort,
}

/// `RR_Connected`.
const CONNECTED: c_ushort = 0;
/// `RR_Rotate_90 | RR_Rotate_270`.
const QUARTER_TURNS: c_ushort = 2 | 8;
const RTLD_NOW: c_int = 2;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Every connected output with a CRTC, as RandR reports it.
///
/// # Safety
/// `display` must be an open Xlib display and `root` its root window, used
/// from the thread that owns the display.
unsafe fn randr_monitors(display: Display, root: Window) -> Option<Vec<Monitor>> {
    let lib = dlopen(c"libXrandr.so.2".as_ptr(), RTLD_NOW);
    if lib.is_null() {
        return None;
    }
    macro_rules! sym {
        ($name:literal, $ty:ty) => {{
            let p = dlsym(lib, $name.as_ptr());
            if p.is_null() {
                return None;
            }
            std::mem::transmute::<*mut c_void, $ty>(p)
        }};
    }
    let get_resources = sym!(
        c"XRRGetScreenResourcesCurrent",
        unsafe extern "C" fn(Display, Window) -> *mut XrrScreenResources
    );
    let free_resources = sym!(c"XRRFreeScreenResources", unsafe extern "C" fn(*mut XrrScreenResources));
    let get_output = sym!(
        c"XRRGetOutputInfo",
        unsafe extern "C" fn(Display, *mut XrrScreenResources, c_ulong) -> *mut XrrOutputInfo
    );
    let free_output = sym!(c"XRRFreeOutputInfo", unsafe extern "C" fn(*mut XrrOutputInfo));
    let get_crtc = sym!(
        c"XRRGetCrtcInfo",
        unsafe extern "C" fn(Display, *mut XrrScreenResources, c_ulong) -> *mut XrrCrtcInfo
    );
    let free_crtc = sym!(c"XRRFreeCrtcInfo", unsafe extern "C" fn(*mut XrrCrtcInfo));

    let resources = get_resources(display, root);
    if resources.is_null() {
        return None;
    }
    let mut monitors = Vec::new();
    let r = &*resources;
    if !r.outputs.is_null() && r.noutput > 0 {
        for &output in std::slice::from_raw_parts(r.outputs, r.noutput as usize) {
            let info = get_output(display, resources, output);
            if info.is_null() {
                continue;
            }
            let o = &*info;
            if o.connection == CONNECTED && o.crtc != 0 {
                let crtc = get_crtc(display, resources, o.crtc);
                if !crtc.is_null() {
                    let c = &*crtc;
                    monitors.push(Monitor {
                        x: c.x,
                        y: c.y,
                        width: c.width as i32,
                        height: c.height as i32,
                        mm_width: o.mm_width.min(i32::MAX as c_ulong) as i32,
                        mm_height: o.mm_height.min(i32::MAX as c_ulong) as i32,
                        quarter_turn: c.rotation & QUARTER_TURNS != 0,
                    });
                    free_crtc(crtc);
                }
            }
            free_output(info);
        }
    }
    free_resources(resources);
    Some(monitors)
}

/// The core protocol's size of the default screen, or zeros.
///
/// # Safety
/// As for [`randr_monitors`].
unsafe fn screen_mm(display: Display) -> (i32, i32) {
    let lib = dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW);
    if lib.is_null() {
        return (0, 0);
    }
    let default_screen = dlsym(lib, c"XDefaultScreen".as_ptr());
    let width_mm = dlsym(lib, c"XDisplayWidthMM".as_ptr());
    let height_mm = dlsym(lib, c"XDisplayHeightMM".as_ptr());
    if default_screen.is_null() || width_mm.is_null() || height_mm.is_null() {
        return (0, 0);
    }
    let default_screen: unsafe extern "C" fn(Display) -> c_int = std::mem::transmute(default_screen);
    let width_mm: unsafe extern "C" fn(Display, c_int) -> c_int = std::mem::transmute(width_mm);
    let height_mm: unsafe extern "C" fn(Display, c_int) -> c_int = std::mem::transmute(height_mm);
    let screen = default_screen(display);
    (width_mm(display, screen), height_mm(display, screen))
}

/// The physical size of the screen showing `window` (root coordinates), in
/// millimetres, and where it came from. `(0, 0)` with [`Source::Unknown`]
/// when the server reports nothing, which the engine reads as null.
///
/// # Safety
/// As for [`randr_monitors`].
pub(crate) unsafe fn measure(display: Display, root: Window, window: (i32, i32, i32, i32)) -> (i32, i32, Source) {
    if let Some((w, h)) = randr_monitors(display, root).and_then(|m| monitor_mm(&m, window)) {
        return (w, h, Source::Output);
    }
    let (w, h) = screen_mm(display);
    if w > 0 && h > 0 {
        (w, h, Source::Screen)
    } else {
        (0, 0, Source::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: i32, y: i32, width: i32, height: i32, mm: (i32, i32)) -> Monitor {
        Monitor { x, y, width, height, mm_width: mm.0, mm_height: mm.1, quarter_turn: false }
    }

    #[test]
    fn one_monitor_answers_for_the_window_on_it() {
        // This machine's layout: one 3440x1440 output reporting 800x330 mm.
        let m = [monitor(0, 0, 3440, 1440, (800, 330))];
        assert_eq!(monitor_mm(&m, (0, 0, 1280, 720)), Some((800, 330)));
    }

    #[test]
    fn the_monitor_with_most_of_the_window_wins() {
        let m = [monitor(0, 0, 3440, 1440, (800, 330)), monitor(3440, 240, 1920, 1200, (520, 320))];
        // Wholly on the second.
        assert_eq!(monitor_mm(&m, (3440 + 320, 240 + 240, 1280, 720)), Some((520, 320)));
        // Straddling, mostly on the second.
        assert_eq!(monitor_mm(&m, (3440 - 200, 300, 1280, 720)), Some((520, 320)));
        // Straddling, mostly on the first.
        assert_eq!(monitor_mm(&m, (3440 - 1000, 300, 1280, 720)), Some((800, 330)));
    }

    #[test]
    fn a_window_off_every_monitor_takes_the_first() {
        let m = [monitor(0, 0, 1920, 1080, (530, 300)), monitor(1920, 0, 1920, 1080, (600, 340))];
        assert_eq!(monitor_mm(&m, (-5000, -5000, 100, 100)), Some((530, 300)));
    }

    #[test]
    fn a_quarter_turned_monitor_swaps_its_millimetres() {
        let mut m = monitor(0, 0, 1080, 1920, (530, 300));
        m.quarter_turn = true;
        assert_eq!(monitor_mm(&[m], (0, 0, 720, 1280)), Some((300, 530)));
    }

    #[test]
    fn monitors_without_a_size_are_passed_over() {
        // A projector or a virtual output reports 0 mm.
        let m = [monitor(0, 0, 1920, 1080, (0, 0)), monitor(1920, 0, 1920, 1080, (600, 340))];
        assert_eq!(monitor_mm(&m, (0, 0, 1280, 720)), Some((600, 340)));
        assert_eq!(monitor_mm(&[monitor(0, 0, 1920, 1080, (0, 0))], (0, 0, 10, 10)), None);
        assert_eq!(monitor_mm(&[], (0, 0, 10, 10)), None);
    }

    #[test]
    fn the_struct_prefixes_match_xrandr_h() {
        use std::mem::offset_of;
        assert_eq!(offset_of!(XrrScreenResources, noutput), 32);
        assert_eq!(offset_of!(XrrScreenResources, outputs), 40);
        assert_eq!(offset_of!(XrrOutputInfo, mm_width), 32);
        assert_eq!(offset_of!(XrrOutputInfo, mm_height), 40);
        assert_eq!(offset_of!(XrrOutputInfo, connection), 48);
        assert_eq!(offset_of!(XrrCrtcInfo, x), 8);
        assert_eq!(offset_of!(XrrCrtcInfo, width), 16);
        assert_eq!(offset_of!(XrrCrtcInfo, rotation), 32);
    }
}
