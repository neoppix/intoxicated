//! A host window, and `ANativeWindow_*` over it.
//!
//! Android hands the engine an `ANativeWindow` and it renders into that. On
//! Linux the equivalent is a window-system surface, so this creates one and
//! implements the ten `ANativeWindow_*` entry points Roblox imports against it.
//!
//! X11 is loaded with `dlopen` rather than linked. Cordial has to run its loader
//! and asset tests on machines with no display at all — CI, containers, a remote
//! shell — and a link-time dependency would make the whole binary refuse to
//! start there. Loading late means "no window" is a runtime condition the caller
//! can handle, which is what it actually is.
//!
//! Wayland is the better long-term target and Roblox's Android build has no
//! opinion either way. X11 first because `eglCreateWindowSurface` takes an
//! `xcb_window_t`/`Window` directly, whereas Wayland needs an `wl_egl_window`
//! and a surface role — more moving parts for the same first frame.

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Android pixel formats, from `android/native_window.h`.
pub const WINDOW_FORMAT_RGBA_8888: i32 = 1;

/// KeyPressMask | KeyReleaseMask | ButtonPressMask | ButtonReleaseMask |
/// PointerMotionMask | ExposureMask | StructureNotifyMask | FocusChangeMask,
/// from X.h.
/// ExposureMask is what makes a damaged window (uncovered, restored,
/// redirected through a compositor) generate `Expose`, which
/// `pump_input_events` turns into `onSurfaceRedrawNeededNative` — without it
/// the window never asked to be told, and a damaged window just stayed
/// damaged until the engine's own next frame.
///
/// Module-level rather than local to `open()` so the redraw wiring can be
/// checked (`EXPOSURE_MASK` bit present) without a live X server.
/// StructureNotifyMask (0x20000) is included so `ConfigureNotify` arrives when
/// the window is resized. Without it Cordial never learned its own window had
/// changed size: the engine kept rendering at the size it was told at startup
/// while X cleared the window to its background colour, which is the black
/// flash on every resize.
const INPUT_EVENT_MASK: c_long =
    0x1 | 0x2 | 0x4 | 0x8 | 0x40 | 0x8000 | 0x20000 | 0x200000;

type Display = *mut c_void;
type Window = c_ulong;

struct Xlib {
    open_display: unsafe extern "C" fn(*const c_char) -> Display,
    default_root_window: unsafe extern "C" fn(Display) -> Window,
    create_simple_window: unsafe extern "C" fn(
        Display, Window, c_int, c_int, u32, u32, u32, c_ulong, c_ulong,
    ) -> Window,
    map_window: unsafe extern "C" fn(Display, Window) -> c_int,
    set_wm_normal_hints: unsafe extern "C" fn(Display, Window, *mut XSizeHints),
    set_class_hint: unsafe extern "C" fn(Display, Window, *mut XClassHint) -> c_int,
    set_wm_hints: unsafe extern "C" fn(Display, Window, *mut XWMHints) -> c_int,
    move_window: unsafe extern "C" fn(Display, Window, c_int, c_int) -> c_int,
    intern_atom: unsafe extern "C" fn(Display, *const c_char, c_int) -> c_ulong,
    set_wm_protocols: unsafe extern "C" fn(Display, Window, *mut c_ulong, c_int) -> c_int,
    change_property: unsafe extern "C" fn(
        Display, Window, c_ulong, c_ulong, c_int, c_int, *const u8, c_int,
    ) -> c_int,
    send_event: unsafe extern "C" fn(Display, Window, c_int, c_long, *mut c_void) -> c_int,
    sync: unsafe extern "C" fn(Display, c_int) -> c_int,
    store_name: unsafe extern "C" fn(Display, Window, *const c_char) -> c_int,
    flush: unsafe extern "C" fn(Display) -> c_int,
    destroy_window: unsafe extern "C" fn(Display, Window) -> c_int,
    // ---- input, added for keyboard/mouse delivery ----
    select_input: unsafe extern "C" fn(Display, Window, c_long),
    connection_number: unsafe extern "C" fn(Display) -> c_int,
    pending: unsafe extern "C" fn(Display) -> c_int,
    next_event: unsafe extern "C" fn(Display, *mut c_void) -> c_int,

    grab_pointer: unsafe extern "C" fn(
        Display, Window, c_int, c_uint, c_int, c_int, Window, c_ulong, c_ulong,
    ) -> c_int,
    ungrab_pointer: unsafe extern "C" fn(Display, c_ulong) -> c_int,
    warp_pointer: unsafe extern "C" fn(
        Display, Window, Window, c_int, c_int, c_uint, c_uint, c_int, c_int,
    ) -> c_int,
    query_pointer: unsafe extern "C" fn(
        Display, Window,
        *mut Window, *mut Window,
        *mut c_int, *mut c_int,
        *mut c_int, *mut c_int,
        *mut c_uint,
    ) -> c_int,

    /// `XLookupString` doubles as the keysym lookup and the ASCII/Latin-1 text
    /// lookup, and — unlike `XKeycodeToKeysym` — takes the event's `state` into
    /// account, so Shift and the rest of the modifier state do not have to be
    /// reimplemented by hand.
    lookup_string:
        unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut c_ulong, *mut c_void) -> c_int,
    // ---- cursor, so the host pointer does not double the engine's own ----
    create_bitmap_from_data:
        unsafe extern "C" fn(Display, Window, *const c_char, c_uint, c_uint) -> c_ulong,
    create_pixmap_cursor: unsafe extern "C" fn(
        Display, c_ulong, c_ulong, *mut XColor, *mut XColor, c_uint, c_uint,
    ) -> c_ulong,
    define_cursor: unsafe extern "C" fn(Display, Window, c_ulong) -> c_int,
    free_pixmap: unsafe extern "C" fn(Display, c_ulong) -> c_int,
    // ---- focus and grab bookkeeping ----
    /// `XQueryKeymap`: which keys are physically down right now, as a 256-bit
    /// vector indexed by X keycode. Read when another client's keyboard grab
    /// ends, so a key released while the grab had the keyboard is let go of
    /// here too -- see the `FOCUS_IN` arm.
    query_keymap: unsafe extern "C" fn(Display, *mut c_char) -> c_int,
    /// `XQueryExtension`, for the XInputExtension's major opcode: a
    /// `GenericEvent` names its extension by opcode and nothing else.
    query_extension: unsafe extern "C" fn(
        Display, *const c_char, *mut c_int, *mut c_int, *mut c_int,
    ) -> c_int,
    /// `XEventsQueued`, only ever with `QueuedAlready`: events Xlib has
    /// already read off the socket, counted without any I/O. See
    /// `pump_input_events` for why the socket alone is not enough to ask.
    events_queued: unsafe extern "C" fn(Display, c_int) -> c_int,
    /// `XkbSetDetectableAutoRepeat`: ask the server to signal keyboard
    /// auto-repeat as bare repeated `KeyPress` events with *no* synthetic
    /// `KeyRelease` between them. Without it, holding a key (walking, holding E
    /// to interact) makes the server emit a `KeyRelease`+`KeyPress` pair for
    /// every repeat tick, which `dispatch_key` forwarded as up/down/up/down --
    /// the engine saw the key let go and re-pressed dozens of times a second,
    /// so a held key "cancelled and spammed" after the repeat delay. Called
    /// once in `open`; the matching keycode `detail` lets `dispatch_key` also
    /// drop the now-redundant repeat *presses* for game input while text still
    /// repeats. `XkbIgnoreExtension`-safe: present in every libX11 since 1996.
    set_detectable_auto_repeat: unsafe extern "C" fn(Display, c_int, *mut c_int) -> c_int,
}

/// `XColor`. Only the pixel/RGB prefix is read by `XCreatePixmapCursor`, but the
/// whole struct has to be the right size because Xlib writes through the pointer.
#[repr(C)]
struct XColor {
    pixel: c_ulong,
    red: u16,
    green: u16,
    blue: u16,
    flags: c_char,
    pad: c_char,
}

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;

impl Xlib {
    fn load() -> Result<Self, String> {
        // SAFETY: a literal soname; the handle is never closed.
        let lib = unsafe { dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err("libX11.so.6 is not available".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                let name = CString::new($name).unwrap();
                // SAFETY: the handle is open and the names are Xlib's documented
                // exports, so the signatures are the ones declared above.
                let p = unsafe { dlsym(lib, name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("libX11 has no {}", $name));
                }
                unsafe { std::mem::transmute(p) }
            }};
        }
        Ok(Xlib {
            open_display: sym!("XOpenDisplay"),
            default_root_window: sym!("XDefaultRootWindow"),
            create_simple_window: sym!("XCreateSimpleWindow"),
            map_window: sym!("XMapWindow"),
            store_name: sym!("XStoreName"),
            flush: sym!("XFlush"),
            destroy_window: sym!("XDestroyWindow"),
            select_input: sym!("XSelectInput"),
            set_wm_normal_hints: sym!("XSetWMNormalHints"),
            set_class_hint: sym!("XSetClassHint"),
            set_wm_hints: sym!("XSetWMHints"),
            move_window: sym!("XMoveWindow"),
            intern_atom: sym!("XInternAtom"),
            set_wm_protocols: sym!("XSetWMProtocols"),
            change_property: sym!("XChangeProperty"),
            send_event: sym!("XSendEvent"),
            sync: sym!("XSync"),
            connection_number: sym!("XConnectionNumber"),
            pending: sym!("XPending"),
            next_event: sym!("XNextEvent"),
            grab_pointer: sym!("XGrabPointer"),
            ungrab_pointer: sym!("XUngrabPointer"),
            warp_pointer: sym!("XWarpPointer"),
            query_pointer: sym!("XQueryPointer"),
            lookup_string: sym!("XLookupString"),
            create_bitmap_from_data: sym!("XCreateBitmapFromData"),
            create_pixmap_cursor: sym!("XCreatePixmapCursor"),
            define_cursor: sym!("XDefineCursor"),
            free_pixmap: sym!("XFreePixmap"),
            query_keymap: sym!("XQueryKeymap"),
            query_extension: sym!("XQueryExtension"),
            events_queued: sym!("XEventsQueued"),
            set_detectable_auto_repeat: sym!("XkbSetDetectableAutoRepeat"),
        })
    }
}

/// XInput2, for unaccelerated relative motion while the pointer is locked
/// (ADR-028).
///
/// **Core X11 cannot say what the mouse did, only where the server put the
/// pointer afterwards.** A `MotionNotify` position has already been through the
/// server's acceleration curve, so the warp-to-centre lock that preceded this
/// could only ever hand the camera an accelerated delta, whatever
/// `CORDIAL_POINTER_ACCEL` or the settings row said. `XI_RawMotion` carries
/// both numbers -- `raw_values` before acceleration and `valuators` after --
/// which is exactly the pair `zwp_relative_pointer_v1` hands the Wayland
/// backend, so the two backends now make the same choice from the same kind
/// of source.
///
/// Loaded by `dlopen` like libX11 and for the same reason: a missing libXi is
/// a runtime condition, not a link failure. The soname is `libXi.so.6` on
/// every system this has to run on -- Rocky's `/usr/lib64` and FreeBSD's
/// `/usr/local/lib` both ship it under that name, the same way both ship
/// `libX11.so.6`, which this file already opens by bare soname.
/// `XGetEventData`/`XFreeEventData` are libX11's (1.3 and later), resolved
/// here rather than in [`Xlib`] so a libX11 too old to have them costs the raw
/// path and not the whole window.
struct Xi {
    /// The XInputExtension's major opcode; a `GenericEvent` carries it in
    /// `extension` and nothing else identifies whose event it is.
    opcode: c_int,
    /// The version the server agreed to, for the startup line and the
    /// `pointerlock` report.
    version: (c_int, c_int),
    select_events: unsafe extern "C" fn(Display, Window, *mut XIEventMask, c_int) -> c_int,
    query_device: unsafe extern "C" fn(Display, c_int, *mut c_int) -> *mut XIDeviceInfo,
    free_device_info: unsafe extern "C" fn(*mut XIDeviceInfo),
    get_event_data: unsafe extern "C" fn(Display, *mut XGenericEventCookie) -> c_int,
    free_event_data: unsafe extern "C" fn(Display, *mut XGenericEventCookie),
}

/// `XIEventMask`, from XI2.h.
#[repr(C)]
struct XIEventMask {
    deviceid: c_int,
    mask_len: c_int,
    mask: *mut u8,
}

/// `XGenericEventCookie`, from Xlib.h. The `data` pointer is valid only
/// between `XGetEventData` and `XFreeEventData` -- nothing in the type says
/// so, which is the trap ADR-028 names, and why [`HostWindow::dispatch_generic`]
/// copies everything it needs out before freeing.
#[repr(C)]
struct XGenericEventCookie {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: Display,
    extension: c_int,
    evtype: c_int,
    cookie: c_uint,
    data: *mut c_void,
}

/// `XIRawEvent`, from XI2.h. `valuators` is the post-acceleration set and
/// `raw_values` the device's own report; both are packed, one double per bit
/// set in `valuators.mask`, which is what [`raw_motion_axes`] unpacks.
#[repr(C)]
struct XIRawEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: Display,
    extension: c_int,
    evtype: c_int,
    time: c_ulong,
    deviceid: c_int,
    sourceid: c_int,
    detail: c_int,
    flags: c_int,
    valuators_mask_len: c_int,
    valuators_mask: *const u8,
    valuators_values: *const f64,
    raw_values: *const f64,
}

/// `XIDeviceInfo`, from XI2.h -- read only to find out whether a device's
/// first valuator is relative or absolute.
#[repr(C)]
struct XIDeviceInfo {
    deviceid: c_int,
    name: *mut c_char,
    use_: c_int,
    attachment: c_int,
    enabled: c_int,
    num_classes: c_int,
    classes: *mut *mut XIValuatorClassInfo,
}

/// `XIValuatorClassInfo`; its leading `type` is shared with every other class
/// (`XIAnyClassInfo`), so it is safe to read through this for any class and
/// check `type_` first.
#[repr(C)]
struct XIValuatorClassInfo {
    type_: c_int,
    sourceid: c_int,
    number: c_int,
    label: c_ulong,
    min: f64,
    max: f64,
    value: f64,
    resolution: c_int,
    mode: c_int,
}

const GENERIC_EVENT: c_int = 35;
const XI_RAW_MOTION: c_int = 17;
const XI_ALL_MASTER_DEVICES: c_int = 1;
const XI_VALUATOR_CLASS: c_int = 2;
const XI_MODE_ABSOLUTE: c_int = 1;

impl Xi {
    /// Negotiate XI2 on `display`, or say why not. `CORDIAL_NO_XI2=1` refuses
    /// on purpose: ADR-028 requires the warp fallback to be exercised "with XI2
    /// forced off, in the same session", and a switch is the only way to do
    /// that on a server that has XI2.
    fn init(xlib: &Xlib, display: Display) -> Result<Self, String> {
        if std::env::var_os("CORDIAL_NO_XI2").is_some() {
            return Err("turned off by CORDIAL_NO_XI2".into());
        }
        // SAFETY: literal sonames; handles are never closed. dlopen of an
        // already-loaded libX11 returns the existing handle.
        let (xi_lib, x11_lib) = unsafe {
            (dlopen(c"libXi.so.6".as_ptr(), RTLD_NOW), dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW))
        };
        if xi_lib.is_null() {
            return Err("libXi.so.6 is not available".into());
        }
        if x11_lib.is_null() {
            return Err("libX11.so.6 could not be reopened".into());
        }
        macro_rules! sym {
            ($lib:expr, $name:literal) => {{
                let name = CString::new($name).unwrap();
                // SAFETY: the handle is open and the names are documented
                // exports whose signatures are the ones declared on `Xi`.
                let p = unsafe { dlsym($lib, name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("no {} exported", $name));
                }
                unsafe { std::mem::transmute(p) }
            }};
        }
        let query_version: unsafe extern "C" fn(Display, *mut c_int, *mut c_int) -> c_int =
            sym!(xi_lib, "XIQueryVersion");
        let (mut opcode, mut first_event, mut first_error) = (0, 0, 0);
        // SAFETY: `display` is open; the out-pointers are live locals.
        let present = unsafe {
            (xlib.query_extension)(
                display,
                c"XInputExtension".as_ptr(),
                &mut opcode,
                &mut first_event,
                &mut first_error,
            )
        };
        if present == 0 {
            return Err("the X server has no XInputExtension".into());
        }
        // 2.2 asked for, 2.0 required: raw events are 2.0, and asking for
        // more costs nothing on a server that has it while leaving touch
        // (2.2) negotiable later without a second handshake.
        let (mut major, mut minor) = (2, 2);
        // SAFETY: as above.
        let status = unsafe { query_version(display, &mut major, &mut minor) };
        if status != 0 || major < 2 {
            return Err(format!("the server offers XInput {major}.{minor}, and 2.0 is needed"));
        }
        Ok(Xi {
            opcode,
            version: (major, minor),
            select_events: sym!(xi_lib, "XISelectEvents"),
            query_device: sym!(xi_lib, "XIQueryDevice"),
            free_device_info: sym!(xi_lib, "XIFreeDeviceInfo"),
            get_event_data: sym!(x11_lib, "XGetEventData"),
            free_event_data: sym!(x11_lib, "XFreeEventData"),
        })
    }

    /// Subscribe to raw motion on the root window, or (`on == false`) stop.
    ///
    /// **Only while the lock is held.** Raw events on the root window report
    /// the mouse whatever has focus, so a standing subscription would have
    /// Cordial receiving the user's pointer movement across their whole
    /// session -- ADR-028 calls that a privacy failure, not merely a bug.
    /// Selecting on lock and deselecting on release means the server does not
    /// send them at all otherwise; `dispatch_generic` additionally drops any
    /// that were already in flight when the lock went.
    fn select_raw_motion(&self, display: Display, root: Window, on: bool) {
        let mut bits = [0u8; 4];
        if on {
            bits[(XI_RAW_MOTION >> 3) as usize] |= 1 << (XI_RAW_MOTION & 7);
        }
        let mut mask = XIEventMask {
            deviceid: XI_ALL_MASTER_DEVICES,
            mask_len: bits.len() as c_int,
            mask: bits.as_mut_ptr(),
        };
        // SAFETY: `mask` and `bits` outlive the call, which copies them into
        // the request.
        unsafe { (self.select_events)(display, root, &mut mask, 1) };
    }

    /// Whether `deviceid`'s first valuator is absolute -- a tablet, or the
    /// "tablet" pointer most virtual machines present. Such a device's raw
    /// values are positions, not movements, and reading them as deltas would
    /// spin the camera by the pointer's distance from the origin every event.
    fn device_is_absolute(&self, display: Display, deviceid: c_int) -> bool {
        let mut n = 0;
        // SAFETY: `display` is open; the returned array is `n` long and freed
        // below, and each class is read only after checking its shared `type`.
        unsafe {
            let info = (self.query_device)(display, deviceid, &mut n);
            if info.is_null() {
                return false;
            }
            let mut absolute = false;
            if n > 0 {
                let dev = &*info;
                for i in 0..dev.num_classes.max(0) as usize {
                    let class = *dev.classes.add(i);
                    if class.is_null() || (*class).type_ != XI_VALUATOR_CLASS {
                        continue;
                    }
                    if (*class).number == 0 {
                        absolute = (*class).mode == XI_MODE_ABSOLUTE;
                        break;
                    }
                }
            }
            (self.free_device_info)(info);
            absolute
        }
    }
}

/// The X and Y movement in one `XI_RawMotion`, as `(accelerated, raw)`.
///
/// Both arrays are packed: there is one value per bit set in `mask`, in bit
/// order, so the value for valuator 1 is at index 1 only if valuator 0 also
/// moved. A purely vertical movement sets bit 1 alone and its Y is at index 0
/// -- which is exactly the mistake an unpacked read makes, sending vertical
/// motion to the horizontal axis. Valuators above 1 (a wheel exposed as an
/// axis, a tablet's pressure) are skipped, not misread as motion.
fn raw_motion_axes(mask: &[u8], accelerated: &[f64], raw: &[f64]) -> ((f64, f64), (f64, f64)) {
    let (mut acc, mut unacc) = ((0.0, 0.0), (0.0, 0.0));
    let mut packed = 0;
    for axis in 0..mask.len() * 8 {
        if mask[axis / 8] & (1 << (axis % 8)) == 0 {
            continue;
        }
        let (a, r) = (
            accelerated.get(packed).copied().unwrap_or(0.0),
            raw.get(packed).copied().unwrap_or(0.0),
        );
        match axis {
            0 => {
                acc.0 = a;
                unacc.0 = r;
            }
            1 => {
                acc.1 = a;
                unacc.1 = r;
            }
            _ => {}
        }
        packed += 1;
    }
    (acc, unacc)
}

/// Which of the two deltas the camera gets, the same choice and the same
/// setting as Wayland's `relative_pointer_motion`: `accelerated` is
/// `CORDIAL_POINTER_ACCEL` (or the live settings row) not saying "unlocked".
fn choose_camera_delta(accelerated: bool, acc: (f64, f64), raw: (f64, f64)) -> (f64, f64) {
    if accelerated {
        acc
    } else {
        raw
    }
}

/// What identifies one physical raw sample: server time, master and source
/// device, and the unaccelerated X and Y. See [`is_duplicate_raw`].
type RawSampleKey = (c_ulong, c_int, c_int, f64, f64);

/// Whether `cur` is the same raw sample as `prev`, delivered a second time.
///
/// **While this client holds a pointer grab, the X server delivers each
/// `XI_RawMotion` twice** -- once through the grab and once through the root
/// window selection. Measured on Xvfb (FreeBSD xorg-vfbserver), driving the
/// XTEST pointer with `xdotool mousemove_relative -- 7 -3`: with no grab,
/// `xinput test-xi2 --root` shows one RawMotion per move; under Cordial's grab
/// the trace showed two per move with identical serial, time, device, source
/// and values (`serial=4078 time=114255876 device=2 source=4`, twice). Summed
/// unfiltered, that is a camera turning at exactly double the mouse, which is
/// precisely "does not feel normal". SDL drops the same duplicate by the same
/// test, time plus values.
///
/// The cost is that two *genuine* samples from one device inside the same
/// millisecond with bit-identical deltas would lose one of them: one count of
/// a 1000 Hz mouse, never more than one in a row.
fn is_duplicate_raw(prev: Option<RawSampleKey>, cur: RawSampleKey) -> bool {
    prev == Some(cur)
}

/// Raw movement summed across one drain of the event queue, delivered to the
/// engine once at the end of it rather than per event.
///
/// One call per drain rather than per event because a 1000 Hz mouse produces
/// a thousand `XI_RawMotion` a second, each a JNI round trip into the engine,
/// while the engine samples the delta once a frame anyway. Summing is
/// lossless for a camera -- rotation is linear in the delta -- and the
/// fractional parts of high-resolution devices are kept rather than rounded
/// away per event, which is what an integer warp delta could not do.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct RawMotionAccumulator {
    dx: f64,
    dy: f64,
    events: u32,
}

impl RawMotionAccumulator {
    fn add(&mut self, d: (f64, f64)) {
        self.dx += d.0;
        self.dy += d.1;
        self.events += 1;
    }

    /// The total so far, leaving the accumulator empty. `None` when nothing
    /// moved, so a drain of zero-length samples does not wake the engine.
    fn take(&mut self) -> Option<(f64, f64)> {
        let out = (self.dx, self.dy);
        *self = Self::default();
        (out != (0.0, 0.0)).then_some(out)
    }
}

/// A mapped host window and the Android-side state the engine queries about it.
pub struct HostWindow {
    xlib: Xlib,
    display: Display,
    window: Window,
    /// `XConnectionNumber(display)` — the socket Xlib reads the wire protocol
    /// from. Polling this with a zero timeout is what lets input delivery avoid
    /// ever calling into Xlib when there is nothing queued, which is what keeps
    /// it from blocking the render loop (see `pump_input_events`, below).
    conn_fd: c_int,
    /// The two atoms a window manager's close request is spelled in, kept so
    /// the event pump can recognise one without a round trip to the server.
    wm_protocols: c_ulong,
    wm_delete_window: c_ulong,
    /// Dimensions the engine asked for via `ANativeWindow_setBuffersGeometry`,
    /// which override the window's own size in every query. Android reports the
    /// buffer geometry, not the surface geometry, and the engine sizes its
    /// framebuffers from the answer.
    buffers: Mutex<Geometry>,
    input: Mutex<InputState>,
    pointer_lock: Mutex<PointerLockState>,
    fullscreen: AtomicBool,
    /// XInput2 when the server negotiated it, `None` for the warp fallback.
    /// See [`Xi`] and ADR-028.
    xi: Option<Xi>,
    /// Keyboard focus as the last *real* `FocusIn`/`FocusOut` left it: 0 not
    /// yet known, 1 focused, 2 not. See [`focus_change_is_real`] for which
    /// focus events count, and `sync_pointer_lock` for what reads it.
    focused: std::sync::atomic::AtomicU8,
}

/// Buttons and timing carried across calls to `pump_input_events`, the way a
/// real `InputDevice` accumulates gesture state between individual X11 events.
struct InputState {
    /// Android `MotionEvent.BUTTON_*` bits currently held down.
    buttons: i32,
    /// `uptimeMillis()` of the button that started the current gesture — reset
    /// to the current time whenever `buttons` goes from zero to non-zero, and
    /// left alone until it goes back to zero. Android's own `downTime` has this
    /// exact meaning: constant across a MOVE/UP sequence, not per-event.
    down_time_ms: i64,
    clock: std::time::Instant,
    /// Keys the engine has been told are down and not yet told are up, as
    /// (Android keycode, X keycode). See the `FOCUS_OUT` arm.
    held_keys: Vec<(i32, i32)>,
    /// Where the pointer last was in window coordinates, from the last core
    /// button or motion event. A button release Cordial has to synthesise --
    /// on a real focus loss, or after another client's grab swallowed the
    /// real one -- has no event of its own to take a position from.
    last_pos: (f32, f32),
}

/// Record that `key` went down or up, so a focus loss can release whatever is
/// still down. A repeat of a key already held is not held twice: auto-repeat
/// sends KeyPress over and over, and one release per press would send the
/// engine releases for a key it was only ever pressed once.
fn track_held_key(held: &mut Vec<(i32, i32)>, down: bool, key: (i32, i32)) {
    if down {
        if !held.contains(&key) {
            held.push(key);
        }
    } else {
        held.retain(|k| *k != key);
    }
}

/// X11 pointer capture state.
///
/// The lock is an `XGrabPointer` confined to this window either way. Where the
/// camera's movement comes from depends on the server: `XI_RawMotion` when
/// XInput2 is there ([`Xi`], ADR-028), and otherwise the original mechanism --
/// warp the pointer to a fixed centre and read each core `MotionNotify` as a
/// distance from it.
struct PointerLockState {
    locked: bool,
    ignore_next_warp: bool,
    /// Consecutive locked `MotionNotify` events discarded while waiting for
    /// the confirmed echo of the last recentring warp. See
    /// [`locked_pointer_delta`] for why a single check on the very next event
    /// was not enough, and [`MAX_WARP_ECHO_WAIT`] for the bound.
    warp_echo_wait: u8,
    /// Where the hidden pointer is held while locked, in window coordinates,
    /// and the absolute position the engine is told throughout. The window's
    /// centre for the warp fallback and whenever the engine itself asked for a
    /// centred lock; on the raw path, a camera drag is held where the button
    /// went down instead -- see `lock_pointer`.
    centre: (i32, i32),
    saved_root: Option<(i32, i32)>,
    /// Raw movement not yet handed to the engine. See
    /// [`RawMotionAccumulator`].
    raw: RawMotionAccumulator,
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
    /// events taken, raw events refused as absolute, and drains delivered.
    raw_events: u64,
    raw_refused: u64,
    raw_deliveries: u64,
    /// The last raw sample taken, and how many second deliveries of one were
    /// dropped. See [`is_duplicate_raw`].
    last_raw: Option<RawSampleKey>,
    raw_duplicates: u64,
}

impl PointerLockState {
    fn new() -> Self {
        Self {
            locked: false,
            ignore_next_warp: false,
            warp_echo_wait: 0,
            centre: (0, 0),
            saved_root: None,
            raw: RawMotionAccumulator::default(),
            raw_relative_seen: false,
            absolute_devices: Vec::new(),
            raw_events: 0,
            raw_refused: 0,
            raw_deliveries: 0,
            last_raw: None,
            raw_duplicates: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    width: i32,
    height: i32,
    format: i32,
}

// The window lives for the process and X11 calls are serialised by the caller.
unsafe impl Send for HostWindow {}
unsafe impl Sync for HostWindow {}

static WINDOW: OnceLock<HostWindow> = OnceLock::new();

/// Set when the window manager delivers `WM_DELETE_WINDOW`, and only read by
/// `looper::asked_to_stop` through `android::window_closed`. A flag rather
/// than a call into the pump, so `CORDIAL_NO_CLOSE_EXIT` gates an X11 close
/// exactly as it gates a Wayland one.
static WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);


/// `XSizeHints`. Only the leading fields matter here, but the struct has to be
/// the full size Xlib expects or `XSetWMNormalHints` reads past the end.
#[repr(C)]
struct XSizeHints {
    flags: c_long,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    min_width: c_int,
    min_height: c_int,
    max_width: c_int,
    max_height: c_int,
    width_inc: c_int,
    height_inc: c_int,
    min_aspect_x: c_int,
    min_aspect_y: c_int,
    max_aspect_x: c_int,
    max_aspect_y: c_int,
    base_width: c_int,
    base_height: c_int,
    win_gravity: c_int,
}

/// `WM_CLASS`, whose second element must match `StartupWMClass` in
/// `packaging/io.github.luohoa97.Cordial.desktop`. A mismatch is invisible in normal
/// use and shows up as an unnamed window in OBS and portal capture pickers, and
/// as a second unbranded taskbar entry. See ADR-009.
const WM_RES_NAME: &str = "intoxicated";
const WM_RES_CLASS: &str = "Intoxicated";

#[repr(C)]
struct XClassHint {
    res_name: *mut c_char,
    res_class: *mut c_char,
}

#[repr(C)]
struct XWMHints {
    flags: c_long,
    input: c_int,
    initial_state: c_int,
    icon_pixmap: c_ulong,
    icon_window: Window,
    icon_x: c_int,
    icon_y: c_int,
    icon_mask: c_ulong,
    window_group: c_ulong,
}

/// Where to put the window, in root coordinates.
///
/// A window created at 0,0 lands on the primary monitor, which is not where
/// anyone wants a game window if they kept a second screen for exactly this.
/// `CORDIAL_MONITOR=<n>` centres the window on the nth monitor reported by
/// Xinerama (0 is the first); `CORDIAL_WINDOW_POS=<x>,<y>` overrides with
/// explicit top-left coordinates and wins if both are set.
///
/// Centring rather than pinning to the monitor's corner, because a monitor
/// origin is not a sensible place for a window — on a layout like
/// `0,0 3440x1440` beside `3440,240 1920x1200`, the corner is where the bezel
/// is.
///
/// Xinerama rather than RandR because the query is one call with no resource
/// management, and every multi-head X server that supports RandR also answers
/// Xinerama. Returns (0, 0) when nothing is configured or the query fails, which
/// is exactly the previous behaviour.
struct Placement {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    fullscreen: bool,
    /// Which monitor was asked for, for `_NET_WM_FULLSCREEN_MONITORS`. A window
    /// manager fullscreens onto whichever monitor it thinks the window is on,
    /// and it does not have to agree with where the window was put — so naming
    /// the monitor explicitly is the only reliable way to say which screen.
    monitor: Option<c_long>,
}

fn placement(win_w: c_int, win_h: c_int) -> Placement {
    let fullscreen = std::env::var_os("CORDIAL_FULLSCREEN").is_some();
    let mut p = Placement { x: 0, y: 0, width: win_w, height: win_h, fullscreen, monitor: None };

    if let Ok(pos) = std::env::var("CORDIAL_WINDOW_POS") {
        let mut parts = pos.split(',').map(str::trim);
        if let (Some(Ok(x)), Some(Ok(y))) = (
            parts.next().map(str::parse::<c_int>),
            parts.next().map(str::parse::<c_int>),
        ) {
            p.x = x;
            p.y = y;
            return p;
        }
        eprintln!("[android] CORDIAL_WINDOW_POS={pos:?} is not <x>,<y>; ignoring");
    }

    let Ok(want) = std::env::var("CORDIAL_MONITOR") else {
        return p;
    };
    let Ok(want) = want.trim().parse::<usize>() else {
        eprintln!("[android] CORDIAL_MONITOR must be a number; ignoring");
        return p;
    };

    #[repr(C)]
    struct XineramaScreenInfo {
        screen_number: c_int,
        x_org: i16,
        y_org: i16,
        width: i16,
        height: i16,
    }

    const RTLD_NOW: c_int = 2;
    // SAFETY: dlopen/dlsym with literal names; every result is null-checked.
    unsafe {
        let lib = dlopen(c"libXinerama.so.1".as_ptr(), RTLD_NOW);
        if lib.is_null() {
            eprintln!("[android] CORDIAL_MONITOR needs libXinerama; ignoring");
            return p;
        }
        let query = dlsym(lib, c"XineramaQueryScreens".as_ptr());
        if query.is_null() {
            return p;
        }
        let query: unsafe extern "C" fn(Display, *mut c_int) -> *mut XineramaScreenInfo =
            std::mem::transmute(query);
        // The caller already has a display open; re-opening here would be a
        // second connection for one query, so this runs against the same one.
        let d = CURRENT_DISPLAY.load(std::sync::atomic::Ordering::Relaxed);
        if d == 0 {
            return p;
        }
        let mut n: c_int = 0;
        let screens = query(d as Display, &mut n);
        if screens.is_null() || n <= 0 {
            return p;
        }
        let list = std::slice::from_raw_parts(screens, n as usize);
        let m = match list.get(want) {
            Some(m) => m,
            None => {
                eprintln!(
                    "[android] CORDIAL_MONITOR={want} but only {n} monitor(s); using the first"
                );
                &list[0]
            }
        };
        p.monitor = Some(want.min(n as usize - 1) as c_long);
        if p.fullscreen {
            // Cover the monitor exactly. The window manager fullscreens onto
            // whichever monitor the window occupies, so filling it first is
            // what pins fullscreen to the requested screen rather than the
            // primary one.
            p.x = m.x_org as c_int;
            p.y = m.y_org as c_int;
            p.width = m.width as c_int;
            p.height = m.height as c_int;
        } else {
            // Clamped at the origin so an oversized window still starts
            // on-screen rather than off the top-left of its monitor.
            p.x = m.x_org as c_int + ((m.width as c_int - win_w) / 2).max(0);
            p.y = m.y_org as c_int + ((m.height as c_int - win_h) / 2).max(0);
        }
        p
    }
}

/// The open display, so `window_origin` can query monitors on the same
/// connection rather than opening a second one for a single call.
static CURRENT_DISPLAY: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Open a window. Fails cleanly when there is no display, which is a normal
/// condition rather than an error — the loader and asset paths do not need one.
pub fn open(width: u32, height: u32, title: &str) -> Result<&'static HostWindow, String> {
    if let Some(w) = WINDOW.get() {
        return Ok(w);
    }
    let xlib = Xlib::load()?;

    // SAFETY: a null display name means $DISPLAY, per Xlib's contract.
    let display = unsafe { (xlib.open_display)(std::ptr::null()) };
    if display.is_null() {
        return Err("no X display (is DISPLAY set?)".into());
    }

    // SAFETY: `display` is open; the geometry and border/background pixels are
    // plain values.
    CURRENT_DISPLAY.store(display as usize, std::sync::atomic::Ordering::Relaxed);
    let place = placement(width as c_int, height as c_int);
    // Reported always, not behind a trace flag: "the window opened on the wrong
    // screen" is a user-visible complaint, and this line is what separates
    // "Cordial computed the wrong position" from "the window manager ignored
    // the one it was given".
    println!(
        "[android] window placement: {}x{} at {},{}{}",
        place.width, place.height, place.x, place.y,
        if place.fullscreen { " (fullscreen)" } else { "" }
    );
    let (ox, oy) = (place.x, place.y);
    // Fullscreen resizes the surface as well as the window: the engine sizes
    // its framebuffers from what `geometry()` reports, so a window covering a
    // 1920x1200 monitor while the surface still says 1280x720 would render a
    // corner of the screen.
    let (width, height) = (place.width as u32, place.height as u32);

    let (window, conn_fd, wm_protocols, wm_delete_window) = unsafe {
        let root = (xlib.default_root_window)(display);
        let w = (xlib.create_simple_window)(display, root, ox, oy, width, height, 0, 0, 0);
        // XStoreName sets WM_NAME, which is XA_STRING — Latin-1, not UTF-8.
        // An em dash here renders as mojibake, so the title is kept ASCII
        // rather than encoded twice for a window caption.
        let ascii: String = title
            .chars()
            .map(|c| if c.is_ascii() { c } else { '-' })
            .collect();
        let name = CString::new(ascii).unwrap_or_default();
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

        // Without WM hints a window manager is free to place this wherever it
        // likes and to decide it does not take keyboard focus. Both were
        // happening: the window landed on the primary monitor whatever
        // `CORDIAL_MONITOR` said, and key events went elsewhere while mouse
        // events still arrived, because ButtonPress is delivered by pointer
        // position but KeyPress follows the focus.
        //
        // USPosition rather than PPosition: it means "the user asked for this
        // position", which window managers honour where they routinely override
        // a mere program preference.
        let mut hints = XSizeHints {
            flags: 1 << 0, // USPosition
            x: ox,
            y: oy,
            width: width as c_int,
            height: height as c_int,
            min_width: 0, min_height: 0, max_width: 0, max_height: 0,
            width_inc: 0, height_inc: 0,
            min_aspect_x: 0, min_aspect_y: 0, max_aspect_x: 0, max_aspect_y: 0,
            base_width: 0, base_height: 0, win_gravity: 0,
        };
        (xlib.set_wm_normal_hints)(display, w, &mut hints);

        // InputHint | StateHint, asking to be given the keyboard.
        let mut wm = XWMHints {
            flags: (1 << 0) | (1 << 1),
            input: 1,
            initial_state: 1, // NormalState
            icon_pixmap: 0, icon_window: 0, icon_x: 0, icon_y: 0,
            icon_mask: 0, window_group: 0,
        };
        (xlib.set_wm_hints)(display, w, &mut wm);

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

        // WM_CLASS, so the window is addressable by rule in a tiling or
        // scripted setup rather than only by title. It is also how a capture
        // tool and the desktop entry resolve the window to the application
        // (ADR-009), which is why the class is a constant with a test against
        // the .desktop rather than a literal here.
        let res_name = CString::new(WM_RES_NAME).unwrap_or_default();
        let res_class = CString::new(WM_RES_CLASS).unwrap_or_default();
        let mut class = XClassHint {
            res_name: res_name.as_ptr() as *mut c_char,
            res_class: res_class.as_ptr() as *mut c_char,
        };
        (xlib.set_class_hint)(display, w, &mut class);

        // Hide the host pointer over this window.
        //
        // Roblox draws its own cursor, so the X11 one sits alongside it and the
        // client shows two. Cordial cannot suppress the engine's — that would be
        // reaching into its rendering — so the host's is the one that goes.
        //
        // `XDefineCursor` is scoped to this window: the pointer is invisible
        // while it is over Cordial and completely untouched everywhere else on
        // the desktop. That matters more than it sounds. The global alternatives
        // (`XFixesHideCursor`, grabbing the pointer) change the cursor for the
        // whole session, and this project has already hijacked the developer's
        // real pointer once with `XTestFakeMotionEvent` — window-scoped is the
        // rule here, not a preference.
        //
        // `CORDIAL_SHOW_CURSOR=1` puts it back, for debugging input where seeing
        // where the host thinks the pointer is matters.
        if std::env::var_os("CORDIAL_SHOW_CURSOR").is_none() {
            // A 1x1 all-zero bitmap used as both source and mask: no pixels are
            // drawn and none are opaque, which is the portable "no cursor".
            let blank: [c_char; 1] = [0];
            let pixmap = (xlib.create_bitmap_from_data)(display, w, blank.as_ptr(), 1, 1);
            if pixmap != 0 {
                let mut black = XColor {
                    pixel: 0, red: 0, green: 0, blue: 0, flags: 0, pad: 0,
                };
                let cursor = (xlib.create_pixmap_cursor)(
                    display, pixmap, pixmap, &mut black, &mut black, 0, 0,
                );
                if cursor != 0 {
                    (xlib.define_cursor)(display, w, cursor);
                    eprintln!("[intoxicated] host cursor hidden over the client window");
                } else {
                    eprintln!("[intoxicated] could not create a blank cursor; host pointer stays visible");
                }
                // The cursor holds its own reference to the pixmap contents, so
                // the pixmap is freed now rather than leaked for the process.
                (xlib.free_pixmap)(display, pixmap);
            }
        }

        (xlib.select_input)(display, w, INPUT_EVENT_MASK);
        // Detectable auto-repeat: without it the server sends a KeyRelease
        // before every repeat KeyPress for a held key, and `dispatch_key`
        // forwarded that as a release/press burst -- a held key cancelled and
        // spammed the engine once the repeat delay elapsed. With it, a held key
        // is bare repeated presses and the real release only comes when the key
        // is physically let go. Process-global to this X connection, so it is
        // set once here. The `supported` out-argument is not read: every X
        // server Cordial can reach supports it, and the fallback if one did not
        // would be exactly today's behaviour, which `dispatch_key`'s own repeat
        // drop already guards against.
        (xlib.set_detectable_auto_repeat)(display, 1, std::ptr::null_mut());
        (xlib.map_window)(display, w);
        // Let the window manager finish its own placement before arguing with
        // it. Moving before it has acted is a race that the window manager
        // wins, which is exactly what happened: Cordial computed 3760,480 and
        // the window still came up at 25,62.
        (xlib.sync)(display, 0);

        let root = (xlib.default_root_window)(display);
        const SUBSTRUCTURE_REDIRECT: c_long = 1 << 20;
        const SUBSTRUCTURE_NOTIFY: c_long = 1 << 19;
        const CLIENT_MESSAGE: c_int = 33;
        let atom = |n: &str| -> c_ulong {
            let c = CString::new(n).unwrap_or_default();
            (xlib.intern_atom)(display, c.as_ptr(), 0)
        };

        // An XClientMessageEvent, laid out by hand. Xlib's XEvent union is
        // large and only the leading fields matter here.
        let mut msg = [0u8; 96];
        let mut send = |message_type: c_ulong, data: [c_long; 5]| {
            msg.fill(0);
            let p = msg.as_mut_ptr();
            *(p as *mut c_int) = CLIENT_MESSAGE;
            *(p.add(8) as *mut c_ulong) = 1; // serial
            *(p.add(16) as *mut c_int) = 1; // send_event
            *(p.add(24) as *mut usize) = display as usize;
            *(p.add(32) as *mut Window) = w;
            *(p.add(40) as *mut c_ulong) = message_type;
            *(p.add(48) as *mut c_int) = 32; // format
            for (i, v) in data.iter().enumerate() {
                *(p.add(56 + i * 8) as *mut c_long) = *v;
            }
            (xlib.send_event)(
                display, root, 0,
                SUBSTRUCTURE_REDIRECT | SUBSTRUCTURE_NOTIFY,
                msg.as_mut_ptr() as *mut c_void,
            );
        };

        if place.fullscreen {
            set_compositor_bypass(&xlib, display, w, true);
            // Name the monitor outright. `_NET_WM_STATE_FULLSCREEN` alone
            // fullscreens onto whichever monitor the window manager believes
            // the window occupies, which is the thing that was wrong.
            if let Some(m) = place.monitor {
                let a = atom("_NET_WM_FULLSCREEN_MONITORS");
                if a != 0 {
                    send(a, [m, m, m, m, 1]);
                }
            }
            let state = atom("_NET_WM_STATE");
            let fs = atom("_NET_WM_STATE_FULLSCREEN");
            if state != 0 && fs != 0 {
                const ADD: c_long = 1;
                send(state, [ADD, fs as c_long, 0, 1, 0]);
            }
        } else if (ox, oy) != (0, 0) {
            (xlib.move_window)(display, w, ox, oy);
        }
        (xlib.flush)(display);
        (xlib.sync)(display, 0);

        (w, (xlib.connection_number)(display), wm_protocols, wm_delete_window)
    };

    // Once, on stdout, unconditionally: which of ADR-028's two mechanisms
    // this session's camera runs on is the first thing a report about how the
    // mouse feels on X11 needs, and it differs by server, not by build.
    let xi = match Xi::init(&xlib, display) {
        Ok(xi) => {
            println!(
                "[android] X11 pointer lock: XInput {}.{} raw motion (camera {})",
                xi.version.0,
                xi.version.1,
                if super::wayland::current_pointer_acceleration() {
                    "accelerated by the X server's pointer settings"
                } else {
                    "unaccelerated: CORDIAL_POINTER_ACCEL=unlocked"
                },
            );
            Some(xi)
        }
        Err(why) => {
            println!(
                "[android] X11 pointer lock: no XInput2 ({why}); falling back to \
                 warp-to-centre, whose deltas the X server has already accelerated, \
                 so CORDIAL_POINTER_ACCEL=unlocked cannot be honoured"
            );
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
        buffers: Mutex::new(Geometry {
            width: width as i32,
            height: height as i32,
            format: WINDOW_FORMAT_RGBA_8888,
        }),
        input: Mutex::new(InputState {
            buttons: 0,
            down_time_ms: 0,
            clock: std::time::Instant::now(),
            held_keys: Vec::new(),
            last_pos: (0.0, 0.0),
        }),
        pointer_lock: Mutex::new(PointerLockState::new()),
        fullscreen: AtomicBool::new(place.fullscreen),
        xi,
        focused: std::sync::atomic::AtomicU8::new(0),
    };
    // No touchscreen, and that is a statement about this backend rather than
    // about the machine: X11 core input has no touch at all, and XInput2 --
    // which this backend now binds, but only for `XI_RawMotion` -- carries
    // touch only in `XI_TouchBegin`/`Update`/`End`, which nothing here selects.
    // So a touchscreen on this host
    // could not reach Cordial through this path however present it is. Saying
    // false is therefore true of what the client can actually receive, which is
    // what `isTouchDevice` is for. A user on a touchscreen who wants the mobile
    // interface on X11 has `CORDIAL_INPUT_TOUCH=1`, which overrides this.
    super::input::report_touchscreen(false);
    Ok(WINDOW.get_or_init(|| host))
}

/// Whether the pointer lock should be held, given one pump's inputs, or `None`
/// for "decide nothing".
///
/// Pulled out of `sync_pointer_lock` so the three independent reasons to want
/// the lock — the engine's own request, a camera-button drag, and the forced
/// override — and the focus gate are unit-testable without a live X server,
/// the same reason [`is_final_expose`] and `input.rs`'s
/// `resolve_mouse_delta`/`touchscreen_reported` take their inputs as plain
/// values rather than reading global state themselves.
///
/// **There is no Escape latch any more.** This used to take a
/// `previously_suppressed` flag that Escape set, so a focused window could be
/// told to lock by the engine and decline -- and in shift lock or first person
/// the engine never stops asking, so one Escape left the camera dead until the
/// player toggled out of it. Wayland removed the same latch in `d440c4f` on a
/// measurement that applies here unchanged, since it is a fact about the
/// engine and not the compositor: Escape is Roblox's own menu key, it reaches
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
    const CAMERA_BUTTONS: i32 = super::input::BUTTON_SECONDARY | super::input::BUTTON_TERTIARY;
    let dragging = !no_drag_lock && (buttons & CAMERA_BUTTONS) != 0;
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
/// to it when the grab ends. `docs/analysis/x11-pointer-lock-review.md` traced
/// the reported "media keys jerk the camera and spam right-click" to exactly
/// this: each such key ungrabbed, warped the pointer across the screen,
/// re-grabbed and warped again, and zeroed the button state with the right
/// button still down.
///
/// `NotifyNormal` is an ordinary focus change. `NotifyWhileGrabbed` is a focus
/// change that happened *while* a keyboard grab was active -- the window
/// manager moving focus during its own binding, say -- and is just as real: the
/// focus did move, only the moment it was reported at differs. So both count.
///
/// `detail == NotifyInferior` is focus moving between this window and one of
/// its own children, which never leaves the window; Cordial creates no
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

/// Of the buttons Cordial believes are down, those the server says are not --
/// a release that went to another client's grab rather than to this window.
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

/// Of the keys Cordial believes are down, those `XQueryKeymap` says are up --
/// released while another client's keyboard grab had the keyboard, so the
/// release went to it and not here. `keymap` is the 256-bit vector indexed by X
/// keycode that `XQueryKeymap` fills.
fn keys_released_elsewhere(held: &[(i32, i32)], keymap: &[u8; 32]) -> Vec<(i32, i32)> {
    held.iter()
        .copied()
        .filter(|&(_, x_keycode)| {
            let k = x_keycode as usize;
            k >= 256 || keymap[k / 8] & (1 << (k % 8)) == 0
        })
        .collect()
}

/// The Android meta bit a modifier key's own event has to have corrected.
///
/// **An X key event's `state` is the modifier mask from before the event.** So
/// Shift's own press arrives with Shift *not* in `state`, and its release with
/// Shift still set -- every modifier's own event one step behind, in both
/// directions. Wayland measured exactly this shape on its backend (see the
/// `self_bit` comment in `wayland.rs`'s key handler: "down SHIFT -> 0x1002
/// shift missing / up SHIFT -> 0x1003 shift still set") and corrects the one bit
/// belonging to the key in hand; this is the same correction for X11, where
/// the cause is the protocol's definition of `state` rather than event order.
/// It matters for shift lock in particular, which is the engine reading a Shift
/// key event: Android's own `KeyEvent` for a Shift press carries
/// `META_SHIFT_ON`, and the release does not.
fn modifier_self_meta(keysym: c_ulong) -> i32 {
    match keysym {
        0xffe1 | 0xffe2 => META_SHIFT_ON, // Shift_L, Shift_R
        0xffe3 | 0xffe4 => META_CTRL_ON,  // Control_L, Control_R
        0xffe9 | 0xffea => META_ALT_ON,   // Alt_L, Alt_R
        _ => 0,
    }
}

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

/// Bound on how many consecutive locked `MotionNotify` events
/// [`locked_pointer_delta`] discards while waiting for the confirmed echo of
/// a recentring warp, before giving up and trusting the next one regardless.
///
/// It exists because `XWarpPointer` onto a pixel the pointer already
/// occupies generates no `MotionNotify` at all -- X does not report a
/// position that did not change -- so there is no unbounded wait that is
/// safe: without this, that one coincidence would discard camera input for
/// the rest of the lock. Four is arbitrary but generous: the steady-state
/// case (nothing else moved the mouse in between) confirms on the very next
/// event, so this bound is only ever exercised by the pathological case it
/// guards against, not by ordinary play.
const MAX_WARP_ECHO_WAIT: u8 = 4;

/// What [`locked_pointer_delta`] decided about one locked `MotionNotify`.
#[derive(Debug, PartialEq, Eq)]
enum LockedMotion {
    /// The confirmed echo of the recentring warp, or an event that
    /// coincidentally lands exactly on `centre` -- either way its delta is
    /// zero, so the two cases need not be told apart. Clears the wait latch.
    Echo,
    /// Not the echo, and still within [`MAX_WARP_ECHO_WAIT`] of the warp that
    /// is being waited for, so discarded rather than trusted.
    Waiting,
    /// A real relative motion to report.
    Real(i32, i32),
}

/// What one `MotionNotify` means while the pointer is locked, or whether it
/// must be discarded instead of reported as camera movement.
///
/// Two different things get discarded here, and conflating them was the
/// X11-180 bug in #41. One is the literal echo of this backend's own
/// `XWarpPointer` call landing back on `centre`. The other is anything still
/// arriving from *before* the lock was taken: the pointer is free-roaming
/// right up to the instant a camera-drag button or the engine's own
/// `SetMouseBehavior(LockCenter)` engages the lock, and X11 guarantees event
/// ordering on one connection -- a `MotionNotify` generated before this
/// backend's `XGrabPointer`+`XWarpPointer` request is necessarily delivered
/// no later than that request's own echo, never after it. The version of
/// this function that shipped only ever checked the *immediate* next event:
/// if a stray in-flight motion sample arrived first, it was read as real
/// motion relative to `centre` rather than discarded, and a pointer that
/// happened to be sitting near a window edge -- exactly where a free cursor
/// tends to be right when the user has just clicked something there -- at
/// the instant the lock engaged reported that stale absolute position as a
/// multi-hundred-pixel relative delta in one frame: a one-shot spin, worst
/// at the edges, which is the shape #41 reports and the discriminator its
/// reporter offered.
///
/// `waiting` is how many consecutive events this lock has already discarded
/// looking for the echo; see [`MAX_WARP_ECHO_WAIT`] for why that is bounded.
///
/// Separate from [`HostWindow::dispatch_motion`] for the same reason as
/// [`pointer_lock_decision`] above: the centre-relative arithmetic and the
/// echo check are the two things in the locked motion path actually worth
/// getting wrong, and neither needs a window to test.
fn locked_pointer_delta(
    event_pos: (i32, i32),
    centre: (i32, i32),
    ignore_next_warp: bool,
    waiting: u8,
) -> LockedMotion {
    if event_pos == centre {
        return LockedMotion::Echo;
    }
    if ignore_next_warp && waiting < MAX_WARP_ECHO_WAIT {
        return LockedMotion::Waiting;
    }
    LockedMotion::Real(event_pos.0 - centre.0, event_pos.1 - centre.1)
}

impl HostWindow {
    /// The X11 `Window`, which is what `eglCreateWindowSurface` takes as its
    /// native window on this platform.
    pub fn egl_native_window(&self) -> c_ulong {
        self.window
    }

    pub fn egl_native_display(&self) -> Display {
        self.display
    }

    /// The X connection's descriptor, so the looper can wait on input rather
    /// than poll for it.
    pub fn connection_fd(&self) -> c_int {
        self.conn_fd
    }

    pub fn geometry(&self) -> (i32, i32, i32) {
        let g = *self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        (g.width, g.height, g.format)
    }

    /// Ask the window manager to add or remove `_NET_WM_STATE_FULLSCREEN`, the
    /// same message `open` sends when `--fullscreen` was asked for at startup.
    ///
    /// This exists for one reason and it is worth stating plainly: **the
    /// fullscreen bug cannot be photographed on Wayland.** This GNOME session
    /// refuses `org.gnome.Shell.Screenshot` and `ScreenshotWindow` with
    /// `AccessDenied`, `grim` is wlroots-only, and `import` cannot see a native
    /// Wayland surface. An X11 window can be photographed with
    /// `import -window`, so the transition can at least be *looked at* on one
    /// backend. It is not the same code path as the Wayland one — GTK and a
    /// subsurface are not involved here — and a result from it says what the
    /// engine does with a fullscreen-sized surface, not what
    /// `sync_canvas_geometry` does.
    pub fn set_fullscreen(&self, on: bool) {
        let xlib = &self.xlib;
        // SAFETY: `display`/`window` are this struct's own live handles, and
        // the event is the same 96-byte `XClientMessageEvent` layout `open`
        // builds by hand a few hundred lines above; see its comment for why the
        // union is written out rather than declared.
        unsafe {
            let root = (xlib.default_root_window)(self.display);
            let name = |s: &std::ffi::CStr| (xlib.intern_atom)(self.display, s.as_ptr(), 0);
            let (state, fs) = (name(c"_NET_WM_STATE"), name(c"_NET_WM_STATE_FULLSCREEN"));
            if state == 0 || fs == 0 {
                return;
            }
            const REMOVE: c_long = 0;
            const ADD: c_long = 1;
            const CLIENT_MESSAGE: c_int = 33;
            const SUBSTRUCTURE_REDIRECT: c_long = 1 << 20;
            const SUBSTRUCTURE_NOTIFY: c_long = 1 << 19;
            let mut msg = [0u8; 96];
            let p = msg.as_mut_ptr();
            *(p as *mut c_int) = CLIENT_MESSAGE;
            *(p.add(8) as *mut c_ulong) = 1;
            *(p.add(16) as *mut c_int) = 1;
            *(p.add(24) as *mut usize) = self.display as usize;
            *(p.add(32) as *mut Window) = self.window;
            *(p.add(40) as *mut c_ulong) = state;
            *(p.add(48) as *mut c_int) = 32;
            for (i, v) in [if on { ADD } else { REMOVE }, fs as c_long, 0, 1, 0].iter().enumerate() {
                *(p.add(56 + i * 8) as *mut c_long) = *v;
            }
            (xlib.send_event)(
                self.display,
                root,
                0,
                SUBSTRUCTURE_REDIRECT | SUBSTRUCTURE_NOTIFY,
                msg.as_mut_ptr() as *mut c_void,
            );
            (xlib.flush)(self.display);
            set_compositor_bypass(xlib, self.display, self.window, on);
        }
        self.fullscreen.store(on, Ordering::Relaxed);
    }

    /// The focus as the last real focus event left it; `None` before any.
    fn focused(&self) -> Option<bool> {
        match self.focused.load(Ordering::Acquire) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// Take or release the pointer to match what the engine and the mouse are
    /// currently asking for. Called at both ends of `pump_input_events`, and
    /// straight after a camera button goes down, so a drag is captured before
    /// a fast pointer can leave the window rather than up to a pump later.
    ///
    /// Still a separate function from Wayland's `sync_pointer_lock`, which
    /// ADR-024 would rather it were not. The decision is now the same shape on
    /// both -- the engine's word, a camera drag, the forced override, a focus
    /// gate and no Escape latch -- and the motion source is now the same kind
    /// of thing (a pre/post-acceleration pair, from `XI_RawMotion` here and
    /// `zwp_relative_pointer_v1` there), which is what ADR-028 said would make
    /// sharing a real refactor rather than a wrapper. What still differs is
    /// everything around the decision: Wayland's right-drag latch, dialogs
    /// and constraint objects have no X11 counterpart yet, so the shared
    /// function is left for when they do rather than written around them.
    fn sync_pointer_lock(&self) {
        let engine_wants = super::input::engine_wants_pointer_lock() == Some(true);

        let buttons = self
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buttons;

        let no_drag_lock = std::env::var_os("CORDIAL_NO_DRAG_LOCK").is_some();
        let force = std::env::var_os("CORDIAL_FORCE_POINTER_LOCK").is_some();

        if std::env::var_os("CORDIAL_NO_POINTER_LOCK").is_some() {
            self.release_pointer_lock();
            return;
        }

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

        let root =
            unsafe { (self.xlib.default_root_window)(self.display) };

        let mut root_return = 0;
        let mut child_return = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut win_x = 0;
        let mut win_y = 0;
        let mut mask = 0;

        // Relative to this window, so `win_x`/`win_y` are where the pointer
        // is over the canvas (the drag anchor) and `root_x`/`root_y` where to
        // put it back on release.
        let queried = unsafe {
            (self.xlib.query_pointer)(
                self.display,
                self.window,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };

        let saved_root =
            if queried != 0 { Some((root_x, root_y)) } else { None };
        let centre = lock_anchor(
            centred,
            self.xi.is_some(),
            (queried != 0).then_some((win_x, win_y)),
            (width, height),
        );

        // X11 CurrentTime is 0.
        // owner_events = True
        // pointer_mode = GrabModeAsync
        // keyboard_mode = GrabModeAsync
        let result = unsafe {
            (self.xlib.grab_pointer)(
                self.display,
                self.window,
                1,
                0x4 | 0x8 | 0x40,
                1,
                1,
                self.window,
                0,
                0,
            )
        };

        if result != 0 {
            // Printed unconditionally, not gated on `trace_mouse()`. The grab
            // this replaces (`set_pointer_capture`) reported a refusal
            // unconditionally too: another client already holding the
            // pointer is a real failure of the lock the user asked for, and
            // burying it behind a trace flag nobody has set by default is
            // exactly the kind of silent stub AGENTS.md rules out.
            eprintln!("[intoxicated] X11 pointer lock was refused (XGrabPointer={result})");
            return;
        }

        {
            let mut state = self
                .pointer_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());

            state.locked = true;
            state.ignore_next_warp = true;
            state.warp_echo_wait = 0;
            state.centre = centre;
            state.saved_root = saved_root;
            state.raw = RawMotionAccumulator::default();
            state.raw_relative_seen = false;
            state.absolute_devices.clear();
            state.last_raw = None;
        }

        if let Some(xi) = &self.xi {
            xi.select_raw_motion(self.display, root, true);
        }

        unsafe {
            (self.xlib.warp_pointer)(
                self.display,
                0,
                self.window,
                0,
                0,
                0,
                0,
                centre.0,
                centre.1,
            );
            (self.xlib.flush)(self.display);
        }

        super::input::reset_mouse_delta();
        // `forget_pending_unlocked_delta`'s own doc says it is called "at
        // every site that also calls `reset_mouse_delta`" -- nothing on the
        // X11 path currently writes `PENDING_UNLOCKED_DELTA` (only
        // `wayland.rs`'s `relative_pointer_motion` does), so there is never
        // anything here to forget. Called anyway so the doc's claim stays
        // true rather than true of Wayland only.
        super::input::forget_pending_unlocked_delta();

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
            state.ignore_next_warp = true;
            state.warp_echo_wait = 0;
        }
        // SAFETY: this struct's own live handles.
        unsafe {
            (self.xlib.warp_pointer)(self.display, 0, self.window, 0, 0, 0, 0, centre.0, centre.1);
            (self.xlib.flush)(self.display);
        }
    }

    fn release_pointer_lock(&self) {
        let (was_locked, saved_root) = {
            let mut state = self
                .pointer_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());

            let was_locked = state.locked;
            let saved_root = state.saved_root.take();

            state.locked = false;
            state.ignore_next_warp = false;
            state.centre = (0, 0);
            // Movement summed this drain and not yet delivered belongs to a
            // lock that no longer exists; delivering it after the release
            // would turn the camera with the cursor already free.
            state.raw = RawMotionAccumulator::default();

            (was_locked, saved_root)
        };

        if !was_locked {
            return;
        }

        unsafe {
            if let Some(xi) = &self.xi {
                let root = (self.xlib.default_root_window)(self.display);
                xi.select_raw_motion(self.display, root, false);
            }

            // X11 CurrentTime is 0.
            (self.xlib.ungrab_pointer)(self.display, 0);

            if let Some((x, y)) = saved_root {
                let root =
                    (self.xlib.default_root_window)(self.display);

                (self.xlib.warp_pointer)(
                    self.display,
                    root,
                    root,
                    0,
                    0,
                    0,
                    0,
                    x,
                    y,
                );
            }

            (self.xlib.flush)(self.display);
        }

        super::input::reset_mouse_delta();
        // See the matching call in `lock_pointer` for why this is here too.
        super::input::forget_pending_unlocked_delta();

        if super::input::trace_mouse() {
            eprintln!("[intoxicated] X11 pointer lock released");
        }
    }

    pub fn close(&self) {
        // SAFETY: both handles came from this struct's own creation calls.
        unsafe {
            (self.xlib.destroy_window)(self.display, self.window);
            (self.xlib.flush)(self.display);
        }
    }
}

pub fn current() -> Option<&'static HostWindow> {
    WINDOW.get()
}

/// Every input to the X11 pointer-lock decision, for the control socket's
/// `pointerlock` verb -- the X11 counterpart of `wayland::pointer_lock_report`,
/// with the same leading fields so one harness reads both.
///
/// `requested` and `confirmed` are the same value here, and that is a fact
/// about X11 rather than a shortcut: `XGrabPointer` either succeeds or returns
/// an error synchronously, so there is no window in which Cordial has asked and
/// the server has not yet answered. `motion` says which of ADR-028's two
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
        state.raw_deliveries,
        if super::wayland::current_pointer_acceleration() { "accelerated" } else { "raw" },
        locked = state.locked,
    )
}

/// Whether the window manager has asked this window to close.
pub fn window_closed() -> bool {
    WINDOW_CLOSED.load(Ordering::Acquire)
}

/// `_NET_WM_BYPASS_COMPOSITOR`, only when `CORDIAL_COMPOSITOR_BYPASS=1`.
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
    if !*OPTED_IN.get_or_init(|| std::env::var("CORDIAL_COMPOSITOR_BYPASS").as_deref() == Ok("1")) {
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

// ------------------------------------------------------------- input pump
//
// Mouse and keyboard, delivered to the engine through the same AGDK
// `GameActivity` natives real Android input goes through — `onTouchEventNative`
// and `onKeyDownNative`/`onKeyUpNative` — via `cordial-linker-sys`'s
// `game_activity` module and the synthesised `MotionEvent`/`KeyEvent` objects in
// `native/game_activity.cpp`.
//
// The design constraint is that this must never block: it runs inside
// `looper::pump`'s own ~50ms-timeout loop, on the thread that also owns the
// engine's message pump, so any call here that waits is a frame the engine
// never gets to render. `XPending`/`XNextEvent` are what actually read queued
// events, but calling either when nothing is queued risks a blocking read in
// at least some libX11 builds. So every drain starts with a zero-timeout
// `poll(2)` on Xlib's own connection fd (`XConnectionNumber`) — a pure
// kernel-side check that can only return immediately — and only touches Xlib
// at all when that says there is something to read.

/// The common prefix shared by `XKeyEvent`, `XButtonEvent` and `XMotionEvent`.
///
/// Xlib deliberately lays these three structs out identically — that is
/// documented behaviour, not a coincidence being relied on here — except for
/// one field whose *meaning* differs: `keycode` for key events, `button` for
/// button events, `is_hint` for motion. It is read generically as `detail` and
/// interpreted according to `type_`.
#[repr(C)]
struct XInputEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    root: c_ulong,
    subwindow: c_ulong,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    detail: c_uint,
    same_screen: c_int,
}

// X11 event `type` values, from X.h.
const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const MOTION_NOTIFY: c_int = 6;
const FOCUS_IN: c_int = 9;
const FOCUS_OUT: c_int = 10;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const EXPOSE: c_int = 12;
const CONFIGURE_NOTIFY: c_int = 22;
const CLIENT_MESSAGE: c_int = 33;
use super::x11_clipboard::{SELECTION_CLEAR, SELECTION_REQUEST};

/// `XConfigureEvent`. Another distinct layout: it carries the window's new
/// geometry rather than a damaged rectangle.
#[repr(C)]
struct XConfigureEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    event: c_ulong,
    window: c_ulong,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    above: c_ulong,
    override_redirect: c_int,
}

/// `XExposeEvent`. A different layout from `XInputEvent` above — Expose
/// carries a damaged rectangle and a batching `count`, not a pointer/keycode
/// `detail` — so it gets its own struct rather than being folded into the
/// shared one.
#[repr(C)]
struct XExposeEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    /// How many more `Expose` events follow for the same repaint, so a
    /// window manager can deliver several damaged rectangles as a batch. 0 on
    /// the last (or only) one — exactly the point at which the whole window
    /// has finished telling us what it needs repainted, and the one point at
    /// which `onSurfaceRedrawNeededNative` should actually fire. Firing on
    /// every event in the batch would mean N redraw requests for one
    /// exposure.
    count: c_int,
}

// X11 modifier bits (X.h) actually consulted below.
const SHIFT_MASK: c_uint = 1 << 0;
const LOCK_MASK: c_uint = 1 << 1; // Caps Lock
const CONTROL_MASK: c_uint = 1 << 2;
const MOD1_MASK: c_uint = 1 << 3; // Alt, on essentially every layout in practice

use super::input::{META_ALT_ON, META_CAPS_LOCK_ON, META_CTRL_ON, META_SHIFT_ON};

fn android_meta_state(x11_state: c_uint) -> i32 {
    let mut m = 0;
    if x11_state & SHIFT_MASK != 0 {
        m |= META_SHIFT_ON;
    }
    if x11_state & CONTROL_MASK != 0 {
        m |= META_CTRL_ON;
    }
    if x11_state & MOD1_MASK != 0 {
        m |= META_ALT_ON;
    }
    if x11_state & LOCK_MASK != 0 {
        m |= META_CAPS_LOCK_ON;
    }
    m
}

// `android.view.MotionEvent.BUTTON_*` / `ACTION_*`, and the keysym table, now
// live in `input.rs` — shared with the Wayland backend. See its module doc for
// why the keysym table in particular carries over unchanged: X11 keysyms and
// XKB keysyms are the same numbering.
use super::input::{
    deliver_field_state, deliver_key, deliver_surface_redraw, deliver_mouse, edit_text_field,
    keysym_to_android, pass_key_event, pass_mouse_button, pass_mouse_move, report_keyboard_state,
    Caret, Edit, ACTION_BUTTON_PRESS, ACTION_BUTTON_RELEASE, ACTION_DOWN, ACTION_HOVER_MOVE, ACTION_MOVE,
    ACTION_UP, BUTTON_BACK, BUTTON_FORWARD, BUTTON_PRIMARY, BUTTON_SECONDARY, BUTTON_TERTIARY,
};

/// X11 numbers buttons 1/2/3 as left/middle/right; Android's bit assignment
/// puts secondary (right) before tertiary (middle). X11's conventional 8/9
/// side buttons become Android's back/forward bits. Buttons 4-7 are the wheel
/// and are handled by [`x11_button_to_wheel`] instead — they must not fall
/// through to here, because delivering a scroll as some button press is worse
/// than dropping it.
fn x11_button_to_android(button: c_uint) -> Option<i32> {
    match button {
        1 => Some(BUTTON_PRIMARY),
        2 => Some(BUTTON_TERTIARY),
        3 => Some(BUTTON_SECONDARY),
        8 => Some(BUTTON_BACK),
        9 => Some(BUTTON_FORWARD),
        _ => None,
    }
}

/// X11's representation of the wheel: four pseudo-buttons, one press-and-release
/// pair per detent, in the order up/down/left/right.
///
/// Returns `(hscroll, vscroll)` in detents with Android's signs — positive
/// away from the user, positive to the right — which is the unit
/// [`super::input::wheel`] takes. One notch is exactly one here, with no
/// conversion to guess at, which is the one thing X11 does better than
/// `wl_pointer.axis`.
fn x11_button_to_wheel(button: c_uint) -> Option<(f32, f32)> {
    match button {
        4 => Some((0.0, 1.0)),
        5 => Some((0.0, -1.0)),
        6 => Some((-1.0, 0.0)),
        7 => Some((1.0, 0.0)),
        _ => None,
    }
}

// `*mut c_void` rather than a typed `*mut PollFd`, to match the `poll`
// declaration `bionic::mod` already has for the emulated libc's own use of the
// same host symbol — `rustc` warns (`clashing_extern_declarations`) about two
// `extern "C" fn poll` with different signatures anywhere in the crate, since
// both ultimately bind the one process-wide C symbol.
extern "C" {
    fn poll(fds: *mut c_void, nfds: c_ulong, timeout_ms: c_int) -> c_int;
}
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
const POLLIN: i16 = 0x001;

/// Whether an `Expose` event is the last one in its batch — `count` is how
/// many more follow for the same repaint, so 0 is the point at which the
/// window has finished describing what it needs redrawn. Pulled out as its
/// own function so the batching decision is unit-testable without a live X11
/// connection.
fn is_final_expose(count: c_int) -> bool {
    count == 0
}

impl HostWindow {
    fn now_ms(&self) -> i64 {
        let state = self.input.lock().unwrap_or_else(|e| e.into_inner());
        state.clock.elapsed().as_millis() as i64
    }

    fn dispatch_button(&self, handle: i64, ev: &XInputEvent, press: bool) {
        // The wheel first. X11 sends a press *and* a release for every detent,
        // and a wheel has no "released" state to report — sending both would
        // scroll twice per notch, so the release half is discarded here rather
        // than by the engine.
        if let Some((hscroll, vscroll)) = x11_button_to_wheel(ev.detail) {
            if press {
                let now = self.now_ms();
                super::input::wheel(handle, ev.x as f32, ev.y as f32, hscroll, vscroll, now);
            }
            return;
        }
        let Some(android_button) = x11_button_to_android(ev.detail) else {
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
    /// [`Self::dispatch_button`] so a release Cordial synthesises -- a real
    /// focus loss, or a release another client's grab swallowed -- tells the
    /// engine exactly what a real one would, rather than only clearing the
    /// shadow bitmask and leaving the engine believing the button is down.
    fn dispatch_button_bit(&self, handle: i64, android_button: i32, x: f32, y: f32, press: bool) {
        let mut state = self.input.lock().unwrap_or_else(|e| e.into_inner());
        if !press && state.buttons & android_button == 0 {
            // Already up as far as the engine knows: a second release would
            // be a release of a button it was never told is down.
            return;
        }
        state.last_pos = (x, y);
        let now = state.clock.elapsed().as_millis() as i64;

        if press {
            if state.buttons == 0 {
                state.down_time_ms = now;
            }
            state.buttons |= android_button;
            let (buttons, down_time) = (state.buttons, state.down_time_ms);
            drop(state);
            // Real Android mouse input delivers exactly this pair for a
            // click: ACTION_DOWN establishes the gesture, then
            // ACTION_BUTTON_PRESS names which button did it.
            deliver_mouse(handle, ACTION_DOWN, x, y, buttons, 0, now, down_time);
            deliver_mouse(handle, ACTION_BUTTON_PRESS, x, y, buttons, android_button, now, down_time);
        } else {
            state.buttons &= !android_button;
            let (buttons, down_time) = (state.buttons, state.down_time_ms);
            drop(state);
            deliver_mouse(handle, ACTION_BUTTON_RELEASE, x, y, buttons, android_button, now, down_time);
            deliver_mouse(handle, ACTION_UP, x, y, buttons, 0, now, down_time);
        }

        // The interface's own input path, alongside AGDK's — and every button,
        // not only the primary one. The gate that used to stand here dropped
        // right and middle before they reached Roblox at all, and a
        // right-button drag is how a mouse turns the camera.
        pass_mouse_button(x, y, press, android_button);
    }

    fn dispatch_motion(&self, handle: i64, ev: &XInputEvent) {
        {
            let mut state = self
                .pointer_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());

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

                let (dx, dy) = match locked_pointer_delta(
                    (ev.x, ev.y),
                    centre,
                    state.ignore_next_warp,
                    state.warp_echo_wait,
                ) {
                    LockedMotion::Echo => {
                        if super::input::trace_mouse() && state.warp_echo_wait > 0 {
                            eprintln!(
                                "[intoxicated] X11 pointer lock: warp echo confirmed at ({}, {}) after discarding {} stale event(s)",
                                ev.x, ev.y, state.warp_echo_wait
                            );
                        }
                        state.ignore_next_warp = false;
                        state.warp_echo_wait = 0;
                        return;
                    }
                    LockedMotion::Waiting => {
                        state.warp_echo_wait += 1;
                        if super::input::trace_mouse() {
                            eprintln!(
                                "[intoxicated] X11 pointer lock: discarding stale motion at ({}, {}), waiting for warp echo at ({}, {}) (wait={})",
                                ev.x, ev.y, centre.0, centre.1, state.warp_echo_wait
                            );
                        }
                        return;
                    }
                    LockedMotion::Real(dx, dy) => {
                        // Either the ordinary case (no warp outstanding) or
                        // `MAX_WARP_ECHO_WAIT` was reached, in which case the
                        // wait is abandoned here rather than left stuck at
                        // the bound forever.
                        state.warp_echo_wait = 0;
                        (dx, dy)
                    }
                };
                let (cx, cy) = centre;
                drop(state);
                self.deliver_locked_delta(handle, centre, dx as f32, dy as f32);

                if dx != 0 || dy != 0 {
                    if super::input::trace_mouse() {
                        eprintln!(
                            "[intoxicated] X11 pointer lock: motion at ({}, {}) -> delta ({dx}, {dy}), re-warping to ({cx}, {cy})",
                            ev.x, ev.y
                        );
                    }

                    let mut state = self
                        .pointer_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());

                    state.ignore_next_warp = true;
                    drop(state);

                    unsafe {
                        (self.xlib.warp_pointer)(
                            self.display,
                            0,
                            self.window,
                            0,
                            0,
                            0,
                            0,
                            cx,
                            cy,
                        );
                        (self.xlib.flush)(self.display);
                    }
                }

                return;
            }
        }

        let (x, y) = (ev.x as f32, ev.y as f32);
        let mut state = self
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.last_pos = (x, y);
        let now = state.clock.elapsed().as_millis() as i64;
        let (buttons, down_time) = (state.buttons, state.down_time_ms);
        drop(state);
        // A held button makes this a drag — part of the gesture the DOWN
        // started, hence ACTION_MOVE with the same down_time. No button held
        // makes it a hover, which is what a mouse (as opposed to touch) sends
        // when it moves without a button down.
        let action = if buttons != 0 { ACTION_MOVE } else { ACTION_HOVER_MOVE };
        deliver_mouse(handle, action, x, y, buttons, 0, now, down_time);
        // And the path the interface reads. Both are driven: AGDK's contract is
        // real and the engine consumes it, it is simply not what hit-tests the
        // Lua UI.
        pass_mouse_move(x, y);
    }

    /// One locked movement, to both input paths: AGDK sees a move at the
    /// anchor (the absolute position does not change while locked) and
    /// `nativePassMouseMove` carries the delta the camera turns by. Shared by
    /// the warp fallback, which calls it per core event, and the raw path,
    /// which calls it once per drain.
    fn deliver_locked_delta(&self, handle: i64, anchor: (i32, i32), dx: f32, dy: f32) {
        let input = self.input.lock().unwrap_or_else(|e| e.into_inner());
        let buttons = input.buttons;
        let down_time = input.down_time_ms;
        let now = input.clock.elapsed().as_millis() as i64;
        drop(input);
        let action = if buttons != 0 { ACTION_MOVE } else { ACTION_HOVER_MOVE };
        let (cx, cy) = (anchor.0 as f32, anchor.1 as f32);
        deliver_mouse(handle, action, cx, cy, buttons, 0, now, down_time);
        if dx != 0.0 || dy != 0.0 {
            super::input::pass_mouse_move_delta(cx, cy, dx, dy);
        }
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
        let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
        if !state.locked {
            // In flight when the lock was released and the subscription
            // dropped. See `Xi::select_raw_motion`: movement outside a lock is
            // not Cordial's to read.
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
        state.raw_relative_seen = true;
        state.raw_events += 1;
        let d = choose_camera_delta(super::wayland::current_pointer_acceleration(), acc, unacc);
        state.raw.add(d);
        if super::input::trace_mouse() {
            eprintln!(
                "[intoxicated] X11 raw motion: time={} device={source} accelerated=({:.3}, {:.3}) raw=({:.3}, {:.3}) -> ({:.3}, {:.3})",
                key.0, acc.0, acc.1, unacc.0, unacc.1, d.0, d.1
            );
        }
    }

    /// Hand the drain's summed raw movement to the engine.
    fn flush_raw_motion(&self, handle: i64) {
        let (anchor, total) = {
            let mut state = self.pointer_lock.lock().unwrap_or_else(|e| e.into_inner());
            let Some(total) = state.raw.take() else { return };
            if !state.locked {
                return;
            }
            state.raw_deliveries += 1;
            (state.centre, total)
        };
        self.deliver_locked_delta(handle, anchor, total.0 as f32, total.1 as f32);
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
            if focus_in {
                self.focused.store(1, Ordering::Release);
            } else {
                self.focused.store(2, Ordering::Release);
                self.lose_focus(handle);
            }
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

    /// The window really lost the keyboard: give the pointer back, and tell
    /// the engine every button and key it still believes is down is up.
    fn lose_focus(&self, handle: i64) {
        self.release_pointer_lock();

        let (buttons, (x, y), stranded) = {
            let mut state = self.input.lock().unwrap_or_else(|e| e.into_inner());
            (state.buttons, state.last_pos, std::mem::take(&mut state.held_keys))
        };

        // **Releases, not a cleared bitmask.** This used to set `buttons = 0`,
        // which changed Cordial's own record and told the engine nothing: it
        // had seen the press, never saw a release, and went on treating the
        // right button as held -- a camera drag that would not end, or a
        // button state that disagreed with Cordial's until the next press.
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
        // `wl_keyboard.leave` for some time; this backend kept no
        // held-key set to do it with. `INFERRED` that this is the
        // whole of #41's second symptom -- no X11 session was run
        // here -- but a release for a key that is up is harmless,
        // and a missing one is the reported bug's shape.
        if !stranded.is_empty() {
            let now = self.now_ms();
            for (keycode, x_keycode) in stranded {
                deliver_key(handle, false, keycode, x_keycode, 0, 0, 0, now, now);
                pass_key_event(false, x_keycode - 8, 0);
            }
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
        let (buttons, (x, y), keys_up) = {
            let mut state = self.input.lock().unwrap_or_else(|e| e.into_inner());
            let up = keys_released_elsewhere(&state.held_keys, &keymap);
            state.held_keys.retain(|k| !up.contains(k));
            (state.buttons, state.last_pos, up)
        };
        if pointer_known {
            for b in buttons_released_elsewhere(buttons, mask) {
                self.dispatch_button_bit(handle, b, x, y, false);
            }
        }
        if !keys_up.is_empty() {
            let now = self.now_ms();
            for (keycode, x_keycode) in keys_up {
                deliver_key(handle, false, keycode, x_keycode, 0, 0, 0, now, now);
                pass_key_event(false, x_keycode - 8, 0);
            }
            if super::input::trace_mouse() || super::input::trace_text() {
                eprintln!("[intoxicated] X11 keyboard grab ended: released keys let go during it");
            }
        }
    }

    fn dispatch_key(&self, handle: i64, buf: &mut [u8; 256], down: bool) {
        let mut keysym: c_ulong = 0;
        let mut text = [0u8; 8];
        // SAFETY: `buf` holds the XKeyEvent `XNextEvent` just filled, laid out
        // identically to `XInputEvent` above (that layout compatibility is
        // documented Xlib behaviour). A null compose-status argument is
        // documented to mean "skip compose-key processing", not "pass a valid
        // pointer" — Xlib treats it as optional.
        let n = unsafe {
            (self.xlib.lookup_string)(
                buf.as_mut_ptr() as *mut c_void,
                text.as_mut_ptr() as *mut c_char,
                text.len() as c_int,
                &mut keysym,
                std::ptr::null_mut(),
            )
        };
        let ev = unsafe { &*(buf.as_ptr() as *const XInputEvent) };
        let fallback = keysym_text_fallback(n, ev.state, keysym);
        let mut fallback_buf = [0u8; 4];
        let typed_text: &str = match fallback {
            Some(ch) => ch.encode_utf8(&mut fallback_buf),
            None => std::str::from_utf8(&text[..n.max(0) as usize]).unwrap_or(""),
        };
        let unicode = match fallback {
            Some(ch) => ch as i32,
            None if n > 0 => text[0] as i32,
            None => 0,
        };
        // See `modifier_self_meta`: a modifier key's own event carries the
        // mask from before it, so Shift's press would otherwise reach the
        // engine without `META_SHIFT_ON` and its release with it.
        let meta = {
            let m = android_meta_state(ev.state);
            let own = modifier_self_meta(keysym);
            if down { m | own } else { m & !own }
        };
        let now = self.now_ms();

        // **Escape is not special here any more.** It used to release the
        // lock and set a latch that kept it released while the engine went on
        // asking, and swallow the key so Roblox never saw it -- so in shift
        // lock one Escape disabled the camera until shift lock was toggled,
        // and the menu Escape is meant to open never opened. It now reaches
        // the engine like any other key; Roblox opens its menu and drops its
        // own lock request, and the lock follows on the next pump. See
        // `pointer_lock_decision` for the measurement, and Wayland's `d440c4f`
        // for the same change there.

        // XK_F11. Like the Wayland game window, this backend is not the GTK
        // launcher and therefore cannot inherit its `win.fullscreen` action.
        if keysym == 0xffc8 {
            if down {
                self.set_fullscreen(!self.fullscreen.load(Ordering::Relaxed));
            }
            return;
        }

        if super::input::trace_text() {
            // `text=` is a length unless `CORDIAL_TRACE_TEXT_SHOW_PASSWORDS=1`:
            // one character at a time is still a password, printed slowly.
            eprintln!(
                "[intoxicated] key {} keysym={keysym:#x} text={} keycode={:?} focus={:?}",
                if down { "down" } else { "up" },
                super::input::redacted(typed_text),
                keysym_to_android(keysym),
                cordial_linker_sys::game_activity::focused_textbox(),
            );
        }

        // Real per-key downTime tracking (one slot per held key) is not
        // implemented; both fields use the current time on every call. That
        // is a simplification, not a faithful `downTime`, and is called out in
        // the report — it does not block a key reaching the engine, only the
        // precision of one timing field most UI code does not consult.
        // Keys the Android keycode table covers. A keysym with no mapping — the
        // shifted symbols, `@` among them — used to `return` here, which also
        // skipped the text path below and silently dropped the character. Text
        // does not need an Android keycode: `@` is a character whether or not
        // AKEYCODE has a name for it, and an email address is unusable without
        // it. So this is now a branch rather than an exit.
        if let Some(keycode) = keysym_to_android(keysym) {
            // A key already held that comes "down" again is an auto-repeat
            // tick. `XkbSetDetectableAutoRepeat` (set in `open`) means those now
            // arrive as bare `KeyPress` with no synthetic release between them,
            // so the repeat is visible here as a down for a key still in
            // `held_keys`. Drop it for the engine's key path: holding a key for
            // a game action (walking, holding E to interact) is one press held,
            // not a burst of presses -- forwarding each tick is what made a held
            // key "spam" once the repeat delay elapsed. The text path below
            // still runs on every tick, so holding a key inside a textbox
            // repeats the character the way Android's own auto-repeat does.
            let is_repeat = {
                let mut input = self.input.lock().unwrap_or_else(|e| e.into_inner());
                let already = down && input.held_keys.contains(&(keycode, ev.detail as i32));
                track_held_key(&mut input.held_keys, down, (keycode, ev.detail as i32));
                already
            };
            if !is_repeat {
                deliver_key(handle, down, keycode, ev.detail as i32, meta, 0, unicode, now, now);
                // The evdev code, not the Android keycode. X11 keycodes are evdev
                // offset by 8 -- XKB reserves the low 8 for historical reasons every
                // consumer has to undo. See `pass_key_event`.
                pass_key_event(down, ev.detail as i32 - 8, meta);
            }
        } else {
            super::trace(format_args!("unmapped X11 keysym {keysym:#x}"));
        }

        // And the text path. Android text fields are edited by state, not by
        // keystrokes — delivering the key alone leaves the box empty, which is
        // exactly what the login form did before this. Only on key-down: a
        // release would deliver the same state twice.
        if down {
            // The timestamp ICCCM wants on a selection claim or conversion;
            // see `x11_clipboard::USER_TIME`.
            super::x11_clipboard::note_user_time(ev.time);
            // Only when the engine has told us a box is focused, via
            // `showKeyboard`. Sending text with no focused box means sending it
            // to handle 0, which is not a box — the engine drops it, silently,
            // which is exactly how this failed before.
            let Some(which) = cordial_linker_sys::game_activity::focused_textbox() else {
                return;
            };
            // Ctrl+A/C/X/V, before anything reads the character.
            //
            // There is no engine call to look for here and that is correct
            // rather than missing: on Android the `EditText` over the GL
            // surface handles these itself and the engine only ever sees text
            // arrive through `gametextinput`. On Wayland a `gtk::Text` is that
            // editor; on X11 there is none (ADR-024), so this is. Before it was
            // here, `XLookupString`'s control characters for Ctrl+A and Ctrl+C
            // reached `Edit::Insert`, which drops control characters, and the
            // shortcuts did nothing at all.
            //
            // The key itself has already gone to the engine above, exactly as
            // on Wayland: `pass_key_event` withholds the letter while a box is
            // focused and forwards Ctrl as a modifier, and AGDK's
            // `onKeyDownNative` hears both, on both backends.
            if let Some(shortcut) = super::input::text_shortcut(keysym, meta) {
                super::clipboard::run_text_shortcut(handle, which, shortcut);
                return;
            }
            let typed = typed_text;
            // Editing keys, before text: an IME consumes these itself rather
            // than committing them, and `XLookupString` reports nothing for
            // them anyway. Keysyms from keysymdef.h. Shift turns a caret move
            // into a selection, as in any desktop field.
            let caret_key = match keysym {
                0xff51 => Some(Caret::Left),  // XK_Left
                0xff53 => Some(Caret::Right), // XK_Right
                0xff50 => Some(Caret::Home),  // XK_Home
                0xff57 => Some(Caret::End),   // XK_End
                _ => None,
            };
            let extend = meta & META_SHIFT_ON != 0;
            let edit = match (keysym, caret_key) {
                (_, Some(to)) if extend => Edit::Extend(to),
                (_, Some(to)) => Edit::Move(to),
                (0xff08, _) => Edit::Backspace, // XK_BackSpace
                (0xffff, _) => Edit::Delete,    // XK_Delete
                _ => Edit::Insert(typed),
            };
            if let Some(state) = edit_text_field(edit) {
                // AGDK's GameTextInput path, and Roblox's own. Both are driven
                // for the same reason as the mouse: the first is the documented
                // contract, the second is what the interface reads.
                deliver_field_state(handle, which, &state);
            }
        }
    }

    /// Drain and deliver whatever X11 input is already queued, then return.
    /// See the module-level comment above for why this never blocks.
    fn pump_input_events(&self, handle: i64) {
        self.sync_pointer_lock();

        // Before draining input: if the engine has opened or closed an editor
        // since last time, acknowledge it. Cheap — an atomic load and a
        // comparison unless something actually changed.
        if super::input::keyboard_report_enabled() {
            let (gw, gh, _) = self.geometry();
            report_keyboard_state((gw, gh));
        }

        let mut pfd = PollFd { fd: self.conn_fd, events: POLLIN, revents: 0 };
        // SAFETY: `pfd` is a live array of length 1; a 0ms timeout makes this a
        // pure non-blocking check.
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

        // Bounded so a burst of queued motion events cannot turn one drain
        // call into unbounded work inside the render loop's own timing
        // budget.
        const MAX_EVENTS_PER_DRAIN: usize = 64;
        for _ in 0..MAX_EVENTS_PER_DRAIN {
            // SAFETY: `self.display` is open; reached only after `poll` above
            // found the connection readable (or a previous iteration left
            // events already queued client-side).
            if unsafe { (self.xlib.pending)(self.display) } <= 0 {
                break;
            }
            let mut buf = [0u8; 256];
            // SAFETY: 256 bytes covers every concrete event struct in the
            // `XEvent` union on every platform Xlib ships for; `buf` is live
            // for the call.
            unsafe { (self.xlib.next_event)(self.display, buf.as_mut_ptr() as *mut c_void) };
            let event_type = unsafe { *(buf.as_ptr() as *const c_int) };

            match event_type {
                BUTTON_PRESS | BUTTON_RELEASE => {
                    let ev = unsafe { &*(buf.as_ptr() as *const XInputEvent) };
                    self.dispatch_button(handle, ev, event_type == BUTTON_PRESS);
                    // A camera button is captured now rather than at the end
                    // of the drain, the same reasoning as Wayland's
                    // `dispatch_pointer_button`: a drain can hold sixty-odd
                    // events, and the drag anchor is meant to be where the
                    // button went down, not wherever the pointer had got to.
                    if event_type == BUTTON_PRESS && matches!(ev.detail, 2 | 3) {
                        self.sync_pointer_lock();
                    }
                }
                MOTION_NOTIFY => {
                    let ev = unsafe { &*(buf.as_ptr() as *const XInputEvent) };
                    self.dispatch_motion(handle, ev);
                }
                KEY_PRESS | KEY_RELEASE => {
                    self.dispatch_key(handle, &mut buf, event_type == KEY_PRESS);
                }
                FOCUS_IN | FOCUS_OUT => {
                    self.dispatch_focus(handle, &buf, event_type == FOCUS_IN);
                }
                GENERIC_EVENT => {
                    self.dispatch_generic(&mut buf);
                }
                EXPOSE => {
                    // SAFETY: `event_type == EXPOSE` means `XNextEvent` just
                    // filled `buf` as the `XExposeEvent` member of Xlib's
                    // `XEvent` union — a different layout from
                    // `XInputEvent` above (see `XExposeEvent`'s own doc
                    // comment), but the one this specific event type is
                    // documented to have.
                    let ev = unsafe { &*(buf.as_ptr() as *const XExposeEvent) };
                    if is_final_expose(ev.count) {
                        deliver_surface_redraw(handle);
                    }
                }
                CONFIGURE_NOTIFY => {
                    // SAFETY: the event type says `XNextEvent` filled `buf` as
                    // the `XConfigureEvent` member of Xlib's union.
                    let ev = unsafe { &*(buf.as_ptr() as *const XConfigureEvent) };
                    self.dispatch_configure(handle, ev.width, ev.height);
                }
                SELECTION_REQUEST | SELECTION_CLEAR => {
                    // Another client asking for what Cordial copied, or taking
                    // the clipboard over. See `x11_clipboard`.
                    super::x11_clipboard::handle_event(&buf);
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
                        // `CORDIAL_NO_CLOSE_EXIT` decides whether it ends the
                        // run, the same as a Wayland close.
                        if !WINDOW_CLOSED.swap(true, Ordering::AcqRel) {
                            println!("[android] X11: the window manager asked the window to close");
                        }
                    }
                }
                _ => {}
            }
        }

        self.flush_raw_motion(handle);
        self.sync_pointer_lock();
    }

    /// The window changed size. Update what the engine is told about it.
    ///
    /// X sends `ConfigureNotify` for moves as well as resizes, and a resize
    /// drag produces a stream of them, so this returns early unless the size
    /// actually changed — re-driving `onSurfaceChangedNative` for every pixel
    /// of a drag would rebuild the engine's framebuffers dozens of times a
    /// second.
    ///
    /// **This used to update only the render surface.** `load.rs` calls
    /// `config::set_screen` once, right after the window first opens, so
    /// `AConfiguration_getScreenWidthDp`/`getScreenHeightDp` agree with the
    /// window at launch — but nothing on this path ever called it again. A
    /// resize (and fullscreen is a resize) kept the true render surface
    /// current while `AConfiguration` went on answering whatever size the
    /// window had when it first opened, which is a screen-size contradiction
    /// of exactly the shape `docs/analysis/platform-identity.md` warns about,
    /// just discovered on the resize path rather than the launch one. Calling
    /// it here as well closes that gap for `AConfiguration` specifically.
    ///
    /// **What this does not close.** `native/init_params.cpp`'s own
    /// `DisplayMetrics`/`Configuration`/`InitParams` objects are a separate
    /// path, and `DisplayMetrics` still reports a compiled 1280x720 at
    /// density 1.0 whatever the window is doing.
    ///
    /// **This used to say `set_display_size` "has no caller anywhere in this
    /// tree", and that was wrong.** It has one: `bin/load.rs:2353` calls
    /// `linker::game_activity::set_display_size`, through
    /// `cordial_set_display_size` into `init_params.cpp`. The path is real and
    /// it runs. Corrected on 2026-08-30 after the false claim was repeated into
    /// a bug report as though it were established.
    ///
    /// The right reason is timing rather than absence, and it is worse: that
    /// call fires only once `initializeNativeCode` has returned, and the engine
    /// reads `DisplayMetrics` exactly once, from inside it, before the host
    /// window exists at all. Measured over a 46-second run in `docs/NEXT.md`:
    /// one construction, `1280x720 density=1.000 densityDpi=160`, and never
    /// again — not on resize, not on anything. So the call is a no-op for
    /// density not because it is missing but because it is too late, and adding
    /// callers will not help.
    ///
    /// This closes only the `AConfiguration` half of the contradiction, not the
    /// `DisplayMetrics` or `User-Agent` half. `INFERRED` that either half
    /// affects the camera — nothing here was run against the engine to check.
    fn dispatch_configure(&self, handle: i64, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        let format = {
            let mut g = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
            if g.width == width && g.height == height {
                return;
            }
            g.width = width;
            g.height = height;
            g.format
        };
        super::config::set_screen(width, height);
        if let Err(e) = cordial_linker_sys::game_activity::surface_resized(
            handle, format, width, height,
        ) {
            super::trace(format_args!("surface resize failed: {e}"));
        }
    }
}

/// Drain and deliver whatever host input is queued, for the current window (if
/// one is open — the loader/asset-only paths that never call `open()` make
/// this a no-op).
pub fn pump_input_events(handle: i64) {
    if let Some(w) = current() {
        w.pump_input_events(handle);
    }
}

// ------------------------------------------------------- ANativeWindow_*

/// The `ANativeWindow*` handed to the engine.
///
/// There is exactly one window, so the pointer is the `HostWindow` itself rather
/// than a separately allocated handle. `acquire`/`release` are then genuinely
/// no-ops instead of pretending to refcount something with a single owner.
fn handle() -> *mut c_void {
    WINDOW.get().map_or(std::ptr::null_mut(), |w| w as *const HostWindow as *mut c_void)
}

fn as_window(p: *mut c_void) -> Option<&'static HostWindow> {
    (!p.is_null()).then(|| WINDOW.get()).flatten()
}

extern "C" fn native_window_from_surface(_env: *mut c_void, _surface: *mut c_void) -> *mut c_void {
    // Cordial's Java `Surface` has no state of its own: there is one window and
    // the Surface object exists only so `onSurfaceCreatedNative`'s signature can
    // be satisfied. Returning the single window is therefore correct rather than
    // a simplification.
    let w = handle();
    // The returned pointer is traced, not just the call. A null here means the
    // engine was handed nothing to render into and every later step will fail
    // for a reason that looks unrelated — and "the Surface has no native peer"
    // is exactly the kind of plausible diagnosis that has been wrong before on
    // this engine. Printing the value settles it instead of inviting the guess.
    super::trace(format_args!("ANativeWindow_fromSurface -> {w:?}"));
    w
}

extern "C" fn native_window_acquire(window: *mut c_void) {
    let _ = window;
}

extern "C" fn native_window_release(window: *mut c_void) {
    let _ = window;
}

extern "C" fn native_window_get_width(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().0)
}

extern "C" fn native_window_get_height(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().1)
}

extern "C" fn native_window_get_format(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().2)
}

/// The engine states the buffer size and format it wants. Android resizes the
/// underlying buffers; here the values are recorded and reported back, because
/// the EGL surface is sized by the X window and the engine only needs the two to
/// agree.
extern "C" fn native_window_set_buffers_geometry(
    window: *mut c_void,
    width: i32,
    height: i32,
    format: i32,
) -> i32 {
    let Some(w) = as_window(window) else {
        return -22; // -EINVAL
    };
    let mut g = w.buffers.lock().unwrap_or_else(|e| e.into_inner());
    // Zero means "whatever the window is", per the API.
    if width > 0 {
        g.width = width;
    }
    if height > 0 {
        g.height = height;
    }
    if format > 0 {
        g.format = format;
    }
    0
}

/// Direct software access to the window's pixels.
///
/// Roblox renders through GLES, so this is not on its path. Returning an error
/// rather than a fake buffer is deliberate: a caller that gets a buffer will
/// write to it and expect the result on screen, and silently discarding that
/// would be far harder to diagnose than a refused lock.
extern "C" fn native_window_lock(
    _window: *mut c_void,
    _buffer: *mut c_void,
    _dirty: *mut c_void,
) -> i32 {
    -38 // -ENOSYS
}

extern "C" fn native_window_unlock_and_post(_window: *mut c_void) -> i32 {
    -38 // -ENOSYS
}

/// `eglCreateWindowSurface`, with the native window translated.
///
/// Android's EGL takes an `ANativeWindow*`. The host's EGL, on X11, takes a
/// `Window` — an XID. Roblox naturally passes the `ANativeWindow*` Cordial
/// handed it through `ANativeWindow_fromSurface`, and Mesa read that pointer as
/// an XID and answered:
///
/// ```text
/// [FLog::SurfaceController] Mode 4 failed: Error creating context: eglCreateWindowSurface 3003
/// [FLog::SurfaceController] RenderView is NULL
/// ```
///
/// 3003 is `EGL_BAD_ALLOC`. Substituting the real window is the whole fix, and
/// it belongs here rather than in `glcount` because the translation is not
/// diagnostic — without it there is no surface at all, whether or not anyone
/// asked for call counts.
///
/// There is exactly one window in this runtime, so any pointer arriving here is
/// that window; the argument is replaced unconditionally rather than compared
/// against a handle that could only ever have one value.
extern "C" fn egl_create_window_surface(
    dpy: *mut c_void,
    config: *mut c_void,
    _native_window: *mut c_void,
    attribs: *mut c_void,
) -> *mut c_void {
    crate::android::glcount::CREATE_WINDOW_SURFACE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    let name = CString::new("eglCreateWindowSurface").unwrap_or_default();
    // SAFETY: RTLD_DEFAULT; libEGL is in the global scope by the time the engine
    // reaches this call.
    let f = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
    if f.is_null() {
        return std::ptr::null_mut();
    }
    type Fn_ = extern "C" fn(*mut c_void, *mut c_void, c_ulong, *mut c_void) -> *mut c_void;
    // SAFETY: resolved from the host for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let win = current().map(|w| w.egl_native_window()).unwrap_or(0);
    f(dpy, config, win, attribs)
}

/// `eglSwapInterval`, with the requested interval clamped to 0.
///
/// The engine asks for `eglSwapInterval(1)` — see the `[FLog::Graphics]` log
/// line of that exact text right after `EGL_MIN_SWAP_INTERVAL: 0`. Honouring
/// that request is what produces the ~1 fps GLES fallback: measured directly
/// (wrapping `eglSwapBuffers` with a timer around the real call), every swap
/// blocks for 0.97-1.00s, not the ~16ms a 60Hz vblank wait should take. That
/// number is too round to be a real refresh interval and stayed exactly 1.00s
/// whether or not the window had input focus (`_NET_ACTIVE_WINDOW` sent by
/// hand made no difference — focus was already ruled out at the Android level
/// separately). Setting the Mesa debug knob `vblank_mode=0` in the process
/// environment makes the block disappear entirely (swaps return in under a
/// millisecond), which isolates the cause to Mesa's DRI3/Present vblank wait,
/// not to Cordial's window, the compositor, or the engine's own pacing.
///
/// The reachable explanation: this host's X server is Xwayland (rootless,
/// under Mutter), which does not own a CRTC and cannot answer DRI3's
/// `GetMSC`/`Present` vblank queries the way a real Xorg/DRM master would.
/// When Mesa's `loader_dri3` can't get real MSC/vblank data it falls back to
/// pacing swaps against a synthetic interval rather than failing outright —
/// on this host that fallback lands on exactly 1 Hz. Vulkan's presentation
/// engine does not go through this code path at all (its own WSI, not GLX/
/// EGL's DRI3 loader), which is why the same host presents at a steady ~27
/// fps over `vkQueuePresentKHR` while GLES stalls on `eglSwapBuffers`.
///
/// Rather than exporting the Mesa env var — which would blanket-disable vsync
/// for every GL/EGL user in the process, including the diagnostic probes in
/// `gl.rs` — the fix is scoped to exactly the call the engine makes: force
/// the interval Mesa actually receives to 0. `eglSwapBuffers` then returns as
/// soon as the frame is submitted instead of waiting on a vblank source this
/// host cannot supply. The engine still paces itself (its own `RenderJob`
/// timing, the same mechanism that limits the Vulkan path to ~27 fps rather
/// than an unthrottled spin), so this does not hand the engine a runaway
/// framerate — it removes an extra, broken 1-Hz throttle underneath that
/// pacing, on top of it.
extern "C" fn egl_swap_interval(dpy: *mut c_void, _interval: c_int) -> u32 {
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    let name = CString::new("eglSwapInterval").unwrap_or_default();
    // SAFETY: RTLD_DEFAULT; libEGL is in the global scope by the time the
    // engine reaches this call.
    let f = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
    if f.is_null() {
        return 0;
    }
    type Fn_ = extern "C" fn(*mut c_void, c_int) -> u32;
    // SAFETY: resolved from the host for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    f(dpy, 0)
}

// The `NativeInputInterface` natives, the text-entry state machine, and
// `set_input_natives` itself have all moved to `input.rs` — see its module
// doc. `dispatch_key`, above, calls back into them by name.

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("ANativeWindow_fromSurface", native_window_from_surface),
        f!("ANativeWindow_acquire", native_window_acquire),
        f!("ANativeWindow_release", native_window_release),
        f!("ANativeWindow_getWidth", native_window_get_width),
        f!("ANativeWindow_getHeight", native_window_get_height),
        f!("ANativeWindow_getFormat", native_window_get_format),
        f!("ANativeWindow_setBuffersGeometry", native_window_set_buffers_geometry),
        f!("ANativeWindow_lock", native_window_lock),
        f!("ANativeWindow_unlockAndPost", native_window_unlock_and_post),
        f!("eglCreateWindowSurface", egl_create_window_surface),
        f!("eglSwapInterval", egl_swap_interval),
    ]
}

/// The character a key typed when `XLookupString` could not say, or `None`.
///
/// `XLookupString` only ever produces Latin-1, so on a Cyrillic layout it
/// returns no bytes and the letter was lost. The keysym still names it, so it
/// is used then -- and only then. It must not win over bytes Xlib did produce,
/// and must not fire under Control or Alt: Ctrl+A's keysym is still `a`, and
/// letting it through inserted the letter into a focused TextBox where Xlib had
/// correctly reported a control character or nothing at all.
fn keysym_text_fallback(lookup_len: c_int, x11_state: c_uint, keysym: c_ulong) -> Option<char> {
    const CONTROL_MASK: c_uint = 1 << 2;
    const MOD1_MASK: c_uint = 1 << 3;
    if lookup_len > 0 || x11_state & (CONTROL_MASK | MOD1_MASK) != 0 {
        return None;
    }
    super::input::keysym_to_char(keysym).filter(|c| !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_keys_are_tracked_once_and_dropped_on_release() {
        let mut held = Vec::new();
        track_held_key(&mut held, true, (51, 25));
        track_held_key(&mut held, true, (51, 25)); // auto-repeat
        track_held_key(&mut held, true, (47, 39));
        assert_eq!(held, vec![(51, 25), (47, 39)]);
        track_held_key(&mut held, false, (51, 25));
        assert_eq!(held, vec![(47, 39)], "only the key that came up is forgotten");
        track_held_key(&mut held, false, (99, 99)); // release for a key never seen
        assert_eq!(held, vec![(47, 39)]);
    }

    #[test]
    fn a_keysym_only_supplies_text_that_xlib_could_not() {
        // Cyrillic_a on a Russian layout: no Latin-1 bytes, no modifiers.
        assert_eq!(keysym_text_fallback(0, 0, 0x06c1), Some('а'));
        // Shift is how capitals are typed and must not suppress them.
        assert_eq!(keysym_text_fallback(0, 1, 0x06e1), Some('А'));
        // Bytes from Xlib always win, including for a plain Latin letter.
        assert_eq!(keysym_text_fallback(1, 0, 0x0061), None);
        // Ctrl+A / Ctrl+C and Alt+letter: the keysym is the letter, but no
        // letter was typed.
        assert_eq!(keysym_text_fallback(0, 1 << 2, 0x0061), None);
        assert_eq!(keysym_text_fallback(0, 1 << 2, 0x0063), None);
        assert_eq!(keysym_text_fallback(0, 1 << 3, 0x06c1), None);
        // An editing key has no character to offer.
        assert_eq!(keysym_text_fallback(0, 0, 0xff08), None);
    }

    #[test]
    fn pointer_lock_is_wanted_for_any_of_its_three_independent_reasons() {
        let focused = Some(true);
        // Nothing asking: no lock.
        assert_eq!(pointer_lock_decision(false, 0, false, false, focused), Some(false));
        // The engine's own request, alone.
        assert_eq!(pointer_lock_decision(true, 0, false, false, focused), Some(true));
        // A camera-button drag, alone -- and blocked by CORDIAL_NO_DRAG_LOCK.
        assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, false, false, focused), Some(true));
        assert_eq!(pointer_lock_decision(false, BUTTON_TERTIARY, false, false, focused), Some(true));
        assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, true, false, focused), Some(false));
        // The primary button is not a camera button and must not arm the lock.
        assert_eq!(pointer_lock_decision(false, BUTTON_PRIMARY, false, false, focused), Some(false));
        // The forced override, alone.
        assert_eq!(pointer_lock_decision(false, 0, false, true, focused), Some(true));
    }

    #[test]
    fn nothing_second_guesses_the_engine_while_focused() {
        // The Escape latch is gone: with the engine asking, the answer is
        // yes on every pump, with no state carried between them that could
        // make it no. (The old signature took a `previously_suppressed`
        // flag; there is nothing left to pass.)
        for _ in 0..3 {
            assert_eq!(pointer_lock_decision(true, 0, false, false, Some(true)), Some(true));
        }
    }

    #[test]
    fn an_unfocused_window_decides_nothing_and_an_unknown_one_still_does() {
        // After a real FocusOut the lock was released; the gate is what
        // stops the next pump taking it straight back while the user is
        // typing in another window.
        assert_eq!(pointer_lock_decision(true, 0, false, false, Some(false)), None);
        assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, false, true, Some(false)), None);
        // No focus event seen yet must not freeze the lock forever.
        assert_eq!(pointer_lock_decision(true, 0, false, false, None), Some(true));
    }

    #[test]
    fn only_real_focus_changes_count_and_grabs_do_not() {
        const NOTIFY_GRAB: c_int = 1;
        const NOTIFY_ANCESTOR: c_int = 0;
        const NOTIFY_NONLINEAR: c_int = 3;
        const NOTIFY_POINTER: c_int = 5;
        // Alt-tab, a click on another window, i3 moving focus.
        assert!(focus_change_is_real(NOTIFY_NORMAL, NOTIFY_NONLINEAR));
        assert!(focus_change_is_real(NOTIFY_NORMAL, NOTIFY_ANCESTOR));
        assert!(focus_change_is_real(NOTIFY_NORMAL, NOTIFY_POINTER));
        // A focus change that happened while a keyboard grab was active is
        // still a focus change.
        assert!(focus_change_is_real(NOTIFY_WHILE_GRABBED, NOTIFY_NONLINEAR));
        // A media key or a window-manager binding activating and releasing a
        // passive key grab: the review's "media keys jerk the camera".
        assert!(!focus_change_is_real(NOTIFY_GRAB, NOTIFY_NONLINEAR));
        assert!(!focus_change_is_real(NOTIFY_UNGRAB, NOTIFY_NONLINEAR));
        // Focus moving into a child of this window never leaves it.
        assert!(!focus_change_is_real(NOTIFY_NORMAL, NOTIFY_INFERIOR));
    }

    #[test]
    fn a_focus_loss_releases_each_held_button_and_nothing_else() {
        assert_eq!(held_button_bits(0), Vec::<i32>::new());
        assert_eq!(held_button_bits(BUTTON_SECONDARY), vec![BUTTON_SECONDARY]);
        assert_eq!(
            held_button_bits(BUTTON_TERTIARY | BUTTON_PRIMARY | BUTTON_FORWARD),
            vec![BUTTON_PRIMARY, BUTTON_TERTIARY, BUTTON_FORWARD],
            "one release per held button, in a fixed order"
        );
    }

    #[test]
    fn a_grab_that_swallowed_a_release_is_reconciled_against_the_server() {
        const BUTTON1: c_uint = 1 << 8;
        const BUTTON3: c_uint = 1 << 10;
        // Right still physically down after a media key: left alone, the
        // drag goes on.
        assert_eq!(buttons_released_elsewhere(BUTTON_SECONDARY, BUTTON3), Vec::<i32>::new());
        // Right let go during the grab: released here, since the real
        // release went to the grabbing client.
        assert_eq!(buttons_released_elsewhere(BUTTON_SECONDARY, 0), vec![BUTTON_SECONDARY]);
        // X's middle is Button2Mask and Android's TERTIARY; the masks must
        // not be crossed with right's.
        assert_eq!(buttons_released_elsewhere(BUTTON_TERTIARY, BUTTON3), vec![BUTTON_TERTIARY]);
        // Unheld buttons are never released, whatever the server says.
        assert_eq!(buttons_released_elsewhere(0, 0), Vec::<i32>::new());
        // Side buttons have no core mask and are not released on a guess.
        assert_eq!(buttons_released_elsewhere(BUTTON_BACK | BUTTON_PRIMARY, BUTTON1), Vec::<i32>::new());

        // W (X keycode 25) still held, Shift_L (50) released during the grab.
        let mut keymap = [0u8; 32];
        keymap[25 / 8] |= 1 << (25 % 8);
        let held = [(51, 25), (59, 50)];
        assert_eq!(keys_released_elsewhere(&held, &keymap), vec![(59, 50)]);
        keymap[50 / 8] |= 1 << (50 % 8);
        assert_eq!(keys_released_elsewhere(&held, &keymap), Vec::<(i32, i32)>::new());
    }

    #[test]
    fn a_modifier_keys_own_event_carries_its_own_bit() {
        // X reports the state from *before* the event: Shift's press has no
        // ShiftMask, its release still has it. The correction is the bit
        // belonging to the key in hand, set on down and cleared on up.
        let fix = |state: c_uint, keysym: c_ulong, down: bool| {
            let m = android_meta_state(state);
            let own = modifier_self_meta(keysym);
            if down { m | own } else { m & !own }
        };
        assert_eq!(fix(0, 0xffe1, true), META_SHIFT_ON, "Shift_L down");
        assert_eq!(fix(SHIFT_MASK, 0xffe1, false), 0, "Shift_L up");
        assert_eq!(fix(0, 0xffe2, true), META_SHIFT_ON, "Shift_R down");
        assert_eq!(fix(0, 0xffe3, true), META_CTRL_ON, "Control_L down");
        // An ordinary key is untouched: W with Shift held keeps Shift.
        assert_eq!(fix(SHIFT_MASK, 0x77, true), META_SHIFT_ON);
        assert_eq!(fix(0, 0x77, false), 0);
        // Shift_L is Android's KEYCODE_SHIFT_LEFT, which is what the engine
        // reads for shift lock.
        assert_eq!(keysym_to_android(0xffe1), Some(59));
    }

    #[test]
    fn raw_valuators_are_unpacked_by_mask_not_by_index() {
        // Both axes moved: two packed values each.
        let both = [0b11u8];
        assert_eq!(
            raw_motion_axes(&both, &[3.0, -2.0], &[1.5, -1.0]),
            ((3.0, -2.0), (1.5, -1.0))
        );
        // Purely vertical: bit 1 alone, and its value is at index 0. An
        // unpacked read would send this to the horizontal axis.
        let y_only = [0b10u8];
        assert_eq!(raw_motion_axes(&y_only, &[4.0], &[2.0]), ((0.0, 4.0), (0.0, 2.0)));
        // A third valuator (a wheel as an axis) is skipped, not read as X.
        let with_wheel = [0b101u8];
        assert_eq!(raw_motion_axes(&with_wheel, &[1.0, 9.0], &[0.5, 9.0]), ((1.0, 0.0), (0.5, 0.0)));
        // Nothing set, or arrays shorter than the mask claims: zeros, no
        // panic.
        assert_eq!(raw_motion_axes(&[0u8, 0], &[], &[]), ((0.0, 0.0), (0.0, 0.0)));
        assert_eq!(raw_motion_axes(&both, &[1.0], &[]), ((1.0, 0.0), (0.0, 0.0)));
    }

    #[test]
    fn the_acceleration_setting_picks_the_pair_and_the_drain_sums_it() {
        let acc = (6.0, -3.0);
        let raw = (2.0, -1.0);
        assert_eq!(choose_camera_delta(true, acc, raw), acc);
        assert_eq!(choose_camera_delta(false, acc, raw), raw);

        let mut sum = RawMotionAccumulator::default();
        assert_eq!(sum.take(), None, "an empty drain wakes nobody");
        sum.add((0.25, 1.0));
        sum.add((0.5, -3.0));
        sum.add((0.25, 0.0));
        assert_eq!(sum.events, 3);
        // Fractions survive: a high-resolution mouse's sub-pixel samples add
        // up rather than each rounding to nothing.
        assert_eq!(sum.take(), Some((1.0, -2.0)));
        assert_eq!(sum.take(), None, "taking empties it");
        // Movement that cancels out within a drain delivers nothing.
        sum.add((1.0, 1.0));
        sum.add((-1.0, -1.0));
        assert_eq!(sum.take(), None);
    }

    #[test]
    fn a_raw_sample_delivered_twice_under_the_grab_is_counted_once() {
        // The measured shape: same time, device, source and values.
        let a: RawSampleKey = (114255876, 2, 4, 7.0, -3.0);
        assert!(!is_duplicate_raw(None, a), "the first sample of a lock is taken");
        assert!(is_duplicate_raw(Some(a), a), "its second delivery is dropped");
        // The next real sample, even with the same values, has a new time.
        let b: RawSampleKey = (114255931, 2, 4, 7.0, -3.0);
        assert!(!is_duplicate_raw(Some(a), b));
        // Same millisecond, different movement: two real samples.
        let c: RawSampleKey = (114255876, 2, 4, 6.0, -3.0);
        assert!(!is_duplicate_raw(Some(a), c));
        // Same millisecond and values from another device: also real.
        let d: RawSampleKey = (114255876, 2, 9, 7.0, -3.0);
        assert!(!is_duplicate_raw(Some(a), d));
    }

    #[test]
    fn a_drag_holds_where_the_button_went_down_and_the_engine_gets_the_centre() {
        let size = (1280, 720);
        // Raw path, camera drag: the cursor stays where the player pressed.
        assert_eq!(lock_anchor(false, true, Some((100, 200)), size), (100, 200));
        // Clamped into the window if the press was reported just outside it.
        assert_eq!(lock_anchor(false, true, Some((-5, 900)), size), (0, 719));
        // The engine's own request is a *centred* lock on either path.
        assert_eq!(lock_anchor(true, true, Some((100, 200)), size), (640, 360));
        // The warp fallback measures from where it warps to and always needs
        // the centre.
        assert_eq!(lock_anchor(false, false, Some((100, 200)), size), (640, 360));
        // No pointer position: the centre.
        assert_eq!(lock_anchor(false, true, None, size), (640, 360));
    }

    #[test]
    fn only_the_edge_band_triggers_a_raw_path_recentre() {
        let size = (1280, 720);
        assert!(!near_edge((640, 360), size));
        assert!(!near_edge((RAW_EDGE_MARGIN, RAW_EDGE_MARGIN), size));
        assert!(near_edge((RAW_EDGE_MARGIN - 1, 360), size));
        assert!(near_edge((640, 719), size));
        assert!(near_edge((1280 - RAW_EDGE_MARGIN, 360), size));
    }

    #[test]
    fn the_warp_echo_is_swallowed_and_real_motion_is_not() {
        // The synthetic MotionNotify this backend's own XWarpPointer produces
        // lands exactly on the capture centre and must be dropped, or every
        // recentring warp would report itself as a fresh delta.
        assert_eq!(locked_pointer_delta((640, 360), (640, 360), true, 0), LockedMotion::Echo);
        // The same coincidence with the latch already spent (a previous
        // frame consumed the echo) is still just a zero delta -- an `Echo`
        // either way, since the two cases are indistinguishable and neither
        // has anything to report.
        assert_eq!(locked_pointer_delta((640, 360), (640, 360), false, 0), LockedMotion::Echo);
        // Ordinary motion away from centre, with no warp outstanding, is
        // never swallowed.
        assert_eq!(locked_pointer_delta((645, 358), (640, 360), false, 0), LockedMotion::Real(5, -2));
    }

    #[test]
    fn stale_pre_lock_motion_is_discarded_rather_than_read_as_a_spin() {
        // #41: a motion event still in flight from before the lock engaged --
        // the free cursor sitting near a window edge at the instant a camera
        // drag or SetMouseBehavior(LockCenter) grabbed it -- must not be
        // reported as a several-hundred-pixel delta just because it is not
        // itself the echo. It is discarded, and the wait latch stays armed.
        assert_eq!(locked_pointer_delta((1900, 40), (640, 360), true, 0), LockedMotion::Waiting);
        // A second stale event before the echo arrives: still discarded.
        assert_eq!(locked_pointer_delta((1850, 55), (640, 360), true, 1), LockedMotion::Waiting);
        // The echo itself, whenever it turns up, is still recognised and
        // clears the latch.
        assert_eq!(locked_pointer_delta((640, 360), (640, 360), true, 2), LockedMotion::Echo);
    }

    #[test]
    fn a_warp_with_no_echo_eventually_gives_up_waiting() {
        // XWarpPointer onto a pixel the pointer already occupies produces no
        // MotionNotify -- there is nothing to confirm the warp with -- so an
        // unbounded wait would discard camera input for the rest of the
        // lock. Once MAX_WARP_ECHO_WAIT is reached the next event is trusted
        // even though the latch was never explicitly cleared.
        for waiting in 0..MAX_WARP_ECHO_WAIT {
            assert_eq!(
                locked_pointer_delta((900, 200), (640, 360), true, waiting),
                LockedMotion::Waiting,
                "wait {waiting} should still be discarded"
            );
        }
        assert_eq!(
            locked_pointer_delta((900, 200), (640, 360), true, MAX_WARP_ECHO_WAIT),
            LockedMotion::Real(260, -160)
        );
    }

    #[test]
    fn only_the_last_expose_in_a_batch_triggers_a_redraw() {
        // A window manager delivering several damaged rectangles as one
        // repaint sets `count` to how many more follow; firing on every one
        // of them would mean N redraw requests for a single exposure.
        assert!(!is_final_expose(3));
        assert!(!is_final_expose(1));
        assert!(is_final_expose(0));
    }

    #[test]
    fn the_wheel_pseudo_buttons_are_not_clicks() {
        // X11 has no wheel; it has buttons 4-7. Letting them fall through to
        // `x11_button_to_android` is how a scroll would arrive as some button
        // press, and the two tables have to stay disjoint for that not to
        // happen — hence both assertions, not just the wheel one.
        for b in 4..=7 {
            assert!(x11_button_to_wheel(b).is_some(), "button {b} is the wheel");
            assert!(x11_button_to_android(b).is_none(), "button {b} must not also be a click");
        }
        for b in 1..=3 {
            assert!(x11_button_to_wheel(b).is_none(), "button {b} is a click, not the wheel");
        }
        assert_eq!(x11_button_to_android(8), Some(BUTTON_BACK));
        assert_eq!(x11_button_to_android(9), Some(BUTTON_FORWARD));
        // Up is positive, matching MotionEvent.AXIS_VSCROLL, and one X11
        // pseudo-button is exactly one detent — no conversion to get wrong.
        assert_eq!(x11_button_to_wheel(4), Some((0.0, 1.0)));
        assert_eq!(x11_button_to_wheel(5), Some((0.0, -1.0)));
        assert_eq!(x11_button_to_wheel(6), Some((-1.0, 0.0)));
        assert_eq!(x11_button_to_wheel(7), Some((1.0, 0.0)));
    }

    #[test]
    fn input_event_mask_watches_for_expose() {
        // ExposureMask (0x8000, X.h) is what makes a damaged window generate
        // `Expose` at all — without it in the mask `open()` passes to
        // `XSelectInput`, `onSurfaceRedrawNeededNative` would never have
        // anything to react to. Checked against the real constant, not a
        // re-derived copy, so a future edit that drops the bit fails this
        // test rather than only failing silently against a live window
        // manager.
        const EXPOSURE_MASK: c_long = 0x8000;
        assert_eq!(INPUT_EVENT_MASK & EXPOSURE_MASK, EXPOSURE_MASK);
        // The previously-driven input classes stay watched too — this is an
        // addition, not a replacement.
        const KEY_BUTTON_MOTION_MASK: c_long = 0x1 | 0x2 | 0x4 | 0x8 | 0x40;
        assert_eq!(
            INPUT_EVENT_MASK & KEY_BUTTON_MOTION_MASK,
            KEY_BUTTON_MOTION_MASK
        );
    }

    #[test]
    fn wm_class_matches_the_desktop_entry() {
        // A capture tool, the taskbar and the portal picker all resolve a
        // window to its application by matching WM_CLASS against
        // StartupWMClass. When they disagree nothing errors — Cordial just
        // shows up in OBS and GNOME as a nameless, iconless window, which is
        // exactly the kind of break nobody notices until a user reports it.
        // ADR-009 commits to this staying true, so it is checked rather than
        // asserted in prose.
        let desktop = include_str!("../../../../packaging/io.github.luohoa97.Cordial.desktop");
        let declared = desktop
            .lines()
            .find_map(|l| l.strip_prefix("StartupWMClass="))
            .expect("desktop entry declares StartupWMClass");
        assert_eq!(declared.trim(), WM_RES_CLASS);
    }
}
