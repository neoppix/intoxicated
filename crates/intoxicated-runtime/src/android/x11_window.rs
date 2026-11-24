//! The client's one window: an X11 window the engine renders into, and the
//! input read from it.
//!
//! What lives here:
//!
//! * Opening it ([`open`]): libX11 loaded at run time, the placement switches
//!   (`INTOXICATED_WINDOW_POS`, `INTOXICATED_MONITOR`, `INTOXICATED_FULLSCREEN`),
//!   ICCCM's `WM_NAME`, `WM_NORMAL_HINTS`, `WM_HINTS`, `WM_CLASS` and
//!   `WM_PROTOCOLS`, the EWMH icon and fullscreen state, a blank cursor, and
//!   the event mask.
//! * The input pump ([`pump_input_events`]), called by the main pump on every
//!   iteration: it never blocks, drains what Xlib has, and dispatches buttons,
//!   the wheel, motion, keys, focus, Expose, resizes, the clipboard's selection
//!   events and the window manager's close request.
//! * The pointer lock: an `XGrabPointer` confined to this window, with the
//!   camera's movement from `XI_RawMotion` (see [`xinput`]) or, without
//!   XInput2, from core motion read the warp way ([`WarpPathMotion`]).
//!
//! The engine-facing size and format are a [`SurfaceGeometry`], shared with
//! `ANativeWindow_*` in `native_window.rs`, and the `ANativeWindow*` the
//! engine holds is the address of the [`HostWindow`] itself.

mod placement;
mod xinput;
mod xlib;

#[cfg(test)]
mod tests;

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::native_window::SurfaceGeometry;
use xinput::{
    choose_camera_delta, current_pointer_acceleration, is_duplicate_raw, raw_motion_axes, RawSampleKey,
    XGenericEventCookie, XIRawEvent, Xi, Xim, GENERIC_EVENT, XI_RAW_MOTION,
};
use xlib::*;

/// `WM_CLASS`: the instance name and the class. The class has to equal
/// `StartupWMClass=` in `packaging/freebsd/intoxicated.desktop`, or the
/// desktop cannot tie the running window to its launcher entry (a test pins
/// the two together). A mismatch is invisible in normal use, which is why it
/// is pinned rather than trusted.
const WM_RES_NAME: &str = "intoxicated";
const WM_RES_CLASS: &str = "Intoxicated";

/// The events this window selects: keys, buttons, motion, Expose (the
/// engine's redraw), StructureNotify (resizes) and focus. Not selected:
/// enter/leave, property and visibility changes. ClientMessage, the
/// selection events and XInput2's GenericEvent need no mask bit.
const EVENT_MASK: c_long = KEY_PRESS_MASK
    | KEY_RELEASE_MASK
    | BUTTON_PRESS_MASK
    | BUTTON_RELEASE_MASK
    | POINTER_MOTION_MASK
    | EXPOSURE_MASK
    | STRUCTURE_NOTIFY_MASK
    | FOCUS_CHANGE_MASK;

const FOCUS_IN: c_int = 9;
const CLIENT_MESSAGE: c_int = 33;
use super::x11_clipboard::{SELECTION_CLEAR, SELECTION_NOTIFY, SELECTION_REQUEST};

// X11 modifier bits (X.h) consulted here are in `xlib`; the META mapping is
// `input::android_meta_state`.

// `android.view.MotionEvent.BUTTON_*` / `ACTION_*` live in `input.rs`.
use super::input::{
    deliver_surface_redraw, report_keyboard_state, ACTION_BUTTON_PRESS, ACTION_BUTTON_RELEASE,
    ACTION_DOWN, ACTION_UP, BUTTON_BACK, BUTTON_FORWARD, BUTTON_PRIMARY, BUTTON_SECONDARY,
    BUTTON_TERTIARY,
};

/// `XK_F11`, the client's own fullscreen toggle.
const XK_F11: c_ulong = 0xffc8;

/// The pointer-lock switches, read once.
struct LockSwitches {
    /// `INTOXICATED_NO_POINTER_LOCK`: never lock, the control for the path.
    never: bool,
    /// `INTOXICATED_NO_DRAG_LOCK`: lock only when the engine asks.
    no_drag_lock: bool,
    /// `INTOXICATED_FORCE_POINTER_LOCK`: hold the lock whenever focused.
    force: bool,
}

fn lock_switches() -> &'static LockSwitches {
    static SWITCHES: OnceLock<LockSwitches> = OnceLock::new();
    SWITCHES.get_or_init(|| {
        let flag = |name: &str| std::env::var_os(name).is_some();
        LockSwitches {
            never: flag("INTOXICATED_NO_POINTER_LOCK"),
            no_drag_lock: flag("INTOXICATED_NO_DRAG_LOCK"),
            force: flag("INTOXICATED_FORCE_POINTER_LOCK"),
        }
    })
}

/// Mouse state the AGDK events are built from.
struct InputState {
    /// Android button bits held, as far as this window knows.
    buttons: i32,
    /// When the current gesture's first button went down, on [`Self::clock`].
    down_time_ms: i64,
    /// Every AGDK event time is milliseconds since the window opened.
    clock: Instant,
    /// Where the pointer last was in window coordinates, from the last core
    /// button or motion event. A button release the client has to synthesise --
    /// on a real focus loss, or after another client's grab swallowed the
    /// real one -- has no event of its own to take a position from.
    last_pos: (f32, f32),
}

// Held keys are tracked in `input::Core`, by physical key, so a key released
// after a modifier changed releases exactly what was pressed.

/// The pointer lock.
///
/// The lock is an `XGrabPointer` confined to this window either way. Where the
/// camera's movement comes from depends on the server: `XI_RawMotion` when
/// XInput2 is there ([`Xi`]), and otherwise the original mechanism --
/// warp the pointer to a fixed centre and read each core `MotionNotify` as a
/// distance from it.
struct PointerLockState {
    locked: bool,
    /// Core motion read the warp way, with the warps this backend has issued
    /// and not yet seen land. See [`WarpPathMotion`].
    warp: WarpPathMotion,
    /// Where the hidden pointer is held while locked, in window coordinates,
    /// and the absolute position the engine is told throughout. The window's
    /// centre for the warp fallback and whenever the engine itself asked for a
    /// centred lock; on the raw path, a camera drag is held where the button
    /// went down instead -- see `lock_pointer`.
    centre: (i32, i32),
    /// Where the pointer was on the root window when the lock was taken, so
    /// the release puts it back there.
    saved_root: Option<(i32, i32)>,
    /// Whether this lock has seen `XI_RawMotion` from a relative device. Until
    /// it has, core motion is still read the warp way, which is what keeps an
    /// absolute-only pointer (a tablet, most VM pointers) working under XI2:
    /// its raw values are positions and are refused, so nothing else would
    /// move the camera. See [`Xi::device_is_absolute`].
    raw_relative_seen: bool,
    /// Devices already asked whether they are absolute, this lock. Cleared on
    /// every lock rather than kept for the session because X reuses device
    /// ids across hot-plugs, and one `XIQueryDevice` per device per lock is
    /// nothing.
    absolute_devices: Vec<(c_int, bool)>,
    /// Totals since the window opened, for the `pointerlock` report: raw
    /// events taken and raw events refused as absolute. Deliveries are
    /// counted where they happen, in `input`.
    raw_events: u64,
    raw_refused: u64,
    /// The last raw sample taken, and how many second deliveries of one were
    /// dropped. See [`is_duplicate_raw`].
    last_raw: Option<RawSampleKey>,
    raw_duplicates: u64,
}

impl PointerLockState {
    fn new() -> Self {
        PointerLockState {
            locked: false,
            warp: WarpPathMotion::engaged((0, 0)),
            centre: (0, 0),
            saved_root: None,
            raw_relative_seen: false,
            absolute_devices: Vec::new(),
            raw_events: 0,
            raw_refused: 0,
            last_raw: None,
            raw_duplicates: 0,
        }
    }
}

/// The window.
///
/// One per process, made by [`open`] and kept until exit; only the GL probe
/// destroys its X window ([`HostWindow::close`]). The address of this object
/// is the `ANativeWindow*` the engine is handed.
///
/// **Threads.** Events are read, and the lock, the cursor and the fullscreen
/// state changed, only on the main pump's thread, which also runs the
/// clipboard on the same connection. The NVIDIA Vulkan driver uses the same
/// `Display*` from the engine's render thread, because the Xlib surface is
/// created on it. `XInitThreads` is called before `XOpenDisplay`, so libX11
/// locks the display around every call whichever thread makes it (libX11
/// 1.8 does that by default; the call makes it independent of how libX11
/// was built). The geometry and the input and lock state are behind their
/// own locks, none held while another is taken except the geometry's.
pub struct HostWindow {
    xlib: Xlib,
    display: Display,
    window: Window,
    conn_fd: c_int,
    /// The two atoms a window manager's close request is spelled in, kept so
    /// the event pump can recognise one without a round trip to the server.
    wm_protocols: c_ulong,
    wm_delete_window: c_ulong,
    geometry: SurfaceGeometry,
    input: Mutex<InputState>,
    pointer_lock: Mutex<PointerLockState>,
    /// What this client last asked the window manager for. Not updated when
    /// the window manager changes the state by itself (an i3 key binding), so
    /// F11 then toggles from a stale value.
    fullscreen: AtomicBool,
    /// Whether F11 is down, so its auto-repeat presses toggle nothing.
    f11_held: AtomicBool,
    /// XInput2 when the server negotiated it, `None` for the warp fallback.
    /// See [`Xi`].
    xi: Option<Xi>,
    /// The local input method for text-box text, `None` when libX11 would
    /// not open one (text then comes from keysyms, without dead keys).
    xim: Option<Xim>,
    /// Keyboard focus as the last *real* `FocusIn`/`FocusOut` left it: 0 not
    /// yet known, 1 focused, 2 not. See [`focus_change_is_real`] for which
    /// focus events count, and `sync_pointer_lock` for what reads it.
    focused: AtomicU8,
    /// Where the window was placed, in root coordinates, which is what
    /// [`HostWindow::physical_mm`] matches against the monitors.
    origin: (c_int, c_int),
}

// SAFETY: see the threading note on the type. The raw pointers are the X
// connection and the input context, both owned by this object for the life of
// the process.
unsafe impl Send for HostWindow {}
unsafe impl Sync for HostWindow {}

static WINDOW: OnceLock<HostWindow> = OnceLock::new();

/// Set when the window manager delivers `WM_DELETE_WINDOW`, and only read by
/// the pump's stop check (`looper::pump`) through `android::window_closed`. A
/// flag rather than a call into the pump, so `INTOXICATED_NO_CLOSE_EXIT` can
/// gate it there.
static WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);

/// The window, once it is open.
pub fn current() -> Option<&'static HostWindow> {
    WINDOW.get()
}

/// Open the window, `width` x `height` unless the placement switches say
/// otherwise, titled `title`. Once a window exists it is returned as it is
/// and the arguments are ignored. No X display is an error the caller
/// reports, not a crash.
pub fn open(width: u32, height: u32, title: &str) -> Result<&'static HostWindow, String> {
    static OPENING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = OPENING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = WINDOW.get() {
        return Ok(w);
    }
    let xlib = Xlib::load()?;
    // SAFETY: plain Xlib calls; `XInitThreads` first, as it asks.
    let display = unsafe {
        (xlib.init_threads)();
        (xlib.open_display)(std::ptr::null())
    };
    if display.is_null() {
        return Err("no X display (is DISPLAY set?)".into());
    }

    let clamp = |v: u32| v.clamp(1, i32::MAX as u32) as i32;
    let mut warnings = Vec::new();
    // SAFETY: `display` was opened above with this `xlib`.
    let place = placement::place(
        clamp(width),
        clamp(height),
        &placement::PlacementRequest::from_env(),
        || unsafe { xinerama_heads(&xlib, display) },
        &mut warnings,
    );
    for w in &warnings {
        println!("{w}");
    }
    println!(
        "[android] window placement: {}x{} at {},{}{}",
        place.width,
        place.height,
        place.x,
        place.y,
        if place.fullscreen { " (fullscreen)" } else { "" }
    );

    // SAFETY: every call is on the connection opened above and the window
    // created at the top of the block; the structures passed are whole and
    // live for the call.
    let (window, conn_fd, wm_protocols, wm_delete_window) = unsafe {
        let root = (xlib.default_root_window)(display);
        let w = (xlib.create_simple_window)(
            display,
            root,
            place.x,
            place.y,
            place.width.max(1) as c_uint,
            place.height.max(1) as c_uint,
            0,
            0,
            0,
        );

        // WM_NAME is Latin-1 STRING; see `placement::ascii_title`.
        let name = CString::new(placement::ascii_title(title)).unwrap_or_default();
        (xlib.store_name)(display, w, name.as_ptr());

        // Set the taskbar / window icon (the Intoxicated logo) via _NET_WM_ICON.
        // The property is an array of CARDINALs: width, height, then width*height
        // ARGB pixels (0xAARRGGBB), one or more images concatenated. `icon_blob`
        // is that sequence stored as little-endian u32; Xlib's format-32 wants
        // each value in a C `long`, so widen to c_ulong before handing it over.
        // Works without any installed .desktop/theme, which is what a
        // run-from-source build needs.
        {
            const ICON_BLOB: &[u8] = include_bytes!("intoxicated_icon.bin");
            let icon: Vec<c_ulong> = ICON_BLOB
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as c_ulong)
                .collect();
            const XA_CARDINAL: c_ulong = 6; // predefined atom, no intern needed
            const PROP_MODE_REPLACE: c_int = 0;
            let net_wm_icon = CString::new("_NET_WM_ICON").unwrap();
            let prop = (xlib.intern_atom)(display, net_wm_icon.as_ptr(), 0);
            if prop != 0 && !icon.is_empty() {
                (xlib.change_property)(
                    display,
                    w,
                    prop,
                    XA_CARDINAL,
                    32,
                    PROP_MODE_REPLACE,
                    icon.as_ptr() as *const u8,
                    icon.len() as c_int,
                );
            }
        }

        // The user-specified position, so a window manager that honours it
        // puts the window where it was asked to be.
        let mut size_hints = XSizeHints {
            flags: US_POSITION,
            x: place.x,
            y: place.y,
            width: place.width,
            height: place.height,
            ..XSizeHints::default()
        };
        (xlib.set_wm_normal_hints)(display, w, &mut size_hints);
        let mut wm_hints =
            XWMHints { flags: INPUT_HINT | STATE_HINT, input: 1, initial_state: NORMAL_STATE, ..XWMHints::default() };
        (xlib.set_wm_hints)(display, w, &mut wm_hints);

        // Advertise `WM_DELETE_WINDOW`, so the window manager asks us to close
        // rather than severing the connection. Without it a close button ends
        // the client in Xlib's fatal I/O handler with status 1, skipping the
        // lifecycle teardown every other way out goes through.
        let wm_protocols = (xlib.intern_atom)(display, c"WM_PROTOCOLS".as_ptr(), 0);
        let wm_delete_window = (xlib.intern_atom)(display, c"WM_DELETE_WINDOW".as_ptr(), 0);
        if wm_protocols != 0 && wm_delete_window != 0 {
            let mut protocol = wm_delete_window;
            (xlib.set_wm_protocols)(display, w, &mut protocol, 1);
        }

        let res_name = CString::new(WM_RES_NAME).unwrap();
        let res_class = CString::new(WM_RES_CLASS).unwrap();
        let mut class_hint =
            XClassHint { res_name: res_name.as_ptr() as *mut c_char, res_class: res_class.as_ptr() as *mut c_char };
        (xlib.set_class_hint)(display, w, &mut class_hint);

        // The engine draws its own cursor, so the host's is hidden over this
        // window (and only this window) for the whole session, locked or not.
        if std::env::var_os("INTOXICATED_SHOW_CURSOR").is_none() {
            if define_blank_cursor(&xlib, display, w) {
                eprintln!("[intoxicated] host cursor hidden over the client window");
            } else {
                eprintln!("[intoxicated] could not create a blank cursor; host pointer stays visible");
            }
        }

        (xlib.select_input)(display, w, EVENT_MASK);

        // Detectable auto-repeat: without it the server sends a KeyRelease
        // before every repeat KeyPress for a held key, and `dispatch_key`
        // forwarded that as a release/press burst -- a held key cancelled and
        // spammed the engine once the repeat delay elapsed. With it, a held key
        // is bare repeated presses and the real release only comes when the key
        // is physically let go. Process-global to this X connection, so it is
        // set once here. The `supported` out-argument is not read: every X
        // server the client can reach supports it, and the fallback if one did
        // not would be exactly today's behaviour, which `dispatch_key`'s own
        // repeat drop already guards against.
        (xlib.set_detectable_auto_repeat)(display, 1, std::ptr::null_mut());

        // Mapped and synchronised before anything moves it: a move sent
        // before the window manager has placed the window loses that race.
        (xlib.map_window)(display, w);
        (xlib.sync)(display, 0);

        if place.fullscreen {
            set_compositor_bypass(&xlib, display, w, true);
            if let Some(m) = place.monitor {
                let atom = (xlib.intern_atom)(display, c"_NET_WM_FULLSCREEN_MONITORS".as_ptr(), 0);
                if atom != 0 {
                    let m = m as c_long;
                    xlib.send_client_message(display, w, atom, [m, m, m, m, 1]);
                }
            }
            request_fullscreen(&xlib, display, w, true);
        } else if (place.x, place.y) != (0, 0) {
            (xlib.move_window)(display, w, place.x, place.y);
        }
        (xlib.flush)(display);
        (xlib.sync)(display, 0);
        (w, (xlib.connection_number)(display), wm_protocols, wm_delete_window)
    };

    // Once, on stdout, unconditionally: which of the two lock mechanisms
    // this session's camera runs on is the first thing a report about how the
    // mouse feels on X11 needs, and it differs by server, not by build.
    let xi = match Xi::init(&xlib, display) {
        Ok(xi) => {
            println!(
                "[android] X11 pointer lock: XInput {}.{} raw motion (camera {})",
                xi.version.0,
                xi.version.1,
                if current_pointer_acceleration() {
                    "accelerated by the X server's pointer settings"
                } else {
                    "unaccelerated: INTOXICATED_POINTER_ACCEL=unlocked"
                },
            );
            Some(xi)
        }
        Err(why) => {
            println!(
                "[android] X11 pointer lock: no XInput2 ({why}); falling back to \
                 warp-to-centre, whose deltas the X server has already accelerated, \
                 so INTOXICATED_POINTER_ACCEL=unlocked cannot be honoured"
            );
            None
        }
    };

    // Once, on stdout, like the lock mechanism above: whether typed text has
    // dead keys and composition is a property of the session.
    let xim = match Xim::open(display, window) {
        Ok(xim) => {
            println!("[android] X11 text input: libX11's local input method (dead keys and Compose)");
            Some(xim)
        }
        Err(why) => {
            println!("[android] X11 text input: no input method ({why}); text comes from keysyms, without dead keys");
            None
        }
    };

    let host = HostWindow {
        xlib,
        display,
        window,
        conn_fd,
        wm_protocols,
        wm_delete_window,
        geometry: SurfaceGeometry::new(place.width, place.height),
        origin: (place.x, place.y),
        input: Mutex::new(InputState {
            buttons: 0,
            down_time_ms: 0,
            clock: Instant::now(),
            last_pos: (0.0, 0.0),
        }),
        pointer_lock: Mutex::new(PointerLockState::new()),
        fullscreen: AtomicBool::new(place.fullscreen),
        f11_held: AtomicBool::new(false),
        xi,
        xim,
        focused: std::sync::atomic::AtomicU8::new(0),
    };
    // No touchscreen. That is a statement about the window system rather than
    // about the machine: X11 core input has no touch at all, and XInput2 --
    // which this backend now binds, but only for `XI_RawMotion` -- carries
    // touch only in `XI_TouchBegin`/`Update`/`End`, which nothing here selects.
    // So a touchscreen on this host never reaches the engine as touch, and
    // telling the engine there is one would only change its layout. Anyone
    // who wants the touch interface on X11 has `INTOXICATED_INPUT_TOUCH=1`,
    // which overrides this.
    super::input::report_touchscreen(false);
    let _ = WINDOW.set(host);
    WINDOW.get().ok_or_else(|| "the window could not be kept".into())
}

/// A 1x1 empty bitmap as both source and mask: a cursor with no visible
/// pixel, defined on `window` alone. Whether it could be made.
///
/// SAFETY: `display` and `window` must be live handles of `xlib`.
unsafe fn define_blank_cursor(xlib: &Xlib, display: Display, window: Window) -> bool {
    let bits = [0 as c_char; 1];
    // SAFETY: per this function's contract; `bits` is the one byte a 1x1
    // bitmap reads, and the colours are whole `XColor`s Xlib may write to.
    unsafe {
        let pixmap = (xlib.create_bitmap_from_data)(display, window, bits.as_ptr(), 1, 1);
        if pixmap == 0 {
            return false;
        }
        let (mut fg, mut bg) = (XColor::default(), XColor::default());
        let cursor = (xlib.create_pixmap_cursor)(display, pixmap, pixmap, &mut fg, &mut bg, 0, 0);
        if cursor != 0 {
            (xlib.define_cursor)(display, window, cursor);
        }
        (xlib.free_pixmap)(display, pixmap);
        cursor != 0
    }
}

/// Ask the window manager for fullscreen on or off (`_NET_WM_STATE`, action
/// ADD or REMOVE, `_NET_WM_STATE_FULLSCREEN`, source indication 1). The size
/// follows in a ConfigureNotify.
///
/// SAFETY: `display` and `window` must be live handles of `xlib`.
unsafe fn request_fullscreen(xlib: &Xlib, display: Display, window: Window, on: bool) {
    // SAFETY: per this function's contract.
    unsafe {
        let state = (xlib.intern_atom)(display, c"_NET_WM_STATE".as_ptr(), 0);
        let fullscreen = (xlib.intern_atom)(display, c"_NET_WM_STATE_FULLSCREEN".as_ptr(), 0);
        if state == 0 || fullscreen == 0 {
            return;
        }
        xlib.send_client_message(display, window, state, [on as c_long, fullscreen as c_long, 0, 1, 0]);
    }
}

impl HostWindow {
    /// `(width, height, format)` as the engine is told them.
    pub fn geometry(&self) -> (i32, i32, i32) {
        self.geometry.get()
    }

    /// The physical size in millimetres of the monitor this window is on, as
    /// the X server reports it, and where the figure came from. `(0, 0)` when
    /// nothing reports one. See [`super::physical_size`].
    pub fn physical_mm(&self) -> (i32, i32, super::physical_size::Source) {
        let (width, height, _) = self.geometry();
        // SAFETY: the display is open for the life of the process, and this
        // is called from the thread that owns it, like every other Xlib call
        // here.
        unsafe {
            let root = (self.xlib.default_root_window)(self.display);
            super::physical_size::measure(self.display, root, (self.origin.0, self.origin.1, width, height))
        }
    }

    /// The shared engine-facing geometry, for `ANativeWindow_*`.
    pub fn surface_geometry(&self) -> &SurfaceGeometry {
        &self.geometry
    }

    /// The X connection's descriptor, for the looper's wait.
    pub fn connection_fd(&self) -> c_int {
        self.conn_fd
    }

    /// The `Display*`, which X11 EGL and `VK_KHR_xlib_surface` take.
    pub fn egl_native_display(&self) -> *mut c_void {
        self.display
    }

    /// The X window id, which X11 EGL and `VK_KHR_xlib_surface` take in place
    /// of an `ANativeWindow*`.
    pub fn egl_native_window(&self) -> c_ulong {
        self.window
    }

    /// Destroy the X window. Only the GL probe does this.
    pub fn close(&self) {
        // SAFETY: this window's own live handles.
        unsafe {
            (self.xlib.destroy_window)(self.display, self.window);
            (self.xlib.flush)(self.display);
        }
    }

    /// Milliseconds on the input clock.
    fn now_ms(&self) -> i64 {
        self.input.lock().unwrap_or_else(|e| e.into_inner()).clock.elapsed().as_millis() as i64
    }

    /// Fullscreen on or off, through the window manager. Sent whether or not
    /// the flag already says so; devctl `fullscreen`/`windowed` and F11 come
    /// here.
    pub fn set_fullscreen(&self, on: bool) {
        let xlib = &self.xlib;
        // SAFETY: this window's own live handles.
        unsafe {
            request_fullscreen(xlib, self.display, self.window, on);
            (xlib.flush)(self.display);
            set_compositor_bypass(xlib, self.display, self.window, on);
        }
        self.fullscreen.store(on, Ordering::Release);
    }

    /// The focus as the last real focus event left it; `None` before any.
    fn focused(&self) -> Option<bool> {
        match self.focused.load(Ordering::Acquire) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// Take or release the pointer lock to match what the engine and the
    /// mouse are
    /// currently asking for. Called at both ends of `pump_input_events`, and
    /// straight after a camera button goes down, so a drag is captured before
    /// a fast pointer can leave the window rather than up to a pump later.
    ///
    /// Still a separate function from Wayland's `sync_pointer_lock`, though
    /// one would be better. The decision is now the same shape on
    /// both -- the engine's word, a camera drag, the forced override, a focus
    /// gate and no Escape latch -- and the motion source is now the same kind
    /// of thing (a pre/post-acceleration pair, from `XI_RawMotion` here and
    /// `zwp_relative_pointer_v1` there), which is what makes sharing a real
    /// refactor rather than a wrapper. What still differs is
    /// everything around the decision: Wayland's right-drag latch, dialogs
    /// and constraint objects have no X11 counterpart yet, so the shared
    /// function is left for when they do rather than written around them.
    fn sync_pointer_lock(&self) {
        let switches = lock_switches();
        if switches.never {
            self.release_pointer_lock();
            return;
        }
        let (no_drag_lock, force) = (switches.no_drag_lock, switches.force);
        // Only an explicit yes counts; "cannot say" is no.
        let engine_wants = super::input::engine_wants_pointer_lock() == Some(true);
        let buttons = self.input.lock().unwrap_or_else(|e| e.into_inner()).buttons;
        let Some(want) =
            pointer_lock_decision(engine_wants, buttons, no_drag_lock, force, self.focused())
        else {
            return;
        };
        let held = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .locked;
        if want && !held {
            self.lock_pointer(engine_wants || force);
        } else if !want && held {
            self.release_pointer_lock();
        } else if want && held && (engine_wants || force) {
            // A drag lock held where the button went down, and the engine has
            // since asked for a centred one -- shift lock toggled mid-drag, or
            // the camera scrolled into first person while the button was
            // held. Move the anchor rather than keep reporting the drag's
            // position as the locked centre.
            self.recentre_lock();
        }
    }

    fn lock_pointer(&self, centred: bool) {
        let (width, height, _) = self.geometry();
        if width <= 0 || height <= 0 {
            return;
        }
        // SAFETY: this struct's own open display.
        let root = unsafe { (self.xlib.default_root_window)(self.display) };
        let (mut root_ret, mut child, mut root_x, mut root_y, mut win_x, mut win_y, mut mask) =
            (0, 0, 0, 0, 0, 0, 0);
        // Relative to this window, so `win_x`/`win_y` are where the pointer
        // is over the canvas (the drag anchor) and `root_x`/`root_y` where to
        // put it back on release.
        // SAFETY: this struct's own live handles; the out-pointers are locals.
        let queried = unsafe {
            (self.xlib.query_pointer)(
                self.display,
                self.window,
                &mut root_ret,
                &mut child,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        let centre = lock_anchor(
            centred,
            self.xi.is_some(),
            (queried != 0).then_some((win_x, win_y)),
            (width, height),
        );

        // SAFETY: this struct's own live handles.
        let result = unsafe {
            (self.xlib.grab_pointer)(
                self.display,
                self.window,
                1,
                (BUTTON_PRESS_MASK | BUTTON_RELEASE_MASK | POINTER_MOTION_MASK) as c_uint,
                GRAB_MODE_ASYNC,
                GRAB_MODE_ASYNC,
                self.window,
                0,
                CURRENT_TIME,
            )
        };
        if result != 0 {
            eprintln!("[intoxicated] X11 pointer lock was refused (XGrabPointer={result})");
            return;
        }

        {
            let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
            state.locked = true;
            state.centre = centre;
            state.saved_root = (queried != 0).then_some((root_x, root_y));
            // Replaced again once the queue has been emptied, below; set here
            // too so nothing read in between measures from a previous lock.
            state.warp = WarpPathMotion::engaged(centre);
            state.raw_relative_seen = false;
            state.absolute_devices.clear();
            state.last_raw = None;
        }

        if let Some(xi) = &self.xi {
            xi.select_raw_motion(self.display, root, true);
        }
        // SAFETY: this struct's own live handles.
        unsafe {
            (self.xlib.warp_pointer)(self.display, 0, self.window, 0, 0, 0, 0, centre.0, centre.1);
            (self.xlib.flush)(self.display);
        }

        // **Empty the queue of core motion from before the lock.** Until the
        // grab and the warp were processed the cursor was free, and every
        // position it reported on the way is still queued here, ahead of
        // anything the lock produces. None of it is camera movement, and none
        // of it can be recognised as stale by position: it is wherever the
        // cursor happened to be, often near an edge, and read as motion it is
        // a window-sized one-frame spin. This backend used to discard up to
        // four such events while waiting for the echo and then trust the
        // fifth, so a backlog of five or more at lock time -- a fast mouse
        // after a hitch -- still leaked one. On the raw path the same events
        // were read the warp way, because `raw_relative_seen` is false until
        // the first raw sample of the lock. `XSync` returns once the server
        // has processed the warp, by which point everything it generated
        // before it, the warp's echo included, is in Xlib's queue, so what
        // follows is genuinely the lock's. Raw motion is a `GenericEvent`,
        // which no core mask matches, so it is left where it is. The cost is
        // one round trip per lock, never per event.
        // SAFETY: this struct's own open display; `scratch` is larger than an
        // `XEvent`.
        let purged = unsafe {
            (self.xlib.sync)(self.display, 0);
            let mut scratch = [0u8; 256];
            let mut n = 0u32;
            while (self.xlib.check_mask_event)(
                self.display,
                POINTER_MOTION_MASK,
                scratch.as_mut_ptr() as *mut c_void,
            ) != 0
            {
                n += 1;
            }
            n
        };
        // The echo went with them, so nothing is expected.
        self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner()).warp =
            WarpPathMotion::engaged(centre);
        if super::input::trace_mouse() && purged > 0 {
            eprintln!("[intoxicated] X11 pointer lock: dropped {purged} motion event(s) queued before the grab");
        }

        // Anything read before the lock was unlocked motion and has been
        // delivered; the unlocked delta starts again after the release.
        super::input::discard_locked_motion();
        super::input::reset_mouse_delta();
        super::input::set_lock_anchor(Some(centre));
        if super::input::trace_mouse() {
            eprintln!(
                "[intoxicated] X11 pointer lock acquired at ({}, {}) via {}",
                centre.0,
                centre.1,
                if self.xi.is_some() { "XI_RawMotion" } else { "warp" },
            );
        }
    }

    /// Move a held lock's anchor to the window centre. See the last arm of
    /// `sync_pointer_lock`.
    fn recentre_lock(&self) {
        let (width, height, _) = self.geometry();
        let centre = (width / 2, height / 2);
        {
            let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
            if !state.locked || state.centre == centre {
                return;
            }
            state.centre = centre;
            state.warp.warped_to(centre);
        }
        super::input::set_lock_anchor(Some(centre));
        // SAFETY: this struct's own live handles.
        unsafe {
            (self.xlib.warp_pointer)(self.display, 0, self.window, 0, 0, 0, 0, centre.0, centre.1);
            (self.xlib.flush)(self.display);
        }
    }

    /// Give the pointer back, at the root position it had when the lock was
    /// taken. Nothing when not locked.
    fn release_pointer_lock(&self) {
        let saved_root = {
            let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
            if !state.locked {
                return;
            }
            state.locked = false;
            state.centre = (0, 0);
            state.warp = WarpPathMotion::engaged((0, 0));
            state.raw_relative_seen = false;
            state.last_raw = None;
            state.saved_root.take()
        };
        // Movement summed and not yet delivered belongs to a lock that no
        // longer exists; delivering it after the release would turn the
        // camera with the cursor already free. Every path that releases on
        // purpose (a camera button let go, a focus loss) has delivered it
        // already, because what reaches the engine next flushes it first.
        super::input::discard_locked_motion();
        super::input::set_lock_anchor(None);
        // SAFETY: this struct's own live handles.
        unsafe {
            if let Some(xi) = &self.xi {
                let root = (self.xlib.default_root_window)(self.display);
                xi.select_raw_motion(self.display, root, false);
            }
            (self.xlib.ungrab_pointer)(self.display, CURRENT_TIME);
            if let Some((x, y)) = saved_root {
                let root = (self.xlib.default_root_window)(self.display);
                (self.xlib.warp_pointer)(self.display, 0, root, 0, 0, 0, 0, x, y);
            }
            (self.xlib.flush)(self.display);
        }
        super::input::reset_mouse_delta();
        if super::input::trace_mouse() {
            eprintln!("[intoxicated] X11 pointer lock released");
        }
    }

    /// A `ButtonPress` or `ButtonRelease`: the wheel, or one real button.
    fn dispatch_button(&self, handle: i64, ev: &XInputEvent, press: bool) {
        if let Some((hscroll, vscroll)) = wheel_detents(ev.detail) {
            // A wheel notch is a press and a release; the press is the notch.
            if press {
                super::input::wheel(handle, ev.x as f32, ev.y as f32, hscroll, vscroll, self.now_ms());
            }
            return;
        }
        let Some(android_button) = android_button(ev.detail) else {
            return;
        };
        // While locked the engine has been told the pointer is at the anchor
        // all along, and a click has to land there too. On the raw path the
        // hidden pointer drifts away from the anchor between edge warps, so
        // the event's own position would put the release of a camera drag
        // somewhere the player never saw the cursor.
        let (x, y) = {
            let lock = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
            if lock.locked {
                (lock.centre.0 as f32, lock.centre.1 as f32)
            } else {
                (ev.x as f32, ev.y as f32)
            }
        };
        self.dispatch_button_bit(handle, android_button, x, y, press);
    }

    /// One button going down or up, to both input paths. Separate from
    /// [`Self::dispatch_button`] so a release the client synthesises -- a real
    /// focus loss, or a release another client's grab swallowed -- tells the
    /// engine exactly what a real one would, rather than only clearing the
    /// shadow bitmask and leaving the engine believing the button is down.
    fn dispatch_button_bit(&self, handle: i64, android_button: i32, x: f32, y: f32, press: bool) {
        let mut state = self.input.lock().unwrap_or_else(|e| e.into_inner());
        let Some(step) = ButtonStep::new(state.buttons, android_button, press) else {
            // A release of a button already up would be a release of one the
            // engine was never told is down, and a press of one already down
            // a second gesture inside the first. See `ButtonStep::new`.
            return;
        };
        state.last_pos = (x, y);
        let now = state.clock.elapsed().as_millis() as i64;
        if step.starts_gesture {
            state.down_time_ms = now;
        }
        state.buttons = step.buttons;
        let down_time = state.down_time_ms;
        drop(state);
        // AGDK's events for the step, then the interface's own
        // `nativePassMouseButton` -- every button, not only the primary one,
        // since a right-button drag is how a mouse turns the camera. Any
        // locked motion read before this edge goes to the engine first.
        super::input::mouse_button(handle, &step.agdk_actions(), x, y, press, android_button, now, down_time);
    }

    /// The gesture state the AGDK half of a motion event is sent with.
    fn gesture(&self) -> (i32, i64, i64) {
        let state = self.input.lock().unwrap_or_else(|e| e.into_inner());
        (state.buttons, state.down_time_ms, state.clock.elapsed().as_millis() as i64)
    }

    /// A core `MotionNotify`.
    fn dispatch_motion(&self, handle: i64, ev: &XInputEvent) {
        let (buttons, down_time, now) = self.gesture();
        {
            let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
            if state.locked {
                let centre = state.centre;
                // **On the raw path a core motion is not a measurement.** Once
                // this lock has seen relative `XI_RawMotion`, the camera's
                // movement comes from there (see `dispatch_generic`), and
                // reading this event as well would count the same movement
                // twice -- once unaccelerated, once as the server's
                // accelerated pointer position. All it is used for is keeping
                // the hidden pointer off the confinement's edge; see
                // `RAW_EDGE_MARGIN`.
                if self.xi.is_some() && state.raw_relative_seen {
                    let (w, h, _) = self.geometry();
                    if (ev.x, ev.y) != centre && near_edge((ev.x, ev.y), (w, h)) {
                        drop(state);
                        // SAFETY: this struct's own live handles.
                        unsafe {
                            (self.xlib.warp_pointer)(
                                self.display, 0, self.window, 0, 0, 0, 0, centre.0, centre.1,
                            );
                            (self.xlib.flush)(self.display);
                        }
                    }
                    return;
                }

                // Read now and handed to `input`, which delivers the drain's
                // sum at its end or before the next button or key, whichever
                // comes first. The warp back is once per drain. See
                // `WarpPathMotion` for why each event is measured from the last
                // rather than from the anchor.
                let (dx, dy) = state.warp.motion((ev.x, ev.y));
                let anchor = state.centre;
                drop(state);
                if (dx, dy) != (0, 0) {
                    super::input::add_locked_motion(anchor, dx as f64, dy as f64, false, buttons, down_time, now);
                }
                if super::input::trace_mouse() {
                    eprintln!(
                        "[intoxicated] X11 pointer lock: motion at ({}, {}) -> delta ({dx}, {dy})",
                        ev.x, ev.y
                    );
                }
                return;
            }
        }
        let (x, y) = (ev.x as f32, ev.y as f32);
        self.input.lock().unwrap_or_else(|e| e.into_inner()).last_pos = (x, y);
        // AGDK first (ACTION_MOVE with a button held, as part of the gesture
        // its DOWN started, else ACTION_HOVER_MOVE), then the path the
        // interface hit-tests, `nativePassMouseMove`, with the delta from the
        // last reported position.
        super::input::unlocked_motion(handle, x, y, buttons, down_time, now);
    }

    /// An XInput2 `GenericEvent`. Only `XI_RawMotion` is selected, and only
    /// while locked; anything else, or anything arriving after the lock went,
    /// is freed and dropped.
    fn dispatch_generic(&self, buf: &mut [u8; 256]) {
        let Some(xi) = &self.xi else { return };
        let cookie = buf.as_mut_ptr() as *mut XGenericEventCookie;
        // SAFETY: `XNextEvent` filled `buf` with a `GenericEvent`, whose
        // `XEvent` member is `XGenericEventCookie`; `buf` is 256 bytes, larger
        // than the union. `XGetEventData` fills `data`, which is freed below on
        // every path that got it, and nothing read through it outlives that.
        unsafe {
            if (*cookie).extension != xi.opcode || (xi.get_event_data)(self.display, cookie) == 0 {
                return;
            }
            if (*cookie).evtype == XI_RAW_MOTION && !(*cookie).data.is_null() {
                let raw = &*((*cookie).data as *const XIRawEvent);
                let mask_len = raw.valuators_mask_len.max(0) as usize;
                let mask: &[u8] = if raw.valuators_mask.is_null() || mask_len == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(raw.valuators_mask, mask_len)
                };
                let n: usize = mask.iter().map(|b| b.count_ones() as usize).sum();
                let acc: &[f64] = if raw.valuators_values.is_null() {
                    &[]
                } else {
                    std::slice::from_raw_parts(raw.valuators_values, n)
                };
                let unacc: &[f64] = if raw.raw_values.is_null() {
                    &[]
                } else {
                    std::slice::from_raw_parts(raw.raw_values, n)
                };
                let (acc, unacc) = raw_motion_axes(mask, acc, unacc);
                let key: RawSampleKey = (raw.time, raw.deviceid, raw.sourceid, unacc.0, unacc.1);
                self.take_raw_motion(xi, key, acc, unacc);
            }
            (xi.free_event_data)(self.display, cookie);
        }
    }

    /// Fold one raw sample into this drain's total, if the lock is held and
    /// the device is relative.
    fn take_raw_motion(&self, xi: &Xi, key: RawSampleKey, acc: (f64, f64), unacc: (f64, f64)) {
        let source = key.2;
        let (buttons, down_time, now) = self.gesture();
        let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
        if !state.locked {
            // In flight when the lock was released and the subscription
            // dropped. See `Xi::select_raw_motion`: movement outside a lock is
            // not the client's to read.
            return;
        }
        if is_duplicate_raw(state.last_raw, key) {
            state.raw_duplicates += 1;
            return;
        }
        state.last_raw = Some(key);
        let absolute = match state.absolute_devices.iter().find(|(id, _)| *id == source) {
            Some(&(_, a)) => a,
            None => {
                let a = xi.device_is_absolute(self.display, source);
                state.absolute_devices.push((source, a));
                if a {
                    eprintln!(
                        "[intoxicated] X11 pointer lock: device {source} reports absolute positions; \
                         its raw motion is ignored and the camera follows it by warping instead"
                    );
                }
                a
            }
        };
        if absolute {
            state.raw_refused += 1;
            return;
        }
        if !state.raw_relative_seen {
            // From here on core motion is not a measurement. Whatever the
            // warp path read before this first raw sample and has not yet
            // delivered is this same movement seen a second way.
            state.raw_relative_seen = true;
            super::input::drop_pending_warp();
        }
        state.raw_events += 1;
        let anchor = state.centre;
        drop(state);
        let d = choose_camera_delta(current_pointer_acceleration(), acc, unacc);
        super::input::add_locked_motion(anchor, d.0, d.1, true, buttons, down_time, now);
        if super::input::trace_mouse() {
            eprintln!(
                "[intoxicated] X11 raw motion: time={} device={source} accelerated=({:.3}, {:.3}) raw=({:.3}, {:.3}) -> ({:.3}, {:.3})",
                key.0, acc.0, acc.1, unacc.0, unacc.1, d.0, d.1
            );
        }
    }

    /// The end of a drain: hand the engine whatever locked movement is still
    /// waiting (raw and warp-path, as one sum), then put the warp path's
    /// pointer back on its anchor.
    fn flush_locked_motion(&self, handle: i64) {
        super::input::flush_locked_motion(handle);
        let (anchor, warp_back) = {
            let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
            if !state.locked {
                return;
            }
            let anchor = state.centre;
            // Only the warp path warps back per drain. Once raw motion is in
            // charge the hidden pointer is moved only at the edge band, and
            // would otherwise be warped on every drain it drifted.
            let warp_path = !(self.xi.is_some() && state.raw_relative_seen);
            // The owed total was already handed over event by event.
            let (_owed, back) = state.warp.end_of_drain(anchor, warp_path);
            (anchor, back)
        };
        if warp_back {
            // SAFETY: this struct's own live handles.
            unsafe {
                (self.xlib.warp_pointer)(self.display, 0, self.window, 0, 0, 0, 0, anchor.0, anchor.1);
                (self.xlib.flush)(self.display);
            }
        }
    }

    /// A `FocusIn` or `FocusOut`. See [`focus_change_is_real`] for which ones
    /// mean anything.
    fn dispatch_focus(&self, handle: i64, buf: &[u8; 256], focus_in: bool) {
        // SAFETY: `XFocusChangeEvent` is `{type, serial, send_event, display,
        // window, mode, detail}`; `mode` and `detail` sit at 40 and 44 on
        // LP64, where `XInputEvent::root` would be -- which is why the review
        // of the original patch noted this event needs its own view rather than
        // the shared one. Read unaligned since `buf` is a byte array.
        let (mode, detail) = unsafe {
            (
                std::ptr::read_unaligned(buf.as_ptr().add(40) as *const c_int),
                std::ptr::read_unaligned(buf.as_ptr().add(44) as *const c_int),
            )
        };
        // Every focus event, with its mode, under the mouse trace: the review
        // asked for exactly this log to tell one desktop's focus behaviour
        // from another's, and without it "the camera jerks on media keys" is
        // unattributable.
        if super::input::trace_mouse() || super::input::trace_text() {
            eprintln!(
                "[intoxicated] X11 Focus{} mode={mode} detail={detail} ({})",
                if focus_in { "In" } else { "Out" },
                if focus_change_is_real(mode, detail) { "acted on" } else { "a grab; ignored" },
            );
        }
        if focus_change_is_real(mode, detail) {
            self.real_focus_change(handle, focus_in);
        } else if focus_in && mode == NOTIFY_UNGRAB {
            // Another client's keyboard grab has ended and the keyboard is
            // back. Nothing about the lock changes -- it was never released --
            // but anything released *during* the grab went to the grabber, so
            // the engine may still be holding a key or button that is up.
            self.release_what_the_grab_swallowed(handle);
        }
        // `FocusOut` with `NotifyGrab`: deliberately nothing. The window has
        // not lost focus; a key binding fired. Releasing the lock here is what
        // warped the pointer twice per media key.
    }

    /// A real focus change (see [`focus_change_is_real`]), from the X server or
    /// from devctl's `focus` verb: record it, act on a loss, and tell the
    /// engine (`onWindowFocusChangedNative`, which it never heard on X11
    /// before).
    fn real_focus_change(&self, handle: i64, focus_in: bool) {
        // An F11 whose release went to another window must not swallow the
        // next press.
        self.f11_held.store(false, Ordering::Release);
        // The same for the effects hotkey.
        crate::effects_hotkey::focus_changed();
        if focus_in {
            self.focused.store(1, Ordering::Release);
        } else {
            self.focused.store(2, Ordering::Release);
            self.lose_focus(handle);
        }
        super::input::report_window_focus(handle, focus_in);
    }

    /// The window really lost the keyboard: give the pointer back, and tell
    /// the engine every button and key it still believes is down is up.
    fn lose_focus(&self, handle: i64) {
        // Movement read before the focus went is real movement: delivered,
        // then the lock released.
        super::input::flush_locked_motion(handle);
        self.release_pointer_lock();

        let (buttons, (x, y)) = {
            let state = self.input.lock().unwrap_or_else(|e| e.into_inner());
            (state.buttons, state.last_pos)
        };

        // **Releases, not a cleared bitmask.** This used to set `buttons = 0`,
        // which changed the client's own record and told the engine nothing: it
        // had seen the press, never saw a release, and went on treating the
        // right button as held -- a camera drag that would not end, or a
        // button state that disagreed with the client's until the next press.
        // Wayland's `pointer_leave` had the same fault and the same fix.
        for b in held_button_bits(buttons) {
            if super::input::trace_mouse() {
                eprintln!("[intoxicated] X11 focus out holding button {b}; releasing it");
            }
            self.dispatch_button_bit(handle, b, x, y, false);
        }

        // **Let go of every key the engine still thinks is down.**
        // X only sends key events to the focused client, so a key
        // released while another window has focus never produces a
        // KeyRelease here, and the engine goes on walking in the
        // direction it was last told. Issue #41 describes exactly
        // that on X11: movement continuing in the previous
        // direction for about five seconds after the camera's
        // warp/grab cycle, which releases the grab through this
        // arm. The Wayland backend has done this on
        // `wl_keyboard.leave` for some time. A release for a key that is
        // up is harmless, and a missing one is the reported bug's shape.
        // Released with META 0, by the physical code the press went with.
        if !super::input::held_keys().is_empty() {
            super::input::release_all_keys(handle, self.now_ms());
            if super::input::trace_mouse() || super::input::trace_text() {
                eprintln!("[intoxicated] X11 focus out: released keys still held");
            }
        }
    }

    /// After another client's keyboard grab: release whatever went up while it
    /// had the input, asking the server what is physically down rather than
    /// guessing. A key still held through the grab -- W held across a volume
    /// key, the usual case -- is left alone, which is the difference from
    /// treating the grab as a focus loss: that released W and the player
    /// stopped walking with the key still down.
    ///
    /// The keymap is read when this event is processed, which can be after
    /// the grab ended, so a key let go *after* the grab can be released here
    /// and then again by its own `KeyRelease` still queued behind. Seen with
    /// `xdotool key alt+x` against an i3 binding: Alt was released here and
    /// then arrived. A second release of a key that is up is harmless -- the
    /// `FOCUS_OUT` path has always relied on that -- where a missing one is a
    /// key the engine believes is held for good.
    fn release_what_the_grab_swallowed(&self, handle: i64) {
        let mut keymap = [0u8; 32];
        let (mut root, mut child, mut rx, mut ry, mut wx, mut wy, mut mask) =
            (0, 0, 0, 0, 0, 0, 0);
        // SAFETY: this struct's own live display and window; `keymap` is the
        // 32 bytes `XQueryKeymap` writes, and the rest are live locals.
        let pointer_known = unsafe {
            (self.xlib.query_keymap)(self.display, keymap.as_mut_ptr() as *mut c_char);
            (self.xlib.query_pointer)(
                self.display, self.window, &mut root, &mut child, &mut rx, &mut ry, &mut wx,
                &mut wy, &mut mask,
            ) != 0
        };
        let keys_up = keys_released_elsewhere(&super::input::held_keys(), &keymap);
        let (buttons, (x, y)) = {
            let state = self.input.lock().unwrap_or_else(|e| e.into_inner());
            (state.buttons, state.last_pos)
        };
        if pointer_known {
            for b in buttons_released_elsewhere(buttons, mask) {
                self.dispatch_button_bit(handle, b, x, y, false);
            }
        }
        if !keys_up.is_empty() {
            let x_keycodes: Vec<i32> = keys_up.iter().map(|&(_, x_keycode)| x_keycode).collect();
            super::input::release_keys(handle, &x_keycodes, self.now_ms());
            if super::input::trace_mouse() || super::input::trace_text() {
                eprintln!("[intoxicated] X11 keyboard grab ended: released keys let go during it");
            }
        }
    }

    /// One `KeyPress` or `KeyRelease`, from `XNextEvent` or built locally by
    /// devctl's `xkey`. `buf` is the whole `XEvent`.
    ///
    /// Two separate things happen, in this order:
    ///
    /// 1. **The key, to the game**, by physical X keycode. Every keycode from
    ///    8 to 255 is a key (`input::host_key`), whatever the modifiers and
    ///    the layout make of it; auto-repeat presses are dropped there, and a
    ///    release goes out with whatever code its press went out with.
    /// 2. **The text, to a focused text box**, on presses only (repeats
    ///    included): a shortcut, a caret move, an erase or the text the input
    ///    method committed (`input::host_text`).
    ///
    /// Escape is an ordinary key: Roblox opens its menu and drops its own
    /// lock request, and the lock follows on the next pump. F11 is the one
    /// key the client keeps, for fullscreen.
    fn dispatch_key(&self, handle: i64, buf: &mut [u8; 256], down: bool) {
        let mut keysym: c_ulong = 0;
        let mut bytes = [0u8; 16];
        // SAFETY: `buf` holds an XKeyEvent, laid out as `XInputEvent` (Xlib
        // documents the shared prefix). A null compose status skips Xlib's
        // own compose handling, which the input method does instead.
        let n = unsafe {
            (self.xlib.lookup_string)(
                buf.as_mut_ptr() as *mut c_void,
                bytes.as_mut_ptr() as *mut c_char,
                bytes.len() as c_int,
                &mut keysym,
                std::ptr::null_mut(),
            )
        };
        let bytes = &bytes[..(n.max(0) as usize).min(bytes.len())];
        let (keycode, state, time) = {
            // SAFETY: as above.
            let ev = unsafe { &*(buf.as_ptr() as *const XInputEvent) };
            (ev.detail, ev.state, ev.time)
        };
        // Keycode 0 is the input method handing back a composed result: text,
        // and never a key.
        let physical = (8..=255).contains(&keycode);
        let keysym0 = if physical {
            // SAFETY: as above; index 0 is always valid.
            unsafe { (self.xlib.lookup_keysym)(buf.as_mut_ptr() as *mut c_void, 0) }
        } else {
            0
        };

        if physical && (keysym == XK_F11 || keysym0 == XK_F11) {
            // The client's fullscreen toggle, on the press. A held F11 repeats
            // as bare presses (detectable auto-repeat), which toggle nothing.
            // Neither the engine nor a text box ever sees it.
            let was_down = self.f11_held.swap(down, Ordering::AcqRel);
            if down && !was_down {
                self.set_fullscreen(!self.fullscreen.load(Ordering::Acquire));
            }
            return;
        }
        let meta = super::input::meta_for_key(state, keysym0 as u64, down);
        let focus = intoxicated_linker_sys::game_activity::focused_textbox();
        if super::input::trace_text() {
            eprintln!(
                "[intoxicated] key {} keycode={keycode} keysym={keysym:#x} base={keysym0:#x} meta={meta:#x} focus={focus:?}",
                if down { "down" } else { "up" },
            );
        }

        if physical {
            let unicode = keysym_text_fallback(bytes, state, keysym).map_or(0, |c| c as i32);
            super::input::host_key(
                handle,
                &super::input::HostKey {
                    down,
                    x_keycode: keycode,
                    state,
                    keysym0: keysym0 as u64,
                    unicode,
                    time: self.now_ms(),
                },
            );
        }

        if !down {
            return;
        }
        if physical {
            // The timestamp ICCCM wants on a selection claim or conversion;
            // see `x11_clipboard::USER_TIME`.
            super::x11_clipboard::note_user_time(time);
        }
        // Text only for a box the engine focused through `showKeyboard`;
        // handle 0 is not a box and the engine drops text sent to it.
        let Some(which) = focus else {
            return;
        };
        let typed = match &self.xim {
            Some(xim) => {
                if xim.filter(buf.as_mut_ptr() as *mut c_void) {
                    // A dead key, or a Compose sequence in progress or just
                    // finished; a finished one comes back as a keycode-0
                    // press, next in the queue.
                    return;
                }
                xim.committed_text(buf.as_mut_ptr() as *mut c_void)
            }
            None => keysym_text_fallback(bytes, state, keysym).map(String::from).unwrap_or_default(),
        };
        if super::input::trace_text() {
            eprintln!("[intoxicated] key committed {}", super::input::redacted(&typed));
        }
        super::input::host_text(handle, which, keysym as u64, meta, &typed);
    }

    /// Drain and deliver whatever X has queued, without ever blocking.
    pub fn pump_input_events(&self, handle: i64) {
        self.sync_pointer_lock();
        if super::input::keyboard_report_enabled() {
            let (w, h, _) = self.geometry();
            report_keyboard_state((w, h));
        }
        let mut pfd = PollFd { fd: self.conn_fd, events: POLLIN, revents: 0 };
        // SAFETY: one live `pollfd`, and a zero timeout.
        let ready = unsafe { poll(&mut pfd as *mut PollFd as *mut c_void, 1, 0) };
        // **The socket is not the whole queue.** A drain stops at
        // `MAX_EVENTS_PER_DRAIN`, and whatever Xlib had already read past that
        // sits in its own buffer, where `poll` cannot see it -- so the rest of
        // a burst waited for the next unrelated byte on the wire. Raw motion
        // makes bursts routine (a 1000 Hz mouse is a thousand events a second
        // before the core ones), which is what turned this from theoretical
        // into a camera that stutters. `QueuedAlready` (0) counts that buffer
        // without reading anything, so it cannot block.
        // SAFETY: `self.display` is open.
        let buffered = unsafe { (self.xlib.events_queued)(self.display, 0) };
        if ready <= 0 && buffered <= 0 {
            return;
        }

        // **Everything queued, every wake.** This used to stop at 64 events,
        // and whatever Xlib had already read past that waited for the next
        // unrelated byte on the wire or the pump's 50 ms timeout: the tail of
        // a fast mouse's burst (two raw copies and a core event per sample
        // under the grab) arrived up to 50 ms late. The bound below exists
        // only so a flood cannot hold the thread forever; when it is hit, the
        // rest is still in Xlib's queue and `input_backlog` makes the pump's
        // next wait zero.
        const MAX_EVENTS_PER_DRAIN: usize = 4096;
        let mut buf = [0u8; 256];
        for _ in 0..MAX_EVENTS_PER_DRAIN {
            // SAFETY: `self.display` is open. `XPending` flushes and reads
            // what the socket has without blocking.
            if unsafe { (self.xlib.pending)(self.display) } <= 0 {
                break;
            }
            // SAFETY: an event is queued, so this returns at once; `buf` is
            // larger than the `XEvent` union.
            unsafe { (self.xlib.next_event)(self.display, buf.as_mut_ptr() as *mut c_void) };
            super::input::note_x_event_read();
            self.dispatch_event(handle, &mut buf);
        }
        super::input::clear_input_origin();
        self.flush_locked_motion(handle);
        self.sync_pointer_lock();
    }

    /// Whether Xlib holds events already read off the socket, which `poll`
    /// on the socket cannot see: the pump must not sleep on them.
    pub fn input_backlog(&self) -> bool {
        // SAFETY: `self.display` is open; `QueuedAlready` (0) reads nothing.
        unsafe { (self.xlib.events_queued)(self.display, 0) > 0 }
    }

    /// devctl's `focus in|out`: the same handling a real `FocusIn` or
    /// `FocusOut` (mode NotifyNormal) gets. Nothing is sent to the X server.
    pub fn devctl_focus(&self, handle: i64, focus_in: bool) {
        self.real_focus_change(handle, focus_in);
    }

    /// devctl's `xkey`: a `KeyPress` or `KeyRelease` built in this process and
    /// run through the same dispatch as one read off the connection, so the
    /// X11 key path (keysyms, the input method, the held table) can be driven
    /// in game. **Nothing is sent to the X server**: the event exists only in
    /// this process, and `XFilterEvent`'s put-back of a composed result goes
    /// to this client's own queue.
    pub fn devctl_xkey(&self, handle: i64, down: bool, keycode: u32, state: u32) {
        let mut buf = [0u8; 256];
        // SAFETY: the default root of this struct's own open display.
        let root = unsafe { (self.xlib.default_root_window)(self.display) };
        let ev = XInputEvent {
            type_: if down { KEY_PRESS } else { KEY_RELEASE },
            serial: 0,
            send_event: 1,
            display: self.display,
            window: self.window,
            root,
            subwindow: 0,
            time: 0,
            x: 0,
            y: 0,
            x_root: 0,
            y_root: 0,
            state,
            detail: keycode,
            same_screen: 1,
        };
        // SAFETY: `buf` is larger than `XInputEvent`; an unaligned write.
        unsafe { std::ptr::write_unaligned(buf.as_mut_ptr() as *mut XInputEvent, ev) };
        // For the latency trace this is an X event in hand, as if
        // `XNextEvent` had just returned it.
        super::input::note_x_event_read();
        self.dispatch_key(handle, &mut buf, down);
    }

    /// One event, by type.
    fn dispatch_event(&self, handle: i64, buf: &mut [u8; 256]) {
        // SAFETY: every XEvent starts with its `int type`.
        let event_type = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const c_int) };
        match event_type {
            BUTTON_PRESS | BUTTON_RELEASE => {
                // SAFETY: a button event, which has the shared prefix.
                let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const XInputEvent) };
                self.dispatch_button(handle, &ev, event_type == BUTTON_PRESS);
                // A camera button is taken or let go now rather than at
                // the end of the drain: the drag anchor is meant to be
                // where the button went down, and motion read after the
                // release must not turn the camera (it is read unlocked).
                if matches!(ev.detail, 2 | 3) {
                    self.sync_pointer_lock();
                }
            }
            MOTION_NOTIFY => {
                // SAFETY: a motion event, which has the shared prefix.
                let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const XInputEvent) };
                self.dispatch_motion(handle, &ev);
            }
            KEY_PRESS | KEY_RELEASE => {
                self.dispatch_key(handle, buf, event_type == KEY_PRESS);
            }
            FOCUS_IN | FOCUS_OUT => {
                self.dispatch_focus(handle, buf, event_type == FOCUS_IN);
            }
            GENERIC_EVENT => {
                self.dispatch_generic(buf);
            }
            EXPOSE => {
                // `XExposeEvent.count` at 56: how many more Expose follow.
                // Only the last of a batch asks the engine to redraw.
                if is_final_expose(event_int(buf, 56)) {
                    deliver_surface_redraw(handle);
                }
            }
            CONFIGURE_NOTIFY => {
                // `XConfigureEvent.width` and `.height` at 56 and 60.
                self.configured(handle, event_int(buf, 56), event_int(buf, 60));
            }
            SELECTION_REQUEST | SELECTION_CLEAR | SELECTION_NOTIFY => {
                // Another client asking for what Intoxicated copied,
                // taking the clipboard over, or answering a paste. See
                // `x11_clipboard`.
                super::x11_clipboard::handle_event(buf);
            }
            CLIENT_MESSAGE => {
                // `WM_DELETE_WINDOW` arrives as a ClientMessage whose
                // `message_type` is WM_PROTOCOLS and whose first data word
                // is the delete atom -- the same offsets `open` writes by
                // hand. Read unaligned, since `buf` is a byte array.
                let message_type =
                    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(40) as *const c_ulong) };
                let protocol =
                    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(56) as *const c_ulong) };
                if message_type == self.wm_protocols && protocol == self.wm_delete_window {
                    // Recorded, not acted on: the pump reads it and
                    // `INTOXICATED_NO_CLOSE_EXIT` decides whether it ends
                    // the run.
                    if !WINDOW_CLOSED.swap(true, Ordering::AcqRel) {
                        println!("[android] X11: the window manager asked the window to close");
                    }
                }
            }
            _ => {}
        }
    }

    /// A `ConfigureNotify`. Moves, and the repeated configures of a drag,
    /// cost nothing; a new size is the engine's `onSurfaceChanged`, with the
    /// `AConfiguration` screen size following it. `DisplayMetrics` is left
    /// alone (the engine reads it once, before the window exists).
    fn configured(&self, handle: i64, width: c_int, height: c_int) {
        let Some(format) = self.geometry.resize(width, height) else {
            return;
        };
        super::config::set_screen(width, height);
        if let Err(e) = intoxicated_linker_sys::game_activity::surface_resized(handle, format, width, height) {
            crate::android::trace(format_args!("surface resize failed: {e}"));
        }
    }
}

/// The `int` at byte `at` of an event.
fn event_int(buf: &[u8; 256], at: usize) -> c_int {
    let bytes = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
    c_int::from_ne_bytes(bytes)
}

/// Drain and deliver the window's input, if there is a window.
pub fn pump_input_events(handle: i64) {
    if let Some(w) = current() {
        w.pump_input_events(handle);
    }
}

/// Whether X events are waiting in Xlib's queue, for the pump's wait.
pub fn input_backlog() -> bool {
    current().is_some_and(HostWindow::input_backlog)
}

/// Every input to the X11 pointer-lock decision, for the control socket's
/// `pointerlock` verb -- the X11 counterpart of `wayland::pointer_lock_report`,
/// with the same leading fields so one harness reads both.
///
/// `requested` and `confirmed` are the same value here, and that is a fact
/// about X11 rather than a shortcut: `XGrabPointer` either succeeds or returns
/// an error synchronously, so there is no window in which the client has asked
/// and the server has not yet answered. `motion` says which of the two
/// mechanisms this session runs on, and the `raw_*` counters are what tell a
/// lock that is receiving raw events from one that is not -- which a
/// screenshot cannot.
pub(crate) fn pointer_lock_report() -> String {
    let engine = match super::input::engine_wants_pointer_lock() {
        Some(v) => if v { "true" } else { "false" },
        None => "unavailable",
    };
    let Some(w) = current() else {
        return "err no X11 window".into();
    };
    let buttons = w.input.lock().unwrap_or_else(|e| e.into_inner()).buttons;
    let (_, raw_deliveries) = super::input::locked_deliveries();
    let state = w.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
    let motion = match &w.xi {
        Some(xi) => format!("xi{}.{}", xi.version.0, xi.version.1),
        None => "warp".to_string(),
    };
    format!(
        "ok requested={locked} confirmed={locked} focused={} engine={engine} buttons={buttons} \
         motion={motion} anchor={},{} raw_relative={} raw_events={} raw_duplicates={} \
         raw_refused={} raw_deliveries={} accel={}",
        match w.focused() {
            Some(true) => "true",
            Some(false) => "false",
            None => "unknown",
        },
        state.centre.0,
        state.centre.1,
        state.raw_relative_seen,
        state.raw_events,
        state.raw_duplicates,
        state.raw_refused,
        raw_deliveries,
        if current_pointer_acceleration() { "accelerated" } else { "raw" },
        locked = state.locked,
    )
}

/// Whether the window manager has asked this window to close.
pub fn window_closed() -> bool {
    WINDOW_CLOSED.load(Ordering::Acquire)
}

/// `_NET_WM_BYPASS_COMPOSITOR`, only when `INTOXICATED_COMPOSITOR_BYPASS=1`.
///
/// Opt-in because nothing about it has been measured here: it is a hint an
/// Xorg compositor may use to unredirect a fullscreen window, and whether that
/// saves a copy or a frame of latency on any given desktop is unknown. Cleared
/// again on leaving fullscreen, so a compositor that caches the property does
/// not go on treating a windowed client as a scanout candidate.
///
/// SAFETY: `display` and `window` must be live handles from `xlib`.
unsafe fn set_compositor_bypass(xlib: &Xlib, display: Display, window: Window, on: bool) {
    static OPTED_IN: OnceLock<bool> = OnceLock::new();
    if !*OPTED_IN.get_or_init(|| std::env::var("INTOXICATED_COMPOSITOR_BYPASS").as_deref() == Ok("1")) {
        return;
    }
    // SAFETY: `display` and `window` are live handles per this function's own
    // contract, stated above; the rest are ordinary xlib calls on them.
    unsafe {
        let property = (xlib.intern_atom)(display, c"_NET_WM_BYPASS_COMPOSITOR".as_ptr(), 0);
        let cardinal = (xlib.intern_atom)(display, c"CARDINAL".as_ptr(), 0);
        if property == 0 || cardinal == 0 {
            return;
        }
        // Format 32 takes a C `long` per item, whatever the platform's width.
        let value: c_ulong = on as c_ulong;
        (xlib.change_property)(
            display, window, property, cardinal, 32, 0, // PropModeReplace
            (&value as *const c_ulong).cast(), 1,
        );
        (xlib.flush)(display);
    }
}

// The `GameActivity` natives real Android input goes through -- `onTouchEventNative`
// and `onKeyDownNative`/`onKeyUpNative` -- via `intoxicated-linker-sys`'s
// bridge are reached from `input.rs`; this file only reads X and decides.

// ------------------------------------------------------------- buttons

/// The Android button an X button is, for the five a mouse has buttons for:
/// 1 left, 2 middle, 3 right, 8 back, 9 forward. The wheel (4 to 7) and
/// anything else is not a button.
fn android_button(x_button: c_uint) -> Option<i32> {
    match x_button {
        1 => Some(BUTTON_PRIMARY),
        2 => Some(BUTTON_TERTIARY),
        3 => Some(BUTTON_SECONDARY),
        8 => Some(BUTTON_BACK),
        9 => Some(BUTTON_FORWARD),
        _ => None,
    }
}

/// X's wheel buttons as `(hscroll, vscroll)` in detents: 4 up, 5 down,
/// 6 left, 7 right.
fn wheel_detents(x_button: c_uint) -> Option<(f32, f32)> {
    match x_button {
        4 => Some((0.0, 1.0)),
        5 => Some((0.0, -1.0)),
        6 => Some((-1.0, 0.0)),
        7 => Some((1.0, 0.0)),
        _ => None,
    }
}

/// Whether an Expose is the last of its batch (`count` more follow).
fn is_final_expose(count: c_int) -> bool {
    count == 0
}

// ------------------------------------------------------------- the lock

/// Whether the pointer lock should be held, given one pump's inputs, or `None`
/// for "decide nothing".
///
/// Lock when the engine asks (first person, shift lock), while a camera button
/// (right or middle) is held unless `no_drag_lock`, or always with the forced
/// override. Kept apart from the live state so those rules and the focus gate
/// are unit-testable without a live X server, the same reason
/// [`is_final_expose`] and `input.rs`'s helpers take their inputs as values
/// rather than reading global state themselves.
///
/// **There is no Escape latch any more.** This used to take a
/// `previously_suppressed` flag that Escape set, so a focused window could be
/// told to lock by the engine and decline -- and in shift lock or first person
/// the engine never stops asking, so one Escape left the camera dead until the
/// player toggled out of it. A Wayland backend, since removed, dropped the
/// same latch on a measurement that applies here unchanged, since it is a
/// fact about the engine and not the compositor: Escape is Roblox's own menu key, it reaches
/// the engine, and opening the menu drops the engine's request by itself --
/// `engine=true` before, `false` with the menu open, `true` again on closing
/// it, measured 2026-09-04. The latch duplicated a decision the engine already
/// makes, and made it worse.
///
/// **`focused == Some(false)` decides nothing**, as `wayland.rs` does. A real
/// focus loss has already released the lock (the `FOCUS_OUT` arm), and without
/// this gate the very next pump would take it straight back if the engine was
/// still asking -- grabbing the pointer of a window that does not have the
/// keyboard, with the user now typing somewhere else. `None` (no focus event
/// seen yet) behaves as focused, because a window manager that never sends
/// one must not freeze the lock forever.
fn pointer_lock_decision(
    engine_wants: bool,
    buttons: i32,
    no_drag_lock: bool,
    force: bool,
    focused: Option<bool>,
) -> Option<bool> {
    if focused == Some(false) {
        return None;
    }
    let dragging = !no_drag_lock && buttons & (BUTTON_SECONDARY | BUTTON_TERTIARY) != 0;
    Some(engine_wants || dragging || force)
}

// `XFocusChangeEvent.mode` and `.detail`, from X.h.
// `NotifyGrab` (1) is the mode that is deliberately *not* named here: it is the
// one this backend now ignores.
const NOTIFY_NORMAL: c_int = 0;
const NOTIFY_UNGRAB: c_int = 2;
const NOTIFY_WHILE_GRABBED: c_int = 3;
const NOTIFY_INFERIOR: c_int = 2;

/// Whether a `FocusIn`/`FocusOut` is the window actually gaining or losing the
/// keyboard, as opposed to a keyboard grab starting or ending around it.
///
/// Xlib's four modes mean different things and this backend used to act on all
/// of them alike. `NotifyGrab` and `NotifyUngrab` are sent when *some client*
/// activates or releases a keyboard grab -- a window manager's key binding
/// (i3's `$mod` combinations are passive `XGrabKey`s), a media-key daemon, a
/// global hotkey -- and the focused window is told so even though focus never
/// moved: it is the same window before and after, and the keyboard comes back
/// to it when the grab ends. The reported "media keys jerk the camera and spam
/// right-click" was traced to exactly this: each such key ungrabbed, warped
/// the pointer across the screen, re-grabbed and warped again, and zeroed the button state with the right
/// button still down.
///
/// `NotifyNormal` is an ordinary focus change. `NotifyWhileGrabbed` is a focus
/// change that happened *while* a keyboard grab was active -- the window
/// manager moving focus during its own binding, say -- and is just as real: the
/// focus did move, only the moment it was reported at differs. So both count.
///
/// `detail == NotifyInferior` is focus moving between this window and one of
/// its own children, which never leaves the window; the client creates no
/// children, so it is excluded for correctness rather than because it is
/// expected.
fn focus_change_is_real(mode: c_int, detail: c_int) -> bool {
    matches!(mode, NOTIFY_NORMAL | NOTIFY_WHILE_GRABBED) && detail != NOTIFY_INFERIOR
}

/// The Android button bits in `buttons`, one at a time, in the order a
/// synthesised release delivers them -- the same order as Wayland's
/// `release_held_buttons`.
fn held_button_bits(buttons: i32) -> Vec<i32> {
    [BUTTON_PRIMARY, BUTTON_SECONDARY, BUTTON_TERTIARY, BUTTON_BACK, BUTTON_FORWARD]
        .into_iter()
        .filter(|b| buttons & b != 0)
        .collect()
}

/// Of the buttons the client believes are down, those the server says are
/// not -- a release that went to another client's grab rather than to this
/// window.
///
/// `x_state` is the button mask `XQueryPointer` returns (`Button1Mask` is
/// `1 << 8`). The core protocol has masks for buttons 1-5 only, so the side
/// buttons (Android's back/forward, X's 8/9) cannot be checked this way and are
/// left as they are rather than released on a guess.
fn buttons_released_elsewhere(held: i32, x_state: c_uint) -> Vec<i32> {
    const BUTTON1_MASK: c_uint = 1 << 8;
    const BUTTON2_MASK: c_uint = 1 << 9;
    const BUTTON3_MASK: c_uint = 1 << 10;
    [
        (BUTTON_PRIMARY, BUTTON1_MASK),
        (BUTTON_SECONDARY, BUTTON3_MASK),
        (BUTTON_TERTIARY, BUTTON2_MASK),
    ]
    .into_iter()
    .filter(|(android, x)| held & android != 0 && x_state & x == 0)
    .map(|(android, _)| android)
    .collect()
}

/// Of the keys the client believes are down, those `XQueryKeymap` says are
/// up -- released while another client's keyboard grab had the keyboard, so
/// the release went to it and not here. `keymap` is the 256-bit vector
/// indexed by X keycode that `XQueryKeymap` fills.
fn keys_released_elsewhere(held: &[(i32, i32)], keymap: &[u8; 32]) -> Vec<(i32, i32)> {
    held.iter()
        .copied()
        .filter(|&(_, x_keycode)| {
            let k = x_keycode as usize;
            k >= 256 || keymap[k / 8] & (1 << (k % 8)) == 0
        })
        .collect()
}

// A modifier key's own event carries the X state from before it, so Shift's
// press lacks Shift and its release still has it; `input::meta_for_key`
// corrects the key's own bit. Shift lock is the engine reading exactly that.

/// Where a lock holds the pointer, in window coordinates.
///
/// The warp fallback always uses the centre: it measures each movement as a
/// distance from where it put the pointer, and needs room on every side.
/// The raw path measures nothing from the position, so it is free to do what
/// desktop Roblox does with a right-button drag -- leave the cursor where the
/// button went down, so that when the drag ends the engine's cursor is still
/// where the player was pointing rather than jumping to the middle of the
/// window. The engine's own request (first person, shift lock) is
/// `nativeGetMainWindowIsMouseLockedCenter` -- *centre* is in the name -- and
/// gets the centre on both paths, as does the forced override.
fn lock_anchor(
    centred: bool,
    raw_path: bool,
    pointer: Option<(i32, i32)>,
    size: (i32, i32),
) -> (i32, i32) {
    let centre = (size.0 / 2, size.1 / 2);
    match pointer {
        Some((x, y)) if raw_path && !centred => {
            (x.clamp(0, (size.0 - 1).max(0)), y.clamp(0, (size.1 - 1).max(0)))
        }
        _ => centre,
    }
}

/// How close to the window's edge the hidden pointer may drift on the raw path
/// before it is put back at its anchor.
///
/// The raw path does not need the pointer anywhere in particular -- raw
/// motion is reported before the server clamps the pointer to the confining
/// window, so a pointer pinned against an edge still turns the camera. The warp
/// back exists only so the hidden pointer is never left on the boundary of the
/// confinement, where any lapse in it -- an unmap, a resize mid-grab -- puts it
/// straight onto whatever is next to the window. One warp per excursion to the
/// edge, not one per event, and `XWarpPointer` produces no raw event, so it
/// cannot be counted as movement.
const RAW_EDGE_MARGIN: i32 = 16;

fn near_edge(pos: (i32, i32), size: (i32, i32)) -> bool {
    pos.0 < RAW_EDGE_MARGIN
        || pos.1 < RAW_EDGE_MARGIN
        || pos.0 >= size.0 - RAW_EDGE_MARGIN
        || pos.1 >= size.1 - RAW_EDGE_MARGIN
}

/// How many drains a warp may go unseen before it stops being expected.
///
/// A warp onto the pixel the pointer already occupies produces no
/// `MotionNotify` at all, so nothing is guaranteed to retire it. That case is
/// almost always retired anyway, by the ordinary event that put the pointer on
/// the anchor in the first place; this bound is for whatever is left, so that
/// an expectation that will never be met cannot stop the next warp-back for
/// the rest of the lock and leave the hidden pointer pinned to the
/// confinement's edge with the camera stalled. Thirty drains is a fraction of
/// a second at any frame rate this runs at, and a real echo is one round trip.
const WARP_UNSEEN_DRAINS: u8 = 30;

/// Core pointer motion while locked on the warp path, accounted so that every
/// pixel the mouse moved reaches the camera exactly once.
///
/// **Each event is measured from the one before it, never from the anchor.**
/// This path used to read every `MotionNotify` as an offset from the centre
/// it warps the pointer back to, which is only right when every event arrives
/// after the previous warp has landed. A warp is a request: everything the
/// server had already queued for this client when it was processed still
/// carries the pre-warp path, and after any hitch -- or simply at a high
/// frame rate with a fast mouse -- the pump reads dozens of those in one go.
/// Each of them, measured from the centre, counted the earlier part of the
/// same movement again, so the camera turned by a triangular number of the
/// distance the mouse moved, the jump reported after a stall. Each event is
/// therefore measured from the one before it, and the warp-back is batched
/// per drain the way the raw path batches its delivery.
///
/// The warp's own echo is the one event that is not motion: it moves the
/// reference point from wherever the pointer was to the anchor. Echoes are
/// recognised by position, one expected entry per warp issued, so a backlog
/// that straddles two warps (a recentre while one is still in flight) still
/// rebases at the right moment.
///
/// Motion from *before* the lock is not handled here at all. It cannot be told
/// apart from movement by position, and `lock_pointer` removes it from the
/// queue instead; see the `XCheckMaskEvent` loop there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WarpPathMotion {
    /// Where the pointer was as of the last event read, or the last echo.
    last: (i32, i32),
    /// Warps issued and not yet seen land: target, and drains waited.
    expected: Vec<((i32, i32), u8)>,
    /// Movement read this drain and not yet delivered.
    owed: (i32, i32),
}

impl WarpPathMotion {
    /// A lock that has just put the pointer at `at`, with nothing queued
    /// behind it.
    fn engaged(at: (i32, i32)) -> Self {
        Self { last: at, expected: Vec::new(), owed: (0, 0) }
    }

    /// Record a warp to `to`, so its echo is not read as motion.
    fn warped_to(&mut self, to: (i32, i32)) {
        // Bounded by construction -- `end_of_drain` never issues a second
        // warp to a target already expected -- but a recentre storm is not
        // worth trusting that to, and the oldest is the least likely to land.
        if self.expected.len() >= 4 {
            self.expected.remove(0);
        }
        self.expected.push((to, 0));
    }

    /// One core `MotionNotify` at `pos`. Returns the movement it carried, which
    /// is also added to what the drain owes the camera.
    fn motion(&mut self, pos: (i32, i32)) -> (i32, i32) {
        if let Some(i) = self.expected.iter().position(|(to, _)| *to == pos) {
            // The echo of warp `i`. Warps land in the order they were
            // issued, so any expected before it either landed on a pixel the
            // pointer was already on or never will: they are retired with it.
            self.expected.drain(..=i);
            self.last = pos;
            return (0, 0);
        }
        let d = (pos.0 - self.last.0, pos.1 - self.last.1);
        self.last = pos;
        self.owed = (self.owed.0 + d.0, self.owed.1 + d.1);
        d
    }

    /// The end of one drain. Returns what the camera is owed, if anything, and
    /// whether the pointer should now be warped back to `anchor`.
    ///
    /// The live path hands each event's movement to `input` as it is read
    /// (so a click later in the drain is delivered after it), and uses this
    /// only for the warp-back; the owed total is what the tests below check.
    ///
    /// One warp per drain rather than one per event. Warping after every event
    /// of a backlog issued a warp for each, and every one of them was another
    /// echo to tell apart from motion; one per drain is the same recentring
    /// with one expectation in flight. No warp is issued while one to the same
    /// anchor is still expected: unless the mouse moved in between, the second
    /// lands on a pointer the first already put there and echoes nothing,
    /// and its leftover expectation would swallow the next genuine event that
    /// happens to cross the anchor.
    fn end_of_drain(&mut self, anchor: (i32, i32), may_warp: bool) -> (Option<(i32, i32)>, bool) {
        let owed = std::mem::take(&mut self.owed);
        for e in &mut self.expected {
            e.1 = e.1.saturating_add(1);
        }
        self.expected.retain(|&(_, waited)| waited <= WARP_UNSEEN_DRAINS);
        let warp = may_warp
            && self.last != anchor
            && !self.expected.iter().any(|(to, _)| *to == anchor);
        if warp {
            self.warped_to(anchor);
        }
        ((owed != (0, 0)).then_some(owed), warp)
    }
}

/// One mouse button changing, as a step of the Android gesture it belongs to.
///
/// **A gesture is every button held at once, not one button.** Android opens
/// it with `ACTION_DOWN` when the first button goes down and closes it with
/// `ACTION_UP` when the last comes up; a button joining or leaving in between
/// is `ACTION_BUTTON_PRESS` or `ACTION_BUTTON_RELEASE` and nothing more. This
/// backend used to send the whole DOWN/PRESS or RELEASE/UP pair for every
/// button, so a right-button camera drag under a held left button restarted
/// the gesture, and letting the right one go sent `ACTION_UP` with the left
/// one still down: the engine took the left button as released and heard
/// nothing more of it until the real release, which is a held attack or tool
/// stopping the moment the camera is turned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ButtonStep {
    button: i32,
    press: bool,
    /// The held set once this step is applied.
    buttons: i32,
    /// No button was down before this press: a new gesture, and a new
    /// `downTime`.
    starts_gesture: bool,
    /// No button is down after this release.
    ends_gesture: bool,
}

impl ButtonStep {
    /// `None` when the change is not a change: a release of a button not held,
    /// or a press of one already held. The second is not something X sends
    /// for one device, but a missed release (another client's grab, before
    /// `release_what_the_grab_swallowed` reconciles it) followed by a press
    /// would otherwise open a second gesture inside the first.
    fn new(held: i32, button: i32, press: bool) -> Option<Self> {
        let was_held = held & button != 0;
        if press == was_held {
            return None;
        }
        let buttons = if press { held | button } else { held & !button };
        Some(Self {
            button,
            press,
            buttons,
            starts_gesture: press && held == 0,
            ends_gesture: !press && buttons == 0,
        })
    }

    /// The AGDK `MotionEvent`s this step is, in delivery order, as
    /// `(action, buttonState, actionButton)`. Every one carries the held set
    /// as it stands after the step, which is what Android reports: a press
    /// already includes its own button and a release already excludes it.
    fn agdk_actions(&self) -> Vec<(i32, i32, i32)> {
        let mut out = Vec::with_capacity(2);
        if self.press {
            if self.starts_gesture {
                out.push((ACTION_DOWN, self.buttons, 0));
            }
            out.push((ACTION_BUTTON_PRESS, self.buttons, self.button));
        } else {
            out.push((ACTION_BUTTON_RELEASE, self.buttons, self.button));
            if self.ends_gesture {
                out.push((ACTION_UP, self.buttons, 0));
            }
        }
        out
    }
}

// The `NativeInputInterface` natives, the text field and the key rules are in
// `input.rs`; `dispatch_key`, above, hands it what X reported.

/// The character a key typed, from its keysym, or `None`. The text path's
/// source when there is no input method, and AGDK's `unicodeChar` always.
///
/// **The keysym, not Xlib's bytes.** `XLookupString` returns Latin-1 in the C
/// locale, so `é` came back as the single byte 0xE9, which is not UTF-8, and
/// the letter was lost; a Cyrillic letter came back as no bytes at all. The
/// keysym names the character on every layout and in every locale. The bytes
/// still tell a control character (Ctrl+A is 0x01 with keysym `a`) from a
/// printable one, and Control or Alt held types nothing.
fn keysym_text_fallback(bytes: &[u8], x11_state: c_uint, keysym: c_ulong) -> Option<char> {
    if x11_state & (CONTROL_MASK | MOD1_MASK) != 0 {
        return None;
    }
    if bytes.first().is_some_and(|&b| b < 0x20 || b == 0x7f) {
        return None;
    }
    super::input::keysym_to_char(keysym as u64).filter(|c| !c.is_control())
}
