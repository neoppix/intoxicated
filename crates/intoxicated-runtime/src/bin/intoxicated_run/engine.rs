//! Loading the engine: the bionic linker, the virtual libraries, the `dlopen`
//! of `libroblox.so`, the raw syscall trap, the read-only text and
//! `JNI_OnLoad`.

use std::ffi::c_void;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use intoxicated_linker_sys as linker;

use crate::cli::Options;

/// The loaded engine.
pub struct Loaded {
    pub lib: linker::Library,
    /// `JNI_OnLoad`, when the library exports it.
    pub on_load: Option<*mut c_void>,
}

/// A duration rounded to a whole number of its own unit, so `Debug` prints
/// `26ms` or `13µs` rather than every digit.
pub fn whole_units(d: Duration) -> Duration {
    let ns = d.as_nanos();
    let unit: u128 = if ns >= 1_000_000_000 {
        1_000_000_000
    } else if ns >= 1_000_000 {
        1_000_000
    } else if ns >= 1_000 {
        1_000
    } else {
        1
    };
    let rounded = (ns + unit / 2) / unit * unit;
    Duration::from_nanos(u64::try_from(rounded).unwrap_or(u64::MAX))
}

/// Size in MiB with one decimal, labelled MB as the log always has.
pub fn megabytes(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

/// Initialise the linker once, register every virtual library of the table
/// in order, set the search path and load the engine with eager binding.
/// A failure before the load is printed and ends the run with 1.
pub fn load(opt: &Options, table: &intoxicated_runtime::symtab::SymbolTable) -> Result<Loaded, ExitCode> {
    println!();
    println!("initialising bionic linker...");
    linker::init();
    for (soname, entries) in &table.libraries {
        let symbols: Vec<(String, *mut c_void)> = entries.iter().map(|e| (e.symbol.to_string(), e.address)).collect();
        if let Err(e) = linker::register(soname, &symbols) {
            eprintln!("failed to register {soname}: {e}");
            return Err(ExitCode::FAILURE);
        }
    }
    println!("registered {} virtual libraries", table.libraries.len());
    if let Err(e) = linker::set_library_path(&opt.lib_dir) {
        eprintln!("bad --lib-dir: {e}");
        return Err(ExitCode::FAILURE);
    }
    println!("search path: {}", opt.lib_dir);

    println!();
    println!("loading {} ...", opt.library);
    let started = Instant::now();
    // RTLD_NOW: nothing may be left to bind lazily into text that is made
    // read-only straight after.
    let lib = match linker::dlopen(&opt.library, linker::RTLD_NOW) {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!();
            eprintln!("LOAD FAILED after {:?}: {e}", whole_units(started.elapsed()));
            intoxicated_runtime::stubs::report();
            return Err(ExitCode::FAILURE);
        }
    };
    let elapsed = whole_units(started.elapsed());
    let (code_base, code_size) = lib.code_region();
    println!();
    println!("LOADED in {elapsed:?}");
    println!("  base       {:#x}", lib.base());
    println!("  code       {code_base:#x} + {code_size} bytes ({} MB)", megabytes(code_size));

    // Neutralise libroblox's raw `syscall` (0f 05) instructions BEFORE the text
    // is downgraded to r-x below: the patch needs the mapping writable. Hyperion
    // issues raw syscalls (bypassing libc, so bypassing the runtime's shims) and
    // compares them against the libc path to detect hooking; on FreeBSD a raw
    // Linux-numbered syscall hits the wrong kernel syscall, the results disagree,
    // and the server reports the client tampered -> reason-304 at grace. Rewriting
    // each site to `ud2` and routing the resulting SIGILL back through the same
    // `bionic_syscall` translator the libc path uses makes raw and libc agree.
    // See native/syscall_trap.c. `INTOXICATED_SYSCALL_TRAP=off` disables (control).
    #[cfg(target_os = "freebsd")]
    unsafe {
        extern "C" {
            fn intoxicated_install_raw_syscall_trap(
                base: usize,
                vaddrs: *const core::ffi::c_ulong,
                count: usize,
            ) -> i32;
        }
        // The sites move with every Roblox build: find them in the file
        // (cached per build-id) rather than trusting a fixed list.
        let sites = intoxicated_runtime::syscall_sites::for_load(
            &std::path::Path::new(&opt.lib_dir).join(&opt.library),
        );
        // The trap chains a SIGILL that is not one of its sites to the handler
        // it replaces. Make that the bionic layer's dispatch, so the engine's
        // own SIGILL handler still runs for those whenever it installs one;
        // the trap's handler stays the one the kernel calls.
        intoxicated_runtime::bionic::signal::prepare_for_raw_syscall_trap();
        let n = intoxicated_install_raw_syscall_trap(lib.base(), sites.as_ptr(), sites.len());
        println!("  [rawsys] install_raw_syscall_trap -> {n}");
    }

    // The bionic linker here maps libroblox's segments from an initial
    // PROT_READ|PROT_WRITE|PROT_EXEC reservation (linker_phdr.cpp) and never
    // downgrades the text, so the code segment stays rwx (confirmed via
    // procstat). A normally-loaded library has r-x text; an rwx executable
    // region is a W^X violation and a classic injected/tampered-code signature
    // an anti-tamper scan flags. dlopen used RTLD_NOW (eager binding) so the PLT
    // is resolved and nothing writes the text afterwards. Downgrade it to r-x.
    // `INTOXICATED_RX_TEXT=off` disables (control).
    if std::env::var("INTOXICATED_RX_TEXT").map_or(true, |v| v != "off") {
        unsafe {
            extern "C" {
                fn mprotect(a: *mut core::ffi::c_void, l: usize, p: i32) -> i32;
            }
            const PROT_READ: i32 = 1;
            const PROT_EXEC: i32 = 4;
            let page = 4096usize;
            let start = code_base & !(page - 1);
            let end = (code_base + code_size + page - 1) & !(page - 1);
            let rc = mprotect(start as *mut _, end - start, PROT_READ | PROT_EXEC);
            println!(
                "  [rx-text] mprotect({start:#x}, {} bytes) -> r-x: rc={rc}",
                end - start
            );
        }
    }

    let on_load = lib.symbol("JNI_OnLoad");
    match on_load {
        Some(address) => println!("  JNI_OnLoad {:#x}", address as usize),
        None => println!("  JNI_OnLoad not found"),
    }
    Ok(Loaded { lib, on_load })
}

/// Create the JavaVM (which registers Intoxicated's Java classes into it) and
/// call `JNI_OnLoad`. `false` when no VM could be created, which ends the run.
pub fn jni_on_load(on_load: *mut c_void) -> bool {
    let Some(vm) = linker::jni::create_vm() else {
        eprintln!("could not create a JavaVM");
        return false;
    };
    println!();
    println!("JavaVM at {vm:p}; calling JNI_OnLoad");
    match linker::jni::call_on_load(on_load) {
        Ok(rc) => println!("JNI_OnLoad returned {rc:#x} = JNI {}.{}", rc >> 16, rc & 0xffff),
        Err(e) => {
            // Not fatal: the bring-up goes on.
            println!("JNI_OnLoad failed: {e}");
            println!(
                "  the engine expects the Android bring-up around it: a VM, then \
                 GameActivity.initializeNativeCode with a real Activity (--game-activity)"
            );
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_print_in_whole_units() {
        assert_eq!(format!("{:?}", whole_units(Duration::from_micros(26_340))), "26ms");
        assert_eq!(format!("{:?}", whole_units(Duration::from_nanos(13_200))), "13µs");
        assert_eq!(format!("{:?}", whole_units(Duration::from_nanos(13_600))), "14µs");
        assert_eq!(format!("{:?}", whole_units(Duration::from_nanos(870))), "870ns");
        assert_eq!(format!("{:?}", whole_units(Duration::from_millis(1_400))), "1s");
        assert_eq!(format!("{:?}", whole_units(Duration::from_micros(999_700))), "1s");
    }

    #[test]
    fn the_code_size_line() {
        assert_eq!(megabytes(111_941_488), "106.8");
        assert_eq!(megabytes(0), "0.0");
    }
}
