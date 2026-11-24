//! Roblox's client settings document: fetched, cached, merged with the flag
//! layers, and kept applied.
//!
//! On Android the app fetches this document and hands it to the engine; the
//! engine does not fetch its own first copy. This module is that fetch. The
//! bring-up code decides when the engine is given the result
//! (`nativeInitClientSettings`, document in the first argument); this module
//! only produces it, and runs the keep-flags loop that re-sends it.
//!
//! **Load order** ([`load_raw`]):
//!
//! 1. A cache younger than [`MAX_AGE`] is used and the network is not touched.
//! 2. Otherwise [`SETTINGS_URL`] is fetched through [`crate::net`]. A body
//!    that does not contain [`MARKER`] is not a settings document (an error
//!    body is valid JSON too) and is never cached.
//! 3. A failed or implausible fetch falls back to the cache at any age.
//! 4. Otherwise the document is empty, which the engine rejects; the client
//!    still starts, on the engine's compiled defaults.
//!
//! **The cache** is Roblox's document exactly as fetched, before overrides, at
//! [`cache_file`] (`~/.cache/intoxicated/clientsettings.json`). Age is the
//! file's modification time, and a time in the future counts as stale.
//!
//! **The overrides** ([`with_overrides`]) are applied to whatever was loaded,
//! on every load: each resolved key that is not the client's own is set,
//! as a JSON string, inside the document's top-level `applicationSettings`
//! object. Everything else in the document is preserved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::flag_layers::{self, Resolved};

/// The settings document this client asks for. Other application names answer
/// HTTP 400.
pub const SETTINGS_URL: &str = "https://clientsettingscdn.roblox.com/v2/settings/application/AndroidApp";

/// How long a cached document is used without asking the network.
pub const MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// Text every settings document contains, quotes included.
pub const MARKER: &str = "\"applicationSettings\"";

/// Shortest fixed re-send interval accepted, so a typo cannot spin the
/// engine's flag path.
pub const KEEP_FLAGS_MIN_MS: u64 = 1_000;

/// How long after the engine's reload the launch flags are sent again, so the
/// engine has finished applying its own copy first.
pub const RELOAD_DELAY: Duration = Duration::from_secs(1);

/// With no reload seen by then, the engine's log line was not recognised (a
/// newer build, say) and the loop falls back to [`FALLBACK_EVERY`].
pub const FALLBACK_AFTER: Duration = Duration::from_secs(150);

/// The fallback interval: the reloader's own period on 2.748.
pub const FALLBACK_EVERY: Duration = Duration::from_secs(120);

/// Where the document came from, or why there is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Cache,
    Fetched,
    StaleCache,
    Nothing(String),
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Cache => f.write_str("cache"),
            Origin::Fetched => f.write_str("fetched"),
            Origin::StaleCache => f.write_str("stale cache, fetch failed"),
            Origin::Nothing(why) => write!(f, "nothing: {why}"),
        }
    }
}

/// The file the cache is read from and written to.
#[derive(Clone, Debug)]
pub struct Cache {
    pub file: PathBuf,
}

impl Cache {
    /// The real cache, [`cache_file`].
    pub fn standard() -> Cache {
        Cache { file: cache_file() }
    }
}

fn cache_home() -> PathBuf {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    set("XDG_CACHE_HOME")
        .or_else(|| set("HOME").map(|h| h.join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
}

/// The cache's path under the cache home.
const CACHE_PATH: &str = "intoxicated/clientsettings.json";

/// `$XDG_CACHE_HOME/intoxicated/clientsettings.json`, else under `~/.cache`,
/// else under the system temporary directory.
pub fn cache_file() -> PathBuf {
    cache_home().join(CACHE_PATH)
}

/// Whether a body is a settings document rather than an error answer.
pub fn plausible(body: &str) -> bool {
    body.contains(MARKER)
}

/// Whether a cache last written at `mtime` is still fresh at `now`.
pub fn is_fresh(mtime: SystemTime, now: SystemTime) -> bool {
    match now.duration_since(mtime) {
        Ok(age) => age < MAX_AGE,
        // Written in the future: a clock that moved, or a file copied from
        // somewhere else. Not evidence of anything being recent.
        Err(_) => false,
    }
}

/// The cached document, with its mtime, when it reads and is plausible.
fn read_cache(path: &Path) -> Option<(String, SystemTime)> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let body = std::fs::read_to_string(path).ok()?;
    plausible(&body).then_some((body, mtime))
}

/// Write `body` beside `path` and rename it in, so a reader never sees half a
/// document. Failures are ignored: the cache only saves a request.
///
/// The cache is shared by every profile, so two clients can write it at
/// once. Each write gets a temporary name of its own (process id and a
/// counter); with one shared name a second writer could truncate the first
/// one's file just before it was renamed into place.
fn write_cache(path: &Path, body: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static WRITES: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{n}.new", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Roblox's document, before overrides, and where it came from.
pub fn load_raw(cache: &Cache, now: SystemTime, fetch: impl FnOnce() -> Result<String, String>) -> (String, Origin) {
    let cached = read_cache(&cache.file);
    if let Some((body, mtime)) = cached.as_ref() {
        if is_fresh(*mtime, now) {
            return (body.clone(), Origin::Cache);
        }
    }
    let why = match fetch() {
        Ok(body) if plausible(&body) => {
            write_cache(&cache.file, &body);
            return (body, Origin::Fetched);
        }
        Ok(_) => format!("{SETTINGS_URL} answered with something that is not a settings document"),
        Err(e) => e,
    };
    match cached {
        Some((body, _)) => (body, Origin::StaleCache),
        None => (String::new(), Origin::Nothing(why)),
    }
}

/// Set every Roblox key of `resolved` inside `doc`'s `applicationSettings`.
///
/// The error is the reason the document was left alone.
pub fn merge(doc: &str, resolved: &BTreeMap<String, Resolved>) -> Result<String, &'static str> {
    let mut value: serde_json::Value =
        serde_json::from_str(doc).map_err(|_| "the settings document did not parse")?;
    let settings = value
        .get_mut("applicationSettings")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("no applicationSettings object")?;
    for (key, entry) in resolved {
        if !flag_layers::is_client_key(key) {
            settings.insert(key.clone(), serde_json::Value::String(entry.value.clone()));
        }
    }
    Ok(serde_json::to_string(&value).expect("a JSON value always serialises"))
}

/// What a successful merge prints: the count, then one line per key whose
/// value replaced another.
pub fn report_lines(resolved: &BTreeMap<String, Resolved>) -> Vec<String> {
    let mut lines = vec![format!("  flags: {} override(s) applied", resolved.len())];
    for (key, entry) in resolved.iter().filter(|(_, e)| !e.overridden.is_empty()) {
        let replaced: Vec<String> =
            entry.overridden.iter().map(|(source, value)| format!("{}={value}", source.describe())).collect();
        lines.push(format!(
            "  flags: {key} = {} from {} (overrides {})",
            entry.value,
            entry.source.describe(),
            replaced.join(", ")
        ));
    }
    lines
}

/// `doc` with the resolved overrides merged in, reporting what was done.
pub fn with_overrides(doc: String, resolved: &BTreeMap<String, Resolved>) -> String {
    if resolved.is_empty() {
        return doc;
    }
    match merge(&doc, resolved) {
        Ok(merged) => {
            for line in report_lines(resolved) {
                println!("{line}");
            }
            merged
        }
        Err(why) => {
            println!("  flags: {why}; ignoring overrides");
            doc
        }
    }
}

/// The document to hand the engine, and where Roblox's part of it came from.
pub fn document_and_origin() -> (String, Origin) {
    let (raw, origin) = load_raw(&Cache::standard(), SystemTime::now(), || {
        crate::net::get_text(SETTINGS_URL).map_err(|e| e.to_string())
    });
    let resolved = flag_layers::resolve(flag_layers::collect());
    (with_overrides(raw, &resolved), origin)
}

/// The document to hand the engine.
pub fn document() -> String {
    document_and_origin().0
}

/// When the keep-flags loop re-sends the launch flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepFlags {
    Off,
    /// [`RELOAD_DELAY`] after each settings reload the engine's log reports.
    AfterReload,
    /// Every this many milliseconds, whatever the engine does.
    Every(u64),
}

/// `switch` is `INTOXICATED_KEEP_FLAGS` (`off` or `0` disable), `interval` is
/// `INTOXICATED_KEEP_FLAGS_MS`: whole milliseconds, at least
/// [`KEEP_FLAGS_MIN_MS`], for a fixed interval instead of following reloads;
/// anything else is ignored.
pub fn keep_flags_mode(switch: Option<&str>, interval: Option<&str>) -> KeepFlags {
    if matches!(switch, Some("off") | Some("0")) {
        return KeepFlags::Off;
    }
    match interval.and_then(|v| v.parse::<u64>().ok()).filter(|&ms| ms >= KEEP_FLAGS_MIN_MS) {
        Some(ms) => KeepFlags::Every(ms),
        None => KeepFlags::AfterReload,
    }
}

/// When to re-send in [`KeepFlags::AfterReload`] mode, given the time since
/// the loop started and the reload count from the engine's log.
#[derive(Debug, Default)]
pub struct ReloadSchedule {
    handled: u64,
    due: Option<Duration>,
    last_send: Duration,
    /// Set once the fallback has started, so it is reported once.
    pub fell_back: bool,
}

impl ReloadSchedule {
    /// Whether to send now.
    pub fn tick(&mut self, now: Duration, reloads: u64) -> bool {
        if reloads != self.handled {
            self.handled = reloads;
            self.due = Some(now + RELOAD_DELAY);
        }
        if let Some(due) = self.due {
            if now < due {
                return false;
            }
            self.due = None;
            self.last_send = now;
            return true;
        }
        if self.handled == 0 && now >= FALLBACK_AFTER && now.saturating_sub(self.last_send) >= FALLBACK_EVERY {
            self.fell_back = true;
            self.last_send = now;
            return true;
        }
        false
    }
}

/// Re-send `doc` to the settings native for the rest of the process.
///
/// **Why.** The fps cap override, `DFIntTaskSchedulerTargetFps`, is a dynamic
/// flag that Roblox's own document does not carry. The engine's reloader
/// re-fetches that document every 120 s (measured over an hour on 2.748) and
/// resets the dynamic flags it does not mention to their compiled defaults,
/// which pins the frame rate at 60. Sending the same merged document again
/// puts the value back.
///
/// **When.** Each send makes the engine parse the whole document, about 1.5
/// MB and 25,000 flags: 260 ms of a core, during which the render thread
/// mostly waits. Sent every 5 s, as this loop used to, that was half of all
/// time spent in frames over 25 ms in two minutes of play (22 of 79 such
/// frames, 6.9 of 14.2 s, measured with DTrace). So it is now sent only
/// [`RELOAD_DELAY`] after the engine's log reports a reload
/// ([`crate::engine_log::flag_reloads_seen`]), and on a fixed
/// [`FALLBACK_EVERY`] only if no reload has been seen [`FALLBACK_AFTER`] into
/// the run. `INTOXICATED_KEEP_FLAGS_MS` still asks for a fixed interval.
///
/// `doc` is the snapshot delivered at bootstrap and is never reloaded, so an
/// edit to `flags.json` during a run does not reach the engine through here.
/// The engine's return code is not checked; a call the bridge itself reports
/// as failed ends the loop.
///
/// `settings_native` is the address of `nativeInitClientSettings`, carried as
/// an integer because a raw pointer is not `Send`; it points into a library
/// that stays mapped for the life of the process.
pub fn keep_applied(settings_native: usize, doc: String) {
    let switch = std::env::var("INTOXICATED_KEEP_FLAGS").ok();
    let interval = std::env::var("INTOXICATED_KEEP_FLAGS_MS").ok();
    let mode = keep_flags_mode(switch.as_deref(), interval.as_deref());
    match mode {
        KeepFlags::Off => {
            println!("  [intoxicated] keep-flags: off (INTOXICATED_KEEP_FLAGS={})", switch.unwrap_or_default());
            return;
        }
        KeepFlags::AfterReload => println!(
            "  [intoxicated] keep-flags: re-applying launch flags after each settings reload the engine \
             reports, so it cannot revert the fps cap (INTOXICATED_KEEP_FLAGS=off to disable)"
        ),
        KeepFlags::Every(ms) => println!(
            "  [intoxicated] keep-flags: re-applying launch flags every {ms} ms (INTOXICATED_KEEP_FLAGS_MS)"
        ),
    }
    let send = move || {
        intoxicated_linker_sys::game_activity::init_client_settings(
            settings_native as *mut std::ffi::c_void,
            &doc,
            "",
            "",
        )
    };
    let spawned = std::thread::Builder::new().name("keep-flags".into()).spawn(move || {
        let start = std::time::Instant::now();
        let mut schedule = ReloadSchedule::default();
        loop {
            let go = match mode {
                KeepFlags::Every(ms) => {
                    std::thread::sleep(Duration::from_millis(ms));
                    true
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(250));
                    let fell_back = schedule.fell_back;
                    let go = schedule.tick(start.elapsed(), crate::engine_log::flag_reloads_seen());
                    if schedule.fell_back && !fell_back {
                        eprintln!(
                            "[intoxicated] keep-flags: no settings reload seen in the engine's log after {} s; \
                             re-applying every {} s instead",
                            FALLBACK_AFTER.as_secs(),
                            FALLBACK_EVERY.as_secs()
                        );
                    }
                    go
                }
            };
            if go {
                if let Err(e) = send() {
                    eprintln!("[intoxicated] keep-flags: re-apply failed, stopping: {e}");
                    break;
                }
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("[intoxicated] keep-flags: could not start ({e}); the fps cap may revert mid-run");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag_layers::{Layer, Source};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("intox-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const DOC: &str = r#"{"applicationSettings":{"FFlagA":"True","DFIntB":"3"}}"#;

    fn set_mtime(path: &Path, when: SystemTime) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }

    fn cache_in(dir: &Path) -> Cache {
        Cache { file: dir.join(CACHE_PATH) }
    }

    #[test]
    fn writers_racing_on_the_shared_cache_leave_one_whole_document() {
        // Clients on two profiles refreshing the cache at the same moment.
        let dir = scratch("race");
        let file = cache_in(&dir).file;
        let bodies: Vec<String> = (0..8)
            .map(|i| format!(r#"{{"applicationSettings":{{"FIntWriter":"{i}","Pad":"{}"}}}}"#, "x".repeat(200_000)))
            .collect();
        std::thread::scope(|s| {
            for body in &bodies {
                let file = &file;
                s.spawn(move || {
                    for _ in 0..10 {
                        write_cache(file, body);
                    }
                });
            }
        });
        let got = std::fs::read_to_string(&file).unwrap();
        assert!(bodies.contains(&got), "the cache is one writer's whole document ({} bytes)", got.len());
        let left: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["clientsettings.json"], "no temporary file is left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn freshness_is_six_hours_and_the_future_is_stale() {
        let now = SystemTime::now();
        assert!(is_fresh(now - Duration::from_secs(60), now));
        assert!(is_fresh(now - (MAX_AGE - Duration::from_secs(1)), now));
        assert!(!is_fresh(now - MAX_AGE, now));
        assert!(!is_fresh(now - Duration::from_secs(7 * 3600), now));
        assert!(!is_fresh(now + Duration::from_secs(60), now));
        assert_eq!(MAX_AGE, Duration::from_secs(21_600));
    }

    #[test]
    fn a_fresh_cache_is_used_without_the_network() {
        let dir = scratch("fresh");
        let cache = cache_in(&dir);
        std::fs::create_dir_all(cache.file.parent().unwrap()).unwrap();
        std::fs::write(&cache.file, DOC).unwrap();
        let (body, origin) = load_raw(&cache, SystemTime::now(), || panic!("the network must not be touched"));
        assert_eq!(body, DOC);
        assert_eq!(origin, Origin::Cache);
        assert_eq!(origin.to_string(), "cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_cache_is_refetched_and_the_fetch_is_cached() {
        let dir = scratch("stale");
        let cache = cache_in(&dir);
        std::fs::create_dir_all(cache.file.parent().unwrap()).unwrap();
        std::fs::write(&cache.file, DOC).unwrap();
        set_mtime(&cache.file, SystemTime::now() - Duration::from_secs(7 * 3600));

        let fetched = DOC.replace("True", "False");
        let (body, origin) = load_raw(&cache, SystemTime::now(), || Ok(fetched.clone()));
        assert_eq!((body.as_str(), origin), (fetched.as_str(), Origin::Fetched));
        assert_eq!(std::fs::read_to_string(&cache.file).unwrap(), fetched);
        assert!(!cache.file.with_extension("json.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_or_implausible_fetch_falls_back_and_is_never_cached() {
        let dir = scratch("fallback");
        let cache = cache_in(&dir);

        // No cache at all.
        let (body, origin) = load_raw(&cache, SystemTime::now(), || Err("could not reach it".into()));
        assert_eq!(body, "");
        assert_eq!(origin.to_string(), "nothing: could not reach it");
        let (_, origin) = load_raw(&cache, SystemTime::now(), || Ok(r#"{"errors":[{"code":0}]}"#.into()));
        assert_eq!(
            origin.to_string(),
            format!("nothing: {SETTINGS_URL} answered with something that is not a settings document")
        );
        assert!(!cache.file.exists(), "an error body must never be cached");

        // A stale cache is better than nothing.
        std::fs::create_dir_all(cache.file.parent().unwrap()).unwrap();
        std::fs::write(&cache.file, DOC).unwrap();
        set_mtime(&cache.file, SystemTime::now() - Duration::from_secs(30 * 3600));
        let (body, origin) = load_raw(&cache, SystemTime::now(), || Err("offline".into()));
        assert_eq!((body.as_str(), origin.to_string().as_str()), (DOC, "stale cache, fetch failed"));
        let (body, _) = load_raw(&cache, SystemTime::now(), || Ok("<html>".into()));
        assert_eq!(body, DOC);
        assert_eq!(std::fs::read_to_string(&cache.file).unwrap(), DOC);

        // A cache file that is not a settings document is not a cache.
        std::fs::write(&cache.file, "{}").unwrap();
        let (_, origin) = load_raw(&cache, SystemTime::now(), || Err("offline".into()));
        assert_eq!(origin, Origin::Nothing("offline".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn resolved(user: &[(&str, &str)]) -> BTreeMap<String, Resolved> {
        let mut layers = vec![Layer {
            source: Source::BuiltIn,
            values: vec![("FFlagUserLaunchedWithBloxstrap".into(), "True".into())],
        }];
        layers.push(Layer {
            source: Source::User,
            values: user.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        });
        flag_layers::resolve(layers)
    }

    #[test]
    fn overrides_land_inside_application_settings_as_strings() {
        let r = resolved(&[
            ("DFIntTaskSchedulerTargetFps", "9999"),
            ("FFlagA", "False"),
            ("IntoxicatedPresentMode", "fifo"),
        ]);
        let merged = merge(r#"{"applicationSettings":{"FFlagA":"True","Keep":"x"},"other":[1]}"#, &r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        let s = &v["applicationSettings"];
        assert_eq!(s["DFIntTaskSchedulerTargetFps"], "9999");
        assert_eq!(s["FFlagA"], "False");
        assert_eq!(s["Keep"], "x");
        assert_eq!(s["FFlagUserLaunchedWithBloxstrap"], "True");
        assert!(s.get("IntoxicatedPresentMode").is_none(), "the client's own keys never reach the engine");
        assert_eq!(v["other"], serde_json::json!([1]));
        assert!(!merged.contains(": "), "the output is compact");
    }

    #[test]
    fn a_document_that_cannot_take_overrides_is_passed_through() {
        let r = resolved(&[("A", "1")]);
        assert_eq!(merge("", &r), Err("the settings document did not parse"));
        assert_eq!(merge("{oops", &r), Err("the settings document did not parse"));
        assert_eq!(merge("{}", &r), Err("no applicationSettings object"));
        assert_eq!(merge(r#"{"applicationSettings":"x"}"#, &r), Err("no applicationSettings object"));
        assert_eq!(with_overrides("{}".into(), &r), "{}");
        assert_eq!(with_overrides(DOC.into(), &BTreeMap::new()), DOC);
    }

    #[test]
    fn the_report_counts_every_key_and_names_conflicts() {
        let r = resolved(&[("FFlagUserLaunchedWithBloxstrap", "False"), ("IntoxicatedGraphicsBackend", "vulkan")]);
        assert_eq!(
            report_lines(&r),
            vec![
                "  flags: 2 override(s) applied".to_string(),
                "  flags: FFlagUserLaunchedWithBloxstrap = False from user (overrides built-in=True)".to_string(),
            ]
        );
    }

    /// Needs the network, so it is run by hand: `cargo test --release -p
    /// intoxicated-runtime roblox_settings -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_endpoint_answers_a_settings_document() {
        let body = crate::net::get_text(SETTINGS_URL).expect("the settings endpoint answers");
        assert!(plausible(&body));
        let merged = merge(&body, &resolved(&[("DFIntTaskSchedulerTargetFps", "9999")])).unwrap();
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["applicationSettings"]["DFIntTaskSchedulerTargetFps"], "9999");
        println!("{} bytes fetched, {} merged", body.len(), merged.len());
    }

    #[test]
    fn keep_flags_switches() {
        use KeepFlags::*;
        assert_eq!(keep_flags_mode(None, None), AfterReload);
        assert_eq!(keep_flags_mode(Some("off"), None), Off);
        assert_eq!(keep_flags_mode(Some("0"), Some("2000")), Off);
        assert_eq!(keep_flags_mode(Some("on"), Some("2000")), Every(2000));
        assert_eq!(keep_flags_mode(Some(""), Some("1000")), Every(1000));
        assert_eq!(keep_flags_mode(None, Some("999")), AfterReload);
        assert_eq!(keep_flags_mode(None, Some("-5")), AfterReload);
        assert_eq!(keep_flags_mode(None, Some("2.5")), AfterReload);
    }

    /// A reload is answered once, a second after it; nothing is sent in
    /// between; with no reload seen at all, the fallback starts at 150 s and
    /// repeats every 120 s, and stops once a reload is seen.
    #[test]
    fn re_sends_follow_the_engines_reloads() {
        let s = |secs: f64| Duration::from_secs_f64(secs);
        let mut r = ReloadSchedule::default();
        assert!(!r.tick(s(5.0), 0) && !r.tick(s(60.0), 0));
        assert!(!r.tick(s(120.2), 1), "not before the delay");
        assert!(!r.tick(s(121.0), 1));
        assert!(r.tick(s(121.25), 1));
        assert!(!r.tick(s(121.5), 1) && !r.tick(s(200.0), 1), "once per reload");
        assert!(!r.tick(s(240.1), 2));
        assert!(r.tick(s(241.2), 2));
        assert!(!r.tick(s(400.0), 2), "no fallback once reloads are seen");
        assert!(!r.fell_back);

        let mut f = ReloadSchedule::default();
        assert!(!f.tick(s(149.9), 0));
        assert!(f.tick(s(150.0), 0) && f.fell_back);
        assert!(!f.tick(s(269.0), 0));
        assert!(f.tick(s(270.0), 0));
        assert!(!f.tick(s(300.0), 1) && f.tick(s(301.0), 1), "a late reload is still answered");
        assert!(!f.tick(s(500.0), 1), "and ends the fallback");
    }
}
