//! The Roblox engine library in a lib directory, and the version it carries.
//!
//! `libroblox.so` holds its own version as a plain ASCII literal (the string
//! it prints in its logs), so reading it is a string search over the file,
//! the same thing `strings` shows.
//!
//! The rule: split the file into maximal runs of bytes in `[0-9.]`. A run is
//! a candidate when it is 9 to 20 bytes long, has exactly four non-empty
//! numeric parts and its first part is `2`. Runs longer than 20 bytes are not
//! candidates and are never held whole. If every candidate is the same
//! string, that string is the version; if there are none, or two different
//! ones, the answer is `None` rather than a guess.
//!
//! Scanning 118 MB costs real startup time, so [`installed_version`] keeps the
//! answer in `.intoxicated-engine-version` beside the library, keyed on the
//! library's length and modification time in nanoseconds:
//!
//! ```text
//! <len> <mtime_nanos>\n<version>\n
//! ```
//!
//! An empty version line records "not readable" so that is not rescanned
//! either. Earlier releases wrote the same format under another name; that
//! file is ignored, so the first launch after the rename scans once.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// File name of the engine inside a lib directory.
pub const LIBRARY: &str = "libroblox.so";
/// The remembered answer, beside the library.
const RECORD: &str = ".intoxicated-engine-version";

const MIN_RUN: usize = 9;
const MAX_RUN: usize = 20;

/// Path of the engine library in `lib_dir`, whether or not it exists.
pub fn library_in(lib_dir: &Path) -> PathBuf {
    lib_dir.join(LIBRARY)
}

/// The engine version for `lib_dir`, from the record when the library has
/// not changed since it was written, otherwise from a fresh scan (which then
/// updates the record, best effort).
pub fn installed_version(lib_dir: &Path) -> Option<String> {
    let library = library_in(lib_dir);
    let key = record_key(&library);
    if let Some(key) = &key {
        if let Some(known) = read_record(lib_dir, key) {
            return (!known.is_empty()).then_some(known);
        }
    }
    let found = version_of(&library);
    if let Some(key) = &key {
        write_record(lib_dir, key, found.as_deref().unwrap_or(""));
    }
    found
}

/// Scan one library file, ignoring any record.
pub fn version_of(library: &Path) -> Option<String> {
    let file = std::fs::File::open(library).ok()?;
    scan(file)
}

fn record_key(library: &Path) -> Option<String> {
    let meta = std::fs::metadata(library).ok()?;
    let nanos = meta.modified().ok()?.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    Some(format!("{} {nanos}", meta.len()))
}

fn read_record(lib_dir: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(lib_dir.join(RECORD)).ok()?;
    let (have, version) = text.split_once('\n')?;
    (have == key).then(|| version.trim_end().to_owned())
}

fn write_record(lib_dir: &Path, key: &str, version: &str) {
    let path = lib_dir.join(RECORD);
    let tmp = lib_dir.join(format!("{RECORD}.tmp"));
    if std::fs::write(&tmp, format!("{key}\n{version}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Tracks the run of `[0-9.]` bytes being read and the answer so far.
struct Scanner {
    run: Vec<u8>,
    /// The current run passed [`MAX_RUN`]; ignore it until it ends.
    too_long: bool,
    found: Option<String>,
    ambiguous: bool,
}

impl Scanner {
    fn byte(&mut self, b: u8) {
        if b.is_ascii_digit() || b == b'.' {
            if self.too_long {
                return;
            }
            if self.run.len() == MAX_RUN {
                self.too_long = true;
                self.run.clear();
            } else {
                self.run.push(b);
            }
        } else {
            self.end_run();
        }
    }

    fn end_run(&mut self) {
        if !self.too_long {
            if let Some(v) = candidate(&self.run) {
                match &self.found {
                    Some(prev) if *prev != v => self.ambiguous = true,
                    Some(_) => {}
                    None => self.found = Some(v),
                }
            }
        }
        self.run.clear();
        self.too_long = false;
    }
}

/// The version literal in anything readable, under the rule in the module doc.
pub fn scan<R: Read>(mut source: R) -> Option<String> {
    let mut s = Scanner { run: Vec::with_capacity(MAX_RUN), too_long: false, found: None, ambiguous: false };
    let mut buf = vec![0u8; 256 << 10];
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        for &b in &buf[..n] {
            s.byte(b);
        }
        if s.ambiguous {
            return None;
        }
    }
    s.end_run();
    if s.ambiguous {
        None
    } else {
        s.found
    }
}

fn candidate(run: &[u8]) -> Option<String> {
    if !(MIN_RUN..=MAX_RUN).contains(&run.len()) {
        return None;
    }
    let text = std::str::from_utf8(run).ok()?;
    let mut parts = 0;
    for part in text.split('.') {
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        parts += 1;
    }
    (parts == 4 && text.starts_with("2.")).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(v: &str) -> Vec<u8> {
        let mut out = b"\0symbol\0".to_vec();
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\0GCC: (GNU) 14.2.0\0");
        out
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("intoxicated-engine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The real engine where the launcher keeps it. The expected value was
    /// recorded from the previous implementation on this same file before it
    /// was removed. Skips when the file is absent.
    #[test]
    fn real_libroblox_reads_the_recorded_version() {
        let Some(home) = std::env::var_os("HOME") else { return };
        let library = Path::new(&home).join(".cache/intoxicated/apk/lib/x86_64/libroblox.so");
        if !library.is_file() {
            eprintln!("skipping: {} is absent", library.display());
            return;
        }
        assert_eq!(version_of(&library).as_deref(), Some("2.748.0.1527"));
    }

    #[test]
    fn finds_the_literal_and_ignores_lookalikes() {
        assert_eq!(scan(&wrapped("2.734.0.917")[..]).as_deref(), Some("2.734.0.917"));
        // An OID contains a four-part prefix, but the whole run is what counts.
        let mut v = wrapped("2.734.0.917");
        v.extend_from_slice(b"2.16.840.1.101.3.4.2.1\0");
        assert_eq!(scan(&v[..]).as_deref(), Some("2.734.0.917"));
        assert_eq!(scan(&wrapped("0.734.0.917")[..]), None);
        assert_eq!(scan(&wrapped("2.734.0")[..]), None);
        assert_eq!(scan(&wrapped("2.734..917")[..]), None);
        assert_eq!(scan(&b""[..]), None);
    }

    #[test]
    fn repeats_agree_and_disagreement_is_none() {
        let mut same = wrapped("2.734.0.917");
        same.extend_from_slice(b"2.734.0.917\0");
        assert_eq!(scan(&same[..]).as_deref(), Some("2.734.0.917"));
        let mut differ = wrapped("2.734.0.917");
        differ.extend_from_slice(b"2.700.1.2\0");
        assert_eq!(scan(&differ[..]), None);
    }

    #[test]
    fn runs_cross_read_boundaries_and_long_runs_are_dropped() {
        let mut v = vec![0u8; (256 << 10) - 4];
        v.extend_from_slice(b"2.734.0.917");
        assert_eq!(scan(&v[..]).as_deref(), Some("2.734.0.917"), "also at end of file");
        assert_eq!(scan(&vec![b'7'; 1 << 20][..]), None);
        // Exactly 20 bytes is a candidate length; 21 is not.
        assert_eq!(scan(&b"2.1234567.12345.1234"[..]).as_deref(), Some("2.1234567.12345.1234"));
        assert_eq!(scan(&b"2.1234567.12345.12345"[..]), None);
    }

    #[test]
    fn the_record_answers_until_the_library_changes() {
        let dir = scratch("record");
        let lib = library_in(&dir);
        std::fs::write(&lib, b"x 2.736.0.1408 y").unwrap();
        assert_eq!(installed_version(&dir).as_deref(), Some("2.736.0.1408"));
        let key = record_key(&lib).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(RECORD)).unwrap(),
            format!("{key}\n2.736.0.1408\n")
        );
        // A planted record for the same key is believed: proof the scan was skipped.
        write_record(&dir, &key, "2.1.1.1");
        assert_eq!(installed_version(&dir).as_deref(), Some("2.1.1.1"));
        // A different length invalidates it.
        std::fs::write(&lib, b"x 2.800.0.1 and more bytes").unwrap();
        assert_eq!(installed_version(&dir).as_deref(), Some("2.800.0.1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_readable_is_recorded_and_absent_is_none() {
        let dir = scratch("unknown");
        std::fs::write(library_in(&dir), b"2.1.1.1000 2.2.2.2000").unwrap();
        assert_eq!(installed_version(&dir), None);
        assert!(std::fs::read_to_string(dir.join(RECORD)).unwrap().ends_with("\n\n"));
        assert_eq!(installed_version(&dir), None);
        std::fs::remove_file(library_in(&dir)).unwrap();
        assert_eq!(installed_version(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
