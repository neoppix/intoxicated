//! XInput2 raw motion for the pointer lock, and libX11's local input method
//! for text-box text.

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CString};

use super::xlib::{dlopen, dlsym, Display, Window, Xlib, RTLD_NOW};

/// XInput2, for unaccelerated relative motion while the pointer is locked.
///
/// **Core X11 cannot say what the mouse did, only where the server put the
/// pointer afterwards.** A `MotionNotify` position has already been through the
/// server's acceleration curve, so the warp-to-centre lock that preceded this
/// could only ever hand the camera an accelerated delta, whatever
/// `INTOXICATED_POINTER_ACCEL` or the settings row said. `XI_RawMotion` carries
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
pub(super) struct Xi {
    /// The XInputExtension's major opcode; a `GenericEvent` carries it in
    /// `extension` and nothing else identifies whose event it is.
    pub(super) opcode: c_int,
    /// The version the server agreed to, for the startup line and the
    /// `pointerlock` report.
    pub(super) version: (c_int, c_int),
    select_events: unsafe extern "C" fn(Display, Window, *mut XIEventMask, c_int) -> c_int,
    query_device: unsafe extern "C" fn(Display, c_int, *mut c_int) -> *mut XIDeviceInfo,
    free_device_info: unsafe extern "C" fn(*mut XIDeviceInfo),
    pub(super) get_event_data: unsafe extern "C" fn(Display, *mut XGenericEventCookie) -> c_int,
    pub(super) free_event_data: unsafe extern "C" fn(Display, *mut XGenericEventCookie),
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
/// so, which is the trap here, and why `HostWindow::dispatch_generic`
/// copies everything it needs out before freeing.
#[repr(C)]
pub(super) struct XGenericEventCookie {
    pub(super) type_: c_int,
    pub(super) serial: c_ulong,
    pub(super) send_event: c_int,
    pub(super) display: Display,
    pub(super) extension: c_int,
    pub(super) evtype: c_int,
    pub(super) cookie: c_uint,
    pub(super) data: *mut c_void,
}

/// `XIRawEvent`, from XI2.h. `valuators` is the post-acceleration set and
/// `raw_values` the device's own report; both are packed, one double per bit
/// set in `valuators.mask`, which is what [`raw_motion_axes`] unpacks.
#[repr(C)]
pub(super) struct XIRawEvent {
    pub(super) type_: c_int,
    pub(super) serial: c_ulong,
    pub(super) send_event: c_int,
    pub(super) display: Display,
    pub(super) extension: c_int,
    pub(super) evtype: c_int,
    pub(super) time: c_ulong,
    pub(super) deviceid: c_int,
    pub(super) sourceid: c_int,
    pub(super) detail: c_int,
    pub(super) flags: c_int,
    pub(super) valuators_mask_len: c_int,
    pub(super) valuators_mask: *const u8,
    pub(super) valuators_values: *const f64,
    pub(super) raw_values: *const f64,
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

pub(super) const GENERIC_EVENT: c_int = 35;
pub(super) const XI_RAW_MOTION: c_int = 17;
const XI_ALL_MASTER_DEVICES: c_int = 1;
const XI_VALUATOR_CLASS: c_int = 2;
const XI_MODE_ABSOLUTE: c_int = 1;

impl Xi {
    /// Negotiate XI2 on `display`, or say why not. `INTOXICATED_NO_XI2=1` refuses
    /// on purpose: the warp fallback has to be exercised with XI2 forced off
    /// in the same session as a comparison, and a switch is the only way to do
    /// that on a server that has XI2.
    pub(super) fn init(xlib: &Xlib, display: Display) -> Result<Self, String> {
        if std::env::var_os("INTOXICATED_NO_XI2").is_some() {
            return Err("turned off by INTOXICATED_NO_XI2".into());
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
                unsafe { std::mem::transmute::<*mut c_void, _>(p) }
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
    /// the client receiving the user's pointer movement across their whole
    /// session, which is a privacy failure, not merely a bug.
    /// Selecting on lock and deselecting on release means the server does not
    /// send them at all otherwise; `dispatch_generic` additionally drops any
    /// that were already in flight when the lock went.
    pub(super) fn select_raw_motion(&self, display: Display, root: Window, on: bool) {
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
    pub(super) fn device_is_absolute(&self, display: Display, deviceid: c_int) -> bool {
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

/// libX11's own input method, for the text a key commits to a focused box:
/// dead keys and Compose sequences, and UTF-8 out of `Xutf8LookupString`
/// whatever the process locale.
///
/// **The local method only (`@im=none`), never an IM server.** A server
/// (fcitx, ibus over XIM) receives the keys it filters and hands them back
/// later, which is latency on the game's input; the local method composes in
/// this process, synchronously, from the locale's Compose table. Only key
/// presses for a focused text box are filtered at all, so game keys never go
/// near it. CJK input through an IM server is therefore not available.
///
/// The process locale is left alone (the engine shares it), so the Compose
/// table is the one for the locale the process already has; under the C
/// locale that is libX11's Latin-1 table, which composes every dead key on a
/// German, French or Spanish layout (measured on this machine with locally
/// built events: dead_acute + e gives "é", dead_circumflex twice gives "^").
///
/// A composed result comes back as a `KeyPress` with keycode 0 that
/// `XFilterEvent` puts at the head of the queue; `dispatch_key` treats keycode
/// 0 as text only, never as a key.
pub(super) struct Xim {
    filter_event: unsafe extern "C" fn(*mut c_void, Window) -> c_int,
    utf8_lookup_string:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_char, c_int, *mut c_ulong, *mut c_int) -> c_int,
    ic: *mut c_void,
}

// `XIMPreeditNothing | XIMStatusNothing`, and the lookup statuses, from Xlib.h.
const XIM_PREEDIT_NOTHING: c_long = 0x0008;
const XIM_STATUS_NOTHING: c_long = 0x0400;
const X_BUFFER_OVERFLOW: c_int = -1;
const X_LOOKUP_CHARS: c_int = 2;
const X_LOOKUP_BOTH: c_int = 4;

impl Xim {
    /// Open the local input method and one input context on `window`.
    pub(super) fn open(display: Display, window: Window) -> Result<Self, String> {
        // SAFETY: an already-loaded soname; the handle is never closed.
        let lib = unsafe { dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err("libX11.so.6 could not be reopened".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                // SAFETY: the handle is open; the names are Xlib's documented
                // exports with the signatures they are given here.
                let p = unsafe { dlsym(lib, $name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("libX11 has no {}", $name.to_string_lossy()));
                }
                unsafe { std::mem::transmute::<*mut c_void, _>(p) }
            }};
        }
        let set_locale_modifiers: unsafe extern "C" fn(*const c_char) -> *mut c_char = sym!(c"XSetLocaleModifiers");
        let open_im: unsafe extern "C" fn(Display, *mut c_void, *mut c_char, *mut c_char) -> *mut c_void =
            sym!(c"XOpenIM");
        // Variadic: name/value pairs ending in NULL.
        let create_ic: unsafe extern "C" fn(*mut c_void, ...) -> *mut c_void = sym!(c"XCreateIC");
        let set_ic_focus: unsafe extern "C" fn(*mut c_void) = sym!(c"XSetICFocus");
        let filter_event = sym!(c"XFilterEvent");
        let utf8_lookup_string = sym!(c"Xutf8LookupString");
        // SAFETY: `display` and `window` are live; the strings are literals and
        // the variadic list is the documented name/value form, NULL-terminated.
        unsafe {
            set_locale_modifiers(c"@im=none".as_ptr());
            let im = open_im(display, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            if im.is_null() {
                return Err("XOpenIM refused the local input method".into());
            }
            let ic = create_ic(
                im,
                c"inputStyle".as_ptr(),
                XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING,
                c"clientWindow".as_ptr(),
                window,
                c"focusWindow".as_ptr(),
                window,
                std::ptr::null_mut::<c_void>(),
            );
            if ic.is_null() {
                return Err("XCreateIC refused an input context".into());
            }
            set_ic_focus(ic);
            Ok(Xim { filter_event, utf8_lookup_string, ic })
        }
    }

    /// Whether the input method took this key event (a dead key, the middle of
    /// a Compose sequence, or its end, whose result comes back as a keycode-0
    /// press). `event` is a whole `XEvent`.
    pub(super) fn filter(&self, event: *mut c_void) -> bool {
        // SAFETY: `event` is a live XEvent; window 0 means "the event's own".
        unsafe { (self.filter_event)(event, 0) != 0 }
    }

    /// The UTF-8 text a key press commits, or empty.
    pub(super) fn committed_text(&self, event: *mut c_void) -> String {
        let mut buf = vec![0u8; 64];
        let (mut keysym, mut status): (c_ulong, c_int) = (0, 0);
        for _ in 0..2 {
            // SAFETY: `event` is a live XKeyPressedEvent, `buf` is as long as
            // passed, and the out-pointers are live locals.
            let n = unsafe {
                (self.utf8_lookup_string)(
                    self.ic,
                    event,
                    buf.as_mut_ptr() as *mut c_char,
                    buf.len() as c_int,
                    &mut keysym,
                    &mut status,
                )
            };
            if status == X_BUFFER_OVERFLOW {
                buf = vec![0u8; n.max(0) as usize + 1];
                continue;
            }
            if status == X_LOOKUP_CHARS || status == X_LOOKUP_BOTH {
                let n = (n.max(0) as usize).min(buf.len());
                return String::from_utf8_lossy(&buf[..n]).into_owned();
            }
            break;
        }
        String::new()
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
pub(super) fn raw_motion_axes(mask: &[u8], accelerated: &[f64], raw: &[f64]) -> ((f64, f64), (f64, f64)) {
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

/// Whether to pass the server's accelerated deltas through to the camera.
///
/// Read once: a locked pointer reports at the mouse's full rate, so an
/// environment lookup on every `XI_RawMotion` would be a needless cost on the
/// hottest input path there is. "always" is the only value that turns this
/// on. Anything else, including the variable being absent, leaves the camera
/// on raw movement -- the shell sends "unlocked" for that.
pub(super) fn current_pointer_acceleration() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| matches!(std::env::var("INTOXICATED_POINTER_ACCEL").as_deref(), Ok("always")))
}

/// Which of the two deltas the camera gets: `accelerated` is
/// `INTOXICATED_POINTER_ACCEL` saying "always".
pub(super) fn choose_camera_delta(accelerated: bool, acc: (f64, f64), raw: (f64, f64)) -> (f64, f64) {
    if accelerated {
        acc
    } else {
        raw
    }
}

/// What identifies one physical raw sample: server time, master and source
/// device, and the unaccelerated X and Y. See [`is_duplicate_raw`].
pub(super) type RawSampleKey = (c_ulong, c_int, c_int, f64, f64);

/// Whether `cur` is the same raw sample as `prev`, delivered a second time.
///
/// **While this client holds a pointer grab, the X server delivers each
/// `XI_RawMotion` twice** -- once through the grab and once through the root
/// window selection. Measured on Xvfb (FreeBSD xorg-vfbserver), driving the
/// XTEST pointer with `xdotool mousemove_relative -- 7 -3`: with no grab,
/// `xinput test-xi2 --root` shows one RawMotion per move; under the client's
/// grab the trace showed two per move with identical serial, time, device,
/// source and values (`serial=4078 time=114255876 device=2 source=4`, twice).
/// Summed unfiltered, that is a camera turning at exactly double the mouse,
/// which is precisely "does not feel normal". SDL drops the same duplicate by
/// the same test, time plus values.
///
/// The cost is that two *genuine* samples from one device inside the same
/// millisecond with bit-identical deltas would lose one of them: one count of
/// a 1000 Hz mouse, never more than one in a row.
pub(super) fn is_duplicate_raw(prev: Option<RawSampleKey>, cur: RawSampleKey) -> bool {
    prev == Some(cur)
}

// Raw movement is summed per drain in `input::PendingMotion`, which also owns
// the rule that it is delivered before any button or key read after it.
