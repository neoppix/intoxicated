//! Against the genuine Roblox 2.748.1527 APK (239 484 151 bytes). Ignored by
//! default because it needs the file and writes ~240 MB copies:
//!
//!   cargo test --release -p intoxicated-fetch --test real_apk -- --ignored
//!
//! INTOXICATED_REAL_APK overrides the path.

mod common;

use common::*;
use intoxicated_fetch::apksig::{self, Refusal, Scheme, BLOCK_V2, BLOCK_V3, BLOCK_V31};
use intoxicated_fetch::pins::Pins;
use intoxicated_fetch::zipdir;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

fn real_apk() -> PathBuf {
    std::env::var_os("INTOXICATED_REAL_APK").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap()).join(".cache/intoxicated/apk/candidate-0.apk")
    })
}

/// Deletes the file when dropped.
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Copy of the genuine file whose signing block is replaced by `pairs`
/// (block start unchanged, so content digests stay valid), per spec 5.5.
fn rebuilt(pairs: &[(u32, Vec<u8>)]) -> TempFile {
    let src = real_apk();
    let mut f = File::open(&src).unwrap();
    let layout = apksig::locate(&mut f).unwrap();
    let block = signing_block(pairs);
    let out = TempFile(scratch("rebuilt.apk"));
    let mut w = File::create(&out.0).unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    std::io::copy(&mut (&mut f).take(layout.block_offset), &mut w).unwrap();
    w.write_all(&block).unwrap();
    let mut rest = Vec::new();
    f.seek(SeekFrom::Start(layout.eocd.cd_offset)).unwrap();
    f.read_to_end(&mut rest).unwrap();
    let e = (layout.eocd.offset - layout.eocd.cd_offset) as usize;
    let new_cd = (layout.block_offset + block.len() as u64) as u32;
    rest[e + 16..e + 20].copy_from_slice(&new_cd.to_le_bytes());
    w.write_all(&rest).unwrap();
    out
}

fn genuine_pairs() -> Vec<(u32, Vec<u8>)> {
    apksig::locate(&mut File::open(real_apk()).unwrap()).unwrap().pairs
}

fn verify(p: &Path, pins: &Pins) -> Result<apksig::Verified, Refusal> {
    apksig::verify_path(p, pins)
}

#[test]
#[ignore]
fn genuine_build_is_accepted_with_both_roblox_certificates() {
    let apk = real_apk();
    assert_eq!(std::fs::metadata(&apk).unwrap().len(), 239_484_151);
    let v = verify(&apk, &Pins::embedded()).unwrap();
    assert_eq!(v.primary(), ORIGINAL);
    assert_eq!(v.fingerprints(), [ORIGINAL, ROTATED]);
    let schemes: Vec<Scheme> = v.signers.iter().map(|s| s.scheme).collect();
    assert_eq!(schemes, [Scheme::V2, Scheme::V3, Scheme::V31]);
    assert_eq!(v.signers[0].certificate_sha1, "f450e842186fe75cfeb0ad887ea95f8d98383b38");
    assert_eq!(v.signers[2].certificate_sha1, "75f3dc66294bbd4e8dc9fa399536060adde225ee");
    assert_eq!(v.signers[1].sdk, Some((24, 32)));
    assert_eq!(v.signers[2].sdk, Some((33, 2_147_483_647)));
    assert_eq!(v.signers[2].algorithms, [0x0104]);

    // Uppercase pins.
    assert!(verify(&apk, &Pins::new([ORIGINAL.to_uppercase(), ROTATED.to_uppercase()])).is_ok());
    // Each certificate involved must be pinned.
    assert!(matches!(verify(&apk, &Pins::new([ORIGINAL])), Err(Refusal::NotPinned { scheme: Scheme::V31, .. })));
    assert!(matches!(verify(&apk, &Pins::new([ROTATED])), Err(Refusal::NotPinned { scheme: Scheme::V2, .. })));
    assert!(matches!(verify(&apk, &Pins::default()), Err(Refusal::NotPinned { .. })));
    assert!(matches!(verify(&apk, &Pins::new(["ab".repeat(32)])), Err(Refusal::NotPinned { .. })));
}

#[test]
#[ignore]
fn genuine_layout_and_digests_match_the_measurements() {
    let mut f = File::open(real_apk()).unwrap();
    let l = apksig::locate(&mut f).unwrap();
    assert_eq!(l.eocd.offset, 239_484_129);
    assert_eq!(l.eocd.cd_offset, 239_267_840);
    assert_eq!(l.block_offset, 239_255_552);
    assert_eq!(l.block_len, 12_288);
    let ids: Vec<u32> = l.pairs.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [BLOCK_V2, BLOCK_V31, BLOCK_V3, 0x6dff_800d, 0x4272_6577]);
    let lens: Vec<usize> = l.pairs.iter().map(|(_, v)| v.len()).collect();
    assert_eq!(lens, [1009, 4933, 1021, 3057, 2176]);
    let (d256, d512) = apksig::content_digests(&mut f, l.block_offset, &l.eocd, true, true).unwrap();
    assert_eq!(
        intoxicated_fetch::hex(&d256.unwrap()),
        "16b4216341244fc235dad3232a65c79641b84b6306704adaa9c397c786c34298"
    );
    assert_eq!(
        intoxicated_fetch::hex(&d512.unwrap()),
        "cbd2a7355a87e0aee1d280553c050aa4b8d221a247bca635f07515218385c419162365667729a01a743a93b12be56b9511b57cb2c1cecc3aeb90100f45ec07eb"
    );
    let names = zipdir::entry_names(&mut f).unwrap();
    assert_eq!(names.len(), 2274);
    assert_eq!(names.iter().filter(|n| n.starts_with("assets/")).count(), 605);
    assert!(names.iter().any(|n| n == zipdir::ENGINE_ENTRY));
}

#[test]
#[ignore]
fn genuine_build_with_one_flipped_byte_is_refused() {
    let len = std::fs::metadata(real_apk()).unwrap().len();
    let copy = TempFile(scratch("flipped.apk"));
    std::fs::copy(real_apk(), &copy.0).unwrap();
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&copy.0).unwrap();
    let at = len / 3;
    assert_eq!(at, 79_828_050);
    let mut b = [0u8];
    f.seek(SeekFrom::Start(at)).unwrap();
    f.read_exact(&mut b).unwrap();
    f.seek(SeekFrom::Start(at)).unwrap();
    f.write_all(&[b[0] ^ 0xff]).unwrap();
    drop(f);
    assert!(matches!(verify(&copy.0, &Pins::embedded()), Err(Refusal::DigestMismatch { .. })));
}

#[test]
#[ignore]
fn genuine_build_truncated_is_refused() {
    let len = std::fs::metadata(real_apk()).unwrap().len();
    let copy = TempFile(scratch("truncated.apk"));
    std::fs::copy(real_apk(), &copy.0).unwrap();
    std::fs::OpenOptions::new().write(true).open(&copy.0).unwrap().set_len(len - 100).unwrap();
    assert_eq!(verify(&copy.0, &Pins::embedded()).unwrap_err(), Refusal::NoEocd);
}

#[test]
#[ignore]
fn rebuilt_blocks_behave_as_specified() {
    let pairs = genuine_pairs();
    let keep = |drop: &[u32]| pairs.iter().filter(|(id, _)| !drop.contains(id)).cloned().collect::<Vec<_>>();

    // Unchanged pairs, rebuilt block: still accepted (the rebuild is sound).
    let same = rebuilt(&pairs);
    assert_eq!(verify(&same.0, &Pins::embedded()).unwrap().primary(), ORIGINAL);

    // v3 and v3.1 removed: v2 says the signer also applied v3.
    let v2_only = rebuilt(&keep(&[BLOCK_V3, BLOCK_V31]));
    assert_eq!(verify(&v2_only.0, &Pins::embedded()).unwrap_err(), Refusal::V3Stripped { claimed: 3 });

    // Only v3.1 removed: v3 announces rotation from SDK 33.
    let no_v31 = rebuilt(&keep(&[BLOCK_V31]));
    assert_eq!(verify(&no_v31.0, &Pins::embedded()).unwrap_err(), Refusal::V31Stripped { rotation_min_sdk: 33 });

    // No signing block at all.
    let none = rebuilt(&[]);
    assert_eq!(verify(&none.0, &Pins::embedded()).unwrap_err(), Refusal::NoSchemeBlock);
}

/// Spec 5.5's forgery, on the genuine file: a v2-only block with Roblox's
/// certificate, the correct digest, and a signature by an unrelated
/// RSA-2048 key presented as the signer's key. The old verifier accepted it.
#[test]
#[ignore]
fn forgery_on_the_genuine_file_is_refused() {
    let attacker = rsa_key(2048);
    let digest =
        hex_decode("16b4216341244fc235dad3232a65c79641b84b6306704adaa9c397c786c34298");
    let sign = |m: &[u8]| rsa_sign(&attacker, 0x0103, m);
    let v2 = scheme_value(&[SignerSpec {
        sdk: None,
        digests: vec![(0x0103, digest)],
        certificate: ROBLOX_CERT.to_vec(),
        attributes: vec![],
        signatures: vec![(0x0103, &sign)],
        public_key: spki_of(&attacker),
    }]);
    let forged = rebuilt(&[(BLOCK_V2, v2)]);
    match verify(&forged.0, &Pins::embedded()) {
        Err(Refusal::KeyMismatch { scheme: Scheme::V2, certificate }) => assert_eq!(certificate, ORIGINAL),
        other => panic!("forgery must be refused, got {other:?}"),
    }
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
