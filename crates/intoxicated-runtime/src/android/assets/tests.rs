//! Two kinds of test: small archives built here, which cover every refusal
//! and run anywhere, and the real APK, whose vectors were measured with
//! Python's `zipfile` and are checked byte for byte.

use super::*;
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

// ------------------------------------------------------------ zip builder

struct Member<'a> {
    name: &'a str,
    method: u16,
    flags: u16,
    /// What goes in the file after the local header.
    data: Vec<u8>,
    crc32: u32,
    size: u32,
    /// Overrides the compressed size in the central directory.
    compressed: Option<u32>,
    local_extra: Vec<u8>,
    central_extra: Vec<u8>,
    /// Overrides the name in the local header.
    local_name: Option<&'a str>,
}

impl<'a> Member<'a> {
    fn stored(name: &'a str, bytes: &[u8]) -> Self {
        Member {
            name,
            method: STORED,
            flags: 0,
            data: bytes.to_vec(),
            crc32: crc32fast::hash(bytes),
            size: bytes.len() as u32,
            compressed: None,
            local_extra: Vec::new(),
            central_extra: Vec::new(),
            local_name: None,
        }
    }

    fn deflated(name: &'a str, bytes: &[u8]) -> Self {
        Member {
            method: DEFLATED,
            data: miniz_oxide::deflate::compress_to_vec(bytes, 6),
            ..Member::stored(name, bytes)
        }
    }
}

fn zip(members: &[Member<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for m in members {
        let at = out.len() as u32;
        let local_name = m.local_name.unwrap_or(m.name);
        let compressed = m.compressed.unwrap_or(m.data.len() as u32);
        out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&m.flags.to_le_bytes());
        out.extend_from_slice(&m.method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&m.crc32.to_le_bytes());
        out.extend_from_slice(&compressed.to_le_bytes());
        out.extend_from_slice(&m.size.to_le_bytes());
        out.extend_from_slice(&(local_name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(m.local_extra.len() as u16).to_le_bytes());
        out.extend_from_slice(local_name.as_bytes());
        out.extend_from_slice(&m.local_extra);
        out.extend_from_slice(&m.data);

        central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&m.flags.to_le_bytes());
        central.extend_from_slice(&m.method.to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&m.crc32.to_le_bytes());
        central.extend_from_slice(&compressed.to_le_bytes());
        central.extend_from_slice(&m.size.to_le_bytes());
        central.extend_from_slice(&(m.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&(m.central_extra.len() as u16).to_le_bytes());
        // Comment length, disk, internal and external attributes.
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&at.to_le_bytes());
        central.extend_from_slice(m.name.as_bytes());
        central.extend_from_slice(&m.central_extra);
    }
    let cd_at = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&END_SIG.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "intoxicated-assets-{}-{}-{tag}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An archive on disk, opened as an APK that lives for the test process,
/// and the directory it is in.
fn apk_kept(tag: &str, bytes: &[u8]) -> (&'static Apk, PathBuf) {
    let dir = scratch(tag);
    let path = dir.join("test.apk");
    std::fs::write(&path, bytes).unwrap();
    (Box::leak(Box::new(Apk::open(&path).unwrap())), dir)
}

/// The same, with the file unlinked at once: the open descriptor keeps it
/// readable and nothing is left in the temporary directory.
fn apk_of(tag: &str, bytes: &[u8]) -> &'static Apk {
    let (apk, dir) = apk_kept(tag, bytes);
    std::fs::remove_dir_all(dir).unwrap();
    apk
}

/// An `AAsset*` from `open_in`, or NULL.
fn open(apk: &'static Apk, name: &str, mode: c_int) -> *mut c_void {
    open_in(apk, name, mode).map_or(std::ptr::null_mut(), |a| Box::into_raw(a).cast())
}

unsafe fn contents(a: *mut c_void) -> Option<Vec<u8>> {
    let p = asset_get_buffer(a) as *const u8;
    if p.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(p, asset_get_length(a) as usize).to_vec())
}

fn read_fd(fd: c_int, at: i64, len: i64) -> Vec<u8> {
    let f = unsafe { File::from_raw_fd(fd) };
    let mut buf = vec![0u8; len as usize];
    f.read_exact_at(&mut buf, at as u64).unwrap();
    buf
}

fn sample() -> Vec<u8> {
    let text = b"stored bytes for the first entry".to_vec();
    let mut padded = Member::stored("assets/a.txt", &text);
    padded.local_extra = vec![0; 3];
    let body: Vec<u8> = (0..20_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
    let mut deflated = Member::deflated("assets/dir/b.bin", &body);
    deflated.central_extra = vec![0xca, 0xfe, 0x00, 0x00, 0x00];
    zip(&[
        Member::stored("lib/x86_64/libx.so", b"not an asset"),
        Member::stored("assets/dir/", b""),
        padded,
        deflated,
        Member::stored("assets/dir/sub/c.txt", b"c"),
        Member::stored("assets/empty", b""),
    ])
}

fn sample_body() -> Vec<u8> {
    (0..20_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect()
}

// --------------------------------------------------------- synthetic tests

#[test]
fn stored_and_deflated_entries_read_back_from_their_local_headers() {
    let apk = apk_of("basic", &sample());
    let a = apk.entry("a.txt").unwrap();
    // Local extra is 3 bytes, central extra 0: the data starts after the local one.
    let header = apk.index().unwrap().get("a.txt").unwrap().header;
    assert_eq!(apk.data_offset(a).unwrap(), header + 30 + "assets/a.txt".len() as u64 + 3);
    assert_eq!(&*apk.read(a).unwrap(), b"stored bytes for the first entry");
    let b = apk.entry("dir/b.bin").unwrap();
    assert_eq!(b.method, DEFLATED);
    assert_eq!(&*apk.read(b).unwrap(), &sample_body()[..]);
    unsafe {
        for mode in [0, 1, 2, MODE_BUFFER] {
            let h = open(apk, "dir/b.bin", mode);
            assert!(!h.is_null());
            assert_eq!(asset_get_length(h), 80_000);
            assert_eq!(contents(h).unwrap(), sample_body());
            asset_close(h);
        }
    }
}

#[test]
fn names_match_exactly_and_directories_do_not_open() {
    let apk = apk_of("names", &sample());
    for miss in [
        "", "dir", "dir/", "/a.txt", "./a.txt", "a.txt/", "A.TXT", "assets/a.txt", "dir//b.bin",
        "lib/x86_64/libx.so", "../lib/x86_64/libx.so", "nothing",
    ] {
        assert!(open(apk, miss, MODE_BUFFER).is_null(), "{miss:?} opened");
        assert!(open(apk, miss, 0).is_null(), "{miss:?} opened");
    }
    // Each open is a fresh handle.
    let (x, y) = (open(apk, "a.txt", 0), open(apk, "a.txt", 0));
    assert!(!x.is_null() && !y.is_null() && x != y);
    unsafe {
        asset_close(x);
        asset_close(y);
    }
}

#[test]
fn listings_are_derived_from_file_paths() {
    let apk = apk_of("list", &sample());
    let root = apk.list("").unwrap();
    assert_eq!(root.files, ["a.txt", "empty"]);
    assert_eq!(root.dirs, ["dir"]);
    let dir = apk.list("dir").unwrap();
    assert_eq!(dir.files, ["b.bin"]);
    assert_eq!(dir.dirs, ["sub"]);
    assert_eq!(apk.list("dir/").unwrap(), dir);
    assert_eq!(apk.list("dir/sub").unwrap().files, ["c.txt"]);
    assert_eq!(apk.list("nowhere").unwrap(), Listing::default());
}

#[test]
fn a_crc_mismatch_is_refused() {
    let mut bad = Member::stored("assets/bad.png", b"these bytes do not match");
    bad.crc32 ^= 1;
    let mut bad_deflated = Member::deflated("assets/bad.json", b"{\"also\": \"wrong\"}");
    bad_deflated.crc32 ^= 0x8000_0000;
    let apk = apk_of("crc", &zip(&[bad, bad_deflated, Member::stored("assets/good", b"ok")]));
    for name in ["bad.png", "bad.json"] {
        assert!(matches!(apk.read(apk.entry(name).unwrap()), Err(Refusal::Crc { .. })));
        // A buffer-mode open is a miss; a probe still sees the name.
        assert!(open(apk, name, MODE_BUFFER).is_null());
        let probe = open(apk, name, 0);
        assert!(!probe.is_null());
        unsafe {
            assert!(asset_get_buffer(probe).is_null());
            let mut out = [0u8; 4];
            assert_eq!(asset_read(probe, out.as_mut_ptr().cast(), 4), -1);
            assert_eq!(asset_open_file_descriptor(probe, std::ptr::null_mut(), std::ptr::null_mut()), -1);
            asset_close(probe);
        }
    }
    // Other entries are unaffected.
    unsafe {
        let h = open(apk, "good", MODE_BUFFER);
        assert_eq!(contents(h).unwrap(), b"ok");
        asset_close(h);
    }
}

#[test]
fn broken_deflate_streams_are_refused() {
    let mut garbage = Member::deflated("assets/garbage", b"x");
    garbage.data = vec![0xff; 64];
    garbage.size = 100;
    // Valid stream, but the directory claims more bytes than it inflates to.
    let mut short = Member::deflated("assets/short", b"twelve bytes");
    short.size = 13;
    // And fewer.
    let mut long = Member::deflated("assets/long", b"twelve bytes");
    long.size = 11;
    // Cut off before the end of the stream.
    let mut cut = Member::deflated("assets/cut", &sample_body());
    cut.data.truncate(cut.data.len() / 2);
    let apk = apk_of("deflate", &zip(&[garbage, short, long, cut]));
    assert!(matches!(apk.read(apk.entry("garbage").unwrap()), Err(Refusal::Inflate(_))));
    assert_eq!(apk.read(apk.entry("short").unwrap()), Err(Refusal::Size { want: 13, got: 12 }));
    assert!(matches!(apk.read(apk.entry("long").unwrap()), Err(Refusal::Inflate(_))));
    assert!(matches!(apk.read(apk.entry("cut").unwrap()), Err(Refusal::Inflate(_))));
    for name in ["garbage", "short", "long", "cut"] {
        assert!(open(apk, name, MODE_BUFFER).is_null(), "{name}");
    }
}

#[test]
fn an_entry_that_runs_past_the_file_is_refused() {
    let mut big = Member::stored("assets/big", b"only a few bytes");
    big.size = 1_000_000;
    big.compressed = Some(1_000_000);
    let apk = apk_of("trunc", &zip(&[big]));
    assert_eq!(apk.read(apk.entry("big").unwrap()), Err(Refusal::Truncated));
    assert!(open(apk, "big", MODE_BUFFER).is_null());
}

#[test]
fn a_local_header_that_disagrees_is_refused() {
    let mut odd = Member::stored("assets/one", b"1");
    odd.local_name = Some("assets/two");
    let apk = apk_of("local", &zip(&[odd]));
    assert!(matches!(apk.read(apk.entry("one").unwrap()), Err(Refusal::LocalHeader(_))));
}

#[test]
fn unsupported_entries_are_misses_even_for_a_probe() {
    let mut bzip = Member::stored("assets/bz", b"data");
    bzip.method = 12;
    let mut locked = Member::stored("assets/locked", b"data");
    locked.flags = 1;
    let apk = apk_of("unsupported", &zip(&[bzip, locked]));
    assert!(apk.entry("bz").is_some());
    for name in ["bz", "locked"] {
        assert!(open(apk, name, 0).is_null(), "{name}");
        assert!(matches!(apk.read(apk.entry(name).unwrap()), Err(Refusal::Unsupported(_))));
    }
}

#[test]
fn not_a_zip_is_remembered_and_every_open_misses() {
    let apk = apk_of("notzip", &[7u8; 5000]);
    assert!(apk.index().is_err());
    assert!(apk.index().is_err());
    assert!(open(apk, "anything", MODE_BUFFER).is_null());
    assert!(apk.list("").is_err());
    let tiny = apk_of("tiny", b"PK");
    assert!(tiny.index().is_err());
}

#[test]
fn zip64_is_refused() {
    let mut bytes = zip(&[Member::stored("assets/a", b"a")]);
    let n = bytes.len();
    // Total entries = 0xffff is the zip64 escape.
    bytes[n - 12..n - 10].copy_from_slice(&0xffffu16.to_le_bytes());
    bytes[n - 14..n - 12].copy_from_slice(&0xffffu16.to_le_bytes());
    let apk = apk_of("zip64", &bytes);
    assert!(apk.index().err().unwrap().contains("zip64"));
}

#[test]
fn a_zero_length_asset_still_has_a_buffer() {
    let apk = apk_of("empty", &sample());
    unsafe {
        let h = open(apk, "empty", MODE_BUFFER);
        assert!(!h.is_null());
        assert!(!asset_get_buffer(h).is_null());
        assert_eq!(asset_get_length(h), 0);
        let mut out = [0u8; 1];
        assert_eq!(asset_read(h, out.as_mut_ptr().cast(), 1), 0);
        asset_close(h);
    }
}

#[test]
fn read_and_seek_follow_the_ndk() {
    let apk = apk_of("seek", &zip(&[Member::deflated("assets/ten", b"0123456789")]));
    unsafe {
        let h = open(apk, "ten", 2);
        let mut buf = [0u8; 16];
        assert_eq!(asset_read(h, buf.as_mut_ptr().cast(), 4), 4);
        assert_eq!(&buf[..4], b"0123");
        assert_eq!(asset_seek(h, 0, libc::SEEK_CUR), 4);
        assert_eq!(asset_seek(h, -2, libc::SEEK_END), 8);
        // A short read returns what there was.
        assert_eq!(asset_read(h, buf.as_mut_ptr().cast(), 16), 2);
        assert_eq!(&buf[..2], b"89");
        assert_eq!(asset_read(h, buf.as_mut_ptr().cast(), 16), 0);
        // Out of range is an error and does not move the position.
        assert_eq!(asset_seek(h, 11, libc::SEEK_SET), -1);
        assert_eq!(asset_seek(h, -1, libc::SEEK_SET), -1);
        assert_eq!(asset_seek(h, 1, libc::SEEK_END), -1);
        assert_eq!(asset_seek(h, i64::MAX, libc::SEEK_CUR), -1);
        assert_eq!(asset_seek(h, 0, 7), -1);
        assert_eq!(asset_seek(h, 0, libc::SEEK_CUR), 10);
        assert_eq!(asset_seek(h, 10, libc::SEEK_SET), 10);
        assert_eq!(asset_seek(h, 3, libc::SEEK_SET), 3);
        assert_eq!(asset_read(h, buf.as_mut_ptr().cast(), 3), 3);
        assert_eq!(&buf[..3], b"345");
        assert_eq!(asset_read(h, buf.as_mut_ptr().cast(), 0), 0);
        assert_eq!(asset_read(h, std::ptr::null_mut(), 1), -1);
        assert_eq!(asset_get_length(h), 10);
        asset_close(h);

        let null = std::ptr::null_mut();
        assert!(asset_get_buffer(null).is_null());
        assert_eq!(asset_get_length(null), 0);
        assert_eq!(asset_read(null, buf.as_mut_ptr().cast(), 1), -1);
        assert_eq!(asset_seek(null, 0, libc::SEEK_SET), -1);
        assert_eq!(asset_open_file_descriptor(null, std::ptr::null_mut(), std::ptr::null_mut()), -1);
        asset_close(null);
    }
}

#[test]
fn descriptors_outlive_the_handle() {
    let (apk, dir) = apk_kept("fd", &sample());
    unsafe {
        // Stored: a descriptor on the archive, pointing at the entry.
        let h = open(apk, "a.txt", MODE_BUFFER);
        let (mut at, mut len) = (-1i64, -1i64);
        let fd = asset_open_file_descriptor(h, &mut at, &mut len);
        asset_close(h);
        assert!(fd >= 0);
        assert_eq!(at as u64, apk.data_offset(apk.entry("a.txt").unwrap()).unwrap());
        assert_eq!(read_fd(fd, at, len), b"stored bytes for the first entry");

        // Deflated: a copy, starting at 0, with the out-parameters optional.
        let h = open(apk, "dir/b.bin", MODE_BUFFER);
        let fd = asset_open_file_descriptor(h, std::ptr::null_mut(), std::ptr::null_mut());
        let fd2 = asset_open_file_descriptor(h, &mut at, &mut len);
        asset_close(h);
        assert!(fd >= 0 && fd2 >= 0 && fd != fd2);
        assert_eq!((at, len), (0, 80_000));
        assert_eq!(read_fd(fd2, 0, len), sample_body());
        drop(File::from_raw_fd(fd));

        // Once the path no longer names the registered file, a stored entry
        // is handed out as a copy instead.
        std::fs::remove_dir_all(&dir).unwrap();
        let h = open(apk, "a.txt", MODE_BUFFER);
        let fd = asset_open_file_descriptor(h, &mut at, &mut len);
        asset_close(h);
        assert!(fd >= 0);
        assert_eq!((at, len), (0, 32));
        assert_eq!(read_fd(fd, at, len), b"stored bytes for the first entry");
    }
}

#[test]
fn the_overlay_walk_stays_inside_its_root() {
    let d = scratch("overlay");
    let root = d.join("overlay");
    std::fs::create_dir_all(root.join("content/fonts")).unwrap();
    std::fs::write(root.join("content/fonts/a.ttf"), b"mine").unwrap();
    std::fs::write(d.join("outside.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(d.join("outside.txt"), root.join("escape.txt")).unwrap();
    std::os::unix::fs::symlink(&d, root.join("up")).unwrap();
    std::os::unix::fs::symlink(root.join("content/fonts/a.ttf"), root.join("alias.ttf")).unwrap();
    let found = overlay::walk(&root);
    let mut names: Vec<_> = found.keys().cloned().collect();
    names.sort();
    assert_eq!(names, ["alias.ttf", "content/fonts/a.ttf"]);
    assert!(overlay::walk(&d.join("missing")).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------- real APK

/// The APK the vectors were measured from (engine 2.748.0.1527). Skipped,
/// with a line saying so, when it is not there or is another build.
fn real() -> Option<&'static Apk> {
    static APK: OnceLock<Option<&'static Apk>> = OnceLock::new();
    *APK.get_or_init(|| {
        let path = std::env::var_os("INTOXICATED_TEST_APK").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".cache/intoxicated/apk/candidate-0.apk")
        });
        match std::fs::metadata(&path) {
            Ok(m) if m.len() == 239_484_151 => Some(&*Box::leak(Box::new(Apk::open(&path).unwrap()))),
            Ok(m) => {
                eprintln!("skipped: {} is {} bytes, not the measured 2.748 APK", path.display(), m.len());
                None
            }
            Err(e) => {
                eprintln!("skipped: no APK at {} ({e})", path.display());
                None
            }
        }
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Vector {
    name: &'static str,
    size: u64,
    crc32: u32,
    sha256: &'static str,
    first: &'static str,
    last: Option<&'static str>,
    /// Stored entries only.
    offset: Option<u64>,
}

const VECTORS: &[Vector] = &[
    Vector {
        name: "content/fonts/SourceSansPro-Regular.ttf",
        size: 269_108,
        crc32: 0x9340855a,
        sha256: "c9868de61ff2bab0b5a3a6d01c4b76f299459f08c6ae2f2c0383b4f9f6bedbf3",
        first: "0001000000110100000400104744454668366ce00002ec5c000003b647504f53",
        last: Some("074c074e075007520754075607600000"),
        offset: Some(49_254_132),
    },
    Vector {
        name: "ExtraContent/textures/sky/white.png",
        size: 323,
        crc32: 0x396bb193,
        sha256: "79fb5b24678d51e690e2cf05c20733358c37c8f1a85eaf25d5f5c5d02499f30f",
        first: "89504e470d0a1a0a0000000d4948445200000010000000100802000000909168",
        last: None,
        offset: Some(33_160_000),
    },
    Vector {
        name: "shaders/shaders_vulkan_mobile.pack",
        size: 20_265_259,
        crc32: 0x3dff57df,
        sha256: "e78e02c15e22f30cbafe1b1c6dbe23773aa771ca949c376ac38612401ee141ae",
        first: "524258530b0001001a000700e0015f113ca8ffff64656661756c740000000000",
        last: Some("0600000009010000fd00010038000100"),
        offset: Some(60_044_040),
    },
    Vector {
        name: "shaders/shaders_glsles3.pack",
        size: 4_915_670,
        crc32: 0xd9e8a7f5,
        sha256: "8feed5dfd1ca48d0f3608119c834c86fa0a7cfd309506715543bef348ef614c1",
        first: "524258530b000100190007009f01dd09a9ffffff64656661756c740000000000",
        last: None,
        offset: Some(55_128_296),
    },
    Vector {
        name: "ExtraContent/textures/ui/LuaApp/graphic/gr-avatar mask-90x90.png",
        size: 598,
        crc32: 0xf5a87653,
        sha256: "6a29debc9fa2a783efd8fad60dfb924b34f63090b7dcac67c60642b1c8e2be65",
        first: "89504e470d0a1a0a0000000d494844520000005a0000005a080400000092a189",
        last: None,
        offset: Some(35_468_996),
    },
    Vector {
        name: "ssl/cacert.pem",
        size: 248_842,
        crc32: 0xc051d183,
        sha256: "869545a59e7cf5f7f8edb9e1dfd28ff4472d2e237948ef65507c7659be69fcc2",
        first: "23230a23232042756e646c65206f6620434120526f6f74204365727469666963",
        last: Some("455254494649434154452d2d2d2d2d0a"),
        offset: None,
    },
    Vector {
        name: "content/fonts/families/Arimo.json",
        size: 965,
        crc32: 0x4efb8531,
        sha256: "93ba9eb9d330aecdf71369695696abafc3d25175ce4b37bfc19c164232bc2ef3",
        first: "7b0a09226e616d65223a20224172696d6f222c0a09226661636573223a205b0a",
        last: None,
        offset: None,
    },
    Vector {
        name: "android/textures/sky/sky512_bk.tex",
        size: 699_172,
        crc32: 0x8078af18,
        sha256: "2edf937e4b5513d016c6fb73d97a6cd180165729d3364a8230d41372ba3f36ad",
        first: "ab4b5458203131bb0d0a1a0a01020304000000000100000000000000648d0000",
        last: None,
        offset: None,
    },
    Vector {
        name: "ExtraContent/models/UniversalApp/UniversalApp_checksum",
        size: 128,
        crc32: 0xeca3b451,
        sha256: "c2ff4f1c0273e18be522d55bd2db5f44f21397426d3f9cfbdd84df38e34c3fe8",
        first: "3538393935396233656238376361383832646563363666376334373261633761",
        last: None,
        offset: None,
    },
    Vector {
        name: "ExtraContent/places/Mobile.rbxl",
        size: 147_973,
        crc32: 0xb7cf4b48,
        sha256: "4aeadcb64bda9db25ffc158d491aa5ac7a937755e7e1fc215d60d9998193ddfe",
        first: "3c726f626c6f782189ff0d0a1a0a000057000000250400000000000000000000",
        last: None,
        offset: None,
    },
    Vector {
        name: "content/configs/DataModelPatchConfig/DataModelPatchConfig.json",
        size: 256,
        crc32: 0x54673a58,
        sha256: "4e1f60444051283e1362faedf7ff2136aa819e453ba56ffcabbc0e715f821af3",
        first: "7b2241707053746f7261676552657365744964223a202230222c202241737365",
        last: None,
        offset: None,
    },
    Vector {
        name: "PublicSuffixDatabase.list",
        size: 132_737,
        crc32: 0xe0ef716d,
        sha256: "c75ab827afd7ffff59a51b14729c25dbfc3b45237eb40ccf02bec7b7d44304a4",
        first: "000206022a2e3030312e746573742e636f64652d6275696c6465722d7374672e",
        last: None,
        offset: None,
    },
];

#[test]
fn real_apk_test_vectors_byte_exact() {
    let Some(apk) = real() else { return };
    for v in VECTORS {
        let e = apk.entry(v.name).unwrap_or_else(|| panic!("{} missing", v.name));
        assert_eq!((e.size, e.crc32), (v.size, v.crc32), "{}", v.name);
        assert_eq!(e.method == STORED, v.offset.is_some(), "{}", v.name);
        if let Some(off) = v.offset {
            assert_eq!(apk.data_offset(e).unwrap(), off, "{}", v.name);
        }
        let bytes = unsafe {
            let h = open(apk, v.name, MODE_BUFFER);
            assert!(!h.is_null(), "{}", v.name);
            assert_eq!(asset_get_length(h), v.size as i64);
            let b = contents(h).unwrap();
            asset_close(h);
            b
        };
        assert_eq!(hex(&bytes[..32]), v.first, "{}", v.name);
        if let Some(last) = v.last {
            assert_eq!(hex(&bytes[bytes.len() - 16..]), last, "{}", v.name);
        }
        assert_eq!(hex(&Sha256::digest(&bytes)), v.sha256, "{}", v.name);
    }
}

#[test]
fn real_apk_every_asset_matches_python_zipfile() {
    let Some(apk) = real() else { return };
    let index = apk.index().unwrap();
    assert_eq!(index.records, 2274);
    let mut names: Vec<&Entry> = index.entries().iter().collect();
    names.sort_by(|a, b| a.name.cmp(&b.name));
    let (mut stored, mut deflated, mut total) = (0, 0, 0u64);
    let mut h = Sha256::new();
    for e in names {
        let b = apk.read(e).unwrap_or_else(|why| panic!("{}: {why}", e.name));
        match e.method {
            STORED => stored += 1,
            _ => deflated += 1,
        }
        total += b.len() as u64;
        h.update(e.name.as_bytes());
        h.update([0]);
        h.update((b.len() as u64).to_le_bytes());
        h.update(&b);
    }
    assert_eq!((stored, deflated, total), (399, 206, 92_709_264));
    // The same digest over `zipfile.ZipFile(apk).read("assets/" + name)` for
    // every name in sorted order, computed with the native Python 3.11.
    assert_eq!(hex(&h.finalize()), "0029d33e9a68ccbaf3e1f0d217195aaf0dfbea52d620dedeedf34bf7bd508e4a");
}

#[test]
fn real_apk_misses() {
    let Some(apk) = real() else { return };
    for name in [
        "content/fonts", "content/fonts/", "assets/ssl/cacert.pem", "/ssl/cacert.pem",
        "./ssl/cacert.pem", "ssl//cacert.pem", "SSL/cacert.pem", "",
        "android/fonts/Arimo-Bold.ttf", "ExtraContent/fonts/Arimo-Bold.ttf",
    ] {
        assert!(open(apk, name, 0).is_null(), "{name:?}");
        assert!(open(apk, name, MODE_BUFFER).is_null(), "{name:?}");
    }
}

#[test]
fn real_apk_directory_listings() {
    let Some(apk) = real() else { return };
    let l = |d| apk.list(d).unwrap();
    assert_eq!(l("").files, ["PublicSuffixDatabase.list"]);
    assert_eq!(l("").dirs, ["ExtraContent", "android", "com", "content", "dexopt", "fonts", "shaders", "ssl"]);
    assert_eq!(l("shaders"), Listing { files: vec!["shaders_glsles3.pack".into(), "shaders_vulkan_mobile.pack".into()], dirs: vec![] });
    assert_eq!(l("ssl").files, ["cacert.pem"]);
    assert_eq!(
        l("content/sky").files,
        ["bn.dds", "cloudAdvection.dds", "cloudDetail.dds", "cloudDetail3D.dds", "clouds.dds", "cloudsfb.dds", "moon.jpg", "noise.dds", "noisefb.dds", "sun.jpg"]
    );
    assert!(l("content/sky").dirs.is_empty());
    assert_eq!(l("fonts").files.len(), 5);
    let sky = l("android/textures/sky").files;
    assert_eq!(sky.len(), 12);
    assert!(sky.iter().all(|f| f.starts_with("indoor512_") || f.starts_with("sky512_")));
    assert!(l("content").files.is_empty());
    assert_eq!(l("content").dirs, ["avatar", "configs", "fonts", "guac", "localization", "models", "sky", "textures"]);
    assert_eq!(l("com"), Listing { files: vec![], dirs: vec!["appsflyer".into()] });
    let families = l("content/fonts/families").files;
    assert_eq!(families.len(), 43);
    assert!(families.iter().all(|f| f.ends_with(".json")));
}

#[test]
fn real_apk_shader_pack_through_a_descriptor() {
    let Some(apk) = real() else { return };
    for (name, want) in [
        ("shaders/shaders_vulkan_mobile.pack", VECTORS[2].sha256),
        ("ssl/cacert.pem", VECTORS[5].sha256),
    ] {
        let (fd, at, len) = unsafe {
            let h = open(apk, name, MODE_BUFFER);
            let (mut at, mut len) = (-1, -1);
            let fd = asset_open_file_descriptor(h, &mut at, &mut len);
            asset_close(h);
            (fd, at, len)
        };
        assert!(fd >= 0, "{name}");
        if name.starts_with("shaders/") {
            assert_eq!((at, len), (60_044_040, 20_265_259));
        } else {
            assert_eq!(at, 0);
        }
        assert_eq!(hex(&Sha256::digest(read_fd(fd, at, len))), want, "{name}");
    }
}

#[test]
fn real_apk_seek_and_length() {
    let Some(apk) = real() else { return };
    unsafe {
        let h = open(apk, "content/fonts/SourceSansPro-Regular.ttf", 0);
        assert_eq!(asset_get_length(h), 269_108);
        assert_eq!(asset_seek(h, -16, libc::SEEK_END), 269_092);
        let mut tail = [0u8; 32];
        assert_eq!(asset_read(h, tail.as_mut_ptr().cast(), 32), 16);
        assert_eq!(hex(&tail[..16]), "074c074e075007520754075607600000");
        assert_eq!(asset_seek(h, 269_109, libc::SEEK_SET), -1);
        assert_eq!(asset_seek(h, 0, libc::SEEK_SET), 0);
        assert_eq!(asset_read(h, tail.as_mut_ptr().cast(), 4), 4);
        assert_eq!(hex(&tail[..4]), "00010000");
        asset_close(h);
    }
}

#[test]
fn real_apk_asset_folder_file_is_keyed_by_content() {
    let Some(apk) = real() else { return };
    let e = apk.entry("ssl/cacert.pem").unwrap();
    let dest = scratch("folder").join("ssl/cacert.pem");
    write_if_changed(apk, e, &dest).unwrap();
    let first = std::fs::metadata(&dest).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    write_if_changed(apk, e, &dest).unwrap();
    assert_eq!(std::fs::metadata(&dest).unwrap().modified().unwrap(), first, "rewritten while unchanged");
    std::fs::write(&dest, b"a bundle from some other build").unwrap();
    write_if_changed(apk, e, &dest).unwrap();
    assert_eq!(hex(&Sha256::digest(std::fs::read(&dest).unwrap())), VECTORS[5].sha256);
    assert_eq!(std::fs::read_dir(dest.parent().unwrap()).unwrap().count(), 1, "a temporary was left behind");
    let _ = std::fs::remove_dir_all(dest.parent().unwrap().parent().unwrap());
}

/// Replays the 111 names one launch asked for, the way the engine asks:
/// every name is probed (mode 0, close); a hit is then read, through the
/// descriptor for the shader pack and through `getBuffer` otherwise. The
/// first pass is on a fresh `Apk` (central directory unparsed, nothing
/// checked); the second repeats it on the same one. Run with `--nocapture`
/// for the numbers.
#[test]
fn real_apk_launch_set() {
    let Some(shared) = real() else { return };
    let apk: &'static Apk = Box::leak(Box::new(Apk::open(shared.path()).unwrap()));
    let set: Vec<(&str, bool)> = include_str!("launch-set.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let (name, kind) = l.split_once('\t').unwrap();
            (name, kind == "hit")
        })
        .collect();
    assert_eq!(set.len(), 111);

    let pass = || -> (Duration, Duration, u64, usize) {
        let started = Instant::now();
        apk.index().unwrap();
        let indexed = started.elapsed();
        let (mut bytes, mut hits) = (0u64, 0usize);
        for &(name, hit) in &set {
            unsafe {
                let p = open(apk, name, 0);
                assert_eq!(!p.is_null(), hit, "{name}");
                asset_close(p);
                if !hit {
                    continue;
                }
                hits += 1;
                let h = open(apk, name, MODE_BUFFER);
                assert!(!h.is_null(), "{name}");
                if name.starts_with("shaders/") {
                    let (mut at, mut len) = (0, 0);
                    let fd = asset_open_file_descriptor(h, &mut at, &mut len);
                    asset_close(h);
                    bytes += read_fd(fd, at, len).len() as u64;
                } else {
                    let n = asset_get_length(h) as usize;
                    let p = asset_get_buffer(h) as *const u8;
                    assert!(!p.is_null(), "{name}");
                    // Touch the bytes the way a consumer would.
                    let s = std::slice::from_raw_parts(p, n);
                    std::hint::black_box((s.first(), s.last()));
                    bytes += n as u64;
                    asset_close(h);
                }
            }
        }
        (indexed, started.elapsed(), bytes, hits)
    };
    let (index_cold, cold, bytes, hits) = pass();
    let (_, warm, bytes2, _) = pass();
    assert_eq!((hits, set.len() - hits), (52, 59));
    assert_eq!((bytes, bytes2), (44_946_866, 44_946_866));
    println!(
        "launch-set: {} names ({hits} hits, {} misses), {bytes} bytes; central directory {:?}; \
         first pass {:?}; second pass {:?}",
        set.len(),
        set.len() - hits,
        index_cold,
        cold,
        warm
    );
}
