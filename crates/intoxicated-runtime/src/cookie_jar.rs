//! The Roblox session across restarts: the engine's cookie jar, kept in the
//! profile.
//!
//! The engine holds its cookies in memory and never writes them to disk; on
//! Android the Java side persists them. This client has no Java side, so it
//! reads the jar out of the engine, keeps it in `<profile>/cookies` through
//! [`crate::secrets`], and hands it back on the next launch.
//!
//! **The two natives speak different formats.** `nativeGetCookiesForDomain`
//! answers Netscape `cookies.txt` records (seven TAB-separated fields, joined
//! by `"; "`), and `nativeSetMultipleCookies` wants a `Cookie:` header body
//! (`name=value` pairs joined by `"; "`). Feeding the getter's text to the
//! setter is accepted silently and leaves the engine holding nothing, so every
//! jar is converted ([`setter_form`]) before it is stored.
//!
//! **Timing.** Both natives return normally and do nothing until
//! `nativeAppBridgeV2InitWithParams` has run, so [`restore`] is called after
//! the app bridge and before the app shell starts. Saves are driven from the
//! looper ([`tick`]), never from the engine's change callback: that arrives on
//! the engine's HTTP thread, and reading the jar back from inside it would
//! re-enter the engine on its own thread. The callback only marks the jar
//! dirty.
//!
//! **Nothing here prints a cookie value, at any verbosity.** Jars travel as
//! [`Jar`], whose `Debug` shows a length. Host names, counts, byte lengths and
//! paths are what the lines below report.
//!
//! **The store format.** UTF-8, LF line endings, every line ending in LF. A
//! header line starting with `#`, then one record per line, `host` TAB
//! `escaped jar`, hosts in byte order. The escapes are `\\`, `\t`, `\n` and
//! `\r` and nothing else. See [`parse_store`] for the reading rules, which
//! also accept the unsorted stores older versions wrote.

use std::collections::BTreeSet;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use intoxicated_linker_sys::game_activity::{self as engine, Jar};

use crate::secrets::{self, Kind};

/// The class both cookie natives are static members of.
const SETTINGS_CLASS: &str = "com/roblox/engine/jni/NativeSettingsInterface";

/// Hosts asked at every save whatever else is known.
pub const SEED_HOSTS: [&str; 4] = ["roblox.com", ".roblox.com", "apis.roblox.com", "auth.roblox.com"];

/// The store's first line. Readers skip any line starting with `#`, so older
/// builds read a store with this header as well as their own.
pub const HEADER: &str = "# intoxicated cookie store v1 -- a live Roblox session. Treat it as a password.";

/// Time between two `periodic` saves.
pub const PERIOD: Duration = Duration::from_secs(30);

/// The switch that turns persistence off for a run.
pub const SKIP_VARIABLE: &str = "INTOXICATED_SKIP_COOKIES";

static SETTER: AtomicUsize = AtomicUsize::new(0);
static GETTER: AtomicUsize = AtomicUsize::new(0);
/// Set by the change callback, cleared by the next tick.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Set once the saved session has been handed back. Until then the engine's
/// jar is not the user's session and must not be written over it.
static RESTORED: AtomicBool = AtomicBool::new(false);
/// Hosts that were in the store at startup, and hosts the callback reported.
static OBSERVED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// When the next `periodic` save is due; `None` before the first tick.
static NEXT_PERIODIC: Mutex<Option<Instant>> = Mutex::new(None);
/// One save at a time.
static SAVING: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether this run keeps the session. Read once.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(SKIP_VARIABLE).is_none())
}

/// One stored host and its jar, in setter form.
#[derive(Debug)]
pub struct Record {
    pub host: String,
    pub jar: Jar,
}

/// What a store's text held.
#[derive(Debug, Default)]
pub struct Loaded {
    /// The usable records, in file order. Duplicated hosts are kept as they are.
    pub records: Vec<Record>,
    /// Records refused because their jar is not in setter form.
    pub rejected: usize,
}

/// Escape a jar so it fits on one line.
pub fn escape(jar: &str) -> String {
    let mut out = String::with_capacity(jar.len());
    for c in jar.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Undo [`escape`]. An unknown escape stays as its two characters and a lone
/// trailing backslash stays as it is.
pub fn unescape(stored: &str) -> String {
    let mut out = String::with_capacity(stored.len());
    let mut chars = stored.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Read a store's text.
///
/// Lines starting with `#` and lines that are blank are skipped; the header's
/// text is not checked. A line is split at its first TAB into host and jar,
/// and a line with no TAB is skipped. A jar that, unescaped, still contains a
/// TAB or contains no `=` is in the getter's Netscape form, which the engine
/// would accept and silently discard, so it is counted in
/// [`Loaded::rejected`] instead of being returned.
pub fn parse_store(text: &str) -> Loaded {
    let mut loaded = Loaded::default();
    for line in text.split('\n') {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((host, stored)) = line.split_once('\t') else { continue };
        let jar = unescape(stored);
        if jar.contains('\t') || !jar.contains('=') {
            loaded.rejected += 1;
            continue;
        }
        loaded.records.push(Record { host: host.to_string(), jar: Jar::from_stored(jar) });
    }
    loaded
}

/// Whether a host can be written and read back as the same host.
fn storable_host(host: &str) -> bool {
    !host.trim().is_empty() && !host.starts_with('#') && !host.contains(['\t', '\n', '\r'])
}

/// The store's text for `records`, in the order given. A host that could not
/// be read back unchanged is left out.
pub fn render_store(records: &[Record]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for r in records.iter().filter(|r| storable_host(&r.host)) {
        out.push_str(&r.host);
        out.push('\t');
        out.push_str(&escape(r.jar.expose()));
        out.push('\n');
    }
    out
}

/// The getter's Netscape records as the setter's `name=value; ...` text.
///
/// Records are split on `"; "` and each on TAB; a record with fewer than seven
/// fields, or an empty name, is dropped. The name is the second-to-last field
/// and the value the last. INFERRED limit: a value that itself contains
/// `"; "` would be cut; no stored value contains `;` at all.
pub fn setter_form(getter_output: &str) -> String {
    let pairs: Vec<String> = getter_output
        .split("; ")
        .filter_map(|record| {
            let fields: Vec<&str> = record.split('\t').collect();
            if fields.len() < 7 {
                return None;
            }
            let name = fields[fields.len() - 2];
            let value = fields[fields.len() - 1];
            (!name.is_empty()).then(|| format!("{name}={value}"))
        })
        .collect();
    pairs.join("; ")
}

/// How many cookies a setter-form jar holds.
pub fn cookie_count(jar: &str) -> usize {
    if jar.is_empty() {
        0
    } else {
        jar.split("; ").count()
    }
}

/// Take the three natives this module drives, as resolved from the engine.
///
/// Call once, after `initializeNativeCode`, as early as the symbols resolve:
/// the change handler only reports changes, so it has to be registered before
/// any `Set-Cookie` arrives. Prints one line per native that is missing.
pub fn attach(setter: Option<*mut c_void>, getter: Option<*mut c_void>, register: Option<*mut c_void>) {
    if !enabled() {
        println!("  [cookies] persistence off ({SKIP_VARIABLE})");
        return;
    }
    match setter {
        None => println!("  [cookies] nativeSetMultipleCookies not exported; a saved session cannot be restored"),
        Some(f) => SETTER.store(f as usize, Ordering::Release),
    }
    match getter {
        None => println!("  [cookies] nativeGetCookiesForDomain not exported; a session cannot be saved"),
        Some(f) => GETTER.store(f as usize, Ordering::Release),
    }
    match register {
        None => println!("  [cookies] updateOnSetCookieHandler not exported; cookie changes will not be noticed"),
        Some(f) => match engine::cookies_register_handler(f, observe_host) {
            Ok(()) => println!("  [cookies] OnSetCookieHandler registered"),
            Err(e) => println!("  [cookies] updateOnSetCookieHandler failed: {e}"),
        },
    }
}

/// The engine's jar for `host`, in setter form.
fn read_jar(getter: usize, host: &str) -> Result<String, String> {
    engine::cookies_for_domain(getter as *mut c_void, SETTINGS_CLASS, host).map(|raw| setter_form(raw.expose()))
}

/// Hand the saved session back to the engine.
///
/// Call after `nativeAppBridgeV2InitWithParams` (earlier, the setter does
/// nothing) and before the app shell starts, which is what issues the first
/// authenticated request. Every stored host joins the set saves re-read, so a
/// host once saved is not dropped by the next save.
pub fn restore() {
    if !enabled() {
        return;
    }
    let dir = crate::profile::active();
    let store = secrets::active();
    let loaded = parse_store(&secrets::load(store, &dir, Kind::Cookies).unwrap_or_default());
    if loaded.rejected > 0 {
        println!(
            "  [cookies] discarded {} record(s) in an old on-disk format; they would have been accepted by \
             the engine and silently ignored. Signing in again will write the store in the current format.",
            loaded.rejected
        );
    }
    lock(&OBSERVED).extend(loaded.records.iter().map(|r| r.host.clone()));

    let setter = SETTER.load(Ordering::Acquire);
    if setter == 0 {
        // `attach` has said why. Without a setter the session can never be
        // given back, so the engine's jar is never written over it either.
        return;
    }
    let mut restored = 0;
    for r in &loaded.records {
        match engine::call_static_strings(setter as *mut c_void, SETTINGS_CLASS, &[&r.host, r.jar.expose()]) {
            Ok(()) => restored += 1,
            Err(e) => eprintln!("[cookies] {}: nativeSetMultipleCookies failed: {e}", r.host),
        }
    }
    let getter = GETTER.load(Ordering::Acquire);
    if restored > 0 && getter != 0 {
        for r in &loaded.records {
            let wanted = cookie_count(r.jar.expose());
            match read_jar(getter, &r.host) {
                Ok(jar) if cookie_count(&jar) >= wanted => {
                    println!("  [cookies] {}: engine holds {} cookie(s) after restore", r.host, cookie_count(&jar))
                }
                Ok(jar) => println!(
                    "  [cookies] {}: wrote {wanted} cookie(s) but the engine holds {}; the session did not take",
                    r.host,
                    cookie_count(&jar)
                ),
                Err(e) => println!("  [cookies] {}: could not read back: {e}", r.host),
            }
        }
    }
    println!(
        "  [cookies] restored {restored} domain(s) from {}",
        secrets::where_kept(store, &dir, Kind::Cookies)
    );
    RESTORED.store(true, Ordering::Release);
}

/// One looper tick: save at once if the jar changed, else every [`PERIOD`],
/// starting with the first tick.
pub fn tick() {
    if !RESTORED.load(Ordering::Acquire) {
        return;
    }
    if DIRTY.swap(false, Ordering::AcqRel) {
        save("changed");
        return;
    }
    let now = Instant::now();
    let due = {
        let mut next = lock(&NEXT_PERIODIC);
        let due = next.map_or(true, |t| now >= t);
        if due {
            *next = Some(now + PERIOD);
        }
        due
    };
    if due {
        save("periodic");
    }
}

/// Read every known host's jar out of the engine and rewrite the store.
///
/// `reason` is printed (`changed`, `periodic`, `teardown`). When every jar is
/// empty nothing is written, so a moment when the engine holds nothing cannot
/// erase a saved session. Call at teardown before the engine's shutdown
/// descent: after `terminateNativeCode` there is no jar to read.
pub fn save(reason: &str) {
    let getter = GETTER.load(Ordering::Acquire);
    if !enabled() || getter == 0 || !RESTORED.load(Ordering::Acquire) {
        return;
    }
    let _one_at_a_time = lock(&SAVING);
    let (hosts, observed) = {
        let observed = lock(&OBSERVED);
        let mut hosts: BTreeSet<String> = SEED_HOSTS.iter().map(|h| h.to_string()).collect();
        hosts.extend(observed.iter().cloned());
        (hosts, observed.len())
    };
    let mut records = Vec::new();
    for host in &hosts {
        match read_jar(getter, host) {
            Ok(jar) if jar.is_empty() => {}
            Ok(jar) => records.push(Record { host: host.clone(), jar: Jar::from_stored(jar) }),
            Err(e) => eprintln!("[cookies] {host}: {e}"),
        }
    }
    if records.is_empty() {
        if reason != "periodic" {
            println!(
                "  [cookies] {reason}: the engine's jar is empty for all {} host(s) tried ({observed} observed); \
                 nothing saved",
                hosts.len()
            );
        }
        return;
    }
    let bytes: usize = records.iter().map(|r| r.jar.len()).sum();
    let dir = crate::profile::active();
    let store = secrets::active();
    match secrets::save(store, &dir, Kind::Cookies, &render_store(&records)) {
        Ok(()) => println!(
            "  [cookies] {reason}: saved {} domain(s), {bytes} bytes to {}",
            records.len(),
            secrets::where_kept(store, &dir, Kind::Cookies)
        ),
        Err(e) => eprintln!("[cookies] the session was not saved: {e}"),
    }
}

/// The engine's change callback: a host whose jar changed.
///
/// Runs on the engine's HTTP thread. Records the host and marks the jar dirty,
/// and nothing else.
pub extern "C" fn observe_host(host: *const c_char) {
    if host.is_null() {
        return;
    }
    // SAFETY: the C side passes a NUL-terminated string valid for this call.
    let host = unsafe { CStr::from_ptr(host) }.to_string_lossy();
    if host.is_empty() {
        return;
    }
    lock(&OBSERVED).insert(host.into_owned());
    DIRTY.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake jar shaped like today's: five pairs, values with `=` and spaces.
    fn fake_jar(tag: &str) -> String {
        [
            format!("GuestData=UserID=-{tag}"),
            "RBXAppDeviceIdentifier=AppDeviceID=fake device".to_string(),
            "rbx-ip2=1".to_string(),
            format!(".ROBLOSECURITY=_FAKE_{tag}_{}", "x".repeat(40)),
            "RBXEventTrackerV2=CreateDate=1/1/2026 browserid=0".to_string(),
        ]
        .join("; ")
    }

    #[test]
    fn escapes_round_trip_and_unknown_ones_survive() {
        for s in ["plain=1", "a\\b=c", "tab\there=1", "line\nbreak=1", "cr\r=1", "\\t literal=1", ""] {
            assert_eq!(unescape(&escape(s)), s, "{s:?}");
            assert!(!escape(s).contains(['\t', '\n', '\r']));
        }
        assert_eq!(unescape("a\\qb"), "a\\qb");
        assert_eq!(unescape("trailing\\"), "trailing\\");
        assert_eq!(escape("a;b c=d"), "a;b c=d", "nothing else is escaped");
    }

    #[test]
    fn a_store_round_trips_with_sorted_hosts() {
        let hosts = [".roblox.com", "apis.roblox.com", "auth.roblox.com", "roblox.com"];
        let records: Vec<Record> = hosts
            .iter()
            .map(|h| Record { host: h.to_string(), jar: Jar::from_stored(fake_jar(h)) })
            .collect();
        let text = render_store(&records);
        assert!(text.starts_with(HEADER));
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 1 + hosts.len());
        let back = parse_store(&text);
        assert_eq!(back.rejected, 0);
        let got: Vec<(&str, &str)> = back.records.iter().map(|r| (r.host.as_str(), r.jar.expose())).collect();
        let want: Vec<(&str, String)> = hosts.iter().map(|h| (*h, fake_jar(h))).collect();
        assert_eq!(got, want.iter().map(|(h, j)| (*h, j.as_str())).collect::<Vec<_>>());
    }

    #[test]
    fn todays_store_reads_record_for_record() {
        // Built from the structure of the store on disk today: the header,
        // six hosts, one TAB per line, five pairs per jar, no escapes.
        let hosts =
            [".roblox.com", "apis.roblox.com", "assetdelivery.roblox.com", "auth.roblox.com", "games.roblox.com", "roblox.com"];
        let mut text = format!("{HEADER}\n");
        let jar = fake_jar("today");
        for h in hosts {
            text.push_str(&format!("{h}\t{jar}\n"));
        }
        let loaded = parse_store(&text);
        assert_eq!(loaded.rejected, 0);
        assert_eq!(loaded.records.len(), 6);
        for (r, h) in loaded.records.iter().zip(hosts) {
            assert_eq!(r.host, h);
            assert_eq!(r.jar.expose(), jar);
            assert_eq!(cookie_count(r.jar.expose()), 5);
        }
        // Written back, the store is identical.
        assert_eq!(render_store(&loaded.records), text);
    }

    #[test]
    fn the_older_unsorted_store_with_single_pair_jars_reads_in_file_order() {
        let hosts = [
            ".roblox.com",
            "apis.roblox.com",
            "auth.roblox.com",
            "roblox.com",
            "www.roblox.com",
            "games.roblox.com",
            "assetdelivery.roblox.com",
            "catalog.roblox.com",
            "thumbnails.roblox.com",
            "assetgame.roblox.com",
            "gamejoin.roblox.com",
        ];
        let mut text = format!("{HEADER}\n");
        for (i, h) in hosts.iter().enumerate() {
            let jar = if i < 4 { "a=1; b=2; .ROBLOSECURITY=_FAKE".to_string() } else { ".ROBLOSECURITY=_FAKE".to_string() };
            text.push_str(&format!("{h}\t{jar}\n"));
        }
        let loaded = parse_store(&text);
        assert_eq!(loaded.rejected, 0);
        let order: Vec<&str> = loaded.records.iter().map(|r| r.host.as_str()).collect();
        assert_eq!(order, hosts);
        assert_eq!(cookie_count(loaded.records[0].jar.expose()), 3);
        assert_eq!(cookie_count(loaded.records[10].jar.expose()), 1);
    }

    #[test]
    fn the_reading_rules() {
        let text = "# any header at all\n\n   \nno tab here\nhost.a\ta=1\nhost.b\tno-equals\n\
                    host.c\t.roblox.com\\tTRUE\\t/\\tTRUE\\t0\\tn\\tv\nhost.a\tb=2\nhost.d\tx=1\\ty\n\
                    host.e\tescaped\\\\=1\n";
        let loaded = parse_store(text);
        // `no-equals` has no `=`; host.c and host.d unescape to a TAB.
        assert_eq!(loaded.rejected, 3);
        let got: Vec<(&str, &str)> = loaded.records.iter().map(|r| (r.host.as_str(), r.jar.expose())).collect();
        assert_eq!(got, vec![("host.a", "a=1"), ("host.a", "b=2"), ("host.e", "escaped\\=1")]);
        assert!(parse_store("").records.is_empty());
        assert!(parse_store("garbage\0\u{1}").records.is_empty());
    }

    #[test]
    fn hosts_that_would_not_read_back_are_not_written() {
        let records: Vec<Record> = ["ok.roblox.com", "bad\thost", "bad\nhost", "#comment", ""]
            .iter()
            .map(|h| Record { host: h.to_string(), jar: Jar::from_stored("a=1".into()) })
            .collect();
        let back = parse_store(&render_store(&records));
        assert_eq!(back.records.len(), 1);
        assert_eq!(back.records[0].host, "ok.roblox.com");
    }

    #[test]
    fn netscape_records_become_name_value_pairs() {
        let getter = "#HttpOnly_.roblox.com\tTRUE\t/\tTRUE\t1893456000\t.ROBLOSECURITY\t_FAKE; \
                      .roblox.com\tTRUE\t/\tFALSE\t0\tGuestData\tUserID=-1; \
                      short\tTRUE\t/; \
                      .roblox.com\tTRUE\t/\tFALSE\t0\t\tnameless; \
                      .roblox.com\tTRUE\t/\tFALSE\t0\tRBXEventTrackerV2\tCreateDate=1/1/2026 browserid=0";
        assert_eq!(
            setter_form(getter),
            ".ROBLOSECURITY=_FAKE; GuestData=UserID=-1; RBXEventTrackerV2=CreateDate=1/1/2026 browserid=0"
        );
        assert_eq!(setter_form(""), "");
        assert_eq!(cookie_count(""), 0);
        assert_eq!(cookie_count("a=1"), 1);
        // Already in setter form: nothing has seven fields, nothing survives.
        assert_eq!(setter_form("a=1; b=2"), "");
    }

    #[test]
    fn a_jar_never_shows_its_value() {
        let r = Record { host: "roblox.com".into(), jar: Jar::from_stored(".ROBLOSECURITY=_DO_NOT_PRINT".into()) };
        let shown = format!("{r:?}");
        assert!(!shown.contains("DO_NOT_PRINT"), "{shown}");
        assert!(shown.contains("roblox.com"));
    }
}
