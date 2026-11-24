//! Android's `/system`, as far as the engine is shown one.
//!
//! The engine looks for system files by absolute path: fonts under
//! `/system/fonts`, and, through the anticheat, `su` binaries and Xposed
//! files elsewhere under `/system`. `native/system_paths.cpp` rewrites every
//! path that begins with `/system/` to the same path under a root this module
//! builds, `$XDG_CACHE_HOME/intoxicated/system`. `/system` itself is never
//! created.
//!
//! The root holds nothing but `fonts/`, because everything under it is what
//! the engine believes `/system` contains, root checks included. `fonts/` is
//! one symlink per font file the host has, by file name, so an exact request
//! for a font the host has finds the real file, plus the AOSP names the engine
//! knows, each standing in for the closest font the host has.
//!
//! This module also hands two other facts to the native layer before the
//! engine loads: which engine and APK this process runs, for the synthetic
//! `/proc/self/maps`, and the profile's files directory.

use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

extern "C" {
    fn intoxicated_set_system_root(root: *const std::ffi::c_char);
    fn intoxicated_set_files_dir(dir: *const std::ffi::c_char);
    fn intoxicated_set_maps_sources(lib_dir: *const std::ffi::c_char, apk: *const std::ffi::c_char);
}

/// Tell the synthetic `/proc/self/maps` which engine and APK this process runs.
///
/// `native/system_paths.cpp` rewrites their real paths to Android ones. It
/// matched `/home/pascal/...` literally, so for anybody else, or for a build
/// outside the launcher's layout, the engine's and the APK's lines reached the
/// blanking pass instead and the anticheat read a map in which `libroblox.so`
/// had no path. Call before the engine loads; the table is built on first use.
pub fn set_maps_sources(lib_dir: &str, apk: Option<&str>) {
    let lib = CString::new(lib_dir).unwrap_or_default();
    let apk = apk.and_then(|a| CString::new(a).ok());
    // SAFETY: the callee copies both strings; neither need outlive the call.
    unsafe {
        intoxicated_set_maps_sources(lib.as_ptr(), apk.as_ref().map_or(std::ptr::null(), |a| a.as_ptr()))
    };
}

/// The host directory behind the engine's `getFilesDir()`.
///
/// Without it the framework layer (`native/jni_values.cpp`) falls back to
/// `intoxicated/instances/default/data`, a layout ADR-012 replaced that follows
/// no `--profile`, so `getFilesDir()` would answer about a directory the client
/// is not using, identically for every profile. Call this with the same path
/// handed to `nativeSetFilesDirectory`, and before anything asks: the C++ side
/// latches it on first use.
pub fn set_files_dir(dir: &Path) {
    let Ok(dir) = CString::new(dir.as_os_str().as_bytes()) else {
        return;
    };
    // SAFETY: the callee copies the string.
    unsafe { intoxicated_set_files_dir(dir.as_ptr()) };
}

/// Where the host keeps fonts, searched in this order. The Linux and Flatpak
/// locations (`/usr/share/fonts`, `/run/host/usr/share/fonts`) do not exist on
/// FreeBSD and are not searched.
fn font_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/usr/local/share/fonts")];
    if let Some(home) = home {
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }
    dirs
}

/// How deep below each font directory the search goes. Symlinked
/// directories are followed, so this is also what stops a symlink loop.
const MAX_DEPTH: usize = 6;

/// The names the engine knows from AOSP, each with the host fonts that may
/// stand in for it, best first.
const AOSP: [(&str, &[&str]); 7] = [
    ("NotoSansCJK-Regular.ttc", &["NotoSansCJK-Regular.ttc", "NotoSansCJKjp-Regular.otf"]),
    ("Roboto-Regular.ttf", &["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
    ("Roboto-Bold.ttf", &["NotoSans-Bold.ttf", "DejaVuSans-Bold.ttf"]),
    ("DroidSans.ttf", &["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
    ("DroidSansMono.ttf", &["NotoSansMono-Regular.ttf", "DejaVuSansMono.ttf"]),
    ("DroidSansFallback.ttf", &["NotoSansCJK-Regular.ttc", "DejaVuSans.ttf"]),
    ("NotoColorEmoji.ttf", &["NotoColorEmoji.ttf"]),
];

/// Whether `path` names a font file by its extension: TrueType or OpenType,
/// single or collection, in any case.
fn is_font(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| ["ttf", "ttc", "otf", "otc"].iter().any(|k| e.eq_ignore_ascii_case(k)))
}

/// The host's fonts by file name. The first file found under a name wins:
/// directories in `dirs` order, then each directory's listing order.
#[derive(Default)]
struct FontIndex {
    by_name: HashMap<OsString, PathBuf>,
    order: Vec<OsString>,
}

impl FontIndex {
    fn build(dirs: &[PathBuf]) -> FontIndex {
        let mut index = FontIndex::default();
        for dir in dirs {
            index.walk(dir, 0);
        }
        index
    }

    fn walk(&mut self, dir: &Path, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Follows symlinks: a linked directory is searched and a linked
            // font is a font.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    self.walk(&path, depth + 1);
                }
            } else if meta.is_file() && is_font(&path) {
                let name = entry.file_name();
                if !self.by_name.contains_key(&name) {
                    self.order.push(name.clone());
                    self.by_name.insert(name, path);
                }
            }
        }
    }

    fn get(&self, name: &str) -> Option<&PathBuf> {
        self.by_name.get(OsStr::new(name))
    }
}

/// The first candidate the index has.
fn stand_in<'a>(index: &'a FontIndex, candidates: &[&str]) -> Option<&'a PathBuf> {
    candidates.iter().find_map(|c| index.get(c))
}

/// Make `link` a symlink to `target`. An existing link to `target` is kept,
/// a link to anything else replaced, and anything that is not a link left
/// alone. True when `link` ends up usable.
///
/// The tree is shared by every profile, and a client already running reads
/// it while the next one builds it. So a link is never removed and made
/// again: the new one is made under a temporary name and renamed over the
/// old one, and a link another client made first is taken as it is.
fn link_font(link: &Path, target: &Path) -> bool {
    let points_at_target = || std::fs::read_link(link).is_ok_and(|t| t == target);
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => points_at_target() || replace_link(link, target),
        Ok(_) => true,
        Err(_) => std::os::unix::fs::symlink(target, link).is_ok() || points_at_target(),
    }
}

/// Point the existing link `link` at `target` with one rename(2).
fn replace_link(link: &Path, target: &Path) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static MADE: AtomicU64 = AtomicU64::new(0);
    let (Some(dir), Some(name)) = (link.parent(), link.file_name()) else {
        return false;
    };
    let n = MADE.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{}.{}.{n}.tmp", name.to_string_lossy(), std::process::id()));
    if std::os::unix::fs::symlink(target, &tmp).is_err() {
        return false;
    }
    if std::fs::rename(&tmp, link).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// Remove the links in `fonts` whose target is gone (a font uninstalled
/// since the last run).
fn prune(fonts: &Path) {
    let Ok(entries) = std::fs::read_dir(fonts) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
        if is_link && std::fs::metadata(&path).is_err() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Remove whatever sits in the root beside `fonts/`: the engine would see it
/// as part of `/system`, and its root checks look there.
fn keep_only_fonts(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == "fonts" {
            continue;
        }
        let path = entry.path();
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        let removed = if is_dir { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        if removed.is_ok() {
            println!("  system: removed {}; only fonts/ belongs in the tree", path.display());
        }
    }
}

/// Build the tree under `root` from `index`. Returns how many names ended up
/// usable: every indexed name, and each AOSP name linked by this call (one
/// already present is skipped uncounted).
fn build(root: &Path, index: &FontIndex) -> usize {
    let fonts = root.join("fonts");
    let _ = std::fs::create_dir_all(&fonts);
    keep_only_fonts(root);
    prune(&fonts);
    let mut count = 0;
    for name in &index.order {
        if link_font(&fonts.join(name), &index.by_name[name]) {
            count += 1;
        }
    }
    for (aosp, candidates) in AOSP {
        let link = fonts.join(aosp);
        if link.exists() {
            continue;
        }
        if let Some(target) = stand_in(index, candidates) {
            if link_font(&link, target) {
                count += 1;
            }
        }
    }
    count
}

/// The cache directory: `$XDG_CACHE_HOME` when it is absolute, else
/// `$HOME/.cache` when `HOME` is absolute, else the temporary directory.
/// The same rule `syscall_sites` follows.
fn cache_base() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
}

/// `$HOME`, when it is an absolute path.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute())
}

/// The Android trace switch, read here because `android::set_trace` has not
/// run yet when the tree is built.
const TRACE_ENV: &str = "INTOXICATED_ANDROID_TRACE";

/// Build the tree and point the native rewrite at it. Returns the root.
///
/// Call once, before anything can resolve a path. It never fails: a font that
/// cannot be linked is left out, and the rewrite is armed whatever happened,
/// so `/system` lookups land in a
/// directory Intoxicated controls and show up in `INTOXICATED_TRACE_PATHS=1` as a miss
/// instead of reaching the host's own files.
pub fn install() -> PathBuf {
    let root = cache_base().join("intoxicated/system");
    let index = FontIndex::build(&font_dirs(home().as_deref()));
    let count = build(&root, &index);
    if std::env::var_os(TRACE_ENV).is_some() {
        println!("[android] /system served from {} ({count} fonts)", root.display());
    }
    if let Ok(c) = CString::new(root.as_os_str().as_bytes()) {
        // SAFETY: the callee copies the string.
        unsafe { intoxicated_set_system_root(c.as_ptr()) };
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("intoxicated-system-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"font").unwrap();
    }

    /// Every entry under `dir`, as `relative -> link target` (or `-` for a
    /// file or directory), sorted.
    fn listing(dir: &Path) -> Vec<(String, String)> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().display().to_string();
                let meta = std::fs::symlink_metadata(&p).unwrap();
                if meta.file_type().is_symlink() {
                    out.push((rel, std::fs::read_link(&p).unwrap().display().to_string()));
                } else if meta.is_dir() {
                    out.push((rel, "-".into()));
                    walk(base, &p, out);
                } else {
                    out.push((rel, "-".into()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn fonts_are_known_by_extension_in_any_case() {
        assert!(is_font(Path::new("/x/NotoSansCJK-Regular.ttc")));
        assert!(is_font(Path::new("/x/DejaVuSans.TTF")));
        assert!(is_font(Path::new("/x/a.otf")) && is_font(Path::new("/x/a.OtC")));
        assert!(!is_font(Path::new("/x/fonts.dir")));
        assert!(!is_font(Path::new("/x/README")));
    }

    #[test]
    fn the_first_candidate_the_host_has_stands_in() {
        let base = scratch("candidates");
        touch(&base.join("a/DejaVuSans.ttf"));
        touch(&base.join("b/NotoSans-Regular.ttf"));
        let index = FontIndex::build(&[base.join("a"), base.join("b")]);
        assert_eq!(
            stand_in(&index, &["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
            Some(&base.join("b/NotoSans-Regular.ttf"))
        );
        assert_eq!(stand_in(&FontIndex::default(), &["NotoColorEmoji.ttf"]), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_first_directory_wins_a_name_and_the_depth_is_bounded() {
        let base = scratch("index");
        touch(&base.join("one/DejaVuSans.ttf"));
        touch(&base.join("two/DejaVuSans.ttf"));
        touch(&base.join("two/1/2/3/4/5/6/Deep6.ttf"));
        touch(&base.join("two/1/2/3/4/5/6/7/Deep7.ttf"));
        // A loop: only the depth bound ends the walk.
        std::os::unix::fs::symlink(base.join("two"), base.join("two/loop")).unwrap();
        let index = FontIndex::build(&[base.join("one"), base.join("two"), base.join("missing")]);
        assert_eq!(index.get("DejaVuSans.ttf"), Some(&base.join("one/DejaVuSans.ttf")));
        assert!(index.get("Deep6.ttf").is_some());
        assert!(index.get("Deep7.ttf").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_tree_holds_one_link_per_font_and_the_stand_ins_and_nothing_else() {
        let base = scratch("tree");
        let host = base.join("host");
        touch(&host.join("noto/NotoSans-Regular.ttf"));
        touch(&host.join("noto/NotoSans-Bold.ttf"));
        touch(&host.join("dejavu/DejaVuSans.ttf"));
        touch(&host.join("dejavu/DejaVuSansMono.ttf"));
        touch(&host.join("fonts.dir"));
        let root = base.join("cache/intoxicated/system");
        // Something that does not belong beside fonts/.
        touch(&root.join("bin/su"));

        let index = FontIndex::build(&[host.clone()]);
        assert_eq!(build(&root, &index), 4 + 5, "four host names and five stand-ins");
        let first = listing(&root);
        let links: Vec<&(String, String)> = first.iter().filter(|(_, t)| t != "-").collect();
        assert_eq!(links.len(), 9, "{first:?}");
        assert!(first.iter().all(|(p, _)| p == "fonts" || p.starts_with("fonts/")), "{first:?}");
        let target = |name: &str| {
            first.iter().find(|(p, _)| p == &format!("fonts/{name}")).map(|(_, t)| t.clone())
        };
        let noto = host.join("noto/NotoSans-Regular.ttf").display().to_string();
        assert_eq!(target("Roboto-Regular.ttf"), Some(noto.clone()));
        assert_eq!(target("DroidSans.ttf"), Some(noto.clone()));
        assert_eq!(target("NotoSans-Regular.ttf"), Some(noto));
        assert_eq!(target("DroidSansFallback.ttf"), Some(host.join("dejavu/DejaVuSans.ttf").display().to_string()));
        assert_eq!(target("NotoColorEmoji.ttf"), None);
        assert_eq!(target("NotoSansCJK-Regular.ttc"), None);

        // A second run changes nothing; the stand-ins already there are not
        // counted again.
        assert_eq!(build(&root, &FontIndex::build(&[host.clone()])), 4);
        assert_eq!(listing(&root), first);

        // A font that goes away takes its link and its stand-ins with it.
        std::fs::remove_file(host.join("noto/NotoSans-Regular.ttf")).unwrap();
        build(&root, &FontIndex::build(&[host.clone()]));
        let after = listing(&root);
        assert!(!after.iter().any(|(p, _)| p == "fonts/NotoSans-Regular.ttf"), "{after:?}");
        let dejavu = host.join("dejavu/DejaVuSans.ttf").display().to_string();
        assert!(after.contains(&("fonts/Roboto-Regular.ttf".into(), dejavu.clone())), "{after:?}");
        assert!(after.contains(&("fonts/DroidSans.ttf".into(), dejavu)), "{after:?}");
        assert!(after.iter().all(|(_, t)| t == "-" || Path::new(t).exists()), "{after:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_running_client_never_sees_a_font_link_missing_while_another_builds() {
        // Two profiles' clients share the tree: one reads it while the next
        // one starts and rebuilds it, here with the link changing target.
        let base = scratch("shared");
        touch(&base.join("a/DejaVuSans.ttf"));
        touch(&base.join("b/DejaVuSans.ttf"));
        let root = base.join("root");
        let one = FontIndex::build(&[base.join("a")]);
        let two = FontIndex::build(&[base.join("b")]);
        build(&root, &one);
        let link = root.join("fonts/DejaVuSans.ttf");
        let done = std::sync::atomic::AtomicBool::new(false);
        let missing = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    if std::fs::symlink_metadata(&link).is_err() {
                        missing.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            });
            s.spawn(|| {
                for i in 0..400 {
                    build(&root, if i % 2 == 0 { &two } else { &one });
                }
                done.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        });
        assert_eq!(missing.into_inner(), 0, "the link was missing while it was replaced");
        assert_eq!(std::fs::read_link(&link).unwrap(), base.join("a/DejaVuSans.ttf"));
        let strays: Vec<_> = listing(&root).into_iter().filter(|(p, _)| p.contains(".tmp")).collect();
        assert!(strays.is_empty(), "{strays:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_file_in_the_way_is_left_alone() {
        let base = scratch("file");
        touch(&base.join("host/DejaVuSans.ttf"));
        let root = base.join("root");
        touch(&root.join("fonts/DejaVuSans.ttf"));
        // The file counts as usable, and DejaVuSans stands in for three AOSP names.
        assert_eq!(build(&root, &FontIndex::build(&[base.join("host")])), 1 + 3);
        let meta = std::fs::symlink_metadata(root.join("fonts/DejaVuSans.ttf")).unwrap();
        assert!(meta.is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `install` itself, in a child copy of this test binary with a scratch
    /// `HOME` and `XDG_CACHE_HOME`: the environment is process-wide, and the
    /// native root it arms would follow every later test in this process.
    #[test]
    fn install_builds_under_the_new_cache_path() {
        if let Some(cache) = std::env::var_os("INTOXICATED_SYSTEM_CHILD") {
            let root = install();
            assert_eq!(root, PathBuf::from(cache).join("intoxicated/system"));
            return;
        }
        let base = scratch("install");
        let home = base.join("home");
        touch(&home.join(".fonts/Mine.ttf"));
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "android::system::tests::install_builds_under_the_new_cache_path", "--nocapture"])
            .env("INTOXICATED_SYSTEM_CHILD", home.join(".cache"))
            .env("HOME", &home)
            .env_remove("XDG_CACHE_HOME")
            .env("INTOXICATED_ANDROID_TRACE", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
        let new = home.join(".cache/intoxicated/system");
        // The host's own /usr/local/share/fonts is searched too, so the
        // count is at least the one font in the scratch HOME.
        let served = format!("[android] /system served from {} (", new.display());
        let count: usize = stdout
            .lines()
            .find_map(|l| l.strip_prefix(&served))
            .and_then(|rest| rest.strip_suffix(" fonts)"))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no served line in {stdout}"));
        assert!(count >= 1, "{stdout}");
        assert_eq!(std::fs::read_link(new.join("fonts/Mine.ttf")).unwrap(), home.join(".fonts/Mine.ttf"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
