//! `intoxicated-run`'s command line, run for real: what it prints and the status
//! it exits with, for the cases that end before the engine is loaded.
//!
//! Each run gets its own `XDG_DATA_HOME` and `XDG_CACHE_HOME` under Cargo's
//! scratch directory, so nothing here reads or writes a real profile.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BANNER_START: &str = "Intoxicated ";

fn scratch(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("intoxicated_run_cli").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn command(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_intoxicated-run"));
    c.args(args)
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin")
        .env("HOME", root.join("home"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .current_dir(root)
        .stdin(Stdio::null());
    c
}

fn run(root: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut c = command(root, args);
    for (k, v) in extra_env {
        c.env(k, v);
    }
    c.output().expect("intoxicated-run starts")
}

fn stdin_run(root: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = command(root, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("intoxicated-run starts");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn profile_dir(root: &Path, name: &str) -> PathBuf {
    root.join("data/intoxicated/profiles").join(name)
}

#[test]
fn help_prints_only_the_usage_and_exits_2() {
    let root = scratch("help");
    let out = run(&root, &["--help"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let stdout = text(&out.stdout);
    assert!(stdout.starts_with(BANNER_START) && stdout.lines().count() == 1, "{stdout}");
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("usage: intoxicated-run --lib-dir <dir> [options]\n\n"), "{stderr}");
    assert!(stderr.contains("\n\nenv:\n") && stderr.ends_with('\n'));
    assert!(!stderr.contains("error:"));
    assert_eq!(run(&root, &["-h"], &[]).status.code(), Some(2));
}

#[test]
fn a_parse_error_is_a_sentence_then_the_usage_and_exits_2() {
    let root = scratch("parse-errors");
    for (args, message) in [
        (&[][..], "--lib-dir is required"),
        (&["--bogus"][..], "unrecognised argument: --bogus"),
        (&["--lib-dir"][..], "--lib-dir needs a value"),
        (&["--lib-dir", "x", "--run", "abc"][..], "--run wants a number"),
        (&["--lib-dir", "x", "--window", "z"][..], "--window wants a number"),
        (
            &["--lib-dir", "x", "--profile", "../evil"][..],
            "\"../evil\" is not a usable profile name; use letters, digits, - and _",
        ),
        (
            &["--lib-dir", "x", "--flag-overrides", "/nonexistent/f.json"][..],
            "/nonexistent/f.json: No such file or directory (os error 2)",
        ),
        (&["--lib-dir", "x", "--check-join-url"][..], "unrecognised argument: --check-join-url"),
    ] {
        let out = run(&root, args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = text(&out.stderr);
        let expected = format!("error: {message}\n\nusage: intoxicated-run --lib-dir <dir> [options]\n");
        assert!(stderr.starts_with(&expected), "{args:?}: {stderr}");
        assert!(text(&out.stdout).starts_with(BANNER_START));
    }
}

#[test]
fn a_second_profile_name_is_refused() {
    let root = scratch("two-profiles");
    let out = run(&root, &["--lib-dir", "x", "--profile", "a", "--profile", "b"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    let expected = format!(
        "error: this instance already runs {}; a profile cannot be changed while the client is up",
        profile_dir(&root, "a").display()
    );
    assert!(stderr.starts_with(&expected), "{stderr}");
    assert!(!profile_dir(&root, "b").exists(), "the refused profile is not created");
}

#[test]
fn a_refused_join_url_ends_the_launch() {
    let root = scratch("join-url");
    let out = run(&root, &["--lib-dir", "x", "--join-url", "http://example.com/"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).starts_with("error: --join-url has scheme \"http\""), "{}", text(&out.stderr));
}

#[test]
fn check_join_url_runs_nothing_else() {
    let root = scratch("check-join-url");
    for (input, stdout) in [
        (&b"roblox://experiences/start?placeId=5938036553\n"[..], "roblox://experiences/start?placeId=5938036553\n"),
        (&b"https://www.roblox.com/games/5938036553/Some-Name\n"[..], "roblox://experiences/start?placeId=5938036553\n"),
    ] {
        let out = stdin_run(&root, &["--check-join-url"], input);
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), stdout, "no banner");
        assert!(out.stderr.is_empty());
    }
    let out = stdin_run(&root, &["--check-join-url", "--ignored"], b"https://example.com/\n");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(text(&out.stderr), "intoxicated: --join-url was refused: this https link is not to roblox.com\n");
    let out = stdin_run(&root, &["--check-join-url"], b"");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stderr), "intoxicated: --join-url was given an empty string\n");
    assert!(!root.join("data/intoxicated").exists(), "no profile was touched");
}

#[test]
fn print_default_flags_prints_the_built_in_layer_and_nothing_else() {
    let root = scratch("default-flags");
    let out = run(&root, &["--print-default-flags"], &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(!stdout.contains(BANNER_START), "no banner: {stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON object");
    let object = doc.as_object().expect("an object");
    let built_in = intoxicated_runtime::flag_layers::BUILT_IN;
    assert_eq!(object.len(), built_in.len(), "{stdout}");
    for (key, value) in built_in {
        assert_eq!(object[key].as_str(), Some(value), "{key}");
    }
    // Sorted in the text as printed (serde_json's map would sort them anyway).
    let mut positions: Vec<(usize, &str)> =
        built_in.iter().map(|(k, _)| (stdout.find(&format!("\"{k}\"")).unwrap(), *k)).collect();
    positions.sort();
    let printed: Vec<&str> = positions.iter().map(|(_, k)| *k).collect();
    let mut sorted: Vec<&str> = built_in.iter().map(|(k, _)| *k).collect();
    sorted.sort();
    assert_eq!(printed, sorted);
    assert!(!root.join("data/intoxicated").exists(), "no profile was touched");

    let help = run(&root, &["--help"], &[]);
    assert!(text(&help.stderr).contains("\n  --print-default-flags  "), "listed in --help");
}

#[test]
fn a_busy_profile_is_refused_with_3() {
    let root = scratch("busy");
    let dir = profile_dir(&root, "default");
    let held = intoxicated_runtime::profile_lock::claim("default", &dir).expect("the test holds the profile");
    let out = run(&root, &["--lib-dir", "/nonexistent"], &[]);
    assert_eq!(out.status.code(), Some(3));
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with(&format!("error: profile \"default\" is already open in another client (process {}", std::process::id())),
        "{stderr}"
    );
    assert!(stderr.contains("Two clients on one profile corrupt its storage."));
    drop(held);
}

#[test]
fn the_profile_gates_refuse_with_1() {
    let root = scratch("gates");
    let vpn = profile_dir(&root, "vpn");
    std::fs::create_dir_all(&vpn).unwrap();
    std::fs::write(vpn.join("network.json"), "{\"mode\": \"vpn-required\"}").unwrap();
    let out = run(&root, &["--lib-dir", "x", "--profile", "vpn"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!(
            "error: {} asks for vpn-required, and this client no longer runs that check. Remove the file to start \
             this profile without it.\n",
            vpn.join("network.json").display()
        )
    );

    let out = run(&root, &["--lib-dir", "x"], &[("INTOXICATED_SECRET_STORE", "keyring")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("error: INTOXICATED_SECRET_STORE=\"keyring\" is not supported"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_bad_apk_is_refused_with_1() {
    let root = scratch("bad-apk");
    let out = run(&root, &["--lib-dir", "x", "--apk", "/nonexistent.apk"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), "bad --apk: /nonexistent.apk: No such file or directory (os error 2)\n");
}
