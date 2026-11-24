//! Finds the raw `syscall` instructions in `libroblox.so`.
//!
//! The engine's anticheat issues Linux syscalls straight from its own code with
//! the `syscall` instruction instead of going through libc. On FreeBSD those
//! would run against the FreeBSD syscall table, so `native/syscall_trap.c`
//! rewrites each one to `ud2` at load and emulates it from the SIGILL handler.
//! That needs the address of every site, and every Roblox update moves them.
//!
//! This module produces that list from the file itself. It reads the ELF
//! section headers, takes every section marked executable (`SHF_EXECINSTR`),
//! and decodes it front to back with a real x86-64 decoder (iced-x86), keeping
//! the address of each decoded `SYSCALL`. Searching the bytes for `0f 05` is not
//! an option: that pair also turns up inside immediates, displacements and
//! the tails of longer instructions, and patching one of those would corrupt
//! the instruction it belongs to.
//!
//! The scan takes a fraction of a second on a release build, but it still runs
//! once per Roblox build only: the result is cached under the file's GNU
//! build-id in `$XDG_CACHE_HOME/intoxicated/syscall-sites/<build-id>.txt`.

use std::fs::File;
use std::io::{self, Write as _};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced_x86::{Code, Decoder, DecoderOptions, Instruction};

/// Names the decoder in the cache header. A cache written by a different
/// decoder release is rescanned rather than trusted. Keep in step with the
/// `iced-x86` version in Cargo.toml.
pub const DECODER: &str = "iced-x86-1.22";

/// First word of a cache file; also versions the file layout.
const CACHE_MAGIC: &str = "intoxicated-syscall-sites-v1";

const SHF_EXECINSTR: u64 = 0x4;
const SHT_NOTE: u32 = 7;
const SHT_NOBITS: u32 = 8;
const NT_GNU_BUILD_ID: u32 = 3;

/// Where a site list came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Scanned,
    Cache,
}

/// The raw syscall sites of one library.
#[derive(Clone, Debug)]
pub struct Discovery {
    /// GNU build-id as lowercase hex, if the file carries one.
    pub build_id: Option<String>,
    /// Virtual address of every `syscall` instruction, ascending. For a shared
    /// object whose first segment is at vaddr 0 this is the offset from the
    /// load base.
    pub sites: Vec<u64>,
    pub source: Source,
    /// Time spent producing `sites` (reading the cache, or scanning).
    pub elapsed: Duration,
}

/// One section of the file that holds code.
#[derive(Clone, Copy, Debug)]
struct ExecSection {
    addr: u64,
    offset: u64,
    size: u64,
}

struct ElfInfo {
    build_id: Option<String>,
    exec: Vec<ExecSection>,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn read_exact_at(f: &File, len: u64, off: u64) -> io::Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| bad("section too large"))?;
    let mut buf = vec![0u8; len];
    f.read_exact_at(&mut buf, off)?;
    Ok(buf)
}

/// Reads the section table and the build-id note. Only the headers and the
/// note sections are read here; code is read later, and only on a cache miss.
fn read_elf(f: &File) -> io::Result<ElfInfo> {
    let mut eh = [0u8; 64];
    f.read_exact_at(&mut eh, 0)?;
    if &eh[..4] != b"\x7fELF" {
        return Err(bad("not an ELF file"));
    }
    if eh[4] != 2 || eh[5] != 1 {
        return Err(bad("not a little-endian 64-bit ELF"));
    }
    if u16_at(&eh, 0x12) != 62 {
        return Err(bad("not an x86-64 ELF"));
    }
    let shoff = u64_at(&eh, 0x28);
    let shentsize = u16_at(&eh, 0x3a) as u64;
    let shnum = u16_at(&eh, 0x3c) as u64;
    if shoff == 0 || shnum == 0 {
        return Err(bad("no section headers"));
    }
    if shentsize < 64 {
        return Err(bad("section header entries too small"));
    }
    let table = read_exact_at(f, shentsize * shnum, shoff)?;

    let mut info = ElfInfo { build_id: None, exec: Vec::new() };
    for i in 0..shnum as usize {
        let sh = &table[i * shentsize as usize..][..64];
        let kind = u32_at(sh, 4);
        let flags = u64_at(sh, 8);
        let addr = u64_at(sh, 0x10);
        let offset = u64_at(sh, 0x18);
        let size = u64_at(sh, 0x20);
        if kind == SHT_NOBITS || size == 0 {
            continue;
        }
        if flags & SHF_EXECINSTR != 0 {
            info.exec.push(ExecSection { addr, offset, size });
        }
        if kind == SHT_NOTE && info.build_id.is_none() && size <= 1 << 16 {
            info.build_id = gnu_build_id(&read_exact_at(f, size, offset)?);
        }
    }
    Ok(info)
}

/// Walks the notes in one SHT_NOTE section for NT_GNU_BUILD_ID.
fn gnu_build_id(notes: &[u8]) -> Option<String> {
    let align4 = |n: usize| (n + 3) & !3;
    let mut at = 0usize;
    while at + 12 <= notes.len() {
        let namesz = u32_at(notes, at) as usize;
        let descsz = u32_at(notes, at + 4) as usize;
        let kind = u32_at(notes, at + 8);
        let name_at = at + 12;
        let desc_at = name_at.checked_add(align4(namesz))?;
        let end = desc_at.checked_add(align4(descsz))?;
        if desc_at + descsz > notes.len() {
            return None;
        }
        if kind == NT_GNU_BUILD_ID && &notes[name_at..name_at + namesz] == b"GNU\0" && descsz > 0 {
            let desc = &notes[desc_at..desc_at + descsz];
            return Some(desc.iter().map(|b| format!("{b:02x}")).collect());
        }
        at = end;
    }
    None
}

/// Decodes `code` (loaded at `addr`) instruction by instruction and returns
/// the address of every SYSCALL. Undecodable bytes are stepped over the way
/// the decoder reports them, the same linear sweep `objdump -d` does.
pub fn syscalls_in(code: &[u8], addr: u64) -> Vec<u64> {
    let mut decoder = Decoder::with_ip(64, code, addr, DecoderOptions::NONE);
    let mut insn = Instruction::default();
    let mut sites = Vec::new();
    while decoder.can_decode() {
        decoder.decode_out(&mut insn);
        if insn.code() == Code::Syscall {
            sites.push(insn.ip());
        }
    }
    sites
}

fn scan(f: &File, exec: &[ExecSection]) -> io::Result<Vec<u64>> {
    let mut sites = Vec::new();
    for s in exec {
        let code = read_exact_at(f, s.size, s.offset)?;
        sites.extend(syscalls_in(&code, s.addr));
    }
    sites.sort_unstable();
    sites.dedup();
    Ok(sites)
}

/// `$XDG_CACHE_HOME/intoxicated/syscall-sites`, or `~/.cache/...` when the
/// variable is unset, empty or not absolute (the XDG spec says to ignore a
/// relative value).
pub fn default_cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".cache"))
        })?;
    Some(base.join("intoxicated").join("syscall-sites"))
}

fn cache_path(dir: &Path, build_id: &str) -> PathBuf {
    dir.join(format!("{build_id}.txt"))
}

fn header(build_id: &str, count: usize) -> String {
    format!("{CACHE_MAGIC} build-id={build_id} decoder={DECODER} count={count}")
}

/// Parses a cache file. `None` for anything that is not exactly what
/// `write_cache` would have written for this build and this decoder.
fn parse_cache(text: &str, build_id: &str) -> Option<Vec<u64>> {
    let mut lines = text.lines();
    let head = lines.next()?;
    let mut words = head.split(' ');
    if words.next()? != CACHE_MAGIC
        || words.next()? != format!("build-id={build_id}")
        || words.next()? != format!("decoder={DECODER}")
    {
        return None;
    }
    let count: usize = words.next()?.strip_prefix("count=")?.parse().ok()?;
    if words.next().is_some() {
        return None;
    }
    let mut sites = Vec::with_capacity(count);
    for line in lines {
        let hex = line.strip_prefix("0x")?;
        sites.push(u64::from_str_radix(hex, 16).ok()?);
    }
    let ascending = sites.windows(2).all(|w| w[0] < w[1]);
    (sites.len() == count && ascending).then_some(sites)
}

/// Writes the cache through a temporary file in the same directory and a
/// rename, so a reader sees either the old file or the complete new one.
/// Clients on two profiles share the cache and may both scan a new build at
/// once; each write has a temporary name of its own (process id and a
/// counter), so neither can truncate the other's before its rename.
fn write_cache(dir: &Path, build_id: &str, sites: &[u64]) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static WRITES: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(dir)?;
    let mut text = header(build_id, sites.len());
    text.push('\n');
    for s in sites {
        text.push_str(&format!("{s:#x}\n"));
    }
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{build_id}.txt.{}.{n}.tmp", std::process::id()));
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, cache_path(dir, build_id))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Finds the raw syscall sites of the library at `lib`. With a `cache_dir`
/// and a build-id, a valid cache entry is used instead of scanning, and a scan
/// result is stored for next time. A cache that cannot be read back as this
/// build's list is ignored and overwritten. Failing to write the cache is not
/// an error: the sites are still returned.
pub fn discover(lib: &Path, cache_dir: Option<&Path>) -> io::Result<Discovery> {
    let start = Instant::now();
    let f = File::open(lib)?;
    let elf = read_elf(&f)?;
    if elf.exec.is_empty() {
        return Err(bad("no executable section"));
    }
    let cached = match (cache_dir, &elf.build_id) {
        (Some(dir), Some(id)) => std::fs::read_to_string(cache_path(dir, id))
            .ok()
            .and_then(|t| parse_cache(&t, id)),
        _ => None,
    };
    if let Some(sites) = cached {
        return Ok(Discovery {
            build_id: elf.build_id,
            sites,
            source: Source::Cache,
            elapsed: start.elapsed(),
        });
    }
    let sites = scan(&f, &elf.exec)?;
    let elapsed = start.elapsed();
    if let (Some(dir), Some(id)) = (cache_dir, &elf.build_id) {
        if let Err(e) = write_cache(dir, id, &sites) {
            eprintln!("[rawsys] could not write {}: {e}", cache_path(dir, id).display());
        }
    }
    Ok(Discovery { build_id: elf.build_id, sites, source: Source::Scanned, elapsed })
}

/// Whether some other build cached in `dir` had at least one site. Used to
/// tell "this build has none" from "the scan stopped finding them".
fn other_build_had_sites(dir: &Path, build_id: Option<&str>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| {
        let name = e.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".txt")) else {
            return false;
        };
        if Some(id) == build_id || id.starts_with('.') {
            return false;
        }
        std::fs::read_to_string(e.path())
            .ok()
            .and_then(|t| parse_cache(&t, id))
            .is_some_and(|s| !s.is_empty())
    })
}

/// The site list for the load path: discovers, logs, and on any failure
/// prints a warning and returns an empty list so the client still starts.
/// Skips the work under `INTOXICATED_SYSCALL_TRAP=off`, where the trap installer
/// does nothing with the list anyway.
pub fn for_load(lib: &Path) -> Vec<u64> {
    if std::env::var("INTOXICATED_SYSCALL_TRAP").is_ok_and(|v| v == "off") {
        return Vec::new();
    }
    let warn = |why: &str| {
        eprintln!(
            "[rawsys] WARNING: {why}; raw syscalls are NOT neutralised, \
             anticheat kicks (304) likely"
        );
    };
    let cache_dir = default_cache_dir();
    let found = match discover(lib, cache_dir.as_deref()) {
        Ok(found) => found,
        Err(e) => {
            warn(&format!("cannot find raw syscall sites in {}: {e}", lib.display()));
            return Vec::new();
        }
    };
    let id = found.build_id.as_deref();
    let prefix = id.map_or("<no build-id>", |id| &id[..id.len().min(12)]);
    let how = match found.source {
        Source::Scanned => format!("scanned in {} ms", found.elapsed.as_millis()),
        Source::Cache => "from cache".to_owned(),
    };
    eprintln!("[rawsys] {} raw syscall sites in {prefix} ({how})", found.sites.len());
    if found.sites.is_empty() && cache_dir.is_some_and(|d| other_build_had_sites(&d, id)) {
        warn("0 raw syscall sites found, but a previous build had some");
    }
    found.sites
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal x86-64 ELF: null section, `.text` (executable) holding
    /// `code` at `addr`, and a `.note.gnu.build-id` holding `id`.
    fn tiny_elf(code: &[u8], addr: u64, id: &[u8]) -> Vec<u8> {
        let mut note = Vec::new();
        note.extend_from_slice(&4u32.to_le_bytes());
        note.extend_from_slice(&(id.len() as u32).to_le_bytes());
        note.extend_from_slice(&NT_GNU_BUILD_ID.to_le_bytes());
        note.extend_from_slice(b"GNU\0");
        note.extend_from_slice(id);
        while note.len() % 4 != 0 {
            note.push(0);
        }

        let code_off = 64u64;
        let note_off = code_off + code.len() as u64;
        let shoff = (note_off + note.len() as u64 + 7) & !7;

        let mut f = vec![0u8; 64];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = 2;
        f[5] = 1;
        f[6] = 1;
        f[0x10..0x12].copy_from_slice(&3u16.to_le_bytes());
        f[0x12..0x14].copy_from_slice(&62u16.to_le_bytes());
        f[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
        f[0x34..0x36].copy_from_slice(&64u16.to_le_bytes());
        f[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        f[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        f.extend_from_slice(code);
        f.extend_from_slice(&note);
        f.resize(shoff as usize, 0);

        let section = |kind: u32, flags: u64, addr: u64, off: u64, size: u64| {
            let mut s = vec![0u8; 64];
            s[4..8].copy_from_slice(&kind.to_le_bytes());
            s[8..16].copy_from_slice(&flags.to_le_bytes());
            s[0x10..0x18].copy_from_slice(&addr.to_le_bytes());
            s[0x18..0x20].copy_from_slice(&off.to_le_bytes());
            s[0x20..0x28].copy_from_slice(&size.to_le_bytes());
            s
        };
        f.extend(vec![0u8; 64]);
        f.extend(section(1, 0x2 | SHF_EXECINSTR, addr, code_off, code.len() as u64));
        f.extend(section(SHT_NOTE, 0x2, 0, note_off, note.len() as u64));
        f
    }

    /// `mov eax, 0x50f` (b8 0f 05 00 00) carries the syscall bytes in its
    /// immediate; `lea rax, [rip+0x50f]` (48 8d 05 0f 05 00 00) in its
    /// displacement. Only the real `syscall` after them is a site.
    const DECOY_CODE: &[u8] = &[
        0xb8, 0x0f, 0x05, 0x00, 0x00, // mov eax, 0x50f
        0x48, 0x8d, 0x05, 0x0f, 0x05, 0x00, 0x00, // lea rax, [rip+0x50f]
        0x0f, 0x05, // syscall
        0xc3, // ret
    ];

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("intoxicated-syscall-sites-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn syscall_bytes_inside_other_instructions_are_not_sites() {
        let naive = DECOY_CODE.windows(2).filter(|w| w == &[0x0f, 0x05]).count();
        assert_eq!(naive, 3, "the decoys must really contain 0f 05");
        assert_eq!(syscalls_in(DECOY_CODE, 0x1000), vec![0x100c]);
    }

    #[test]
    fn discovers_sites_and_build_id_in_an_elf() {
        let dir = scratch("elf");
        let lib = dir.join("lib.so");
        std::fs::write(&lib, tiny_elf(DECOY_CODE, 0x4000, &[0xde, 0xad, 0xbe, 0xef])).unwrap();
        let found = discover(&lib, None).unwrap();
        assert_eq!(found.build_id.as_deref(), Some("deadbeef"));
        assert_eq!(found.sites, vec![0x400c]);
        assert_eq!(found.source, Source::Scanned);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clients_scanning_one_build_at_once_leave_a_valid_cache() {
        // Two profiles starting together on a new build: both scan and both
        // write. Every writer and every reader must see a whole list.
        let dir = scratch("race");
        let cache = dir.join("cache");
        let lib = dir.join("lib.so");
        std::fs::write(&lib, tiny_elf(DECOY_CODE, 0x4000, &[0xab, 0xcd])).unwrap();
        let sites: Vec<u64> = (0..20_000u64).map(|i| 0x4000 + i * 16).collect();
        std::thread::scope(|s| {
            for _ in 0..6 {
                s.spawn(|| {
                    for _ in 0..5 {
                        write_cache(&cache, "abcd", &sites).unwrap();
                        let text = std::fs::read_to_string(cache_path(&cache, "abcd")).unwrap();
                        assert_eq!(parse_cache(&text, "abcd").as_deref(), Some(&sites[..]));
                    }
                });
            }
        });
        let left: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["abcd.txt"], "no temporary file is left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn second_call_hits_the_cache_and_a_corrupt_cache_is_rewritten() {
        let dir = scratch("cache");
        let cache = dir.join("cache");
        let lib = dir.join("lib.so");
        std::fs::write(&lib, tiny_elf(DECOY_CODE, 0x4000, &[0x12, 0x34])).unwrap();
        let file = cache.join("1234.txt");

        let first = discover(&lib, Some(&cache)).unwrap();
        assert_eq!(first.source, Source::Scanned);
        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(written, format!("{}\n0x400c\n", header("1234", 1)));

        let second = discover(&lib, Some(&cache)).unwrap();
        assert_eq!(second.source, Source::Cache);
        assert_eq!(second.sites, first.sites);

        for junk in [
            "garbage\n".to_owned(),
            format!("{}\n0x400c\n", header("1234", 2)),
            format!("{}\nzz\n", header("1234", 1)),
            format!("{}\n0x400c\n", header("5678", 1)),
            "".to_owned(),
        ] {
            std::fs::write(&file, &junk).unwrap();
            let again = discover(&lib, Some(&cache)).unwrap();
            assert_eq!(again.source, Source::Scanned, "cache {junk:?} was trusted");
            assert_eq!(again.sites, first.sites);
            assert_eq!(std::fs::read_to_string(&file).unwrap(), written);
        }
        let leftovers: Vec<_> = std::fs::read_dir(&cache).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "temporary files left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zero_sites_is_noticed_against_an_earlier_build() {
        let dir = scratch("zero");
        write_cache(&dir, "old", &[0x10, 0x20]).unwrap();
        write_cache(&dir, "new", &[]).unwrap();
        assert!(other_build_had_sites(&dir, Some("new")));
        std::fs::remove_file(dir.join("old.txt")).unwrap();
        assert!(!other_build_had_sites(&dir, Some("new")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_or_non_elf_files_are_errors() {
        let dir = scratch("bad");
        assert!(discover(&dir.join("absent.so"), None).is_err());
        let junk = dir.join("junk.so");
        std::fs::write(&junk, [0u8; 128]).unwrap();
        assert!(discover(&junk, None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Roblox 2.748.1527 as fetched by the launcher. Run with
    /// `cargo test --release -- --ignored real_libroblox`.
    #[test]
    #[ignore = "needs ~/.cache/intoxicated/apk/lib/x86_64/libroblox.so"]
    fn real_libroblox_2_748_1527() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let lib = home.join(".cache/intoxicated/apk/lib/x86_64/libroblox.so");
        let dir = scratch("real");
        let found = discover(&lib, Some(&dir)).unwrap();
        eprintln!("scanned in {} ms", found.elapsed.as_millis());
        assert_eq!(
            found.build_id.as_deref(),
            Some("00ce2627eafafafe6257fdf5a544879fa2da7424")
        );
        let expected: Vec<u64> = vec![
            0x2b8ecbb, 0x2b8ecde, 0x2b8eead, 0x2b8eefc, 0x2b8ef26, 0x2b8efab, 0x2b8f439,
            0x636de4a, 0x636debe, 0x636df68, 0x636ee4d, 0x636ef38, 0x636f008, 0x636f093,
            0x636fc05, 0x63705bd,
        ];
        assert_eq!(found.sites, expected);
        let again = discover(&lib, Some(&dir)).unwrap();
        assert_eq!(again.source, Source::Cache);
        assert_eq!(again.sites, expected);
        eprintln!("cache hit in {} us", again.elapsed.as_micros());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
