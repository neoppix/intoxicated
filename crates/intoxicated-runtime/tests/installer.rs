//! `install.sh`, run for real against stand-ins for the system.
//!
//! `pkg`, `pciconf`, `kldstat`, `kldload`, `sysrc`, `pw`, `sudo`, `cargo`,
//! `cmake`, `xdg-mime` and `update-desktop-database` are scripts that answer
//! from the environment and write down what they were asked. `sudo` runs the
//! root batch as the test user, so the files land under a scratch `PREFIX`;
//! `cmake` "builds" Intoxicated Settings as a one-line script. git
//! is the real one, cloning a scratch repository through
//! `INTOXICATED_GIT_URL`. Every run is in a new session with no terminal, so
//! a question can only be answered by `INTOXICATED_YES=1`.

use std::fs;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn installer() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh")
}

const NVIDIA: &str = "vgapci0@pci0:1:0:0:\tclass=0x030000 rev=0xa1 hdr=0x00 vendor=0x10de device=0x2782 subvendor=0x1569 subdevice=0xf294";
const AMD: &str = "vgapci1@pci0:10:0:0:\tclass=0x030000 rev=0xc8 hdr=0x00 vendor=0x1002 device=0x1638 subvendor=0x1043 subdevice=0x8809";

/// Name and body of each stand-in.
const FAKES: &[(&str, &str)] = &[
    (
        "pkg",
        r#"case "$1" in
  -N) [ -z "$FAKE_PKG_ABSENT" ] ;;
  info)
    case "$2" in
      -e) for m in $FAKE_MISSING; do [ "$m" = "$3" ] && exit 1; done; exit 0 ;;
      -g) [ -z "$FAKE_NO_NVIDIA_DRIVER" ] ;;
    esac ;;
  rquery) [ -n "$FAKE_QT_SIZE" ] || exit 1; printf '%s\n' "$FAKE_QT_SIZE" ;;
  *) echo "pkg $*" >> "$RECORD/root" ;;
esac"#,
    ),
    ("pciconf", r#"[ -n "$FAKE_GPU" ] && printf '%s\n' "$FAKE_GPU"; exit 0"#),
    ("kldstat", r#"[ -z "$FAKE_NO_MODESET" ]"#),
    ("kldload", r#"echo "kldload $*" >> "$RECORD/root""#),
    ("pw", r#"echo "pw $*" >> "$RECORD/root""#),
    (
        "sysrc",
        r#"if [ "$1" = -n ]; then printf '%s\n' "$FAKE_KLD_LIST"; else echo "sysrc $*" >> "$RECORD/root"; fi"#,
    ),
    (
        "sudo",
        r#"if [ "$1" = -A ]; then printf '%s\n' "$SUDO_ASKPASS" >> "$RECORD/askpass"; shift; fi
n=$(ls "$RECORD" | grep -c '^batch')
cp "$2" "$RECORD/batch$n"
exec "$@""#,
    ),
    (
        "cargo",
        r#"echo "$PWD $*" >> "$RECORD/cargo"
mkdir -p target/release
for b in intoxicated-run intoxicated-fetch; do printf '#!/bin/sh\n' > target/release/$b; chmod 755 target/release/$b; done"#,
    ),
    (
        "cmake",
        r#"echo "$PWD $*" >> "$RECORD/cmake"
case "$1" in
  -S) [ -z "$FAKE_CMAKE_FAIL" ] || exit 1; mkdir -p "$4" ;;
  --build) printf '#!/bin/sh\necho settings %s\n' "${FAKE_SETTINGS_VERSION:-1}" > "$2/intoxicated-settings"; chmod 755 "$2/intoxicated-settings" ;;
esac"#,
    ),
    (
        "xdg-mime",
        r#"if [ "$1" = query ]; then printf '%s\n' "$FAKE_HANDLER"; else echo "xdg-mime $*" >> "$RECORD/user"; fi"#,
    ),
    ("update-desktop-database", r#"echo "update-desktop-database $*" >> "$RECORD/root""#),
];

/// The checkout's launcher, cut down to the line the installer rewrites and a
/// record of how it was run.
const LAUNCHER: &str = r#"#!/bin/sh
REPO="${INTOXICATED_REPO:-$HOME/intoxicated}"
echo "$REPO $*" >> "$RECORD/launcher"
"#;

struct Setup {
    root: PathBuf,
}

impl Setup {
    fn new(name: &str) -> Setup {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("installer").join(name);
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "record", "home", "prefix", "cache"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for (name, body) in FAKES {
            let path = root.join("bin").join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let setup = Setup { root };
        setup.upstream_commit("first", LAUNCHER);
        setup
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t", "-c", "init.defaultBranch=freebsd-port"])
            .args(args)
            .current_dir(dir)
            .env("HOME", self.root.join("home"))
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Commit `launcher` to the scratch upstream, creating it the first time.
    fn upstream_commit(&self, message: &str, launcher: &str) {
        let up = self.root.join("upstream");
        if !up.exists() {
            fs::create_dir_all(up.join("packaging/freebsd")).unwrap();
            fs::create_dir_all(up.join("packaging/branding")).unwrap();
            self.git(&up, &["init", "-q"]);
            fs::write(up.join("packaging/freebsd/intoxicated.desktop"), "[Desktop Entry]\n").unwrap();
            fs::write(up.join("packaging/freebsd/intoxicated-settings.desktop"), "[Desktop Entry]\nName=Intoxicated Settings\n")
                .unwrap();
            fs::write(up.join("packaging/branding/intoxicated-logo.png"), b"png").unwrap();
        }
        fs::write(up.join("packaging/freebsd/intoxicated"), launcher).unwrap();
        self.git(&up, &["add", "-A"]);
        self.git(&up, &["commit", "-q", "-m", message]);
    }

    fn home_repo(&self) -> PathBuf {
        self.root.join("home/intoxicated")
    }

    fn run(&self, env: &[(&str, &str)]) -> Outcome {
        self.run_with(&[], env, None)
    }

    /// The choice the installer keeps between runs, if any.
    fn install_conf(&self) -> Option<String> {
        fs::read_to_string(self.root.join("home/.config/intoxicated/install.conf")).ok()
    }

    /// The Roblox build in place, so a run does not fetch it.
    fn roblox_build(&self) {
        let apk = self.root.join("cache/intoxicated/apk");
        fs::create_dir_all(apk.join("lib/x86_64")).unwrap();
        fs::write(apk.join("candidate-0.apk"), b"").unwrap();
        fs::write(apk.join("lib/x86_64/libroblox.so"), b"").unwrap();
    }

    /// `install.sh ARGS`; with `answers`, on a terminal of its own that has
    /// them typed in already, one question per line.
    fn run_with(&self, args: &[&str], env: &[(&str, &str)], answers: Option<&str>) -> Outcome {
        let path = format!(
            "{}:/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin",
            self.root.join("bin").display()
        );
        let mut command = Command::new("/bin/sh");
        command
            .arg(installer())
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("PREFIX", self.root.join("prefix"))
            .env("INTOXICATED_GIT_URL", self.root.join("upstream"))
            .env("RECORD", self.root.join("record"))
            .env("FAKE_GPU", NVIDIA)
            .env("FAKE_KLD_LIST", "nvidia-modeset nullfs");
        for (k, v) in env {
            command.env(k, v);
        }
        // One terminal run at a time (ptsname has one buffer).
        static TERMINAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _one = answers.map(|_| TERMINAL.lock().unwrap_or_else(|e| e.into_inner()));
        let tty = answers.map(|typed| {
            let fail = |what: &str| -> ! { panic!("{what}: {}", std::io::Error::last_os_error()) };
            // Both ends close on exec from the start, so no other test's
            // child, forked meanwhile, holds one open.
            let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
            // SAFETY: plain calls on a descriptor of ours; ptsname's buffer
            // is read at once, under the lock above.
            let (master, slave) = unsafe {
                let master = libc::posix_openpt(flags);
                if master < 0 {
                    fail("posix_openpt");
                }
                if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                    fail("grantpt");
                }
                let name = libc::ptsname(master);
                if name.is_null() {
                    fail("ptsname");
                }
                let slave = libc::open(name, flags);
                if slave < 0 {
                    fail("open the terminal");
                }
                (master, slave)
            };
            // SAFETY: the master is ours alone from here on.
            let mut m = unsafe { fs::File::from_raw_fd(master) };
            // Waiting in the terminal's input queue until each is read.
            m.write_all(typed.as_bytes()).unwrap();
            (m, slave)
        });
        let slave = tty.as_ref().map(|(_, s)| *s);
        // SAFETY: setsid and ioctl are async-signal-safe and touch no Rust
        // state. The terminal, when there is one, becomes the session's.
        unsafe {
            command.pre_exec(move || {
                libc::setsid();
                if let Some(s) = slave {
                    if libc::ioctl(s, libc::TIOCSCTTY, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        // The terminal is read while the installer runs: when it exits, as
        // the session's leader, what is still queued on it is dropped.
        let reader = tty.map(|(m, s)| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut m = m;
                let mut chunk = [0u8; 4096];
                while let Ok(n @ 1..) = m.read(&mut chunk) {
                    if tx.send(chunk[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            (rx, s)
        });
        let out = command.output().unwrap_or_else(|e| panic!("/bin/sh runs: {e}"));
        let terminal = match reader {
            Some((rx, s)) => {
                // SAFETY: closed once, here; the installer has exited, so the
                // reader sees the end of the terminal.
                unsafe { libc::close(s) };
                // What came, without waiting on a reader that never ends.
                let mut bytes = Vec::new();
                while let Ok(chunk) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
                    bytes.extend(chunk);
                }
                String::from_utf8_lossy(&bytes).into_owned()
            }
            None => String::new(),
        };
        let record = self.root.join("record");
        let read = |f: &str| fs::read_to_string(record.join(f)).unwrap_or_default();
        let mut batches = Vec::new();
        while let Ok(b) = fs::read_to_string(record.join(format!("batch{}", batches.len()))) {
            batches.push(b);
        }
        let outcome = Outcome {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            batches,
            root: read("root"),
            cargo: read("cargo"),
            cmake: read("cmake"),
            askpass: read("askpass"),
            user: read("user"),
            launcher: read("launcher"),
            terminal,
        };
        for f in fs::read_dir(&record).unwrap() {
            fs::remove_file(f.unwrap().path()).unwrap();
        }
        outcome
    }
}

#[derive(Debug)]
struct Outcome {
    status: i32,
    stdout: String,
    stderr: String,
    /// Each root batch as `sudo` was handed it.
    batches: Vec<String>,
    /// What the root batches did to the stand-ins.
    root: String,
    cargo: String,
    /// Each cmake call as `<cwd> <arguments>`.
    cmake: String,
    /// SUDO_ASKPASS, once per `sudo -A`.
    askpass: String,
    user: String,
    launcher: String,
    /// What the questions wrote on the terminal, when there was one: the
    /// prompts and the answers echoed.
    terminal: String,
}

const YES: (&str, &str) = ("INTOXICATED_YES", "1");

#[test]
fn a_fresh_install_clones_installs_builds_and_fetches() {
    let s = Setup::new("fresh");
    let o = s.run(&[YES]);
    assert_eq!(o.status, 0, "{o:#?}");
    let repo = s.home_repo();
    assert!(repo.join(".git").exists(), "{o:#?}");
    // Nothing was missing, so the only root batch installs the files.
    assert_eq!(o.batches.len(), 1, "{o:#?}");
    let prefix = s.root.join("prefix");
    assert_eq!(
        fs::read_to_string(prefix.join("bin/intoxicated")).unwrap(),
        LAUNCHER,
        "the default checkout needs no rewrite"
    );
    let mode = fs::metadata(prefix.join("bin/intoxicated")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert!(prefix.join("share/applications/intoxicated.desktop").is_file());
    assert!(prefix.join("share/pixmaps/intoxicated.png").is_file());
    assert!(o.root.contains("update-desktop-database"), "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", repo.display()));
    assert!(o.user.contains("xdg-mime default intoxicated.desktop x-scheme-handler/roblox-player"), "{o:#?}");
    assert_eq!(o.launcher.trim(), format!("{} --update-apk", repo.display()));
    assert!(o.stdout.contains("intoxicated --update"), "{o:#?}");
    // INTOXICATED_YES=1 takes Intoxicated Settings too, in the same batch.
    assert!(prefix.join("bin/intoxicated-settings").is_file(), "{o:#?}");
    assert!(o.askpass.is_empty(), "plain sudo without SUDO_ASKPASS: {o:#?}");
}

/// The app's question comes before the first root batch, so a missing Qt is
/// installed with the other packages; the app is built as the user with
/// cmake and installed with the launcher's files.
#[test]
fn a_fresh_install_with_intoxicated_settings() {
    let s = Setup::new("settings-fresh");
    let o = s.run(&[YES, ("FAKE_MISSING", "jq qt6-base"), ("FAKE_QT_SIZE", "65.9MiB")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.batches.len(), 2, "{o:#?}");
    let first: Vec<&str> = o.batches[0].lines().skip(1).collect();
    assert_eq!(first, ["pkg install -y jq qt6-base"], "{o:#?}");

    let repo = s.home_repo();
    let cmake: Vec<&str> = o.cmake.lines().collect();
    assert_eq!(cmake.len(), 2, "{o:#?}");
    assert_eq!(cmake[0], format!("{} -S settings -B settings/build -DCMAKE_BUILD_TYPE=Release", repo.display()));
    assert!(cmake[1].starts_with(&format!("{} --build settings/build -j ", repo.display())), "{cmake:?}");
    let jobs: u32 = cmake[1].rsplit(' ').next().unwrap().parse().expect("a job count");
    assert!(jobs >= 1);

    let installs: Vec<&str> = o.batches[1].lines().filter(|l| l.starts_with("install -m")).collect();
    assert_eq!(installs.len(), 5, "{installs:?}");
    assert!(installs[3].starts_with("install -m 755 ") && installs[3].ends_with("/prefix/bin/intoxicated-settings'"));
    assert!(installs[4].starts_with("install -m 644 "));
    assert!(installs[4].ends_with("/prefix/share/applications/intoxicated-settings.desktop'"), "{installs:?}");
    let prefix = s.root.join("prefix");
    assert_eq!(fs::read_to_string(prefix.join("bin/intoxicated-settings")).unwrap(), "#!/bin/sh\necho settings 1\n");
    let mode = fs::metadata(prefix.join("bin/intoxicated-settings")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert!(prefix.join("share/applications/intoxicated-settings.desktop").is_file());
    assert!(o.stdout.contains("intoxicated-settings"), "{o:#?}");
}

/// Once installed, the app is rebuilt on every run without a question, and
/// only a binary that changed is installed again.
#[test]
fn an_installed_intoxicated_settings_is_updated_without_asking() {
    let s = Setup::new("settings-update");
    assert_eq!(s.run(&[YES]).status, 0);
    let apk = s.root.join("cache/intoxicated/apk");
    fs::create_dir_all(apk.join("lib/x86_64")).unwrap();
    fs::write(apk.join("candidate-0.apk"), b"").unwrap();
    fs::write(apk.join("lib/x86_64/libroblox.so"), b"").unwrap();

    // No INTOXICATED_YES and no terminal: any question would fail the run.
    let o = s.run(&[("FAKE_HANDLER", "intoxicated.desktop")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(!o.stderr.contains("no terminal to ask"), "{o:#?}");
    assert!(o.stdout.contains("Intoxicated Settings is installed; it is updated too"), "{o:#?}");
    assert_eq!(o.cmake.lines().count(), 2, "rebuilt: {o:#?}");
    assert!(o.batches.is_empty(), "an identical build is not installed again: {o:#?}");

    let o = s.run(&[YES, ("FAKE_HANDLER", "intoxicated.desktop"), ("FAKE_SETTINGS_VERSION", "2")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.batches.len(), 1, "{o:#?}");
    let installs: Vec<&str> = o.batches[0].lines().filter(|l| l.starts_with("install -m")).collect();
    assert_eq!(installs.len(), 1, "{installs:?}");
    assert!(installs[0].ends_with("/prefix/bin/intoxicated-settings'"), "{installs:?}");
    assert_eq!(
        fs::read_to_string(s.root.join("prefix/bin/intoxicated-settings")).unwrap(),
        "#!/bin/sh\necho settings 2\n"
    );
}

/// Declined (here by having no terminal to answer on), nothing of the app is
/// built or installed and Qt is not added to the packages.
#[test]
fn intoxicated_settings_can_be_declined() {
    let s = Setup::new("settings-declined");
    // A first install, then the app removed, so the second run has no file
    // to install and needs no root batch at all.
    assert_eq!(s.run(&[YES]).status, 0);
    fs::remove_file(s.root.join("prefix/bin/intoxicated-settings")).unwrap();
    let apk = s.root.join("cache/intoxicated/apk");
    fs::create_dir_all(apk.join("lib/x86_64")).unwrap();
    fs::write(apk.join("candidate-0.apk"), b"").unwrap();
    fs::write(apk.join("lib/x86_64/libroblox.so"), b"").unwrap();

    let o = s.run(&[("FAKE_HANDLER", "intoxicated.desktop"), ("FAKE_MISSING", "qt6-base"), ("FAKE_QT_SIZE", "65.9MiB")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(
        o.stderr.contains(
            "no terminal to ask \"Also install Intoxicated Settings (a settings and flags app; needs Qt 6, about 65.9MiB)?\""
        ),
        "{o:#?}"
    );
    assert!(o.stdout.contains("Intoxicated Settings left out"), "{o:#?}");
    assert!(o.cmake.is_empty(), "{o:#?}");
    assert!(o.batches.is_empty(), "no qt6-base and nothing to install: {o:#?}");
    assert!(!s.root.join("prefix/bin/intoxicated-settings").exists());
    assert!(!o.stdout.contains("intoxicated-settings"), "{o:#?}");

    // Without a size from pkg the question leaves the number out.
    let o = s.run(&[("FAKE_HANDLER", "intoxicated.desktop")]);
    assert!(o.stderr.contains("(a settings and flags app; needs Qt 6)?"), "{o:#?}");
}

#[test]
fn a_failed_settings_build_leaves_the_app_out_and_installs_the_rest() {
    let s = Setup::new("settings-fails");
    let o = s.run(&[YES, ("FAKE_CMAKE_FAIL", "1")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("Intoxicated Settings did not build"), "{o:#?}");
    assert!(s.root.join("prefix/bin/intoxicated").is_file());
    assert!(!s.root.join("prefix/bin/intoxicated-settings").exists());
    assert!(!o.batches[0].contains("intoxicated-settings"), "{o:#?}");
}

/// Started from Intoxicated Settings there is no terminal; with SUDO_ASKPASS
/// set the root batch goes through `sudo -A`, which asks through that program.
#[test]
fn without_a_terminal_sudo_asks_through_sudo_askpass() {
    let s = Setup::new("askpass");
    let askpass = s.root.join("askpass-helper");
    let o = s.run(&[YES, ("SUDO_ASKPASS", askpass.to_str().unwrap())]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.batches.len(), 1, "{o:#?}");
    assert_eq!(o.askpass.trim(), askpass.to_str().unwrap(), "{o:#?}");
    assert!(s.root.join("prefix/bin/intoxicated").is_file());
}

#[test]
fn missing_packages_and_driver_setup_go_in_one_batch_before_the_clone() {
    let s = Setup::new("missing");
    let o = s.run(&[
        YES,
        ("FAKE_MISSING", "jq libXrandr xdg-utils"),
        ("FAKE_NO_NVIDIA_DRIVER", "1"),
        ("FAKE_NO_MODESET", "1"),
        ("FAKE_KLD_LIST", "nullfs"),
    ]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.batches.len(), 2, "{o:#?}");
    let first = &o.batches[0];
    assert!(first.starts_with("set -e\n"), "{first}");
    let lines: Vec<&str> = first.lines().skip(1).collect();
    assert_eq!(
        lines,
        [
            "pkg install -y libXrandr jq xdg-utils",
            "pkg install -y nvidia-driver",
            "kldload nvidia-modeset",
            "sysrc kld_list+=nvidia-modeset",
        ]
    );
    assert!(o.stdout.contains("These commands need root"), "{o:#?}");
}

#[test]
fn an_unbootstrapped_pkg_is_bootstrapped_and_everything_installed() {
    let s = Setup::new("bootstrap");
    let o = s.run(&[YES, ("FAKE_PKG_ABSENT", "1")]);
    assert_eq!(o.status, 0, "{o:#?}");
    let first: Vec<&str> = o.batches[0].lines().skip(1).collect();
    assert_eq!(first[0], "env ASSUME_ALWAYS_YES=yes pkg bootstrap");
    assert!(first[1].starts_with("pkg install -y git rust cmake "), "{first:?}");
    assert!(first[1].ends_with(" jq xdg-utils desktop-file-utils qt6-base"), "{first:?}");
}

#[test]
fn without_a_terminal_or_yes_nothing_runs_as_root() {
    let s = Setup::new("no-tty");
    let o = s.run(&[("FAKE_MISSING", "jq")]);
    assert_eq!(o.status, 1, "{o:#?}");
    assert!(o.batches.is_empty(), "{o:#?}");
    assert!(o.stderr.contains("INTOXICATED_YES=1 answers yes"), "{o:#?}");
    assert!(o.stderr.contains("nothing was run as root"), "{o:#?}");
    assert!(!s.home_repo().exists(), "cloned after refusing");
}

#[test]
fn a_second_run_updates_and_leaves_identical_files_alone() {
    let s = Setup::new("update");
    assert_eq!(s.run(&[YES]).status, 0);
    let newer = format!("{LAUNCHER}# newer\n");
    s.upstream_commit("second", &newer);
    let o = s.run(&[YES, ("FAKE_HANDLER", "intoxicated.desktop")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("Updating"), "{o:#?}");
    assert!(o.stdout.contains("second"), "{o:#?}");
    // Only the launcher changed upstream, so only it is installed again.
    assert_eq!(o.batches.len(), 1, "{o:#?}");
    let installs: Vec<&str> = o.batches[0].lines().filter(|l| l.starts_with("install -m")).collect();
    assert_eq!(installs.len(), 1, "{installs:?}");
    assert!(installs[0].ends_with("/prefix/bin/intoxicated'"), "{installs:?}");
    assert_eq!(fs::read_to_string(s.root.join("prefix/bin/intoxicated")).unwrap(), newer);
    // The handler is already set and the build is not fetched again once present.
    assert!(o.user.is_empty(), "{o:#?}");

    let apk = s.root.join("cache/intoxicated/apk");
    fs::create_dir_all(apk.join("lib/x86_64")).unwrap();
    fs::write(apk.join("candidate-0.apk"), b"").unwrap();
    fs::write(apk.join("lib/x86_64/libroblox.so"), b"").unwrap();
    let o = s.run(&[YES, ("FAKE_HANDLER", "intoxicated.desktop")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.batches.is_empty(), "nothing changed, so no password: {o:#?}");
    assert!(o.stdout.contains("already up to date"), "{o:#?}");
    assert!(o.launcher.is_empty(), "{o:#?}");
}

#[test]
fn a_clone_from_before_the_history_restart_is_moved_aside() {
    let s = Setup::new("old-history");
    // An unrelated history on the same branch name, tracking the upstream.
    let repo = s.home_repo();
    fs::create_dir_all(&repo).unwrap();
    s.git(&repo, &["init", "-q"]);
    fs::write(repo.join("old"), "old\n").unwrap();
    s.git(&repo, &["add", "-A"]);
    s.git(&repo, &["commit", "-q", "-m", "old history"]);
    let upstream = s.root.join("upstream");
    s.git(&repo, &["remote", "add", "origin", upstream.to_str().unwrap()]);
    s.git(&repo, &["fetch", "-q", "origin"]);
    s.git(&repo, &["branch", "-q", "-u", "origin/freebsd-port"]);

    let o = s.run(&[YES]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("before the history restart"), "{o:#?}");
    assert!(!repo.join("old").exists(), "{o:#?}");
    assert!(repo.join("packaging/freebsd/intoxicated").is_file());
    let moved: Vec<_> = fs::read_dir(s.root.join("home"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("intoxicated.old-"))
        .collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!(s.root.join("home").join(&moved[0]).join("old").is_file());
}

#[test]
fn another_checkout_place_is_written_into_the_installed_launcher() {
    let s = Setup::new("elsewhere");
    let repo = s.root.join("src/intox");
    let o = s.run(&[YES, ("INTOXICATED_REPO", repo.to_str().unwrap())]);
    assert_eq!(o.status, 0, "{o:#?}");
    let installed = fs::read_to_string(s.root.join("prefix/bin/intoxicated")).unwrap();
    assert!(installed.contains(&format!("REPO=\"${{INTOXICATED_REPO:-{}}}\"", repo.display())), "{installed}");
    // The checkout itself is left as cloned.
    assert_eq!(fs::read_to_string(repo.join("packaging/freebsd/intoxicated")).unwrap(), LAUNCHER);
    assert_eq!(o.launcher.trim(), format!("{} --update-apk", repo.display()));
}

#[test]
fn an_amd_gpu_without_a_vulkan_driver_gets_mesa_and_drm() {
    let s = Setup::new("amd");
    let o = s.run(&[YES, ("FAKE_GPU", AMD), ("FAKE_KLD_LIST", "")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.batches[0].contains("pkg install -y drm-kmod mesa-dri\n"), "{o:#?}");
    assert!(!o.batches[0].contains("nvidia"), "{o:#?}");
}

#[test]
fn it_refuses_to_run_as_root() {
    let s = Setup::new("root");
    let id = s.root.join("bin/id");
    fs::write(&id, "#!/bin/sh\n[ \"$1\" = -u ] && { echo 0; exit 0; }\nexec /usr/bin/id \"$@\"\n").unwrap();
    fs::set_permissions(&id, fs::Permissions::from_mode(0o755)).unwrap();
    let o = s.run(&[YES]);
    assert_eq!(o.status, 1, "{o:#?}");
    assert!(o.stderr.contains("not root"), "{o:#?}");
    assert!(o.batches.is_empty() && o.cargo.is_empty(), "{o:#?}");
}

#[test]
fn the_launchers_update_runs_the_checkouts_installer() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("installer/launcher-update");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("home")).unwrap();
    fs::write(root.join("install.sh"), "printf '%s|%s\\n' \"$INTOXICATED_REPO\" \"$*\" > \"$INTOXICATED_REPO/ran\"\n").unwrap();
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/freebsd/intoxicated");
    let out = Command::new("/bin/sh")
        .arg(launcher)
        .arg("--update")
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin")
        .env("HOME", root.join("home"))
        .env("INTOXICATED_REPO", &root)
        .output()
        .expect("/bin/sh runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_to_string(root.join("ran")).unwrap(), format!("{}|\n", root.display()));
}

const EFFECTS_QUESTION: &str =
    "Install visual effects (reflections, depth of field, bloom, sharpening; optional, off until you turn them on)? [y/N]";

/// Asked on a terminal, after Intoxicated Settings: an empty answer is no,
/// so the client is built without the runtime's default (`shaders`)
/// feature, and the choice is kept.
#[test]
fn a_fresh_install_declines_the_visual_effects_by_default() {
    let s = Setup::new("effects-declined");
    // Intoxicated Settings, the effects, the root batch, the Play button:
    // each takes its default.
    let o = s.run_with(&[], &[], Some("\n\n\n\n\n\n"));
    assert_eq!(o.status, 0, "{o:#?}");
    let settings = o.terminal.find("Also install Intoxicated Settings").expect("asked about the app first");
    let effects = o.terminal.find(EFFECTS_QUESTION).expect("asked about the effects");
    assert!(settings < effects, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release --no-default-features", s.home_repo().display()));
    assert!(o.stdout.contains("without the visual effects"), "{o:#?}");
    assert!(o.stdout.contains("intoxicated --update --effects adds them"), "{o:#?}");
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=no\nRT=no\n"));
    assert!(s.root.join("prefix/bin/intoxicated-settings").is_file(), "the app took its own default: {o:#?}");
}

#[test]
fn a_fresh_install_can_take_the_visual_effects() {
    let s = Setup::new("effects-taken");
    let o = s.run_with(&[], &[], Some("\ny\n\n\n\n\n"));
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.terminal.contains(EFFECTS_QUESTION), "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", s.home_repo().display()));
    assert!(o.stdout.contains("with the visual effects"), "{o:#?}");
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=yes\nRT=no\n"));

    // INTOXICATED_YES=1 says yes to it as to every question.
    let s = Setup::new("effects-yes");
    let o = s.run(&[YES]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", s.home_repo().display()));
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=yes\nRT=no\n"));
}

/// An update (here with no terminal, as Intoxicated Settings runs it, and
/// with INTOXICATED_YES=1 as well) builds as the first install chose and
/// asks nothing.
#[test]
fn an_update_keeps_the_remembered_choice_without_asking() {
    let s = Setup::new("effects-update");
    assert_eq!(s.run_with(&[], &[], Some("\n\n\n\n\n\n")).status, 0);
    s.roblox_build();
    let repo = s.home_repo();
    for env in [vec![("FAKE_HANDLER", "intoxicated.desktop")], vec![YES, ("FAKE_HANDLER", "intoxicated.desktop")]] {
        let o = s.run(&env);
        assert_eq!(o.status, 0, "{o:#?}");
        assert!(!o.stderr.contains("no terminal to ask"), "{o:#?}");
        assert!(o.stdout.contains("visual effects: left out, as before (--effects adds them)"), "{o:#?}");
        assert_eq!(o.cargo.trim(), format!("{} build --release --no-default-features", repo.display()));
        assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=no\nRT=no\n"));
    }

    // The same the other way round.
    let s = Setup::new("effects-update-yes");
    assert_eq!(s.run(&[YES]).status, 0);
    s.roblox_build();
    let o = s.run(&[("FAKE_HANDLER", "intoxicated.desktop")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("visual effects: built in, as before (--no-effects leaves them out)"), "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", s.home_repo().display()));
}

/// An install from before there was a choice had the effects; with no
/// choice kept, an update keeps them rather than asking.
#[test]
fn an_install_from_before_the_choice_keeps_its_effects() {
    let s = Setup::new("effects-older");
    assert_eq!(s.run(&[YES]).status, 0);
    fs::remove_file(s.root.join("home/.config/intoxicated/install.conf")).unwrap();
    s.roblox_build();
    let o = s.run(&[("FAKE_HANDLER", "intoxicated.desktop")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(!o.stderr.contains("no terminal to ask"), "{o:#?}");
    assert!(o.stdout.contains("visual effects: kept, as this install had them"), "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", s.home_repo().display()));
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=yes\nRT=no\n"));
}

/// --effects and --no-effects change the kept choice, with or without a
/// terminal, and other lines of the file are left as they were.
#[test]
fn effects_and_no_effects_change_the_choice() {
    let s = Setup::new("effects-flags");
    let o = s.run_with(&["--no-effects"], &[YES], None);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("visual effects: left out (--no-effects)"), "{o:#?}");
    let repo = s.home_repo();
    assert_eq!(o.cargo.trim(), format!("{} build --release --no-default-features", repo.display()));
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=no\nRT=no\n"));

    s.roblox_build();
    let conf = s.root.join("home/.config/intoxicated/install.conf");
    fs::write(&conf, "# kept\nEFFECTS=no\n").unwrap();
    let o = s.run_with(&["--effects"], &[("FAKE_HANDLER", "intoxicated.desktop")], None);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("visual effects: built in (--effects)"), "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", repo.display()));
    assert_eq!(s.install_conf().as_deref(), Some("# kept\nEFFECTS=yes\nRT=no\n"));

    // Kept from then on.
    let o = s.run(&[("FAKE_HANDLER", "intoxicated.desktop")]);
    assert_eq!(o.cargo.trim(), format!("{} build --release", repo.display()));

    let o = s.run_with(&["--no-effects"], &[("FAKE_HANDLER", "intoxicated.desktop")], None);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release --no-default-features", repo.display()));
    assert_eq!(s.install_conf().as_deref(), Some("# kept\nEFFECTS=no\nRT=no\n"));

    // Anything else is refused before anything runs.
    let o = s.run_with(&["--effect"], &[YES], None);
    assert_eq!(o.status, 1, "{o:#?}");
    assert!(o.stderr.contains("unknown argument: --effect"), "{o:#?}");
    assert!(o.stderr.contains("usage: install.sh [--effects | --no-effects]"), "{o:#?}");
    assert!(o.cargo.is_empty() && o.batches.is_empty(), "{o:#?}");
    let o = s.run_with(&["--help"], &[], None);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("--no-effects  build without them"), "{o:#?}");
}

const RT_QUESTION: &str = "Install the experimental ray tracing (optional, off until you turn it on)? [y/N]";

/// With the visual effects, the experimental ray tracing is asked about
/// next, no by default; a typed yes or --rt builds it in (the `rt`
/// feature), INTOXICATED_YES=1 does not, and without the effects it is never
/// built.
#[test]
fn the_ray_tracing_is_asked_after_the_effects_and_only_built_on_a_yes() {
    let s = Setup::new("rt-declined");
    let o = s.run_with(&[], &[], Some("\ny\n\n\n\n\n\n"));
    assert_eq!(o.status, 0, "{o:#?}");
    let effects = o.terminal.find(EFFECTS_QUESTION).expect("asked about the effects");
    let rt = o.terminal.find(RT_QUESTION).expect("asked about the ray tracing");
    assert!(effects < rt, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", s.home_repo().display()));
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=yes\nRT=no\n"));

    let s = Setup::new("rt-taken");
    let o = s.run_with(&[], &[], Some("\ny\ny\n\n\n\n\n"));
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release --features rt", s.home_repo().display()));
    assert!(o.stdout.contains("experimental ray tracing is built in"), "{o:#?}");
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=yes\nRT=yes\n"));
    // Kept by an update, with no terminal.
    s.roblox_build();
    let o = s.run(&[("FAKE_HANDLER", "intoxicated.desktop")]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert!(o.stdout.contains("ray tracing (experimental): built in, as before"), "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release --features rt", s.home_repo().display()));
    // --no-rt takes it out again.
    let o = s.run_with(&["--no-rt"], &[("FAKE_HANDLER", "intoxicated.desktop")], None);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", s.home_repo().display()));
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=yes\nRT=no\n"));

    // INTOXICATED_YES=1 does not add it; --rt does.
    let s = Setup::new("rt-yes");
    let o = s.run(&[YES]);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release", s.home_repo().display()));
    let s = Setup::new("rt-flag");
    let o = s.run_with(&["--rt"], &[YES], None);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release --features rt", s.home_repo().display()));
    // Never without the effects.
    let s = Setup::new("rt-no-effects");
    let o = s.run_with(&["--no-effects", "--rt"], &[YES], None);
    assert_eq!(o.status, 0, "{o:#?}");
    assert_eq!(o.cargo.trim(), format!("{} build --release --no-default-features", s.home_repo().display()));
    assert!(o.stdout.contains("it needs the visual effects"), "{o:#?}");
    assert_eq!(s.install_conf().as_deref(), Some("EFFECTS=no\nRT=no\n"));
}

#[test]
fn the_launchers_update_hands_its_arguments_to_the_installer() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("installer/launcher-update-effects");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("home")).unwrap();
    fs::write(root.join("install.sh"), "printf '%s\\n' \"$*\" > \"$INTOXICATED_REPO/ran\"\n").unwrap();
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/freebsd/intoxicated");
    for flag in ["--effects", "--no-effects", "--rt", "--no-rt"] {
        let out = Command::new("/bin/sh")
            .arg(&launcher)
            .args(["--update", flag])
            .env_clear()
            .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin")
            .env("HOME", root.join("home"))
            .env("INTOXICATED_REPO", &root)
            .output()
            .expect("/bin/sh runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(fs::read_to_string(root.join("ran")).unwrap(), format!("{flag}\n"));
    }
}
