//! The looper contract and the pump's rules, without an engine or a window.
//!
//! Numbered as the spec's test vectors. Every looper test runs on a thread of
//! its own, because a looper belongs to the thread that prepared it. Times
//! have slack for a loaded machine. The two child-process tests re-run this
//! test binary on a test that does nothing unless `INTOXICATED_LOOPER_CHILD`
//! names it.

use super::pump::{self, Host, Step, Watchdog};
use super::*;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicI32;
use std::sync::Arc;

/// Runs `f` on a new thread, which has no looper yet.
fn fresh<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(f).join().expect("the test thread")
}

fn poll(timeout: c_int) -> c_int {
    alooper_poll_once(timeout, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
}

fn timed(f: impl FnOnce() -> c_int) -> (c_int, Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

fn eventfd(initial: u32) -> c_int {
    // SAFETY: plain values.
    let fd = unsafe { libc::eventfd(initial, 0) };
    assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
    fd
}

fn pipe() -> (c_int, c_int) {
    let mut fds = [0; 2];
    // SAFETY: a two-element array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    (fds[0], fds[1])
}

fn close(fd: c_int) {
    // SAFETY: a descriptor the test owns.
    unsafe { libc::close(fd) };
}

fn drain_eventfd(fd: c_int) -> u64 {
    let mut v = 0u64;
    // SAFETY: eight bytes into a u64.
    let n = unsafe { libc::read(fd, (&mut v as *mut u64).cast(), 8) };
    assert_eq!(n, 8);
    v
}

fn bump_eventfd(fd: c_int) {
    let v = 1u64;
    // SAFETY: eight bytes from a u64.
    assert_eq!(unsafe { libc::write(fd, (&v as *const u64).cast(), 8) }, 8);
}

/// What a callback saw, and what it answers.
struct Recorder {
    calls: Mutex<Vec<(c_int, c_int, usize)>>,
    answer: AtomicI32,
    /// For vector 11: the looper and the descriptor to register instead.
    looper: AtomicUsize,
    other: AtomicI32,
}

impl Recorder {
    fn new(answer: c_int) -> Arc<Recorder> {
        Arc::new(Recorder {
            calls: Mutex::new(Vec::new()),
            answer: AtomicI32::new(answer),
            looper: AtomicUsize::new(0),
            other: AtomicI32::new(-1),
        })
    }

    fn data(self: &Arc<Self>) -> *mut c_void {
        Arc::as_ptr(self) as *mut c_void
    }

    fn calls(&self) -> Vec<(c_int, c_int, usize)> {
        lock(&self.calls).clone()
    }
}

unsafe extern "C" fn record(fd: c_int, events: c_int, data: *mut c_void) -> c_int {
    // SAFETY: every test passes a live `Recorder` as the data.
    let r = unsafe { &*(data as *const Recorder) };
    lock(&r.calls).push((fd, events, data as usize));
    r.answer.load(Ordering::SeqCst)
}

/// Removes its own fd and registers `other` as ident 7, from inside the
/// dispatch.
unsafe extern "C" fn swap_registration(fd: c_int, events: c_int, data: *mut c_void) -> c_int {
    // SAFETY: as for `record`.
    let r = unsafe { &*(data as *const Recorder) };
    lock(&r.calls).push((fd, events, data as usize));
    let looper = r.looper.load(Ordering::SeqCst) as *mut c_void;
    assert_eq!(alooper_remove_fd(looper, fd), 1);
    assert_eq!(alooper_add_fd(looper, r.other.load(Ordering::SeqCst), 7, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
    1
}

fn this() -> &'static Looper {
    current().expect("a looper on this thread")
}

// ------------------------------------------------------------ 1-6: lifetime, timeouts, wake

#[test]
fn v01_prepare_and_for_thread() {
    fresh(|| {
        assert!(alooper_for_thread().is_null(), "forThread never creates");
        let a = alooper_prepare(0);
        assert!(!a.is_null());
        assert_eq!(alooper_for_thread(), a);
        assert_eq!(alooper_prepare(0), a);
        let b = fresh(|| alooper_prepare(0) as usize);
        assert_ne!(b, 0);
        assert_ne!(b, a as usize, "each thread has its own");
    });
}

#[test]
fn v02_poll_without_a_looper_is_an_error_at_once() {
    fresh(|| {
        let (r, took) = timed(|| poll(1000));
        assert_eq!(r, ALOOPER_POLL_ERROR);
        assert!(took < Duration::from_millis(20), "{took:?}");
    });
}

#[test]
fn v03_timeouts_on_an_empty_looper() {
    fresh(|| {
        alooper_prepare(0);
        let (r, took) = timed(|| poll(50));
        assert_eq!(r, ALOOPER_POLL_TIMEOUT);
        assert!(took >= Duration::from_millis(40), "{took:?}");
        let (r, took) = timed(|| poll(0));
        assert_eq!(r, ALOOPER_POLL_TIMEOUT);
        assert!(took < Duration::from_millis(20), "{took:?}");
    });
}

#[test]
fn v04_an_infinite_wait_stops_at_the_ceiling_and_says_timeout() {
    fresh(|| {
        alooper_prepare(0);
        let before = this().ceiling_expired.load(Ordering::SeqCst);
        let (r, took) = timed(|| poll(-1));
        assert_eq!(r, ALOOPER_POLL_TIMEOUT, "never an invented wake");
        assert!(took >= Duration::from_millis(40) && took < Duration::from_secs(5), "{took:?}");
        assert_eq!(this().ceiling_expired.load(Ordering::SeqCst), before + 1);
    });
}

#[test]
fn v05_wakes_coalesce() {
    fresh(|| {
        let l = alooper_prepare(0);
        alooper_wake(l);
        let (r, took) = timed(|| poll(-1));
        assert_eq!(r, ALOOPER_POLL_WAKE);
        assert!(took < Duration::from_millis(50), "{took:?}");
        for _ in 0..3 {
            alooper_wake(l);
        }
        assert_eq!(poll(0), ALOOPER_POLL_WAKE);
        assert_eq!(poll(0), ALOOPER_POLL_TIMEOUT);
    });
}

#[test]
fn v06_a_wake_from_another_thread_ends_the_wait() {
    fresh(|| {
        let l = alooper_prepare(0) as usize;
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            alooper_wake(l as *mut c_void);
        });
        let (r, took) = timed(|| poll(1000));
        waker.join().unwrap();
        assert_eq!(r, ALOOPER_POLL_WAKE);
        assert!(took < Duration::from_millis(200), "{took:?}");
    });
}

// ------------------------------------------------------------ 7-13: registrations

#[test]
fn v07_an_ident_registration_comes_back_until_removed() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let fd = eventfd(1);
        let d = 0xd00d_usize as *mut c_void;
        assert_eq!(alooper_add_fd(l, fd, 42, ALOOPER_EVENT_INPUT, None, d), 1);
        for _ in 0..2 {
            let (mut ofd, mut oev, mut odata) = (-1, 0, std::ptr::null_mut());
            assert_eq!(alooper_poll_once(500, &mut ofd, &mut oev, &mut odata), 42, "not drained: again");
            assert_eq!(ofd, fd);
            assert_ne!(oev & ALOOPER_EVENT_INPUT, 0);
            assert_eq!(odata, d);
        }
        assert_eq!(alooper_remove_fd(l, fd), 1);
        assert_eq!(alooper_remove_fd(l, fd), 0);
        assert_eq!(poll(50), ALOOPER_POLL_TIMEOUT);
        close(fd);
    });
}

#[test]
fn v08_null_out_parameters_are_allowed() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let fd = eventfd(1);
        assert_eq!(alooper_add_fd(l, fd, 5, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        assert_eq!(poll(500), 5);
        alooper_remove_fd(l, fd);
        close(fd);
    });
}

#[test]
fn v09_a_callback_that_keeps_its_registration() {
    fresh(|| {
        let l = alooper_prepare(0);
        let fd = eventfd(1);
        let rec = Recorder::new(1);
        assert_eq!(alooper_add_fd(l, fd, 99, ALOOPER_EVENT_INPUT, Some(record), rec.data()), 1);
        let (mut ofd, mut oev, mut odata) = (-7, -7, 0x77 as *mut c_void);
        assert_eq!(alooper_poll_once(500, &mut ofd, &mut oev, &mut odata), ALOOPER_POLL_CALLBACK);
        assert_eq!((ofd, oev, odata), (-7, -7, 0x77 as *mut c_void), "out parameters untouched");
        let calls = rec.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, fd);
        assert_ne!(calls[0].1 & ALOOPER_EVENT_INPUT, 0);
        assert_eq!(calls[0].2, rec.data() as usize);
        assert_eq!(poll(500), ALOOPER_POLL_CALLBACK, "not drained: it runs again");
        assert_eq!(rec.calls().len(), 2);
        alooper_remove_fd(l, fd);
        close(fd);
    });
}

#[test]
fn v10_a_callback_that_returns_zero_is_unregistered() {
    fresh(|| {
        let l = alooper_prepare(0);
        let fd = eventfd(1);
        let rec = Recorder::new(0);
        assert_eq!(alooper_add_fd(l, fd, 0, ALOOPER_EVENT_INPUT, Some(record), rec.data()), 1);
        assert_eq!(this().registrations().0, 1);
        assert_eq!(poll(500), ALOOPER_POLL_CALLBACK);
        assert_eq!(rec.calls().len(), 1);
        assert_eq!(poll(50), ALOOPER_POLL_TIMEOUT, "still readable, no longer registered");
        assert_eq!(alooper_remove_fd(l, fd), 0);
        assert_eq!(this().registrations(), (0, String::new()));
        assert_eq!(this().registered.load(Ordering::SeqCst), 0);
        close(fd);
    });
}

#[test]
fn v11_a_callback_may_change_the_table_it_is_dispatched_from() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let first = eventfd(1);
        let other = eventfd(1);
        let rec = Recorder::new(1);
        rec.looper.store(l as usize, Ordering::SeqCst);
        rec.other.store(other, Ordering::SeqCst);
        assert_eq!(alooper_add_fd(l, first, 0, ALOOPER_EVENT_INPUT, Some(swap_registration), rec.data()), 1);
        assert_eq!(poll(500), ALOOPER_POLL_CALLBACK);
        assert_eq!(rec.calls().len(), 1);
        assert_eq!(this().registrations(), (1, format!("{other}:7:-")));
        assert_eq!(poll(500), 7, "the new registration works");
        assert_eq!(rec.calls().len(), 1, "the removed one never runs again");
        alooper_remove_fd(l, other);
        close(first);
        close(other);
    });
}

#[test]
fn v12_a_closed_writer_is_a_hangup() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let (r, w) = pipe();
        assert_eq!(alooper_add_fd(l, r, 3, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        close(w);
        let mut events = 0;
        assert_eq!(alooper_poll_once(500, std::ptr::null_mut(), &mut events, std::ptr::null_mut()), 3);
        assert_eq!(events, ALOOPER_EVENT_HANGUP, "hang-up only, nothing to read");
        assert_eq!(poll(500), 3, "reported until removed");
        alooper_remove_fd(l, r);
        close(r);
    });
}

#[test]
fn v13_a_writable_pipe_reports_output() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let (r, w) = pipe();
        assert_eq!(alooper_add_fd(l, w, 4, ALOOPER_EVENT_OUTPUT, None, std::ptr::null_mut()), 1);
        let mut events = 0;
        assert_eq!(alooper_poll_once(500, std::ptr::null_mut(), &mut events, std::ptr::null_mut()), 4);
        assert_ne!(events & ALOOPER_EVENT_OUTPUT, 0);
        alooper_remove_fd(l, w);
        close(r);
        close(w);
    });
}

// ------------------------------------------------------------ 14-15: null and reference counts

#[test]
fn v14_a_null_looper_is_refused_or_ignored() {
    let null = std::ptr::null_mut();
    assert_eq!(alooper_add_fd(null, 0, 1, ALOOPER_EVENT_INPUT, None, null), -1);
    assert_eq!(alooper_remove_fd(null, 0), -1);
    alooper_wake(null);
    alooper_acquire(null);
    alooper_release(null);
    assert_eq!(add_fd_line(15, 0, 1, true), "[android] ALooper_addFd(fd=15, ident=0, events=1, callback=yes)");
    assert_eq!(add_fd_line(17, 1, 1, false), "[android] ALooper_addFd(fd=17, ident=1, events=1, callback=no)");
}

#[test]
fn v15_the_reference_count_never_frees() {
    fresh(|| {
        let l = alooper_prepare(0);
        alooper_acquire(l);
        alooper_acquire(l);
        for _ in 0..3 {
            alooper_release(l);
        }
        assert_eq!(alooper_for_thread(), l);
        alooper_wake(l);
        assert_eq!(poll(500), ALOOPER_POLL_WAKE);
        assert_eq!(poll(0), ALOOPER_POLL_TIMEOUT);
    });
}

// ------------------------------------------------------------ 16: the backoff

#[test]
fn v16_the_zero_timeout_backoff() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let sleeps = || this().backoff_sleeps.load(Ordering::SeqCst);

        // Below the present gate the loop is never slowed.
        TEST_PRESENTS.with(|p| p.set(Some(BACKOFF_PRESENTS - 1)));
        let t = Instant::now();
        for _ in 0..100_000 {
            poll(0);
        }
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert_eq!(sleeps(), 0);

        // At the gate, after 64 empty polls, each further one sleeps first.
        TEST_PRESENTS.with(|p| p.set(Some(BACKOFF_PRESENTS)));
        let fd = eventfd(0);
        assert_eq!(alooper_add_fd(l, fd, 9, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        let fast = |n: u32| {
            let t = Instant::now();
            for _ in 0..n {
                assert_eq!(poll(0), ALOOPER_POLL_TIMEOUT);
            }
            t.elapsed()
        };
        let first = sleeps();
        fast(BACKOFF_AFTER);
        assert_eq!(sleeps(), first, "the first 64 never sleep");
        let t = Instant::now();
        for _ in 0..20 {
            poll(0);
        }
        let each = t.elapsed() / 20;
        assert!(each >= Duration::from_micros(BACKOFF_SLEEP_US), "{each:?}");
        assert_eq!(sleeps(), first + 20);

        // Finite and infinite timeouts never sleep this way.
        let before = sleeps();
        assert_eq!(poll(1), ALOOPER_POLL_TIMEOUT);
        alooper_wake(l);
        assert_eq!(poll(-1), ALOOPER_POLL_WAKE);
        assert_eq!(sleeps(), before);

        // A wake, an addFd and a readable descriptor each reset the run.
        let reset_then_fast = |reset: &dyn Fn()| {
            for _ in 0..BACKOFF_AFTER {
                poll(0);
            }
            reset();
            let s = sleeps();
            fast(BACKOFF_AFTER);
            assert_eq!(sleeps(), s, "the next 64 are fast again");
        };
        reset_then_fast(&|| {
            alooper_wake(l);
            assert_eq!(poll(0), ALOOPER_POLL_WAKE);
        });
        let spare = eventfd(0);
        reset_then_fast(&|| {
            assert_eq!(alooper_add_fd(l, spare, 10, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        });
        reset_then_fast(&|| {
            bump_eventfd(fd);
            assert_eq!(poll(0), 9);
            drain_eventfd(fd);
        });
        TEST_PRESENTS.with(|p| p.set(None));
        alooper_remove_fd(l, fd);
        alooper_remove_fd(l, spare);
        close(fd);
        close(spare);
    });
}

// ------------------------------------------------------------ 17: statistics

#[test]
fn v17_statistics_follow_what_happened() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let me = this();
        let stats = || {
            let (_, all) = snapshot();
            all.into_iter().find(|s| s.tid == me.tid).expect("this thread's looper is listed")
        };
        let s = stats();
        assert_eq!((s.fds, s.polls, s.events, s.wakes, s.last_wake_ms), (0, 0, 0, 0, 0));

        // Vector 7's sequence.
        let a = eventfd(1);
        assert_eq!(alooper_add_fd(l, a, 42, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        assert_eq!(stats().list, format!("{a}:42:-"));
        assert_eq!(poll(500), 42);
        assert_eq!(poll(500), 42);
        alooper_remove_fd(l, a);
        assert_eq!(poll(50), ALOOPER_POLL_TIMEOUT);
        let s = stats();
        assert_eq!((s.fds, s.list.as_str(), s.polls, s.events), (0, "", 3, 2));

        // Vector 10's, with a callback: listed as ident -2 with `cb`.
        let b = eventfd(1);
        let rec = Recorder::new(1);
        assert_eq!(alooper_add_fd(l, b, 0, ALOOPER_EVENT_INPUT, Some(record), rec.data()), 1);
        let c = eventfd(0);
        assert_eq!(alooper_add_fd(l, c, 3, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        let s = stats();
        assert_eq!((s.fds, s.list.clone()), (2, format!("{b}:-2:cb,{c}:3:-")));
        rec.answer.store(0, Ordering::SeqCst);
        assert_eq!(poll(500), ALOOPER_POLL_CALLBACK);
        let s = stats();
        assert_eq!((s.fds, s.list.clone()), (1, format!("{c}:3:-")), "the list drops it too");

        alooper_wake(l);
        assert_eq!(poll(0), ALOOPER_POLL_WAKE);
        let s = stats();
        assert_eq!((s.polls, s.events, s.wakes), (5, 4, 1));
        assert!(s.last_wake_ms <= clock_ms() && s.last_event_ms >= s.last_wake_ms);
        alooper_remove_fd(l, c);
        for fd in [a, b, c] {
            close(fd);
        }
    });
}

// ------------------------------------------------------------ 18-19: lines and limits

#[test]
fn v18_the_health_line() {
    assert_eq!(
        pump::health_line(
            1800,
            30.0,
            5400,
            Some("frames n=900 p50=16.7ms p95=17.1ms p99=33.4ms max=41.0ms median_fps=60")
        ),
        "[intoxicated] health: 1800 presents in 30s (60.0/s), 5400 total; frames n=900 p50=16.7ms p95=17.1ms \
         p99=33.4ms max=41.0ms median_fps=60"
    );
    assert_eq!(
        pump::health_line(0, 30.0, 12, None),
        "[intoxicated] health: 0 presents in 30s (0.0/s), 12 total -- nothing was drawn"
    );
    // The rate is over the unrounded time; the seconds are rounded.
    assert_eq!(pump::health_line(25, 30.6, 210, None), "[intoxicated] health: 25 presents in 31s (0.8/s), 210 total");
}

#[test]
fn v19_the_teardown_limit() {
    let ten = Some(Duration::from_secs(10));
    assert_eq!(pump::teardown_limit(None), ten);
    assert_eq!(pump::teardown_limit(Some("")), ten);
    assert_eq!(pump::teardown_limit(Some(" 3 ")), Some(Duration::from_secs(3)));
    assert_eq!(pump::teardown_limit(Some("off")), None);
    assert_eq!(pump::teardown_limit(Some("0")), None);
    assert_eq!(pump::teardown_limit(Some("ten")), ten, "a typo never disarms it");
}

// ------------------------------------------------------------ 20-21: the watchdog

static OVERRUNS: Mutex<Vec<Instant>> = Mutex::new(Vec::new());

fn note_overrun(line: &[u8]) {
    assert!(line.ends_with(b"\n"));
    lock(&OVERRUNS).push(Instant::now());
}

#[test]
fn v20_watchdog_verdicts() {
    // Never answered: an overrun, not early and not late.
    let armed = Instant::now();
    let mut w = Watchdog::arm(Duration::from_millis(200), note_overrun).unwrap();
    let overran = w.take_thread().unwrap().join().unwrap();
    let took = armed.elapsed();
    assert!(overran);
    assert!(took >= Duration::from_millis(200) && took < Duration::from_secs(2), "{took:?}");
    drop(w);

    // Answered after 50 ms with a 10 s limit: stands down at once.
    let mut w = Watchdog::arm(Duration::from_secs(10), note_overrun).unwrap();
    let thread = w.take_thread().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let t = Instant::now();
    w.stand_down();
    assert!(!thread.join().unwrap());
    assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());

    // Dropped after 50 ms without an answer (an unwind): the same.
    let mut w = Watchdog::arm(Duration::from_secs(10), note_overrun).unwrap();
    let thread = w.take_thread().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let t = Instant::now();
    drop(w);
    assert!(!thread.join().unwrap());
    assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
    assert_eq!(lock(&OVERRUNS).len(), 1);
}

const CHILD: &str = "INTOXICATED_LOOPER_CHILD";

fn child(test: &str, role: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, role)
        .env(pump::TEARDOWN_TIMEOUT_VARIABLE, "off")
        .stdin(Stdio::null());
    c
}

fn wait_with_limit(p: &mut std::process::Child, limit: Duration) -> std::process::ExitStatus {
    let t = Instant::now();
    loop {
        if let Some(s) = p.try_wait().unwrap() {
            return s;
        }
        if t.elapsed() > limit {
            let _ = p.kill();
            panic!("the child did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Child only: a watchdog that is never stood down.
#[test]
fn child_watchdog_exit() {
    if std::env::var(CHILD).as_deref() != Ok("watchdog") {
        return;
    }
    let _w = Watchdog::arm(Duration::from_millis(300), pump::overrun_exit).unwrap();
    std::thread::sleep(Duration::from_secs(20));
}

#[test]
fn v21_the_watchdog_exits_124() {
    let t = Instant::now();
    let out = child("android::looper::tests::child_watchdog_exit", "watchdog")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert_eq!(out.status.code(), Some(124));
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(&pump::overrun_line(Duration::from_millis(300))), "{stderr}");
}

// ------------------------------------------------------------ 22-25: teardown, stopping, signals

/// A host with no engine and no window. Records the teardown steps.
#[derive(Default)]
struct Fake {
    steps: Vec<Step>,
    /// Answer every step as not registered, as with no engine at all.
    unregistered: bool,
    /// Print markers and hold the first teardown step, for the signal test.
    noisy: bool,
    iterations: u64,
}

impl Host for Fake {
    fn presents(&self) -> u64 {
        0
    }
    fn frame_summary(&self) -> Option<String> {
        None
    }
    fn places_loaded(&self) -> u32 {
        0
    }
    fn window_closed(&self) -> bool {
        false
    }
    fn display_fd(&self) -> Option<c_int> {
        None
    }
    fn arm_clipboard(&mut self) {}
    fn input(&mut self, _handle: i64) {}
    fn input_backlog(&self) -> bool {
        false
    }
    fn housekeeping(&mut self) {
        self.iterations += 1;
        if self.noisy && self.iterations == 1 {
            println!("CHILD-PUMPING");
        }
    }
    fn save_cookies(&mut self) {}
    fn teardown_step(&mut self, _handle: i64, step: Step) -> Result<Option<()>, String> {
        self.steps.push(step);
        if self.noisy && step == Step::FocusLost {
            println!("CHILD-TEARDOWN");
            std::thread::sleep(Duration::from_secs(30));
        }
        if self.unregistered {
            Ok(None)
        } else {
            Ok(Some(()))
        }
    }
}

#[test]
fn v22_teardown_without_an_engine_is_bounded() {
    fresh(|| {
        alooper_prepare(0);
        let mut host = Fake { unregistered: true, ..Fake::default() };
        let t = Instant::now();
        pump::teardown(&mut host, current(), 1);
        let took = t.elapsed();
        assert!(took >= pump::GRACE && took < Duration::from_secs(2), "{took:?}");
        assert_eq!(host.steps.len(), 5, "every step is still tried");
        // And with no looper at all.
        let t = Instant::now();
        pump::teardown(&mut Fake { unregistered: true, ..Fake::default() }, None, 1);
        assert!(t.elapsed() < Duration::from_secs(2));
    });
}

#[test]
fn v23_the_teardown_order() {
    fresh(|| {
        let mut host = Fake::default();
        pump::teardown(&mut host, None, 1);
        assert_eq!(host.steps, pump::TEARDOWN);
        let names: Vec<&str> = host.steps.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [
                "onWindowFocusChangedNative(false)",
                "onPauseNative",
                "onSurfaceDestroyedNative",
                "onStopNative",
                "terminateNativeCode"
            ]
        );
    });
}

#[test]
fn v24_stop_precedence() {
    use pump::stop_reason;
    assert_eq!(stop_reason(true, true, true, true), Some("a terminating signal"));
    assert_eq!(stop_reason(false, true, true, true), Some("the window closing"));
    assert_eq!(stop_reason(false, true, true, false), Some("something asking the pump to stop"));
    assert_eq!(stop_reason(false, true, false, false), None);
    assert_eq!(stop_reason(false, false, true, true), Some("something asking the pump to stop"));
    assert_eq!(stop_reason(false, false, false, true), None);
}

/// Child only: the pump with a fake host whose teardown hangs.
#[test]
fn child_pump_signals() {
    if std::env::var(CHILD).as_deref() != Ok("pump") {
        return;
    }
    pump::reset_flags_for_test();
    let mut host = Fake { noisy: true, ..Fake::default() };
    pump::run(&mut host, Duration::ZERO, Some(1));
    println!("CHILD-RETURNED");
}

fn signal_twice(sig: c_int) -> (Option<i32>, Vec<String>) {
    let mut p = child("android::looper::tests::child_pump_signals", "pump")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = p.id() as libc::pid_t;
    let mut lines = BufReader::new(p.stdout.take().unwrap()).lines();
    let mut seen = Vec::new();
    let mut until = |marker: &str, seen: &mut Vec<String>| {
        for line in lines.by_ref() {
            let line = line.unwrap();
            seen.push(line.clone());
            if line.contains(marker) {
                return;
            }
        }
        panic!("the child ended before {marker:?}: {seen:?}");
    };
    until("CHILD-PUMPING", &mut seen);
    let first = Instant::now();
    // SAFETY: signalling our own child.
    unsafe { libc::kill(pid, sig) };
    until("CHILD-TEARDOWN", &mut seen);
    assert!(first.elapsed() < Duration::from_secs(2), "the stop took {:?}", first.elapsed());
    // SAFETY: as above.
    unsafe { libc::kill(pid, sig) };
    let status = wait_with_limit(&mut p, Duration::from_secs(10));
    (status.code(), seen)
}

#[test]
fn v25_signals_end_the_run_and_a_second_one_ends_the_process() {
    let (code, seen) = signal_twice(libc::SIGTERM);
    assert!(seen.iter().any(|l| l == "[android] ending the run: a terminating signal"), "{seen:?}");
    assert!(!seen.iter().any(|l| l == "CHILD-RETURNED"));
    assert_eq!(code, Some(128 + libc::SIGTERM));
    // Ctrl-C twice: 130, which the launcher takes as the user quitting.
    let (code, seen) = signal_twice(libc::SIGINT);
    assert!(seen.iter().any(|l| l == "[android] ending the run: a terminating signal"), "{seen:?}");
    assert_eq!(code, Some(130));
}

// ------------------------------------------------------------ beyond the spec's vectors

#[test]
fn the_kernel_event_is_freebsds_layout() {
    assert_eq!(std::mem::size_of::<kqueue::KEvent>(), 64);
}

#[test]
fn a_non_callback_registration_needs_the_prepare_option() {
    fresh(|| {
        let l = alooper_prepare(0);
        let fd = eventfd(1);
        assert_eq!(alooper_add_fd(l, fd, 1, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), -1);
        let rec = Recorder::new(1);
        assert_eq!(alooper_add_fd(l, fd, 1, ALOOPER_EVENT_INPUT, Some(record), rec.data()), 1);
        alooper_remove_fd(l, fd);
        close(fd);
    });
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        assert_eq!(alooper_add_fd(l, 0, -5, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), -1, "negative ident");
        assert_eq!(alooper_add_fd(l, -1, 1, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), -1, "no descriptor");
        assert_eq!(this().registrations().0, 0);
    });
}

#[test]
fn registering_a_descriptor_again_replaces_it() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let fd = eventfd(1);
        assert_eq!(alooper_add_fd(l, fd, 1, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        assert_eq!(alooper_add_fd(l, fd, 2, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        assert_eq!(this().registrations(), (1, format!("{fd}:2:-")));
        assert_eq!(poll(500), 2);
        alooper_remove_fd(l, fd);
        close(fd);
    });
}

#[test]
fn every_ready_callback_runs_and_idents_queue() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let (a, b, c, d) = (eventfd(1), eventfd(1), eventfd(1), eventfd(1));
        let rec = Recorder::new(1);
        assert_eq!(alooper_add_fd(l, a, 0, ALOOPER_EVENT_INPUT, Some(record), rec.data()), 1);
        assert_eq!(alooper_add_fd(l, b, 0, ALOOPER_EVENT_INPUT, Some(record), rec.data()), 1);
        assert_eq!(alooper_add_fd(l, c, 11, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        assert_eq!(alooper_add_fd(l, d, 12, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        // One wait: both callbacks run, then the first ident comes back and
        // the second is handed out by the next call without a wait.
        let first = poll(500);
        assert_eq!(rec.calls().len(), 2);
        let second = poll(0);
        let mut idents = [first, second];
        idents.sort_unstable();
        assert_eq!(idents, [11, 12]);
        for fd in [a, b, c, d] {
            alooper_remove_fd(l, fd);
            close(fd);
        }
    });
}

fn thread_cpu() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a live timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// Not a check: what one `ALooper_pollOnce` costs on this machine, for the
/// commit message. `cargo test --release -p intoxicated-runtime --lib
/// bench_poll_paths -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_poll_paths() {
    fresh(|| {
        let l = alooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
        let rate = |name: &str, presents: u64, expect: c_int| {
            TEST_PRESENTS.with(|p| p.set(Some(presents)));
            let (t, c) = (Instant::now(), thread_cpu());
            let mut n = 0u64;
            while t.elapsed() < Duration::from_secs(2) {
                for _ in 0..1000 {
                    assert_eq!(poll(0), expect);
                }
                n += 1000;
            }
            let (dt, dc) = (t.elapsed().as_secs_f64(), (thread_cpu() - c).as_secs_f64());
            println!(
                "{name:<44} {:>9.0} polls/s {:>7.3} us/poll  thread cpu {:.2}%",
                n as f64 / dt,
                dt / n as f64 * 1e6,
                dc / dt * 100.0
            );
        };
        rate("pollOnce(0), empty, before the present gate", 0, ALOOPER_POLL_TIMEOUT);
        rate("pollOnce(0), empty, backoff engaged", BACKOFF_PRESENTS, ALOOPER_POLL_TIMEOUT);
        let fd = eventfd(1);
        assert_eq!(alooper_add_fd(l, fd, 3, ALOOPER_EVENT_INPUT, None, std::ptr::null_mut()), 1);
        rate("pollOnce(0), one ready ident fd", 0, 3);
        alooper_remove_fd(l, fd);
        close(fd);
        TEST_PRESENTS.with(|p| p.set(None));

        // Wake from another thread into a blocked pollOnce(1000).
        let handle = l as usize;
        let mut lat = Vec::new();
        for _ in 0..1000 {
            let sent = Arc::new(Mutex::new(None::<Instant>));
            let s2 = sent.clone();
            let w = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1));
                *lock(&s2) = Some(Instant::now());
                alooper_wake(handle as *mut c_void);
            });
            assert_eq!(poll(1000), ALOOPER_POLL_WAKE);
            let back = Instant::now();
            w.join().unwrap();
            let at = lock(&sent).expect("sent");
            lat.push(back.saturating_duration_since(at).as_secs_f64() * 1e6);
        }
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "ALooper_wake to pollOnce return            mean {:.1} us  p50 {:.1} us  max {:.1} us",
            lat.iter().sum::<f64>() / lat.len() as f64,
            lat[lat.len() / 2],
            lat[lat.len() - 1]
        );
    });
}

#[test]
fn the_coalesce_variable() {
    assert_eq!(parse_coalesce(None), 250);
    assert_eq!(parse_coalesce(Some("0")), 0);
    assert_eq!(parse_coalesce(Some(" 100 ")), 100);
    assert_eq!(parse_coalesce(Some("fast")), 250);
}

#[test]
fn the_join_timeout_and_lines() {
    assert_eq!(pump::join_timeout(None), Duration::from_secs(60));
    assert_eq!(pump::join_timeout(Some("90")), Duration::from_secs(90));
    assert_eq!(pump::join_timeout(Some("0")), Duration::from_secs(60));
    assert_eq!(pump::join_timeout(Some("-3")), Duration::from_secs(60));
    assert_eq!(
        pump::stall_line(6, 4048, 123),
        "[android] the engine has presented nothing for 6s after 4048 frames. The pump is still running, so \
         this is not the idle throttle. Take a backtrace -- `lldb -p 123 -b -o 'thread backtrace all -c 16'`."
    );
    assert!(pump::join_line(61).starts_with("[android] a join was requested 61s ago and the engine has not"));
    assert!(pump::join_line(61).ends_with("Set INTOXICATED_JOIN_TIMEOUT to change how long this waits."));
    assert_eq!(
        pump::overrun_line(Duration::from_secs(10)),
        "[android] the engine's shutdown has not returned after 10s; exiting without it, status 124. Cookies \
         were saved before it began. INTOXICATED_TEARDOWN_TIMEOUT_S=off disables this."
    );
}
