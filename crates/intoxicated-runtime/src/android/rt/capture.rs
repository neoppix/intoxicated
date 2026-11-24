//! The scene's geometry, captured with transform feedback as the engine
//! draws it.
//!
//! # What is captured
//!
//! Every vertex shader the engine creates on a device with transform
//! feedback (see `rt`) has its clip-space position decorated for capture
//! ([`super::spirv`]); nothing else of it changes. A graphics pipeline counts
//! as drawing visible scene geometry, and is captured, when its vertex
//! shader was instrumented, it has no tessellation or geometry stage, it
//! draws triangles (a list, strip or fan, not set dynamically), it does not
//! discard its primitives before rasterisation, and it writes colour (at
//! least one blend attachment with a non-zero write mask, not set
//! dynamically). So depth-only and shadow pipelines are never captured.
//!
//! Transform feedback is active only inside the scene passes (a depth
//! attachment the size of a swapchain image, the same test the reflections
//! use), and only while such a pipeline is bound: Vulkan does not allow a
//! pipeline to be bound while it is active, so it is ended before each bind
//! of the engine's and begun again after it, resuming where it stopped
//! through a counter buffer. A frame starts at the scene pass that clears
//! the depth: the first begin after it writes from the start of our buffer.
//! Shadow maps, the swapchain pass and anything else outside the scene
//! passes are never captured, whatever pipeline they use.
//!
//! Every triangle, of every instance, lands in our buffer as three clip-space
//! positions in the order drawn, strips and fans already split into separate
//! triangles: post-transform, post-skinning geometry, the player's own
//! character included, and only what the engine actually drew this frame.
//!
//! # The research switch
//!
//! `INTOXICATED_RT_DUMP=<dir>` writes every shader module the engine creates
//! to `<dir>` (`vs-<hash>.spv` for vertex shaders, `xx-<hash>.spv` for the
//! rest, the engine's original code), a line per graphics pipeline to
//! `<dir>/pipelines.tsv`, and makes devctl `rt draws` write the next frame's
//! scene draws to `<dir>/draws-<present>.tsv` and `rt dump <path>` the next
//! frame's captured triangles to `<path>`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::super::frame_capture::{self, device_fn, BufferCreateInfo, Dev, MemoryAllocateInfo, MemoryBarrier, MemoryRequirements, PhysicalDeviceMemoryProperties};
use super::super::vk_layer::passes::{Pass, Target};
use super::super::vk_layer::registry::{self, get};
use super::spirv;

const VK_SUCCESS: i32 = 0;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;

const ST_BUFFER_CREATE_INFO: i32 = 12;
const ST_MEMORY_ALLOCATE_INFO: i32 = 5;
const ST_MEMORY_BARRIER: i32 = 46;
const ST_GRAPHICS_PIPELINE_LIBRARY_CREATE_INFO: u32 = 1_000_320_002;

const STAGE_VERTEX: u32 = 0x1;
const STAGE_TESSELLATION: u32 = 0x2 | 0x4;
const STAGE_GEOMETRY: u32 = 0x8;

const DYNAMIC_PRIMITIVE_TOPOLOGY: u32 = 1_000_267_002;
const DYNAMIC_RASTERIZER_DISCARD_ENABLE: u32 = 1_000_377_001;
const DYNAMIC_COLOR_WRITE_ENABLE: u32 = 1_000_381_000;
const DYNAMIC_COLOR_WRITE_MASK: u32 = 1_000_455_011;

const PIPELINE_STAGE_DRAW_INDIRECT: u32 = 0x2;
const PIPELINE_STAGE_COMPUTE: u32 = 0x800;
const PIPELINE_STAGE_TRANSFER: u32 = 0x1000;
const PIPELINE_STAGE_HOST: u32 = 0x4000;
const PIPELINE_STAGE_TRANSFORM_FEEDBACK: u32 = 0x0100_0000;
const ACCESS_SHADER_READ: u32 = 0x20;
const ACCESS_TRANSFER_READ: u32 = 0x800;
const ACCESS_TRANSFER_WRITE: u32 = 0x1000;
const ACCESS_HOST_READ: u32 = 0x2000;
const ACCESS_XFB_WRITE: u32 = 0x0200_0000;
const ACCESS_XFB_COUNTER_READ: u32 = 0x0400_0000;
const ACCESS_XFB_COUNTER_WRITE: u32 = 0x0800_0000;

const USAGE_TRANSFER_SRC: u32 = 0x1;
const USAGE_TRANSFER_DST: u32 = 0x2;
const USAGE_STORAGE: u32 = 0x20;
const USAGE_XFB: u32 = 0x800;
const USAGE_XFB_COUNTER: u32 = 0x1000;

const MEMORY_DEVICE_LOCAL: u32 = 0x1;

const LOAD_OP_CLEAR: u32 = 1;
const CONTENTS_SECONDARY: u32 = 1;
const BIND_POINT_GRAPHICS: u32 = 0;

/// Bytes per captured triangle.
pub(crate) const TRIANGLE_BYTES: u64 = 3 * spirv::STRIDE as u64;

/// The triangles our buffer holds, unless `INTOXICATED_RT_TRIANGLES` says.
const DEFAULT_TRIANGLES: u64 = 2 << 20;

pub const DUMP_ENV: &str = "INTOXICATED_RT_DUMP";
pub const TRIANGLES_ENV: &str = "INTOXICATED_RT_TRIANGLES";

/// The dump directory, when the research switch is on.
pub(crate) fn dump_dir() -> Option<&'static str> {
    static DIR: OnceLock<Option<String>> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::var(DUMP_ENV).ok().filter(|d| !d.trim().is_empty())?;
        match std::fs::create_dir_all(&d) {
            Ok(()) => {
                println!("[android] vulkan: rt: shader modules and pipelines are written to {d} ({DUMP_ENV})");
                Some(d)
            }
            Err(e) => {
                println!("[android] vulkan: rt: {DUMP_ENV}={d} cannot be made ({e}); nothing is dumped");
                None
            }
        }
    })
    .as_deref()
}

pub(crate) fn capacity_triangles() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var(TRIANGLES_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&n| (1024..=(16 << 20)).contains(&n))
            .unwrap_or(DEFAULT_TRIANGLES)
    })
}

/// Whether the hooks of this module are handed out.
pub(crate) fn wanted() -> bool {
    super::requested() || dump_dir().is_some()
}

// ------------------------------------------------------------ tables

#[derive(Clone, Copy, Debug)]
struct Module {
    hash: u64,
    vertex: bool,
    instrumented: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Pipeline {
    pub vs: u64,
    pub fs: u64,
    /// Captured while bound in a scene pass, or why not.
    pub capture: Result<(), &'static str>,
    pub topology: u32,
}

/// One command buffer's recording, as far as capture goes.
#[derive(Default)]
struct Cmd {
    device: usize,
    /// Inside a scene pass.
    scene: bool,
    /// And capturing it is possible.
    capturing: bool,
    /// The graphics pipeline bound.
    bound: u64,
    active: bool,
    /// The draws of this frame are listed for `rt draws`.
    listing: bool,
    /// The scene passes of this frame copy our buffer out for `rt dump`.
    dumping: bool,
}

/// Our objects on one device.
struct Gpu {
    device: usize,
    fns: Fns,
    xfb: Owned,
    counter: Owned,
    /// Host-visible: the counter at 0, then (for dumps) the whole buffer.
    readback: Owned,
    readback_ptr: usize,
    readback_size: u64,
    capacity: u64,
    /// A begin since the frame started, so the next resumes.
    started: bool,
}

#[derive(Default, Clone, Copy)]
pub(super) struct Owned {
    pub(super) buffer: u64,
    pub(super) memory: u64,
}

#[derive(Default)]
pub(crate) struct Stats {
    pub frames: u64,
    pub begins: u64,
    pub binds_captured: u64,
    pub binds_skipped: u64,
    pub draws_captured: u64,
    pub draws_skipped: u64,
    pub modules_instrumented: u64,
    pub modules_left: u64,
    pub pipelines_captured: u64,
    pub pipelines_skipped: u64,
}

#[derive(Default)]
struct State {
    modules: HashMap<(usize, u64), Module>,
    pipelines: HashMap<(usize, u64), Pipeline>,
    cmds: HashMap<usize, Cmd>,
    gpus: Vec<Gpu>,
    failed: Vec<(usize, String)>,
    stats: Stats,
    /// Lines of the current `rt draws` listing.
    listing: Vec<String>,
    reported: std::collections::HashSet<String>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> MutexGuard<'static, State> {
    STATE.get_or_init(|| Mutex::new(State::default())).lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn once(&mut self, line: String) {
        if self.reported.insert(line.clone()) {
            println!("[android] vulkan: rt: {line}");
        }
    }
}

/// `rt draws` and `rt dump` armed for the next frame.
static LIST_NEXT: AtomicBool = AtomicBool::new(false);
static DUMP_NEXT: AtomicBool = AtomicBool::new(false);
/// The present count when the dump's frame was recorded, 0 for none yet.
static DUMPED_AT: AtomicU64 = AtomicU64::new(0);
static PRESENTS: AtomicU64 = AtomicU64::new(0);
/// The last frame's captured bytes, as last read back.
static LAST_BYTES: AtomicU64 = AtomicU64::new(0);

// ------------------------------------------------------------ hooks

static CREATE_SHADER_MODULE: AtomicUsize = AtomicUsize::new(0);
static DESTROY_SHADER_MODULE: AtomicUsize = AtomicUsize::new(0);
static CREATE_GRAPHICS_PIPELINES: AtomicUsize = AtomicUsize::new(0);
static DESTROY_PIPELINE: AtomicUsize = AtomicUsize::new(0);

fn next<F: Copy>(slot: &AtomicUsize) -> Option<F> {
    let p = slot.load(Relaxed);
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<usize>());
    // SAFETY: `F` is the function pointer type of the name the slot was
    // filled for, and a non-zero `p` came from the getter for that name.
    (p != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&p) })
}

/// From the getters' fall-through: ours in place of `next` for the four
/// creation commands, when this module is wanted.
pub(crate) fn hook(name: &[u8], next: *mut c_void) -> *mut c_void {
    if next.is_null() || !wanted() {
        return next;
    }
    let (slot, ours) = match name {
        b"vkCreateShaderModule" => (&CREATE_SHADER_MODULE, create_shader_module as *const ()),
        b"vkDestroyShaderModule" => (&DESTROY_SHADER_MODULE, destroy_shader_module as *const ()),
        b"vkCreateGraphicsPipelines" => (&CREATE_GRAPHICS_PIPELINES, create_graphics_pipelines as *const ()),
        b"vkDestroyPipeline" => (&DESTROY_PIPELINE, destroy_pipeline as *const ()),
        _ => return next,
    };
    slot.store(next as usize, Relaxed);
    ours as *mut c_void
}

type CreateFn = extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32;
type DestroyFn = extern "C" fn(*mut c_void, u64, *const c_void);
type CreatePipelinesFn = extern "C" fn(*mut c_void, u64, u32, *const c_void, *const c_void, *mut u64) -> i32;

/// The words of a `VkShaderModuleCreateInfo`.
///
/// # Safety
/// `info` must be a valid `VkShaderModuleCreateInfo`.
unsafe fn code_of(info: *const c_void) -> &'static [u32] {
    let (size, code): (usize, *const u32) = (get(info, 24), get(info, 32));
    if code.is_null() || size < 20 {
        return &[];
    }
    std::slice::from_raw_parts(code, size / 4)
}

extern "C" fn create_shader_module(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) = next::<CreateFn>(&CREATE_SHADER_MODULE) else { return VK_ERROR_INITIALIZATION_FAILED };
    if info.is_null() || out.is_null() {
        return f(device, info, alloc, out);
    }
    // SAFETY: the engine's create info, read during its own call.
    let words = unsafe { code_of(info) };
    let s = spirv::summary(words);
    let vertex = s.models.contains(&spirv::MODEL_VERTEX);
    let hash = spirv::hash(words);
    if let Some(dir) = dump_dir() {
        let path = format!("{dir}/{}-{hash:016x}.spv", if vertex { "vs" } else { "xx" });
        if !std::path::Path::new(&path).exists() {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let _ = std::fs::write(&path, bytes);
        }
    }
    let xfb = super::enabled(device as usize).transform_feedback;
    let instrumented = if vertex && xfb { Some(spirv::instrument_vertex(words)) } else { None };
    let mut rc = -1;
    let mut done = false;
    if let Some(Ok(code)) = &instrumented {
        // SAFETY: a copy of the engine's create info with our code; both
        // outlive the call.
        let mut copy: [u8; 40] = unsafe { get(info, 0) };
        unsafe {
            std::ptr::write_unaligned(copy.as_mut_ptr().add(24) as *mut usize, code.len() * 4);
            std::ptr::write_unaligned(copy.as_mut_ptr().add(32) as *mut *const u32, code.as_ptr());
        }
        rc = f(device, copy.as_ptr() as *const c_void, alloc, out);
        done = rc == VK_SUCCESS;
    }
    if !done {
        rc = f(device, info, alloc, out);
    }
    if rc == VK_SUCCESS {
        // SAFETY: success means the driver wrote the handle.
        let handle = unsafe { *out };
        let mut st = state();
        if vertex && xfb {
            if done {
                st.stats.modules_instrumented += 1;
            } else {
                st.stats.modules_left += 1;
                let why = match &instrumented {
                    Some(Err(why)) => why.to_string(),
                    _ => "the driver refused the instrumented module".to_string(),
                };
                st.once(format!("a vertex shader is not captured: {why} (said once per reason)"));
            }
        }
        st.modules.insert((device as usize, handle), Module { hash, vertex, instrumented: done });
    }
    rc
}

extern "C" fn destroy_shader_module(device: *mut c_void, module: u64, alloc: *const c_void) {
    state().modules.remove(&(device as usize, module));
    if let Some(f) = next::<DestroyFn>(&DESTROY_SHADER_MODULE) {
        f(device, module, alloc);
    }
}

/// Whether `states` lists `which`.
///
/// # Safety
/// `dynamic` must be null or a valid `VkPipelineDynamicStateCreateInfo`.
unsafe fn is_dynamic(dynamic: *const c_void, which: &[u32]) -> bool {
    if dynamic.is_null() {
        return false;
    }
    let (n, list): (u32, *const u32) = (get(dynamic, 20), get(dynamic, 24));
    !list.is_null() && std::slice::from_raw_parts(list, n as usize).iter().any(|s| which.contains(s))
}

/// What one `VkGraphicsPipelineCreateInfo` draws, from its own structures.
///
/// # Safety
/// `info` must be a valid `VkGraphicsPipelineCreateInfo`.
unsafe fn describe_pipeline(info: *const c_void, device: usize, modules: &HashMap<(usize, u64), Module>) -> Pipeline {
    let mut p = Pipeline { vs: 0, fs: 0, capture: Ok(()), topology: u32::MAX };
    let fail = |p: &mut Pipeline, why: &'static str| {
        if p.capture.is_ok() {
            p.capture = Err(why);
        }
    };
    if !registry::find_in_chain(get(info, 8), ST_GRAPHICS_PIPELINE_LIBRARY_CREATE_INFO).is_null() {
        fail(&mut p, "a pipeline library");
    }
    let (n, stages): (u32, *const u8) = (get(info, 20), get(info, 24));
    let mut vertex = None;
    for i in 0..n as usize {
        if stages.is_null() {
            break;
        }
        let s = stages.add(i * 48) as *const c_void;
        let (stage, module): (u32, u64) = (get(s, 20), get(s, 24));
        let m = modules.get(&(device, module));
        match stage {
            STAGE_VERTEX => {
                p.vs = m.map_or(0, |m| m.hash);
                vertex = Some(m.copied());
            }
            0x10 => p.fs = m.map_or(0, |m| m.hash),
            s if s & (STAGE_TESSELLATION | STAGE_GEOMETRY) != 0 => fail(&mut p, "a tessellation or geometry stage"),
            _ => {}
        }
    }
    match vertex {
        None => fail(&mut p, "no vertex stage"),
        Some(None) => fail(&mut p, "its vertex shader is not a module the client saw"),
        Some(Some(m)) if !m.vertex => fail(&mut p, "its vertex stage has no vertex entry point"),
        Some(Some(m)) if !m.instrumented => fail(&mut p, "its vertex shader is not instrumented"),
        _ => {}
    }
    let dynamic: *const c_void = get(info, 96);
    let ia: *const c_void = get(info, 40);
    if ia.is_null() {
        fail(&mut p, "no input assembly state");
    } else {
        p.topology = get(ia, 20);
        if !(3..=5).contains(&p.topology) {
            fail(&mut p, "it does not draw triangles");
        }
    }
    if is_dynamic(dynamic, &[DYNAMIC_PRIMITIVE_TOPOLOGY]) {
        fail(&mut p, "its topology is dynamic");
    }
    let rs: *const c_void = get(info, 64);
    if rs.is_null() || get::<u32>(rs, 24) != 0 || is_dynamic(dynamic, &[DYNAMIC_RASTERIZER_DISCARD_ENABLE]) {
        fail(&mut p, "it may discard its primitives");
    }
    let cb: *const c_void = get(info, 88);
    let writes_colour = !cb.is_null() && {
        let (n, atts): (u32, *const u8) = (get(cb, 28), get(cb, 32));
        !atts.is_null() && (0..n as usize).any(|i| get::<u32>(atts.add(i * 32) as *const c_void, 28) != 0)
    };
    if !writes_colour {
        fail(&mut p, "it writes no colour (depth only)");
    }
    if is_dynamic(dynamic, &[DYNAMIC_COLOR_WRITE_ENABLE, DYNAMIC_COLOR_WRITE_MASK]) {
        fail(&mut p, "its colour writes are dynamic");
    }
    p
}

extern "C" fn create_graphics_pipelines(
    device: *mut c_void,
    cache: u64,
    count: u32,
    infos: *const c_void,
    alloc: *const c_void,
    out: *mut u64,
) -> i32 {
    let Some(f) = next::<CreatePipelinesFn>(&CREATE_GRAPHICS_PIPELINES) else { return VK_ERROR_INITIALIZATION_FAILED };
    let rc = f(device, cache, count, infos, alloc, out);
    if infos.is_null() || out.is_null() {
        return rc;
    }
    let mut st = state();
    let mut lines = Vec::new();
    for i in 0..count as usize {
        // SAFETY: the engine's array of `count` create infos (144 bytes
        // each) and the handles the driver wrote; a failed one is null.
        let (info, handle) = unsafe { ((infos as *const u8).add(i * 144) as *const c_void, *out.add(i)) };
        if handle == 0 {
            continue;
        }
        // SAFETY: as above.
        let p = unsafe { describe_pipeline(info, device as usize, &st.modules) };
        if p.capture.is_ok() {
            st.stats.pipelines_captured += 1;
        } else {
            st.stats.pipelines_skipped += 1;
        }
        if dump_dir().is_some() {
            lines.push(format!(
                "0x{handle:x}\tvs-{:016x}\tfs-{:016x}\ttopology={}\t{}\n",
                p.vs,
                p.fs,
                p.topology,
                match p.capture {
                    Ok(()) => "captured",
                    Err(why) => why,
                }
            ));
        }
        st.pipelines.insert((device as usize, handle), p);
    }
    drop(st);
    if let (Some(dir), false) = (dump_dir(), lines.is_empty()) {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}/pipelines.tsv")) {
            let _ = file.write_all(lines.concat().as_bytes());
        }
    }
    rc
}

extern "C" fn destroy_pipeline(device: *mut c_void, pipeline: u64, alloc: *const c_void) {
    state().pipelines.remove(&(device as usize, pipeline));
    if let Some(f) = next::<DestroyFn>(&DESTROY_PIPELINE) {
        f(device, pipeline, alloc);
    }
}

// ------------------------------------------------------------ recording

/// `vkBeginCommandBuffer`.
pub(crate) fn cmd_begun(cmd: *mut c_void) {
    state().cmds.insert(cmd as usize, Cmd::default());
}

/// A pass begins, before the begin is forwarded. A scene pass that clears
/// the depth starts a frame: the barrier that keeps this frame's capture
/// from overwriting what the last one's readers still use is recorded here,
/// outside the pass.
pub(crate) fn pass_beginning(pass: &Pass) {
    let (depth, full) = {
        let reg = super::super::vk_layer::passes::registry();
        let d = super::super::ssr::describe(pass, &reg);
        (d.depth, reg.swapchain_extents())
    };
    let mut st = state();
    let cmd = pass.cmd;
    let Some(z) = depth.filter(|d| registry::is_depth_format(d.format) && full.contains(&d.extent)) else {
        if let Some(c) = st.cmds.get_mut(&cmd) {
            c.scene = false;
            c.capturing = false;
        }
        return;
    };
    let secondary = matches!(pass.target, Target::RenderPass { contents, .. } if contents == CONTENTS_SECONDARY);
    let device = z.device;
    let xfb = super::enabled(device).transform_feedback;
    let starts = z.load == LOAD_OP_CLEAR;
    if starts && LIST_NEXT.swap(false, Relaxed) {
        st.listing.clear();
        if let Some(c) = st.cmds.get_mut(&cmd) {
            c.listing = true;
        }
    }
    if starts && xfb && DUMP_NEXT.swap(false, Relaxed) {
        if let Some(c) = st.cmds.get_mut(&cmd) {
            c.dumping = true;
        }
    }
    let c = st.cmds.entry(cmd).or_default();
    c.device = device;
    c.scene = true;
    c.capturing = false;
    c.active = false;
    if !xfb {
        return;
    }
    if secondary {
        st.once("a scene pass records its draws in secondary command buffers, which cannot be captured".into());
        return;
    }
    if let Err(why) = ensure_gpu(&mut st, device) {
        st.once(format!("nothing is captured: {why}"));
        return;
    }
    let g = st.gpus.iter_mut().find(|g| g.device == device).expect("made above");
    if starts {
        g.started = false;
        // The last frame's readers of our buffers (the trace's compute, the
        // readback's copies) finish before this frame writes them.
        g.barrier(
            cmd as *mut c_void,
            PIPELINE_STAGE_COMPUTE | PIPELINE_STAGE_TRANSFER,
            PIPELINE_STAGE_TRANSFORM_FEEDBACK | PIPELINE_STAGE_DRAW_INDIRECT,
            ACCESS_SHADER_READ | ACCESS_TRANSFER_READ,
            ACCESS_XFB_WRITE | ACCESS_XFB_COUNTER_READ | ACCESS_XFB_COUNTER_WRITE,
        );
        st.stats.frames += 1;
    }
    if let Some(c) = st.cmds.get_mut(&cmd) {
        c.capturing = true;
    }
}

/// After a pass's begin was forwarded: inside a scene pass, our buffer is
/// bound and capture begins if the bound pipeline is one to capture.
pub(crate) fn pass_started(cmd: *mut c_void) {
    let mut st = state();
    let Some(c) = st.cmds.get(&(cmd as usize)) else { return };
    if !c.capturing {
        return;
    }
    let (device, bound) = (c.device, c.bound);
    let Some(g) = st.gpus.iter().find(|g| g.device == device) else { return };
    let sizes = [g.capacity];
    (g.fns.bind_xfb)(cmd, 0, 1, &g.xfb.buffer, &0u64, sizes.as_ptr());
    begin_if_captured(&mut st, cmd, device, bound);
}

fn begin_if_captured(st: &mut State, cmd: *mut c_void, device: usize, pipeline: u64) {
    let captured = st.pipelines.get(&(device, pipeline)).is_some_and(|p| p.capture.is_ok());
    if pipeline != 0 {
        if captured {
            st.stats.binds_captured += 1;
        } else {
            st.stats.binds_skipped += 1;
        }
    }
    if !captured {
        return;
    }
    let Some(g) = st.gpus.iter_mut().find(|g| g.device == device) else { return };
    if g.started {
        (g.fns.begin_xfb)(cmd, 0, 1, &g.counter.buffer, &0u64);
    } else {
        (g.fns.begin_xfb)(cmd, 0, 0, std::ptr::null(), std::ptr::null());
        g.started = true;
    }
    st.stats.begins += 1;
    if let Some(c) = st.cmds.get_mut(&(cmd as usize)) {
        c.active = true;
    }
}

fn end_if_active(st: &mut State, cmd: *mut c_void) {
    let Some(c) = st.cmds.get_mut(&(cmd as usize)) else { return };
    if !c.active {
        return;
    }
    c.active = false;
    let device = c.device;
    if let Some(g) = st.gpus.iter().find(|g| g.device == device) {
        (g.fns.end_xfb)(cmd, 0, 1, &g.counter.buffer, &0u64);
    }
}

/// Before the engine's `vkCmdBindPipeline` is forwarded.
pub(crate) fn binding(cmd: *mut c_void, bind_point: u32) {
    if bind_point != BIND_POINT_GRAPHICS {
        return;
    }
    end_if_active(&mut state(), cmd);
}

/// After it was forwarded.
pub(crate) fn bound(cmd: *mut c_void, bind_point: u32, pipeline: u64) {
    if bind_point != BIND_POINT_GRAPHICS {
        return;
    }
    let mut st = state();
    let Some(c) = st.cmds.get_mut(&(cmd as usize)) else { return };
    c.bound = pipeline;
    if !c.capturing {
        return;
    }
    let device = c.device;
    begin_if_captured(&mut st, cmd, device, pipeline);
}

/// Before `vkCmdNextSubpass` or `vkCmdExecuteCommands`: capture stops for
/// the rest of the pass, which neither allows while it is active.
pub(crate) fn interrupted(cmd: *mut c_void) {
    let mut st = state();
    end_if_active(&mut st, cmd);
    if let Some(c) = st.cmds.get_mut(&(cmd as usize)) {
        c.capturing = false;
    }
}

/// Before a pass's end is forwarded.
pub(crate) fn pass_ending(cmd: *mut c_void) {
    end_if_active(&mut state(), cmd);
}

/// After it was forwarded, outside the pass: what was written is made
/// visible to the next scene pass and to the compute that reads it, and the
/// counter (and for a dump, everything) is copied where the host reads it.
pub(crate) fn pass_ended(cmd: *mut c_void) {
    let mut st = state();
    let Some(c) = st.cmds.get_mut(&(cmd as usize)) else { return };
    let capturing = c.capturing;
    c.scene = false;
    c.capturing = false;
    if !capturing {
        return;
    }
    let (device, dumping) = (c.device, c.dumping);
    let presents = PRESENTS.load(Relaxed);
    let Some(g) = st.gpus.iter().find(|g| g.device == device) else { return };
    let cmd_ = cmd;
    g.barrier(
        cmd_,
        PIPELINE_STAGE_TRANSFORM_FEEDBACK,
        PIPELINE_STAGE_TRANSFORM_FEEDBACK | PIPELINE_STAGE_DRAW_INDIRECT | PIPELINE_STAGE_TRANSFER | PIPELINE_STAGE_COMPUTE,
        ACCESS_XFB_WRITE | ACCESS_XFB_COUNTER_WRITE,
        ACCESS_XFB_WRITE | ACCESS_XFB_COUNTER_READ | ACCESS_XFB_COUNTER_WRITE | ACCESS_TRANSFER_READ | ACCESS_SHADER_READ,
    );
    let mut regions = vec![[0u64, 0, 16]];
    if dumping && g.readback_size >= 256 + g.capacity {
        regions.push([0, 256, g.capacity]);
    }
    (g.fns.copy_buffer)(cmd_, g.counter.buffer, g.readback.buffer, 1, regions[0].as_ptr());
    if regions.len() > 1 {
        (g.fns.copy_buffer)(cmd_, g.xfb.buffer, g.readback.buffer, 1, regions[1].as_ptr());
        DUMPED_AT.store(presents.max(1), Relaxed);
    }
    g.barrier(cmd_, PIPELINE_STAGE_TRANSFER, PIPELINE_STAGE_HOST, ACCESS_TRANSFER_WRITE, ACCESS_HOST_READ);
}

/// A draw in `cmd`, for the counts and `rt draws`.
pub(crate) fn drew(cmd: *mut c_void, kind: &'static str, count: u32, instances: u32) {
    let mut st = state();
    let Some(c) = st.cmds.get(&(cmd as usize)) else { return };
    if !c.scene && !c.listing {
        return;
    }
    let (active, listing, bound, device, scene) = (c.active, c.listing, c.bound, c.device, c.scene);
    if c.capturing {
        if active {
            st.stats.draws_captured += 1;
        } else {
            st.stats.draws_skipped += 1;
        }
    }
    if listing && scene {
        let p = st.pipelines.get(&(device, bound)).cloned();
        let line = format!(
            "0x{:x}\t0x{bound:x}\t{kind}\t{count}\t{instances}\t{}\tvs-{:016x}\tfs-{:016x}\n",
            cmd as usize,
            match p.as_ref().map(|p| p.capture) {
                Some(Ok(())) => "captured",
                Some(Err(why)) => why,
                None => "unknown pipeline",
            },
            p.as_ref().map_or(0, |p| p.vs),
            p.as_ref().map_or(0, |p| p.fs),
        );
        st.listing.push(line);
    }
}

/// From `vkQueuePresentKHR`.
pub(crate) fn frame_end(presents: u64) {
    PRESENTS.store(presents, Relaxed);
    let mut st = state();
    let mut finished_listing = false;
    for c in st.cmds.values_mut() {
        if c.listing {
            c.listing = false;
            finished_listing = true;
        }
        c.dumping = false;
    }
    if finished_listing {
        if let Some(dir) = dump_dir() {
            let path = format!("{dir}/draws-{presents}.tsv");
            let mut text = String::from("cmd\tpipeline\tkind\tcount\tinstances\tcapture\tvs\tfs\n");
            text.push_str(&st.listing.concat());
            match std::fs::write(&path, text) {
                Ok(()) => println!("[android] vulkan: rt: {} scene draws written to {path}", st.listing.len()),
                Err(e) => println!("[android] vulkan: rt: {path}: {e}"),
            }
        }
        st.listing.clear();
    }
    if let Some(g) = st.gpus.first() {
        if g.readback_ptr != 0 {
            // SAFETY: our mapping, at least 16 bytes; the GPU writes it
            // through a copy, and a torn or old value only skews a count.
            let bytes = unsafe { std::ptr::read_volatile(g.readback_ptr as *const u32) } as u64;
            LAST_BYTES.store(bytes, Relaxed);
        }
    }
}

/// Before `vkDestroyDevice` is forwarded.
pub(crate) fn device_destroying(device: *mut c_void) {
    if !wanted() {
        return;
    }
    let mut st = state();
    let (gone, keep): (Vec<Gpu>, Vec<Gpu>) = std::mem::take(&mut st.gpus).into_iter().partition(|g| g.device == device as usize);
    st.gpus = keep;
    for g in gone {
        g.destroy();
    }
    st.modules.retain(|(d, _), _| *d != device as usize);
    st.pipelines.retain(|(d, _), _| *d != device as usize);
    st.failed.retain(|(d, _)| *d != device as usize);
    st.cmds.clear();
}

/// The capture buffers of `device` when this frame's scene was captured
/// (transform feedback began at least once since its depth was cleared):
/// the triangles, the counter of bytes written and the capacity in bytes.
pub(crate) fn frame_captured(device: usize) -> Option<(u64, u64, u64)> {
    let st = state();
    st.gpus.iter().find(|g| g.device == device && g.started).map(|g| (g.xfb.buffer, g.counter.buffer, g.capacity))
}

/// The bytes the last frames captured, as last read back.
pub(crate) fn last_bytes() -> u64 {
    LAST_BYTES.load(Relaxed)
}

// ------------------------------------------------------------ devctl

pub(crate) fn stats_line() -> String {
    let st = state();
    let s = &st.stats;
    let bytes = LAST_BYTES.load(Relaxed);
    format!(
        "triangles={} capacity={} frames={} begins={} binds_captured={} binds_skipped={} draws_captured={} draws_skipped={} \
         vs_instrumented={} vs_left={} pipelines_captured={} pipelines_skipped={}",
        bytes / TRIANGLE_BYTES,
        capacity_triangles(),
        s.frames,
        s.begins,
        s.binds_captured,
        s.binds_skipped,
        s.draws_captured,
        s.draws_skipped,
        s.modules_instrumented,
        s.modules_left,
        s.pipelines_captured,
        s.pipelines_skipped
    )
}

/// devctl `rt draws`: the next frame's scene draws to the dump directory.
pub(crate) fn request_draws() -> Result<String, String> {
    let dir = dump_dir().ok_or_else(|| format!("{DUMP_ENV}=<dir> at start is needed"))?;
    LIST_NEXT.store(true, Relaxed);
    Ok(format!("the next frame's scene draws go to {dir}/draws-<present>.tsv"))
}

/// devctl `rt dump <path>`: the next frame's captured triangles, as a
/// little-endian `u32` byte count and then that many bytes of clip-space
/// positions (`vec4` per vertex, three per triangle).
pub(crate) fn dump(path: &str) -> Result<String, String> {
    if dump_dir().is_none() {
        return Err(format!("{DUMP_ENV}=<dir> at start is needed"));
    }
    DUMPED_AT.store(0, Relaxed);
    DUMP_NEXT.store(true, Relaxed);
    let t0 = std::time::Instant::now();
    loop {
        let at = DUMPED_AT.load(Relaxed);
        if at != 0 && PRESENTS.load(Relaxed) >= at + 6 {
            break;
        }
        if t0.elapsed() > std::time::Duration::from_secs(5) {
            DUMP_NEXT.store(false, Relaxed);
            return Err("no scene frame was captured within 5 s".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let st = state();
    let g = st.gpus.first().ok_or("no capture on any device")?;
    // SAFETY: our mapping of `readback_size` bytes; the copies into it are
    // done (six presents later), and a later frame's counter copy only
    // changes the first word, read here first.
    let (bytes, data) = unsafe {
        let bytes = (std::ptr::read_volatile(g.readback_ptr as *const u32) as u64).min(g.capacity);
        (bytes, std::slice::from_raw_parts((g.readback_ptr + 256) as *const u8, bytes as usize).to_vec())
    };
    drop(st);
    let mut out = (bytes as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&data);
    std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))?;
    Ok(format!("{} triangles to {path}", bytes / TRIANGLE_BYTES))
}

// ------------------------------------------------------------ our objects

pub(super) struct Fns {
    pub(super) create_buffer: extern "C" fn(Dev, *const BufferCreateInfo, *const c_void, *mut u64) -> i32,
    pub(super) destroy_buffer: extern "C" fn(Dev, u64, *const c_void),
    pub(super) buffer_requirements: extern "C" fn(Dev, u64, *mut MemoryRequirements),
    pub(super) allocate_memory: extern "C" fn(Dev, *const MemoryAllocateInfo, *const c_void, *mut u64) -> i32,
    pub(super) free_memory: extern "C" fn(Dev, u64, *const c_void),
    pub(super) bind_buffer_memory: extern "C" fn(Dev, u64, u64, u64) -> i32,
    pub(super) map_memory: extern "C" fn(Dev, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    #[allow(clippy::type_complexity)]
    pub(super) pipeline_barrier:
        extern "C" fn(*mut c_void, u32, u32, u32, u32, *const MemoryBarrier, u32, *const c_void, u32, *const c_void),
    pub(super) copy_buffer: extern "C" fn(*mut c_void, u64, u64, u32, *const u64),
    pub(super) bind_xfb: extern "C" fn(*mut c_void, u32, u32, *const u64, *const u64, *const u64),
    pub(super) begin_xfb: extern "C" fn(*mut c_void, u32, u32, *const u64, *const u64),
    pub(super) end_xfb: extern "C" fn(*mut c_void, u32, u32, *const u64, *const u64),
}

impl Fns {
    pub(super) fn resolve(d: Dev) -> Result<Fns, String> {
        Ok(Fns {
            create_buffer: device_fn(d, c"vkCreateBuffer")?,
            destroy_buffer: device_fn(d, c"vkDestroyBuffer")?,
            buffer_requirements: device_fn(d, c"vkGetBufferMemoryRequirements")?,
            allocate_memory: device_fn(d, c"vkAllocateMemory")?,
            free_memory: device_fn(d, c"vkFreeMemory")?,
            bind_buffer_memory: device_fn(d, c"vkBindBufferMemory")?,
            map_memory: device_fn(d, c"vkMapMemory")?,
            pipeline_barrier: device_fn(d, c"vkCmdPipelineBarrier")?,
            copy_buffer: device_fn(d, c"vkCmdCopyBuffer")?,
            bind_xfb: device_fn(d, c"vkCmdBindTransformFeedbackBuffersEXT")?,
            begin_xfb: device_fn(d, c"vkCmdBeginTransformFeedbackEXT")?,
            end_xfb: device_fn(d, c"vkCmdEndTransformFeedbackEXT")?,
        })
    }
}

pub(super) fn check(rc: i32, what: &str) -> Result<(), String> {
    if rc == VK_SUCCESS {
        Ok(())
    } else {
        Err(format!("{what} returned {rc}"))
    }
}

pub(super) fn memory_types() -> Result<Vec<u32>, String> {
    let (_, physical) = frame_capture::current_device().ok_or("no Vulkan device is known")?;
    let mp = super::super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties");
    if mp.is_null() {
        return Err("vkGetPhysicalDeviceMemoryProperties is unavailable".into());
    }
    // SAFETY: resolved by the host loader for exactly this name.
    let mp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties) = unsafe { std::mem::transmute(mp) };
    let mut props = PhysicalDeviceMemoryProperties {
        memory_type_count: 0,
        memory_types: [[0; 2]; 32],
        memory_heap_count: 0,
        memory_heaps: [[0; 2]; 16],
    };
    mp(physical as *mut c_void, &mut props);
    Ok(props.memory_types[..(props.memory_type_count as usize).min(32)].iter().map(|t| t[0]).collect())
}

/// A buffer of `size` bytes in memory of the first type with `want`, or, for
/// `host`, the readback type `frame_capture` would choose.
pub(super) fn make_buffer(
    fns: &Fns,
    d: Dev,
    types: &[u32],
    size: u64,
    usage: u32,
    host: bool,
) -> Result<Owned, String> {
    let none = std::ptr::null();
    let info = BufferCreateInfo {
        s_type: ST_BUFFER_CREATE_INFO,
        p_next: none,
        flags: 0,
        size,
        usage,
        sharing_mode: 0,
        queue_family_index_count: 0,
        p_queue_family_indices: none.cast(),
    };
    let mut o = Owned::default();
    check((fns.create_buffer)(d, &info, none, &mut o.buffer), "vkCreateBuffer")?;
    let mut req = MemoryRequirements::default();
    (fns.buffer_requirements)(d, o.buffer, &mut req);
    let index = if host {
        frame_capture::readback_memory_type(types, req.memory_type_bits).map(|c| c.index)
    } else {
        (0..types.len())
            .find(|&i| req.memory_type_bits & (1 << i) != 0 && types[i] & MEMORY_DEVICE_LOCAL != 0)
            .map(|i| i as u32)
            .ok_or_else(|| "no device-local memory type fits the buffer".to_string())
    };
    let index = match index {
        Ok(i) => i,
        Err(e) => {
            (fns.destroy_buffer)(d, o.buffer, none);
            return Err(e);
        }
    };
    let alloc = MemoryAllocateInfo { s_type: ST_MEMORY_ALLOCATE_INFO, p_next: none, allocation_size: req.size, memory_type_index: index };
    let made = check((fns.allocate_memory)(d, &alloc, none, &mut o.memory), "vkAllocateMemory")
        .and_then(|()| check((fns.bind_buffer_memory)(d, o.buffer, o.memory, 0), "vkBindBufferMemory"));
    if let Err(e) = made {
        (fns.destroy_buffer)(d, o.buffer, none);
        if o.memory != 0 {
            (fns.free_memory)(d, o.memory, none);
        }
        return Err(e);
    }
    Ok(o)
}

fn ensure_gpu(st: &mut State, device: usize) -> Result<(), String> {
    if st.gpus.iter().any(|g| g.device == device) {
        return Ok(());
    }
    if let Some((_, why)) = st.failed.iter().find(|(d, _)| *d == device) {
        return Err(why.clone());
    }
    match make_gpu(device) {
        Ok(g) => {
            st.once(format!(
                "capturing the scene on device 0x{device:x}: room for {} triangles ({} MiB)",
                g.capacity / TRIANGLE_BYTES,
                g.capacity >> 20
            ));
            st.gpus.push(g);
            Ok(())
        }
        Err(why) => {
            st.failed.push((device, why.clone()));
            Err(why)
        }
    }
}

fn make_gpu(device: usize) -> Result<Gpu, String> {
    let (current, _) = frame_capture::current_device().ok_or("no Vulkan device is known")?;
    if current != device {
        return Err("the scene is on another device than the one the client follows".into());
    }
    let d = device as Dev;
    let fns = Fns::resolve(d)?;
    let types = memory_types()?;
    let capacity = capacity_triangles() * TRIANGLE_BYTES;
    let readback_size = if dump_dir().is_some() { 256 + capacity } else { 256 };
    let mut g = Gpu {
        device,
        fns,
        xfb: Owned::default(),
        counter: Owned::default(),
        readback: Owned::default(),
        readback_ptr: 0,
        readback_size,
        capacity,
        started: false,
    };
    let made = (|| {
        g.xfb = make_buffer(&g.fns, d, &types, capacity, USAGE_XFB | USAGE_STORAGE | USAGE_TRANSFER_SRC | USAGE_TRANSFER_DST, false)?;
        g.counter = make_buffer(&g.fns, d, &types, 256, USAGE_XFB_COUNTER | USAGE_STORAGE | USAGE_TRANSFER_SRC | USAGE_TRANSFER_DST, false)?;
        g.readback = make_buffer(&g.fns, d, &types, readback_size, USAGE_TRANSFER_DST, true)?;
        let mut p = std::ptr::null_mut();
        check((g.fns.map_memory)(d, g.readback.memory, 0, readback_size, 0, &mut p), "vkMapMemory")?;
        g.readback_ptr = p as usize;
        Ok::<(), String>(())
    })();
    match made {
        Ok(()) => Ok(g),
        Err(e) => {
            g.destroy();
            Err(e)
        }
    }
}

impl Gpu {
    fn barrier(&self, cmd: *mut c_void, src_stage: u32, dst_stage: u32, src: u32, dst: u32) {
        let b = MemoryBarrier { s_type: ST_MEMORY_BARRIER, p_next: std::ptr::null(), src_access_mask: src, dst_access_mask: dst };
        (self.fns.pipeline_barrier)(cmd, src_stage, dst_stage, 0, 1, &b, 0, std::ptr::null(), 0, std::ptr::null());
    }

    fn destroy(self) {
        let (d, none) = (self.device as Dev, std::ptr::null());
        for o in [self.xfb, self.counter, self.readback] {
            if o.buffer != 0 {
                (self.fns.destroy_buffer)(d, o.buffer, none);
            }
            if o.memory != 0 {
                (self.fns.free_memory)(d, o.memory, none);
            }
        }
    }
}

