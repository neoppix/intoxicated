//! One client per profile.
//!
//! Two clients writing one profile's Roblox storage and cookie jar corrupt
//! both, so `intoxicated-run` takes an exclusive `flock` on `<profile>/.lock`
//! before it touches anything else and holds it until the process exits.
//! The lock belongs to the open file, so the kernel drops it however the
//! process ends; a lock file left behind by a crash is simply unlocked and the
//! next claim takes it.
//!
//! When the profile is busy, the refusal names the holder. That is found by
//! walking FreeBSD's `kern.proc` sysctls for a process with the lock file
//! open, which needs no procfs mount.
//!
//! A claim that finds the lock taken tries again for [`CLAIM_PATIENCE`]
//! before it refuses. Intoxicated Settings tells whether a profile is in use
//! by taking the lock and dropping it at once; a client starting in that
//! instant would otherwise be refused for a profile nobody is running.
//!
//! While it holds the lock the client publishes its process id in
//! `<profile>/client.pid` ([`Claim::publish_pid`]), so Intoxicated Settings
//! can stop the one client it means without looking for it by name. The file
//! is removed on every exit the process sees ([`withdraw_pid_file`], from
//! `Drop` and the `_exit` paths); one left by a crash is overwritten by the
//! next client, and a reader trusts it only while the process it names holds
//! the lock file open.

use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long a claim waits for a lock held by someone else to come free.
pub const CLAIM_PATIENCE: Duration = Duration::from_millis(300);

/// The file, in the profile, that names the client holding it.
pub const PID_FILE: &str = "client.pid";

/// The published pid file's path, kept as a C string so the `_exit` paths
/// (a signal handler among them) can remove it without allocating.
static PUBLISHED: OnceLock<CString> = OnceLock::new();

/// A held claim. Dropping it releases the profile.
pub struct Claim {
    _file: File,
    dir: PathBuf,
}

impl Claim {
    pub fn profile_dir(&self) -> &Path {
        &self.dir
    }

    /// Write this process's id to `<profile>/client.pid`, owner-only,
    /// through a temporary file and a rename so a reader never sees half of
    /// it. Done once per process; a second call changes nothing.
    pub fn publish_pid(&self) -> std::io::Result<PathBuf> {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.dir.join(PID_FILE);
        if PUBLISHED.get().is_some() {
            return Ok(path);
        }
        let tmp = self.dir.join(format!(".{PID_FILE}.{}.tmp", std::process::id()));
        let written = (|| {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            writeln!(f, "{}", std::process::id())?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
            let _ = PUBLISHED.set(c);
        }
        Ok(path)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // Before the file closes, so the pid file never outlives the lock.
        withdraw_pid_file();
    }
}

/// Remove the pid file [`Claim::publish_pid`] wrote, if it did. Only the
/// holder of the lock ever writes it, so whatever is there is this
/// process's. Async-signal-safe: one `unlink(2)` of a path made earlier.
pub fn withdraw_pid_file() {
    if let Some(path) = PUBLISHED.get() {
        // SAFETY: a NUL-terminated path that lives for the whole process.
        unsafe { libc::unlink(path.as_ptr()) };
    }
}

/// Who holds a busy profile, when that could be seen.
#[derive(Debug, Clone)]
pub struct Holder {
    pub pid: u32,
    pub command: String,
}

#[derive(Debug)]
pub enum ClaimError {
    /// Another process holds the lock.
    Busy { name: String, holder: Option<Holder> },
    /// The profile could not be created or opened at all.
    Unusable(String),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Busy { name, holder } => {
                write!(f, "profile {name:?} is already open in another client")?;
                match holder {
                    Some(h) if h.command.is_empty() => write!(f, " (process {})", h.pid)?,
                    Some(h) => write!(f, " (process {}: {})", h.pid, h.command)?,
                    None => {}
                }
                write!(
                    f,
                    ".\nTwo clients on one profile corrupt its storage. Close that one, \
                     or run this one with --profile <other-name> or its own XDG_DATA_HOME."
                )
            }
            ClaimError::Unusable(why) => f.write_str(why),
        }
    }
}

/// Claim the profile directory `dir`, named `name` for messages.
pub fn claim(name: &str, dir: &Path) -> Result<Claim, ClaimError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| ClaimError::Unusable(format!("{}: {e}", dir.display())))?;
    let path = dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| ClaimError::Unusable(format!("{}: {e}", path.display())))?;
    let deadline = Instant::now() + CLAIM_PATIENCE;
    loop {
        // SAFETY: `file` owns a valid descriptor for the duration of the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Claim { _file: file, dir: dir.to_path_buf() });
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(ClaimError::Unusable(format!("{}: {err}", path.display())));
        }
        if Instant::now() >= deadline {
            return Err(ClaimError::Busy { name: name.to_string(), holder: holder_of(&path) });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The first other process with `path` open, by device and inode.
fn holder_of(path: &Path) -> Option<Holder> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    let me = std::process::id();
    kern::pids()
        .into_iter()
        .filter(|&pid| pid != me)
        .find(|&pid| kern::has_open(pid, meta.dev(), meta.ino()))
        .map(|pid| Holder { pid, command: kern::args(pid).unwrap_or_default() })
}

/// The few `kern.proc` reads [`holder_of`] needs.
mod kern {
    use std::ffi::c_int;

    /// One sysctl read, retried if the answer grew between sizing and reading.
    fn sysctl(mib: &[c_int]) -> Option<Vec<u8>> {
        for _ in 0..4 {
            let mut len = 0usize;
            // SAFETY: a size query; only `len` is written.
            let r = unsafe {
                libc::sysctl(mib.as_ptr(), mib.len() as u32, std::ptr::null_mut(), &mut len, std::ptr::null(), 0)
            };
            if r != 0 {
                return None;
            }
            let mut buf = vec![0u8; len + len / 2 + 4096];
            let mut got = buf.len();
            // SAFETY: `buf` has `got` writable bytes; the kernel writes at most that.
            let r = unsafe {
                libc::sysctl(mib.as_ptr(), mib.len() as u32, buf.as_mut_ptr().cast(), &mut got, std::ptr::null(), 0)
            };
            if r == 0 {
                buf.truncate(got);
                return Some(buf);
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
                return None;
            }
        }
        None
    }

    /// Every live, non-zombie process id.
    pub fn pids() -> Vec<u32> {
        let Some(buf) = sysctl(&[libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PROC, 0]) else {
            return Vec::new();
        };
        let size = std::mem::size_of::<libc::kinfo_proc>();
        buf.chunks_exact(size)
            // SAFETY: the kernel returns an array of `kinfo_proc`; read
            // unaligned because a Vec<u8> carries no alignment promise.
            .map(|c| unsafe { std::ptr::read_unaligned(c.as_ptr().cast::<libc::kinfo_proc>()) })
            .filter(|kp| kp.ki_structsize as usize == size && kp.ki_stat != libc::SZOMB)
            .map(|kp| kp.ki_pid as u32)
            .collect()
    }

    /// The process's argument vector, space separated.
    pub fn args(pid: u32) -> Option<String> {
        let raw = sysctl(&[libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ARGS, pid as c_int])?;
        Some(String::from_utf8_lossy(&raw).replace('\0', " ").trim().to_string())
    }

    // `struct kinfo_file` (sys/user.h): `kf_structsize` at 0, `kf_type` at 4,
    // and the vnode's `kf_file_fsid`/`kf_file_fileid` at 256 and 272 into the
    // union that starts at byte 32. `libc` leaves that union opaque.
    const TYPE_AT: usize = 4;
    const FSID_AT: usize = 32 + 256;
    const FILEID_AT: usize = 32 + 272;

    /// Whether `pid` has the vnode `(dev, ino)` open. False when its
    /// descriptor table cannot be read (another user's process, or gone).
    pub fn has_open(pid: u32, dev: u64, ino: u64) -> bool {
        let Some(buf) = sysctl(&[libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_FILEDESC, pid as c_int])
        else {
            return false;
        };
        let i32_at = |b: &[u8], at: usize| i32::from_ne_bytes(b[at..at + 4].try_into().unwrap());
        let u64_at = |b: &[u8], at: usize| u64::from_ne_bytes(b[at..at + 8].try_into().unwrap());
        let mut rest = &buf[..];
        while rest.len() >= 8 {
            let size = i32_at(rest, 0) as usize;
            if size == 0 || size > rest.len() {
                break;
            }
            let rec = &rest[..size];
            if size >= FILEID_AT + 8
                && i32_at(rec, TYPE_AT) == libc::KF_TYPE_VNODE
                && u64_at(rec, FSID_AT) == dev
                && u64_at(rec, FILEID_AT) == ino
            {
                return true;
            }
            rest = &rest[size..];
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("intox-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn a_second_claim_is_refused_until_the_first_is_dropped() {
        let dir = scratch("busy");
        let first = claim("busy", &dir).expect("first claim");
        // A second open file description conflicts with the first even inside
        // one process, which is what lets this be tested without a child.
        match claim("busy", &dir) {
            Err(ClaimError::Busy { name, holder }) => {
                assert_eq!(name, "busy");
                // The holder is this very process, which `holder_of` skips by
                // design, so nothing else can be named here.
                assert!(holder.is_none(), "{holder:?}");
            }
            Err(e) => panic!("expected Busy, got {e}"),
            Ok(_) => panic!("a second claim must be refused"),
        }
        drop(first);
        assert!(claim("busy", &dir).is_ok(), "dropping the claim must release it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_taken_only_for_a_moment_does_not_refuse_the_claim() {
        // What Intoxicated Settings does to see whether a profile is in use:
        // take the lock and drop it again. A client starting just then waits.
        let dir = scratch("probe");
        std::fs::create_dir_all(&dir).unwrap();
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".lock"))
            .unwrap();
        // SAFETY: a valid descriptor.
        assert_eq!(unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            drop(probe);
        });
        let started = Instant::now();
        let claimed = claim("probe", &dir);
        release.join().unwrap();
        assert!(claimed.is_ok(), "a lock dropped after 80 ms must not refuse the claim");
        assert!(started.elapsed() >= Duration::from_millis(60), "{:?}", started.elapsed());
        drop(claimed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pid_file_names_this_process_and_goes_with_the_claim() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("pid");
        let held = claim("pid", &dir).expect("claim");
        let path = held.publish_pid().expect("publish");
        assert_eq!(path, dir.join(PID_FILE));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{}\n", std::process::id()));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "only .lock and client.pid: {names:?}");
        // A second publish changes nothing, and the file is still there.
        held.publish_pid().unwrap();
        assert!(path.exists());
        drop(held);
        assert!(!path.exists(), "dropping the claim removes the pid file");
        assert!(dir.join(".lock").exists(), "the lock file itself stays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_lock_file_is_reclaimed() {
        // A crashed client leaves `.lock` behind, unlocked. That must not
        // keep the profile shut.
        let dir = scratch("stale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".lock"), b"left by a crash").unwrap();
        let held = claim("stale", &dir).expect("a stale lock file must be reclaimed");
        assert_eq!(held.profile_dir(), dir.as_path());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_process_holding_the_lock_is_named() {
        let dir = scratch("child");
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join(".lock");
        // lockf(1) is in FreeBSD's base system: it holds an flock on the file
        // for as long as the command it runs.
        let Ok(mut child) = std::process::Command::new("lockf")
            .arg("-k")
            .arg(&lock)
            .args(["sleep", "30"])
            .spawn()
        else {
            eprintln!("skipping: no lockf(1)");
            return;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let outcome = loop {
            match claim("child", &dir) {
                Err(ClaimError::Busy { holder, .. }) => break Some(holder),
                Ok(c) if std::time::Instant::now() < deadline => {
                    drop(c);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                _ => break None,
            }
        };
        let _ = child.kill();
        let _ = child.wait();
        let holder = outcome.expect("lockf never took the lock").expect("the holder must be named");
        // lockf(1) or the `sleep` it forked, which inherits the descriptor.
        assert!(
            holder.command.contains("lockf") || holder.command.contains("sleep"),
            "{holder:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
