//! The launcher's settings.conf, profile and information switches, run for
//! real.
//!
//! As in `launcher_links.rs`, `packaging/freebsd/intoxicated` is run with
//! `/bin/sh` against a scratch checkout whose `target/release/intoxicated-run`
//! is a script. It hands `--check-join-url`, `--print-default-flags`,
//! `--print-ssr-looks`, `--print-shader-looks` and `--help` to the real
//! binary and otherwise writes down its arguments, the
//! `INTOXICATED_*` variables it was started with and the launcher's own
//! arguments as `ps` shows them, then exits 0. HOME, XDG_CONFIG_HOME,
//! XDG_DATA_HOME and XDG_CACHE_HOME all point into the scratch directory.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn launcher() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/freebsd/intoxicated")
}

const FAKE: &str = r#"#!/bin/sh
case "$1" in
  --check-join-url|--print-default-flags|--print-ssr-looks|--print-shader-looks|--help) exec "$REAL_INTOXICATED_RUN" "$@" ;;
esac
for a in "$@"; do printf '%s\n' "$a"; done > "$RECORD/args"
env | grep '^INTOXICATED_' > "$RECORD/env"
ps -ww -o args= -p "$PPID" > "$RECORD/launcher"
exit 0
"#;

struct Scratch {
    root: PathBuf,
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
    /// `None` when the client was never started.
    args: Option<Vec<String>>,
    env: BTreeMap<String, String>,
    launcher_args: String,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("launcher_settings").join(name);
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("repo/target/release/intoxicated-run");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, FAKE).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let apk = root.join("cache/intoxicated/apk");
        fs::create_dir_all(apk.join("lib/x86_64")).unwrap();
        fs::write(apk.join("candidate-0.apk"), b"").unwrap();
        fs::write(apk.join("lib/x86_64/libroblox.so"), b"").unwrap();
        for dir in ["record", "home", "config/intoxicated", "data"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        Scratch { root }
    }

    fn settings(&self, text: &str) {
        fs::write(self.root.join("config/intoxicated/settings.conf"), text).unwrap();
    }

    fn flags_of(&self, profile: &str) -> PathBuf {
        self.root.join("data/intoxicated/profiles").join(profile).join("flags.json")
    }

    /// Run the launcher with `args` and, on top of the scratch environment,
    /// `env`. The fps and video memory flags are left off unless `env` turns
    /// them back on.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let record = self.root.join("record");
        for f in fs::read_dir(&record).unwrap() {
            fs::remove_file(f.unwrap().path()).unwrap();
        }
        let mut command = Command::new("/bin/sh");
        command
            .arg(launcher())
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("INTOXICATED_REPO", self.root.join("repo"))
            .env("INTOXICATED_FPS", "off")
            .env("INTOXICATED_VIDEO_MEMORY", "off")
            .env("REAL_INTOXICATED_RUN", env!("CARGO_BIN_EXE_intoxicated-run"))
            .env("RECORD", &record);
        for (k, v) in env {
            command.env(k, v);
        }
        let out = command.output().expect("/bin/sh runs");
        let read = |f: &str| fs::read_to_string(record.join(f)).ok();
        let env = read("env")
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Run {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            args: read("args").map(|a| a.lines().map(str::to_string).collect()),
            env,
            launcher_args: read("launcher").unwrap_or_default(),
        }
    }
}

#[test]
fn settings_conf_is_applied_and_the_environment_wins() {
    let s = Scratch::new("applied");
    s.settings(
        "# Intoxicated Settings\n\
         \n\
         INTOXICATED_RESOLUTION=1920x1080\n\
         INTOXICATED_MONITOR=1\n\
         INTOXICATED_DPI_SCALE=1.5\n\
         INTOXICATED_DPI_SCALE=2\n\
         INTOXICATED_FULLSCREEN=1\n",
    );
    let r = s.run(&[], &[("INTOXICATED_MONITOR", "0"), ("INTOXICATED_FULLSCREEN", "")]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(r.args.is_some(), "the client was started");
    assert_eq!(r.env["INTOXICATED_RESOLUTION"], "1920x1080");
    assert_eq!(r.env["INTOXICATED_MONITOR"], "0", "the environment wins");
    assert_eq!(r.env["INTOXICATED_DPI_SCALE"], "2", "the last line for a key wins");
    assert_eq!(r.env["INTOXICATED_FULLSCREEN"], "", "set but empty in the environment still wins");

    // Without the file the launcher runs as before.
    fs::remove_file(s.root.join("config/intoxicated/settings.conf")).unwrap();
    let r = s.run(&[], &[]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(!r.env.contains_key("INTOXICATED_RESOLUTION"));
}

#[test]
fn nothing_in_settings_conf_runs_and_other_lines_are_ignored() {
    let s = Scratch::new("literal");
    s.settings(
        "INTOXICATED_X=$(touch pwned)\n\
         INTOXICATED_Y=`touch pwned2`; touch pwned3\n\
         INTOXICATED_Z=a \"b\"  'c' $HOME \\n \n\
         INTOXICATED_EQ=a=b=c\n\
         PATH=/nonexistent\n\
         HOME=/nonexistent\n\
         FOO=bar\n\
         intoxicated_lower=1\n\
         \x20INTOXICATED_INDENTED=1\n\
         export INTOXICATED_EXPORTED=1\n\
         INTOXICATED_BAD-NAME=1\n\
         INTOXICATED_lower=1\n\
         INTOXICATED_LINK_REWRITTEN=1\n\
         INTOXICATED_LAST=no newline at the end",
    );
    // A link the check rewrites: the launcher must still run itself again
    // with the rewritten one, which INTOXICATED_LINK_REWRITTEN would stop.
    let r = s.run(&["https://www.roblox.com/games/1818/Some-Name"], &[]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert_eq!(r.env["INTOXICATED_X"], "$(touch pwned)");
    assert_eq!(r.env["INTOXICATED_Y"], "`touch pwned2`; touch pwned3");
    assert_eq!(r.env["INTOXICATED_Z"], "a \"b\"  'c' $HOME \\n ");
    assert_eq!(r.env["INTOXICATED_EQ"], "a=b=c");
    assert_eq!(r.env["INTOXICATED_LAST"], "no newline at the end");
    for ignored in ["INTOXICATED_INDENTED", "INTOXICATED_EXPORTED", "INTOXICATED_BAD-NAME", "INTOXICATED_lower"] {
        assert!(!r.env.contains_key(ignored), "{ignored}: {:?}", r.env);
    }
    for name in ["pwned", "pwned2", "pwned3"] {
        assert!(!s.root.join(name).exists(), "{name} was created: settings.conf ran");
        assert!(!s.root.join("home").join(name).exists(), "{name} was created in HOME");
    }
    assert!(!r.launcher_args.contains("Some-Name"), "not run again: {}", r.launcher_args);
    assert!(r.launcher_args.contains("placeId=1818"), "{}", r.launcher_args);
}

#[test]
fn a_profile_other_than_default_is_passed_on_and_gets_the_flags() {
    let s = Scratch::new("profile");
    s.settings("INTOXICATED_PROFILE=alt\n");
    let keep_flags = [("INTOXICATED_FPS", "120"), ("INTOXICATED_VIDEO_MEMORY", "2147483647")];
    let r = s.run(&[], &keep_flags);
    assert_eq!(r.status, 0, "{}", r.stderr);
    let args = r.args.unwrap();
    assert_eq!(&args[args.len() - 2..], ["--profile", "alt"], "{args:?}");
    let flags: serde_json::Value = serde_json::from_str(&fs::read_to_string(s.flags_of("alt")).unwrap()).unwrap();
    assert_eq!(flags["DFIntTaskSchedulerTargetFps"], "120");
    assert_eq!(flags["FIntRenderForceVideoMemorySize"], "2147483647");
    assert!(!s.flags_of("default").exists(), "the default profile was not touched");

    // The environment picks another one over settings.conf.
    let r = s.run(&["5938036553"], &[("INTOXICATED_PROFILE", "second_1")]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    let args = r.args.unwrap();
    let at = args.iter().position(|a| a == "--profile").expect("--profile");
    assert_eq!(args[at + 1], "second_1");
    assert!(args.contains(&"--join-url".to_string()));

    // default is not named, so the client's own default applies.
    let r = s.run(&[], &[("INTOXICATED_PROFILE", "default"), keep_flags[0]]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(!r.args.unwrap().contains(&"--profile".to_string()));
    assert!(s.flags_of("default").exists());
}

#[test]
fn an_invalid_profile_name_is_refused_with_2() {
    let s = Scratch::new("bad-profile");
    let long = "x".repeat(33);
    for name in ["../evil", "a b", "dot.name", "sl/ash", "tab\there", long.as_str()] {
        let r = s.run(&[], &[("INTOXICATED_PROFILE", name)]);
        assert_eq!(r.status, 2, "{name:?}: {}", r.stderr);
        assert!(r.args.is_none(), "{name:?} started the client");
        assert!(r.stderr.contains("is not a profile name"), "{name:?}: {}", r.stderr);
    }
    s.settings("INTOXICATED_PROFILE=../../etc\n");
    let r = s.run(&["5938036553"], &[]);
    assert_eq!(r.status, 2, "{}", r.stderr);
    assert!(r.args.is_none());
    assert!(!s.root.join("data/intoxicated").exists(), "nothing was created for it");

    let longest = "Ab_-9".repeat(6) + "zz";
    assert_eq!(longest.len(), 32);
    let r = s.run(&[], &[("INTOXICATED_PROFILE", &longest)]);
    assert_eq!(r.status, 0, "{}", r.stderr);
}

#[test]
fn default_flags_ssr_looks_and_version_come_from_the_client() {
    let s = Scratch::new("info");
    let r = s.run(&["--default-flags"], &[]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(r.args.is_none(), "the client was not started");
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("JSON");
    let object = doc.as_object().expect("an object");
    let built_in = intoxicated_runtime::flag_layers::BUILT_IN;
    assert_eq!(object.len(), built_in.len());
    for (key, value) in built_in {
        assert_eq!(object[key].as_str(), Some(value));
    }

    let r = s.run(&["--ssr-looks"], &[]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(r.args.is_none(), "the client was not started");
    assert_eq!(r.stdout, intoxicated_runtime::android::ssr::looks_json() + "\n");
    // settings.conf's reflection lines do not change the table.
    s.settings("INTOXICATED_SSR=1\nINTOXICATED_SSR_LOOK=glossy\nINTOXICATED_SSR_INTENSITY=1\n");
    let r = s.run(&["--ssr-looks"], &[]);
    assert_eq!(r.stdout, intoxicated_runtime::android::ssr::looks_json() + "\n");
    s.settings("");

    // Every effect drawn before the HUD, the reflections' table among them.
    let r = s.run(&["--shader-looks"], &[]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(r.args.is_none(), "the client was not started");
    assert_eq!(r.stdout, intoxicated_runtime::android::ssr::shader_looks_json() + "\n");
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).expect("JSON");
    if cfg!(feature = "shaders") {
        assert_eq!(doc["available"], true);
        let effects = doc["effects"].as_array().expect("a list of effects");
        let switches: Vec<&str> = effects.iter().map(|e| e["switch"].as_str().unwrap()).collect();
        assert_eq!(switches, ["INTOXICATED_SSR", "INTOXICATED_DOF", "INTOXICATED_BLOOM", "INTOXICATED_LENS", "INTOXICATED_SHARPEN"]);
    } else {
        // Built without the visual effects: the launcher hands on the
        // client's own word for it.
        assert_eq!(doc["available"], false);
    }
    s.settings("INTOXICATED_DOF=1\nINTOXICATED_DOF_STRENGTH=20\nINTOXICATED_BLOOM_LOOK=neon\n");
    let r = s.run(&["--shader-looks"], &[]);
    assert_eq!(r.stdout, intoxicated_runtime::android::ssr::shader_looks_json() + "\n");
    s.settings("");

    let r = s.run(&["--version"], &[]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(r.args.is_none(), "the client was not started");
    assert_eq!(r.stdout.lines().count(), 1, "{}", r.stdout);
    assert!(r.stdout.starts_with("Intoxicated "), "{}", r.stdout);
    assert!(r.stdout.contains(": native FreeBSD Roblox client."), "{}", r.stdout);

    // Both work whatever profile is set, valid or not.
    let r = s.run(&["--version"], &[("INTOXICATED_PROFILE", "../x")]);
    assert_eq!(r.status, 0, "{}", r.stderr);
}
