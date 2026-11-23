//! Command-line contract checks that need no network.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_intoxicated-fetch"))
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{}-{name}", std::process::id()))
}

#[test]
fn usage_is_exit_2() {
    let out = bin().output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage:"));
    assert_eq!(bin().args(["a", "b"]).output().unwrap().status.code(), Some(2));
    assert_eq!(bin().arg("--verify").output().unwrap().status.code(), Some(2));
}

#[test]
fn non_empty_directory_is_refused_before_any_request() {
    let dir = scratch("full");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("leftover"), b"x").unwrap();
    let out = bin().arg(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already holds files"), "{stderr}");
    // Nothing was asked of the network.
    assert!(!String::from_utf8_lossy(&out.stdout).contains("asking apkpure"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn uncreatable_directory_is_exit_1() {
    let file = scratch("plainfile");
    std::fs::write(&file, b"x").unwrap();
    let out = bin().arg(file.join("sub")).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("could not create"));
    std::fs::remove_file(file).unwrap();
}

#[test]
fn verify_mode_refuses_non_apk() {
    let file = scratch("text.apk");
    std::fs::write(&file, b"this is not an archive, just a long enough line of text").unwrap();
    let out = bin().arg("--verify").arg(&file).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("refused"));
    std::fs::remove_file(file).unwrap();
}
