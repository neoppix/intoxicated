//! Build script: the stub table (`src/stub_table.rs` includes what this writes)
//! and the commit stamp for the window title.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

/// The guest linker answers these itself, so they never get a stub.
const LINKER_PROVIDED: [&str; 7] = ["dlopen", "dlsym", "dlclose", "dlerror", "dladdr", "dl_iterate_phdr", "dlvsym"];

fn main() {
    generate_stub_table();
    stamp_commit();
}

/// `__start_<section>` and `__stop_<section>` are the bounds a static linker
/// defines for a section it keeps. `libroblox.so` imports four of them weakly
/// (`ls_rbx_dyninit1`, `ls_rbx_dyninit4`); a weak reference nothing defines
/// reads as null, which is the answer it has on Android. A stub would hand
/// it two unrelated code addresses to walk between.
fn is_section_bound(name: &str) -> bool {
    name.starts_with("__start_") || name.starts_with("__stop_")
}

/// One entry point per name in `docs/analysis/undefined-symbols.tsv`
/// (`consumer<TAB>symbol` per line), deduplicated and in byte order, so that
/// a name's index is its position and a call identifies the name.
fn generate_stub_table() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let list = manifest.join("../../docs/analysis/undefined-symbols.tsv");
    println!("cargo:rerun-if-changed={}", list.display());
    let text = std::fs::read_to_string(&list).unwrap_or_else(|e| panic!("cannot read {}: {e}", list.display()));

    let names: BTreeSet<&str> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(_, symbol)| symbol.trim())
        .filter(|s| !s.is_empty() && !LINKER_PROVIDED.contains(s) && !is_section_bound(s))
        .collect();

    let mut out = String::new();
    let n = names.len();
    let _ = writeln!(out, "/// How many names the stub list holds.");
    let _ = writeln!(out, "pub const STUB_COUNT: usize = {n};");
    let _ = writeln!(out, "/// The names, in byte order; a stub's index is its position here.");
    let _ = writeln!(out, "pub static STUB_NAMES: [&str; STUB_COUNT] = [");
    for name in &names {
        let _ = writeln!(out, "    {name:?},");
    }
    let _ = writeln!(out, "];");
    let _ = writeln!(out, "/// One entry point per name, in the same order.");
    let _ = writeln!(out, "pub static STUB_ENTRIES: [extern \"C\" fn() -> i64; STUB_COUNT] = [");
    for i in 0..n {
        let _ = writeln!(out, "    stub_{i},");
    }
    let _ = writeln!(out, "];");
    for i in 0..n {
        let _ = writeln!(out, "extern \"C\" fn stub_{i}() -> i64 {{ hit({i}) }}");
    }

    let dest = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("stub_table.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| panic!("cannot write {}: {e}", dest.display()));
    println!("cargo:warning=generated {n} stubs");
}

/// `INTOXICATED_GIT_SHA` for `desktop::title`: the short commit, with `-dirty`
/// when tracked files have uncommitted changes. A packager building without a
/// `.git` can set `INTOXICATED_GIT_SHA` itself. Neither leaves it unset.
fn stamp_commit() {
    println!("cargo:rerun-if-env-changed=INTOXICATED_GIT_SHA");
    if let Ok(sha) = std::env::var("INTOXICATED_GIT_SHA") {
        if !sha.trim().is_empty() {
            println!("cargo:rustc-env=INTOXICATED_GIT_SHA={}", sha.trim());
            return;
        }
    }
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
    };
    // Rebuild when HEAD moves or the index changes. `--git-path` resolves
    // these inside a linked worktree as well as in a plain checkout.
    let mut watch = vec!["HEAD".to_string(), "index".to_string()];
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch.push(head_ref);
    }
    for name in watch {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", &name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let Some(sha) = git(&["rev-parse", "--short=9", "HEAD"]).filter(|s| !s.is_empty()) else {
        return;
    };
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=INTOXICATED_GIT_SHA={sha}{}", if dirty { "-dirty" } else { "" });
}
