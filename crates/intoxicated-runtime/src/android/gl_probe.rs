//! A diagnostic, not a wrapper: proves that the EGL and GLES entry points the
//! engine will be given work, by calling them through the built symbol table.
//!
//! `--gl-probe` draws into a 64x64 pbuffer and reads one pixel back;
//! `--window <seconds>` opens a host window and animates its clear colour.
//! Every function comes from the table's `libEGL.so` and `libGLESv2.so`
//! entries, stubs refused (a stub would "succeed" at everything). The one
//! exception is the window surface: the table's `eglCreateWindowSurface` is
//! the runtime's own, which takes the engine's `ANativeWindow`, so the window
//! probe asks the host driver for that one call with the X window itself.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CStr};

use crate::symtab::{Source, SymbolTable};

/// What the probe saw.
#[derive(Clone, Debug)]
pub struct Report {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    /// RGBA bytes of the pixel read back.
    pub pixel: [u8; 4],
}

type Display = *mut c_void;
type Config = *mut c_void;
type Surface = *mut c_void;
type Context = *mut c_void;

const EGL_NONE: c_int = 0x3038;
const EGL_SURFACE_TYPE: c_int = 0x3033;
const EGL_PBUFFER_BIT: c_int = 0x0001;
const EGL_WINDOW_BIT: c_int = 0x0004;
const EGL_RENDERABLE_TYPE: c_int = 0x3040;
const EGL_OPENGL_ES2_BIT: c_int = 0x0004;
const EGL_RED_SIZE: c_int = 0x3024;
const EGL_GREEN_SIZE: c_int = 0x3023;
const EGL_BLUE_SIZE: c_int = 0x3022;
const EGL_ALPHA_SIZE: c_int = 0x3021;
const EGL_WIDTH: c_int = 0x3057;
const EGL_HEIGHT: c_int = 0x3056;
const EGL_CONTEXT_CLIENT_VERSION: c_int = 0x3098;
const GL_COLOR_BUFFER_BIT: c_uint = 0x4000;
const GL_RGBA: c_uint = 0x1908;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_VENDOR: c_uint = 0x1F00;
const GL_RENDERER: c_uint = 0x1F01;
const GL_VERSION: c_uint = 0x1F02;

/// The 15 entry points, resolved from the table.
struct Gl {
    get_display: extern "C" fn(*mut c_void) -> Display,
    initialize: extern "C" fn(Display, *mut c_int, *mut c_int) -> c_uint,
    choose_config: extern "C" fn(Display, *const c_int, *mut Config, c_int, *mut c_int) -> c_uint,
    create_pbuffer: extern "C" fn(Display, Config, *const c_int) -> Surface,
    swap_buffers: extern "C" fn(Display, Surface) -> c_uint,
    create_context: extern "C" fn(Display, Config, Context, *const c_int) -> Context,
    make_current: extern "C" fn(Display, Surface, Surface, Context) -> c_uint,
    get_error: extern "C" fn() -> c_int,
    terminate: extern "C" fn(Display) -> c_uint,
    clear_color: extern "C" fn(f32, f32, f32, f32),
    clear: extern "C" fn(c_uint),
    read_pixels: extern "C" fn(c_int, c_int, c_int, c_int, c_uint, c_uint, *mut c_void),
    viewport: extern "C" fn(c_int, c_int, c_int, c_int),
    get_string: extern "C" fn(c_uint) -> *const c_char,
}

fn resolve(table: &SymbolTable, soname: &str, name: &str) -> Result<*mut c_void, String> {
    let entries = table.libraries.get(soname).ok_or_else(|| format!("{soname} is not in the symbol table"))?;
    let e = entries.iter().find(|e| e.symbol == name).ok_or_else(|| format!("{soname} has no {name}"))?;
    if e.source == Source::Stub {
        return Err(format!("{name} is a stub"));
    }
    Ok(e.address)
}

impl Gl {
    fn load(table: &SymbolTable) -> Result<Gl, String> {
        macro_rules! egl {
            ($n:literal) => {
                // SAFETY: the entry is the EGL function of this name.
                unsafe { std::mem::transmute(resolve(table, "libEGL.so", $n)?) }
            };
        }
        macro_rules! gles {
            ($n:literal) => {
                // SAFETY: the entry is the GLES function of this name.
                unsafe { std::mem::transmute(resolve(table, "libGLESv2.so", $n)?) }
            };
        }
        // Resolved for the check even though the window probe uses the host's.
        let _: *mut c_void = resolve(table, "libEGL.so", "eglCreateWindowSurface")?;
        Ok(Gl {
            get_display: egl!("eglGetDisplay"),
            initialize: egl!("eglInitialize"),
            choose_config: egl!("eglChooseConfig"),
            create_pbuffer: egl!("eglCreatePbufferSurface"),
            swap_buffers: egl!("eglSwapBuffers"),
            create_context: egl!("eglCreateContext"),
            make_current: egl!("eglMakeCurrent"),
            get_error: egl!("eglGetError"),
            terminate: egl!("eglTerminate"),
            clear_color: gles!("glClearColor"),
            clear: gles!("glClear"),
            read_pixels: gles!("glReadPixels"),
            viewport: gles!("glViewport"),
            get_string: gles!("glGetString"),
        })
    }

    fn fail(&self, call: &str) -> String {
        format!("{call} failed (EGL error {:#x})", (self.get_error)())
    }

    fn string(&self, which: c_uint) -> String {
        let p = (self.get_string)(which);
        if p.is_null() {
            return String::new();
        }
        // SAFETY: GL returns a static NUL-terminated string.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    /// Initialise `display`, choose a GLES2 RGBA8888 config for `surface_bit`.
    fn setup(&self, display: Display, surface_bit: c_int) -> Result<Config, String> {
        if display.is_null() {
            return Err(self.fail("eglGetDisplay"));
        }
        let (mut major, mut minor) = (0, 0);
        if (self.initialize)(display, &mut major, &mut minor) == 0 {
            return Err(self.fail("eglInitialize"));
        }
        let attribs = [
            EGL_SURFACE_TYPE,
            surface_bit,
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_ES2_BIT,
            EGL_RED_SIZE,
            8,
            EGL_GREEN_SIZE,
            8,
            EGL_BLUE_SIZE,
            8,
            EGL_ALPHA_SIZE,
            8,
            EGL_NONE,
        ];
        let mut config: Config = std::ptr::null_mut();
        let mut n = 0;
        if (self.choose_config)(display, attribs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 {
            return Err(self.fail("eglChooseConfig"));
        }
        Ok(config)
    }

    fn context(&self, display: Display, config: Config, surface: Surface) -> Result<Context, String> {
        let attribs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
        let ctx = (self.create_context)(display, config, std::ptr::null_mut(), attribs.as_ptr());
        if ctx.is_null() {
            return Err(self.fail("eglCreateContext"));
        }
        if (self.make_current)(display, surface, surface, ctx) == 0 {
            return Err(self.fail("eglMakeCurrent"));
        }
        Ok(ctx)
    }

    fn report(&self, pixel: [u8; 4]) -> Report {
        Report {
            vendor: self.string(GL_VENDOR),
            renderer: self.string(GL_RENDERER),
            version: self.string(GL_VERSION),
            pixel,
        }
    }

    fn read_pixel(&self, x: c_int, y: c_int) -> [u8; 4] {
        let mut px = [0u8; 4];
        (self.read_pixels)(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px.as_mut_ptr().cast());
        px
    }
}

/// The pbuffer probe: clear a 64x64 surface to (0x33, 0x66, 0x99, 0xff) and
/// read the centre back. Passes when every channel is within 1.
pub fn probe(table: &SymbolTable) -> Result<Report, String> {
    let gl = Gl::load(table)?;
    let display = (gl.get_display)(std::ptr::null_mut());
    let config = gl.setup(display, EGL_PBUFFER_BIT)?;
    let size = [EGL_WIDTH, 64, EGL_HEIGHT, 64, EGL_NONE];
    let surface = (gl.create_pbuffer)(display, config, size.as_ptr());
    if surface.is_null() {
        return Err(gl.fail("eglCreatePbufferSurface"));
    }
    gl.context(display, config, surface)?;
    (gl.viewport)(0, 0, 64, 64);
    (gl.clear_color)(0x33 as f32 / 255.0, 0x66 as f32 / 255.0, 0x99 as f32 / 255.0, 1.0);
    (gl.clear)(GL_COLOR_BUFFER_BIT);
    let pixel = gl.read_pixel(32, 32);
    let report = gl.report(pixel);
    (gl.terminate)(display);
    let want = [0x33u8, 0x66, 0x99, 0xff];
    if pixel.iter().zip(want).any(|(&g, w)| g.abs_diff(w) > 1) {
        return Err(format!("glReadPixels read {pixel:02x?}, cleared to {want:02x?}"));
    }
    Ok(report)
}

/// The window probe: a 1280x720 host window, its own display connection, and
/// `seconds` x 60 frames (at least one) of an animated clear colour at about
/// 60 a second, reading the centre pixel on the first.
pub fn probe_window(table: &SymbolTable, seconds: u64) -> Result<Report, String> {
    let gl = Gl::load(table)?;
    let window = crate::android::open_window(1280, 720, &crate::window_title("GL probe"))?;
    let display = (gl.get_display)(window.egl_native_display());
    let config = gl.setup(display, EGL_WINDOW_BIT)?;
    let create_window_surface = host_create_window_surface()?;
    let surface = create_window_surface(display, config, window.egl_native_window(), std::ptr::null());
    if surface.is_null() {
        return Err(gl.fail("eglCreateWindowSurface"));
    }
    gl.context(display, config, surface)?;
    (gl.viewport)(0, 0, 1280, 720);
    let frames = (seconds.saturating_mul(60)).max(1);
    let mut pixel = [0u8; 4];
    for frame in 0..frames {
        let t = frame as f32 / 60.0;
        (gl.clear_color)(0.5 + 0.5 * t.sin(), 0.5 + 0.5 * (t * 0.7).sin(), 0.5 + 0.5 * (t * 1.3).sin(), 1.0);
        (gl.clear)(GL_COLOR_BUFFER_BIT);
        if frame == 0 {
            pixel = gl.read_pixel(640, 360);
        }
        (gl.swap_buffers)(display, surface);
        std::thread::sleep(std::time::Duration::from_micros(16_667));
    }
    let report = gl.report(pixel);
    (gl.terminate)(display);
    Ok(report)
}

type CreateWindowSurface = extern "C" fn(Display, Config, c_ulong, *const c_int) -> Surface;

/// The host driver's `eglCreateWindowSurface`, which takes the X window.
fn host_create_window_surface() -> Result<CreateWindowSurface, String> {
    // SAFETY: dlopen/dlsym of the host's EGL, kept for the process.
    unsafe {
        let h = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if h.is_null() {
            return Err("libEGL.so.1 cannot be opened".into());
        }
        let f = libc::dlsym(h, c"eglCreateWindowSurface".as_ptr());
        if f.is_null() {
            return Err("libEGL.so.1 has no eglCreateWindowSurface".into());
        }
        Ok(std::mem::transmute::<*mut c_void, CreateWindowSurface>(f))
    }
}
