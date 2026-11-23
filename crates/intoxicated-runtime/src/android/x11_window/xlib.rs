//! libX11 and libXinerama, loaded at run time, and the parts of the Xlib ABI
//! the window reads and writes by hand.
//!
//! Nothing here links against X: the library is `dlopen`ed by soname, so a
//! machine without a display still runs the loader and asset paths, and "no
//! libX11" is an error message rather than a failure to start. Every layout
//! below is the public LP64 one from `Xlib.h`, `Xutil.h` and `Xinerama.h`.

use std::ffi::{c_char, c_int, c_long, c_short, c_uint, c_ulong, c_void};

/// `Display *`.
pub(super) type Display = *mut c_void;
/// `Window`, and every other XID (`Pixmap`, `Cursor`, `Atom`, `Time`).
pub(super) type Window = c_ulong;

extern "C" {
    pub(super) fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    pub(super) fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    pub(super) fn poll(fds: *mut c_void, nfds: c_ulong, timeout: c_int) -> c_int;
}

pub(super) const RTLD_NOW: c_int = 2;

/// `struct pollfd` and `POLLIN`, from FreeBSD's `poll.h`.
#[repr(C)]
pub(super) struct PollFd {
    pub(super) fd: c_int,
    pub(super) events: c_short,
    pub(super) revents: c_short,
}
pub(super) const POLLIN: c_short = 0x0001;

// ---- event types (X.h)

pub(super) const KEY_PRESS: c_int = 2;
pub(super) const KEY_RELEASE: c_int = 3;
pub(super) const BUTTON_PRESS: c_int = 4;
pub(super) const BUTTON_RELEASE: c_int = 5;
pub(super) const MOTION_NOTIFY: c_int = 6;
pub(super) const FOCUS_OUT: c_int = 10;
pub(super) const EXPOSE: c_int = 12;
pub(super) const CONFIGURE_NOTIFY: c_int = 22;

// ---- event masks (X.h)

pub(super) const KEY_PRESS_MASK: c_long = 1 << 0;
pub(super) const KEY_RELEASE_MASK: c_long = 1 << 1;
pub(super) const BUTTON_PRESS_MASK: c_long = 1 << 2;
pub(super) const BUTTON_RELEASE_MASK: c_long = 1 << 3;
/// `PointerMotionMask`. Matches every `MotionNotify` in `XCheckMaskEvent`.
pub(super) const POINTER_MOTION_MASK: c_long = 0x40;
pub(super) const EXPOSURE_MASK: c_long = 1 << 15;
pub(super) const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;
pub(super) const SUBSTRUCTURE_NOTIFY_MASK: c_long = 1 << 19;
pub(super) const SUBSTRUCTURE_REDIRECT_MASK: c_long = 1 << 20;
pub(super) const FOCUS_CHANGE_MASK: c_long = 1 << 21;

// ---- modifier bits in an event's `state` (X.h)

pub(super) const CONTROL_MASK: c_uint = 1 << 2;
pub(super) const MOD1_MASK: c_uint = 1 << 3;

// ---- grabs, hints, window manager protocol values

pub(super) const GRAB_MODE_ASYNC: c_int = 1;
pub(super) const CURRENT_TIME: c_ulong = 0;
/// `USPosition` in `XSizeHints.flags`.
pub(super) const US_POSITION: c_long = 1 << 0;
/// `InputHint` and `StateHint` in `XWMHints.flags`.
pub(super) const INPUT_HINT: c_long = 1 << 0;
pub(super) const STATE_HINT: c_long = 1 << 1;
pub(super) const NORMAL_STATE: c_int = 1;
pub(super) const CLIENT_MESSAGE_TYPE: c_int = 33;

/// The size of the `XEvent` union (24 longs). Every buffer an event is read
/// into or built in is at least this big.
pub(super) const XEVENT_SIZE: usize = 192;

/// The shared prefix of `XKeyEvent`, `XButtonEvent` and `XMotionEvent`.
///
/// `detail` is `keycode` or `button` (both `unsigned int`); in a motion
/// event the same bytes hold the one-byte `is_hint` and padding, which
/// nothing here reads.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct XInputEvent {
    pub(super) type_: c_int,
    pub(super) serial: c_ulong,
    pub(super) send_event: c_int,
    pub(super) display: Display,
    pub(super) window: Window,
    pub(super) root: Window,
    pub(super) subwindow: Window,
    pub(super) time: c_ulong,
    pub(super) x: c_int,
    pub(super) y: c_int,
    pub(super) x_root: c_int,
    pub(super) y_root: c_int,
    pub(super) state: c_uint,
    pub(super) detail: c_uint,
    pub(super) same_screen: c_int,
}

/// `XSizeHints`, whole: `XSetWMNormalHints` reads all of it.
#[repr(C)]
#[derive(Default)]
pub(super) struct XSizeHints {
    pub(super) flags: c_long,
    pub(super) x: c_int,
    pub(super) y: c_int,
    pub(super) width: c_int,
    pub(super) height: c_int,
    pub(super) min_width: c_int,
    pub(super) min_height: c_int,
    pub(super) max_width: c_int,
    pub(super) max_height: c_int,
    pub(super) width_inc: c_int,
    pub(super) height_inc: c_int,
    pub(super) min_aspect: [c_int; 2],
    pub(super) max_aspect: [c_int; 2],
    pub(super) base_width: c_int,
    pub(super) base_height: c_int,
    pub(super) win_gravity: c_int,
}

/// `XWMHints`.
#[repr(C)]
#[derive(Default)]
pub(super) struct XWMHints {
    pub(super) flags: c_long,
    pub(super) input: c_int,
    pub(super) initial_state: c_int,
    pub(super) icon_pixmap: c_ulong,
    pub(super) icon_window: Window,
    pub(super) icon_x: c_int,
    pub(super) icon_y: c_int,
    pub(super) icon_mask: c_ulong,
    pub(super) window_group: c_ulong,
}

/// `XClassHint`.
#[repr(C)]
pub(super) struct XClassHint {
    pub(super) res_name: *mut c_char,
    pub(super) res_class: *mut c_char,
}

/// `XColor`, in full: `XCreatePixmapCursor` writes through it.
#[repr(C)]
#[derive(Default)]
pub(super) struct XColor {
    pub(super) pixel: c_ulong,
    pub(super) red: u16,
    pub(super) green: u16,
    pub(super) blue: u16,
    pub(super) flags: c_char,
    pub(super) pad: c_char,
}

/// The libX11 entry points the window uses, resolved once.
pub(super) struct Xlib {
    pub(super) init_threads: unsafe extern "C" fn() -> c_int,
    pub(super) open_display: unsafe extern "C" fn(*const c_char) -> Display,
    pub(super) default_root_window: unsafe extern "C" fn(Display) -> Window,
    pub(super) create_simple_window: unsafe extern "C" fn(
        Display, Window, c_int, c_int, c_uint, c_uint, c_uint, c_ulong, c_ulong,
    ) -> Window,
    pub(super) map_window: unsafe extern "C" fn(Display, Window) -> c_int,
    pub(super) store_name: unsafe extern "C" fn(Display, Window, *const c_char) -> c_int,
    pub(super) flush: unsafe extern "C" fn(Display) -> c_int,
    pub(super) destroy_window: unsafe extern "C" fn(Display, Window) -> c_int,
    pub(super) select_input: unsafe extern "C" fn(Display, Window, c_long) -> c_int,
    pub(super) set_wm_normal_hints: unsafe extern "C" fn(Display, Window, *mut XSizeHints),
    pub(super) set_class_hint: unsafe extern "C" fn(Display, Window, *mut XClassHint) -> c_int,
    pub(super) set_wm_hints: unsafe extern "C" fn(Display, Window, *mut XWMHints) -> c_int,
    pub(super) move_window: unsafe extern "C" fn(Display, Window, c_int, c_int) -> c_int,
    pub(super) intern_atom: unsafe extern "C" fn(Display, *const c_char, c_int) -> c_ulong,
    pub(super) send_event: unsafe extern "C" fn(Display, Window, c_int, c_long, *mut c_void) -> c_int,
    pub(super) sync: unsafe extern "C" fn(Display, c_int) -> c_int,
    pub(super) connection_number: unsafe extern "C" fn(Display) -> c_int,
    pub(super) pending: unsafe extern "C" fn(Display) -> c_int,
    pub(super) next_event: unsafe extern "C" fn(Display, *mut c_void) -> c_int,
    pub(super) grab_pointer: unsafe extern "C" fn(
        Display, Window, c_int, c_uint, c_int, c_int, Window, c_ulong, c_ulong,
    ) -> c_int,
    pub(super) ungrab_pointer: unsafe extern "C" fn(Display, c_ulong) -> c_int,
    pub(super) warp_pointer: unsafe extern "C" fn(
        Display, Window, Window, c_int, c_int, c_uint, c_uint, c_int, c_int,
    ) -> c_int,
    pub(super) query_pointer: unsafe extern "C" fn(
        Display, Window, *mut Window, *mut Window, *mut c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_uint,
    ) -> c_int,
    pub(super) lookup_string:
        unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut c_ulong, *mut c_void) -> c_int,
    pub(super) create_bitmap_from_data:
        unsafe extern "C" fn(Display, Window, *const c_char, c_uint, c_uint) -> c_ulong,
    pub(super) create_pixmap_cursor: unsafe extern "C" fn(
        Display, c_ulong, c_ulong, *mut XColor, *mut XColor, c_uint, c_uint,
    ) -> c_ulong,
    pub(super) define_cursor: unsafe extern "C" fn(Display, Window, c_ulong) -> c_int,
    pub(super) free_pixmap: unsafe extern "C" fn(Display, c_ulong) -> c_int,
    pub(super) free: unsafe extern "C" fn(*mut c_void) -> c_int,
    // ---- WM_DELETE_WINDOW and the icon
    pub(super) set_wm_protocols: unsafe extern "C" fn(Display, Window, *mut c_ulong, c_int) -> c_int,
    pub(super) change_property: unsafe extern "C" fn(
        Display, Window, c_ulong, c_ulong, c_int, c_int, *const u8, c_int,
    ) -> c_int,
    /// `XLookupKeysym(event, 0)`: the key's first keysym (group 1, level 1),
    /// whatever the modifiers. What the layout prints on the unshifted key,
    /// which is what the AGDK keycode and a modifier's self-bit are read from;
    /// `XLookupString`'s keysym already has Shift applied (`exclam` for the 1
    /// key), which is what made shifted keys vanish before.
    pub(super) lookup_keysym: unsafe extern "C" fn(*mut c_void, c_int) -> c_ulong,
    // ---- focus and grab bookkeeping ----
    /// `XQueryKeymap`: which keys are physically down right now, as a 256-bit
    /// vector indexed by X keycode. Read when another client's keyboard grab
    /// ends, so a key released while the grab had the keyboard is let go of
    /// here too -- see the `FOCUS_IN` arm.
    pub(super) query_keymap: unsafe extern "C" fn(Display, *mut c_char) -> c_int,
    /// `XQueryExtension`, for the XInputExtension's major opcode: a
    /// `GenericEvent` names its extension by opcode and nothing else.
    pub(super) query_extension: unsafe extern "C" fn(
        Display, *const c_char, *mut c_int, *mut c_int, *mut c_int,
    ) -> c_int,
    /// `XEventsQueued`, only ever with `QueuedAlready`: events Xlib has
    /// already read off the socket, counted without any I/O. See
    /// `pump_input_events` for why the socket alone is not enough to ask.
    pub(super) events_queued: unsafe extern "C" fn(Display, c_int) -> c_int,
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
    pub(super) set_detectable_auto_repeat: unsafe extern "C" fn(Display, c_int, *mut c_int) -> c_int,
    /// `XCheckMaskEvent`: take one queued event matching a mask, or return 0,
    /// without ever blocking. Only used when a lock engages, to empty the queue
    /// of core motion the free cursor produced before it; see `lock_pointer`.
    pub(super) check_mask_event: unsafe extern "C" fn(Display, c_long, *mut c_void) -> c_int,
}

impl Xlib {
    /// Open libX11 by soname and resolve every entry point, or say which one
    /// is missing. The handle is never closed.
    pub(super) fn load() -> Result<Self, String> {
        // SAFETY: a literal soname; immediate binding, never closed.
        let lib = unsafe { dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err("libX11.so.6 is not available".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                // SAFETY: the handle is open and the name NUL-terminated.
                let p = unsafe { dlsym(lib, concat!($name, "\0").as_ptr() as *const c_char) };
                if p.is_null() {
                    return Err(format!("libX11 has no {}", $name));
                }
                // SAFETY: each name is an Xlib export whose C prototype is the
                // field type it is stored in.
                unsafe { std::mem::transmute::<*mut c_void, _>(p) }
            }};
        }
        Ok(Xlib {
            init_threads: sym!("XInitThreads"),
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
            free: sym!("XFree"),
            set_wm_protocols: sym!("XSetWMProtocols"),
            change_property: sym!("XChangeProperty"),
            lookup_keysym: sym!("XLookupKeysym"),
            query_keymap: sym!("XQueryKeymap"),
            query_extension: sym!("XQueryExtension"),
            events_queued: sym!("XEventsQueued"),
            set_detectable_auto_repeat: sym!("XkbSetDetectableAutoRepeat"),
            check_mask_event: sym!("XCheckMaskEvent"),
        })
    }

    /// Send a format-32 `ClientMessage` about `window` to the root window,
    /// the way EWMH asks a window manager to change a managed window.
    ///
    /// SAFETY: `display` and `window` must be live handles of this library.
    pub(super) unsafe fn send_client_message(
        &self,
        display: Display,
        window: Window,
        message_type: c_ulong,
        data: [c_long; 5],
    ) {
        let mut buf = [0u8; XEVENT_SIZE];
        let p = buf.as_mut_ptr();
        // SAFETY: every offset is inside the 192-byte buffer; the layout is
        // `XClientMessageEvent` on LP64. Written unaligned, as bytes.
        unsafe {
            std::ptr::write_unaligned(p as *mut c_int, CLIENT_MESSAGE_TYPE);
            std::ptr::write_unaligned(p.add(8) as *mut c_ulong, 1);
            std::ptr::write_unaligned(p.add(16) as *mut c_int, 1);
            std::ptr::write_unaligned(p.add(24) as *mut Display, display);
            std::ptr::write_unaligned(p.add(32) as *mut Window, window);
            std::ptr::write_unaligned(p.add(40) as *mut c_ulong, message_type);
            std::ptr::write_unaligned(p.add(48) as *mut c_int, 32);
            for (i, word) in data.iter().enumerate() {
                std::ptr::write_unaligned(p.add(56 + 8 * i) as *mut c_long, *word);
            }
            let root = (self.default_root_window)(display);
            (self.send_event)(
                display,
                root,
                0,
                SUBSTRUCTURE_REDIRECT_MASK | SUBSTRUCTURE_NOTIFY_MASK,
                p as *mut c_void,
            );
        }
    }
}

/// One Xinerama head, in root-window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Head {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: i32,
    pub(super) height: i32,
}

/// `XineramaScreenInfo`.
#[repr(C)]
struct XineramaScreenInfo {
    screen_number: c_int,
    x_org: c_short,
    y_org: c_short,
    width: c_short,
    height: c_short,
}

/// The heads Xinerama reports on `display`, or `Err` when libXinerama cannot
/// be opened. An empty list is "no answer"; the caller places at 0,0.
///
/// SAFETY: `display` must be an open connection of `xlib`.
pub(super) unsafe fn xinerama_heads(xlib: &Xlib, display: Display) -> Result<Vec<Head>, ()> {
    // SAFETY: a literal soname, never closed.
    let lib = unsafe { dlopen(c"libXinerama.so.1".as_ptr(), RTLD_NOW) };
    if lib.is_null() {
        return Err(());
    }
    // SAFETY: the handle is open; the name is the documented export.
    let p = unsafe { dlsym(lib, c"XineramaQueryScreens".as_ptr()) };
    if p.is_null() {
        return Err(());
    }
    type QueryScreens = unsafe extern "C" fn(Display, *mut c_int) -> *mut XineramaScreenInfo;
    // SAFETY: `XineramaQueryScreens`'s prototype.
    let query: QueryScreens = unsafe { std::mem::transmute::<*mut c_void, QueryScreens>(p) };
    let mut count: c_int = 0;
    // SAFETY: `display` is open per this function's contract.
    let list = unsafe { query(display, &mut count) };
    if list.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: Xinerama returned `count` entries at `list`, which is freed
    // with `XFree` once they are copied.
    let heads = unsafe {
        let heads = std::slice::from_raw_parts(list, count.max(0) as usize)
            .iter()
            .map(|s| Head { x: s.x_org as i32, y: s.y_org as i32, width: s.width as i32, height: s.height as i32 })
            .collect();
        (xlib.free)(list as *mut c_void);
        heads
    };
    Ok(heads)
}
