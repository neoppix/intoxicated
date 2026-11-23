//! `AAsset*`: the Android asset manager, served straight out of the APK.
//!
//! Written clean-room from a behavioural spec of the engine's use of the NDK's
//! `asset_manager.h`, the NDK documentation and the ZIP format (APPNOTE.TXT).
//!
//! # What the engine does with it
//!
//! `libroblox.so` imports six functions (`AAssetManager_fromJava`,
//! `AAssetManager_open`, `AAsset_getBuffer`, `AAsset_getLength`,
//! `AAsset_close`, `AAsset_openFileDescriptor`) and uses them in three ways:
//!
//! * existence probes: `open` in mode 0, then `close`, over a list of
//!   prefixes (`android/`, `ExtraContent/`, `content/`). About half of all
//!   names asked for are misses. So `open` in that mode is a hash lookup and
//!   reads nothing.
//! * whole-buffer reads: `open` in `AASSET_MODE_BUFFER`, `getBuffer`,
//!   `getLength`, and the handle stays open for as long as the engine uses the
//!   bytes.
//! * the shader pack: `open` in buffer mode, `openFileDescriptor`, then
//!   `fdopen` and `fseek(start)`, and `close` on the handle at once. The
//!   descriptor outlives the handle and the engine closes it.
//!
//! FMOD's Android file layer also looks these names up with `dlsym`, and needs
//! `AAsset_read` and `AAsset_seek` as well, which the engine does not import.
//! They are part of the NDK's `libandroid.so`, so they are implemented here
//! and the symbol table exports them (see `symtab::build`).
//!
//! # How entries are read
//!
//! The central directory is parsed once, on first use, into a name index. An
//! entry's data offset comes from its *local* header (30 bytes, the name, the
//! local extra field): in the 2.748 APK 302 of the 605 asset entries have a
//! local extra field of a different length than the central one (zipalign
//! padding). Stored entries are read with `pread`, deflated ones are inflated
//! with `miniz_oxide`. Every byte handed out has been checked against the
//! entry's CRC32; an entry that fails is refused, never served in part.
//!
//! Buffer-mode opens check the entry before returning a handle, so a corrupt
//! entry is a miss (NULL) where the engine can still fall back, rather than a
//! NULL buffer it does not check for. Probes stay a lookup.
//!
//! Bytes belong to the handle and are freed by `AAsset_close`.
//!
//! The shader pack is stored, so its descriptor is one on the APK with the
//! entry's data offset as the start, as on Android. Measured: the engine then
//! loads the same 2755 shaders, and the byte count in its `Loaded 2755 shaders
//! from pack vulkan_mobile` log line comes out about 60 MB higher than with a
//! copy starting at 0 (the pack starts 60,044,040 bytes into the APK), so that
//! number counts from the start of the file it was handed.
//!
//! `INTOXICATED_TRACE_ASSETS=1` prints one line per open and per `fromJava`
//! call, and the index size, to stderr. Refused entries are always reported,
//! once each.
//!
//! # No extraction cache
//!
//! Measured on 2026-11-22 with `ktrace -t n` over a whole session (home page,
//! a game join, 65 s in game) on the build before this one: the only file the
//! engine opened by path under the old extraction tree was `ssl/cacert.pem`
//! (curl's trust store, through the `exe/cacert.pem` links). Everything else
//! was directory lookups of `content`, `android` and `ExtraContent`, which the
//! engine resolves with `boost::filesystem::canonical` and needs to exist. So
//! [`asset_folder`] builds the top-level directories and writes that one file,
//! nothing else.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// `AASSET_MODE_BUFFER` from `asset_manager.h`.
pub const MODE_BUFFER: c_int = 3;

/// Where the engine's asset names live inside the APK.
const PREFIX: &str = "assets/";

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const END_SIG: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const STORED: u16 = 0;
const DEFLATED: u16 = 8;
/// Deflate cannot expand by more than this (a 258-byte match in 2 bits, a
/// ratio of 1032). An entry that claims more is refused before allocating.
const DEFLATE_MAX_RATIO: u64 = 1032;

// ------------------------------------------------------------------ errors

/// Why an entry was not served. Every variant is a refusal: nothing partial
/// or unverified leaves this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Io(String),
    /// The file ends before the entry does.
    Truncated,
    LocalHeader(&'static str),
    Unsupported(&'static str),
    Inflate(&'static str),
    Size { want: u64, got: u64 },
    Crc { want: u32, got: u32 },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Io(e) => write!(f, "read error: {e}"),
            Refusal::Truncated => write!(f, "the file ends inside the entry"),
            Refusal::LocalHeader(why) => write!(f, "bad local header: {why}"),
            Refusal::Unsupported(why) => write!(f, "unsupported entry: {why}"),
            Refusal::Inflate(why) => write!(f, "bad deflate stream: {why}"),
            Refusal::Size { want, got } => write!(f, "size {got}, the directory says {want}"),
            Refusal::Crc { want, got } => write!(f, "CRC32 {got:08x}, the directory says {want:08x}"),
        }
    }
}

fn read_exact_at(file: &File, buf: &mut [u8], at: u64) -> Result<(), Refusal> {
    // `read_exact_at` loops over short reads and reports a premature end as
    // `UnexpectedEof`, which is the only case that is a truncation.
    file.read_exact_at(buf, at).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => Refusal::Truncated,
        _ => Refusal::Io(e.to_string()),
    })
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

// ----------------------------------------------------------------- archive

/// One file under `assets/`, as the central directory describes it.
pub struct Entry {
    /// Relative to `assets/`, which is how the engine names it.
    pub name: Box<str>,
    pub method: u16,
    pub crc32: u32,
    pub compressed: u64,
    pub size: u64,
    header: u64,
    /// Set when the directory already says the entry cannot be served.
    unsupported: Option<&'static str>,
    data: OnceLock<Result<u64, Refusal>>,
    /// Stored entries only: the whole entry has been read once and matched
    /// its CRC32. Lets the shader pack, opened three times per launch, be
    /// handed out as a descriptor without being read again each time.
    checked: OnceLock<Result<(), Refusal>>,
    reported: AtomicBool,
}

pub struct Index {
    entries: Vec<Entry>,
    by_name: HashMap<Box<str>, usize>,
    /// All central directory records, not just assets.
    pub records: usize,
}

impl Index {
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

/// The files and subdirectories directly inside one asset directory, each
/// sorted, which is what `AAssetDir` would enumerate. The APK has no directory
/// entries under `assets/`, so directories are derived from file paths.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub files: Vec<String>,
    pub dirs: Vec<String>,
}

/// A registered APK. Registration only opens the file; the central directory
/// is read on first use, and a failure to read it is kept and reported once.
pub struct Apk {
    path: PathBuf,
    file: File,
    len: u64,
    identity: (u64, u64),
    index: OnceLock<Result<Index, String>>,
}

impl Apk {
    pub fn open(path: &Path) -> io::Result<Apk> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        Ok(Apk {
            // Absolute, because the descriptor route reopens it by path after
            // the process has moved into its run directory.
            path: path.canonicalize()?,
            file,
            len: meta.len(),
            identity: (meta.dev(), meta.ino()),
            index: OnceLock::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn index(&self) -> Result<&Index, &str> {
        self.index
            .get_or_init(|| {
                let started = std::time::Instant::now();
                let r = parse_central_directory(&self.file, self.len);
                match &r {
                    Ok(index) => trace(format_args!(
                        "indexed {} assets of {} entries in {} in {:?}",
                        index.entries.len(),
                        index.records,
                        self.path.display(),
                        started.elapsed()
                    )),
                    Err(e) => eprintln!(
                        "assets: cannot read {}: {e}; every asset will be a miss",
                        self.path.display()
                    ),
                }
                r
            })
            .as_ref()
            .map_err(|e| e.as_str())
    }

    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.index().ok()?.get(name)
    }

    /// Where the entry's bytes start in the file, from its local header.
    pub fn data_offset(&self, e: &Entry) -> Result<u64, Refusal> {
        e.data
            .get_or_init(|| {
                let full = PREFIX.len() + e.name.len();
                let mut head = vec![0u8; 30 + full];
                read_exact_at(&self.file, &mut head, e.header)?;
                if le32(&head, 0) != LOCAL_SIG {
                    return Err(Refusal::LocalHeader("no signature"));
                }
                let name_len = le16(&head, 26) as usize;
                let extra_len = le16(&head, 28) as u64;
                if name_len != full
                    || &head[30..30 + PREFIX.len()] != PREFIX.as_bytes()
                    || &head[30 + PREFIX.len()..] != e.name.as_bytes()
                {
                    return Err(Refusal::LocalHeader("its name differs from the central directory"));
                }
                let at = e.header + 30 + name_len as u64 + extra_len;
                match at.checked_add(e.compressed) {
                    Some(end) if end <= self.len => Ok(at),
                    _ => Err(Refusal::Truncated),
                }
            })
            .clone()
    }

    /// The entry's uncompressed bytes, checked against its CRC32.
    pub fn read(&self, e: &Entry) -> Result<Box<[u8]>, Refusal> {
        if let Some(why) = e.unsupported {
            return Err(Refusal::Unsupported(why));
        }
        let at = self.data_offset(e)?;
        let mut raw = vec![0u8; e.compressed as usize];
        read_exact_at(&self.file, &mut raw, at)?;
        let bytes = match e.method {
            STORED => raw,
            _ => inflate(&raw, e.size)?,
        };
        let got = crc32fast::hash(&bytes);
        if got != e.crc32 {
            return Err(Refusal::Crc { want: e.crc32, got });
        }
        Ok(bytes.into_boxed_slice())
    }

    /// For a stored entry: read it through once, in pieces, and compare the
    /// CRC32. Remembered, because the file does not change under a running
    /// client (the launcher replaces the APK by renaming a new directory in).
    fn check_stored(&self, e: &Entry) -> Result<(), Refusal> {
        e.checked
            .get_or_init(|| {
                if let Some(why) = e.unsupported {
                    return Err(Refusal::Unsupported(why));
                }
                let at = self.data_offset(e)?;
                let mut hasher = crc32fast::Hasher::new();
                let mut buf = vec![0u8; (1 << 20).min(e.size as usize)];
                let mut done = 0u64;
                while done < e.size {
                    let n = (e.size - done).min(buf.len() as u64) as usize;
                    read_exact_at(&self.file, &mut buf[..n], at + done)?;
                    hasher.update(&buf[..n]);
                    done += n as u64;
                }
                let got = hasher.finalize();
                if got != e.crc32 {
                    return Err(Refusal::Crc { want: e.crc32, got });
                }
                Ok(())
            })
            .clone()
    }

    /// A new descriptor on the same file, with its own file offset. Opened by
    /// path rather than `dup`ed, so two FILE*s on the shader pack cannot move
    /// each other's position, and refused when the path no longer names the
    /// file that was registered.
    fn reopen(&self) -> Option<File> {
        let f = File::open(&self.path).ok()?;
        let m = f.metadata().ok()?;
        ((m.dev(), m.ino()) == self.identity && m.len() == self.len).then_some(f)
    }

    /// The files and subdirectories directly inside `dir` (`""` is the root).
    pub fn list(&self, dir: &str) -> Result<Listing, &str> {
        let index = self.index()?;
        let mut files = BTreeSet::new();
        let mut dirs = BTreeSet::new();
        let dir = dir.trim_end_matches('/');
        for e in &index.entries {
            let rest = if dir.is_empty() {
                &*e.name
            } else {
                match e.name.strip_prefix(dir).and_then(|r| r.strip_prefix('/')) {
                    Some(r) => r,
                    None => continue,
                }
            };
            match rest.split_once('/') {
                Some((sub, _)) => dirs.insert(sub.to_string()),
                None => files.insert(rest.to_string()),
            };
        }
        Ok(Listing { files: files.into_iter().collect(), dirs: dirs.into_iter().collect() })
    }
}

fn parse_central_directory(file: &File, len: u64) -> Result<Index, String> {
    if len < 22 {
        return Err("too short to be a zip".into());
    }
    // The end record is the last 22 bytes plus a comment of up to 64 KiB.
    let tail_len = len.min(22 + 0xffff);
    let tail_at = len - tail_len;
    let mut tail = vec![0u8; tail_len as usize];
    read_exact_at(file, &mut tail, tail_at).map_err(|e| e.to_string())?;
    let end = (0..=tail.len() - 22)
        .rev()
        .find(|&i| le32(&tail, i) == END_SIG && i + 22 + le16(&tail, i + 20) as usize == tail.len())
        .ok_or("no end of central directory record")?;
    let r = &tail[end..];
    let (disk, cd_disk) = (le16(r, 4), le16(r, 6));
    let (on_disk, total) = (le16(r, 8), le16(r, 10));
    let cd_size = le32(r, 12) as u64;
    let cd_at = le32(r, 16) as u64;
    if (end >= 20 && le32(&tail, end - 20) == ZIP64_LOCATOR_SIG)
        || total == 0xffff
        || cd_size == 0xffff_ffff
        || cd_at == 0xffff_ffff
    {
        return Err("zip64 archives are not supported".into());
    }
    if disk != 0 || cd_disk != 0 || on_disk != total {
        return Err("multi-disk archives are not supported".into());
    }
    if cd_at + cd_size > tail_at + end as u64 {
        return Err("the central directory runs past its end record".into());
    }
    let mut cd = vec![0u8; cd_size as usize];
    read_exact_at(file, &mut cd, cd_at).map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    let mut by_name = HashMap::new();
    let mut at = 0usize;
    for n in 0..total {
        if at + 46 > cd.len() || le32(&cd, at) != CENTRAL_SIG {
            return Err(format!("central directory record {n} is damaged"));
        }
        let flags = le16(&cd, at + 8);
        let method = le16(&cd, at + 10);
        let crc32 = le32(&cd, at + 16);
        let compressed = le32(&cd, at + 20);
        let size = le32(&cd, at + 24);
        let name_len = le16(&cd, at + 28) as usize;
        let extra_len = le16(&cd, at + 30) as usize;
        let comment_len = le16(&cd, at + 32) as usize;
        let header = le32(&cd, at + 42);
        let name_at = at + 46;
        let next = name_at + name_len + extra_len + comment_len;
        if next > cd.len() {
            return Err(format!("central directory record {n} runs past the directory"));
        }
        at = next;
        // Not UTF-8 means it cannot match a name the engine asks for.
        let Ok(full) = std::str::from_utf8(&cd[name_at..name_at + name_len]) else { continue };
        let Some(name) = full.strip_prefix(PREFIX) else { continue };
        if name.is_empty() || name.ends_with('/') {
            continue;
        }
        let unsupported = if flags & 1 != 0 {
            Some("encrypted")
        } else if compressed == 0xffff_ffff || size == 0xffff_ffff || header == 0xffff_ffff {
            Some("zip64 sizes")
        } else if method != STORED && method != DEFLATED {
            Some("compression method other than stored or deflate")
        } else if method == STORED && compressed != size {
            Some("stored with two different sizes")
        } else if method == DEFLATED && size as u64 > compressed as u64 * DEFLATE_MAX_RATIO + 64 {
            Some("larger than deflate can expand to")
        } else {
            None
        };
        if by_name.contains_key(name) {
            // First one wins; the 2.748 APK has no duplicates.
            continue;
        }
        by_name.insert(name.into(), entries.len());
        entries.push(Entry {
            name: name.into(),
            method,
            crc32,
            compressed: compressed as u64,
            size: size as u64,
            header: header as u64,
            unsupported,
            data: OnceLock::new(),
            checked: OnceLock::new(),
            reported: AtomicBool::new(false),
        });
    }
    Ok(Index { entries, by_name, records: total as usize })
}

/// Raw deflate (no zlib header) into a buffer of exactly the size the
/// directory gives. A stream that wants to write more, or ends short, is
/// refused.
fn inflate(input: &[u8], size: u64) -> Result<Vec<u8>, Refusal> {
    use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
    use miniz_oxide::inflate::TINFLStatus;
    let mut out = vec![0u8; size as usize];
    let mut state = Box::<DecompressorOxide>::default();
    let (status, _, written) = decompress(
        &mut state,
        input,
        &mut out,
        0,
        inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
    );
    match status {
        TINFLStatus::Done if written as u64 == size => Ok(out),
        TINFLStatus::Done => Err(Refusal::Size { want: size, got: written as u64 }),
        TINFLStatus::HasMoreOutput => Err(Refusal::Inflate("more data than the directory says")),
        TINFLStatus::FailedCannotMakeProgress | TINFLStatus::NeedsMoreInput => {
            Err(Refusal::Inflate("the stream ends early"))
        }
        _ => Err(Refusal::Inflate("invalid data")),
    }
}

// ------------------------------------------------------------ registration

static APK: OnceLock<Apk> = OnceLock::new();

/// Register the APK assets are served from. Opens the file and nothing else:
/// a damaged zip is reported when it is first read, not here.
pub fn register_apk(path: &Path) -> Result<(), String> {
    if let Some(have) = APK.get() {
        return if path.canonicalize().is_ok_and(|p| p == have.path) {
            Ok(())
        } else {
            Err(format!("{} is already registered, not {}", have.path.display(), path.display()))
        };
    }
    let apk = Apk::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = APK.set(apk);
    register_apk(path)
}

fn tracing() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("INTOXICATED_TRACE_ASSETS").is_some())
}

fn trace(args: fmt::Arguments<'_>) {
    if tracing() {
        eprintln!("assets: {args}");
    }
}

fn report(apk: &Apk, e: &Entry, why: &Refusal) {
    if !e.reported.swap(true, Ordering::Relaxed) {
        eprintln!("assets: refusing {} from {}: {why}", e.name, apk.path.display());
    }
}

// ----------------------------------------------------------------- handles

/// What an `AAsset*` points to.
struct Asset {
    apk: &'static Apk,
    /// `None` for a file served from the overlay.
    entry: Option<&'static Entry>,
    bytes: OnceLock<Option<Box<[u8]>>>,
    len: u64,
    pos: Mutex<u64>,
}

impl Asset {
    fn bytes(&self) -> Option<&[u8]> {
        self.bytes
            .get_or_init(|| {
                let e = self.entry?;
                self.apk.read(e).map_err(|why| report(self.apk, e, &why)).ok()
            })
            .as_deref()
    }

    /// `(fd, start, length)`. A stored APK entry is a descriptor on the APK
    /// itself, the way Android does it; anything else (a deflated entry, an
    /// overlay file) is a copy in an anonymous memory file. Android refuses
    /// those, but the engine's descriptor site has no fallback.
    fn descriptor(&self) -> Option<(c_int, i64, i64)> {
        if let Some(e) = self.entry.filter(|e| e.method == STORED) {
            if let Err(why) = self.apk.check_stored(e) {
                report(self.apk, e, &why);
                return None;
            }
            if let (Some(f), Ok(at)) = (self.apk.reopen(), self.apk.data_offset(e)) {
                return Some((f.into_raw_fd(), at as i64, e.size as i64));
            }
        }
        let bytes = self.bytes()?;
        memory_file(bytes).map(|fd| (fd, 0, bytes.len() as i64))
    }
}

fn memory_file(bytes: &[u8]) -> Option<c_int> {
    let fd = unsafe { libc::memfd_create(b"intoxicated-asset\0".as_ptr().cast(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // Owned from here on, so an error path closes it.
    let file = unsafe { File::from_raw_fd(fd) };
    // pwrite: the descriptor's own offset stays at 0 for the reader.
    file.write_all_at(bytes, 0).ok()?;
    Some(file.into_raw_fd())
}

/// The open logic, on a given APK so tests can use their own archives.
fn open_in(apk: &'static Apk, name: &str, mode: c_int) -> Option<Box<Asset>> {
    if let Some(bytes) = overlay::read(name) {
        trace(format_args!("open {name} mode {mode}: overlay"));
        let len = bytes.len() as u64;
        let asset = Asset { apk, entry: None, bytes: OnceLock::new(), len, pos: Mutex::new(0) };
        let _ = asset.bytes.set(Some(bytes));
        return Some(Box::new(asset));
    }
    let Some(e) = apk.entry(name) else {
        trace(format_args!("open {name} mode {mode}: miss"));
        return None;
    };
    if let Some(why) = e.unsupported {
        report(apk, e, &Refusal::Unsupported(why));
        return None;
    }
    let asset = Asset { apk, entry: Some(e), bytes: OnceLock::new(), len: e.size, pos: Mutex::new(0) };
    // Buffer mode means the caller wants the bytes, so a corrupt entry is
    // found here and becomes a miss. A deflated entry is inflated now and kept;
    // a stored one is checked (once per process) and read on `getBuffer`, so
    // the shader pack's descriptor route never copies it.
    if mode == MODE_BUFFER {
        let ok = if e.method == STORED {
            apk.check_stored(e).map_err(|why| report(apk, e, &why)).is_ok()
        } else {
            asset.bytes().is_some()
        };
        if !ok {
            trace(format_args!("open {name} mode {mode}: refused"));
            return None;
        }
    }
    trace(format_args!(
        "open {name} mode {mode}: {} {} bytes",
        if e.method == STORED { "stored" } else { "deflated" },
        e.size
    ));
    Some(Box::new(asset))
}

// ---------------------------------------------------------- the C entry points

/// The one asset manager. Its address is the token `fromJava` returns.
static MANAGER: u8 = 0;

unsafe fn handle<'a>(asset: *mut c_void) -> Option<&'a Asset> {
    (asset as *const Asset).as_ref()
}

/// `AAssetManager* AAssetManager_fromJava(JNIEnv*, jobject)`. Every Java
/// `AssetManager` here is a placeholder, so every one maps to the APK.
extern "C" fn manager_from_java(_env: *mut c_void, _manager: *mut c_void) -> *mut c_void {
    let token = match APK.get() {
        Some(_) => &MANAGER as *const u8 as *mut c_void,
        None => std::ptr::null_mut(),
    };
    super::trace(format_args!("AAssetManager_fromJava -> {token:p}"));
    trace(format_args!("AAssetManager_fromJava -> {token:p}"));
    token
}

/// `AAsset* AAssetManager_open(AAssetManager*, const char*, int mode)`.
unsafe extern "C" fn manager_open(_manager: *mut c_void, name: *const c_char, mode: c_int) -> *mut c_void {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    let Some(apk) = APK.get() else {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            eprintln!("assets: AAssetManager_open before an APK was registered (pass --apk)");
        }
        return std::ptr::null_mut();
    };
    let Ok(name) = CStr::from_ptr(name).to_str() else { return std::ptr::null_mut() };
    match open_in(apk, name, mode) {
        Some(a) => Box::into_raw(a).cast(),
        None => std::ptr::null_mut(),
    }
}

/// `const void* AAsset_getBuffer(AAsset*)`. Never NULL for a readable
/// handle, including a zero-length one.
unsafe extern "C" fn asset_get_buffer(asset: *mut c_void) -> *const c_void {
    match handle(asset).and_then(Asset::bytes) {
        Some(b) => b.as_ptr().cast(),
        None => std::ptr::null(),
    }
}

/// `off_t AAsset_getLength(AAsset*)`.
unsafe extern "C" fn asset_get_length(asset: *mut c_void) -> i64 {
    handle(asset).map_or(0, |a| a.len as i64)
}

/// `void AAsset_close(AAsset*)`.
unsafe extern "C" fn asset_close(asset: *mut c_void) {
    if !asset.is_null() {
        drop(Box::from_raw(asset as *mut Asset));
    }
}

/// `int AAsset_openFileDescriptor(AAsset*, off_t* outStart, off_t* outLength)`.
/// The descriptor is the caller's and stays valid after `AAsset_close`.
unsafe extern "C" fn asset_open_file_descriptor(asset: *mut c_void, start: *mut i64, length: *mut i64) -> c_int {
    let Some((fd, at, len)) = handle(asset).and_then(Asset::descriptor) else { return -1 };
    if !start.is_null() {
        *start = at;
    }
    if !length.is_null() {
        *length = len;
    }
    fd
}

/// `int AAsset_read(AAsset*, void* buf, size_t count)`: bytes read, 0 at the
/// end, negative on error.
unsafe extern "C" fn asset_read(asset: *mut c_void, buf: *mut c_void, count: usize) -> c_int {
    let Some(a) = handle(asset) else { return -1 };
    if count == 0 {
        return 0;
    }
    if buf.is_null() {
        return -1;
    }
    let Some(bytes) = a.bytes() else { return -1 };
    let mut pos = a.pos.lock().unwrap_or_else(|p| p.into_inner());
    let from = (*pos).min(bytes.len() as u64) as usize;
    let n = count.min(bytes.len() - from).min(c_int::MAX as usize);
    std::ptr::copy_nonoverlapping(bytes[from..].as_ptr(), buf.cast::<u8>(), n);
    *pos = (from + n) as u64;
    n as c_int
}

/// `off_t AAsset_seek(AAsset*, off_t offset, int whence)`. As in AOSP's
/// `Asset::handleSeek`, a position outside `0..=length` is an error (-1) and
/// leaves the position where it was.
unsafe extern "C" fn asset_seek(asset: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let Some(a) = handle(asset) else { return -1 };
    let mut pos = a.pos.lock().unwrap_or_else(|p| p.into_inner());
    let base = match whence {
        libc::SEEK_SET => 0,
        libc::SEEK_CUR => *pos as i64,
        libc::SEEK_END => a.len as i64,
        _ => return -1,
    };
    match base.checked_add(offset) {
        Some(new) if (0..=a.len as i64).contains(&new) => {
            *pos = new as u64;
            new
        }
        _ => -1,
    }
}

/// The whole asset API this module implements, for the virtual
/// `libandroid.so`. The first six are what `libroblox.so` imports; the last
/// two are only ever reached through `dlsym` (FMOD).
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("AAssetManager_fromJava", manager_from_java),
        f!("AAssetManager_open", manager_open),
        f!("AAsset_getBuffer", asset_get_buffer),
        f!("AAsset_getLength", asset_get_length),
        f!("AAsset_close", asset_close),
        f!("AAsset_openFileDescriptor", asset_open_file_descriptor),
        f!("AAsset_read", asset_read),
        f!("AAsset_seek", asset_seek),
    ]
}

/// `--read-asset`: open, read and close one name through the C entry points.
pub fn probe(name: &str) -> Result<usize, String> {
    let c = CString::new(name).map_err(|_| "the name contains a NUL byte".to_string())?;
    unsafe {
        let a = manager_open(manager_from_java(std::ptr::null_mut(), std::ptr::null_mut()), c.as_ptr(), MODE_BUFFER);
        if a.is_null() {
            return Err("not in the APK, or refused".into());
        }
        let len = asset_get_length(a);
        let ok = !asset_get_buffer(a).is_null();
        asset_close(a);
        if ok {
            Ok(len as usize)
        } else {
            Err("unreadable".into())
        }
    }
}

// ----------------------------------------------------------------- overlay

/// User files served in place of APK entries: `<root>/<name>` replaces
/// `assets/<name>`. The root is `INTOXICATED_OVERLAY`, else
/// `$XDG_CONFIG_HOME/intoxicated/overlay`, else `~/.config/intoxicated/overlay`.
/// Walked once, on the first open; a missing root contributes nothing.
mod overlay {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    pub fn root() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("INTOXICATED_OVERLAY") {
            return Some(PathBuf::from(p));
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("intoxicated/overlay"))
    }

    fn index() -> &'static HashMap<String, PathBuf> {
        static INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
        INDEX.get_or_init(|| root().map(|r| walk(&r)).unwrap_or_default())
    }

    /// The file for `name`, read whole. If it has gone since the walk, the
    /// APK entry is served instead.
    pub fn read(name: &str) -> Option<Box<[u8]>> {
        std::fs::read(index().get(name)?).ok().map(Vec::into_boxed_slice)
    }

    /// Every regular file under `root`, by its `/`-separated relative name.
    /// Anything that resolves outside the root, through a symlink or
    /// otherwise, is left out.
    pub fn walk(root: &Path) -> HashMap<String, PathBuf> {
        let mut found = HashMap::new();
        let Ok(base) = root.canonicalize() else { return found };
        let mut todo = vec![(base.clone(), String::new(), 0u32)];
        while let Some((dir, prefix, depth)) = todo.pop() {
            let Ok(list) = std::fs::read_dir(&dir) else { continue };
            for item in list.flatten() {
                let Ok(real) = item.path().canonicalize() else { continue };
                let Some(leaf) = item.file_name().to_str().map(str::to_string) else { continue };
                if !real.starts_with(&base) || real == base {
                    continue;
                }
                let name = format!("{prefix}{leaf}");
                match std::fs::metadata(&real) {
                    Ok(m) if m.is_dir() && depth < 16 => todo.push((real, format!("{name}/"), depth + 1)),
                    Ok(m) if m.is_file() => {
                        found.insert(name, real);
                    }
                    _ => {}
                }
            }
        }
        found
    }
}

// ------------------------------------------------------------ asset folder

/// Files the engine opens by path rather than through `AAsset*`, so they
/// have to exist on disk. Measured: see the module documentation.
const ON_DISK: &[&str] = &["ssl/cacert.pem"];

static FOLDER: OnceLock<Result<PathBuf, String>> = OnceLock::new();

fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("intoxicated/assets")
}

/// The directory the engine is given as its asset root (it is handed
/// `<root>/content` and resolves the siblings against the parent).
///
/// Holds one empty directory per top-level directory of the APK's `assets/`
/// and the files in [`ON_DISK`]. A file is rewritten when its size or CRC32
/// differs from the registered APK's entry, so a new APK with a different
/// bundle replaces it and an unchanged one costs a 250 KB read. Prepared once
/// per process; later calls return the same answer.
pub fn asset_folder() -> Result<&'static Path, String> {
    FOLDER.get_or_init(prepare_folder).as_deref().map_err(Clone::clone)
}

/// The CA bundle inside [`asset_folder`], once that has been prepared.
pub fn ca_bundle() -> Option<PathBuf> {
    let p = FOLDER.get()?.as_ref().ok()?.join("ssl/cacert.pem");
    p.is_file().then_some(p)
}

fn prepare_folder() -> Result<PathBuf, String> {
    let apk = APK.get().ok_or("no APK registered")?;
    let index = apk.index().map_err(|e| format!("cannot read {}: {e}", apk.path.display()))?;
    let root = cache_root();
    let top = apk.list("").map_err(str::to_string)?;
    for dir in &top.dirs {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    for &name in ON_DISK {
        match index.get(name) {
            Some(e) => write_if_changed(apk, e, &root.join(name))?,
            None => println!("  assets: {} has no {name}; nothing to put on disk", apk.path.display()),
        }
    }
    Ok(root)
}

fn write_if_changed(apk: &Apk, e: &Entry, dest: &Path) -> Result<(), String> {
    if let Ok(have) = std::fs::read(dest) {
        if have.len() as u64 == e.size && crc32fast::hash(&have) == e.crc32 {
            return Ok(());
        }
    }
    let bytes = apk.read(e).map_err(|why| format!("{}: {why}", e.name))?;
    let dir = dest.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let leaf = dest.file_name().and_then(|n| n.to_str()).unwrap_or("asset");
    let tmp = dir.join(format!(".{leaf}.{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, dest));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {err}", dest.display()));
    }
    println!("  assets: wrote {} ({} bytes) to {}", e.name, bytes.len(), dir.display());
    Ok(())
}

#[cfg(test)]
mod tests;
