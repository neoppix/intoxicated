//! The development control socket: how a harness drives the running client
//! and reads it back, without touching the X server.
//!
//! Off unless `INTOXICATED_DEV_CONTROL` is set to anything but empty or `0`.
//! Then [`start`] binds a Unix stream socket, `<profile>/devctl.sock` unless
//! `INTOXICATED_DEV_CONTROL_SOCKET` names another path, makes it `0600`, and
//! serves it on one thread. Connections are served one at a time; each line
//! in gets exactly one line out.
//!
//! - Input (`move`, `click`, `down`, `up`, `key`, `tap`, `text`, `scroll`,
//!   `fullscreen`, `windowed`, `focus`, `xkey`) is queued and answered `ok`
//!   at once. `ok` means queued, not applied: the looper pump applies the
//!   queue on its own thread, through the client's own input path, as if the
//!   events had come from the window ([`apply_queued`]). The engine's input
//!   natives are never called from the socket thread.
//! - State (`info`, `textbox`, `natives`, `loopers`, `pointerlock`, `latency`)
//!   is read and answered on the socket thread.
//! - `screenshot <path>` asks the Vulkan layer for the next frame.
//!   `depthshot <path>` asks for the next frame's scene depth as a grayscale
//!   PNG, with a screenshot of the same frame beside it, and `passes` writes
//!   the next frame's render passes to stderr. `ssr on|off|stats|get|reset|
//!   look <name>|intensity <x>|set <name> <value>` drives the screen-space
//!   reflections and answers with their state, their looks and their GPU
//!   time; `dof ...`, `bloom ...`, `lens ...` and `sharpen ...` take the
//!   same words for the depth of field, the bloom, the lens effects and the
//!   sharpening; `shaders mode <name>` sets every effect's switch and look
//!   from a mode (off, competitive, cinematic) and `shaders` reports them.
//! - `bussub`, `buslast` and `deeplink` reach the engine's message bus through
//!   `deeplink::dev_request`, which blocks this thread for at most 5 s.
//!
//! Nothing is sent to the X server: XTest or anything like it lands on
//! whatever has focus, which is the user's desktop. And nothing can be asked
//! but coordinates, counters and frames. The engine draws its interface
//! itself and hands the Android side no view hierarchy (see
//! ADR-019 -- so there is no semantic element list to offer
//! a harness), which works from coordinates and `screenshot` instead.
//!
//! The reply formats are fixed: harnesses parse them, and the fields of
//! `info` are only ever appended to.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// The switch.
const ENV: &str = "INTOXICATED_DEV_CONTROL";
/// The socket path override.
const SOCKET_ENV: &str = "INTOXICATED_DEV_CONTROL_SOCKET";

/// One queued input command.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// Pointer motion to window coordinates.
    Move { x: f32, y: f32 },
    /// One press or release of `button` at window coordinates.
    Button { x: f32, y: f32, down: bool, button: i32 },
    /// One key event: an evdev code and the modifier state.
    Key { down: bool, evdev: i32, modifiers: i32 },
    /// Text, typed one character at a time.
    Text(String),
    /// A wheel turn of `detents` at window coordinates.
    Scroll { x: f32, y: f32, detents: f32 },
    /// Fullscreen on or off.
    Fullscreen(bool),
    /// A real focus change, handled exactly as a `FocusIn`/`FocusOut` from
    /// the X server would be (keys and buttons released on a loss, the
    /// engine told). Nothing is sent to the X server.
    Focus(bool),
    /// An X key event built in this process and run through the window's own
    /// key dispatch: X keycode (evdev + 8) and X modifier state. Nothing is
    /// sent to the X server; see `x11_window::HostWindow::devctl_xkey`.
    XKey { down: bool, keycode: u32, state: u32 },
}

/// Queued commands, each with when its line was read, for the latency trace.
static QUEUE: Mutex<Vec<(std::time::Instant, Cmd)>> = Mutex::new(Vec::new());

/// Commands accepted since start. A `click` is three, a `tap` two.
static ACCEPTED: AtomicU64 = AtomicU64::new(0);

/// Queue one command and wake the pump to apply it.
fn push(c: Cmd) {
    {
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        q.push((std::time::Instant::now(), c));
    }
    ACCEPTED.fetch_add(1, Ordering::Relaxed);
    // Applied on the pump's next iteration; without the wake that is after
    // its wait, up to 50 ms on an idle client.
    crate::android::looper::wake_pump();
}

/// Everything queued so far, oldest first, leaving the queue empty.
pub fn drain() -> Vec<(std::time::Instant, Cmd)> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Whether the switch is on: valid UTF-8, neither empty nor exactly `0`.
fn switch_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// Whether this run asked for the socket. Read once.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| switch_on(std::env::var(ENV).ok().as_deref()))
}

/// Where the socket goes: `INTOXICATED_DEV_CONTROL_SOCKET` as given, else
/// `devctl.sock` in the active profile.
pub fn socket_path() -> PathBuf {
    match std::env::var_os(SOCKET_ENV) {
        Some(p) => PathBuf::from(p),
        None => crate::profile::active().join("devctl.sock"),
    }
}

/// Bind the socket at `path`, owner-only.
fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // A socket a killed client left behind. Removing it is safe because this
    // process already holds the profile's lock. Anything that is not a
    // socket is left where it is, and the bind below reports it.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket()) {
        let _ = std::fs::remove_file(path);
    }
    let listener = UnixListener::bind(path)?;
    // The umask's mode keeps other users out only while the socket is inside
    // the `0700` profile; an override path may not be. FreeBSD needs write
    // permission on a socket to connect to it.
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(listener)
}

/// Bind and start serving, if this run asked for the socket. Called once,
/// after the profile is latched and before the engine loads. A failure is
/// one line on stdout and never stops the launch.
pub fn start() {
    if !enabled() {
        return;
    }
    let path = socket_path();
    let started = bind(&path).and_then(|listener| {
        std::thread::Builder::new().name("intoxicated-devctl".into()).spawn(move || serve(listener))
    });
    match started {
        Ok(_) => println!("  devctl: listening on {}", path.display()),
        Err(e) => println!("  devctl: could not bind {} ({e}); continuing without it", path.display()),
    }
}

/// Serve connections one at a time, in the order they arrive.
fn serve(listener: UnixListener) {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => connection(stream),
            // Out of descriptors, say. Waiting a moment keeps a persistent
            // error from spinning this thread.
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

/// One connection: any number of lines, each answered before the next is
/// read. Ends silently at end of stream, on a write error, or on a line that
/// is not UTF-8.
fn connection(stream: UnixStream) {
    let Ok(read_side) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_side);
    let mut writer = stream;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            return;
        };
        let mut out = reply(line.trim());
        out.push('\n');
        if writer.write_all(out.as_bytes()).and_then(|()| writer.flush()).is_err() {
            return;
        }
    }
}

/// A coordinate or a detent count, as Rust's `f32` parsing reads it.
fn num(v: Option<&str>) -> Option<f32> {
    v?.parse().ok()
}

/// An integer: optional sign and decimal digits.
fn int(v: Option<&str>) -> Option<i32> {
    v?.parse().ok()
}

/// The reply to one line, already trimmed.
fn reply(line: &str) -> String {
    let mut it = line.split_whitespace();
    let Some(verb) = it.next() else {
        return "ok".into();
    };
    match verb {
        "ping" => "ok".into(),
        "info" => info(),
        "textbox" => textbox(),
        "loopers" => loopers(),
        // The window module's own report, verbatim: it owns the format and the
        // answer. See `x11_window::pointer_lock_report`.
        "pointerlock" => crate::android::x11_window::pointer_lock_report(),
        // What the engine's own pointer-lock getter is made to answer: 2 true,
        // 1 false, 0 the engine's real answer.
        "fakeenginelock" => {
            let (value, word) = match it.next() {
                Some("true") => (2, "true"),
                Some("false") => (1, "false"),
                Some("clear") => (0, "clear"),
                _ => return "err fakeenginelock <true|false|clear>".into(),
            };
            crate::android::input::FAKE_ENGINE_LOCK.store(value, Ordering::Relaxed);
            format!("ok fakeenginelock={word}")
        }
        // The natives the engine registered on a Java class. For a class it
        // never registered the linker answers `class not registered`, which
        // is counted as three words like any other answer.
        "natives" => match it.next() {
            Some(class) => {
                let list = intoxicated_linker_sys::game_activity::registered_natives(class);
                let words = list.split_whitespace().count();
                if words == 0 {
                    format!("ok class={class} natives=0")
                } else {
                    format!("ok class={class} natives={words} {list}")
                }
            }
            None => "err natives <java/class/Name>".into(),
        },
        // `buslast <id>`, `bussub <id>` and `deeplink <url>`: the engine's
        // message bus, read and written from the looper the way
        // `deeplink::tick` does it. What a join link publishes and what the
        // Play button publishes could not be compared without them; see
        // `deeplink::dev_request`.
        "buslast" | "bussub" | "deeplink" => {
            let rest: Vec<&str> = it.collect();
            crate::deeplink::dev_request(verb, &rest.join(" "))
        }
        "move" =>match (num(it.next()), num(it.next())) {
            (Some(x), Some(y)) => {
                push(Cmd::Move { x, y });
                "ok".into()
            }
            _ => "err move <x> <y>".into(),
        },
        "click" => match (num(it.next()), num(it.next())) {
            (Some(x), Some(y)) => {
                let button = int(it.next()).unwrap_or(1);
                push(Cmd::Move { x, y });
                push(Cmd::Button { x, y, down: true, button });
                push(Cmd::Button { x, y, down: false, button });
                "ok".into()
            }
            _ => "err click <x> <y> [button]".into(),
        },
        "down" | "up" => match (num(it.next()), num(it.next())) {
            (Some(x), Some(y)) => {
                let button = int(it.next()).unwrap_or(1);
                push(Cmd::Button { x, y, down: verb == "down", button });
                "ok".into()
            }
            _ => format!("err {verb} <x> <y> [button]"),
        },
        // A press only for exactly `down`; any other word, or none, is a
        // release.
        "key" => {
            let down = it.next() == Some("down");
            match int(it.next()) {
                Some(evdev) => {
                    let modifiers = int(it.next()).unwrap_or(0);
                    push(Cmd::Key { down, evdev, modifiers });
                    "ok".into()
                }
                None => "err key <down|up> <evdev-code> [modifiers]".into(),
            }
        }
        "tap" => match int(it.next()) {
            Some(evdev) => {
                let modifiers = int(it.next()).unwrap_or(0);
                push(Cmd::Key { down: true, evdev, modifiers });
                push(Cmd::Key { down: false, evdev, modifiers });
                "ok".into()
            }
            None => "err tap <evdev-code> [modifiers]".into(),
        },
        // The rest of the line, inner whitespace kept.
        "text" => match line[verb.len()..].trim() {
            "" => "err text <string>".into(),
            text => {
                push(Cmd::Text(text.to_string()));
                "ok".into()
            }
        },
        "fullscreen" | "windowed" => {
            push(Cmd::Fullscreen(verb == "fullscreen"));
            "ok".into()
        }
        "focus" => match it.next() {
            Some(v @ ("in" | "out")) => {
                push(Cmd::Focus(v == "in"));
                "ok".into()
            }
            _ => "err focus <in|out>".into(),
        },
        // `xkey down|up <x-keycode> [x-state]`: the X key path, driven from
        // inside the process. X keycodes are evdev + 8 (W is 25, Shift_L 50);
        // the state is X's modifier mask (1 Shift, 4 Control, 8 Alt).
        "xkey" => {
            let down = match it.next() {
                Some("down") => true,
                Some("up") => false,
                _ => return "err xkey <down|up> <x-keycode> [x-state]".into(),
            };
            match it.next().and_then(|v| v.parse::<u32>().ok()) {
                Some(keycode) if (8..=255).contains(&keycode) => {
                    let state = it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    push(Cmd::XKey { down, keycode, state });
                    "ok".into()
                }
                _ => "err xkey <down|up> <x-keycode 8..255> [x-state]".into(),
            }
        }
        // The input latency trace (`INTOXICATED_TRACE_INPUT_LATENCY=1`).
        "latency" => match it.next() {
            Some("reset") => {
                crate::android::input::latency_reset();
                "ok".into()
            }
            _ => format!("ok {}", crate::android::input::latency_summary()),
        },
        "scroll" => match (num(it.next()), num(it.next()), num(it.next())) {
            (Some(x), Some(y), Some(detents)) => {
                push(Cmd::Scroll { x, y, detents });
                "ok".into()
            }
            _ => "err scroll <x> <y> <detents>".into(),
        },
        // The capture the compositor would not give us. See `android::frame_capture`.
        "screenshot" => match it.next() {
            Some(path) => match crate::android::vk_layer::request_capture(path) {
                Ok(description) => format!("ok {description}"),
                Err(reason) => format!("err {reason}"),
            },
            None => "err screenshot <path>".into(),
        },
        // The next frame's render passes, to stderr. See `vk_layer::passes`.
        "passes" => match crate::android::vk_layer::request_pass_trace() {
            Ok(summary) => format!("ok {summary}"),
            Err(reason) => format!("err {reason}"),
        },
        // The scene depth of the next frame, beside its colour. See
        // `android::depth_capture`.
        "depthshot" => match it.next() {
            Some(path) => match crate::android::vk_layer::request_depth_capture(path) {
                Ok(description) => format!("ok {description}"),
                Err(reason) => format!("err {reason}"),
            },
            None => "err depthshot <path>".into(),
        },
        // Screen-space reflections, depth of field, bloom, lens effects and
        // sharpening: on, off, their settings and GPU cost; and the modes
        // that set them all. See `android::ssr`.
        verb @ ("ssr" | "dof" | "bloom" | "lens" | "sharpen" | "shaders") => {
            let args: Vec<&str> = it.collect();
            match crate::android::ssr::control_verb(verb, &args) {
                Some(Ok(state)) => format!("ok {state}"),
                Some(Err(reason)) => format!("err {reason}"),
                None => format!("err unknown verb {verb:?}"),
            }
        }
        other => format!("err unknown verb {other:?}"),
    }
}

/// `info`: presents, the frame summary when there is one, commands accepted,
/// the swapchain's extent, the pid, the worst frame and the present mode.
fn info() -> String {
    let presents = crate::android::glcount::QUEUE_PRESENT.load(Ordering::Relaxed);
    let (w, h) = crate::android::vk_layer::last_extent();
    // Fields are only ever appended here: harnesses parse this line.
    // `worst30s` is the longest frame since the last health line (which the
    // summary's 1024-frame window can miss), and `present_mode` the last
    // swapchain's.
    format!(
        "ok presents={presents}{} accepted={} extent={w}x{h} pid={} worst30s={:.1}ms present_mode={}",
        crate::android::present_stats::summary().map(|s| format!(" {s}")).unwrap_or_default(),
        ACCEPTED.load(Ordering::Relaxed),
        std::process::id(),
        crate::android::present_stats::worst_since_health(),
        crate::android::vk_layer::last_present_mode(),
    )
}

/// `textbox`: the focused box, its generation, and the text buffer as the
/// client holds it, the text itself redacted unless the input module's show
/// switch is on.
///
/// The value is the field `input` keeps for the focused box, read without
/// refilling it: exactly the text and caret the engine was last told.
fn textbox() -> String {
    let focus = intoxicated_linker_sys::game_activity::focused_textbox();
    let (text, caret) = crate::android::input::text_buffer_peek();
    format!(
        "ok focus={} gen={} rev={} chars={} bytes={} caret={caret}{} text={}",
        focus.map_or_else(|| "none".to_string(), |h| h.to_string()),
        intoxicated_linker_sys::game_activity::textbox_generation(),
        crate::android::input::text_buffer_revision(),
        text.chars().count(),
        text.len(),
        font_block(),
        crate::android::input::redacted(&text),
    )
}

/// The focused box's font-and-alignment ints.
///
/// **`xAlign`, `yAlign` and `multiline` are named from the engine's own
/// `TextBoxInfo` (the Java
/// constructor field order, 2026-08-30), and so is `i9` as `font`.**
/// `i10`/`i11` (`textInputType`, `returnKeyType`) are settled by the same
/// evidence but kept numbered here because nothing downstream reads them yet.
/// Before the alignment fields existed this line was the whole experiment for
/// finding the font slot: `intoxicated_textbox` twice, once on a restyled box and
/// once on a default one as the control, watching which int moved. That is
/// still how a slot this file gets wrong in a future Roblox build would be
/// caught.
///
/// Empty when the linker has no info for the focused box. None of these is
/// sensitive, so none is ever withheld.
fn font_block() -> String {
    let Some(info) = intoxicated_linker_sys::game_activity::focused_textbox_info() else {
        return String::new();
    };
    format!(
        " xAlign={} yAlign={} multiline={} font={} textInputType={} returnKeyType={} \
         textWrapped={}",
        info.x_alignment,
        info.y_alignment,
        info.multiline,
        info.font,
        info.text_input_type,
        info.return_key_type,
        info.text_wrapped,
    )
}

/// `loopers`: every looper in creation order, with how long since each last
/// found work and was last woken (`since_wake` equals `now` for one never
/// woken).
fn loopers() -> String {
    let (now, stats) = crate::android::looper::snapshot();
    let mut out = format!("ok now={now} loopers={}", stats.len());
    for s in &stats {
        out.push_str(&format!(
            " | tid={} fds={} [{}] polls={} events={} wakes={} since_event={}ms since_wake={}ms",
            s.tid,
            s.fds,
            // `fd:ident:flag` per registration, the pump's display
            // connection included, so a descriptor that stopped being
            // watched shows up here before it shows up as a hang.
            s.list,
            s.polls,
            s.events,
            s.wakes,
            now.saturating_sub(s.last_event_ms),
            now.saturating_sub(s.last_wake_ms),
        ));
    }
    out
}

/// Monotonic milliseconds since the surface first ran, for the input paths
/// that want an event time. Started lazily rather than at load so that a run
/// driven late does not begin with a large timestamp: the clock starts the
/// first time devctl needs one.
fn now_ms() -> i64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = *START.get_or_init(std::time::Instant::now);
    i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// Apply everything queued, in order, on the pump thread. Called by the
/// looper pump on every iteration, after the host input was drained, with
/// the GameActivity handle the pump holds.
pub fn apply_queued(handle: i64) {
    if !enabled() {
        return;
    }
    for (queued_at, cmd) in drain() {
        crate::android::input::note_devctl_command(queued_at);
        apply_cmd(handle, cmd);
    }
    crate::android::input::clear_input_origin();
}

/// Apply one command through the client's own input path, on the thread that
/// owns it (the looper pump). Shared by [`apply_queued`] and the macro player
/// in [`crate::macros`], so both reach the engine the one way, as if the
/// events had come from the window.
pub fn apply_cmd(handle: i64, cmd: Cmd) {
    match cmd {
        Cmd::Move { x, y } => crate::android::input::script_move(handle, x, y, now_ms()),
        Cmd::Button { x, y, down, button } => {
            crate::android::input::script_button(handle, x, y, down, button, now_ms())
        }
        Cmd::Key { down, evdev, modifiers } => crate::android::input::pass_key_event(down, evdev, modifiers),
        Cmd::Text(text) => {
            // Typed per character through the path a keystroke takes, so
            // the client's own text buffer and the editor both see it.
            // The handle is the GameActivity handle
            // `script_type` wants -- the handle the pump was given and
            // passes to `apply_queued`. Handing it the *focused textbox*
            // handle instead crashed the client on the first keystroke
            // (recorded in the inherited notes, not re-run).
            crate::android::input::script_type(handle, &text, now_ms());
        }
        Cmd::Scroll { x, y, detents } => crate::android::input::wheel(handle, x, y, 0.0, detents, now_ms()),
        Cmd::Fullscreen(on) => crate::android::backend_set_fullscreen(on),
        Cmd::Focus(focus_in) => {
            if let Some(w) = crate::android::x11_window::current() {
                w.devctl_focus(handle, focus_in);
            }
        }
        Cmd::XKey { down, keycode, state } => {
            if let Some(w) = crate::android::x11_window::current() {
                w.devctl_xkey(handle, down, keycode, state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The queue and the counter are process-wide; tests that use them take
    /// turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn queued() -> Vec<Cmd> {
        drain().into_iter().map(|(_, c)| c).collect()
    }

    fn mv(x: f32, y: f32) -> Cmd {
        Cmd::Move { x, y }
    }

    fn button(x: f32, y: f32, down: bool, button: i32) -> Cmd {
        Cmd::Button { x, y, down, button }
    }

    fn key(down: bool, evdev: i32, modifiers: i32) -> Cmd {
        Cmd::Key { down, evdev, modifiers }
    }

    #[test]
    fn every_line_gets_its_reply_and_queues_what_it_says() {
        let _serial = serial();
        drain();
        let cases: Vec<(&str, &str, Vec<Cmd>)> = vec![
            ("", "ok", vec![]),
            ("ping", "ok", vec![]),
            ("move 10 20", "ok", vec![mv(10.0, 20.0)]),
            ("move 10", "err move <x> <y>", vec![]),
            ("click 5 6", "ok", vec![mv(5.0, 6.0), button(5.0, 6.0, true, 1), button(5.0, 6.0, false, 1)]),
            ("click 5 6 x", "ok", vec![mv(5.0, 6.0), button(5.0, 6.0, true, 1), button(5.0, 6.0, false, 1)]),
            ("click 1e3 -2.5 3", "ok", vec![mv(1000.0, -2.5), button(1000.0, -2.5, true, 3), button(1000.0, -2.5, false, 3)]),
            ("down 1 2 3", "ok", vec![button(1.0, 2.0, true, 3)]),
            ("up 1 2", "ok", vec![button(1.0, 2.0, false, 1)]),
            ("up a b", "err up <x> <y> [button]", vec![]),
            ("down 1", "err down <x> <y> [button]", vec![]),
            ("key down 30 1", "ok", vec![key(true, 30, 1)]),
            ("key DOWN 30", "ok", vec![key(false, 30, 0)]),
            ("key up +30 junk", "ok", vec![key(false, 30, 0)]),
            ("key down", "err key <down|up> <evdev-code> [modifiers]", vec![]),
            ("key", "err key <down|up> <evdev-code> [modifiers]", vec![]),
            ("tap 28", "ok", vec![key(true, 28, 0), key(false, 28, 0)]),
            ("tap", "err tap <evdev-code> [modifiers]", vec![]),
            ("text   hello  world  ", "ok", vec![Cmd::Text("hello  world".into())]),
            ("text", "err text <string>", vec![]),
            ("fullscreen", "ok", vec![Cmd::Fullscreen(true)]),
            ("windowed", "ok", vec![Cmd::Fullscreen(false)]),
            ("focus in", "ok", vec![Cmd::Focus(true)]),
            ("focus out", "ok", vec![Cmd::Focus(false)]),
            ("focus sideways", "err focus <in|out>", vec![]),
            ("focus", "err focus <in|out>", vec![]),
            ("xkey down 7", "err xkey <down|up> <x-keycode 8..255> [x-state]", vec![]),
            ("xkey down 256", "err xkey <down|up> <x-keycode 8..255> [x-state]", vec![]),
            ("xkey down", "err xkey <down|up> <x-keycode 8..255> [x-state]", vec![]),
            ("xkey press 25", "err xkey <down|up> <x-keycode> [x-state]", vec![]),
            ("xkey up 255 1", "ok", vec![Cmd::XKey { down: false, keycode: 255, state: 1 }]),
            ("xkey down 8", "ok", vec![Cmd::XKey { down: true, keycode: 8, state: 0 }]),
            ("scroll 1 2", "err scroll <x> <y> <detents>", vec![]),
            ("scroll 1 2 -3", "ok", vec![Cmd::Scroll { x: 1.0, y: 2.0, detents: -3.0 }]),
            ("screenshot", "err screenshot <path>", vec![]),
            ("depthshot", "err depthshot <path>", vec![]),
            ("fakeenginelock maybe", "err fakeenginelock <true|false|clear>", vec![]),
            ("natives", "err natives <java/class/Name>", vec![]),
            ("frobnicate", "err unknown verb \"frobnicate\"", vec![]),
            ("PING", "err unknown verb \"PING\"", vec![]),
            ("say\"hi\\", "err unknown verb \"say\\\"hi\\\\\"", vec![]),
        ];
        for (line, want, cmds) in cases {
            assert_eq!(reply(line), want, "{line:?}");
            assert_eq!(queued(), cmds, "{line:?}");
        }
    }

    #[test]
    fn accepted_counts_each_queued_command() {
        let _serial = serial();
        drain();
        let before = ACCEPTED.load(Ordering::Relaxed);
        reply("click 1 2");
        reply("tap 30");
        reply("move 1");
        reply("ping");
        assert_eq!(ACCEPTED.load(Ordering::Relaxed) - before, 5);
        let info = reply("info");
        assert!(info.contains(&format!(" accepted={} ", before + 5)), "{info}");
        drain();
    }

    #[test]
    fn fakeenginelock_sets_the_override() {
        let lock = &crate::android::input::FAKE_ENGINE_LOCK;
        let _serial = serial();
        let was = lock.load(Ordering::Relaxed);
        assert_eq!(reply("fakeenginelock true"), "ok fakeenginelock=true");
        assert_eq!(lock.load(Ordering::Relaxed), 2);
        assert_eq!(reply("fakeenginelock false"), "ok fakeenginelock=false");
        assert_eq!(lock.load(Ordering::Relaxed), 1);
        assert_eq!(reply("fakeenginelock clear"), "ok fakeenginelock=clear");
        assert_eq!(lock.load(Ordering::Relaxed), 0);
        lock.store(was, Ordering::Relaxed);
    }

    #[test]
    fn state_replies_have_their_fields_in_order() {
        let info = reply("info");
        let pid = std::process::id();
        assert!(info.starts_with("ok presents="), "{info}");
        let fields = [" accepted=", " extent=", &format!(" pid={pid} "), " worst30s=", "ms present_mode="];
        let mut at = 0;
        for f in fields {
            at += info[at..].find(f).unwrap_or_else(|| panic!("{f} in {info}"));
        }
        let textbox = reply("textbox");
        assert!(textbox.starts_with("ok focus="), "{textbox}");
        for f in [" gen=", " rev=", " chars=", " bytes=", " caret=", " text=<"] {
            assert!(textbox.contains(f), "{f} in {textbox}");
        }
        assert!(reply("loopers").starts_with("ok now="));
        assert!(reply("natives java/lang/Object").starts_with("ok class=java/lang/Object natives="));
        assert!(reply("latency").starts_with("ok "));
        assert_eq!(reply("latency reset"), "ok");
        assert_eq!(reply("pointerlock"), "err no X11 window");
    }

    #[test]
    fn the_switch_is_off_unless_set_to_something_but_zero() {
        assert!(!switch_on(None));
        assert!(!switch_on(Some("")));
        assert!(!switch_on(Some("0")));
        assert!(switch_on(Some("1")));
        assert!(switch_on(Some("00")));
        assert!(switch_on(Some("yes")));
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("intoxicated-devctl-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_connection_carries_lines_until_it_ends() {
        let _serial = serial();
        drain();
        let dir = scratch("conn");
        let path = dir.join("sub/devctl.sock");
        let listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (s, _) = listener.accept().unwrap();
                connection(s);
            }
        });

        let mut c = UnixStream::connect(&path).unwrap();
        // CRLF, blank and padded lines, and a last line with no newline.
        c.write_all(b"ping\r\n   \n  frobnicate  \nmove 1 2").unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let mut got = String::new();
        std::io::Read::read_to_string(&mut c, &mut got).unwrap();
        assert_eq!(got, "ok\nok\nerr unknown verb \"frobnicate\"\nok\n");
        assert_eq!(queued(), vec![mv(1.0, 2.0)]);

        // A line that is not UTF-8 ends the connection without a reply.
        let mut c = UnixStream::connect(&path).unwrap();
        c.write_all(b"ping\n\xff\xfe\nping\n").unwrap();
        let mut got = String::new();
        std::io::Read::read_to_string(&mut c, &mut got).unwrap();
        assert_eq!(got, "ok\n");
        server.join().unwrap();

        // A stale socket is replaced; anything else in the way is not.
        drop(bind(&path).unwrap());
        let file = dir.join("not-a-socket");
        std::fs::write(&file, b"keep").unwrap();
        assert!(bind(&file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_applied_while_the_switch_is_off() {
        let _serial = serial();
        drain();
        reply("move 3 4");
        if !enabled() {
            apply_queued(0);
            assert_eq!(queued(), vec![mv(3.0, 4.0)]);
        }
        drain();
    }
}
