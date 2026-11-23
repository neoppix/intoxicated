//! Builders for test archives: a tiny stored ZIP, X.509 certificates, and
//! v2 / v3 signing blocks, all written from the AOSP format description.
#![allow(dead_code)]

use intoxicated_fetch::apksig;
use intoxicated_fetch::zipdir;
use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Sign, Pss, RsaPrivateKey};
use sha2::{Digest, Sha256, Sha512};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const ORIGINAL: &str = "44932ea35a17a267372d71b54d1a0cb3da0dca5113e94406ae2fe18090ba1477";
pub const ROTATED: &str = "2bebd189e8d3106401347056c93d045b61e20e22d0c3cbed85474aeb00a3d12a";
pub const ROBLOX_CERT: &[u8] = include_bytes!("../fixtures/roblox-original-cert.der");

pub fn scratch(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{}-{n}-{name}", std::process::id()))
}

pub fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
    let p = scratch(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

pub fn rsa_key(bits: usize) -> RsaPrivateKey {
    RsaPrivateKey::new(&mut rand_core::OsRng, bits).unwrap()
}

pub fn spki_of(key: &RsaPrivateKey) -> Vec<u8> {
    key.to_public_key().to_public_key_der().unwrap().as_bytes().to_vec()
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A stored (uncompressed) ZIP of the given entries.
pub fn tiny_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let mut common = Vec::new();
        common.extend_from_slice(&10u16.to_le_bytes()); // version needed
        common.extend_from_slice(&0u16.to_le_bytes()); // flags
        common.extend_from_slice(&0u16.to_le_bytes()); // stored
        common.extend_from_slice(&0u32.to_le_bytes()); // time, date
        common.extend_from_slice(&crc.to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&common);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&20u16.to_le_bytes()); // version made by
        cd.extend_from_slice(&common);
        cd.extend_from_slice(&0u16.to_le_bytes()); // comment len
        cd.extend_from_slice(&0u16.to_le_bytes()); // disk
        cd.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        cd.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        cd.extend_from_slice(&offset.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub fn sample_zip() -> Vec<u8> {
    let engine: Vec<u8> = (0..5000u32).flat_map(|i| i.to_le_bytes()).collect();
    tiny_zip(&[("lib/x86_64/libroblox.so", &engine), ("assets/hello.txt", b"hello from the test archive")])
}

/// (central directory offset, EOCD offset) of a ZIP held in memory.
pub fn zip_offsets(zip: &[u8]) -> (usize, usize) {
    let eocd = zip.len() - 22;
    assert_eq!(&zip[eocd..eocd + 4], b"PK\x05\x06");
    let cd = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    (cd, eocd)
}

/// Chunked content digests of an unsigned ZIP (the block will be inserted
/// at the central directory offset, so that is the block offset).
pub fn content_digests(zip: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let p = write_temp("digest.zip", zip);
    let mut f = File::open(&p).unwrap();
    let eocd = zipdir::find_eocd(&mut f).unwrap().unwrap();
    let (a, b) = apksig::content_digests(&mut f, eocd.cd_offset, &eocd, true, true).unwrap();
    std::fs::remove_file(p).unwrap();
    (a.unwrap(), b.unwrap())
}

pub fn lp(b: &[u8]) -> Vec<u8> {
    let mut v = (b.len() as u32).to_le_bytes().to_vec();
    v.extend_from_slice(b);
    v
}

pub fn lp_seq(items: &[Vec<u8>]) -> Vec<u8> {
    lp(&items.iter().flat_map(|i| lp(i)).collect::<Vec<u8>>())
}

pub fn record(id: u32, bytes: &[u8]) -> Vec<u8> {
    let mut v = id.to_le_bytes().to_vec();
    v.extend_from_slice(&lp(bytes));
    v
}

pub fn attribute(id: u32, value: &[u8]) -> Vec<u8> {
    let mut v = id.to_le_bytes().to_vec();
    v.extend_from_slice(value);
    v
}

/// Sign `msg` with algorithm `alg` (0x0101..0x0104).
pub fn rsa_sign(key: &RsaPrivateKey, alg: u32, msg: &[u8]) -> Vec<u8> {
    let mut rng = rand_core::OsRng;
    match alg {
        0x0103 => key.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(msg)).unwrap(),
        0x0104 => key.sign(Pkcs1v15Sign::new::<Sha512>(), &Sha512::digest(msg)).unwrap(),
        0x0101 => key.sign_with_rng(&mut rng, Pss::new_with_salt::<Sha256>(32), &Sha256::digest(msg)).unwrap(),
        0x0102 => key.sign_with_rng(&mut rng, Pss::new_with_salt::<Sha512>(64), &Sha512::digest(msg)).unwrap(),
        _ => panic!("no such test algorithm"),
    }
}

/// Everything one signer record needs.
pub struct SignerSpec<'a> {
    pub sdk: Option<(u32, u32)>,
    /// (algorithm, content digest) pairs placed in signed data.
    pub digests: Vec<(u32, Vec<u8>)>,
    pub certificate: Vec<u8>,
    pub attributes: Vec<Vec<u8>>,
    /// (algorithm, signer function over signed data).
    pub signatures: Vec<(u32, &'a dyn Fn(&[u8]) -> Vec<u8>)>,
    pub public_key: Vec<u8>,
}

pub fn scheme_value(signers: &[SignerSpec]) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = signers
        .iter()
        .map(|s| {
            let mut sd = lp_seq(&s.digests.iter().map(|(a, d)| record(*a, d)).collect::<Vec<_>>());
            sd.extend_from_slice(&lp_seq(&[s.certificate.clone()]));
            if let Some((lo, hi)) = s.sdk {
                sd.extend_from_slice(&lo.to_le_bytes());
                sd.extend_from_slice(&hi.to_le_bytes());
            }
            sd.extend_from_slice(&lp_seq(&s.attributes));
            let sigs: Vec<Vec<u8>> = s.signatures.iter().map(|(a, f)| record(*a, &f(&sd))).collect();
            let mut signer = lp(&sd);
            if let Some((lo, hi)) = s.sdk {
                signer.extend_from_slice(&lo.to_le_bytes());
                signer.extend_from_slice(&hi.to_le_bytes());
            }
            signer.extend_from_slice(&lp_seq(&sigs));
            signer.extend_from_slice(&lp(&s.public_key));
            signer
        })
        .collect();
    lp_seq(&encoded)
}

/// The signing block: u64 size, pairs, u64 size, magic.
pub fn signing_block(pairs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, value) in pairs {
        body.extend_from_slice(&((value.len() + 4) as u64).to_le_bytes());
        body.extend_from_slice(&id.to_le_bytes());
        body.extend_from_slice(value);
    }
    let size = (body.len() + 24) as u64;
    let mut out = size.to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(apksig::MAGIC);
    out
}

/// Insert a signing block before the central directory of an unsigned
/// ZIP and fix the EOCD's central directory offset.
pub fn insert_block(zip: &[u8], block: &[u8]) -> Vec<u8> {
    let (cd, eocd) = zip_offsets(zip);
    let mut out = zip[..cd].to_vec();
    out.extend_from_slice(block);
    out.extend_from_slice(&zip[cd..]);
    let new_cd = (cd + block.len()) as u32;
    let e = eocd + block.len();
    out[e + 16..e + 20].copy_from_slice(&new_cd.to_le_bytes());
    out
}

// ---- DER and X.509 ----

pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = n.to_be_bytes().iter().copied().skip_while(|&b| b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend_from_slice(&bytes);
    }
    out.extend_from_slice(content);
    out
}

pub fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn name(cn: &str) -> Vec<u8> {
    let atv = seq(&[tlv(0x06, &[0x55, 0x04, 0x03]), tlv(0x0c, cn.as_bytes())]);
    seq(&[tlv(0x31, &atv)])
}

/// An X.509 v3 certificate for `spki`, signed by `issuer_key` with
/// sha256WithRSAEncryption. With the subject's own key it is self-signed.
pub fn certificate(spki: &[u8], issuer_key: &RsaPrivateKey, cn: &str) -> Vec<u8> {
    let sha256_rsa = seq(&[
        tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]),
        tlv(0x05, &[]),
    ]);
    let tbs = seq(&[
        tlv(0xa0, &tlv(0x02, &[2])),
        tlv(0x02, &[0x01, 0x23, 0x45]),
        sha256_rsa.clone(),
        name(cn),
        seq(&[tlv(0x17, b"260101000000Z"), tlv(0x17, b"360101000000Z")]),
        name(cn),
        spki.to_vec(),
    ]);
    let sig = issuer_key.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(&tbs)).unwrap();
    let mut bits = vec![0u8];
    bits.extend_from_slice(&sig);
    seq(&[tbs, sha256_rsa, tlv(0x03, &bits)])
}

pub fn sha256_hex(b: &[u8]) -> String {
    apksig::sha256_hex(b)
}
