//! The launcher's handling of its first argument, run for real.
//!
//! `packaging/freebsd/intoxicated` is run with `/bin/sh` against a scratch
//! checkout whose `target/release/intoxicated-run` is a script: it hands
//! `--check-join-url` to the real binary and otherwise writes down the
//! arguments and switches it was started with, and the launcher's own
//! arguments as `ps` shows them, then exits 0.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const TICKET: &str = "SYNTHETIC-NOT-A-TICKET";

fn launcher() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/freebsd/intoxicated")
}

const FAKE: &str = r#"#!/bin/sh
if [ "$1" = --check-join-url ]; then
  exec "$REAL_INTOXICATED_RUN" --check-join-url
fi
for a in "$@"; do printf '%s\n' "$a"; done > "$RECORD/args"
printf '%s\n' "${INTOXICATED_DEEPLINK_RETRY-unset}" > "$RECORD/retry"
ps -ww -o args= -p "$PPID" > "$RECORD/launcher"
echo started >> "$RECORD/launches"
exit "${FAKE_STATUS:-0}"
"#;

struct Run {
    status: i32,
    stderr: String,
    /// `None` when the client was never started.
    args: Option<Vec<String>>,
    retry: String,
    launcher_args: String,
    /// How many times the launcher started the client.
    launches: usize,
}

impl Run {
    fn join_url(&self) -> Option<&str> {
        let args = self.args.as_ref().expect("the client was started");
        let at = args.iter().position(|a| a == "--join-url")?;
        Some(&args[at + 1])
    }
}

fn run(name: &str, arg: Option<&str>) -> Run {
    run_exiting(name, arg, 0)
}

/// As [`run`], with a client that exits with `status`.
fn run_exiting(name: &str, arg: Option<&str>, status: i32) -> Run {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("launcher_links").join(name);
    let _ = fs::remove_dir_all(&root);
    let bin = root.join("repo/target/release/intoxicated-run");
    fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fs::write(&bin, FAKE).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let apk = root.join("cache/intoxicated/apk");
    fs::create_dir_all(apk.join("lib/x86_64")).unwrap();
    fs::write(apk.join("candidate-0.apk"), b"").unwrap();
    fs::write(apk.join("lib/x86_64/libroblox.so"), b"").unwrap();
    let record = root.join("record");
    fs::create_dir_all(&record).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();

    let mut command = Command::new("/bin/sh");
    command
        .arg(launcher())
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin")
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("INTOXICATED_REPO", root.join("repo"))
        .env("INTOXICATED_FPS", "off")
        .env("INTOXICATED_VIDEO_MEMORY", "off")
        .env("REAL_INTOXICATED_RUN", env!("CARGO_BIN_EXE_intoxicated-run"))
        .env("RECORD", &record)
        .env("FAKE_STATUS", status.to_string());
    if let Some(arg) = arg {
        command.arg(arg);
    }
    let out = command.output().expect("/bin/sh runs");
    let read = |f: &str| fs::read_to_string(record.join(f)).ok();
    Run {
        status: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        args: read("args").map(|a| a.lines().map(str::to_string).collect()),
        retry: read("retry").unwrap_or_default().trim().to_string(),
        launcher_args: read("launcher").unwrap_or_default(),
        launches: read("launches").map_or(0, |l| l.lines().count()),
    }
}

fn joins(name: &str, arg: &str, link: &str) -> Run {
    let r = run(name, Some(arg));
    assert_eq!(r.status, 0, "{arg}: {}", r.stderr);
    assert_eq!(r.join_url(), Some(link), "{arg}");
    assert_eq!(r.retry, "1");
    r
}

fn refused(name: &str, arg: &str) -> Run {
    let r = run(name, Some(arg));
    assert_eq!(r.status, 2, "{arg}: {}", r.stderr);
    assert!(r.args.is_none(), "{arg} started the client");
    assert!(r.stderr.contains("intoxicated: --join-url "), "{arg}: {}", r.stderr);
    r
}

#[test]
fn no_argument_is_no_link() {
    let r = run("none", None);
    assert_eq!(r.status, 0, "{}", r.stderr);
    let args = r.args.as_ref().unwrap();
    assert_eq!(args.len(), 8, "{args:?}");
    assert_eq!([&args[0], &args[2]], ["--lib-dir", "--apk"]);
    assert_eq!(&args[4..], ["--host-libc", "--game-activity", "--run", "0"]);
    assert_eq!(r.join_url(), None);
    assert_eq!(r.retry, "unset");
}

/// A client that stopped because the user quit is not started again: 0 after
/// a close or one Ctrl-C, 130 or 143 after a second SIGINT or SIGTERM, 124
/// after a shutdown the watchdog cut off, and 3 when the profile is already
/// open in another client. Anything else is a failed start and is retried,
/// four launches in all.
#[test]
fn only_a_failed_start_is_relaunched() {
    for status in [0, 3, 124, 130, 143] {
        let r = run_exiting(&format!("exit-{status}"), None, status);
        assert_eq!(r.status, status, "{}", r.stderr);
        assert_eq!(r.launches, 1, "exit {status} was relaunched: {}", r.stderr);
        assert!(!r.stderr.contains("hiccup"), "{}", r.stderr);
    }
    let r = run_exiting("exit-1", None, 1);
    assert_eq!(r.launches, 4, "{}", r.stderr);
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("boot hiccup, retrying (3)"), "{}", r.stderr);
}

#[test]
fn a_place_id_and_engine_links_are_passed_on() {
    joins("digits", "5938036553", "roblox://experiences/start?placeId=5938036553");
    joins("zeros", "0042", "roblox://experiences/start?placeId=0042");
    joins(
        "roblox",
        "roblox://experiences/start?placeId=1818&accessCode=X",
        "roblox://experiences/start?placeId=1818&accessCode=X",
    );
}

#[test]
fn other_link_forms_are_translated_and_the_launcher_runs_with_the_result() {
    let r = joins("player", "roblox-player://placeId=1818", "roblox://placeId=1818");
    assert!(r.launcher_args.contains("roblox://placeId=1818"), "{}", r.launcher_args);
    assert!(!r.launcher_args.contains("roblox-player"), "{}", r.launcher_args);
    joins("upper", "ROBLOX://placeId=1", "roblox://placeId=1");
    joins("web", "https://www.roblox.com/games/1818/Some-Name", "roblox://experiences/start?placeId=1818");
    joins(
        "private",
        "https://www.roblox.com/games/1818?privateServerLinkCode=12345678901234567890",
        "roblox://experiences/start?placeId=1818&linkCode=12345678901234567890",
    );
}

#[test]
fn the_browser_link_joins_and_its_ticket_goes_nowhere() {
    let link = format!(
        "roblox-player:1+launchmode:play+gameinfo:{TICKET}+launchtime:1763820000000+placelauncherurl:\
         https%3A%2F%2Fwww.roblox.com%2FGame%2FPlaceLauncher.ashx%3Frequest%3DRequestGame%26browserTrackerId%3D1\
         %26placeId%3D5938036553%26isPlayTogetherGame%3Dfalse+browsertrackerid:1+robloxLocale:en_us+gameLocale:en_us"
    );
    let r = joins("browser", &link, "roblox://experiences/start?placeId=5938036553");
    assert!(!r.args.as_ref().unwrap().iter().any(|a| a.contains(TICKET)));
    assert!(!r.stderr.contains(TICKET), "{}", r.stderr);
    assert!(r.launcher_args.contains("placeId=5938036553"), "the launcher's own arguments: {}", r.launcher_args);
    assert!(!r.launcher_args.contains(TICKET), "the ticket stayed in the launcher's arguments: {}", r.launcher_args);

    let refused = refused("browser-edit", &link.replace("launchmode:play", "launchmode:edit"));
    assert!(!refused.stderr.contains(TICKET), "{}", refused.stderr);
    assert!(refused.stderr.contains("launchmode"), "{}", refused.stderr);
}

#[test]
fn anything_else_is_refused_before_the_client_starts() {
    for (name, arg) in [
        ("letters", "12a"),
        ("negative", "-5"),
        ("long", "999999999999999999999"),
        ("no-place", "roblox-player:1+launchmode:play+placelauncherurl:x"),
        ("http", "http://roblox.com"),
        ("other-host", "https://evil.example/games/1818"),
        ("mobile", "robloxmobile://experiences/start?placeId=1"),
        ("bad-server", "roblox://experiences/start?placeId=1&gameInstanceId=abc"),
    ] {
        let r = refused(name, arg);
        if arg.len() > 3 {
            assert!(!r.stderr.contains(arg), "the argument was echoed: {}", r.stderr);
        }
    }
}

#[test]
fn the_desktop_entry_hands_the_link_to_the_launcher() {
    let entry = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/freebsd/intoxicated.desktop"))
        .unwrap();
    assert!(entry.lines().any(|l| l == "Exec=intoxicated %u"), "one URL, as the first argument");
    let mime = entry.lines().find_map(|l| l.strip_prefix("MimeType=")).expect("a MimeType line");
    let types: Vec<&str> = mime.split(';').filter(|t| !t.is_empty()).collect();
    assert!(types.contains(&"x-scheme-handler/roblox-player"));
    assert!(types.contains(&"x-scheme-handler/roblox"));
}
