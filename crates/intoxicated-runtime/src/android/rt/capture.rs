//! The scene's geometry, captured with transform feedback as the engine
//! draws it.
//!
//! # What is captured
//!
//! Every vertex shader of lit world geometry the engine creates on a device
//! with transform feedback (see `rt`) has its clip-space position, its
//! colour output and its camera vector output decorated for capture
//! ([`super::spirv`]); nothing it computes changes. Other shaders
//! (particles, billboards, the sky) are left alone and never captured.
//! A graphics pipeline counts as drawing visible scene geometry, and is
//! captured, when its vertex shader was instrumented, it has no
//! tessellation or geometry stage, it draws triangles (a list, strip or
//! fan, not set dynamically; strips and fans without primitive restart), it
//! does not discard its primitives before rasterisation, it writes colour
//! (at least one blend attachment with a non-zero write mask, not set
//! dynamically) and it does not blend. So depth-only and shadow pipelines
//! are never captured, nor transparent ones, nor the interface the engine
//! draws in the world (text and images on signs and boards, name tags):
//! its fragment shader clips it to its frame through alpha, and its
//! triangles reach well past what shows (a scrolling text on a display
//! showed in reflections beside the display).
//!
//! Transform feedback is active only inside the scene passes (a depth
//! attachment the size of a swapchain image, no swapchain image among the
//! colour attachments; Timebomb Duels has two a frame, the 3D scene and a
//! second one that loads its depth), and only around the draws of such a
//! pipeline. Vulkan does not allow a pipeline to be bound while it is
//! active, so it is ended before each bind of the engine's and begun again
//! at the next draw to capture. Each begin writes at the offset our own
//! count of the frame's vertices has reached, which the client works out
//! from each draw's vertex or index count, instance count and topology as
//! it is recorded; the frame starts at 0 with the first scene pass after a
//! present, and the trace is told the total. Resuming through the
//! extension's counter buffer instead did not work: inside a render pass
//! there is no barrier between an end that writes the counter and the next
//! begin that reads it, and in game the begins read stale counters, so
//! draws overwrote each other (a frame of about 100000 triangles came out as
//! 2400) or frames ran on into each other until the buffer was full.
//! Indirect draws, whose counts only the GPU knows, are not captured (the
//! lobby has none), nor is a draw for which the buffer has no room left.
//! Shadow maps, the swapchain pass and anything else outside the scene
//! passes are never captured, whatever pipeline they use.
//!
//! Every triangle, of every instance, lands in our buffer as three vertices
//! (clip-space position, the colour the vertex shader passes on, the vector
//! from the vertex to the camera in the world) in the order drawn, strips and fans
//! already split into separate triangles: post-transform, post-skinning
//! geometry, the player's own character included, and only what the engine
//! actually drew this frame. Pipelines that draw strips or fans with
//! primitive restart are not captured: how many triangles they write is
//! not known when they are recorded.
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

const DYNAMIC_SCISSOR: u32 = 1;
const DYNAMIC_SCISSOR_WITH_COUNT: u32 = 1_000_267_004;
const DYNAMIC_PRIMITIVE_TOPOLOGY: u32 = 1_000_267_002;
const DYNAMIC_RASTERIZER_DISCARD_ENABLE: u32 = 1_000_377_001;
const DYNAMIC_PRIMITIVE_RESTART_ENABLE: u32 = 1_000_377_004;
const DYNAMIC_COLOR_WRITE_ENABLE: u32 = 1_000_381_000;
const DYNAMIC_COLOR_WRITE_MASK: u32 = 1_000_455_011;

const PIPELINE_STAGE_COMPUTE: u32 = 0x800;
const PIPELINE_STAGE_TRANSFER: u32 = 0x1000;
const PIPELINE_STAGE_HOST: u32 = 0x4000;
const PIPELINE_STAGE_TRANSFORM_FEEDBACK: u32 = 0x0100_0000;
const ACCESS_SHADER_READ: u32 = 0x20;
const ACCESS_TRANSFER_READ: u32 = 0x800;
const ACCESS_TRANSFER_WRITE: u32 = 0x1000;
const ACCESS_HOST_READ: u32 = 0x2000;
const ACCESS_XFB_WRITE: u32 = 0x0200_0000;

const USAGE_TRANSFER_SRC: u32 = 0x1;
const USAGE_TRANSFER_DST: u32 = 0x2;
const USAGE_STORAGE: u32 = 0x20;
const USAGE_XFB: u32 = 0x800;

const MEMORY_DEVICE_LOCAL: u32 = 0x1;

const CONTENTS_SECONDARY: u32 = 1;
const BIND_POINT_GRAPHICS: u32 = 0;

/// Texture serials: the low 30 bits of a draw's code; its top two say how
/// the texture combines with the colour (see `spirv::Albedo`).
pub(crate) const SERIAL_MASK: u32 = (1 << 30) - 1;
pub(crate) const MODE_COLOUR: u32 = 1 << 30;
pub(crate) const MODE_DETAIL: u32 = 2 << 30;
/// The engine's terrain: no colour captured; the trace learns it from the
/// screen (see `build.comp` INSERT and `ssr.comp`'s GI pass).
pub(crate) const MODE_TERRAIN: u32 = 3 << 30;
/// Base colour textures a frame can use.
pub(crate) const MAX_TEXTURES: usize = 256;

/// Bytes per captured triangle.
pub(crate) const TRIANGLE_BYTES: u64 = 3 * spirv::STRIDE as u64;

/// The triangles our buffer holds, unless `INTOXICATED_RT_TRIANGLES` says
/// (144 MiB; Timebomb Duels draws 30000 to 180000 a frame).
const DEFAULT_TRIANGLES: u64 = 1 << 19;

pub const DUMP_ENV: &str = "INTOXICATED_RT_DUMP";
pub const TRIANGLES_ENV: &str = "INTOXICATED_RT_TRIANGLES";
pub const MEMORY_ENV: &str = "INTOXICATED_RT_MEMORY";

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

/// The triangles our buffers are sized for: `INTOXICATED_RT_TRIANGLES` when
/// set, else what `INTOXICATED_RT_MEMORY` (MiB) allows, else what an eighth
/// of the GPU's largest device-local heap allows, at most 1536 MiB; a power
/// of two from 131072 to 4194304. Each triangle takes about 600 bytes over
/// the capture, the list to trace, the hierarchy and the cache.
pub(crate) fn capacity_triangles() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    if let Some(&n) = N.get() {
        return n;
    }
    if let Some(n) = std::env::var(TRIANGLES_ENV).ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|&n| (1024..=(16 << 20)).contains(&n)) {
        return *N.get_or_init(|| n);
    }
    let asked = std::env::var(MEMORY_ENV).ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|&m| m >= 64);
    let budget = match (asked, device_local_heap()) {
        (Some(mib), _) => mib << 20,
        (None, Some(heap)) => (heap / 8).min(1536 << 20),
        // No device yet: the default, not kept.
        (None, None) => return DEFAULT_TRIANGLES,
    };
    let n = triangles_for(budget);
    *N.get_or_init(|| {
        println!("[android] vulkan: rt: room for {n} triangles ({} MiB of video memory; {MEMORY_ENV})", (n * BYTES_PER_TRIANGLE) >> 20);
        n
    })
}

/// The most a dump copies to the host (the first triangles of the frame).
const DUMP_BYTES: u64 = 128 << 20;

/// Bytes a triangle of room takes over every buffer sized by it.
pub(crate) const BYTES_PER_TRIANGLE: u64 = 600;

/// The power of two of triangles a budget of bytes allows.
pub(crate) fn triangles_for(budget: u64) -> u64 {
    let n = (budget / BYTES_PER_TRIANGLE).clamp(1 << 17, 1 << 22);
    1 << (63 - n.leading_zeros())
}

/// The size of the GPU's largest device-local heap.
fn device_local_heap() -> Option<u64> {
    let (_, physical) = frame_capture::current_device()?;
    let mp = super::super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties");
    if mp.is_null() {
        return None;
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
    props.memory_heaps[..(props.memory_heap_count as usize).min(16)].iter().filter(|h| h[1] & 1 != 0).map(|h| h[0]).max()
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
    /// A vertex shader's colour and texture coordinate output locations.
    outputs: Option<spirv::VertexInfo>,
    /// A fragment shader's base colour texture.
    albedo: Option<spirv::Albedo>,
    /// A fragment shader that gives off light (the Neon material).
    emission: Option<spirv::Emission>,
}

#[derive(Clone, Debug)]
pub(crate) struct Pipeline {
    pub vs: u64,
    pub fs: u64,
    /// Captured while bound in a scene pass, or why not.
    pub capture: Result<(), &'static str>,
    pub topology: u32,
    /// The scissor fixed in the pipeline, if it is not dynamic.
    pub scissor: Option<[i32; 4]>,
    /// The base colour texture's descriptor set and binding, and whether it
    /// is a grey detail texture (see `spirv::Albedo`), when the fragment
    /// shader samples it at the texture coordinates the vertex shader
    /// passes on and multiplies it with the colour it passes on, both of
    /// which are captured.
    pub albedo: Option<(u32, u32, bool)>,
    /// It blends: its triangles count only where their colour's alpha says
    /// they are opaque (see `build.comp` INSERT).
    pub blends: bool,
    /// The light its fragment shader gives off, packed (see
    /// `spirv::Emission::packed`), or 0.
    pub emission: u32,
    /// It draws the engine's terrain (see `spirv::VertexInfo::terrain`).
    pub terrain: bool,
}

/// One command buffer's recording, as far as capture goes.
#[derive(Default)]
struct Cmd {
    device: usize,
    /// Inside a scene pass.
    scene: bool,
    /// And capturing it is possible.
    capturing: bool,
    /// The colour of the frame's first scene pass, to copy when it ends.
    opaque: Option<super::super::ssr::Att>,
    /// The pass's extent, and the scissor last set (x, y, width, height).
    extent: [u32; 2],
    scissor: Option<[i32; 4]>,
    /// The graphics pipeline bound.
    bound: u64,
    active: bool,
    /// The draws of this frame are listed for `rt draws`.
    listing: bool,
    /// The scene passes of this frame copy our buffer out for `rt dump`.
    dumping: bool,
    /// The graphics descriptor sets bound, by set number, and what was
    /// pushed: (set, binding) to (image view, layout).
    sets: [u64; 8],
    pushed: HashMap<(u32, u32), (u64, u32)>,
    /// The engine's per-frame uniform block (set 0, binding 0): pushed
    /// (buffer, offset, range), and the first dynamic offset bound with set 0.
    pushed_frame: Option<(u64, u64, u64)>,
    set0_dynamic: u32,
}

/// Our objects on one device.
struct Gpu {
    device: usize,
    fns: Fns,
    xfb: Owned,
    /// Host-visible: for dumps, the whole buffer from byte 256.
    readback: Owned,
    readback_ptr: usize,
    readback_size: u64,
    capacity: u64,
    /// Anything was captured since the buffer was made.
    started: bool,
    /// The present count when the current frame started.
    frame: u64,
    /// The bytes this frame's draws write, as counted when recorded.
    bytes: u64,
    /// This frame's base colour textures: (first triangle, serial and mode,
    /// whether the pipeline blends) each time either changes, and the views
    /// (serial, view, layout).
    draws: Vec<[u32; 4]>,
    textures: Vec<(u32, u64, u32)>,
    /// The per-frame uniform block a captured draw last read (buffer,
    /// offset, range): the engine's lighting, read by the trace.
    frame_block: Option<(u64, u64, u64)>,
    /// A draw to capture found no room this frame.
    overflowed: bool,
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
    /// Skipped because the buffer was full.
    pub draws_overflowed: u64,
    /// Skipped because a scissor smaller than the pass cut them.
    pub draws_clipped: u64,
    /// Captured with their base colour texture known.
    pub draws_textured: u64,
    /// Captured draws of the Neon material, which give off light.
    pub draws_emissive: u64,
    /// Captured draws of the engine's terrain.
    pub draws_terrain: u64,
    /// The last frames' time in our hooks (ns) and their draws.
    pub cpu: std::collections::VecDeque<(u64, u64)>,
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
    /// What happened in this frame and the last, in recording order: scene
    /// passes (the depth's load op, the colour attachments, the triangles
    /// before and after) and the trace.
    frame_log: Vec<String>,
    last_log: String,
    reported: std::collections::HashSet<String>,
    /// Descriptor sets the engine wrote: set to binding to (image view,
    /// layout), for the images it samples.
    set_views: HashMap<u64, HashMap<u32, (u64, u32)>>,
    /// Its update templates: the image entries (binding, offset, stride),
    /// and the set number of a push template.
    templates: HashMap<u64, (Vec<(u32, usize)>, u32)>,
    /// A number for each image view a captured draw used, kept while the
    /// view lives, so cached triangles can name their texture.
    view_serials: HashMap<u64, u32>,
    next_serial: u32,
    /// Binding 0 of each descriptor set the engine wrote as a uniform
    /// buffer: (buffer, offset, range, dynamic).
    set_frames: HashMap<u64, (u64, u64, u64, bool)>,
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
/// The last frame's captured bytes.
static LAST_BYTES: AtomicU64 = AtomicU64::new(0);
/// The bytes the dump's frame copied out.
static DUMPED_BYTES: AtomicU64 = AtomicU64::new(0);

// ------------------------------------------------------------ hooks

static CREATE_SHADER_MODULE: AtomicUsize = AtomicUsize::new(0);
static DESTROY_SHADER_MODULE: AtomicUsize = AtomicUsize::new(0);
static CREATE_GRAPHICS_PIPELINES: AtomicUsize = AtomicUsize::new(0);
static DESTROY_PIPELINE: AtomicUsize = AtomicUsize::new(0);
static CMD_SET_SCISSOR: AtomicUsize = AtomicUsize::new(0);
static UPDATE_SETS: AtomicUsize = AtomicUsize::new(0);
static CREATE_TEMPLATE: AtomicUsize = AtomicUsize::new(0);
static UPDATE_WITH_TEMPLATE: AtomicUsize = AtomicUsize::new(0);
static CMD_BIND_SETS: AtomicUsize = AtomicUsize::new(0);
static CMD_PUSH: AtomicUsize = AtomicUsize::new(0);
static CMD_PUSH_TEMPLATE: AtomicUsize = AtomicUsize::new(0);
static DESTROY_VIEW: AtomicUsize = AtomicUsize::new(0);

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
        b"vkCmdSetScissor" => (&CMD_SET_SCISSOR, cmd_set_scissor as *const ()),
        b"vkUpdateDescriptorSets" => (&UPDATE_SETS, update_sets as *const ()),
        b"vkCreateDescriptorUpdateTemplate" | b"vkCreateDescriptorUpdateTemplateKHR" => {
            (&CREATE_TEMPLATE, create_template as *const ())
        }
        b"vkUpdateDescriptorSetWithTemplate" | b"vkUpdateDescriptorSetWithTemplateKHR" => {
            (&UPDATE_WITH_TEMPLATE, update_with_template as *const ())
        }
        b"vkCmdBindDescriptorSets" => (&CMD_BIND_SETS, cmd_bind_sets as *const ()),
        b"vkCmdPushDescriptorSetKHR" => (&CMD_PUSH, cmd_push as *const ()),
        b"vkCmdPushDescriptorSetWithTemplateKHR" => (&CMD_PUSH_TEMPLATE, cmd_push_template as *const ()),
        b"vkDestroyImageView" => (&DESTROY_VIEW, destroy_view as *const ()),
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
        let outputs = if done { spirv::vertex_info(words) } else { None };
        let albedo = if !vertex && xfb { spirv::fragment_albedo(words) } else { None };
        let emission = if !vertex && xfb { spirv::fragment_emission(words) } else { None };
        st.modules.insert((device as usize, handle), Module { hash, vertex, instrumented: done, outputs, albedo, emission });
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
    let mut p = Pipeline { vs: 0, fs: 0, capture: Ok(()), topology: u32::MAX, scissor: None, albedo: None, blends: false, emission: 0, terrain: false };
    let mut fragment = None;
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
                p.terrain = m.is_some_and(|m| m.outputs.is_some_and(|o| o.terrain));
                vertex = Some(m.copied());
            }
            0x10 => {
                p.fs = m.map_or(0, |m| m.hash);
                fragment = m.and_then(|m| m.albedo);
                p.emission = m.and_then(|m| m.emission).map_or(0, spirv::Emission::packed);
            }
            s if s & (STAGE_TESSELLATION | STAGE_GEOMETRY) != 0 => fail(&mut p, "a tessellation or geometry stage"),
            _ => {}
        }
    }
    if let (Some(Some(v)), Some(a)) = (vertex, fragment) {
        if let Some(o) = v.outputs {
            if o.colour == a.colour && o.uv == Some(a.uv) {
                p.albedo = Some((a.set, a.binding, a.scalar));
            }
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
        // Restarts would make what a strip writes unknown when recorded.
        if p.topology != 3 && (get::<u32>(ia, 24) != 0 || is_dynamic(dynamic, &[DYNAMIC_PRIMITIVE_RESTART_ENABLE])) {
            fail(&mut p, "it draws strips or fans with primitive restart");
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
    // Blending: transparent parts, and the interface the engine draws in the
    // world (SurfaceGui and BillboardGui text and images, name tags), whose
    // fragment shader clips it to its frame by alpha, so its triangles reach
    // well past what shows. Only opaque geometry is captured.
    let blends = !cb.is_null() && {
        let (n, atts): (u32, *const u8) = (get(cb, 28), get(cb, 32));
        !atts.is_null()
            && (0..n as usize).any(|i| {
                let a = atts.add(i * 32) as *const c_void;
                get::<u32>(a, 0) != 0 && get::<u32>(a, 28) != 0
            })
    };
    // A blending pipeline is captured only for characters: a skinned lit
    // mesh (the engine draws characters' heads so, opaque or nearly), and
    // then only its triangles whose colour is opaque (see `build.comp`).
    // Blending static parts are left out: the game's nearly invisible
    // barriers and zone markers are drawn so, some with their transparency
    // where the capture cannot see it; and so is the interface drawn in the
    // world.
    // Neon parts are captured too, blending or not (the engine draws them
    // so in some places): they give off light, and are traced as opaque
    // wherever their colour is not nearly invisible (see `build.comp`).
    let character = matches!(vertex, Some(Some(m)) if m.outputs.is_some_and(|o| o.normal && o.skinned));
    p.blends = blends;
    if blends && !character && p.emission == 0 {
        fail(&mut p, "it blends and is not a character (see-through parts, interface drawn in the world)");
    }
    if is_dynamic(dynamic, &[DYNAMIC_COLOR_WRITE_ENABLE, DYNAMIC_COLOR_WRITE_MASK]) {
        fail(&mut p, "its colour writes are dynamic");
    }
    // A scissor fixed in the pipeline (`VkPipelineViewportStateCreateInfo`:
    // scissorCount at 32, pScissors at 40), unless it is dynamic.
    let vs: *const c_void = get(info, 56);
    if !vs.is_null() && !is_dynamic(dynamic, &[DYNAMIC_SCISSOR, DYNAMIC_SCISSOR_WITH_COUNT]) {
        let (n, rects): (u32, *const c_void) = (get(vs, 32), get(vs, 40));
        if n > 0 && !rects.is_null() {
            p.scissor = Some(get(rects, 0));
        }
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
                    Ok(()) if p.emission != 0 => "captured, gives off light (Neon)",
                    Ok(()) if p.terrain => "captured, terrain (colour learnt from the screen)",
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

// ------------------------------------------------------------ textures
//
// Which image each captured draw samples as its base colour: the engine's
// descriptor writes (sets, update templates, pushed descriptors) are noted
// as they pass, and at a draw the pipeline's base colour binding is looked
// up in what is bound. Only combined image samplers and sampled images, and
// only element 0 of a binding.

const DESCRIPTOR_COMBINED_IMAGE_SAMPLER: u32 = 1;
const DESCRIPTOR_SAMPLED_IMAGE: u32 = 2;

/// The (binding, view, layout) of each image in `count` `VkWriteDescriptorSet`
/// (72 bytes each) for a set.
///
/// # Safety
/// `writes` must be `count` valid `VkWriteDescriptorSet`.
unsafe fn image_writes(count: u32, writes: *const u8) -> Vec<(u64, u32, u64, u32)> {
    let mut out = Vec::new();
    if writes.is_null() {
        return out;
    }
    for i in 0..count as usize {
        let w = writes.add(i * 72) as *const c_void;
        let (set, binding, element, n, kind, images): (u64, u32, u32, u32, u32, *const u8) =
            (get(w, 16), get(w, 24), get(w, 28), get(w, 32), get(w, 36), get(w, 40));
        if element != 0 || n == 0 || images.is_null() || !matches!(kind, DESCRIPTOR_COMBINED_IMAGE_SAMPLER | DESCRIPTOR_SAMPLED_IMAGE) {
            continue;
        }
        // `VkDescriptorImageInfo`: sampler, view, layout.
        let (view, layout): (u64, u32) = (get(images as *const c_void, 8), get(images as *const c_void, 16));
        out.push((set, binding, view, layout));
    }
    out
}

/// A template entry that is the per-frame uniform block (bit 0: dynamic).
const FRAME_BLOCK_ENTRY: u32 = 1 << 31;

/// The per-frame uniform block a template writes: (buffer, offset, range,
/// dynamic).
///
/// # Safety
/// `data` must be what the engine passes with this template.
unsafe fn template_frame_block(entries: &[(u32, usize)], data: *const u8) -> Option<(u64, u64, u64, bool)> {
    if data.is_null() {
        return None;
    }
    let &(tag, offset) = entries.iter().find(|e| e.0 & FRAME_BLOCK_ENTRY != 0)?;
    let b = data.add(offset) as *const c_void;
    Some((get(b, 0), get(b, 8), get(b, 16), tag & 1 != 0))
}

/// The (binding, view, layout) a template writes from `data`.
///
/// # Safety
/// `data` must be what the engine passes with this template.
unsafe fn template_images(entries: &[(u32, usize)], data: *const u8) -> Vec<(u32, u64, u32)> {
    if data.is_null() {
        return Vec::new();
    }
    entries
        .iter()
        .filter(|e| e.0 & FRAME_BLOCK_ENTRY == 0)
        .map(|&(binding, offset)| {
            let info = data.add(offset) as *const c_void;
            (binding, get::<u64>(info, 8), get::<u32>(info, 16))
        })
        .collect()
}

const DESCRIPTOR_UNIFORM_BUFFER: u32 = 6;
const DESCRIPTOR_UNIFORM_BUFFER_DYNAMIC: u32 = 8;

/// The uniform buffer at binding 0, element 0, among `count`
/// `VkWriteDescriptorSet`: (set, buffer, offset, range, dynamic).
///
/// # Safety
/// `writes` must be `count` valid `VkWriteDescriptorSet`.
unsafe fn frame_block_writes(count: u32, writes: *const u8) -> Vec<(u64, u64, u64, u64, bool)> {
    let mut out = Vec::new();
    if writes.is_null() {
        return out;
    }
    for i in 0..count as usize {
        let w = writes.add(i * 72) as *const c_void;
        let (set, binding, element, n, kind, buffers): (u64, u32, u32, u32, u32, *const u8) =
            (get(w, 16), get(w, 24), get(w, 28), get(w, 32), get(w, 36), get(w, 48));
        if binding != 0 || element != 0 || n == 0 || buffers.is_null() {
            continue;
        }
        if !matches!(kind, DESCRIPTOR_UNIFORM_BUFFER | DESCRIPTOR_UNIFORM_BUFFER_DYNAMIC) {
            continue;
        }
        // `VkDescriptorBufferInfo`: buffer, offset, range.
        let b = buffers as *const c_void;
        out.push((set, get(b, 0), get(b, 8), get(b, 16), kind == DESCRIPTOR_UNIFORM_BUFFER_DYNAMIC));
    }
    out
}

extern "C" fn update_sets(device: *mut c_void, count: u32, writes: *const u8, copies: u32, copy_list: *const c_void) {
    let timed = Timed::start();
    // SAFETY: the engine's own array, read during its call.
    let found = unsafe { image_writes(count, writes) };
    // SAFETY: as above.
    let blocks = unsafe { frame_block_writes(count, writes) };
    if !found.is_empty() || !blocks.is_empty() {
        let mut st = state();
        for (set, binding, view, layout) in found {
            st.set_views.entry(set).or_default().insert(binding, (view, layout));
        }
        for (set, buffer, offset, range, dynamic) in blocks {
            st.set_frames.insert(set, (buffer, offset, range, dynamic));
        }
    }
    drop(timed);
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, *const u8, u32, *const c_void)>(&UPDATE_SETS) {
        f(device, count, writes, copies, copy_list);
    }
}

extern "C" fn create_template(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) = next::<CreateFn>(&CREATE_TEMPLATE) else { return VK_ERROR_INITIALIZATION_FAILED };
    let rc = f(device, info, alloc, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() {
        // SAFETY: `VkDescriptorUpdateTemplateCreateInfo`: entryCount 20,
        // pEntries 24, set 64; each entry (32 bytes) binding 0, arrayElement
        // 4, count 8, type 12, offset 16, stride 24.
        let (entries, set) = unsafe {
            let (n, list, set): (u32, *const u8, u32) = (get(info, 20), get(info, 24), get(info, 64));
            let mut e = Vec::new();
            for i in 0..if list.is_null() { 0 } else { n as usize } {
                let x = list.add(i * 32) as *const c_void;
                let (binding, element, count, kind, offset): (u32, u32, u32, u32, usize) =
                    (get(x, 0), get(x, 4), get(x, 8), get(x, 12), get(x, 16));
                if element == 0 && count > 0 && matches!(kind, DESCRIPTOR_COMBINED_IMAGE_SAMPLER | DESCRIPTOR_SAMPLED_IMAGE) {
                    e.push((binding, offset));
                }
                // The per-frame uniform block, by an impossible binding
                // number so the image readers pass it by.
                if binding == 0 && element == 0 && count > 0 && matches!(kind, DESCRIPTOR_UNIFORM_BUFFER | DESCRIPTOR_UNIFORM_BUFFER_DYNAMIC) {
                    e.push((FRAME_BLOCK_ENTRY | u32::from(kind == DESCRIPTOR_UNIFORM_BUFFER_DYNAMIC), offset));
                }
            }
            (e, set)
        };
        // SAFETY: success means the driver wrote the handle.
        let handle = unsafe { *out };
        state().templates.insert(handle, (entries, set));
    }
    rc
}

extern "C" fn update_with_template(device: *mut c_void, set: u64, template: u64, data: *const u8) {
    {
        let _timed = Timed::start();
        let mut st = state();
        if let Some((entries, _)) = st.templates.get(&template) {
            // SAFETY: the engine's data for this template, read during its call.
            let found = unsafe { template_images(entries, data) };
            // SAFETY: as above.
            let block = unsafe { template_frame_block(entries, data) };
            let views = st.set_views.entry(set).or_default();
            for (binding, view, layout) in found {
                views.insert(binding, (view, layout));
            }
            if let Some(b) = block {
                st.set_frames.insert(set, b);
            }
        }
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u64, u64, *const u8)>(&UPDATE_WITH_TEMPLATE) {
        f(device, set, template, data);
    }
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_bind_sets(
    cmd: *mut c_void,
    bind_point: u32,
    layout: u64,
    first: u32,
    count: u32,
    sets: *const u64,
    dynamic_count: u32,
    dynamic: *const u32,
) {
    if bind_point == BIND_POINT_GRAPHICS && !sets.is_null() {
        let _timed = Timed::start();
        let mut st = state();
        if let Some(c) = st.cmds.get_mut(&(cmd as usize)) {
            for i in 0..count {
                if let Some(slot) = c.sets.get_mut((first + i) as usize) {
                    // SAFETY: the engine's array of `count` sets.
                    *slot = unsafe { *sets.add(i as usize) };
                }
            }
            // Dynamic offsets go in set and binding order: set 0's first,
            // binding 0 its first when it is dynamic.
            if first == 0 && dynamic_count > 0 && !dynamic.is_null() {
                // SAFETY: the engine's array of `dynamic_count` offsets.
                c.set0_dynamic = unsafe { *dynamic };
            }
        }
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u64, u32, *const u32)>(&CMD_BIND_SETS) {
        f(cmd, bind_point, layout, first, count, sets, dynamic_count, dynamic);
    }
}

extern "C" fn cmd_push(cmd: *mut c_void, bind_point: u32, layout: u64, set: u32, count: u32, writes: *const u8) {
    if bind_point == BIND_POINT_GRAPHICS {
        let _timed = Timed::start();
        // SAFETY: the engine's own array, read during its call.
        let found = unsafe { image_writes(count, writes) };
        // SAFETY: as above.
        let blocks = unsafe { frame_block_writes(count, writes) };
        let mut st = state();
        if let Some(c) = st.cmds.get_mut(&(cmd as usize)) {
            for (_, binding, view, layout) in found {
                c.pushed.insert((set, binding), (view, layout));
            }
            if set == 0 {
                if let Some(&(_, buffer, offset, range, _)) = blocks.first() {
                    c.pushed_frame = Some((buffer, offset, range));
                }
            }
        }
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u8)>(&CMD_PUSH) {
        f(cmd, bind_point, layout, set, count, writes);
    }
}

extern "C" fn cmd_push_template(cmd: *mut c_void, template: u64, layout: u64, set: u32, data: *const u8) {
    {
        let _timed = Timed::start();
        let mut st = state();
        if let Some((entries, _)) = st.templates.get(&template) {
            // SAFETY: the engine's data for this template, read during its call.
            let found = unsafe { template_images(entries, data) };
            // SAFETY: as above.
            let block = unsafe { template_frame_block(entries, data) };
            if let Some(c) = st.cmds.get_mut(&(cmd as usize)) {
                for (binding, view, layout) in found {
                    c.pushed.insert((set, binding), (view, layout));
                }
                if let (0, Some((buffer, offset, range, _))) = (set, block) {
                    c.pushed_frame = Some((buffer, offset, range));
                }
            }
        }
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u64, u64, u32, *const u8)>(&CMD_PUSH_TEMPLATE) {
        f(cmd, template, layout, set, data);
    }
}

extern "C" fn destroy_view(device: *mut c_void, view: u64, alloc: *const c_void) {
    {
        let _timed = Timed::start();
        let mut st = state();
        st.view_serials.remove(&view);
        for views in st.set_views.values_mut() {
            views.retain(|_, v| v.0 != view);
        }
        for c in st.cmds.values_mut() {
            c.pushed.retain(|_, v| v.0 != view);
        }
    }
    if let Some(f) = next::<DestroyFn>(&DESTROY_VIEW) {
        f(device, view, alloc);
    }
}

/// The base colour texture a draw in `c` with pipeline `p` samples:
/// (view, layout, grey detail texture).
fn draw_texture(st: &State, c: &Cmd, p: &Pipeline) -> Option<(u64, u32, bool)> {
    let (set, binding, scalar) = p.albedo?;
    let (view, layout) = match c.pushed.get(&(set, binding)) {
        Some(&v) => v,
        None => *st.set_views.get(c.sets.get(set as usize)?)?.get(&binding)?,
    };
    (view != 0).then_some((view, layout, scalar))
}

/// The render thread's time in our hooks' own work (not the driver's),
/// summed until the frame ends, and how many draws that covered; then the
/// last `CPU_WINDOW` frames' totals for `rt stats`.
static HOOK_NS: AtomicU64 = AtomicU64::new(0);
static HOOK_DRAWS: AtomicU64 = AtomicU64::new(0);
const CPU_WINDOW: usize = 120;

pub(crate) struct Timed(std::time::Instant);

impl Timed {
    pub(crate) fn start() -> Timed {
        Timed(std::time::Instant::now())
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        HOOK_NS.fetch_add(self.0.elapsed().as_nanos() as u64, Relaxed);
    }
}

/// The kinds of draw that can be left out of the capture for bisecting
/// (devctl `rt kind neon|terrain on|off`), all on at start.
pub(crate) const KINDS: [&str; 2] = ["neon", "terrain"];
static KINDS_ON: [AtomicBool; 2] = [AtomicBool::new(true), AtomicBool::new(true)];

pub(crate) fn set_kind(name: &str, on: bool) -> Result<(), String> {
    let i = KINDS.iter().position(|k| *k == name).ok_or_else(|| format!("rt kind {} on|off", KINDS.join("|")))?;
    KINDS_ON[i].store(on, Relaxed);
    Ok(())
}

fn kind_on(p: &Pipeline) -> bool {
    !(p.emission != 0 && !KINDS_ON[0].load(Relaxed)) && !(p.terrain && !KINDS_ON[1].load(Relaxed))
}

/// `vkCmdSetScissor`: the first rectangle, for the capture's test of a
/// clipped draw.
extern "C" fn cmd_set_scissor(cmd: *mut c_void, first: u32, count: u32, rects: *const [i32; 4]) {
    if first == 0 && count > 0 && !rects.is_null() {
        // SAFETY: the engine's array of `count` rectangles, read during its call.
        let r = unsafe { std::ptr::read_unaligned(rects) };
        if let Some(c) = state().cmds.get_mut(&(cmd as usize)) {
            c.scissor = Some(r);
        }
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u32, *const [i32; 4])>(&CMD_SET_SCISSOR) {
        f(cmd, first, count, rects);
    }
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
    let _timed = Timed::start();
    state().cmds.insert(cmd as usize, Cmd::default());
}

/// A pass begins, before the begin is forwarded. The first scene pass after
/// a present starts a frame: our buffer is written from its start again.
/// Before every scene pass, outside it: the barrier that keeps this frame's
/// capture from overwriting what the last one's readers still use.
pub(crate) fn pass_beginning(pass: &Pass) {
    let _timed = Timed::start();
    let (depth, full, to_swapchain, colours, colour) = {
        let reg = super::super::vk_layer::passes::registry();
        let d = super::super::ssr::describe(pass, &reg);
        let to_swapchain = d.colors.iter().chain(&d.resolves).any(|a| a.swapchain);
        // The scene colour as it stands at the end of the pass: the
        // single-sample resolve target when the pass has one, else the
        // colour attachment itself.
        let colour = d.resolves.first().or(d.colors.first()).cloned();
        (d.depth, reg.swapchain_extents(), to_swapchain, d.colors.len(), colour)
    };
    let mut st = state();
    let cmd = pass.cmd;
    // A pass that draws into a swapchain image is the frame's last, the
    // interface over the finished scene, not the scene.
    let Some(z) = depth.filter(|d| registry::is_depth_format(d.format) && full.contains(&d.extent) && !to_swapchain) else {
        if let Some(c) = st.cmds.get_mut(&cmd) {
            c.scene = false;
            c.capturing = false;
        }
        return;
    };
    let secondary = matches!(pass.target, Target::RenderPass { contents, .. } if contents == CONTENTS_SECONDARY);
    let device = z.device;
    let xfb = super::enabled(device).transform_feedback;
    let presents = PRESENTS.load(Relaxed);
    let starts = match st.gpus.iter().find(|g| g.device == device) {
        Some(g) => g.frame != presents,
        None => true,
    };
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
    c.extent = z.extent;
    // The frame's first scene pass, the opaque world: its colour is copied
    // when it ends (see `pass_ended`).
    c.opaque = if starts { colour } else { None };
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
        g.bytes = 0;
        g.overflowed = false;
        g.draws.clear();
        g.textures.clear();
        g.frame_block = None;
        g.frame = presents;
        st.stats.frames += 1;
        st.frame_log.clear();
    }
    let g = st.gpus.iter().find(|g| g.device == device).expect("made above");
    // The last frame's readers of our buffer (the trace's compute, the
    // dump's copy) finish before this pass writes it.
    g.barrier(
        cmd as *mut c_void,
        PIPELINE_STAGE_COMPUTE | PIPELINE_STAGE_TRANSFER,
        PIPELINE_STAGE_TRANSFORM_FEEDBACK,
        ACCESS_SHADER_READ | ACCESS_TRANSFER_READ,
        ACCESS_XFB_WRITE,
    );
    let at = g.bytes;
    let k = st.frame_log.len();
    st.frame_log.push(format!("scene{k}(load={},colours={colours},from={})", z.load, at / TRIANGLE_BYTES));
    if let Some(c) = st.cmds.get_mut(&cmd) {
        c.capturing = true;
    }
}

/// Whether the scissor `r` (x, y, width, height) leaves all of `extent`.
fn covers(r: [i32; 4], extent: [u32; 2]) -> bool {
    r[0] <= 0 && r[1] <= 0 && r[0] as i64 + r[2] as i64 >= extent[0] as i64 && r[1] as i64 + r[3] as i64 >= extent[1] as i64
}

/// How many vertices a draw of `count` vertices or indices of `instances`
/// instances writes to transform feedback with `topology`: whole triangles
/// only, three vertices each, strips and fans split (never with primitive
/// restart, see `describe_pipeline`).
pub(crate) fn xfb_vertices(topology: u32, count: u32, instances: u32) -> u64 {
    let triangles = match topology {
        3 => count / 3,
        4 | 5 => count.saturating_sub(2),
        _ => 0,
    } as u64;
    3 * triangles * instances as u64
}

fn end_if_active(st: &mut State, cmd: *mut c_void) {
    let Some(c) = st.cmds.get_mut(&(cmd as usize)) else { return };
    if !c.active {
        return;
    }
    c.active = false;
    let device = c.device;
    if let Some(g) = st.gpus.iter().find(|g| g.device == device) {
        (g.fns.end_xfb)(cmd, 0, 0, std::ptr::null(), std::ptr::null());
    }
}

/// Before the engine's `vkCmdBindPipeline` is forwarded: no pipeline may be
/// bound while transform feedback is active.
pub(crate) fn binding(cmd: *mut c_void, bind_point: u32) {
    let _timed = Timed::start();
    if bind_point != BIND_POINT_GRAPHICS {
        return;
    }
    end_if_active(&mut state(), cmd);
}

/// After it was forwarded.
pub(crate) fn bound(cmd: *mut c_void, bind_point: u32, pipeline: u64) {
    let _timed = Timed::start();
    if bind_point != BIND_POINT_GRAPHICS {
        return;
    }
    let mut st = state();
    let Some(c) = st.cmds.get_mut(&(cmd as usize)) else { return };
    c.bound = pipeline;
    if !c.capturing || pipeline == 0 {
        return;
    }
    let device = c.device;
    if st.pipelines.get(&(device, pipeline)).is_some_and(|p| p.capture.is_ok()) {
        st.stats.binds_captured += 1;
    } else {
        st.stats.binds_skipped += 1;
    }
}

/// Before `vkCmdNextSubpass` or `vkCmdExecuteCommands`: capture stops for
/// the rest of the pass, which neither allows while it is active.
pub(crate) fn interrupted(cmd: *mut c_void) {
    let _timed = Timed::start();
    let mut st = state();
    end_if_active(&mut st, cmd);
    if let Some(c) = st.cmds.get_mut(&(cmd as usize)) {
        c.capturing = false;
    }
}

/// Before a pass's end is forwarded.
pub(crate) fn pass_ending(cmd: *mut c_void) {
    let _timed = Timed::start();
    end_if_active(&mut state(), cmd);
}

/// After it was forwarded, outside the pass: what was written is made
/// visible to the compute that reads it, and for a dump copied where the
/// host reads it.
pub(crate) fn pass_ended(cmd: *mut c_void) {
    let _timed = Timed::start();
    let mut st = state();
    let Some(c) = st.cmds.get_mut(&(cmd as usize)) else { return };
    let capturing = c.capturing;
    c.scene = false;
    c.capturing = false;
    if !capturing {
        return;
    }
    let (device, dumping, opaque) = (c.device, c.dumping, c.opaque.take());
    let presents = PRESENTS.load(Relaxed);
    let Some(g) = st.gpus.iter().find(|g| g.device == device) else { return };
    g.barrier(
        cmd,
        PIPELINE_STAGE_TRANSFORM_FEEDBACK,
        PIPELINE_STAGE_TRANSFER | PIPELINE_STAGE_COMPUTE,
        ACCESS_XFB_WRITE,
        ACCESS_TRANSFER_READ | ACCESS_SHADER_READ,
    );
    let bytes = g.bytes;
    if dumping && g.readback_size > 256 && bytes > 0 {
        let bytes = bytes.min(g.readback_size - 256) / TRIANGLE_BYTES * TRIANGLE_BYTES;
        let region = [0u64, 256, bytes];
        (g.fns.copy_buffer)(cmd, g.xfb.buffer, g.readback.buffer, 1, region.as_ptr());
        g.barrier(cmd, PIPELINE_STAGE_TRANSFER, PIPELINE_STAGE_HOST, ACCESS_TRANSFER_WRITE, ACCESS_HOST_READ);
        DUMPED_BYTES.store(bytes, Relaxed);
        DUMPED_AT.store(presents.max(1), Relaxed);
    }
    if let Some(last) = st.frame_log.last_mut() {
        last.push_str(&format!("..{}", bytes / TRIANGLE_BYTES));
    }
    drop(st);
    // The opaque world's colour, for the trace (outside our lock: the
    // reflections' state is taken while it is held elsewhere).
    if let Some(a) = opaque {
        if super::trace::is_on() {
            let copied = super::super::ssr::copy_opaque(cmd, &a);
            if let Some(last) = state().frame_log.last_mut() {
                last.push_str(if copied { ",opaque copied" } else { ",opaque not copied" });
            }
        }
    }
}

/// A draw in `cmd`, before it is forwarded: inside a scene pass, with a
/// pipeline to capture bound, transform feedback is begun if it is not
/// active, at the offset our count of what this frame wrote has reached
/// (there is no waiting on the GPU's own counter, whose end-to-begin
/// round trip inside a render pass Vulkan gives no barrier for), and the
/// count grows by what the draw writes. An indirect draw, whose size only
/// the GPU knows, is not captured. Also the counts and `rt draws`.
pub(crate) fn drew(cmd: *mut c_void, kind: &'static str, count: u32, instances: u32) {
    let _timed = Timed::start();
    HOOK_DRAWS.fetch_add(1, Relaxed);
    let mut st = state();
    let Some(c) = st.cmds.get(&(cmd as usize)) else { return };
    if !c.scene && !c.listing {
        return;
    }
    let (listing, bound, device, scene, capturing, extent, dynamic_scissor) =
        (c.listing, c.bound, c.device, c.scene, c.capturing, c.extent, c.scissor);
    let p = st.pipelines.get(&(device, bound)).cloned();
    // A draw cut down by a scissor smaller than the pass (an interface
    // clipped to its frame, a scrolling text clipped to its display) would
    // be captured whole, outside where it shows; it is not captured.
    let scissor = p.as_ref().and_then(|p| p.scissor).or(dynamic_scissor);
    let clipped = scissor.is_some_and(|r| !covers(r, extent));
    if capturing {
        let take = p
            .as_ref()
            .filter(|p| p.capture.is_ok() && kind != "indirect" && !clipped && kind_on(p))
            .map(|p| xfb_vertices(p.topology, count, instances));
        if clipped && p.as_ref().is_some_and(|p| p.capture.is_ok()) {
            st.stats.draws_clipped += 1;
        }
        let room = st.gpus.iter().find(|g| g.device == device).map(|g| (g.capacity, g.bytes));
        match (take, room) {
            (Some(vertices), Some((capacity, at))) if at + vertices * spirv::STRIDE as u64 <= capacity => {
                let bytes = vertices * spirv::STRIDE as u64;
                let active = st.cmds.get(&(cmd as usize)).is_some_and(|c| c.active);
                let g = st.gpus.iter_mut().find(|g| g.device == device).expect("found above");
                if !active && bytes > 0 {
                    let size = capacity - at;
                    (g.fns.bind_xfb)(cmd, 0, 1, &g.xfb.buffer, &at, &size);
                    (g.fns.begin_xfb)(cmd, 0, 0, std::ptr::null(), std::ptr::null());
                    g.started = true;
                    st.stats.begins += 1;
                    if let Some(c) = st.cmds.get_mut(&(cmd as usize)) {
                        c.active = true;
                    }
                }
                // The draw's base colour texture, numbered, from its first
                // triangle on.
                let texture = st.cmds.get(&(cmd as usize)).zip(p.as_ref()).and_then(|(c, p)| draw_texture(&st, c, p));
                let terrain = p.as_ref().is_some_and(|p| p.terrain);
                let code = match texture {
                    _ if terrain => MODE_TERRAIN,
                    Some((view, layout, scalar)) => {
                        let serial = match st.view_serials.get(&view) {
                            Some(&n) => n,
                            None => {
                                st.next_serial = (st.next_serial + 1) & SERIAL_MASK;
                                let n = st.next_serial.max(1);
                                st.view_serials.insert(view, n);
                                n
                            }
                        };
                        let g = st.gpus.iter_mut().find(|g| g.device == device).expect("found above");
                        if !g.textures.iter().any(|t| t.0 == serial) && g.textures.len() < MAX_TEXTURES {
                            g.textures.push((serial, view, layout));
                        }
                        serial | if scalar { MODE_DETAIL } else { MODE_COLOUR }
                    }
                    None => 0,
                };
                let frame_block = st.cmds.get(&(cmd as usize)).and_then(|c| {
                    c.pushed_frame.or_else(|| {
                        st.set_frames.get(&c.sets[0]).map(|&(buffer, offset, range, dynamic)| {
                            (buffer, offset + if dynamic { c.set0_dynamic as u64 } else { 0 }, range)
                        })
                    })
                });
                let g = st.gpus.iter_mut().find(|g| g.device == device).expect("found above");
                if frame_block.is_some() {
                    g.frame_block = frame_block;
                }
                let blended = u32::from(p.as_ref().is_some_and(|p| p.blends));
                let emission = p.as_ref().map_or(0, |p| p.emission);
                if g.draws.last().map_or((0, 0, 0), |d| (d[1], d[2], d[3])) != (code, blended, emission) {
                    g.draws.push([(g.bytes / TRIANGLE_BYTES) as u32, code, blended, emission]);
                }
                g.bytes += bytes;
                st.stats.draws_captured += 1;
                if code != 0 && !terrain {
                    st.stats.draws_textured += 1;
                }
                if emission != 0 {
                    st.stats.draws_emissive += 1;
                }
                if terrain {
                    st.stats.draws_terrain += 1;
                }
            }
            (Some(_), Some(_)) => {
                // No room left this frame.
                end_if_active(&mut st, cmd);
                if let Some(g) = st.gpus.iter_mut().find(|g| g.device == device) {
                    g.overflowed = true;
                }
                st.stats.draws_skipped += 1;
                st.stats.draws_overflowed += 1;
            }
            _ => {
                // An indirect draw, or a pipeline not to capture (already
                // ended at its bind).
                end_if_active(&mut st, cmd);
                st.stats.draws_skipped += 1;
            }
        }
    }
    if listing && scene {
        let line = format!(
            "0x{:x}\t0x{bound:x}\t{kind}\t{count}\t{instances}\t{}\tvs-{:016x}\tfs-{:016x}\t{:?}\n",
            cmd as usize,
            match p.as_ref().map(|p| p.capture) {
                Some(Ok(())) if clipped => "clipped by its scissor",
                Some(Ok(())) => "captured",
                Some(Err(why)) => why,
                None => "unknown pipeline",
            },
            p.as_ref().map_or(0, |p| p.vs),
            p.as_ref().map_or(0, |p| p.fs),
            scissor,
        );
        st.listing.push(line);
    }
}

/// From `vkQueuePresentKHR`.
pub(crate) fn frame_end(presents: u64) {
    PRESENTS.store(presents, Relaxed);
    let mut st = state();
    let (ns, draws) = (HOOK_NS.swap(0, Relaxed), HOOK_DRAWS.swap(0, Relaxed));
    if st.stats.cpu.len() == CPU_WINDOW {
        st.stats.cpu.pop_front();
    }
    st.stats.cpu.push_back((ns, draws));
    if !st.frame_log.is_empty() {
        st.last_log = st.frame_log.join(",");
    }
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
            let mut text = String::from("cmd\tpipeline\tkind\tcount\tinstances\tcapture\tvs\tfs\tscissor\n");
            text.push_str(&st.listing.concat());
            match std::fs::write(&path, text) {
                Ok(()) => println!("[android] vulkan: rt: {} scene draws written to {path}", st.listing.len()),
                Err(e) => println!("[android] vulkan: rt: {path}: {e}"),
            }
        }
        st.listing.clear();
    }
    if let Some(g) = st.gpus.first() {
        LAST_BYTES.store(g.bytes, Relaxed);
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

/// This frame's capture on `device`, when anything was captured since our
/// buffer was made: the buffer of triangles, the buffer whose first word
/// the trace sets to the bytes written, the capacity in bytes, and the
/// bytes this frame wrote so far.
pub(crate) fn frame_captured(device: usize) -> Option<Captured> {
    let st = state();
    st.gpus.iter().find(|g| g.device == device && g.started).map(|g| Captured {
        xfb: g.xfb.buffer,
        capacity: g.capacity,
        bytes: g.bytes,
        whole: !g.overflowed,
    })
}

/// This frame's base colour textures on `device`: the draws' codes by first
/// triangle and the views.
pub(crate) fn frame_textures(device: usize) -> (Vec<[u32; 4]>, Vec<(u32, u64, u32)>) {
    let st = state();
    st.gpus.iter().find(|g| g.device == device).map_or((Vec::new(), Vec::new()), |g| (g.draws.clone(), g.textures.clone()))
}

/// The engine's per-frame uniform block this frame's captured draws read:
/// (buffer, offset, range).
pub(crate) fn frame_block(device: usize) -> Option<(u64, u64, u64)> {
    state().gpus.iter().find(|g| g.device == device).and_then(|g| g.frame_block)
}

/// This frame's capture on one device.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Captured {
    pub xfb: u64,
    /// Bytes our buffer holds, and this frame wrote so far.
    pub capacity: u64,
    pub bytes: u64,
    /// No draw to capture was left out for want of room.
    pub whole: bool,
}

/// The trace was recorded, for the frame's report.
pub(crate) fn traced() {
    state().frame_log.push("trace".into());
}

// ------------------------------------------------------------ devctl

pub(crate) fn stats_line() -> String {
    let st = state();
    let s = &st.stats;
    let bytes = LAST_BYTES.load(Relaxed);
    let failed = st.failed.last().map_or(String::new(), |(_, why)| format!(" capture_failed={why:?}"));
    let n = s.cpu.len().max(1) as f64;
    let (ns, draws) = s.cpu.iter().fold((0u64, 0u64), |a, b| (a.0 + b.0, a.1 + b.1));
    let cpu_frame = ns as f64 / n / 1000.0;
    let mut sorted: Vec<u64> = s.cpu.iter().map(|c| c.0).collect();
    sorted.sort_unstable();
    let cpu_p95 = sorted.get((sorted.len() * 95 / 100).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0) as f64 / 1000.0;
    let cpu_draw = if draws > 0 { ns as f64 / draws as f64 } else { 0.0 };
    let draws_frame = draws as f64 / n;
    format!(
        "triangles={} capacity={} frames={} begins={} binds_captured={} binds_skipped={} draws_captured={} draws_skipped={} \
         cpu_us_frame={:.0} cpu_us_p95={:.0} cpu_ns_draw={:.0} draws_frame={:.0} draws_overflowed={} draws_clipped={} draws_textured={} draws_emissive={} draws_terrain={} textures={} vs_instrumented={} vs_left={} pipelines_captured={} pipelines_skipped={} frame={}{}",
        bytes / TRIANGLE_BYTES,
        st.gpus.first().map_or(capacity_triangles(), |g| g.capacity / TRIANGLE_BYTES),
        s.frames,
        s.begins,
        s.binds_captured,
        s.binds_skipped,
        s.draws_captured,
        s.draws_skipped,
        cpu_frame,
        cpu_p95,
        cpu_draw,
        draws_frame,
        s.draws_overflowed,
        s.draws_clipped,
        s.draws_textured,
        s.draws_emissive,
        s.draws_terrain,
        st.gpus.first().map_or(0, |g| g.textures.len()),
        s.modules_instrumented,
        s.modules_left,
        s.pipelines_captured,
        s.pipelines_skipped,
        st.last_log,
        failed,
    )
}

/// devctl `rt draws`: the next frame's scene draws to the dump directory.
pub(crate) fn request_draws() -> Result<String, String> {
    let dir = dump_dir().ok_or_else(|| format!("{DUMP_ENV}=<dir> at start is needed"))?;
    LIST_NEXT.store(true, Relaxed);
    Ok(format!("the next frame's scene draws go to {dir}/draws-<present>.tsv"))
}

/// devctl `rt dump <path>`: the next frame's captured triangles, as a
/// little-endian `u32` byte count and then that many bytes of vertices,
/// three per triangle, each a clip-space position, a colour and the vector
/// to the camera (`vec4` each; the last one's w is not written).
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
    let bytes = DUMPED_BYTES.load(Relaxed).min(g.readback_size - 256);
    // SAFETY: our mapping of `readback_size` bytes; the copy into it is done
    // (six presents later).
    let data = unsafe { std::slice::from_raw_parts((g.readback_ptr + 256) as *const u8, bytes as usize).to_vec() };
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
    // The room the budget asks for, else half as much and so on, down to the
    // least: a failed allocation must not leave nothing captured.
    let mut triangles = capacity_triangles();
    let made = loop {
        match make_gpu(device, triangles) {
            Ok(g) => break Ok(g),
            Err(why) if triangles > 1 << 17 => {
                st.once(format!("room for {triangles} triangles could not be made ({why}); trying half"));
                triangles /= 2;
            }
            Err(why) => break Err(why),
        }
    };
    match made {
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

fn make_gpu(device: usize, triangles: u64) -> Result<Gpu, String> {
    let (current, _) = frame_capture::current_device().ok_or("no Vulkan device is known")?;
    if current != device {
        return Err("the scene is on another device than the one the client follows".into());
    }
    let d = device as Dev;
    let fns = Fns::resolve(d)?;
    let types = memory_types()?;
    let capacity = triangles * TRIANGLE_BYTES;
    // A dump's host copy is held to DUMP_BYTES: host-visible memory is
    // scarcer than the device's, and the whole capture (hundreds of MiB once
    // sized from the video memory) found none, so nothing was captured.
    let readback_size = if dump_dir().is_some() { 256 + capacity.min(DUMP_BYTES) } else { 256 };
    let mut g = Gpu {
        device,
        fns,
        xfb: Owned::default(),
        readback: Owned::default(),
        readback_ptr: 0,
        readback_size,
        capacity,
        started: false,
        frame: u64::MAX,
        bytes: 0,
        overflowed: false,
        draws: Vec::new(),
        textures: Vec::new(),
        frame_block: None,
    };
    let made = (|| {
        g.xfb = make_buffer(&g.fns, d, &types, capacity, USAGE_XFB | USAGE_STORAGE | USAGE_TRANSFER_SRC | USAGE_TRANSFER_DST, false)?;
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
        for o in [self.xfb, self.readback] {
            if o.buffer != 0 {
                (self.fns.destroy_buffer)(d, o.buffer, none);
            }
            if o.memory != 0 {
                (self.fns.free_memory)(d, o.memory, none);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_draw_writes_is_counted_in_whole_triangles() {
        // A list: whole triangles only.
        assert_eq!(xfb_vertices(3, 36, 1), 36);
        assert_eq!(xfb_vertices(3, 37, 2), 72);
        // A strip and a fan: one triangle per vertex after the second.
        assert_eq!(xfb_vertices(4, 4, 1), 6);
        assert_eq!(xfb_vertices(5, 5, 3), 27);
        assert_eq!(xfb_vertices(4, 1, 1), 0);
        // Not triangles.
        assert_eq!(xfb_vertices(1, 100, 1), 0);
        // Room from a budget: 1536 MiB is two million triangles; at least
        // 131072, at most four million.
        assert_eq!(triangles_for(1536 << 20), 2 << 20);
        assert_eq!(triangles_for(1 << 20), 1 << 17);
        assert_eq!(triangles_for(64 << 30), 4 << 20);
        // A scissor of the whole pass, or more, clips nothing.
        assert!(covers([0, 0, 1667, 1349], [1667, 1349]));
        assert!(covers([-10, 0, 4000, 4000], [1667, 1349]));
        assert!(!covers([100, 0, 1567, 1349], [1667, 1349]));
        assert!(!covers([0, 0, 1667, 1000], [1667, 1349]));
        // A vertex is a position, a colour, a camera vector, a world
        // position, texture coordinates and a normal.
        assert_eq!(TRIANGLE_BYTES, 288);
    }
}
