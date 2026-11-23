//! The client settings and flag names, delivered before the engine decides
//! its flags.
//!
//! Two deliveries reach the engine during bring-up:
//!
//! - the early settings, on the main thread before `initializeNativeCode`.
//!   Load-bearing: measured on a fresh profile without them, the engine
//!   reported `onFlagsFailed` 3 ms after `bootstrapTheApp` began and aborted
//!   with `Can't initialize the TaskScheduler before flags have been loaded`
//!   (SIGTRAP). They are no longer behind `INTOXICATED_EARLY_SETTINGS`; the
//!   variable is still accepted and changes nothing.
//! - the delivery proper (settings, then the flag names), run by the
//!   `bootstrapTheApp` hook the engine calls on a thread of its own just
//!   after `initializeNativeCode` returns, while the main thread goes on with
//!   the bring-up.
//!
//! The main thread used to look whether the hook had *begun*, about 6 ms
//! after it began, and went on to the globals while the settings call (33 to
//! 49 ms) was still running. On a warm launch the call always happened to
//! return before `nativeGameGlobalInit` (18 of 18 logged); on a fresh profile
//! the globals came first (2 of 2 measured), and the flag names were then
//! registered after them (provider ID 1 instead of 0). Now the main thread
//! waits, with a bound, until the delivery has *finished* before the globals
//! ([`await_delivery`]): an explicit happens-before instead of a timing
//! margin, with a line saying how long it waited. A delivery that has not
//! begun by then is made from the main thread instead, and a later hook call
//! says it was already delivered.
//!
//! What the wait cannot change: on a fresh profile the engine reaches its
//! first flags verdict about 10 ms after `initializeNativeCode` from its own
//! signed flag cache (`<cache>/cache/flag_cache.dat`, absent until a launch
//! has loaded flags), whatever the delivery is doing. Measured: with the
//! delivery held back 100 ms, `onFlagsFailed` still came 10 ms after the
//! handle. The late post and retry after the surface handover turn that
//! verdict into `flags loaded`, as they did before.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use intoxicated_runtime::roblox_settings::Origin;
use intoxicated_linker_sys as linker;

/// The 139 flag names the engine is sent when no `--client-settings` file
/// replaces them; it echoes them back in this order.
pub const FLAG_NAMES: &str = include_str!("../../native-flag-names.txt");

/// How long the main thread waits for the delivery before carrying on. The
/// delivery took 33 to 49 ms in every measured run; this only bounds a hook
/// that never runs or never returns.
pub const WAIT: Duration = Duration::from_secs(2);

/// Where one delivery has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Nobody has begun it.
    Waiting,
    /// One thread is delivering.
    Running,
    /// It has returned.
    Done,
}

/// One delivery that exactly one thread makes, and that others can wait for.
pub struct Handoff {
    stage: Mutex<Stage>,
    changed: Condvar,
}

impl Handoff {
    pub const fn new() -> Handoff {
        Handoff { stage: Mutex::new(Stage::Waiting), changed: Condvar::new() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Stage> {
        self.stage.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Claim the delivery. `true` for the first caller only, who must then
    /// call [`Handoff::finish`] (or hold a [`Finish`] guard).
    pub fn begin(&self) -> bool {
        let mut stage = self.lock();
        if *stage == Stage::Waiting {
            *stage = Stage::Running;
            true
        } else {
            false
        }
    }

    /// The delivery has returned.
    pub fn finish(&self) {
        *self.lock() = Stage::Done;
        self.changed.notify_all();
    }

    #[cfg(test)]
    pub fn stage(&self) -> Stage {
        *self.lock()
    }

    /// Wait until the delivery has finished or `timeout` has passed, and say
    /// where it had got to and how long the wait took.
    pub fn wait_done(&self, timeout: Duration) -> (Stage, Duration) {
        let started = Instant::now();
        let stage = self.lock();
        let (stage, _) = self
            .changed
            .wait_timeout_while(stage, timeout, |s| *s != Stage::Done)
            .unwrap_or_else(|p| p.into_inner());
        (*stage, started.elapsed())
    }
}

/// Finishes a claimed delivery when dropped, so a delivery that panics does
/// not leave the main thread waiting for the full bound.
pub struct Finish<'a>(pub &'a Handoff);

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// What the delivery sends, prepared on the main thread before the hook is
/// installed.
pub struct Plan {
    /// `NativeGLInterface.nativeInitClientSettings`, 0 when not exported.
    pub settings_native: usize,
    /// `FlagJniInterface.nativeInitializeNativeFlags`, 0 when not exported.
    pub flags_native: usize,
    /// The ClientSettings document, the same one the early delivery sent.
    pub settings: String,
    /// Where Roblox's part of `settings` came from, for the log line;
    /// see `roblox_settings::Origin`.
    pub origin: Origin,
    /// The flag names, one per line.
    pub flag_names: String,
    /// The engine library, named in the diagnostic for a build without the
    /// settings native.
    pub library: PathBuf,
}

static PLAN: OnceLock<Plan> = OnceLock::new();
static DELIVERY: Handoff = Handoff::new();

const SETTINGS_NATIVE: &str = "Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings";
const FLAGS_NATIVE: &str = "Java_com_roblox_client_flags_FlagJniInterface_nativeInitializeNativeFlags";

fn address(native: Option<*mut c_void>) -> usize {
    native.map_or(0, |p| p as usize)
}

/// The ClientSettings document and its origin, read once for the whole
/// launch: the early delivery and the delivery proper send the same text.
pub fn document() -> (String, Origin) {
    intoxicated_runtime::roblox_settings::document_and_origin()
}

/// The early delivery, made when the settings native resolves.
pub fn deliver_early(lib: linker::Library, settings: &str) {
    let Some(native) = lib.symbol(SETTINGS_NATIVE) else { return };
    match linker::game_activity::init_client_settings(native, settings, "", "") {
        Ok(code) => println!("  early client settings ({} bytes) -> {code}", settings.len()),
        Err(e) => println!("  early client settings failed: {e}"),
    }
}

/// The flag names: the `--client-settings` file's content when it can be
/// read, the built-in list otherwise.
pub fn flag_names(file: Option<&str>) -> String {
    file.and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_else(|| FLAG_NAMES.to_string())
}

/// Prepare the delivery and install the hook. Before `initializeNativeCode`:
/// the engine calls the hook right after it.
pub fn install(lib: linker::Library, lib_dir: &str, settings: String, origin: Origin, names: String) {
    let plan = Plan {
        settings_native: address(lib.symbol(SETTINGS_NATIVE)),
        flags_native: address(lib.symbol(FLAGS_NATIVE)),
        settings,
        origin,
        flag_names: names,
        library: intoxicated_runtime::engine::library_in(Path::new(lib_dir)),
    };
    // Only one plan per process; a second install keeps the first.
    let _ = PLAN.set(plan);
    linker::game_activity::set_bootstrap(Some(bootstrap_the_app));
    println!("  bootstrapTheApp installed");
}

/// `GameActivity.bootstrapTheApp()`, on whichever engine thread calls it,
/// usually once just after `initializeNativeCode` and on some launches again
/// later. Nothing may unwind out of it into C.
extern "C" fn bootstrap_the_app() {
    let _ = std::panic::catch_unwind(|| {
        let Some(plan) = PLAN.get() else {
            eprintln!("  bootstrapTheApp: nothing planned");
            return;
        };
        if !DELIVERY.begin() {
            println!("  bootstrapTheApp: already delivered");
            return;
        }
        let _finish = Finish(&DELIVERY);
        deliver(plan, "bootstrapTheApp");
    });
}

/// The delivery: the settings document, then the flag names. Whoever claimed
/// [`DELIVERY`] runs it, once.
fn deliver(plan: &Plan, who: &str) {
    println!("  {who}: delivering settings and flags");
    println!("  client settings: {} bytes ({})", plan.settings.len(), plan.origin);
    if plan.settings_native == 0 {
        // Never skipped silently: without this native the engine aborts.
        println!(
            "  nativeInitClientSettings is not exported by this libroblox.so, so its flags cannot be delivered \
             and the engine will abort with \"Can't initialize the TaskScheduler before flags have been loaded\"; \
             this is a Roblox build Intoxicated does not know how to start."
        );
        println!(
            "  Please report it with the engine version and the output of: readelf -sW {} | grep -i initclientsettings",
            plan.library.display()
        );
    } else {
        // NOTE on the 3 string args: "ClientAppSettings" in the 1st or 2nd position
        // makes nativeInitClientSettings return 1 (rejected); empty, or it in the
        // 3rd position, returns 0 (accepted). It does NOT fix the getFlags deadlock
        // (that is a FunctionMarshaller-thread race, not a document-key issue); a
        // build-timing-dependent success on the 3rd-position variant was a red
        // herring. Left as the original accepted form.
        let native = plan.settings_native as *mut c_void;
        match linker::game_activity::init_client_settings(native, &plan.settings, "", "") {
            Ok(code) => println!("    nativeInitClientSettings -> {code}"),
            Err(e) => println!("    nativeInitClientSettings failed: {e}"),
        }

        // Keep the launch flags applied, so the engine's own flag reloader
        // cannot revert the fps cap mid-session. The loop, its switches and
        // the measurement behind it are in `roblox_settings::keep_applied`.
        // Started here, after the first delivery whatever its result, and
        // only on this path: the early and late deliveries do not start it.
        intoxicated_runtime::roblox_settings::keep_applied(plan.settings_native, plan.settings.clone());
    }
    if plan.flags_native == 0 {
        println!("  nativeInitializeNativeFlags is not exported by this build; no flag names sent");
    } else {
        match linker::game_activity::init_flags(plan.flags_native as *mut c_void, &plan.flag_names) {
            Ok(()) => println!("    flags initialised"),
            Err(e) => println!("    flag init failed: {e}"),
        }
    }
}

/// Wait, at most [`WAIT`], until the delivery has finished; deliver from
/// this thread when nothing had begun it by then. Called by the main thread
/// just before the globals (`nativeGameGlobalInit`).
pub fn await_delivery() {
    let Some(plan) = PLAN.get() else { return };
    let (stage, waited) = DELIVERY.wait_done(WAIT);
    let ms = waited.as_millis();
    match stage {
        Stage::Done => println!("  settings and flags delivered by bootstrapTheApp (waited {ms} ms)"),
        Stage::Running => println!("  bootstrapTheApp is still delivering after {ms} ms; carrying on without it"),
        Stage::Waiting => {
            if DELIVERY.begin() {
                println!("  bootstrapTheApp was not called within {ms} ms; delivering from the main thread");
                let _finish = Finish(&DELIVERY);
                deliver(plan, "main thread");
            } else {
                println!("  bootstrapTheApp began after {ms} ms; carrying on without waiting longer");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn the_built_in_flag_list_is_the_one_the_engine_echoes() {
        let names: Vec<&str> = FLAG_NAMES.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(names.len(), 139);
        assert_eq!(names[0], "EnableAndroidBinaryChannelDownloadTiming");
        assert_eq!(names[138], "EnableSendNotificationSettingsOnPushAck");
    }

    #[test]
    fn an_unreadable_client_settings_file_means_the_built_in_list() {
        assert_eq!(flag_names(None), FLAG_NAMES);
        assert_eq!(flag_names(Some("/nonexistent/flag-names")), FLAG_NAMES);
    }

    #[test]
    fn only_the_first_claim_delivers() {
        let h = Handoff::new();
        assert_eq!(h.stage(), Stage::Waiting);
        assert!(h.begin());
        assert_eq!(h.stage(), Stage::Running);
        assert!(!h.begin());
        h.finish();
        assert_eq!(h.stage(), Stage::Done);
        assert!(!h.begin(), "a finished delivery is not made again");
    }

    #[test]
    fn the_wait_ends_when_the_delivery_finishes_not_when_it_begins() {
        let h = Arc::new(Handoff::new());
        assert!(h.begin());
        let other = Arc::clone(&h);
        let delivering = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            other.finish();
        });
        let (stage, waited) = h.wait_done(Duration::from_secs(5));
        delivering.join().unwrap();
        assert_eq!(stage, Stage::Done);
        assert!(waited >= Duration::from_millis(55), "{waited:?}");
        assert!(waited < Duration::from_secs(4), "{waited:?}");
    }

    #[test]
    fn a_finished_delivery_is_not_waited_for() {
        let h = Handoff::new();
        assert!(h.begin());
        h.finish();
        let (stage, waited) = h.wait_done(Duration::from_secs(5));
        assert_eq!(stage, Stage::Done);
        assert!(waited < Duration::from_secs(1));
    }

    #[test]
    fn the_wait_is_bounded() {
        let h = Handoff::new();
        let (stage, waited) = h.wait_done(Duration::from_millis(40));
        assert_eq!(stage, Stage::Waiting);
        assert!(waited >= Duration::from_millis(40));
        assert!(h.begin(), "the waiter may then deliver itself");
        let (stage, _) = h.wait_done(Duration::from_millis(20));
        assert_eq!(stage, Stage::Running);
    }

    #[test]
    fn a_delivery_that_panics_still_finishes() {
        let h = Arc::new(Handoff::new());
        let other = Arc::clone(&h);
        let r = std::thread::spawn(move || {
            assert!(other.begin());
            let _finish = Finish(&other);
            panic!("the engine call blew up");
        })
        .join();
        assert!(r.is_err());
        let (stage, waited) = h.wait_done(Duration::from_secs(5));
        assert_eq!(stage, Stage::Done);
        assert!(waited < Duration::from_secs(1));
    }

    #[test]
    fn many_waiters_see_one_delivery() {
        let h = Arc::new(Handoff::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let h = Arc::clone(&h);
                std::thread::spawn(move || {
                    let mine = h.begin();
                    if mine {
                        std::thread::sleep(Duration::from_millis(20));
                        h.finish();
                    }
                    let (stage, _) = h.wait_done(Duration::from_secs(5));
                    (mine, stage)
                })
            })
            .collect();
        let claimed: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(claimed.iter().filter(|(mine, _)| *mine).count(), 1);
        assert!(claimed.iter().all(|(_, stage)| *stage == Stage::Done));
    }
}
