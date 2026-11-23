//! Where a session is kept: a `0600` file in the profile.
//!
//! [`crate::cookie_jar`] made the client the custodian of a live `.ROBLOSECURITY`,
//! a bearer token that is whole-account access, and [`crate::account`] added
//! the username and user id beside it. Both are kept as plaintext files in the
//! profile directory, which is itself `0700`.
//!
//! The desktop Secret Service backend that used to sit in front of this went
//! with zbus. [`SETTING`], the switch that backend was chosen by, is still
//! read, and the only value it may hold is `file` (or nothing): anything else
//! stops the client with a message rather than quietly keeping a session
//! somewhere the user did not ask for. The session goes to the file, which
//! `[secrets]` names on every launch.
//!
//! A body is never printed, logged or formatted into an error here, and no
//! caller is handed one for a message. Where its size is
//! useful, a length is what is reported.

use std::io;
use std::path::Path;

/// Which of the two stores a body belongs to. The name is the file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The cookie jar, `.ROBLOSECURITY` among it (`cookie_jar.rs`).
    Cookies,
    /// The signed-in username and user id (`account.rs`).
    Identity,
}

impl Kind {
    /// The file name in the profile. This is the on-disk format: renaming
    /// one signs every existing profile out.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Cookies => "cookies",
            Kind::Identity => "identity",
        }
    }
}

/// Where this instance keeps a session. One variant now; kept as a type so
/// the call sites say which store they mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    /// A `0600` file in the profile, in plaintext.
    File,
}

/// The setting that once chose a store.
pub const SETTING: &str = "INTOXICATED_SECRET_STORE";

/// Refuse any `INTOXICATED_SECRET_STORE` other than `file` or unset.
///
/// `intoxicated-run` calls this before it reads a session, and exits on an error.
pub fn check_setting() -> Result<(), String> {
    match std::env::var(SETTING).as_deref() {
        Err(_) | Ok("") | Ok("file") => Ok(()),
        Ok(other) => Err(format!(
            "{SETTING}={other:?} is not supported: the session is only ever kept in a 0600 file \
             in the profile. Unset it or set it to \"file\"."
        )),
    }
}

/// The store, announced once per process.
pub fn active() -> Store {
    static ANNOUNCED: std::sync::Once = std::sync::Once::new();
    ANNOUNCED.call_once(|| {
        println!(
            "  [secrets] the session is kept in plaintext at {}",
            crate::profile::active().join(Kind::Cookies.name()).display()
        );
    });
    Store::File
}

/// Read a body back, or `None` for "nothing saved". A missing or unreadable
/// file is a signed-out launch, never a failed one.
pub fn load(store: Store, dir: &Path, kind: Kind) -> Option<String> {
    match store {
        Store::File => std::fs::read_to_string(dir.join(kind.name())).ok(),
    }
}

/// Write a body, replacing what was there.
pub fn save(store: Store, dir: &Path, kind: Kind, body: &str) -> io::Result<()> {
    match store {
        Store::File => write_private(dir, kind.name(), body.as_bytes()),
    }
}

/// Remove a body. Absent is success.
pub fn erase(store: Store, dir: &Path, kind: Kind) -> io::Result<()> {
    match store {
        Store::File => {
            let file = dir.join(kind.name());
            if file.exists() {
                shred(&file)?;
            }
            Ok(())
        }
    }
}

/// Where a body of `kind` is kept, for messages: the path, never the body.
pub fn where_kept(store: Store, dir: &Path, kind: Kind) -> String {
    match store {
        Store::File => dir.join(kind.name()).display().to_string(),
    }
}

/// Write `dir/name` at `0600` through a temporary and a rename, so a reader
/// sees the old body or the new one and never half a cookie. The mode is set
/// before anything is written.
fn write_private(dir: &Path, name: &str, body: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = dir.join(format!("{name}.new"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    // `mode` applies only when the open creates the file; a leftover temp
    // keeps whatever it had.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, dir.join(name))
}

/// Overwrite a file with zeros, then unlink it. A floor rather than a
/// guarantee: copy-on-write filesystems and snapshots keep the old blocks.
fn shred(path: &Path) -> io::Result<()> {
    use std::io::Write;
    {
        let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
        let zeros = [0u8; 4096];
        let mut left = file.metadata()?.len();
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            file.write_all(&zeros[..n])?;
            left -= n as u64;
        }
        file.sync_all()?;
    }
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("intoxicated-secrets-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_stored_body_is_private_and_round_trips() {
        let dir = scratch("roundtrip");
        save(Store::File, &dir, Kind::Cookies, "a=1\n").unwrap();
        save(Store::File, &dir, Kind::Cookies, "a=2\n").unwrap();
        let mode = std::fs::metadata(dir.join("cookies")).unwrap().permissions().mode();
        assert_eq!(mode & 0o177, 0, "a session must not be readable by anyone else");
        assert!(!dir.join("cookies.new").exists(), "the temporary must be renamed away");
        assert_eq!(load(Store::File, &dir, Kind::Cookies).as_deref(), Some("a=2\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leftover_temporary_does_not_lend_its_mode() {
        let dir = scratch("leftover");
        std::fs::write(dir.join("cookies.new"), b"half").unwrap();
        std::fs::set_permissions(dir.join("cookies.new"), std::fs::Permissions::from_mode(0o644)).unwrap();
        save(Store::File, &dir, Kind::Cookies, "a=3\n").unwrap();
        let mode = std::fs::metadata(dir.join("cookies")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(load(Store::File, &dir, Kind::Cookies).as_deref(), Some("a=3\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_saved_is_none_and_erasing_nothing_succeeds() {
        let dir = scratch("empty");
        assert_eq!(load(Store::File, &dir, Kind::Cookies), None);
        assert_eq!(load(Store::File, &dir, Kind::Identity), None);
        erase(Store::File, &dir, Kind::Identity).unwrap();
        // Not UTF-8 is "nothing saved" too.
        std::fs::write(dir.join("identity"), [0xff, 0xfe]).unwrap();
        assert_eq!(load(Store::File, &dir, Kind::Identity), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn erasing_removes_the_file() {
        let dir = scratch("erase");
        save(Store::File, &dir, Kind::Identity, &"x".repeat(10_000)).unwrap();
        assert!(dir.join("identity").is_file());
        erase(Store::File, &dir, Kind::Identity).unwrap();
        assert!(!dir.join("identity").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_kinds_keep_their_file_names() {
        assert_eq!(Kind::Cookies.name(), "cookies");
        assert_eq!(Kind::Identity.name(), "identity");
        let dir = Path::new("/p/default");
        assert_eq!(where_kept(Store::File, dir, Kind::Cookies), "/p/default/cookies");
        assert_eq!(where_kept(Store::File, dir, Kind::Identity), "/p/default/identity");
    }
}
