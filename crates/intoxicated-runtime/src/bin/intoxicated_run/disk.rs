//! The free-space warning around a launch.
//!
//! The engine does not report a full disk: a failed write shows up somewhere
//! unrelated to it. So the files directory's filesystem is looked at twice,
//! before the surface handover and after the pump returns, and a line is
//! printed only when space is short. Free space comes from `statfs(2)`
//! (available blocks times the fragment size), read natively; the old query
//! ran `df --output`, which FreeBSD's `df` does not have, so it never fired.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const MIB: u64 = 1024 * 1024;
/// Below this the launch will probably fail.
pub const CRITICAL: u64 = 512 * MIB;
/// Below this it is worth a line.
pub const LOW: u64 = 2048 * MIB;

/// When the check runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    BeforeLaunch,
    AfterExit,
}

/// Bytes available to this user on the filesystem holding `path`, walking up
/// to the nearest ancestor that exists. `None` when it cannot be told.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut at = path;
    while !at.exists() {
        at = at.parent()?;
    }
    let c_path = CString::new(at.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero `statfs` is a valid value of the plain C struct.
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `fs` is a live, writable struct.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut fs) } != 0 {
        return None;
    }
    // `f_bavail` is signed: a filesystem past its reserve has none left.
    let blocks = i128::from(fs.f_bavail).max(0);
    let bytes = blocks.checked_mul(i128::from(fs.f_bsize))?;
    u64::try_from(bytes).ok()
}

/// The lines to print for `free` bytes on `path` at `moment`, or nothing.
/// Unknown space is never reported as short.
pub fn warning(free: Option<u64>, path: &Path, moment: Moment) -> Vec<String> {
    let Some(free) = free else { return Vec::new() };
    let mib = free / MIB;
    let path = path.display();
    match moment {
        Moment::BeforeLaunch if free < CRITICAL => vec![
            String::new(),
            format!(
                "  *** {mib} MB free on {path}. Roblox writes its storage, logs and asset cache here, and at \
                 this little space it will probably fail in a way that does not mention the disk. ***"
            ),
            String::new(),
        ],
        Moment::BeforeLaunch if free < LOW => {
            vec![format!("  warning: {mib} MB free on {path}; Roblox writes its storage and asset cache here")]
        }
        Moment::AfterExit if free < CRITICAL => vec![
            String::new(),
            format!(
                "  *** the client stopped with only {mib} MB free on {path}. A failed write is the most likely \
                 cause: the engine does not report a full disk, it fails somewhere unrelated to it. ***"
            ),
        ],
        _ => Vec::new(),
    }
}

/// Look at the filesystem holding `files` and say so if space is short.
pub fn check(files: &Path, moment: Moment) {
    for line in warning(free_bytes(files), files, moment) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    #[test]
    fn plenty_is_silent() {
        let p = Path::new("/data/files");
        assert!(warning(Some(50 * GIB), p, Moment::BeforeLaunch).is_empty());
        assert!(warning(Some(50 * GIB), p, Moment::AfterExit).is_empty());
    }

    #[test]
    fn low_warns_only_before_the_launch() {
        let p = Path::new("/data/files");
        let before = warning(Some(GIB), p, Moment::BeforeLaunch);
        assert_eq!(before, ["  warning: 1024 MB free on /data/files; Roblox writes its storage and asset cache here"]);
        assert!(warning(Some(GIB), p, Moment::AfterExit).is_empty());
    }

    #[test]
    fn critical_warns_at_both_moments() {
        let p = Path::new("/data/files");
        let before = warning(Some(100 * MIB), p, Moment::BeforeLaunch).join("\n");
        assert!(before.contains("100 MB free") && before.contains("asset cache") && before.contains("/data/files"));
        let after = warning(Some(100 * MIB), p, Moment::AfterExit).join("\n");
        assert!(after.contains("the client stopped") && after.contains("does not report a full disk"));
        assert!(after.contains("/data/files"));
    }

    #[test]
    fn the_thresholds_are_exclusive_and_mib_rounds_down() {
        let p = Path::new("/f");
        assert!(warning(Some(LOW), p, Moment::BeforeLaunch).is_empty());
        assert_eq!(warning(Some(LOW - 1), p, Moment::BeforeLaunch).len(), 1);
        assert_eq!(warning(Some(CRITICAL), p, Moment::BeforeLaunch).len(), 1);
        assert_eq!(warning(Some(CRITICAL - 1), p, Moment::BeforeLaunch).len(), 3);
        assert!(warning(Some(CRITICAL - 1), p, Moment::BeforeLaunch)[1].contains(" 511 MB free"));
        assert!(warning(Some(0), p, Moment::AfterExit)[1].contains("only 0 MB free"));
    }

    #[test]
    fn unknown_space_is_never_short() {
        assert!(warning(None, Path::new("/f"), Moment::BeforeLaunch).is_empty());
        assert!(warning(None, Path::new("/f"), Moment::AfterExit).is_empty());
    }

    #[test]
    fn statfs_answers_for_a_real_path_and_walks_up_from_a_missing_one() {
        let root = free_bytes(Path::new("/")).expect("statfs of /");
        assert!(root > 0);
        let tmp = std::env::temp_dir();
        let here = free_bytes(&tmp).expect("statfs of the temporary directory");
        let below = free_bytes(&tmp.join("no/such/directory/below")).expect("walks up");
        // Same filesystem, read a moment apart.
        assert!(here.abs_diff(below) < 64 * MIB, "{here} {below}");
    }
}
