//! The audio libraries the guest links or opens: `libOpenSLES.so` and
//! `libaaudio.so`, both implemented in C++ over FreeBSD sound(4).
//!
//! - `native/opensl_refusal.cpp`: the eight OpenSL ES names `libroblox.so`
//!   links (`slCreateEngine` and seven `SL_IID_*` data symbols, which must
//!   resolve at load time). `slCreateEngine` answers
//!   `SL_RESULT_FEATURE_UNSUPPORTED`.
//! - `native/aaudio_oss.cpp`: the 26 AAudio names engine 2.748 resolves with
//!   `dlsym` after `org.fmod.FMOD.supportsAAudio()` said yes.
//! - `native/oss_device.cpp`: the /dev/dsp node, its negotiation, the format
//!   conversion and the probe behind that predicate.

use std::ffi::{c_char, c_void, CStr};

#[repr(C)]
struct Symbol {
    name: *const c_char,
    addr: *mut c_void,
}

fn collect(table: *const Symbol, count: usize) -> Vec<(&'static str, *mut c_void)> {
    if table.is_null() {
        return Vec::new();
    }
    // SAFETY: `table` points at `count` initialised entries in a C++ static
    // that outlives the process, each with a string literal for a name.
    let entries = unsafe { std::slice::from_raw_parts(table, count) };
    entries
        .iter()
        .map(|e| {
            // SAFETY: see above; the names are NUL-terminated literals.
            let name = unsafe { CStr::from_ptr(e.name) }.to_str().unwrap_or("");
            (name, e.addr)
        })
        .collect()
}

extern "C" {
    fn intoxicated_opensl_symbols(count: *mut usize) -> *const Symbol;
    fn intoxicated_aaudio_symbols(count: *mut usize) -> *const Symbol;
    fn intoxicated_audio_announce();
}

/// The `libOpenSLES.so` names, for the override table.
pub fn opensles_overrides() -> Vec<(&'static str, *mut c_void)> {
    let mut count = 0usize;
    // SAFETY: returns a static table and writes its length.
    let table = unsafe { intoxicated_opensl_symbols(&mut count) };
    collect(table, count)
}

/// The `libaaudio.so` names, registered as a virtual library of their own.
pub fn aaudio_overrides() -> Vec<(&'static str, *mut c_void)> {
    let mut count = 0usize;
    // SAFETY: returns a static table and writes its length.
    let table = unsafe { intoxicated_aaudio_symbols(&mut count) };
    collect(table, count)
}

/// Prints the output node, its /dev/sndstat description and the probe result,
/// once, during startup.
pub fn announce_audio_backend() {
    // SAFETY: no arguments; writes to stderr once.
    unsafe { intoxicated_audio_announce() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_int;
    use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// The 26 names engine 2.748.0.1527 contains (spec section 1.2).
    /// Duplicated on purpose: this is the assertion that the C++ table and
    /// the measurement agree. A typo would give the guest a null `dlsym`.
    const ENGINE_2748: &[&str] = &[
        "AAudio_createStreamBuilder",
        "AAudioStreamBuilder_delete",
        "AAudioStreamBuilder_openStream",
        "AAudioStreamBuilder_setBufferCapacityInFrames",
        "AAudioStreamBuilder_setDataCallback",
        "AAudioStreamBuilder_setDirection",
        "AAudioStreamBuilder_setErrorCallback",
        "AAudioStreamBuilder_setFormat",
        "AAudioStreamBuilder_setInputPreset",
        "AAudioStreamBuilder_setPerformanceMode",
        "AAudioStreamBuilder_setUsage",
        "AAudioStream_close",
        "AAudioStream_getBufferCapacityInFrames",
        "AAudioStream_getBufferSizeInFrames",
        "AAudioStream_getChannelCount",
        "AAudioStream_getFormat",
        "AAudioStream_getFramesPerBurst",
        "AAudioStream_getSampleRate",
        "AAudioStream_getState",
        "AAudioStream_getXRunCount",
        "AAudioStream_read",
        "AAudioStream_requestPause",
        "AAudioStream_requestStart",
        "AAudioStream_requestStop",
        "AAudioStream_setBufferSizeInFrames",
        "AAudioStream_waitForStateChange",
    ];

    #[test]
    fn aaudio_exports_exactly_the_engine_names() {
        let mut got: Vec<&str> = aaudio_overrides().into_iter().map(|(n, _)| n).collect();
        got.sort_unstable();
        let mut want = ENGINE_2748.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
        for (name, addr) in aaudio_overrides() {
            assert!(!addr.is_null(), "{name} resolved to null");
        }
    }

    #[test]
    fn opensl_has_the_eight_link_names_with_distinct_ids() {
        let syms = opensles_overrides();
        let mut names: Vec<&str> = syms.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "SL_IID_ANDROIDCONFIGURATION",
                "SL_IID_ANDROIDSIMPLEBUFFERQUEUE",
                "SL_IID_BUFFERQUEUE",
                "SL_IID_ENGINE",
                "SL_IID_PLAY",
                "SL_IID_RECORD",
                "SL_IID_VOLUME",
                "slCreateEngine",
            ]
        );
        let mut ids = Vec::new();
        for (name, addr) in &syms {
            assert!(!addr.is_null());
            if name.starts_with("SL_IID_") {
                // SAFETY: each data symbol is a pointer variable to a 16-byte id.
                let id = unsafe { *(*addr as *const *const [u8; 16]) };
                assert!(!id.is_null());
                ids.push(unsafe { *id });
            }
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 7, "interface ids must be distinct");

        let create = syms.iter().find(|(n, _)| *n == "slCreateEngine").unwrap().1;
        type Create = unsafe extern "C" fn(
            *mut *mut c_void,
            u32,
            *const c_void,
            u32,
            *const c_void,
            *const c_void,
        ) -> u32;
        // SAFETY: the address is ix_slCreateEngine with this prototype.
        let f: Create = unsafe { std::mem::transmute(create) };
        let mut engine = 1usize as *mut c_void;
        let r = unsafe { f(&mut engine, 0, std::ptr::null(), 0, std::ptr::null(), std::ptr::null()) };
        assert_eq!(r, 0x0C, "SL_RESULT_FEATURE_UNSUPPORTED");
        assert!(engine.is_null());
    }

    // ------------------------------------------------- conversion and math

    extern "C" {
        fn intoxicated_oss_fragment_arg(count: c_int, frames: c_int, bpf: c_int) -> u32;
        fn intoxicated_oss_fragment_frames(fragment_bytes: c_int, bpf: c_int) -> c_int;
        fn intoxicated_oss_frames_to_us(frames: i64, rate: c_int) -> i64;
        fn intoxicated_oss_default_fragment_frames(rate: c_int) -> c_int;
        fn intoxicated_oss_parse_fragment(text: *const c_char, count: *mut c_int, frames: *mut c_int) -> c_int;
        fn intoxicated_f32_to_s16(input: *const f32, out: *mut i16, n: usize);
        fn intoxicated_f32_to_s32(input: *const f32, out: *mut i32, n: usize);
        fn intoxicated_s16_to_s32(input: *const i16, out: *mut i32, n: usize);
        fn intoxicated_s32_to_s16(input: *const i32, out: *mut i16, n: usize);
        fn intoxicated_aaudio_use_test_sink(on: c_int);
        fn intoxicated_aaudio_fail_test_sink(err: c_int) -> c_int;
    }

    #[test]
    fn float_to_s16_scales_clamps_and_silences_nan() {
        let input = [0.0f32, 1.0, -1.0, 0.5, -0.5, 2.0, -7.0, f32::NAN, 1.0 / 32767.0];
        let mut out = [0i16; 9];
        unsafe { intoxicated_f32_to_s16(input.as_ptr(), out.as_mut_ptr(), input.len()) };
        assert_eq!(out, [0, 32767, -32767, 16384, -16384, 32767, -32767, 0, 1]);
    }

    #[test]
    fn float_to_s32_scales_clamps_and_silences_nan() {
        let input = [0.0f32, 1.0, -1.0, 0.5, 3.0, f32::NAN];
        let mut out = [0i32; 6];
        unsafe { intoxicated_f32_to_s32(input.as_ptr(), out.as_mut_ptr(), input.len()) };
        assert_eq!(out, [0, i32::MAX, -i32::MAX, 1073741824, i32::MAX, 0]);
    }

    #[test]
    fn integer_widening_and_narrowing_round_trip() {
        let input = [0i16, 1, -1, i16::MAX, i16::MIN];
        let mut wide = [0i32; 5];
        let mut back = [0i16; 5];
        unsafe {
            intoxicated_s16_to_s32(input.as_ptr(), wide.as_mut_ptr(), 5);
            intoxicated_s32_to_s16(wide.as_ptr(), back.as_mut_ptr(), 5);
        }
        assert_eq!(wide, [0, 65536, -65536, 32767 << 16, -32768 << 16]);
        assert_eq!(back, input);
    }

    #[test]
    fn fragment_math() {
        unsafe {
            // 3 x 256 frames of S16 stereo = 3 x 1024 B.
            assert_eq!(intoxicated_oss_fragment_arg(3, 256, 4), 0x0003_000a);
            // The spec's measured 4 x 512 B request.
            assert_eq!(intoxicated_oss_fragment_arg(4, 128, 4), 0x0004_0009);
            // S32 stereo doubles the byte size.
            assert_eq!(intoxicated_oss_fragment_arg(3, 256, 8), 0x0003_000b);
            // A byte size that is not a power of two rounds to the nearest
            // one: 6 ch S16 x 256 frames = 3072 B = 2^11.58, so 2^12.
            assert_eq!(intoxicated_oss_fragment_arg(3, 256, 12) & 0xffff, 12);
            // Clamps: at least 2 fragments, 16 B to 64 KiB.
            assert_eq!(intoxicated_oss_fragment_arg(1, 1, 1), 0x0002_0004);
            assert_eq!(intoxicated_oss_fragment_arg(2, 1 << 20, 8) & 0xffff, 16);

            assert_eq!(intoxicated_oss_fragment_frames(1024, 4), 256);
            assert_eq!(intoxicated_oss_fragment_frames(4096, 12), 341);
            assert_eq!(intoxicated_oss_fragment_frames(0, 4), 0);

            assert_eq!(intoxicated_oss_frames_to_us(768, 48000), 16000);
            assert_eq!(intoxicated_oss_frames_to_us(512, 48000), 10666);
            assert_eq!(intoxicated_oss_frames_to_us(48000, 0), 0);

            assert_eq!(intoxicated_oss_default_fragment_frames(48000), 128);
            assert_eq!(intoxicated_oss_default_fragment_frames(44100), 128);
            assert_eq!(intoxicated_oss_default_fragment_frames(96000), 256);
            // Default plan: 4 x 128 frames of S16 stereo = 4 x 512 B, the
            // shape the spec measured, 10.7 ms of buffer at 48 kHz.
            let f = intoxicated_oss_default_fragment_frames(48000);
            assert_eq!(intoxicated_oss_fragment_arg(4, f, 4), 0x0004_0009);
            assert_eq!(intoxicated_oss_frames_to_us(4 * f as i64, 48000), 10_666);
        }
    }

    #[test]
    fn fragment_override_parsing() {
        let parse = |s: &str| {
            let c = std::ffi::CString::new(s).unwrap();
            let (mut n, mut f) = (0, 0);
            let ok = unsafe { intoxicated_oss_parse_fragment(c.as_ptr(), &mut n, &mut f) };
            (ok, n, f)
        };
        assert_eq!(parse("4x256"), (1, 4, 256));
        assert_eq!(parse("2X128"), (1, 2, 128));
        assert_eq!(parse("1x256").0, 0);
        assert_eq!(parse("4x8").0, 0);
        assert_eq!(parse("4x256ms").0, 0);
        assert_eq!(parse("x256").0, 0);
        assert_eq!(parse("").0, 0);
    }

    // ------------------------------------------------------ state machine

    const OK: i32 = 0;
    const ERR_INVALID_STATE: i32 = -895;
    const ERR_UNIMPLEMENTED: i32 = -890;
    const ERR_DISCONNECTED: i32 = -899;
    const ERR_TIMEOUT: i32 = -885;
    const ERR_INVALID_FORMAT: i32 = -883;
    const OPEN: i32 = 2;
    const STARTED: i32 = 4;
    const PAUSING: i32 = 5;
    const PAUSED: i32 = 6;
    const STOPPING: i32 = 9;
    const STOPPED: i32 = 10;
    const CLOSED: i32 = 12;
    const DISCONNECTED: i32 = 13;

    type P = *mut c_void;
    type DataCb = extern "C" fn(P, P, P, i32) -> i32;
    type ErrCb = extern "C" fn(P, P, i32);

    struct Api {
        create: unsafe extern "C" fn(*mut P) -> i32,
        delete: unsafe extern "C" fn(P) -> i32,
        set_direction: unsafe extern "C" fn(P, i32),
        set_format: unsafe extern "C" fn(P, i32),
        set_data_cb: unsafe extern "C" fn(P, Option<DataCb>, P),
        set_err_cb: unsafe extern "C" fn(P, Option<ErrCb>, P),
        open: unsafe extern "C" fn(P, *mut P) -> i32,
        start: unsafe extern "C" fn(P) -> i32,
        pause: unsafe extern "C" fn(P) -> i32,
        stop: unsafe extern "C" fn(P) -> i32,
        close: unsafe extern "C" fn(P) -> i32,
        state: unsafe extern "C" fn(P) -> i32,
        wait: unsafe extern "C" fn(P, i32, *mut i32, i64) -> i32,
        read: unsafe extern "C" fn(P, P, i32, i64) -> i32,
        rate: unsafe extern "C" fn(P) -> i32,
        channels: unsafe extern "C" fn(P) -> i32,
        format: unsafe extern "C" fn(P) -> i32,
        burst: unsafe extern "C" fn(P) -> i32,
        capacity: unsafe extern "C" fn(P) -> i32,
        set_size: unsafe extern "C" fn(P, i32) -> i32,
    }

    fn api() -> Api {
        let syms = aaudio_overrides();
        let get = |n: &str| syms.iter().find(|(k, _)| *k == n).unwrap().1;
        // SAFETY: each address is the C++ entry point with the AAudio prototype.
        unsafe {
            use std::mem::transmute as t;
            Api {
                create: t(get("AAudio_createStreamBuilder")),
                delete: t(get("AAudioStreamBuilder_delete")),
                set_direction: t(get("AAudioStreamBuilder_setDirection")),
                set_format: t(get("AAudioStreamBuilder_setFormat")),
                set_data_cb: t(get("AAudioStreamBuilder_setDataCallback")),
                set_err_cb: t(get("AAudioStreamBuilder_setErrorCallback")),
                open: t(get("AAudioStreamBuilder_openStream")),
                start: t(get("AAudioStream_requestStart")),
                pause: t(get("AAudioStream_requestPause")),
                stop: t(get("AAudioStream_requestStop")),
                close: t(get("AAudioStream_close")),
                state: t(get("AAudioStream_getState")),
                wait: t(get("AAudioStream_waitForStateChange")),
                read: t(get("AAudioStream_read")),
                rate: t(get("AAudioStream_getSampleRate")),
                channels: t(get("AAudioStream_getChannelCount")),
                format: t(get("AAudioStream_getFormat")),
                burst: t(get("AAudioStream_getFramesPerBurst")),
                capacity: t(get("AAudioStream_getBufferCapacityInFrames")),
                set_size: t(get("AAudioStream_setBufferSizeInFrames")),
            }
        }
    }

    /// The state machine tests share the process-wide test-sink switch and
    /// the callback counters below, so they run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    static CALLS: AtomicU64 = AtomicU64::new(0);
    static FRAMES: AtomicU64 = AtomicU64::new(0);
    /// After this many calls the callback acts: see `BEHAVIOUR`.
    static ACT_AT: AtomicU64 = AtomicU64::new(u64::MAX);
    /// 0 nothing, 1 return STOP, 2 requestStop from inside, 3 try close from inside.
    static BEHAVIOUR: AtomicI32 = AtomicI32::new(0);
    static INSIDE_RESULT: AtomicI32 = AtomicI32::new(1);
    static ERRORS: AtomicI32 = AtomicI32::new(0);
    static LAST_ERROR: AtomicI32 = AtomicI32::new(0);
    static API_FOR_CB: std::sync::OnceLock<Api> = std::sync::OnceLock::new();

    extern "C" fn data_cb(stream: P, _user: P, buf: P, frames: i32) -> i32 {
        // Interleaved stereo float, one burst.
        unsafe { std::ptr::write_bytes(buf as *mut f32, 0, frames as usize * 2) };
        let n = CALLS.fetch_add(1, Ordering::SeqCst) + 1;
        FRAMES.fetch_add(frames as u64, Ordering::SeqCst);
        if n == ACT_AT.load(Ordering::SeqCst) {
            let api = API_FOR_CB.get_or_init(api);
            match BEHAVIOUR.load(Ordering::SeqCst) {
                1 => return 1,
                2 => INSIDE_RESULT.store(unsafe { (api.stop)(stream) }, Ordering::SeqCst),
                3 => INSIDE_RESULT.store(unsafe { (api.close)(stream) }, Ordering::SeqCst),
                _ => {}
            }
        }
        0
    }

    extern "C" fn err_cb(_stream: P, _user: P, error: i32) {
        ERRORS.fetch_add(1, Ordering::SeqCst);
        LAST_ERROR.store(error, Ordering::SeqCst);
    }

    fn reset() {
        CALLS.store(0, Ordering::SeqCst);
        FRAMES.store(0, Ordering::SeqCst);
        ACT_AT.store(u64::MAX, Ordering::SeqCst);
        BEHAVIOUR.store(0, Ordering::SeqCst);
        INSIDE_RESULT.store(1, Ordering::SeqCst);
        ERRORS.store(0, Ordering::SeqCst);
        LAST_ERROR.store(0, Ordering::SeqCst);
        unsafe { intoxicated_aaudio_use_test_sink(1) };
        let _ = API_FOR_CB.get_or_init(api);
    }

    fn open(api: &Api, direction: i32, format: i32, cb: bool) -> (i32, P) {
        unsafe {
            let mut b: P = std::ptr::null_mut();
            assert_eq!((api.create)(&mut b), OK);
            (api.set_direction)(b, direction);
            (api.set_format)(b, format);
            if cb {
                (api.set_data_cb)(b, Some(data_cb), std::ptr::null_mut());
                (api.set_err_cb)(b, Some(err_cb), std::ptr::null_mut());
            }
            let mut s: P = std::ptr::null_mut();
            let r = (api.open)(b, &mut s);
            assert_eq!((api.delete)(b), OK);
            (r, s)
        }
    }

    fn wait_until(what: impl Fn() -> bool, limit: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < limit {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        what()
    }

    #[test]
    fn output_lifecycle_open_start_pause_start_stop_close() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let api = api();
        let (r, s) = open(&api, 0, 0, true);
        assert_eq!(r, OK);
        unsafe {
            assert_eq!((api.state)(s), OPEN);
            assert_eq!((api.format)(s), 2, "UNSPECIFIED is delivered as PCM_FLOAT");
            assert_eq!((api.rate)(s), 48000);
            assert_eq!((api.channels)(s), 2);
            assert_eq!((api.burst)(s), 256);
            assert_eq!((api.capacity)(s), 768);
            assert_eq!((api.set_size)(s, 9600), 768, "the size in force, not the request");
            assert_eq!(CALLS.load(Ordering::SeqCst), 0, "no callbacks before start");

            assert_eq!((api.start)(s), OK);
            assert_eq!((api.state)(s), STARTED);
            // Real-time pacing: about 187.5 callbacks per second of 256 frames.
            std::thread::sleep(Duration::from_millis(300));
            let n = CALLS.load(Ordering::SeqCst);
            assert!((40..=70).contains(&n), "paced at the device rate, got {n} in 300 ms");
            assert!(FRAMES.load(Ordering::SeqCst) >= n * 256, "every callback is one whole burst");

            assert_eq!((api.pause)(s), OK);
            let st = (api.state)(s);
            assert!(st == PAUSING || st == PAUSED);
            let mut next = 0;
            if st == PAUSING {
                assert_eq!((api.wait)(s, PAUSING, &mut next, 1_000_000_000), OK);
                assert_eq!(next, PAUSED);
            }
            assert_eq!((api.state)(s), PAUSED);
            let frozen = CALLS.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(60));
            assert_eq!(CALLS.load(Ordering::SeqCst), frozen, "no callbacks while paused");

            // Timeout honoured when nothing changes.
            let t = Instant::now();
            assert_eq!((api.wait)(s, PAUSED, &mut next, 30_000_000), ERR_TIMEOUT);
            assert_eq!(next, PAUSED);
            assert!(t.elapsed() >= Duration::from_millis(29) && t.elapsed() < Duration::from_millis(500));

            assert_eq!((api.start)(s), OK);
            assert!(wait_until(|| CALLS.load(Ordering::SeqCst) > frozen + 5, Duration::from_secs(2)));

            assert_eq!((api.stop)(s), OK);
            assert!(wait_until(|| (api.state)(s) == STOPPED, Duration::from_secs(2)));
            let frozen = CALLS.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            assert_eq!(CALLS.load(Ordering::SeqCst), frozen, "no callbacks while stopped");

            let t = Instant::now();
            assert_eq!((api.close)(s), OK);
            assert!(t.elapsed() < Duration::from_millis(500), "close does not wait on drain");
            assert_eq!((api.state)(s), CLOSED);
            assert_eq!((api.start)(s), ERR_INVALID_STATE);
            assert_eq!((api.close)(s), ERR_INVALID_STATE);
            let mut next = 0;
            assert_eq!((api.wait)(s, OPEN, &mut next, 1_000_000), OK);
            assert_eq!(next, CLOSED);
        }
    }

    #[test]
    fn callback_stop_result_and_control_calls_from_inside() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let api = api();
        unsafe {
            // Returning STOP ends the callbacks and leaves the stream STOPPED, open.
            BEHAVIOUR.store(1, Ordering::SeqCst);
            ACT_AT.store(5, Ordering::SeqCst);
            let (r, s) = open(&api, 0, 0, true);
            assert_eq!(r, OK);
            assert_eq!((api.start)(s), OK);
            assert!(wait_until(|| (api.state)(s) == STOPPED, Duration::from_secs(2)));
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(CALLS.load(Ordering::SeqCst), 5);
            // Restartable.
            ACT_AT.store(u64::MAX, Ordering::SeqCst);
            assert_eq!((api.start)(s), OK);
            assert!(wait_until(|| CALLS.load(Ordering::SeqCst) > 8, Duration::from_secs(2)));
            assert_eq!((api.close)(s), OK);

            // requestStop from inside the callback: no deadlock.
            reset();
            BEHAVIOUR.store(2, Ordering::SeqCst);
            ACT_AT.store(3, Ordering::SeqCst);
            let (_, s) = open(&api, 0, 0, true);
            assert_eq!((api.start)(s), OK);
            let mut next = 0;
            assert!(wait_until(|| (api.state)(s) == STOPPED, Duration::from_secs(2)));
            assert_eq!(INSIDE_RESULT.load(Ordering::SeqCst), OK);
            assert_eq!((api.wait)(s, STOPPING, &mut next, 1_000_000), OK);
            assert_eq!(next, STOPPED);
            assert_eq!((api.close)(s), OK);

            // close from inside the callback is refused, not a hang.
            reset();
            BEHAVIOUR.store(3, Ordering::SeqCst);
            ACT_AT.store(3, Ordering::SeqCst);
            let (_, s) = open(&api, 0, 0, true);
            assert_eq!((api.start)(s), OK);
            assert!(wait_until(|| INSIDE_RESULT.load(Ordering::SeqCst) != 1, Duration::from_secs(2)));
            assert_eq!(INSIDE_RESULT.load(Ordering::SeqCst), ERR_INVALID_STATE);
            assert_eq!((api.state)(s), STARTED);
            assert_eq!((api.close)(s), OK);
        }
    }

    #[test]
    fn device_loss_disconnects_and_reports_once() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let api = api();
        unsafe {
            let (_, s) = open(&api, 0, 0, true);
            assert_eq!((api.start)(s), OK);
            assert!(wait_until(|| CALLS.load(Ordering::SeqCst) > 3, Duration::from_secs(2)));
            assert_eq!(intoxicated_aaudio_fail_test_sink(6 /* ENXIO */), 0);
            assert!(wait_until(|| ERRORS.load(Ordering::SeqCst) > 0, Duration::from_secs(2)));
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(ERRORS.load(Ordering::SeqCst), 1);
            assert_eq!(LAST_ERROR.load(Ordering::SeqCst), ERR_DISCONNECTED);
            assert_eq!((api.state)(s), DISCONNECTED);
            assert_eq!((api.start)(s), ERR_DISCONNECTED);
            assert_eq!((api.close)(s), OK);
        }
    }

    #[test]
    fn probe_streams_and_refusals() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let api = api();
        unsafe {
            // Output without callbacks opens and is never pulled.
            let (r, s) = open(&api, 0, 0, false);
            assert_eq!(r, OK);
            assert_eq!((api.start)(s), OK);
            assert_eq!((api.stop)(s), OK);
            assert_eq!((api.state)(s), STOPPED);
            assert_eq!((api.close)(s), OK);
            // Formats with no conversion here are refused, not faked.
            let (r, _) = open(&api, 0, 3 /* I24_PACKED */, true);
            assert_eq!(r, ERR_INVALID_FORMAT);
            // An integer request is honoured.
            let (r, s) = open(&api, 0, 1 /* I16 */, false);
            assert_eq!(r, OK);
            assert_eq!((api.format)(s), 1);
            assert_eq!((api.close)(s), OK);
            // Callback-driven capture is not a shape the engine uses.
            let (r, _) = open(&api, 1, 0, true);
            assert_eq!(r, ERR_UNIMPLEMENTED);
        }
    }

    #[test]
    fn input_declares_its_shape_and_reads_only_when_started() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let api = api();
        unsafe {
            let (r, s) = open(&api, 1, 0, false);
            assert_eq!(r, OK);
            assert_eq!((api.rate)(s), 48000);
            assert_eq!((api.channels)(s), 1);
            assert_eq!((api.format)(s), 1);
            assert_eq!((api.burst)(s), 480);
            assert_eq!((api.capacity)(s), 24000);
            let mut buf = [1i16; 480];
            let p = buf.as_mut_ptr() as P;
            assert_eq!((api.read)(s, p, 480, 0), ERR_INVALID_STATE);
            assert_eq!((api.start)(s), OK);
            assert_eq!((api.read)(s, p, 480, 100_000_000), 480);
            assert!(buf.iter().all(|&v| v == 0));
            assert_eq!((api.pause)(s), OK);
            assert_eq!((api.state)(s), PAUSED);
            assert_eq!((api.read)(s, p, 480, 0), ERR_INVALID_STATE);
            assert_eq!((api.start)(s), OK);
            assert_eq!((api.stop)(s), OK);
            assert_eq!((api.read)(s, p, 480, 0), ERR_INVALID_STATE);
            assert_eq!((api.close)(s), OK);
            assert_eq!((api.state)(s), CLOSED);
        }
    }
}
