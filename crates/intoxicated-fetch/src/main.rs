//! intoxicated-fetch: download the newest Roblox Android build from APKPure
//! into a fresh directory and accept it only if its APK signature verifies
//! against a pinned Roblox certificate.
//!
//!   intoxicated-fetch <directory>      fetch into <directory>
//!   intoxicated-fetch --verify <apk>   verify an existing file only
//!
//! Exit status: 0 accepted, 1 any failure, 2 usage.

use intoxicated_fetch::apksig::{self, Verified};
use intoxicated_fetch::catalog::{self, Plan};
use intoxicated_fetch::net;
use intoxicated_fetch::pins::Pins;
use intoxicated_fetch::zipdir;
use std::fs::{self, File};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: intoxicated-fetch <directory to download into>\n       intoxicated-fetch --verify <apk>";

enum Failure {
    /// Anything that is not a signature verdict.
    Other(String),
    /// The signature check refused the build.
    Refused(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, apk] if flag == "--verify" => verify_only(Path::new(apk)),
        [dir] if !dir.starts_with("--") => match fetch(Path::new(dir)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(Failure::Other(m)) => {
                eprintln!("\n{m}");
                ExitCode::from(1)
            }
            Err(Failure::Refused(m)) => {
                eprintln!("\nthe downloaded build was refused: {m}");
                ExitCode::from(1)
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn print_verified(v: &Verified) {
    for s in &v.signers {
        let sdk = s.sdk.map(|(lo, hi)| format!(", sdk {lo}..{hi}")).unwrap_or_default();
        let algs: Vec<String> = s.algorithms.iter().map(|a| format!("{a:#06x}")).collect();
        println!(
            "  {:<4} signer sha256 {} (sha1 {}, algorithm {}{sdk})",
            s.scheme.name(),
            s.certificate_sha256,
            s.certificate_sha1,
            algs.join(",")
        );
    }
    println!("signed by {}", v.primary());
}

fn verify_only(apk: &Path) -> ExitCode {
    match apksig::verify_path(apk, &Pins::load()) {
        Ok(v) => {
            println!("accepted: {}", apk.display());
            print_verified(&v);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("refused: {}: {e}", apk.display());
            ExitCode::from(1)
        }
    }
}

/// Removes every file it tracks unless disarmed: no partial build is left
/// behind on any failure path.
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = fs::remove_file(p);
        }
    }
}

fn progress_printer(name: String) -> impl FnMut(u64, Option<u64>) {
    let tty = std::io::stderr().is_terminal();
    let mut finished = false;
    move |done, total| {
        let mib = |b: u64| b as f64 / 1048576.0;
        let line = match total {
            Some(t) if t > 0 => {
                format!("{name}: {:.1} / {:.1} MiB ({:.0}%)", mib(done), mib(t), done as f64 * 100.0 / t as f64)
            }
            _ => format!("{name}: {:.1} MiB", mib(done)),
        };
        if tty {
            eprint!("\r{line}   ");
            if !finished && total == Some(done) {
                eprintln!();
                finished = true;
            }
        } else if !finished && total == Some(done) {
            // Not a terminal: no running progress, only the final line.
            eprintln!("{line}");
            finished = true;
        }
        let _ = std::io::stderr().flush();
    }
}

fn fetch(dir: &Path) -> Result<(), Failure> {
    fs::create_dir_all(dir).map_err(|e| Failure::Other(format!("could not create {}: {e}", dir.display())))?;
    let mut entries = fs::read_dir(dir).map_err(|e| Failure::Other(format!("could not read {}: {e}", dir.display())))?;
    if entries.next().is_some() {
        return Err(Failure::Other(format!(
            "{} already holds files; refusing to download into a directory that is not empty",
            dir.display()
        )));
    }

    println!("asking apkpure for the newest {} build", catalog::PACKAGE);
    let body = net::fetch_metadata().map_err(Failure::Other)?;
    let releases = catalog::parse_version_list(&body)
        .map_err(|e| Failure::Other(format!("the version list from {} is not understood: {e}", net::METADATA_URL)))?;
    let plan: Plan = catalog::plan(&releases).map_err(|e| Failure::Other(e.to_string()))?;
    for (url, _) in &plan.downloads {
        intoxicated_fetch::urlpolicy::check(url, intoxicated_fetch::urlpolicy::DOWNLOAD)
            .map_err(|e| Failure::Other(e.to_string()))?;
    }
    println!(
        "newest: {} (version code {}), {} archive(s)",
        plan.version_name,
        plan.version_code,
        plan.downloads.len()
    );

    let mut cleanup = Cleanup(Vec::new());
    let mut landed: Vec<(PathBuf, u64, bool, bool)> = Vec::new();
    for (i, (url, asset)) in plan.downloads.iter().enumerate() {
        let partial = dir.join(format!("candidate-{i}.partial"));
        let target = dir.join(format!("candidate-{i}.apk"));
        cleanup.0.push(partial.clone());
        cleanup.0.push(target.clone());
        let name = format!("candidate-{i}.apk");
        let mut announced = false;
        let mut show = progress_printer(name.clone());
        let got = net::download(url, &partial, &mut |done, total| {
            if !announced {
                println!("{name}: {done}/{} bytes", total.map_or("?".into(), |t| t.to_string()));
                announced = true;
            }
            show(done, total);
        })
        .map_err(Failure::Other)?;
        if let Some(size) = asset.size.filter(|&s| s != got.bytes) {
            return Err(Failure::Other(format!(
                "{name}: received {} bytes but the mirror listed {size}",
                got.bytes
            )));
        }
        if let Some(sha1) = asset.sha1.as_deref().filter(|s| !s.eq_ignore_ascii_case(&got.sha1)) {
            return Err(Failure::Other(format!(
                "{name}: SHA-1 is {} but the mirror listed {sha1}",
                got.sha1
            )));
        }
        fs::rename(&partial, &target)
            .map_err(|e| Failure::Other(format!("could not rename {} to {}: {e}", partial.display(), target.display())))?;
        let (engine, assets) = File::open(&target)
            .and_then(|mut f| zipdir::classify(&mut f))
            .map_err(|e| Failure::Refused(format!("{}: {e}", target.display())))?;
        landed.push((target, got.bytes, engine, assets));
        if landed.iter().any(|l| l.2) && landed.iter().any(|l| l.3) {
            break;
        }
    }
    if !landed.iter().any(|l| l.2) {
        return Err(Failure::Other(format!(
            "the mirror served {} archive(s) for version {} and none carries {}",
            landed.len(),
            plan.version_name,
            zipdir::ENGINE_ENTRY
        )));
    }

    let pins = Pins::load();
    let mut signer: Option<String> = None;
    let mut reports = Vec::new();
    for (path, _, _, _) in &landed {
        println!("verifying {}", path.file_name().unwrap().to_string_lossy());
        let v = apksig::verify_path(path, &pins).map_err(|e| Failure::Refused(format!("{}: {e}", path.display())))?;
        let primary = v.primary().to_owned();
        match &signer {
            Some(s) if *s != primary => {
                return Err(Failure::Refused(format!(
                    "the archives are signed by different certificates ({s} and {primary})"
                )))
            }
            _ => signer = Some(primary),
        }
        reports.push(v);
    }

    cleanup.0.clear(); // accepted: keep the files
    println!(
        "accepted {} (version code {}) from apkpure",
        plan.version_name, plan.version_code
    );
    print_verified(&reports[0]);
    for (path, bytes, _, _) in &landed {
        println!("  {} {bytes} bytes", path.display());
    }
    Ok(())
}
