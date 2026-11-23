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

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// A held claim. Dropping it releases the profile.
pub struct Claim {
    _file: File,
    dir: PathBuf,
}

impl Claim {
    pub fn profile_dir(&self) -> &Path {
        &self.dir
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
    // SAFETY: `file` owns a valid descriptor for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(ClaimError::Unusable(format!("{}: {err}", path.display())));
        }
        return Err(ClaimError::Busy { name: name.to_string(), holder: holder_of(&path) });
    }
    Ok(Claim { _file: file, dir: dir.to_path_buf() })
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
