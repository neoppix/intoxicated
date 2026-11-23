//! The main pump: the loop on the process's main thread.
//!
//! The loader calls [`pump`] once, after bring-up, on the thread whose looper
//! it prepared before `initializeNativeCode` (so AGDK's main-work pipe is
//! registered there) and which owns the X11 connection. X11, the clipboard
//! and every engine input native are only ever called from here.
//!
//! **One iteration, in order:** the stop check; the health line every 30 s;
//! stall tracking; the join watchdog; host input (the window drains
//! everything Xlib has, then devctl's queue, then the idle keepalive); one
//! wait on this thread's looper, which is what runs AGDK's main-work callback
//! and what lets X11 traffic, a devctl command or a signal end the wait early;
//! then the housekeeping that must run on this thread whether or not the
//! window is focused: the cookie jar, the engine log, the deep link, the
//! clipboard and the battery report.
//!
//! **The wait** is 50 ms while the display connection is registered on the
//! looper, 8 ms when it is not, and 0 while Xlib still holds events it read
//! off the socket (the socket cannot show those, so waiting on it would leave
//! them queued until the next unrelated byte).
//!
//! **Stopping.** A terminating signal, the window manager's close request
//! (unless `INTOXICATED_NO_CLOSE_EXIT` is set), a stop request from inside
//! the process or the `--run` deadline end the loop, checked in that order at
//! the top of each iteration. All four leave the same way: the cookie jar is
//! saved, then the engine is shut down in Android's order under the teardown
//! watchdog, then a 500 ms grace keeps its callbacks running.
//!
//! **Exit statuses this module produces.** The loader exits 0 after the pump
//! returns. A second SIGINT or SIGTERM ends the process at once with 128 plus
//! the signal number (130 for Ctrl-C twice, 143 for SIGTERM), the statuses a
//! shell reports for a process those signals killed, so the launcher can tell
//! a user quitting from a failed start. A shutdown that hangs past the
//! teardown limit ends with 124.

use std::ffi::c_int;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{prepare, Looper, Outs, ALOOPER_EVENT_INPUT, ALOOPER_PREPARE_ALLOW_NON_CALLBACKS, PUMP_LOOPER};

/// The ident of the pump's display registration ("Into"). Nobody reads it;
/// it makes the registration easy to spot in devctl's `loopers`.
pub const PUMP_IDENT: c_int = 0x496e_746f;
/// The wait while the display connection is registered.
pub const WATCHED_WAIT_MS: c_int = 50;
/// The wait when it is not (no window, or the registration failed).
pub const UNWATCHED_WAIT_MS: c_int = 8;
pub const HEALTH_PERIOD: Duration = Duration::from_secs(30);
pub const STALL_AFTER: Duration = Duration::from_secs(5);
pub const JOIN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(60);
pub const GRACE: Duration = Duration::from_millis(500);
pub const TEARDOWN_LIMIT_DEFAULT: Duration = Duration::from_secs(10);

pub const NO_CLOSE_EXIT_VARIABLE: &str = "INTOXICATED_NO_CLOSE_EXIT";
pub const JOIN_TIMEOUT_VARIABLE: &str = "INTOXICATED_JOIN_TIMEOUT";
pub const TEARDOWN_TIMEOUT_VARIABLE: &str = "INTOXICATED_TEARDOWN_TIMEOUT_S";

// ------------------------------------------------------------ flags

/// The first terminating signal's number, 0 before one arrives.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);
/// Something in the process asked the pump to stop.
static QUIT: AtomicBool = AtomicBool::new(false);
/// The command line carried `--join-url`.
static JOIN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Arms the join watchdog. The loader calls it when it parses `--join-url`:
/// only a join this process asked for is watched.
pub fn note_join_requested() {
    JOIN_REQUESTED.store(true, Ordering::Release);
}

/// Asks the pump to end the run through its normal teardown. A flag, not an
/// exit; `INTOXICATED_NO_CLOSE_EXIT` does not affect it.
pub fn request_quit() {
    QUIT.store(true, Ordering::Release);
    super::wake_pump();
}

/// Why the run ends, first match wins, or `None` to carry on. The deadline is
/// checked separately and prints nothing.
pub fn stop_reason(signalled: bool, window_closed: bool, quit: bool, honour_close: bool) -> Option<&'static str> {
    if signalled {
        Some("a terminating signal")
    } else if window_closed && honour_close {
        Some("the window closing")
    } else if quit {
        Some("something asking the pump to stop")
    } else {
        None
    }
}

// ------------------------------------------------------------ signals

/// The first SIGINT or SIGTERM only records itself; the pump sees it at the
/// top of its next iteration (the looper also watches both signals with
/// `EVFILT_SIGNAL`, so the wait ends at once). A second one, of either kind
/// and at any later time, ends the process now with 128 plus its number.
/// Async-signal-safe: one atomic swap and `_exit`.
extern "C" fn on_signal(sig: c_int) {
    if SIGNALLED.swap(sig, Ordering::SeqCst) != 0 {
        // SAFETY: `_exit` is async-signal-safe.
        unsafe { libc::_exit(128 + sig) }
    }
}

/// SIGINT and SIGTERM to [`on_signal`], replacing whatever the engine put
/// there through the bionic layer. The handler persists and interrupted
/// calls restart where the kernel allows it (BSD `signal()` semantics).
fn install_signal_handlers() {
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: a zeroed `sigaction` is valid; the handler is a plain
        // `extern "C" fn(c_int)`, as `sa_handler` expects.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(c_int) as libc::sighandler_t;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                eprintln!("[android] could not install the handler for signal {sig}: {}", std::io::Error::last_os_error());
            }
        }
    }
}

// ------------------------------------------------------------ what the pump drives

/// One engine call of the teardown, in Android's own shutdown order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    FocusLost,
    Pause,
    SurfaceDestroyed,
    Stop,
    Terminate,
}

pub const TEARDOWN: [Step; 5] = [Step::FocusLost, Step::Pause, Step::SurfaceDestroyed, Step::Stop, Step::Terminate];

impl Step {
    /// As the teardown lines name it.
    pub fn name(self) -> &'static str {
        match self {
            Step::FocusLost => "onWindowFocusChangedNative(false)",
            Step::Pause => "onPauseNative",
            Step::SurfaceDestroyed => "onSurfaceDestroyedNative",
            Step::Stop => "onStopNative",
            Step::Terminate => "terminateNativeCode",
        }
    }
}

/// Everything outside this module that the pump calls, so the loop can run in
/// a test without an engine or a window.
pub(crate) trait Host {
    /// The process-wide `vkQueuePresentKHR` count.
    fn presents(&self) -> u64;
    /// The frame-pacing summary, once there is one, with the worst frame
    /// since the previous call (`worst30s=`). Called once per health line.
    fn frame_summary(&self) -> Option<String>;
    /// How many times the engine reported a place finished loading.
    fn places_loaded(&self) -> u32;
    /// Whether the window manager asked the window to close.
    fn window_closed(&self) -> bool;
    /// The display connection's descriptor, if a window is open.
    fn display_fd(&self) -> Option<c_int>;
    /// Subscribes the clipboard to the engine's `setClipboardText`.
    fn arm_clipboard(&mut self);
    /// Host input, devctl's queue and the keepalive. Never blocks.
    fn input(&mut self, handle: i64);
    /// Whether Xlib holds events the socket cannot show.
    fn input_backlog(&self) -> bool;
    /// The per-iteration work after the wait.
    fn housekeeping(&mut self);
    /// The final cookie save, before the engine's shutdown descent.
    fn save_cookies(&mut self);
    /// One teardown call: `Ok(None)` when the engine never registered it.
    fn teardown_step(&mut self, handle: i64, step: Step) -> Result<Option<()>, String>;
}

/// The real thing.
struct Live {
    keepalive: bool,
}

impl Host for Live {
    fn presents(&self) -> u64 {
        crate::android::glcount::QUEUE_PRESENT.load(Ordering::Relaxed)
    }

    fn frame_summary(&self) -> Option<String> {
        crate::android::present_stats::health_summary()
    }

    fn places_loaded(&self) -> u32 {
        intoxicated_linker_sys::game_activity::games_loaded()
    }

    fn window_closed(&self) -> bool {
        crate::android::window_closed()
    }

    fn display_fd(&self) -> Option<c_int> {
        crate::android::connection_fd()
    }

    fn arm_clipboard(&mut self) {
        crate::android::clipboard::arm();
    }

    fn input(&mut self, handle: i64) {
        crate::android::pump_input_events(handle);
        crate::devctl::apply_queued(handle);
        if self.keepalive {
            crate::android::input::idle_keepalive();
        }
    }

    fn input_backlog(&self) -> bool {
        crate::android::input_backlog()
    }

    fn housekeeping(&mut self) {
        crate::cookie_jar::tick();
        crate::engine_log::poll();
        crate::deeplink::tick();
        crate::android::clipboard::pump_pending();
        crate::power::tick();
    }

    fn save_cookies(&mut self) {
        crate::cookie_jar::save("teardown");
    }

    fn teardown_step(&mut self, handle: i64, step: Step) -> Result<Option<()>, String> {
        use intoxicated_linker_sys::game_activity as ga;
        match step {
            Step::FocusLost => ga::window_focus(handle, false),
            other => ga::lifecycle(handle, other.name()),
        }
    }
}

// ------------------------------------------------------------ lines

/// `[intoxicated] health: ...`. `secs` is the unrounded time since the last
/// line (or pump entry); the rate is taken over it.
pub fn health_line(drawn: u64, secs: f64, total: u64, summary: Option<&str>) -> String {
    let rate = if secs > 0.0 { drawn as f64 / secs } else { 0.0 };
    let mut line =
        format!("[intoxicated] health: {drawn} presents in {}s ({rate:.1}/s), {total} total", secs.round() as u64);
    if drawn == 0 {
        line.push_str(" -- nothing was drawn");
    }
    if let Some(s) = summary {
        line.push_str("; ");
        line.push_str(s);
    }
    line
}

pub fn stall_line(secs: u64, frames: u64, pid: u32) -> String {
    format!(
        "[android] the engine has presented nothing for {secs}s after {frames} frames. The pump is still \
         running, so this is not the idle throttle. Take a backtrace -- \
         `lldb -p {pid} -b -o 'thread backtrace all -c 16'`."
    )
}

pub fn join_line(secs: u64) -> String {
    format!(
        "[android] a join was requested {secs}s ago and the engine has not reported a loaded place. It may \
         still be connecting -- this is a timeout, not a failure. If the window is showing the app shell \
         rather than a place, the join did not take. Set {JOIN_TIMEOUT_VARIABLE} to change how long this waits."
    )
}

/// The limit as the shortest decimal: `10s`, `0.3s`.
pub fn overrun_line(limit: Duration) -> String {
    format!(
        "[android] the engine's shutdown has not returned after {}s; exiting without it, status 124. Cookies \
         were saved before it began. {TEARDOWN_TIMEOUT_VARIABLE}=off disables this.",
        limit.as_secs_f64()
    )
}

/// `INTOXICATED_JOIN_TIMEOUT`: whole seconds above 0, anything else 60.
pub fn join_timeout(v: Option<&str>) -> Duration {
    match v.and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(n) if n > 0 => Duration::from_secs(n),
        _ => JOIN_TIMEOUT_DEFAULT,
    }
}

/// `INTOXICATED_TEARDOWN_TIMEOUT_S`: `None` is disarmed (`off` or `0`).
/// Unset, empty or anything that is not a whole number is 10 s, so a typo
/// never disarms it.
pub fn teardown_limit(v: Option<&str>) -> Option<Duration> {
    let v = v.map(str::trim).unwrap_or("");
    match v {
        "off" | "0" => None,
        _ => Some(v.parse::<u64>().map(Duration::from_secs).unwrap_or(TEARDOWN_LIMIT_DEFAULT)),
    }
}

fn flush() {
    let _ = std::io::stdout().flush();
}

// ------------------------------------------------------------ the timers

/// The health line, stall tracking and the join watchdog.
struct Monitor {
    entered: Instant,
    last_health: Instant,
    presents_at_health: u64,
    last_presents: u64,
    last_change: Instant,
    stall_reported: bool,
    join_watched: bool,
    join_timeout: Duration,
    join_reported: bool,
}

impl Monitor {
    fn new(entered: Instant, join_watched: bool, join_timeout: Duration) -> Monitor {
        Monitor {
            entered,
            last_health: entered,
            presents_at_health: 0,
            last_presents: 0,
            last_change: entered,
            stall_reported: false,
            join_watched,
            join_timeout,
            join_reported: false,
        }
    }

    fn tick(&mut self, host: &dyn Host, now: Instant) {
        let total = host.presents();

        let since = now.duration_since(self.last_health);
        if since >= HEALTH_PERIOD {
            let drawn = total.saturating_sub(self.presents_at_health);
            let summary = host.frame_summary();
            println!("{}", health_line(drawn, since.as_secs_f64(), total, summary.as_deref()));
            flush();
            self.last_health = now;
            self.presents_at_health = total;
        }

        if total != self.last_presents {
            self.last_presents = total;
            self.last_change = now;
        } else if total > 0 && !self.stall_reported {
            let still = now.duration_since(self.last_change);
            if still >= STALL_AFTER {
                println!("{}", stall_line(still.as_secs_f64().round() as u64, total, std::process::id()));
                flush();
                self.stall_reported = true;
            }
        }

        if self.join_watched && !self.join_reported {
            let waited = now.duration_since(self.entered);
            if waited >= self.join_timeout && host.places_loaded() == 0 {
                println!("{}", join_line(waited.as_secs_f64().round() as u64));
                flush();
                self.join_reported = true;
            }
        }
    }
}

// ------------------------------------------------------------ the loop

/// Runs the main pump for `duration` (zero: until something stops it), then
/// shuts the engine down. `handle` is the GameActivity handle; without it
/// there is no display watch, no input, no devctl, no keepalive and no
/// teardown.
pub fn pump(duration: Duration, handle: Option<i64>) {
    let policy = crate::android::input::throttle_policy();
    let mut live = Live { keepalive: crate::android::input::keepalive_wanted(policy, None, None) };
    run(&mut live, duration, handle);
}

/// The pump against any [`Host`].
pub(crate) fn run(host: &mut dyn Host, duration: Duration, handle: Option<i64>) {
    install_signal_handlers();
    let entered = Instant::now();
    let deadline = (!duration.is_zero()).then(|| entered + duration);

    let looper = prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
    if let Some(l) = looper {
        PUMP_LOOPER.store(l as *const Looper as *mut Looper, Ordering::Release);
        for sig in [libc::SIGINT, libc::SIGTERM] {
            // Without it a signal taken by another thread is seen at the end
            // of the current wait instead of at once; nothing else changes.
            let _ = l.waits.watch_signal(sig);
        }
    }

    // The display connection, as a real table entry with an ident nobody
    // reads: X11 traffic then ends the wait, so input is drained as it
    // arrives instead of after the idle period.
    let watched = match (handle, looper, host.display_fd()) {
        (Some(_), Some(l), Some(fd)) => (l.add(fd, PUMP_IDENT, ALOOPER_EVENT_INPUT, None, 0) == 1).then_some(fd),
        _ => None,
    };
    if handle.is_some() {
        host.arm_clipboard();
    }

    let honour_close = std::env::var_os(NO_CLOSE_EXIT_VARIABLE).is_none();
    let join_watched = JOIN_REQUESTED.load(Ordering::Acquire);
    let mut monitor =
        Monitor::new(entered, join_watched, join_timeout(std::env::var(JOIN_TIMEOUT_VARIABLE).ok().as_deref()));
    let wait_ms = if watched.is_some() { WATCHED_WAIT_MS } else { UNWATCHED_WAIT_MS };

    loop {
        let reason = stop_reason(
            SIGNALLED.load(Ordering::SeqCst) != 0,
            host.window_closed(),
            QUIT.load(Ordering::Acquire),
            honour_close,
        );
        if let Some(reason) = reason {
            println!("[android] ending the run: {reason}");
            flush();
            break;
        }
        let now = Instant::now();
        if deadline.is_some_and(|d| now >= d) {
            break;
        }
        monitor.tick(host, now);

        if let Some(h) = handle {
            host.input(h);
        }
        let timeout = if host.input_backlog() { 0 } else { wait_ms };
        wait(looper, timeout);
        host.housekeeping();
    }

    // Nothing drains the display any more; left registered, X11 traffic
    // would turn the grace period's waits into a spin.
    if let (Some(l), Some(fd)) = (looper, watched) {
        l.remove(fd);
    }
    // Before the shutdown descent: the jar lives in the engine.
    host.save_cookies();
    if let Some(h) = handle {
        teardown(host, looper, h);
    }
}

/// One wait on the pump's looper, or a plain sleep if it has none.
fn wait(looper: Option<&'static Looper>, timeout_ms: c_int) {
    match looper {
        Some(l) => {
            l.poll(timeout_ms, Outs::NONE);
        }
        None => std::thread::sleep(Duration::from_millis(timeout_ms.max(0) as u64)),
    }
}

/// The five engine calls under the watchdog, then the grace period.
pub(super) fn teardown(host: &mut dyn Host, looper: Option<&'static Looper>, handle: i64) {
    let limit = teardown_limit(std::env::var(TEARDOWN_TIMEOUT_VARIABLE).ok().as_deref());
    let watchdog = limit.and_then(|limit| match Watchdog::arm(limit, overrun_exit) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("[android] teardown watchdog not started ({e}); a hung shutdown will not be cut off");
            None
        }
    });
    for step in TEARDOWN {
        match host.teardown_step(handle, step) {
            Ok(Some(())) => crate::android::trace(format_args!("{}", step.name())),
            Ok(None) => eprintln!("[android] {}: not registered", step.name()),
            Err(e) => eprintln!("[android] {} failed: {e}", step.name()),
        }
    }
    if let Some(w) = watchdog {
        w.stand_down();
    }
    grace(looper);
}

/// Keeps the looper's callbacks running for [`GRACE`], in waits of at most
/// 50 ms. No input, no housekeeping.
fn grace(looper: Option<&'static Looper>) {
    let end = Instant::now() + GRACE;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let ms = left.as_millis().clamp(1, WATCHED_WAIT_MS as u128) as c_int;
        wait(looper, ms);
    }
}

// ------------------------------------------------------------ the watchdog

/// Ends the process with 124 from the watchdog thread. The line was built
/// before the wait, and goes out with one write(2): nothing here takes a lock
/// or allocates, since the stuck thread may hold anything.
pub(super) fn overrun_exit(line: &[u8]) {
    // SAFETY: a live buffer; `_exit` runs no handlers.
    unsafe {
        libc::write(2, line.as_ptr().cast(), line.len());
        libc::_exit(124);
    }
}

/// A thread that cuts off a shutdown that does not return. Armed around the
/// five teardown calls only; standing it down, or dropping it on any path
/// (an unwind included), lets the thread end at once.
pub(crate) struct Watchdog {
    tx: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<bool>>,
}

impl Watchdog {
    /// Starts the thread. `on_overrun` gets the overrun line (newline
    /// included) if `limit` passes first; the thread's result says whether it
    /// did.
    pub(crate) fn arm(limit: Duration, on_overrun: fn(&[u8])) -> std::io::Result<Watchdog> {
        let (tx, rx) = mpsc::channel::<()>();
        let line = format!("{}\n", overrun_line(limit)).into_bytes();
        let thread = std::thread::Builder::new().name("teardown-watchdog".into()).spawn(move || {
            match rx.recv_timeout(limit) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    on_overrun(&line);
                    true
                }
                // Answered, or the guard went away.
                _ => false,
            }
        })?;
        Ok(Watchdog { tx: Some(tx), thread: Some(thread) })
    }

    /// The five calls returned.
    pub(crate) fn stand_down(mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }

    /// The thread, for a test to join.
    #[cfg(test)]
    pub(crate) fn take_thread(&mut self) -> Option<JoinHandle<bool>> {
        self.thread.take()
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        // Dropping the sender wakes the thread with `Disconnected`.
        self.tx.take();
        // Detached: joining would only add the thread's exit to the close.
        self.thread.take();
    }
}

#[cfg(test)]
pub(crate) fn reset_flags_for_test() {
    SIGNALLED.store(0, Ordering::SeqCst);
    QUIT.store(false, Ordering::SeqCst);
}
