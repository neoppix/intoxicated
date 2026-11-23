//! Protocol Buffers wire format, read without a schema.
//!
//! Each field is a varint key (field number << 3 | wire type) followed by a
//! payload whose shape the wire type gives: 0 varint, 1 eight bytes,
//! 2 varint length then bytes, 5 four bytes. Groups (3, 4) are deprecated and
//! refused. Nested messages are simply wire-type-2 payloads decoded again.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    pub number: u32,
    pub payload: Payload<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError {
    pub offset: usize,
    pub what: &'static str,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "protobuf decode error at byte {}: {}", self.offset, self.what)
    }
}

impl std::error::Error for WireError {}

fn varint(buf: &[u8], pos: &mut usize) -> Result<u64, WireError> {
    let start = *pos;
    let mut value: u64 = 0;
    for shift in 0..10 {
        let Some(&b) = buf.get(*pos) else {
            return Err(WireError { offset: start, what: "truncated varint" });
        };
        *pos += 1;
        if shift == 9 && b > 1 {
            return Err(WireError { offset: start, what: "varint longer than 64 bits" });
        }
        value |= u64::from(b & 0x7f) << (7 * shift);
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(WireError { offset: start, what: "varint longer than 10 bytes" })
}

/// Decode every top-level field of one message. The whole buffer must be
/// consumed; trailing garbage is an error.
pub fn decode(buf: &[u8]) -> Result<Vec<Field<'_>>, WireError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let at = pos;
        let key = varint(buf, &mut pos)?;
        let number = key >> 3;
        if number == 0 || number > u64::from(u32::MAX >> 3) {
            return Err(WireError { offset: at, what: "invalid field number" });
        }
        let take = |pos: &mut usize, n: usize| -> Result<&[u8], WireError> {
            let end = pos.checked_add(n).filter(|&e| e <= buf.len()).ok_or(WireError {
                offset: at,
                what: "field runs past the end of its message",
            })?;
            let s = &buf[*pos..end];
            *pos = end;
            Ok(s)
        };
        let payload = match key & 7 {
            0 => Payload::Varint(varint(buf, &mut pos)?),
            1 => Payload::Fixed64(u64::from_le_bytes(take(&mut pos, 8)?.try_into().unwrap())),
            2 => {
                let len = varint(buf, &mut pos)?;
                let len = usize::try_from(len).map_err(|_| WireError {
                    offset: at,
                    what: "length does not fit in memory",
                })?;
                Payload::Bytes(take(&mut pos, len)?)
            }
            5 => Payload::Fixed32(u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap())),
            _ => return Err(WireError { offset: at, what: "unsupported wire type" }),
        };
        out.push(Field { number: number as u32, payload });
    }
    Ok(out)
}

/// Helpers over a decoded message.
pub trait Message<'a> {
    fn bytes_of(&self, number: u32) -> Option<&'a [u8]>;
    fn all_bytes_of(&self, number: u32) -> Vec<&'a [u8]>;
    fn varint_of(&self, number: u32) -> Option<u64>;
    fn string_of(&self, number: u32) -> Option<String> {
        self.bytes_of(number).and_then(|b| std::str::from_utf8(b).ok()).map(str::to_owned)
    }
}

impl<'a> Message<'a> for [Field<'a>] {
    fn bytes_of(&self, number: u32) -> Option<&'a [u8]> {
        self.iter().find_map(|f| match f.payload {
            Payload::Bytes(b) if f.number == number => Some(b),
            _ => None,
        })
    }

    fn all_bytes_of(&self, number: u32) -> Vec<&'a [u8]> {
        self.iter()
            .filter_map(|f| match f.payload {
                Payload::Bytes(b) if f.number == number => Some(b),
                _ => None,
            })
            .collect()
    }

    fn varint_of(&self, number: u32) -> Option<u64> {
        self.iter().find_map(|f| match f.payload {
            Payload::Varint(v) if f.number == number => Some(v),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_wire_type() {
        // field 1 varint 300, field 2 bytes "hi", field 3 fixed32, field 4 fixed64
        let buf = [
            0x08, 0xac, 0x02, 0x12, 0x02, b'h', b'i', 0x1d, 1, 0, 0, 0, 0x21, 2, 0, 0, 0, 0, 0, 0, 0,
        ];
        let f = decode(&buf).unwrap();
        assert_eq!(f[0], Field { number: 1, payload: Payload::Varint(300) });
        assert_eq!(f[1], Field { number: 2, payload: Payload::Bytes(b"hi") });
        assert_eq!(f[2], Field { number: 3, payload: Payload::Fixed32(1) });
        assert_eq!(f[3], Field { number: 4, payload: Payload::Fixed64(2) });
    }

    #[test]
    fn refuses_truncation_and_groups() {
        assert!(decode(&[0x12, 0x05, b'a']).is_err());
        assert!(decode(&[0x08]).is_err());
        assert!(decode(&[0x0b]).is_err()); // start group
        assert!(decode(&[0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]).is_err());
    }
}
