//! Which profile this instance runs against.
//!
//! A profile is a directory under [`root`], named on the command line with
//! `--profile <name>` and `default` otherwise. It is the one
//! directory this module resolves: one account's Roblox storage, session and
//! settings, kept apart from every other profile's.
//!
//! **The lock that makes "one instance per profile" true is in
//! [`crate::profile_lock`]**, claimed by `main` in `bin/intoxicated_run/` before anything
//! touches the directory. This module decides *which* directory that is.
//!
//! The decision is made once per process and latched: [`set_active`] while
//! the arguments are parsed, or the default the first time [`active`] is read
//! without one. A profile cannot change under a running client, because the
//! lock, the session and Roblox's own storage would then belong to two
//! directories at once.
//!
//! Names are refused rather than cleaned up ([`is_valid_name`]): a name is one
//! path component, and a name that is not usable as one is an error the user
//! sees, never a different directory than the one asked for.
//!
//! A profile holds configuration as well as storage: the user's `flags.json`
//! and the session (`cookies` and `identity`, both `0600`, see `secrets.rs`).
//! See [ADR-013](../../../docs/adr/ADR-013-per-profile-configuration.md).
//! Only the profile *directory* is decided here; what goes in it is resolved by
//! `flags.rs` and the others, which take the directory rather than looking it
//! up for themselves.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The profile used when no `--profile` is given.
pub const DEFAULT_NAME: &str = "default";

/// Where profiles live: `$XDG_DATA_HOME/intoxicated/profiles`, else
/// `~/.local/share/intoxicated/profiles`. `INTOXICATED_PROFILE_ROOT` replaces
/// the whole path, which is what the unit tests use.
pub fn root() -> PathBuf {
    if let Some(root) = std::env::var_os("INTOXICATED_PROFILE_ROOT") {
        return PathBuf::from(root);
    }
    data_home().join("intoxicated/profiles")
}

/// `$XDG_DATA_HOME`, else `~/.local/share`.
fn data_home() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => PathBuf::from(d),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".local/share"),
            None => std::env::temp_dir(),
        },
    }
}

/// The latched profile directory.
static ACTIVE: OnceLock<PathBuf> = OnceLock::new();

/// Fix this instance's profile to `dir`, creating it owner-only.
///
/// The first call decides; calling again with the same path is a no-op, and
/// a different path is refused, leaving the first in place. Paths are compared
/// as given, not canonicalised. The lock
/// [`crate::profile_lock::claim`] takes in `main` is what makes
/// a second *process* on the same directory impossible; this is what makes a
/// second directory in the same process impossible.
///
/// **There is deliberately no way to name the session or the flags file
/// separately: the profile is the one decision, and everything else is read
/// from
/// underneath it.** Flag overrides and the session are resolved from this one
/// directory, so a second argument naming either would be a second source of truth for something already decided.
pub fn set_active(dir: PathBuf) -> Result<(), String> {
    if let Some(first) = ACTIVE.get() {
        return same_or_refused(first, &dir);
    }
    // Created and tightened here because this runs first: `parse()` resolves
    // `--profile` before `main` claims anything, and `create_dir_all` alone
    // would leave the umask's `0755` on a directory that holds a session token
    // (see `cookies.rs` and ADR-012).
    let _ = std::fs::create_dir_all(&dir);
    owner_only(&dir);
    match ACTIVE.set(dir) {
        Ok(()) => Ok(()),
        // Another thread latched first, between the check and the set.
        Err(dir) => same_or_refused(ACTIVE.get().expect("set failed because it is set"), &dir),
    }
}

fn same_or_refused(first: &Path, dir: &Path) -> Result<(), String> {
    if first == dir {
        Ok(())
    } else {
        Err(format!(
            "this instance already runs {}; a profile cannot be changed while the client is up",
            first.display()
        ))
    }
}

/// This instance's profile directory: the one [`set_active`] latched, or
/// `root()/default` while nothing has. Reading neither latches nor creates
/// anything.
pub fn active() -> PathBuf {
    match ACTIVE.get() {
        Some(dir) => dir.clone(),
        None => root().join(DEFAULT_NAME),
    }
}

/// Whether `name` can name a profile. It becomes one path component under
/// [`root`], so a name that could leave it, or that a shell or a file manager
/// would mangle, is refused outright: a cleaned-up name would be a directory
/// the user did not ask for, and a session in it they would not know how to
/// get.
///
/// ASCII letters, digits, `-` and `_`, at most 64 of them.
pub fn is_valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The directory for profile `name`, refusing a name that is not valid.
pub fn dir(name: &str) -> Result<PathBuf, String> {
    if is_valid_name(name) {
        Ok(root().join(name))
    } else {
        Err(format!("{name:?} is not a usable profile name; use letters, digits, - and _"))
    }
}

/// Take group and other access away from a profile directory.
///
/// The engine keeps its cookies in memory and expects the Java side of the app
/// to persist them, so the client reads the jar out of the engine and writes it
/// here: it *is* the custodian of a session token. See `cookies.rs` and ADR-012.
///
/// This mode still matters: the profile holds `flags.json`, Roblox's own
/// `appData` and the `0600` session store.
///
/// A mode with any group or other bit becomes exactly `0700`; anything already
/// owner-only is left as it is (`0500` stays `0500`). Errors are ignored: a
/// filesystem without Unix modes must not stop the launch.
fn owner_only(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(dir) else {
        return;
    };
    if meta.permissions().mode() & 0o077 != 0 {
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// `INTOXICATED_PROFILE_ROOT` is process-wide and cargo runs tests in parallel
    /// threads, so every test that sets it holds this.
    static ROOT_VARIABLE: Mutex<()> = Mutex::new(());

    /// A fresh scratch root, set as `INTOXICATED_PROFILE_ROOT` while the
    /// returned guard is held.
    fn scratch_root(tag: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let guard = ROOT_VARIABLE.lock().unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!("intoxicated-profile-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        std::env::set_var("INTOXICATED_PROFILE_ROOT", &p);
        (guard, p)
    }

    #[test]
    fn names_are_refused_never_sanitised() {
        for bad in ["../../etc", "/absolute", "has/slash", "", &"a".repeat(65), "sp ace", "dot.ted", "\u{e9}"] {
            assert!(!is_valid_name(bad), "{bad:?}");
        }
        for good in ["default", "alt_account-2", &"b".repeat(64)] {
            assert!(is_valid_name(good), "{good:?}");
        }
        assert_eq!(
            dir("../x").unwrap_err(),
            "\"../x\" is not a usable profile name; use letters, digits, - and _"
        );
    }

    // The lock's own tests live with the lock, in `profile_lock.rs`.

    /// The latch is once per process, so this is one test, in order.
    #[test]
    fn the_first_profile_is_latched_owner_only_and_cannot_change() {
        let (_guard, root) = scratch_root(&format!("latch-{}", std::process::id()));
        assert_eq!(active(), root.join("default"));
        assert!(!root.join("default").exists(), "reading the default creates nothing");

        let alt = dir("alt_account").unwrap();
        assert_eq!(alt, root.join("alt_account"));
        set_active(alt.clone()).unwrap();
        assert_eq!(active(), alt);
        let mode = std::fs::metadata(&alt).unwrap().permissions().mode();
        // Under the usual umask `create_dir_all` alone would have left this
        // `0755`, so another account could read the flag overrides, Roblox's
        // own appData and the session store. This is the door a hand-run
        // client would otherwise leave open.
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");

        assert_eq!(set_active(alt.clone()), Ok(()));
        let main = dir("main").unwrap();
        let err = set_active(main.clone()).unwrap_err();
        assert_eq!(
            err,
            format!(
                "this instance already runs {}; a profile cannot be changed while the client is up",
                alt.display()
            )
        );
        assert_eq!(active(), alt);
        assert!(!main.exists(), "a refused profile is not created");
        std::env::remove_var("INTOXICATED_PROFILE_ROOT");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn owner_only_tightens_open_modes_and_leaves_tight_ones() {
        let base = std::env::temp_dir().join(format!("intoxicated-profile-modes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for (name, before, after) in [("open", 0o755, 0o700), ("group", 0o710, 0o700), ("tight", 0o500, 0o500)] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(before)).unwrap();
            owner_only(&d);
            assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, after, "{name}");
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        owner_only(&base.join("absent"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
