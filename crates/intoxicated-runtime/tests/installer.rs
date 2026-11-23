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
        let path = format!(
            "{}:/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin",
            self.root.join("bin").display()
        );
        let mut command = Command::new("/bin/sh");
        command
            .arg(installer())
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
        // SAFETY: setsid is async-signal-safe and touches no Rust state.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let out = command.output().expect("/bin/sh runs");
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
