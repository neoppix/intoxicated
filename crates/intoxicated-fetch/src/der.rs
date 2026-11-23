//! Just enough DER to find a certificate's SubjectPublicKeyInfo and to read
//! the algorithm out of a SubjectPublicKeyInfo. Single-byte tags, definite
//! lengths only.

pub const SEQUENCE: u8 = 0x30;
pub const OID: u8 = 0x06;
pub const BIT_STRING: u8 = 0x03;

/// One TLV: its tag, its content, and the full encoding including header.
#[derive(Debug, Clone, Copy)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub whole: &'a [u8],
}

/// Read one TLV from the front of `buf`; returns it and the rest.
pub fn read(buf: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *buf.first()?;
    if tag & 0x1f == 0x1f {
        return None; // multi-byte tag numbers are not used here
    }
    let first = *buf.get(1)?;
    let (len, header) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let bytes = buf.get(2..2 + n)?;
        if bytes[0] == 0 {
            return None; // not minimal
        }
        let len = bytes.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize);
        if len < 0x80 {
            return None; // not minimal
        }
        (len, 2 + n)
    };
    let end = header.checked_add(len)?;
    let whole = buf.get(..end)?;
    Some((Tlv { tag, content: &whole[header..], whole }, &buf[end..]))
}

fn expect(buf: &[u8], tag: u8) -> Option<(Tlv<'_>, &[u8])> {
    read(buf).filter(|(t, _)| t.tag == tag)
}

/// The full DER encoding of the SubjectPublicKeyInfo inside an X.509
/// certificate.
///
/// Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
/// TBSCertificate ::= SEQUENCE { [0] version OPTIONAL, serialNumber,
///   signature, issuer, validity, subject, subjectPublicKeyInfo, ... }
pub fn certificate_spki(cert: &[u8]) -> Option<&[u8]> {
    let (outer, rest) = expect(cert, SEQUENCE)?;
    if !rest.is_empty() {
        return None;
    }
    let (tbs, _) = expect(outer.content, SEQUENCE)?;
    let mut cur = tbs.content;
    if cur.first() == Some(&0xa0) {
        cur = read(cur)?.1;
    }
    for _ in 0..5 {
        cur = read(cur)?.1; // serial, signature, issuer, validity, subject
    }
    let (spki, _) = expect(cur, SEQUENCE)?;
    Some(spki.whole)
}

/// Algorithm OID content bytes, parameters (whole TLV, may be empty) and the
/// key bits of a SubjectPublicKeyInfo.
pub fn spki_parts(spki: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let (outer, rest) = expect(spki, SEQUENCE)?;
    if !rest.is_empty() {
        return None;
    }
    let (alg, after_alg) = expect(outer.content, SEQUENCE)?;
    let (oid, params) = expect(alg.content, OID)?;
    let (bits, tail) = expect(after_alg, BIT_STRING)?;
    if !tail.is_empty() || bits.content.first() != Some(&0) {
        return None;
    }
    Some((oid.content, params, &bits.content[1..]))
}

pub const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
pub const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
pub const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
pub const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
