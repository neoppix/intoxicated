//! `intoxicated-run`, the program the launcher starts: Roblox's Android x86-64
//! engine loaded into a native FreeBSD process and brought up as the
//! Android app would bring it up.
//!
//! This file only sequences the steps; each lives in its module.
//!
//! 1. `--check-join-url` alone, if it is the first argument ([`cli`]).
//! 2. The banner and the descriptor limit ([`startup`]).
//! 3. The command line ([`cli`]): `--profile` is latched while it is read,
//!    before anything can ask for the profile.
//! 4. The profile's lock, before anything reads or writes the profile, then
//!    its gates ([`startup`]).
//! 5. `/system`, the APK, the run directory, the identity mirrors, the
//!    audio probe, the symbol table and the GL probes ([`startup`]).
//! 6. The linker, the load, the raw syscall trap, the read-only text and
//!    `JNI_OnLoad` ([`engine`]).
//! 7. With `--game-activity`, the bring-up and the pump ([`bringup`], with
//!    the settings delivery in [`bootstrap`] and the disk check in [`disk`]).
//! 8. The end-of-run reports and `_exit` ([`teardown`]).
//!
//! Exit statuses: 0 for a finished run, 1 for a failure the loader reports,
//! 2 for the command line, 3 for a refused profile; the pump itself ends the
//! process with 124 (the teardown watchdog), 130 or 143 (a second SIGINT or
//! SIGTERM).

use std::process::ExitCode;

mod bootstrap;
mod bringup;
mod cli;
mod disk;
mod engine;
mod startup;
mod switches;
mod teardown;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--check-join-url") {
        return cli::check_join_url();
    }
    // First line of every run: which build this is, and its licence. The full
    // copyright and licence notices are in LICENSE and NOTICE.
    println!(
        "Intoxicated {}: native FreeBSD Roblox client. GPL-3.0, see LICENSE.",
        env!("CARGO_PKG_VERSION")
    );

    startup::raise_file_limit();
    let mut opt = match cli::from_process() {
        Ok(opt) => opt,
        Err(code) => return code,
    };

    // Held until the process ends; the kernel releases it on any exit.
    let _claim = match startup::claim(&opt) {
        Ok(claim) => claim,
        Err(code) => return code,
    };
    if let Err(code) = startup::profile_gates() {
        return code;
    }

    let prepared = match startup::prepare(&mut opt) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let table = startup::symbol_table(&opt);
    if let Err(code) = startup::gl_probes(&opt, &table) {
        return code;
    }
    let loaded = match engine::load(&opt, &table) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };

    // The engine's constructors have run: from here every exit is `_exit`.
    let mut jni_ran = false;
    if let (true, Some(on_load)) = (opt.jni_onload, loaded.on_load) {
        if !engine::jni_on_load(on_load) {
            teardown::leave(1);
        }
        jni_ran = true;
        if opt.game_activity {
            bringup::run(loaded.lib, &opt, &prepared.asset_root, &prepared.data_root);
        }
    }
    teardown::finish(teardown::Ending {
        dump_classes: opt.dump_classes.as_deref().filter(|_| jni_ran),
        data_root: &prepared.data_root,
    })
}
