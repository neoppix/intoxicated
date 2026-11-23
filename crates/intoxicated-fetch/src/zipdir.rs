//! ZIP End of Central Directory lookup and central directory entry names
//! (PKWARE APPNOTE 4.3.12 and 4.3.16). No ZIP64.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

pub const EOCD_LEN: u64 = 22;
const EOCD_SIG: [u8; 4] = *b"PK\x05\x06";
const CDFH_SIG: [u8; 4] = *b"PK\x01\x02";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eocd {
    /// Offset of the EOCD record in the file.
    pub offset: u64,
    /// Central directory offset as written in the EOCD.
    pub cd_offset: u64,
    /// Central directory size as written in the EOCD.
    pub cd_size: u64,
    pub entries: u16,
    pub file_len: u64,
}

pub fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buf)
}

/// Find the EOCD: scan backwards over the last 22 + 65535 bytes for its
/// signature, accepting the first hit whose comment length reaches exactly
/// to the end of the file.
pub fn find_eocd(file: &mut File) -> io::Result<Option<Eocd>> {
    let file_len = file.metadata()?.len();
    if file_len < EOCD_LEN {
        return Ok(None);
    }
    let window = file_len.min(EOCD_LEN + 0xffff);
    let start = file_len - window;
    let mut tail = vec![0u8; window as usize];
    read_at(file, start, &mut tail)?;
    let mut i = tail.len() - EOCD_LEN as usize;
    loop {
        if tail[i..i + 4] == EOCD_SIG {
            let r = &tail[i..];
            let comment_len = u16::from_le_bytes([r[20], r[21]]) as usize;
            if i + EOCD_LEN as usize + comment_len == tail.len() {
                return Ok(Some(Eocd {
                    offset: start + i as u64,
                    entries: u16::from_le_bytes([r[10], r[11]]),
                    cd_size: u32::from_le_bytes(r[12..16].try_into().unwrap()) as u64,
                    cd_offset: u32::from_le_bytes(r[16..20].try_into().unwrap()) as u64,
                    file_len,
                }));
            }
        }
        if i == 0 {
            return Ok(None);
        }
        i -= 1;
    }
}

/// Names of every entry in the central directory.
pub fn entry_names(file: &mut File) -> io::Result<Vec<String>> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_owned());
    let eocd = find_eocd(file)?.ok_or_else(|| bad("no end-of-central-directory record"))?;
    if eocd.cd_offset.checked_add(eocd.cd_size) != Some(eocd.offset) {
        return Err(bad("central directory does not end at the end-of-central-directory record"));
    }
    let mut cd = vec![0u8; eocd.cd_size as usize];
    read_at(file, eocd.cd_offset, &mut cd)?;
    let mut names = Vec::new();
    let mut pos = 0usize;
    while pos < cd.len() {
        let h = cd.get(pos..pos + 46).ok_or_else(|| bad("truncated central directory header"))?;
        if h[..4] != CDFH_SIG {
            return Err(bad("bad central directory header signature"));
        }
        let name_len = u16::from_le_bytes([h[28], h[29]]) as usize;
        let extra_len = u16::from_le_bytes([h[30], h[31]]) as usize;
        let comment_len = u16::from_le_bytes([h[32], h[33]]) as usize;
        let name = cd
            .get(pos + 46..pos + 46 + name_len)
            .ok_or_else(|| bad("truncated central directory name"))?;
        names.push(String::from_utf8_lossy(name).into_owned());
        pos += 46 + name_len + extra_len + comment_len;
    }
    Ok(names)
}

/// Whether the file starts with a ZIP signature: "PK", then 03/05/07, then
/// 04/06/08.
pub fn has_zip_magic(head: &[u8]) -> bool {
    head.len() >= 4
        && head[0] == b'P'
        && head[1] == b'K'
        && matches!(head[2], 3 | 5 | 7)
        && matches!(head[3], 4 | 6 | 8)
}

pub const ENGINE_ENTRY: &str = "lib/x86_64/libroblox.so";

/// (carries the engine, carries assets/)
pub fn classify(file: &mut File) -> io::Result<(bool, bool)> {
    let names = entry_names(file)?;
    Ok((
        names.iter().any(|n| n == ENGINE_ENTRY),
        names.iter().any(|n| n.starts_with("assets/")),
    ))
}
