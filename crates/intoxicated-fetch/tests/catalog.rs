//! Decoding a captured APKPure app_version response (x86_64 list, captured
//! 2026-11-22, 92 554 bytes).

use intoxicated_fetch::catalog::{parse_version_list, plan};
use intoxicated_fetch::urlpolicy::{check, DOWNLOAD};
use intoxicated_fetch::wire::{decode, Payload};

const BODY: &[u8] = include_bytes!("fixtures/app_version_x86_64.bin");

#[test]
fn fixture_decodes_as_protobuf_throughout() {
    assert_eq!(BODY.len(), 92_554);
    // Every length-delimited payload either decodes as a message or is a
    // leaf; the top level must decode with no leftover bytes.
    let top = decode(BODY).unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].number, 1);
    match top[0].payload {
        Payload::Bytes(b) => assert_eq!(b.len(), 92_522),
        _ => panic!("field 1 must be length-delimited"),
    }
}

#[test]
fn fixture_lists_fifteen_apk_releases_newest_first() {
    let releases = parse_version_list(BODY).unwrap();
    assert_eq!(releases.len(), 15);
    let first = &releases[0];
    assert_eq!(first.package.as_deref(), Some("com.roblox.client"));
    assert_eq!(first.version_name.as_deref(), Some("2.748.1527"));
    assert_eq!(first.version_code, Some(22294));
    assert_eq!(first.cert_sha1.as_deref(), Some("f450e842186fe75cfeb0ad887ea95f8d98383b38"));
    assert_eq!(first.developer.as_deref(), Some("Roblox Corporation"));
    let asset = first.asset.as_ref().unwrap();
    assert_eq!(asset.size, Some(239_484_151));
    assert_eq!(asset.kind.as_deref(), Some("APK"));
    assert_eq!(asset.sha1.as_deref(), Some("319fc3e970fea6b209dca0d1c48d1d602681cc90"));
    let url = asset.url.as_deref().unwrap();
    let uri = check(url, DOWNLOAD).unwrap();
    assert_eq!(uri.host(), Some("download.pureapk.com"));
    assert!(releases.iter().all(|r| r.asset.as_ref().unwrap().kind.as_deref() == Some("APK")));

    let codes: Vec<u64> = releases.iter().map(|r| r.version_code.unwrap()).collect();
    assert_eq!(&codes[..4], &[22294, 20836, 3222, 3092]);
    assert_eq!(releases[14].version_name.as_deref(), Some("2.702.622"));
}

#[test]
fn fixture_plan_is_one_monolithic_apk() {
    let p = plan(&parse_version_list(BODY).unwrap()).unwrap();
    assert_eq!(p.version_name, "2.748.1527");
    assert_eq!(p.version_code, 22294);
    assert_eq!(p.downloads.len(), 1);
    assert!(p.downloads[0].0.starts_with("https://download.pureapk.com/b/APK/"));
}

#[test]
fn damaged_body_is_an_error_not_a_guess() {
    assert!(parse_version_list(&BODY[..BODY.len() - 7]).is_err());
    assert!(parse_version_list(b"<html>Cloudflare</html>").is_err());
    // A well-formed but empty envelope yields no releases.
    assert!(parse_version_list(&[]).unwrap().is_empty());
}
