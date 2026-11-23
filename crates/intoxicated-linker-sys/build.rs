//! Builds `native/` with CMake and tells rustc how to link what it produced.
//!
//! The native tree is one CMake project (`native/CMakeLists.txt`) that pulls in
//! the bionic linker port and libjnivm from `third_party/`. This script adds
//! nothing to it: it checks the one submodule CMake cannot diagnose well, runs
//! the build, and names the six archives the Rust side needs.
//!
//! FreeBSD is the only target. The link line below is the FreeBSD one (libc++,
//! no libdl) and nothing else has been built or run, so
//! another target is refused here rather than handed a link line that was
//! never true for it.

use std::env;
use std::path::{Path, PathBuf};

/// The archives `native/CMakeLists.txt` installs that the Rust side links, in
/// link order. `logger` has to come after `jnivm`: libjnivm calls into the
/// logger, and a single-pass archive linker only resolves references to
/// archives that appear later on the line. They are exactly the six
/// `native/CMakeLists.txt` installs.
const ARCHIVES: [&str; 6] =
    ["intoxicated_linker_shim", "intoxicated_jni_shim", "intoxicated_liblog", "jnivm", "logger", "linker"];

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "freebsd" {
        panic!(
            "intoxicated-linker-sys builds for FreeBSD only (target OS is `{target_os}`); \
             no other link line exists"
        );
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    // This crate sits at crates/<name>/, so the workspace root is two up.
    let root = manifest_dir
        .join("../..")
        .canonicalize()
        .expect("the workspace root two directories above this crate must exist");
    let native = root.join("native");
    let loader = root.join("third_party/mcpelauncher-linker");

    // CMake's own error for a missing submodule is a wall of "Cannot find
    // source file" lines that never says the word submodule. Probing a file
    // inside the nested `bionic` also catches the outer submodule cloned
    // without `--recursive`, which is the more common way to get this wrong.
    if !loader.join("bionic/linker/linker.cpp").is_file() {
        panic!(
            "third_party/mcpelauncher-linker is not checked out (or its nested bionic and core are missing).\n\
             Run `git submodule update --init --recursive` in the repository root and build again."
        );
    }

    watch(&native, &loader, &root.join("third_party/libjnivm"));

    let trace = env::var_os("INTOXICATED_JNI_TRACE").is_some();
    let prefix = cmake::Config::new(&native)
        // Bare names, resolved through PATH: the bionic sources need Clang
        // and native/CMakeLists.txt refuses anything else at configure time.
        .define("CMAKE_C_COMPILER", "clang")
        .define("CMAKE_CXX_COMPILER", "clang++")
        // Release whatever Cargo's profile is, so `cargo test` and a debug
        // `cargo build` link the same native code the release client runs.
        // Release is the only configuration of this tree anyone has run.
        .profile("Release")
        // libjnivm's per-call trace. Present at all (even empty) turns it on,
        // which matches how the C side reads the same variable at run time.
        .define("INTOXICATED_JNI_TRACE", if trace { "ON" } else { "OFF" })
        .build();

    let lib_dir = prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    // Cargo's default static kind bundles each archive into this crate's
    // rlib, which the final link places after intoxicated-runtime's. That
    // ordering is load-bearing: the runtime declares about a hundred of these
    // archives' symbols itself (the fortify and bionic_* shims, the audio and
    // MessageBus entry points), and they resolve only because this rlib comes
    // later on the line.
    for archive in ARCHIVES {
        println!("cargo:rustc-link-lib=static={archive}");
    }
    // FreeBSD's C++ runtime. Rust links with -nodefaultlibs, so nothing else
    // would bring it in; libc++.so is a linker script that also names libcxxrt.
    println!("cargo:rustc-link-lib=dylib=c++");
    // The linker archive's zip reader (core/libziparchive) is written against
    // zlib, and mcpelauncher-linker's CMakeLists links it publicly. Nothing
    // in intoxicated-run keeps a reference to it, so --as-needed leaves libz out
    // of DT_NEEDED; the request stays because the archive's dependency is real.
    println!("cargo:rustc-link-lib=dylib=z");
    // libthr. dlopen and dlsym are in FreeBSD's libc, so there is no libdl.
    println!("cargo:rustc-link-lib=dylib=pthread");
    // epoll-shim (ports) is not linked: the runtime's ALooper waits on native
    // kqueue, and the engine's own epoll imports reach epoll-shim through
    // native/freebsd_abi.c, which dlopens it at run time.
}

/// Tells Cargo which source changes have to rebuild the native tree.
///
/// Cargo hashes nothing here; it compares modification times, and a directory
/// is scanned recursively. Missing one of these is the failure mode where an
/// edit compiles, links, and has no effect because a stale archive was
/// linked instead, so this errs towards watching too much.
fn watch(native: &Path, loader: &Path, jnivm: &Path) {
    println!("cargo:rerun-if-env-changed=INTOXICATED_JNI_TRACE");
    // The whole directory rather than a file list, so a file added next to
    // CMakeLists.txt is covered before anyone remembers to list it here.
    println!("cargo:rerun-if-changed={}", native.display());

    // Inside the submodules, the source directories only: a hand-made clone
    // keeps a real `.git` directory in each of them, and watching that would
    // rebuild the native tree whenever git touches its own bookkeeping.
    let third_party = [
        loader.join("CMakeLists.txt"),
        loader.join("src"),
        loader.join("include"),
        loader.join("public_include"),
        loader.join("bionic/linker"),
        loader.join("bionic/libdl"),
        loader.join("bionic/libc/bionic"),
        loader.join("bionic/libc/async_safe"),
        loader.join("bionic/libc/upstream-openbsd/lib/libc/string"),
        loader.join("core/base"),
        loader.join("core/liblog"),
        loader.join("core/libcutils"),
        loader.join("core/libziparchive"),
        jnivm.join("CMakeLists.txt"),
        jnivm.join("include"),
        jnivm.join("src"),
    ];
    for path in third_party.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
