//! The gap register: every question the engine asked that the client
//! answered with a made-up value or not at all, counted, and written out at
//! the end of a run as a work queue.
//!
//! Four kinds feed it: placeholders the loader hands the framework, JNI
//! lookups libjnivm could not answer (from `native/jnivm_logger/jnivm_log.cpp`),
//! AGDK natives called while unregistered (`android::input`), and libc stubs
//! the engine reached (`stub_table`). Recording is a counter bump under one
//! lock, cheap enough for the input path that calls it per dropped event.
//!
//! The report goes to stdout and to a file: `INTOXICATED_UNIMPLEMENTED_LOG`
//! if set, else `files/appData/logs/intoxicated-unimplemented.log` under the
//! working directory (created if missing), else
//! `intoxicated/intoxicated-unimplemented.log` under `$XDG_DATA_HOME`,
//! `$HOME/.local/share` or the temporary directory, in that order.

use std::collections::HashMap;
use std::ffi::{c_char, c_uint, CStr};
use std::path::PathBuf;
use std::sync::Mutex;

/// What kind of gap a record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A framework call answered with an invented value.
    Placeholder,
    /// A JNI class, method or field libjnivm does not have.
    Jni,
    /// An AGDK native the engine called before it was registered.
    NativeNotRegistered,
    /// A libc import that resolved to a stub and was called.
    LibcStub,
}

impl Kind {
    /// Report order.
    const ALL: [Kind; 4] = [Kind::Placeholder, Kind::Jni, Kind::NativeNotRegistered, Kind::LibcStub];

    fn heading(self) -> &'static str {
        match self {
            Kind::Placeholder => "framework calls answered with a made-up value",
            Kind::Jni => "JNI classes, methods and fields libjnivm does not have",
            Kind::NativeNotRegistered => "AGDK natives called while unregistered",
            Kind::LibcStub => "libc stubs called (they return zero)",
        }
    }

    /// The numbering `intoxicated_unimplemented_record` takes from C.
    fn from_c(kind: c_uint) -> Kind {
        match kind {
            1 => Kind::LibcStub,
            2 => Kind::NativeNotRegistered,
            3 => Kind::Placeholder,
            _ => Kind::Jni,
        }
    }
}

static GAPS: Mutex<Option<HashMap<(Kind, String), u64>>> = Mutex::new(None);

/// Count one occurrence of `detail` under `kind`.
pub fn record(kind: Kind, detail: impl AsRef<str>) {
    let detail = detail.as_ref();
    let mut gaps = GAPS.lock().unwrap_or_else(|e| e.into_inner());
    let map = gaps.get_or_insert_with(HashMap::new);
    if let Some(n) = map.get_mut(&(kind, detail.to_string())) {
        *n += 1;
    } else {
        map.insert((kind, detail.to_string()), 1);
    }
}

/// Record a placeholder: `what` was answered with `returned`.
pub fn placeholder(what: impl std::fmt::Display, returned: impl std::fmt::Display) {
    record(Kind::Placeholder, format!("{what} -> {returned}"));
}

/// The C side's entry: kind 1 libc stub, 2 native not registered, 3
/// placeholder, anything else JNI. A null detail is ignored.
#[no_mangle]
pub extern "C" fn intoxicated_unimplemented_record(kind: c_uint, detail: *const c_char) {
    if detail.is_null() {
        return;
    }
    // SAFETY: the caller passes a NUL-terminated string that outlives the call.
    let text = unsafe { CStr::from_ptr(detail) }.to_string_lossy();
    record(Kind::from_c(kind), text);
}

/// Every record, grouped by kind in report order; within a kind, most counts
/// first and ties by detail.
fn snapshot() -> Vec<(Kind, Vec<(String, u64)>)> {
    let gaps = GAPS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = gaps.as_ref() else {
        return Vec::new();
    };
    Kind::ALL
        .iter()
        .filter_map(|&kind| {
            let mut rows: Vec<(String, u64)> =
                map.iter().filter(|((k, _), _)| *k == kind).map(|((_, d), n)| (d.clone(), *n)).collect();
            if rows.is_empty() {
                return None;
            }
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            Some((kind, rows))
        })
        .collect()
}

/// The report text.
pub fn render() -> String {
    let sections = snapshot();
    if sections.is_empty() {
        return "=== nothing went unanswered this run ===\n".to_string();
    }
    let mut out = String::from("=== what Intoxicated did not answer this run ===\n");
    out.push_str(
        "Each line is a question the engine asked that Intoxicated answered badly or not\n\
         at all. This is a work queue, not a diagnosis: a gap here is not evidence that\n\
         it is the gap that broke anything. Only what was reached this run is listed.\n\n",
    );
    if !sections.iter().any(|(k, _)| *k == Kind::Jni) {
        out.push_str(
            "No JNI gaps are listed, and that is NOT evidence there were none: libjnivm only\n\
             reports `Constructed Unresolved symbol` when the native tree is built with its\n\
             JNI trace (INTOXICATED_JNI_TRACE=1 in cargo's environment). It is very slow.\n\n",
        );
    }
    for (kind, rows) in sections {
        out.push_str(&format!("{} ({}):\n", kind.heading(), rows.len()));
        for (detail, n) in rows {
            out.push_str(&format!("  {n:>9}  {detail}\n"));
        }
        out.push('\n');
    }
    out
}

/// Where the report is written; see the module documentation.
fn log_path() -> PathBuf {
    const NAME: &str = "intoxicated-unimplemented.log";
    if let Some(p) = std::env::var_os("INTOXICATED_UNIMPLEMENTED_LOG") {
        return PathBuf::from(p);
    }
    let local = PathBuf::from("files/appData/logs");
    if local.is_dir() || std::fs::create_dir_all(&local).is_ok() {
        return local.join(NAME);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("intoxicated");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(NAME)
}

/// Print the report on stdout and write it to the log. A write failure is
/// said and does not fail the run.
pub fn report() {
    let text = render();
    println!("\n{text}");
    let path = log_path();
    match std::fs::write(&path, &text) {
        Ok(()) => println!("[unimplemented] also written to {}", path.display()),
        Err(e) => println!("[unimplemented] could not write {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_counted_grouped_and_ordered() {
        record(Kind::LibcStub, "gap_log_test_b");
        record(Kind::LibcStub, "gap_log_test_a");
        record(Kind::LibcStub, "gap_log_test_b");
        placeholder("gap_log_test_what", 7);
        let text = render();
        let b = text.find("          2  gap_log_test_b\n").expect("b counted twice");
        let a = text.find("          1  gap_log_test_a\n").expect("a counted once");
        assert!(b < a, "most counts first");
        assert!(text.contains("  gap_log_test_what -> 7\n"));
        let p = text.find(Kind::Placeholder.heading()).unwrap();
        let s = text.find(Kind::LibcStub.heading()).unwrap();
        assert!(p < s, "placeholders come before libc stubs");
    }

    #[test]
    fn the_c_entry_maps_kinds_and_ignores_null() {
        intoxicated_unimplemented_record(2, c"gap_log_test_native".as_ptr());
        intoxicated_unimplemented_record(9, c"gap_log_test_jni".as_ptr());
        intoxicated_unimplemented_record(1, std::ptr::null());
        let sections = snapshot();
        let has = |kind: Kind, d: &str| sections.iter().any(|(k, rows)| *k == kind && rows.iter().any(|(x, _)| x == d));
        assert!(has(Kind::NativeNotRegistered, "gap_log_test_native"));
        assert!(has(Kind::Jni, "gap_log_test_jni"));
    }
}
