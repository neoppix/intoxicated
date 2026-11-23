//! APK Signature Scheme v2, v3 and v3.1 verification.
//!
//! Written from the AOSP documentation ("APK Signature Scheme v2", "v3" and
//! "v3.1" on source.android.com). For every scheme block present, every
//! signer must pass all of these links, in order:
//!
//! 1. each signature with a supported algorithm verifies over the signer's
//!    signed data with the signer's own public key (and at least one does);
//! 2. the digest and signature lists name the same algorithms;
//! 3. the signer's public key is byte-identical to the SubjectPublicKeyInfo
//!    of the first certificate in the signed data;
//! 4. (v3, v3.1) the SDK range in the signed data equals the signer's;
//! 5. the chunked content digest recomputed from the file equals the signed
//!    digest for every verified algorithm;
//! 6. the SHA-256 of the first certificate is pinned.
//!
//! Across blocks: a v2 or v3 block must exist; a v2 stripping-protection
//! attribute naming a higher scheme than present is refused; a v3 signer
//! announcing a rotation SDK (so a v3.1 block was written) without a v3.1
//! block is refused.

use crate::der;
use crate::pins::Pins;
use crate::zipdir::{self, Eocd};
use sha2::{Digest, Sha256, Sha512};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

pub const BLOCK_V2: u32 = 0x7109_871a;
pub const BLOCK_V3: u32 = 0xf053_68c0;
pub const BLOCK_V31: u32 = 0x1b93_ad61;

/// v2 signed-data attribute: highest scheme the signer also applied.
pub const ATTR_STRIPPING_PROTECTION: u32 = 0xbeef_f00d;
/// v3 signed-data attribute: SDK from which a v3.1 block takes over.
pub const ATTR_ROTATION_MIN_SDK: u32 = 0x559f_8b02;

pub const MAGIC: &[u8; 16] = b"APK Sig Block 42";
pub const MAX_BLOCK: u64 = 64 << 20;
pub const CHUNK: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scheme {
    V2,
    V3,
    V31,
}

impl Scheme {
    pub fn name(self) -> &'static str {
        match self {
            Scheme::V2 => "v2",
            Scheme::V3 => "v3",
            Scheme::V31 => "v3.1",
        }
    }
    fn block_id(self) -> u32 {
        match self {
            Scheme::V2 => BLOCK_V2,
            Scheme::V3 => BLOCK_V3,
            Scheme::V31 => BLOCK_V31,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentHash {
    Sha256,
    Sha512,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    RsaPss(ContentHash),
    RsaPkcs1(ContentHash),
    EcdsaSha256,
}

/// Signature algorithm IDs this verifier implements. Anything else
/// (ECDSA with SHA-512, DSA, unknown) is skipped, never counted as a pass.
fn method_for(alg: u32) -> Option<Method> {
    match alg {
        0x0101 => Some(Method::RsaPss(ContentHash::Sha256)),
        0x0102 => Some(Method::RsaPss(ContentHash::Sha512)),
        0x0103 => Some(Method::RsaPkcs1(ContentHash::Sha256)),
        0x0104 => Some(Method::RsaPkcs1(ContentHash::Sha512)),
        0x0201 => Some(Method::EcdsaSha256),
        _ => None,
    }
}

fn content_hash_of(m: Method) -> ContentHash {
    match m {
        Method::RsaPss(h) | Method::RsaPkcs1(h) => h,
        Method::EcdsaSha256 => ContentHash::Sha256,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Io(String),
    NotZip,
    NoEocd,
    CentralDirectory(String),
    NoSigningBlock,
    Malformed(String),
    NoSchemeBlock,
    NoSupportedSignature { scheme: Scheme, algorithms: Vec<u32> },
    BadSignature { scheme: Scheme, algorithm: u32, why: String },
    KeyMismatch { scheme: Scheme, certificate: String },
    DigestMismatch { scheme: Scheme, algorithm: u32 },
    V3Stripped { claimed: u32 },
    V31Stripped { rotation_min_sdk: u32 },
    NotPinned { scheme: Scheme, certificate: String },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Io(e) => write!(f, "could not read the archive: {e}"),
            Refusal::NotZip => write!(f, "the file is not a ZIP archive, so not an APK"),
            Refusal::NoEocd => write!(f, "no end-of-central-directory record; the archive is truncated or not a ZIP"),
            Refusal::CentralDirectory(m) => write!(f, "the ZIP central directory is inconsistent: {m}"),
            Refusal::NoSigningBlock => write!(
                f,
                "the archive has no APK Signing Block (unsigned or JAR-signed only); v1 signatures are not accepted"
            ),
            Refusal::Malformed(m) => write!(f, "the APK Signing Block is malformed: {m}"),
            Refusal::NoSchemeBlock => write!(f, "the APK Signing Block holds no v2 or v3 signature"),
            Refusal::NoSupportedSignature { scheme, algorithms } => write!(
                f,
                "the {} signer uses no supported signature algorithm (offered: {})",
                scheme.name(),
                algorithms.iter().map(|a| format!("{a:#06x}")).collect::<Vec<_>>().join(", ")
            ),
            Refusal::BadSignature { scheme, algorithm, why } => write!(
                f,
                "the {} signature (algorithm {algorithm:#06x}) does not verify with the signer's public key: {why}",
                scheme.name()
            ),
            Refusal::KeyMismatch { scheme, certificate } => write!(
                f,
                "the {} signer's public key is not the key in its certificate (certificate SHA-256 {certificate}); someone attached a certificate they do not hold the key for",
                scheme.name()
            ),
            Refusal::DigestMismatch { scheme, algorithm } => write!(
                f,
                "the archive contents do not match the signed {} digest (algorithm {algorithm:#06x}); the file was modified after signing",
                scheme.name()
            ),
            Refusal::V3Stripped { claimed } => write!(
                f,
                "the v2 signature says the signer also applied scheme v{claimed}, but that block is missing; it was stripped"
            ),
            Refusal::V31Stripped { rotation_min_sdk } => write!(
                f,
                "the v3 signer announces key rotation from SDK {rotation_min_sdk}, but the v3.1 block is missing; it was stripped"
            ),
            Refusal::NotPinned { scheme, certificate } => write!(
                f,
                "the {} signature is valid but its certificate (SHA-256 {certificate}) is not a pinned Roblox certificate",
                scheme.name()
            ),
        }
    }
}

impl std::error::Error for Refusal {}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Refusal::Io(e.to_string())
    }
}

/// Where the signing block sits, and its ID-value pairs in file order.
#[derive(Debug, Clone)]
pub struct Layout {
    pub eocd: Eocd,
    pub block_offset: u64,
    pub block_len: u64,
    pub pairs: Vec<(u32, Vec<u8>)>,
}

fn malformed<T>(m: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::Malformed(m.into()))
}

/// Locate and split the APK Signing Block.
pub fn locate(file: &mut File) -> Result<Layout, Refusal> {
    let len = file.metadata()?.len();
    if len < zipdir::EOCD_LEN {
        return Err(Refusal::NotZip);
    }
    let eocd = zipdir::find_eocd(file)?.ok_or(Refusal::NoEocd)?;
    let cd = eocd.cd_offset;
    if cd >= eocd.offset {
        return Err(Refusal::CentralDirectory("central directory offset is not before the end record".into()));
    }
    if cd + eocd.cd_size != eocd.offset {
        return Err(Refusal::CentralDirectory("central directory does not end at the end record".into()));
    }
    if cd < 32 {
        return Err(Refusal::NoSigningBlock);
    }
    let mut footer = [0u8; 24];
    zipdir::read_at(file, cd - 24, &mut footer)?;
    if &footer[8..] != MAGIC {
        return Err(Refusal::NoSigningBlock);
    }
    let size = u64::from_le_bytes(footer[..8].try_into().unwrap());
    if size < 24 || size > MAX_BLOCK {
        return malformed(format!("block size {size} is out of range"));
    }
    if size + 8 > cd {
        return malformed("block claims to start before the beginning of the file");
    }
    let block_offset = cd - size - 8;
    let mut block = vec![0u8; (size + 8) as usize];
    zipdir::read_at(file, block_offset, &mut block)?;
    if u64::from_le_bytes(block[..8].try_into().unwrap()) != size {
        return malformed("leading and trailing block sizes disagree");
    }
    let mut rest = &block[8..block.len() - 24];
    let mut pairs = Vec::new();
    while !rest.is_empty() {
        if rest.len() < 8 {
            return malformed("truncated ID-value pair length");
        }
        let n = u64::from_le_bytes(rest[..8].try_into().unwrap());
        if n < 4 || n > (rest.len() - 8) as u64 {
            return malformed(format!("ID-value pair length {n} is out of range"));
        }
        let n = n as usize;
        let id = u32::from_le_bytes(rest[8..12].try_into().unwrap());
        pairs.push((id, rest[12..8 + n].to_vec()));
        rest = &rest[8 + n..];
    }
    for s in [Scheme::V2, Scheme::V3, Scheme::V31] {
        if pairs.iter().filter(|(id, _)| *id == s.block_id()).count() > 1 {
            return malformed(format!("more than one {} block", s.name()));
        }
    }
    Ok(Layout { eocd, block_offset, block_len: size + 8, pairs })
}

/// Cursor over little-endian, u32-length-prefixed structures.
struct Cur<'a>(&'a [u8], &'static str);

impl<'a> Cur<'a> {
    fn u32(&mut self) -> Result<u32, Refusal> {
        if self.0.len() < 4 {
            return malformed(format!("truncated integer in {}", self.1));
        }
        let v = u32::from_le_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        Ok(v)
    }
    fn prefixed(&mut self) -> Result<&'a [u8], Refusal> {
        let n = self.u32()? as usize;
        if n > self.0.len() {
            return malformed(format!("length prefix runs past the end of {}", self.1));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn sequence(&mut self) -> Result<Vec<&'a [u8]>, Refusal> {
        let what = self.1;
        let mut inner = Cur(self.prefixed()?, what);
        let mut out = Vec::new();
        while !inner.0.is_empty() {
            out.push(inner.prefixed()?);
        }
        Ok(out)
    }
}

/// (algorithm ID, bytes) records: digests and signatures share this shape.
fn records<'a>(items: Vec<&'a [u8]>, what: &'static str) -> Result<Vec<(u32, &'a [u8])>, Refusal> {
    items
        .into_iter()
        .map(|item| {
            let mut c = Cur(item, what);
            Ok((c.u32()?, c.prefixed()?))
        })
        .collect()
}

struct SignedData<'a> {
    digests: Vec<(u32, &'a [u8])>,
    certificates: Vec<&'a [u8]>,
    sdk: Option<(u32, u32)>,
    attributes: Vec<(u32, &'a [u8])>,
}

struct Signer<'a> {
    signed_data: &'a [u8],
    parsed: SignedData<'a>,
    sdk: Option<(u32, u32)>,
    signatures: Vec<(u32, &'a [u8])>,
    public_key: &'a [u8],
}

fn parse_signers(scheme: Scheme, value: &[u8]) -> Result<Vec<Signer<'_>>, Refusal> {
    let with_sdk = scheme != Scheme::V2;
    let mut outer = Cur(value, "the signer list");
    let signers = outer.sequence()?;
    if !outer.0.is_empty() {
        return malformed(format!("trailing bytes after the {} signer list", scheme.name()));
    }
    if signers.is_empty() {
        return malformed(format!("the {} block has no signers", scheme.name()));
    }
    signers
        .into_iter()
        .map(|raw| {
            let mut c = Cur(raw, "a signer");
            let signed_data = c.prefixed()?;
            let sdk = if with_sdk { Some((c.u32()?, c.u32()?)) } else { None };
            let signatures = records(c.sequence()?, "a signature record")?;
            let public_key = c.prefixed()?;

            let mut d = Cur(signed_data, "signed data");
            let digests = records(d.sequence()?, "a digest record")?;
            let certificates = d.sequence()?;
            let inner_sdk = if with_sdk { Some((d.u32()?, d.u32()?)) } else { None };
            let attributes = d
                .sequence()?
                .into_iter()
                .map(|a| {
                    let mut ac = Cur(a, "an attribute");
                    Ok((ac.u32()?, ac.0))
                })
                .collect::<Result<Vec<_>, Refusal>>()?;
            Ok(Signer {
                signed_data,
                parsed: SignedData { digests, certificates, sdk: inner_sdk, attributes },
                sdk,
                signatures,
                public_key,
            })
        })
        .collect()
}

fn check_signature(method: Method, spki: &[u8], message: &[u8], signature: &[u8]) -> Result<(), String> {
    match method {
        Method::RsaPss(h) | Method::RsaPkcs1(h) => {
            use rsa::pkcs8::DecodePublicKey;
            let key = rsa::RsaPublicKey::from_public_key_der(spki).map_err(|e| format!("not an RSA public key ({e})"))?;
            let result = match (method, h) {
                (Method::RsaPkcs1(_), ContentHash::Sha256) => {
                    key.verify(rsa::Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(message), signature)
                }
                (Method::RsaPkcs1(_), ContentHash::Sha512) => {
                    key.verify(rsa::Pkcs1v15Sign::new::<Sha512>(), &Sha512::digest(message), signature)
                }
                (_, ContentHash::Sha256) => {
                    key.verify(rsa::Pss::new_with_salt::<Sha256>(32), &Sha256::digest(message), signature)
                }
                (_, ContentHash::Sha512) => {
                    key.verify(rsa::Pss::new_with_salt::<Sha512>(64), &Sha512::digest(message), signature)
                }
            };
            result.map_err(|e| e.to_string())
        }
        Method::EcdsaSha256 => {
            use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1, ECDSA_P384_SHA256_ASN1};
            let (oid, params, point) = der::spki_parts(spki).ok_or("unparseable public key")?;
            if oid != der::OID_EC_PUBLIC_KEY {
                return Err("not an EC public key".into());
            }
            let curve = der::read(params).map(|(t, _)| t.content);
            let alg = match curve {
                Some(c) if c == der::OID_P256 => &ECDSA_P256_SHA256_ASN1,
                Some(c) if c == der::OID_P384 => &ECDSA_P384_SHA256_ASN1,
                _ => return Err("unsupported elliptic curve".into()),
            };
            UnparsedPublicKey::new(alg, point).verify(message, signature).map_err(|_| "bad signature".into())
        }
    }
}

pub fn sha256_hex(b: &[u8]) -> String {
    crate::hex(&Sha256::digest(b))
}

/// What one signer turned out to be, after its checks passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerReport {
    pub scheme: Scheme,
    pub certificate_sha256: String,
    pub certificate_sha1: String,
    pub algorithms: Vec<u32>,
    pub sdk: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub signers: Vec<SignerReport>,
}

impl Verified {
    /// The certificate that names the signer: the first v3 signer's, else
    /// the first v2 signer's.
    pub fn primary(&self) -> &str {
        self.signers
            .iter()
            .find(|s| s.scheme == Scheme::V3)
            .or_else(|| self.signers.iter().find(|s| s.scheme == Scheme::V2))
            .map(|s| s.certificate_sha256.as_str())
            .unwrap_or("")
    }

    /// Distinct certificate fingerprints, in report order.
    pub fn fingerprints(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in &self.signers {
            if !out.contains(&s.certificate_sha256.as_str()) {
                out.push(&s.certificate_sha256);
            }
        }
        out
    }
}

/// Chunked content digests (AOSP v2 "APK integrity protection") over the
/// three covered regions, with the EOCD's central directory offset replaced
/// by the signing block's offset. Computes only the requested hashes, in
/// one pass over the file.
pub fn content_digests(
    file: &mut File,
    block_offset: u64,
    eocd: &Eocd,
    want256: bool,
    want512: bool,
) -> io::Result<(Option<Vec<u8>>, Option<Vec<u8>>)> {
    let mut tail = vec![0u8; (eocd.file_len - eocd.offset) as usize];
    zipdir::read_at(file, eocd.offset, &mut tail)?;
    let patched = u32::try_from(block_offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "signing block offset beyond 4 GiB"))?;
    tail[16..20].copy_from_slice(&patched.to_le_bytes());

    let mut chunks256: Vec<u8> = Vec::new();
    let mut chunks512: Vec<u8> = Vec::new();
    let mut count: u32 = 0;
    let mut absorb = |chunk: &[u8]| {
        let len = (chunk.len() as u32).to_le_bytes();
        if want256 {
            let mut h = Sha256::new();
            h.update([0xa5]);
            h.update(len);
            h.update(chunk);
            chunks256.extend_from_slice(&h.finalize());
        }
        if want512 {
            let mut h = Sha512::new();
            h.update([0xa5]);
            h.update(len);
            h.update(chunk);
            chunks512.extend_from_slice(&h.finalize());
        }
        count += 1;
    };

    let mut buf = vec![0u8; CHUNK];
    for (start, end) in [(0, block_offset), (eocd.cd_offset, eocd.offset)] {
        file.seek(SeekFrom::Start(start))?;
        let mut left = end - start;
        while left > 0 {
            let n = left.min(CHUNK as u64) as usize;
            file.read_exact(&mut buf[..n])?;
            absorb(&buf[..n]);
            left -= n as u64;
        }
    }
    for chunk in tail.chunks(CHUNK) {
        absorb(chunk);
    }

    let top = |chunks: &[u8], sha512: bool| -> Vec<u8> {
        let head = [&[0x5a][..], &count.to_le_bytes()[..]];
        if sha512 {
            let mut h = Sha512::new();
            head.iter().for_each(|p| h.update(p));
            h.update(chunks);
            h.finalize().to_vec()
        } else {
            let mut h = Sha256::new();
            head.iter().for_each(|p| h.update(p));
            h.update(chunks);
            h.finalize().to_vec()
        }
    };
    Ok((want256.then(|| top(&chunks256, false)), want512.then(|| top(&chunks512, true))))
}

/// Verify an APK on disk against the pinned set.
pub fn verify_path(path: &Path, pins: &Pins) -> Result<Verified, Refusal> {
    let mut file = File::open(path).map_err(|e| Refusal::Io(format!("{}: {e}", path.display())))?;
    verify_file(&mut file, pins)
}

pub fn verify_file(file: &mut File, pins: &Pins) -> Result<Verified, Refusal> {
    let layout = locate(file)?;
    let mut head = [0u8; 4];
    zipdir::read_at(file, 0, &mut head)?;
    if !zipdir::has_zip_magic(&head) {
        return Err(Refusal::NotZip);
    }

    let block = |s: Scheme| layout.pairs.iter().find(|(id, _)| *id == s.block_id()).map(|(_, v)| v.as_slice());
    let present: Vec<(Scheme, &[u8])> =
        [Scheme::V2, Scheme::V3, Scheme::V31].into_iter().filter_map(|s| block(s).map(|v| (s, v))).collect();
    if !present.iter().any(|(s, _)| matches!(s, Scheme::V2 | Scheme::V3)) {
        return Err(Refusal::NoSchemeBlock);
    }

    let mut reports = Vec::new();
    // (scheme, algorithm, expected digest) for every verified signature.
    let mut expected: Vec<(Scheme, u32, Vec<u8>)> = Vec::new();
    let mut stripping_claim: Option<u32> = None;
    let mut rotation_min_sdk: Option<u32> = None;

    for &(scheme, value) in &present {
        for signer in parse_signers(scheme, value)? {
            // Link 1: signatures, with the signer's own key.
            let mut verified_algs = Vec::new();
            for &(alg, sig) in &signer.signatures {
                let Some(method) = method_for(alg) else { continue };
                check_signature(method, signer.public_key, signer.signed_data, sig).map_err(|why| {
                    Refusal::BadSignature { scheme, algorithm: alg, why }
                })?;
                verified_algs.push(alg);
            }
            if verified_algs.is_empty() {
                return Err(Refusal::NoSupportedSignature {
                    scheme,
                    algorithms: signer.signatures.iter().map(|(a, _)| *a).collect(),
                });
            }
            let sd = &signer.parsed;

            // Link 2: digests and signatures name the same algorithms.
            let sig_ids: Vec<u32> = signer.signatures.iter().map(|(a, _)| *a).collect();
            let dig_ids: Vec<u32> = sd.digests.iter().map(|(a, _)| *a).collect();
            if sig_ids != dig_ids {
                return malformed(format!(
                    "{} signature algorithms {sig_ids:x?} differ from digest algorithms {dig_ids:x?}",
                    scheme.name()
                ));
            }

            // Link 3: the key belongs to the first certificate.
            let cert = *sd
                .certificates
                .first()
                .ok_or_else(|| Refusal::Malformed(format!("{} signer has no certificate", scheme.name())))?;
            let cert_sha256 = sha256_hex(cert);
            let spki = der::certificate_spki(cert)
                .ok_or_else(|| Refusal::Malformed(format!("{} certificate is not parseable X.509", scheme.name())))?;
            if spki != signer.public_key {
                return Err(Refusal::KeyMismatch { scheme, certificate: cert_sha256 });
            }

            // Link 4: SDK ranges agree (v3, v3.1).
            if signer.sdk != sd.sdk {
                return malformed(format!("{} signer SDK range differs from its signed data", scheme.name()));
            }
            if let Some((lo, hi)) = signer.sdk {
                if lo > hi {
                    return malformed(format!("{} signer has min SDK {lo} above max SDK {hi}", scheme.name()));
                }
            }

            for &(alg, value) in &sd.attributes {
                let as_u32 = || value.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
                match (scheme, alg) {
                    (Scheme::V2, ATTR_STRIPPING_PROTECTION) => {
                        let v = as_u32().ok_or_else(|| Refusal::Malformed("short stripping attribute".into()))?;
                        stripping_claim = Some(stripping_claim.map_or(v, |c| c.max(v)));
                    }
                    (Scheme::V3, ATTR_ROTATION_MIN_SDK) => {
                        let v = as_u32().ok_or_else(|| Refusal::Malformed("short rotation attribute".into()))?;
                        rotation_min_sdk = Some(v);
                    }
                    _ => {}
                }
            }

            for &alg in &verified_algs {
                let digest = sd.digests.iter().find(|(a, _)| *a == alg).map(|(_, d)| d.to_vec()).unwrap();
                expected.push((scheme, alg, digest));
            }
            reports.push(SignerReport {
                scheme,
                certificate_sha1: crate::hex(&<sha1::Sha1 as Digest>::digest(cert)),
                certificate_sha256: cert_sha256,
                algorithms: verified_algs,
                sdk: signer.sdk,
            });
        }
    }

    // Cross-block stripping checks.
    if block(Scheme::V3).is_none() {
        if let Some(claimed) = stripping_claim.filter(|&c| c > 2) {
            return Err(Refusal::V3Stripped { claimed });
        }
    }
    if block(Scheme::V31).is_none() {
        if let Some(sdk) = rotation_min_sdk {
            return Err(Refusal::V31Stripped { rotation_min_sdk: sdk });
        }
    }

    // Link 5: contents.
    let want = |h: ContentHash| expected.iter().any(|(_, a, _)| content_hash_of(method_for(*a).unwrap()) == h);
    let (d256, d512) =
        content_digests(file, layout.block_offset, &layout.eocd, want(ContentHash::Sha256), want(ContentHash::Sha512))?;
    for (scheme, alg, digest) in &expected {
        let actual = match content_hash_of(method_for(*alg).unwrap()) {
            ContentHash::Sha256 => d256.as_deref(),
            ContentHash::Sha512 => d512.as_deref(),
        };
        if actual != Some(digest.as_slice()) {
            return Err(Refusal::DigestMismatch { scheme: *scheme, algorithm: *alg });
        }
    }

    // Link 6: pins.
    for r in &reports {
        if !pins.contains(&r.certificate_sha256) {
            return Err(Refusal::NotPinned { scheme: r.scheme, certificate: r.certificate_sha256.clone() });
        }
    }
    Ok(Verified { signers: reports })
}
