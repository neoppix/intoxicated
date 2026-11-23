//! Everything before the engine is loaded: the descriptor limit, the
//! profile and its gates, the APK, the run directory, the identity mirrors,
//! the audio probe, the symbol table and the GL probes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::Options;
use crate::switches;

/// Raise the soft descriptor limit. Never lowers it, prints nothing and
/// cannot fail the launch.
pub fn raise_file_limit() {
    // Raise RLIMIT_NOFILE to a moderate cap. The default 1024 soft limit
    // inherited from a login shell is exhausted by the engine's cache/socket
    // fds on the X11 path (occasional EMFILE). The engine's
    // readiness polling is routed to epoll-shim (kqueue), not select(), so fds
    // >= FD_SETSIZE are fine; the cap stays modest anyway so nothing that does
    // size an array by the limit blows up. Env override: INTOXICATED_NOFILE.
    unsafe {
        #[repr(C)]
        struct Rlimit {
            cur: u64,
            max: u64,
        }
        extern "C" {
            fn getrlimit(resource: i32, rlp: *mut Rlimit) -> i32;
            fn setrlimit(resource: i32, rlp: *const Rlimit) -> i32;
        }
        const RLIMIT_NOFILE: i32 = 8; // FreeBSD <sys/resource.h>
        let want_soft: u64 = std::env::var("INTOXICATED_NOFILE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8192);
        let mut rl = Rlimit { cur: 0, max: 0 };
        if getrlimit(RLIMIT_NOFILE, &mut rl) == 0 {
            let target = want_soft.min(rl.max);
            if rl.cur < target {
                let put = Rlimit {
                    cur: target,
                    max: rl.max,
                };
                let _ = setrlimit(RLIMIT_NOFILE, &put);
            }
        }
    }
}

/// Take the profile's lock for the life of the process, or say who has it
/// and why the client is not starting. See `intoxicated_runtime::profile_lock`.
fn claim_profile(opt: &Options) -> Result<intoxicated_runtime::profile_lock::Claim, String> {
    let name = opt.profile.as_deref().unwrap_or(intoxicated_runtime::profile::DEFAULT_NAME);
    let dir = intoxicated_runtime::profile::dir(name)?;
    intoxicated_runtime::profile_lock::claim(name, &dir).map_err(|e| e.to_string())
}

/// The claim, before anything reads or writes the profile. Refused: exit 3.
pub fn claim(opt: &Options) -> Result<intoxicated_runtime::profile_lock::Claim, ExitCode> {
    claim_profile(opt).map_err(|refusal| {
        eprintln!("error: {refusal}");
        ExitCode::from(crate::cli::EXIT_PROFILE)
    })
}

/// The two settings that can refuse a profile before it is used.
pub fn profile_gates() -> Result<(), ExitCode> {
    // A profile set to `vpn-required` in its `network.json` asked not to run
    // without a VPN check. The check went with the launcher that ran it, so
    // such a profile is refused outright rather than started unprotected.
    let network = intoxicated_runtime::profile::active().join("network.json");
    if std::fs::read_to_string(&network).is_ok_and(|t| t.contains("vpn-required")) {
        eprintln!(
            "error: {} asks for vpn-required, and this client no longer runs that check. \
             Remove the file to start this profile without it.",
            network.display()
        );
        return Err(ExitCode::FAILURE);
    }

    // The session store, before anything reads a session.
    if let Err(e) = intoxicated_runtime::secrets::check_setting() {
        eprintln!("error: {e}");
        return Err(ExitCode::FAILURE);
    }
    Ok(())
}

/// What the engine is handed as its asset root.
///
/// Not the APK. The engine resolves its asset directories with
/// `boost::filesystem::canonical`, so they have to exist, and curl's
/// `CURLOPT_CAINFO` needs a real file for `ssl/cacert.pem`. Everything else
/// it reads through `AAsset*`, straight from the APK.
/// `android::assets::asset_folder` builds the directories and that one file;
/// see it for the measurement.
///
/// It points at the `content` **subdirectory**, not the folder itself. The
/// engine resolves `android/`, `ssl/` and `fonts/` against the parent of the
/// path it is given, the way the Android app hands it `.../assets/content`
/// (INHERITED, from a capture of the real app). Handing it
/// the folder itself therefore sends every one of those lookups a level too high.
///
/// If the folder cannot be prepared, this says so and hands the engine the APK
/// path, which it refuses with `'<apk>' is not a directory`: a failure that
/// shows, rather than a client that starts without its trust store.
fn engine_asset_root(apk: &str) -> String {
    match intoxicated_runtime::android::assets::asset_folder() {
        Ok(dir) => dir.join("content").to_string_lossy().into_owned(),
        Err(e) => {
            println!("  asset folder failed ({e}); using the APK path");
            apk.to_string()
        }
    }
}

/// What the steps before the load leave for the rest of the launch.
pub struct Prepared {
    /// The asset folder value: empty without `--apk`.
    pub asset_root: String,
    /// `<data root>`.
    pub data_root: PathBuf,
}

/// The graphics line, `/system`, the APK and its asset folder, the run
/// directory, the asset probe, the trace switch, the identity mirrors and the
/// audio probe, in that order.
pub fn prepare(opt: &mut Options) -> Result<Prepared, ExitCode> {
    intoxicated_runtime::graphics::report();
    // Before anything can resolve a path through the engine: it asks for
    // `/system/fonts/NotoSansCJK-Regular.ttc` during app start-up.
    intoxicated_runtime::android::system::install();

    let mut asset_root = String::new();
    if let Some(apk) = opt.apk.as_deref() {
        match intoxicated_runtime::android::assets::register_apk(std::path::Path::new(apk)) {
            Ok(()) => println!("assets: {apk}"),
            Err(e) => {
                eprintln!("bad --apk: {e}");
                return Err(ExitCode::FAILURE);
            }
        }
        // After the APK is registered (so the CA bundle can be written out) and
        // before anything asks the engine to resolve a path.
        asset_root = engine_asset_root(apk);
    }

    let data_root = switches::data_root();
    enter_run_dir(opt, &data_root);
    // After `enter_run_dir`, which made both absolute; the maps rewrite matches
    // them against the kernel's paths. See `set_maps_sources`.
    intoxicated_runtime::android::system::set_maps_sources(&opt.lib_dir, opt.apk.as_deref());

    if let Some(name) = opt.read_asset.as_deref() {
        match intoxicated_runtime::android::assets::probe(name) {
            Ok(bytes) => println!("asset {name}: {bytes} bytes"),
            Err(e) => {
                eprintln!("asset {name}: {e}");
                return Err(ExitCode::FAILURE);
            }
        }
    }

    intoxicated_runtime::android::set_trace(std::env::var_os("INTOXICATED_ANDROID_TRACE").is_some());

    // The signed-in user's identity, from `<profile>/identity` into the
    // native layer's mirrors. Touches no engine symbol, so it runs before the
    // load, and it has to: `StartAppParams` copies the mirrors once, inside
    // `nativeAppBridgeV2StartAppWithParams`, and the engine may ask for them
    // before that. The sinks and the store are in
    // `intoxicated_runtime::account`.
    intoxicated_runtime::account::listen();
    intoxicated_runtime::account::restore();

    // The audio output node and its probe, before the table is built, so the
    // log names the device before anything uses it.
    intoxicated_runtime::bionic::announce_audio_backend();

    Ok(Prepared { asset_root, data_root })
}

/// Make `--lib-dir` and `--apk` absolute, start devctl, and make
/// `<profile>/run` the working directory, with the trust store linked where
/// curl looks for it. Nothing here stops the launch.
fn enter_run_dir(opt: &mut Options, data_root: &Path) {
    let absolute = |p: &str| std::fs::canonicalize(p).map_or_else(|_| p.to_string(), |a| a.to_string_lossy().into_owned());
    opt.lib_dir = absolute(&opt.lib_dir);
    opt.apk = opt.apk.as_deref().map(absolute);

    // Here: the profile has latched and is held, and the socket lives in it.
    intoxicated_runtime::devctl::start();

    let run = intoxicated_runtime::profile::active().join("run");
    if let Err(e) = std::fs::create_dir_all(run.join("exe")) {
        println!("  could not create {}: {e}", run.display());
        return;
    }

    // The trust store, from the APK's own copy in the asset folder. Linked
    // rather than copied so a bundle rewritten for a new APK is picked up
    // without a stale duplicate.
    let ca = intoxicated_runtime::android::assets::ca_bundle();
    // curl is handed the relative `exe/cacert.pem`, which resolves against the
    // working directory or, once `nativeSetFilesDirectory` has been called,
    // against the files directory; both need the link.
    let link = |dest: PathBuf| {
        let Some(ca) = ca.as_deref() else { return };
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::read_link(&dest).ok().as_deref() != Some(ca) {
            let _ = std::fs::remove_file(&dest);
            let _ = std::os::unix::fs::symlink(ca, &dest);
        }
    };
    link(run.join("exe/cacert.pem"));
    link(data_root.join("files/exe/cacert.pem"));

    // The engine writes `./exe`, `http/`, `sounds/`, `cache/` and
    // `ContentProvider_<pid>` relative to the working directory.
    if let Err(e) = std::env::set_current_dir(&run) {
        println!("  could not enter {}: {e}", run.display());
    }
}

/// Build the symbol table and print it.
pub fn symbol_table(opt: &Options) -> intoxicated_runtime::symtab::SymbolTable {
    let table = intoxicated_runtime::symtab::build(opt.host_libc);
    let t = table.totals();
    println!(
        "symbol table: {} intoxicated, {} host, {} stub, {} total across {} libraries",
        t.intoxicated,
        t.host,
        t.stub,
        t.total(),
        table.libraries.len()
    );
    for (lib, s) in &table.stats {
        println!(
            "  {lib:<20} intoxicated={:<4} host={:<5} stub={}",
            s.intoxicated, s.host, s.stub
        );
    }
    for lib in &table.missing_host_libs {
        println!("  warning: host {lib} unavailable; its symbols are stubbed");
    }
    if opt.verbose {
        for (lib, entries) in &table.libraries {
            for e in entries {
                println!("  {lib:<20} {:<44} {}", e.symbol, e.source.label());
            }
        }
    }
    table
}

/// Two hex digits per byte, as a list.
fn pixel(bytes: [u8; 4]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("[{}]", hex.join(", "))
}

/// `--window`, then `--gl-probe`, both before the linker is initialised.
pub fn gl_probes(opt: &Options, table: &intoxicated_runtime::symtab::SymbolTable) -> Result<(), ExitCode> {
    if let Some(seconds) = opt.window {
        match intoxicated_runtime::android::gl::probe_window(table, seconds) {
            Ok(r) => {
                println!();
                println!("GL probe rendered into a real window (this is a test pattern, not Roblox):");
                println!("  renderer  {}", r.renderer);
                println!("  version   {}", r.version);
                println!("  readback  {}", pixel(r.pixel));
            }
            Err(e) => {
                eprintln!();
                eprintln!("window render failed: {e}");
                return Err(ExitCode::FAILURE);
            }
        }
    }
    if opt.gl_probe {
        match intoxicated_runtime::android::gl::probe(table) {
            Ok(r) => {
                println!();
                println!("GLES2 context is live:");
                println!("  vendor    {}", r.vendor);
                println!("  renderer  {}", r.renderer);
                println!("  version   {}", r.version);
                println!("  readback  {} - drew and read it back", pixel(r.pixel));
            }
            Err(e) => {
                eprintln!();
                eprintln!("GL probe failed: {e}");
                return Err(ExitCode::FAILURE);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_is_listed_in_lowercase_hex() {
        assert_eq!(pixel([0x33, 0x66, 0x99, 0xff]), "[33, 66, 99, ff]");
        assert_eq!(pixel([0, 1, 0xab, 0x10]), "[00, 01, ab, 10]");
    }
}
