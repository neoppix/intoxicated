//! The stubs: one entry point for every name in the stub list
//! (`docs/analysis/undefined-symbols.tsv`, turned into a table by `build.rs`).
//!
//! The symbol table hands a stub to any import nothing else answers. A stub
//! does nothing and returns 0 in the integer return register; what it adds is
//! the record. Each call is counted per name, from any thread, and the order in
//! which names were first reached is kept, so [`report`] can say which
//! unanswered imports the engine actually reached and how often.
//!
//! On the first call of a name it goes into the gap register
//! ([`crate::gap_log`]) as a libc stub and `[stub] <name>` is printed on
//! stderr, unless `INTOXICATED_STUB_QUIET` is set. With `INTOXICATED_STUB_ABORT`
//! set, every call prints which stub was reached, the report, and aborts.
//!
//! A data import resolved to a stub gets the stub's code address, so the
//! engine reads instruction bytes through it. Names that are data in bionic
//! therefore have real objects in `bionic::data_overrides` instead.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

include!(concat!(env!("OUT_DIR"), "/stub_table.rs"));

/// The four zstd tracing hooks. Weak in the engine; a zero from them is their
/// documented "tracing is off", so reaching one is not a gap.
const TRACE_OFF_ANSWERS: [&str; 4] = [
    "ZSTD_trace_compress_begin",
    "ZSTD_trace_compress_end",
    "ZSTD_trace_decompress_begin",
    "ZSTD_trace_decompress_end",
];

#[allow(clippy::declare_interior_mutable_const)]
const UNCALLED: AtomicU64 = AtomicU64::new(0);
static CALLS: [AtomicU64; STUB_COUNT] = [UNCALLED; STUB_COUNT];
/// Indices in the order their first call arrived.
static FIRST_CALLED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn env_set(cell: &'static OnceLock<bool>, name: &str) -> bool {
    *cell.get_or_init(|| std::env::var_os(name).is_some())
}

fn quiet() -> bool {
    static QUIET: OnceLock<bool> = OnceLock::new();
    env_set(&QUIET, "INTOXICATED_STUB_QUIET")
}

fn abort_on_call() -> bool {
    static ABORT: OnceLock<bool> = OnceLock::new();
    env_set(&ABORT, "INTOXICATED_STUB_ABORT")
}

/// What every generated stub runs: count the call, note a first one, return 0.
pub fn hit(index: usize) -> i64 {
    let Some(count) = CALLS.get(index) else {
        return 0;
    };
    let name = STUB_NAMES[index];
    if count.fetch_add(1, Ordering::Relaxed) == 0 {
        FIRST_CALLED.lock().unwrap_or_else(|e| e.into_inner()).push(index);
        if !TRACE_OFF_ANSWERS.contains(&name) {
            crate::gap_log::record(crate::gap_log::Kind::LibcStub, name);
        }
        if !quiet() {
            eprintln!("[stub] {name}");
        }
    }
    if abort_on_call() {
        eprintln!("\n*** INTOXICATED_STUB_ABORT: the engine called the stub for {name} ***");
        report();
        std::process::abort();
    }
    0
}

/// Every stub as `(name, entry point)`, in stub-list order.
pub fn entries() -> impl Iterator<Item = (&'static str, *mut c_void)> {
    STUB_NAMES.iter().zip(STUB_ENTRIES.iter()).map(|(name, f)| (*name, *f as *mut c_void))
}

/// The entry point for `name`, if the stub list has it.
pub fn address(name: &str) -> Option<*mut c_void> {
    STUB_NAMES.binary_search(&name).ok().map(|i| STUB_ENTRIES[i] as *mut c_void)
}

/// The names reached so far with their call counts: most calls first, ties
/// in the order they were first reached.
pub fn calls() -> Vec<(&'static str, u64)> {
    let order = FIRST_CALLED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut rows: Vec<(usize, &'static str, u64)> = order
        .iter()
        .enumerate()
        .map(|(rank, &i)| (rank, STUB_NAMES[i], CALLS[i].load(Ordering::Relaxed)))
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    rows.into_iter().map(|(_, name, n)| (name, n)).collect()
}

/// The report [`report`] prints.
pub fn render() -> String {
    let rows = calls();
    if rows.is_empty() {
        return "\n=== no stubs were called ===\n".to_string();
    }
    let mut out = format!("\n=== stubs called: {} distinct of {} ===\n", rows.len(), STUB_COUNT);
    for (name, n) in rows {
        out.push_str(&format!("  {n:>9}  {name}\n"));
    }
    out
}

/// Print which stubs were reached, on stderr.
pub fn report() {
    eprint!("{}", render());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_sorted_unique_and_skips_what_the_linker_provides() {
        assert!(STUB_COUNT > 0);
        assert!(STUB_NAMES.windows(2).all(|w| w[0] < w[1]), "not strictly ascending");
        for name in ["dlopen", "dlsym", "dlclose", "dlerror", "dladdr", "dl_iterate_phdr", "dlvsym"] {
            assert!(address(name).is_none(), "{name} has a stub");
        }
        // Section bounds stay unresolved so a weak reference reads as null.
        for name in ["__start_ls_rbx_dyninit1", "__stop_ls_rbx_dyninit4"] {
            assert!(address(name).is_none(), "{name} has a stub");
        }
    }

    #[test]
    fn every_name_has_its_own_entry_point() {
        let mut seen: Vec<usize> = entries().map(|(_, a)| a as usize).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), STUB_COUNT, "two names share an entry point");
    }

    #[test]
    fn a_stub_returns_zero_and_is_counted() {
        let name = "AndroidBitmap_getInfo";
        let f: extern "C" fn() -> i64 = STUB_ENTRIES[STUB_NAMES.binary_search(&name).unwrap()];
        assert_eq!(f(), 0);
        assert_eq!(f(), 0);
        let n = calls().into_iter().find(|(s, _)| *s == name).map(|(_, n)| n);
        assert!(n.is_some_and(|n| n >= 2), "{n:?}");
        assert!(render().contains(&format!("  {:>9}  {name}\n", n.unwrap())));
    }
}
