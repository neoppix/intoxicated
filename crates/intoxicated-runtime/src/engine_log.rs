//! What the engine's own log says about joining and leaving a game.
//!
//! The engine writes one log per session into `<files>/appData/logs/`, named
//! `<version>_<UTC stamp>_Player_<hex>_last.log`, and appends to it. The
//! looper calls [`poll`] every tick; it follows the newest `*.log` file in that
//! directory and prints one line per event it recognises:
//!
//! ```text
//! [intoxicated] game: joining server <job id> of place <place id>
//! [intoxicated] game: joined place <place id> (universe <universe id>)
//! [intoxicated] game: server <address>:<port>
//! [intoxicated] game: left
//! ```
//!
//! It also counts, without printing anything, each time the engine's own
//! settings reloader finishes a fetch ([`flag_reloads_seen`]); the keep-flags
//! loop in [`crate::roblox_settings`] re-applies the launch flags after it.
//!
//! **The joined line does not name the user.** The engine's line carries the
//! Roblox user id; it is parsed (a join without one is not a join) and never
//! printed.
//!
//! Polling never blocks and never fails: a missing directory or file is
//! nothing to report. The directory is rescanned at most once a second; in
//! between, only what was appended is read. A new newest file is followed from
//! its start, because a session's first lines belong to that session. A file
//! that got shorter is read again from the start. A last line with no LF yet
//! waits for the rest.
//!
//! With `INTOXICATED_CLOSE_ON_LEAVE` set to `1`, `true` or `yes`, a leave also
//! asks the pump to stop, through the normal teardown.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How often the directory is rescanned for a newer log.
const RESCAN: Duration = Duration::from_secs(1);

/// Most bytes read in one poll, so a large backlog cannot stall the looper.
const READ_CAP: u64 = 256 * 1024;

/// The switch that ends the run when the player leaves a game.
pub const CLOSE_ON_LEAVE_VARIABLE: &str = "INTOXICATED_CLOSE_ON_LEAVE";

/// Something the engine's log said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Joining { job: String, place: u64 },
    /// `user` is parsed and must never be printed.
    Joined { place: u64, universe: u64, user: u64 },
    Server { address: String, port: u16 },
    Left,
    /// The engine re-fetched and applied Roblox's settings document, which
    /// resets the dynamic flags the document does not carry.
    FlagsReloaded,
}

impl Event {
    /// The line printed for this event.
    pub fn line(&self) -> String {
        match self {
            Event::Joining { job, place } => format!("[intoxicated] game: joining server {job} of place {place}"),
            Event::Joined { place, universe, .. } => {
                format!("[intoxicated] game: joined place {place} (universe {universe})")
            }
            Event::Server { address, port } => format!("[intoxicated] game: server {address}:{port}"),
            Event::Left => "[intoxicated] game: left".to_string(),
            Event::FlagsReloaded => "[intoxicated] flags: the engine reloaded its settings".to_string(),
        }
    }
}

/// The digits right after `label`, if there are any.
fn number_after(line: &str, label: &str) -> Option<u64> {
    let rest = &line[line.find(label)? + label.len()..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `text` up to the first `,`, space or `|`.
fn field(text: &str) -> &str {
    let end = text.find([',', ' ', '|']).unwrap_or(text.len());
    &text[..end]
}

/// The event one engine log line carries, if any.
pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim_end();
    if line.contains("[FLog::SingleSurfaceApp] leaveUGCGameInternal") {
        return Some(Event::Left);
    }
    if line.contains("[FLog::DynamicFastVariableReloader] DynamicFastVariableReloader finished flag fetch") {
        return Some(Event::FlagsReloaded);
    }
    if line.contains("[FLog::GameJoinLoadTime]") && line.contains("game_join_loadtime") {
        return Some(Event::Joined {
            place: number_after(line, "placeid:")?,
            universe: number_after(line, "universeid:")?,
            user: number_after(line, "userid:")?,
        });
    }
    if let Some(at) = line.find("! Joining game '") {
        let rest = &line[at + "! Joining game '".len()..];
        let job = &rest[..rest.find('\'')?];
        // The job id ends up in URLs, so it has to be exactly a UUID's shape.
        if job.len() != 36 || !job.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return None;
        }
        let place = number_after(rest, "' place ")?;
        return Some(Event::Joining { job: job.to_string(), place });
    }
    if let Some(at) = line.find("[FLog::Network] UDMUX Address") {
        let rest = &line[at..];
        let address = field(&rest[rest.find("UDMUX Address = ")? + "UDMUX Address = ".len()..]);
        // The first `Port = ` after the UDMUX address is its port; the RCC
        // address later on the line is a private one.
        let port = field(&rest[rest.find("Port = ")? + "Port = ".len()..]).parse().ok()?;
        if address.is_empty() {
            return None;
        }
        return Some(Event::Server { address: address.to_string(), port });
    }
    None
}

/// The file being followed and how far it has been read.
#[derive(Debug, Default)]
pub struct Follower {
    dir: Option<PathBuf>,
    file: Option<PathBuf>,
    offset: u64,
    partial: Vec<u8>,
    last_scan: Option<Instant>,
}

impl Follower {
    pub const fn new() -> Follower {
        Follower { dir: None, file: None, offset: 0, partial: Vec::new(), last_scan: None }
    }

    /// Follow the logs in `dir` from now on.
    pub fn watch(&mut self, dir: PathBuf) {
        *self = Follower { dir: Some(dir), ..Follower::new() };
    }

    /// The newest `*.log` in the directory.
    fn newest(dir: &Path) -> Option<PathBuf> {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .max()
            .map(|(_, p)| p)
    }

    /// Whatever events were appended since the last call.
    pub fn poll(&mut self, now: Instant) -> Vec<Event> {
        let Some(dir) = self.dir.clone() else { return Vec::new() };
        if self.last_scan.map_or(true, |t| now.duration_since(t) >= RESCAN) {
            self.last_scan = Some(now);
            if let Some(newest) = Follower::newest(&dir) {
                if self.file.as_ref() != Some(&newest) {
                    self.file = Some(newest);
                    self.offset = 0;
                    self.partial.clear();
                }
            }
        }
        let Some(file) = &self.file else { return Vec::new() };
        let Ok(mut f) = std::fs::File::open(file) else { return Vec::new() };
        let Ok(len) = f.metadata().map(|m| m.len()) else { return Vec::new() };
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut bytes = Vec::new();
        let Ok(n) = f.take(READ_CAP).read_to_end(&mut bytes) else { return Vec::new() };
        self.offset += n as u64;
        self.take_lines(&bytes)
    }

    /// Split newly read bytes into complete lines and parse them.
    fn take_lines(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.partial.extend_from_slice(bytes);
        let Some(last_lf) = self.partial.iter().rposition(|&b| b == b'\n') else { return Vec::new() };
        let rest = self.partial.split_off(last_lf + 1);
        let complete = std::mem::replace(&mut self.partial, rest);
        complete
            .split(|&b| b == b'\n')
            .filter_map(|line| parse_line(&String::from_utf8_lossy(line)))
            .collect()
    }
}

static FOLLOWER: Mutex<Follower> = Mutex::new(Follower::new());

fn follower() -> MutexGuard<'static, Follower> {
    FOLLOWER.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Follow the engine's logs in `dir` (`<files>/appData/logs`).
pub fn watch(dir: PathBuf) {
    follower().watch(dir);
}

fn close_on_leave() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| matches!(std::env::var(CLOSE_ON_LEAVE_VARIABLE).as_deref(), Ok("1" | "true" | "yes")))
}

/// Joins the engine's log has reported in this process.
static JOINS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many `joining server` events [`poll`] has seen. The join link reads it
/// so that it never publishes again once a join is under way.
pub fn joins_seen() -> u64 {
    JOINS.load(std::sync::atomic::Ordering::Acquire)
}

/// Settings reloads the engine's log has reported in this process.
static RELOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times [`poll`] has seen the engine's settings reloader finish a
/// fetch: every 120 s in a run on 2.748 (measured over an hour).
pub fn flag_reloads_seen() -> u64 {
    RELOADS.load(std::sync::atomic::Ordering::Acquire)
}

/// One looper tick: print what the engine's log reported since the last one.
pub fn poll() {
    let events = follower().poll(Instant::now());
    for event in events {
        if matches!(event, Event::Joining { .. }) {
            JOINS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        if event == Event::FlagsReloaded {
            RELOADS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            continue;
        }
        println!("{}", event.line());
        if event == Event::Left && close_on_leave() {
            println!("[intoxicated] game: {CLOSE_ON_LEAVE_VARIABLE} is set; closing");
            crate::android::looper::request_quit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    const PREFIX: &str = "2026-11-22T17:00:00.000Z,12.345678,1a2b,6 ";
    const JOB: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

    fn joined() -> String {
        format!(
            "{PREFIX}[FLog::GameJoinLoadTime] Report game_join_loadtime: sid:{JOB}, clienttime:12.5, \
             join_time:3.25, referral_page:deeplink, placeid:5938036553, userid:987654321, universeid:2055331556,"
        )
    }

    fn joining() -> String {
        format!("{PREFIX}[FLog::Output] ! Joining game '{JOB}' place 5938036553 at 10.0.0.1")
    }

    fn server() -> String {
        format!("{PREFIX}[FLog::Network] UDMUX Address = 128.116.1.2, Port = 56789 | RCC Server Address = 10.1.2.3, Port = 1234")
    }

    #[test]
    fn a_settings_reload_is_recognised() {
        let line = format!(
            "{PREFIX}[FLog::DynamicFastVariableReloader] DynamicFastVariableReloader finished flag fetch. Tombstone status: valid"
        );
        assert_eq!(parse_line(&line), Some(Event::FlagsReloaded));
        assert_eq!(parse_line(&format!("{PREFIX}[FLog::AndroidGLView] nativeInitClientSettings")), None);
    }

    #[test]
    fn the_four_events() {
        assert_eq!(
            parse_line(&joined()),
            Some(Event::Joined { place: 5938036553, universe: 2055331556, user: 987654321 })
        );
        assert_eq!(parse_line(&joining()), Some(Event::Joining { job: JOB.into(), place: 5938036553 }));
        assert_eq!(parse_line(&server()), Some(Event::Server { address: "128.116.1.2".into(), port: 56789 }));
        assert_eq!(
            parse_line(&format!("{PREFIX}[FLog::SingleSurfaceApp] leaveUGCGameInternal  \r")),
            Some(Event::Left)
        );
    }

    #[test]
    fn the_joined_line_does_not_name_the_user() {
        let line = parse_line(&joined()).unwrap().line();
        assert_eq!(line, "[intoxicated] game: joined place 5938036553 (universe 2055331556)");
        assert!(!line.contains("987654321"));
    }

    #[test]
    fn the_printed_lines() {
        assert_eq!(
            parse_line(&joining()).unwrap().line(),
            format!("[intoxicated] game: joining server {JOB} of place 5938036553")
        );
        assert_eq!(parse_line(&server()).unwrap().line(), "[intoxicated] game: server 128.116.1.2:56789");
        assert_eq!(Event::Left.line(), "[intoxicated] game: left");
    }

    #[test]
    fn what_is_not_an_event() {
        // A join without all three ids.
        assert_eq!(parse_line(&joined().replace("userid:987654321", "userid:")), None);
        assert_eq!(parse_line(&joined().replace("universeid:", "universe:")), None);
        // A job id that is not a UUID's shape.
        assert_eq!(parse_line(&joining().replace(JOB, "not-a-uuid")), None);
        assert_eq!(parse_line(&joining().replace(JOB, &JOB.replace('0', "g"))), None);
        // A port that does not fit.
        assert_eq!(parse_line(&server().replace("56789", "70000")), None);
        // Disconnects are not a leave.
        for line in [
            "[FLog::Network] Connection lost",
            "[FLog::Network] Client:Disconnect 277",
            "[FLog::Network] Disconnection Notification. Reason: 277",
            "[FLog::Output] websocket 401",
            "[FLog::MegaReplicator] clean up",
        ] {
            assert_eq!(parse_line(&format!("{PREFIX}{line}")), None, "{line}");
        }
    }

    #[test]
    fn the_follower_reads_appends_partial_lines_and_newer_files() {
        let dir = std::env::temp_dir().join(format!("intox-enginelog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut f = Follower::new();
        let t0 = Instant::now();
        f.watch(dir.clone());
        assert!(f.poll(t0).is_empty(), "no directory is nothing to report");
        std::fs::create_dir_all(&dir).unwrap();

        let first = dir.join("2.748.0.1527_20261122T170000Z_Player_a1b2c_last.log");
        std::fs::write(&first, format!("{}\nunrelated\n", joining())).unwrap();
        // Not rescanned within a second of the last scan.
        assert!(f.poll(t0 + Duration::from_millis(10)).is_empty());
        assert_eq!(f.poll(t0 + RESCAN).len(), 1, "a new file is read from its start");

        // Half a line, then the rest.
        let mut out = std::fs::OpenOptions::new().append(true).open(&first).unwrap();
        let s = joined();
        let (a, b) = s.split_at(40);
        write!(out, "{a}").unwrap();
        assert!(f.poll(t0 + RESCAN).is_empty());
        writeln!(out, "{b}").unwrap();
        writeln!(out, "{}", server()).unwrap();
        let events = f.poll(t0 + RESCAN);
        assert!(matches!(events[0], Event::Joined { .. }));
        assert!(matches!(events[1], Event::Server { .. }));
        assert!(f.poll(t0 + RESCAN).is_empty(), "nothing is reported twice");

        // A newer file is switched to.
        let second = dir.join("2.748.0.1527_20261122T180000Z_Player_d4e5f_last.log");
        std::fs::write(&second, format!("{PREFIX}[FLog::SingleSurfaceApp] leaveUGCGameInternal\n")).unwrap();
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(&second).unwrap().set_modified(later).unwrap();
        assert_eq!(f.poll(t0 + RESCAN * 2), vec![Event::Left]);

        // A file that got shorter is read again from the start.
        std::fs::write(&second, "[FLog::SingleSurfaceApp] leaveUGCGameInternal\n").unwrap();
        std::fs::File::options().write(true).open(&second).unwrap().set_modified(later).unwrap();
        assert_eq!(f.poll(t0 + RESCAN * 2), vec![Event::Left]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
