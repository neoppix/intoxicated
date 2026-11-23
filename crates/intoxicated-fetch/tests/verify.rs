//! Signature verification against archives built in the test.

mod common;

use common::*;
use intoxicated_fetch::apksig::{self, Refusal, Scheme, BLOCK_V2, BLOCK_V3, BLOCK_V31};
use intoxicated_fetch::pins::Pins;
use rsa::RsaPrivateKey;

fn verify(bytes: &[u8], pins: &Pins) -> Result<apksig::Verified, Refusal> {
    let p = write_temp("t.apk", bytes);
    let r = apksig::verify_path(&p, pins);
    std::fs::remove_file(p).unwrap();
    r
}

struct Signed {
    apk: Vec<u8>,
    fingerprint: String,
}

/// A self-signed key and certificate, with an archive signed by them under
/// v2 (claiming v3) and v3.
fn self_signed_v2_v3(key: &RsaPrivateKey, alg: u32) -> Signed {
    let zip = sample_zip();
    let (d256, d512) = content_digests(&zip);
    let digest = if matches!(alg, 0x0102 | 0x0104) { d512 } else { d256 };
    let spki = spki_of(key);
    let cert = certificate(&spki, key, "Intoxicated test signer");
    let sign = |m: &[u8]| rsa_sign(key, alg, m);
    let v2 = scheme_value(&[SignerSpec {
        sdk: None,
        digests: vec![(alg, digest.clone())],
        certificate: cert.clone(),
        attributes: vec![attribute(apksig::ATTR_STRIPPING_PROTECTION, &3u32.to_le_bytes())],
        signatures: vec![(alg, &sign)],
        public_key: spki.clone(),
    }]);
    let v3 = scheme_value(&[SignerSpec {
        sdk: Some((24, 0x7fff_ffff)),
        digests: vec![(alg, digest)],
        certificate: cert.clone(),
        attributes: vec![],
        signatures: vec![(alg, &sign)],
        public_key: spki,
    }]);
    let apk = insert_block(&zip, &signing_block(&[(BLOCK_V2, v2), (BLOCK_V3, v3), (0x4272_6577, vec![0; 64])]));
    Signed { apk, fingerprint: sha256_hex(&cert) }
}

#[test]
fn own_certificate_verifies_once_pinned() {
    let key = rsa_key(2048);
    let s = self_signed_v2_v3(&key, 0x0103);
    let v = verify(&s.apk, &Pins::new([&s.fingerprint])).expect("valid signature must verify");
    assert_eq!(v.primary(), s.fingerprint);
    assert_eq!(v.signers.len(), 2);
    assert_eq!(v.signers[0].scheme, Scheme::V2);
    assert_eq!(v.signers[1].scheme, Scheme::V3);
    // Uppercase pins match too.
    assert!(verify(&s.apk, &Pins::new([s.fingerprint.to_uppercase()])).is_ok());
}

#[test]
fn unpinned_self_signed_archive_is_refused() {
    let key = rsa_key(2048);
    let s = self_signed_v2_v3(&key, 0x0103);
    match verify(&s.apk, &Pins::embedded()) {
        Err(Refusal::NotPinned { certificate, .. }) => assert_eq!(certificate, s.fingerprint),
        other => panic!("expected NotPinned, got {other:?}"),
    }
    assert!(matches!(verify(&s.apk, &Pins::default()), Err(Refusal::NotPinned { .. })));
    let wrong = "00".repeat(32);
    assert!(matches!(verify(&s.apk, &Pins::new([wrong])), Err(Refusal::NotPinned { .. })));
}

/// The regression test for the old hole: Roblox's genuine certificate, a
/// correct content digest, but a signature made with an unrelated key and
/// that key presented as the signer's public key.
#[test]
fn genuine_certificate_with_foreign_key_is_refused() {
    let zip = sample_zip();
    let (d256, _) = content_digests(&zip);
    let attacker = rsa_key(2048);
    let sign = |m: &[u8]| rsa_sign(&attacker, 0x0103, m);
    let v2 = scheme_value(&[SignerSpec {
        sdk: None,
        digests: vec![(0x0103, d256.clone())],
        certificate: ROBLOX_CERT.to_vec(),
        attributes: vec![],
        signatures: vec![(0x0103, &sign)],
        public_key: spki_of(&attacker),
    }]);
    let apk = insert_block(&zip, &signing_block(&[(BLOCK_V2, v2)]));
    match verify(&apk, &Pins::embedded()) {
        Err(Refusal::KeyMismatch { certificate, scheme: Scheme::V2 }) => assert_eq!(certificate, ORIGINAL),
        other => panic!("forgery must be refused as a key mismatch, got {other:?}"),
    }

    // Same, but presenting Roblox's real key: the signature cannot verify.
    let roblox_spki = intoxicated_fetch::der::certificate_spki(ROBLOX_CERT).unwrap().to_vec();
    let v2 = scheme_value(&[SignerSpec {
        sdk: None,
        digests: vec![(0x0103, d256)],
        certificate: ROBLOX_CERT.to_vec(),
        attributes: vec![],
        signatures: vec![(0x0103, &sign)],
        public_key: roblox_spki,
    }]);
    let apk = insert_block(&zip, &signing_block(&[(BLOCK_V2, v2)]));
    assert!(matches!(verify(&apk, &Pins::embedded()), Err(Refusal::BadSignature { .. })));
}

#[test]
fn flipped_content_byte_is_a_digest_mismatch() {
    let key = rsa_key(2048);
    let s = self_signed_v2_v3(&key, 0x0103);
    let pins = Pins::new([&s.fingerprint]);
    // A byte inside the engine entry's data.
    let mut apk = s.apk.clone();
    apk[200] ^= 0xff;
    assert!(matches!(verify(&apk, &pins), Err(Refusal::DigestMismatch { .. })), "{:?}", verify(&apk, &pins));
    // A byte inside the central directory (an entry name).
    let mut apk = s.apk.clone();
    let (cd, _) = zip_offsets(&apk);
    apk[cd + 50] ^= 0x01;
    assert!(matches!(verify(&apk, &pins), Err(Refusal::DigestMismatch { .. })));
}

#[test]
fn missing_or_damaged_signing_block_is_refused() {
    let key = rsa_key(2048);
    let s = self_signed_v2_v3(&key, 0x0103);
    let pins = Pins::new([&s.fingerprint]);

    // No block at all.
    assert_eq!(verify(&sample_zip(), &pins).unwrap_err(), Refusal::NoSigningBlock);

    // Truncated file (EOCD cut off).
    let cut = &s.apk[..s.apk.len() - 100];
    assert_eq!(verify(cut, &pins).unwrap_err(), Refusal::NoEocd);

    // Not a ZIP at all.
    assert_eq!(verify(b"just some text, certainly long enough to scan", &pins).unwrap_err(), Refusal::NoEocd);
    assert_eq!(verify(b"short", &pins).unwrap_err(), Refusal::NotZip);

    // Leading size field disagrees with the trailing one.
    let (cd, _) = zip_offsets(&s.apk);
    let size = u64::from_le_bytes(s.apk[cd - 24..cd - 16].try_into().unwrap()) as usize;
    let start = cd - size - 8;
    let mut apk = s.apk.clone();
    apk[start] ^= 1;
    assert!(matches!(verify(&apk, &pins), Err(Refusal::Malformed(_))));

    // Trailing size claims more than the file holds.
    let mut apk = s.apk.clone();
    apk[cd - 24..cd - 16].copy_from_slice(&(cd as u64 + 10).to_le_bytes());
    assert!(matches!(verify(&apk, &pins), Err(Refusal::Malformed(_))));

    // A block whose pair length runs past the block.
    let zip = sample_zip();
    let mut block = signing_block(&[(BLOCK_V2, vec![1, 2, 3, 4])]);
    block[8..16].copy_from_slice(&1000u64.to_le_bytes());
    assert!(matches!(verify(&insert_block(&zip, &block), &pins), Err(Refusal::Malformed(_))));

    // Only unknown pairs.
    let block = signing_block(&[(0x4272_6577, vec![0; 16])]);
    assert_eq!(verify(&insert_block(&zip, &block), &pins).unwrap_err(), Refusal::NoSchemeBlock);

    // Truncated scheme block contents.
    let block = signing_block(&[(BLOCK_V2, vec![0xff, 0, 0, 0, 1])]);
    assert!(matches!(verify(&insert_block(&zip, &block), &pins), Err(Refusal::Malformed(_))));
}

#[test]
fn stripped_v3_and_v31_are_refused() {
    let key = rsa_key(2048);
    let zip = sample_zip();
    let (d256, _) = content_digests(&zip);
    let spki = spki_of(&key);
    let cert = certificate(&spki, &key, "strip test");
    let pins = Pins::new([sha256_hex(&cert)]);
    let sign = |m: &[u8]| rsa_sign(&key, 0x0103, m);
    let v2 = scheme_value(&[SignerSpec {
        sdk: None,
        digests: vec![(0x0103, d256.clone())],
        certificate: cert.clone(),
        attributes: vec![attribute(apksig::ATTR_STRIPPING_PROTECTION, &3u32.to_le_bytes())],
        signatures: vec![(0x0103, &sign)],
        public_key: spki.clone(),
    }]);
    let apk = insert_block(&zip, &signing_block(&[(BLOCK_V2, v2)]));
    assert_eq!(verify(&apk, &pins).unwrap_err(), Refusal::V3Stripped { claimed: 3 });

    let v3 = scheme_value(&[SignerSpec {
        sdk: Some((24, 32)),
        digests: vec![(0x0103, d256)],
        certificate: cert,
        attributes: vec![attribute(apksig::ATTR_ROTATION_MIN_SDK, &33u32.to_le_bytes())],
        signatures: vec![(0x0103, &sign)],
        public_key: spki,
    }]);
    let apk = insert_block(&zip, &signing_block(&[(BLOCK_V3, v3)]));
    assert_eq!(verify(&apk, &pins).unwrap_err(), Refusal::V31Stripped { rotation_min_sdk: 33 });
}

/// v3.1 is verified like v3, and its certificate must be pinned as well.
#[test]
fn v31_block_is_verified_and_pinned() {
    let old = rsa_key(1024);
    let new = rsa_key(2048);
    let zip = sample_zip();
    let (d256, d512) = content_digests(&zip);
    let (old_spki, new_spki) = (spki_of(&old), spki_of(&new));
    let old_cert = certificate(&old_spki, &old, "old");
    let new_cert = certificate(&new_spki, &new, "new");
    let sign_old = |m: &[u8]| rsa_sign(&old, 0x0103, m);
    let sign_new = |m: &[u8]| rsa_sign(&new, 0x0104, m);
    let v3 = scheme_value(&[SignerSpec {
        sdk: Some((24, 32)),
        digests: vec![(0x0103, d256)],
        certificate: old_cert.clone(),
        attributes: vec![attribute(apksig::ATTR_ROTATION_MIN_SDK, &33u32.to_le_bytes())],
        signatures: vec![(0x0103, &sign_old)],
        public_key: old_spki,
    }]);
    let build = |digest: Vec<u8>| {
        let v31 = scheme_value(&[SignerSpec {
            sdk: Some((33, 0x7fff_ffff)),
            digests: vec![(0x0104, digest)],
            certificate: new_cert.clone(),
            attributes: vec![],
            signatures: vec![(0x0104, &sign_new)],
            public_key: new_spki.clone(),
        }]);
        insert_block(&zip, &signing_block(&[(BLOCK_V31, v31), (BLOCK_V3, v3.clone())]))
    };
    let (old_fp, new_fp) = (sha256_hex(&old_cert), sha256_hex(&new_cert));
    let apk = build(d512.clone());
    let v = verify(&apk, &Pins::new([&old_fp, &new_fp])).unwrap();
    assert_eq!(v.primary(), old_fp);
    assert_eq!(v.fingerprints(), [old_fp.as_str(), new_fp.as_str()]);
    match verify(&apk, &Pins::new([&old_fp])) {
        Err(Refusal::NotPinned { scheme: Scheme::V31, certificate }) => assert_eq!(certificate, new_fp),
        other => panic!("{other:?}"),
    }
    // A wrong SHA-512 digest in v3.1 alone is caught.
    let mut bad = d512;
    bad[0] ^= 1;
    assert_eq!(
        verify(&build(bad), &Pins::new([&old_fp, &new_fp])).unwrap_err(),
        Refusal::DigestMismatch { scheme: Scheme::V31, algorithm: 0x0104 }
    );
}

#[test]
fn rsa_1024_pkcs1_and_pss_all_digests() {
    // PSS with SHA-512 and a 64-byte salt needs 64 + 64 + 2 bytes of
    // modulus, more than 1024 bits give, so that one uses 2048.
    for (alg, bits) in [(0x0101, 1024), (0x0102, 2048), (0x0103, 1024), (0x0104, 1024)] {
        let key = rsa_key(bits);
        let s = self_signed_v2_v3(&key, alg);
        let v = verify(&s.apk, &Pins::new([&s.fingerprint])).unwrap_or_else(|e| panic!("{alg:#x}: {e}"));
        assert_eq!(v.signers[0].algorithms, [alg]);
    }
}

#[test]
fn unsupported_algorithms_never_pass() {
    let key = rsa_key(2048);
    let zip = sample_zip();
    let (d256, _) = content_digests(&zip);
    let spki = spki_of(&key);
    let cert = certificate(&spki, &key, "dsa");
    let junk = |_: &[u8]| vec![0u8; 64];
    let v2 = scheme_value(&[SignerSpec {
        sdk: None,
        digests: vec![(0x0301, d256)],
        certificate: cert.clone(),
        attributes: vec![],
        signatures: vec![(0x0301, &junk)],
        public_key: spki,
    }]);
    let apk = insert_block(&zip, &signing_block(&[(BLOCK_V2, v2)]));
    assert!(matches!(
        verify(&apk, &Pins::new([sha256_hex(&cert)])),
        Err(Refusal::NoSupportedSignature { .. })
    ));
}

#[test]
fn tampered_signed_data_breaks_the_signature() {
    let key = rsa_key(2048);
    let s = self_signed_v2_v3(&key, 0x0103);
    // Flip the first byte of the v2 signed data's digest value: the block is
    // not covered by the content digest, but it is covered by the signature.
    let (cd, _) = zip_offsets(&s.apk);
    let size = u64::from_le_bytes(s.apk[cd - 24..cd - 16].try_into().unwrap()) as usize;
    let start = cd - size - 8;
    // pair len (8) + id (4) + signer seq len (4) + signer len (4) + sd len (4)
    // + digests seq len (4) + digest len (4) + alg (4) + digest len (4)
    let at = start + 8 + 8 + 4 + 4 + 4 + 4 + 4 + 4 + 4 + 4;
    let mut apk = s.apk.clone();
    apk[at] ^= 1;
    assert!(matches!(
        verify(&apk, &Pins::new([&s.fingerprint])),
        Err(Refusal::BadSignature { scheme: Scheme::V2, .. })
    ));
}

#[test]
fn ecdsa_p256_signer_verifies() {
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let mut bits = vec![0u8];
    bits.extend_from_slice(pair.public_key().as_ref());
    let spki = seq(&[
        seq(&[tlv(0x06, intoxicated_fetch::der::OID_EC_PUBLIC_KEY), tlv(0x06, intoxicated_fetch::der::OID_P256)]),
        tlv(0x03, &bits),
    ]);
    // The certificate's own signature is not part of APK verification, so
    // an RSA issuer is fine here.
    let issuer = rsa_key(1024);
    let cert = certificate(&spki, &issuer, "ec");
    let zip = sample_zip();
    let (d256, _) = content_digests(&zip);
    let sign = |m: &[u8]| pair.sign(&rng, m).unwrap().as_ref().to_vec();
    let v2 = scheme_value(&[SignerSpec {
        sdk: None,
        digests: vec![(0x0201, d256)],
        certificate: cert.clone(),
        attributes: vec![],
        signatures: vec![(0x0201, &sign)],
        public_key: spki,
    }]);
    let apk = insert_block(&zip, &signing_block(&[(BLOCK_V2, v2)]));
    let v = verify(&apk, &Pins::new([sha256_hex(&cert)])).unwrap();
    assert_eq!(v.signers[0].algorithms, [0x0201]);
}

#[test]
fn engine_classification() {
    let p = write_temp("c.zip", &sample_zip());
    let r = intoxicated_fetch::zipdir::classify(&mut std::fs::File::open(&p).unwrap()).unwrap();
    std::fs::remove_file(p).unwrap();
    assert_eq!(r, (true, true));
}
