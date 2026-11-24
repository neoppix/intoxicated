//! The ray-traced reflections themselves: every frame whose scene was
//! captured (`capture`), an acceleration structure over its triangles and a
//! trace of every reflecting pixel's ray against it, recorded in place of the
//! screen-space march of `android::ssr`, whose surfaces, strength, compose
//! and look knobs they share.
//!
//! The driver on the GPU this was written on (NVIDIA 580 on FreeBSD) has no
//! hardware acceleration structures or ray queries, so both are compute
//! shaders of ours (`build.comp`, and `ssr/ssr.comp` with `RT`): a linear
//! BVH (Morton codes of the triangle centres, a bitonic sort, the hierarchy
//! from the sorted codes and the boxes from the leaves up), rebuilt from
//! nothing every frame, and a stack traversal of it for one pixel of every
//! 2 by 2 block, spread back over the pixels of the same surface by a
//! depth-aware upsample.
//!
//! What it reflects is what the engine drew in the scene passes this frame,
//! and nothing else: the triangles it rasterised with colour writes. What
//! the engine did not draw (behind the camera, culled, hidden parts) cannot
//! be hit.
//!
//! The work is recorded by `ssr::record` when the reflections are on and
//! these are (`INTOXICATED_RT=1`, which turns the reflections on as well,
//! devctl `rt on|off`): after the depth copy, the build, then the trace into
//! a buffer of ours and the upsample into the reflections' trace image,
//! which their compose blends in as it does the march's.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use super::super::depth_capture::{
    ComputePipelineCreateInfo, CreateFn, DescriptorBufferInfo, DescriptorImageInfo, DescriptorPoolCreateInfo,
    DescriptorPoolSize, DescriptorSetAllocateInfo, DescriptorSetLayoutBinding, DescriptorSetLayoutCreateInfo,
    DestroyFn, ImageViewCreateInfo, PipelineLayoutCreateInfo, PipelineShaderStageCreateInfo, PushConstantRange,
    SamplerCreateInfo, ShaderModuleCreateInfo, WriteDescriptorSet,
};
use super::super::frame_capture::{device_fn, Dev, MemoryBarrier};
use super::capture::{self, check, make_buffer, memory_types, Owned};

const ST_SHADER_MODULE_CREATE_INFO: i32 = 16;
const ST_PIPELINE_SHADER_STAGE_CREATE_INFO: i32 = 18;
const ST_COMPUTE_PIPELINE_CREATE_INFO: i32 = 29;
const ST_PIPELINE_LAYOUT_CREATE_INFO: i32 = 30;
const ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: i32 = 32;
const ST_DESCRIPTOR_POOL_CREATE_INFO: i32 = 33;
const ST_DESCRIPTOR_SET_ALLOCATE_INFO: i32 = 34;
const ST_WRITE_DESCRIPTOR_SET: i32 = 35;
const ST_MEMORY_BARRIER: i32 = 46;
const ST_QUERY_POOL_CREATE_INFO: i32 = 11;
const ST_IMAGE_CREATE_INFO: u32 = 14;
const ST_IMAGE_VIEW_CREATE_INFO: i32 = 15;
const ST_SAMPLER_CREATE_INFO: i32 = 31;
const ST_MEMORY_ALLOCATE_INFO: i32 = 5;
const ST_IMAGE_MEMORY_BARRIER: i32 = 45;
const FORMAT_RGBA8: u32 = 37;
const USAGE_SAMPLED: u32 = 0x4;
const DESCRIPTOR_COMBINED_IMAGE_SAMPLER: u32 = 1;
const DESCRIPTOR_UNIFORM_BUFFER: u32 = 6;
const USAGE_UNIFORM: u32 = 0x10;
/// The engine's per-frame uniform block, as far as the trace reads it
/// (61 vec4s; see ssr.comp).
const FRAME_BLOCK_BYTES: u64 = 61 * 16;
const LAYOUT_SHADER_READ_ONLY: i32 = 5;

const SHADER_STAGE_COMPUTE: u32 = 0x20;
const BIND_POINT_COMPUTE: u32 = 1;
const DESCRIPTOR_STORAGE_BUFFER: u32 = 7;
const STAGE_COMPUTE: u32 = 0x800;
const STAGE_TRANSFER: u32 = 0x1000;
const STAGE_BOTTOM_OF_PIPE: u32 = 0x2000;
const ACCESS_SHADER_READ: u32 = 0x20;
const ACCESS_SHADER_WRITE: u32 = 0x40;
const ACCESS_TRANSFER_READ: u32 = 0x800;
const ACCESS_TRANSFER_WRITE: u32 = 0x1000;
const USAGE_TRANSFER_DST: u32 = 0x2;
const USAGE_STORAGE: u32 = 0x20;
const QUERY_TYPE_TIMESTAMP: u32 = 2;
const QUERY_RESULT_64: u32 = 0x1;
const QUERY_RESULT_WITH_AVAILABILITY: u32 = 0x4;

const SPIRV_FIT: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_fit.spv.rs"));
const SPIRV_INSERT: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_insert.spv.rs"));
const SPIRV_GATHER: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_gather.spv.rs"));
const SPIRV_PREP: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_prep.spv.rs"));
const SPIRV_SORT_LOCAL: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_sort_local.spv.rs"));
const SPIRV_SORT_GLOBAL: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_sort_global.spv.rs"));
const SPIRV_BUILD: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_build.spv.rs"));
const SPIRV_BOUNDS: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_bounds.spv.rs"));
/// `ssr/ssr.comp` with `RT`, and with `RT` and `UPSAMPLE`.
const SPIRV_TRACE: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_trace.spv.rs"));
const SPIRV_UPSAMPLE: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_upsample.spv.rs"));
/// `ssr/ssr.comp` with `RT` and `ENVMAP`.
const SPIRV_ENV: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_env.spv.rs"));
/// `ssr/ssr.comp` with `RT` and `GI`, with `RT` and `GIAPPLY`, and with
/// `RT` and `GIFILTER`.
const SPIRV_GI: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_gi.spv.rs"));
const SPIRV_GI_APPLY: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_gi_apply.spv.rs"));
const SPIRV_GI_FILTER: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_gi_filter.spv.rs"));

const PREP: usize = 0;
const SORT_LOCAL: usize = 1;
const SORT_GLOBAL: usize = 2;
const BUILD: usize = 3;
const BOUNDS: usize = 4;
const TRACE: usize = 5;
const UPSAMPLE: usize = 6;
const FIT: usize = 7;
const INSERT: usize = 8;
const GATHER: usize = 9;
const ENV: usize = 10;
const GI: usize = 11;
const GI_APPLY: usize = 12;
const GI_FILTER: usize = 13;
const PIPELINES: usize = 14;
/// The bindings of our set: 0 the capture, 2 the sorted keys, 3 the nodes,
/// 4 the parents, 5 the flags, 6 the globals, 7 the traced blocks, 8 the
/// triangles to trace, 9 the cache, 10 this frame's draws, 11 its textures
/// by serial, 12 the environment, 13 and 14 the indirect light, 15 and 16
/// the filtered indirect light, 17 the triangles that give off light.
const BINDINGS: [u32; 17] = [0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17];
/// Bytes of the indirect light a 2 by 2 block.
const GI_BYTES: u64 = 32;
/// The environment: 256 by 256 directions, 16 bytes each.
const ENV_BYTES: u64 = 16 * 256 * 256;
/// Slots in the cache of static triangles (128 bytes each): as many as the
/// capture has room for (see `capture::capacity_triangles`).
fn slots() -> u32 {
    capture::capacity_triangles().next_power_of_two() as u32
}
const SLOT_BYTES: u64 = 128;
/// Triangles the list to trace holds (96 bytes each): this frame's and the
/// cache's.
fn listed() -> u64 {
    capture::capacity_triangles()
}
const TRI_BYTES: u64 = 96;
/// Draws a frame can tell the build about (16 bytes each), and the entries
/// of the serial-to-texture table (8 bytes each).
const MAX_DRAWS: usize = 4096;
const TEX_MAP: usize = 1024;
/// Textures in the trace's set 2.
const TEXTURES: u32 = capture::MAX_TEXTURES as u32;
/// Bytes of the globals (`build.comp`'s `Globals`) and of what `rt stats`
/// reads back of them.
const GLOBALS_BYTES: u64 = 512;
const GLOBALS_READ: usize = 77;
/// Our mapped copy of the globals' first `GLOBALS_READ` words.
const READBACK_BYTES: u64 = 512;
/// Byte offset of `full_rate` in the globals.
const GLOBALS_FULL_RATE: u64 = 208;
/// Bytes per traced 2 by 2 block.
const BLOCK_BYTES: u64 = 16;
/// The blocks buffer has room for at least this extent's.
const ROOM_FOR: [u32; 2] = [3840, 2160];

/// The 2 by 2 blocks of an extent.
pub(crate) fn blocks_of(extent: [u32; 2]) -> u64 {
    extent[0].div_ceil(2) as u64 * extent[1].div_ceil(2) as u64
}

/// Keys one local sort dispatch handles.
const BLOCK: u32 = 1024;

/// `build.comp`'s push constants.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BuildParams {
    pub n: u32,
    pub cap: u32,
    pub k: u32,
    pub j: u32,
    pub current: u32,
    pub flags: u32,
    pub slot_count: u32,
    pub size: u32,
    pub draw_count: u32,
    pub pad: [u32; 3],
}

/// Whether the trace replaces the march, while the reflections are on.
static ON: OnceLock<AtomicBool> = OnceLock::new();

fn on() -> &'static AtomicBool {
    ON.get_or_init(|| AtomicBool::new(super::requested()))
}

pub(crate) fn is_on() -> bool {
    on().load(Relaxed)
}

pub(crate) fn set_on(v: bool) {
    on().store(v, Relaxed);
}

/// Where a hit's colour comes from: its own (texture times colour) lit as
/// the engine lights it (its sun, with a ray for the shadow, and ambient,
/// from its per-frame uniform block; the default), its own times the
/// measured exposure, that mixed with a quarter of the screen's where the
/// screen shows it, or the screen's wherever it shows it (the first way).
/// `INTOXICATED_RT_SHADE`, devctl `rt shade`.
pub const SHADE_ENV: &str = "INTOXICATED_RT_SHADE";
pub(crate) const SHADES: [&str; 4] = ["lit", "own", "mix", "screen"];
static SHADE: OnceLock<std::sync::atomic::AtomicUsize> = OnceLock::new();

fn shade() -> &'static std::sync::atomic::AtomicUsize {
    SHADE.get_or_init(|| {
        let v = std::env::var(SHADE_ENV).ok();
        let i = v.as_deref().and_then(|v| SHADES.iter().position(|s| *s == v.trim())).unwrap_or(0);
        if let Some(v) = v.filter(|v| !SHADES.contains(&v.trim())) {
            println!("[android] vulkan: rt: {SHADE_ENV}={v} is not one of {}; own", SHADES.join(", "));
        }
        std::sync::atomic::AtomicUsize::new(i)
    })
}

pub(crate) fn set_shade(name: &str) -> Result<(), String> {
    let i = SHADES.iter().position(|s| *s == name).ok_or_else(|| format!("rt shade {}", SHADES.join("|")))?;
    shade().store(i, Relaxed);
    Ok(())
}

/// `ssr.comp` flags: bit 12 the engine's light is bound, bit 13 shade lit
/// (bits 8 to 11 are the colour format's).
const FLAG_LIGHT: u32 = 1 << 12;
const FLAG_LIT: u32 = 1 << 13;

/// The trace's push constant flags for the shade (`ssr.comp`: bit 3 the
/// screen's colour, bit 4 a quarter of it).
pub(crate) fn shade_flags() -> u32 {
    match shade().load(Relaxed) {
        1 => 0,
        2 => 16,
        3 => 8,
        _ => FLAG_LIT,
    }
}

/// The environment built from the sky the screen shows, for rays that meet
/// nothing: on unless `INTOXICATED_RT_ENV=0`; devctl `rt env on|off`.
pub const ENV_ENV: &str = "INTOXICATED_RT_ENV";
static ENV_ON: OnceLock<AtomicBool> = OnceLock::new();
const FLAG_NO_ENV: u32 = 32;
/// The indirect light was computed this frame before the reflections
/// (`ssr.comp` `gi_at`).
const FLAG_GI: u32 = 1 << 14;
/// Bytes of the list of triangles that give off light (`MAX_EMITTERS` in
/// `rt/build.comp`), and where their count is in the globals.
const EMITTERS_BYTES: u64 = 4 * 16384;
const GLOBALS_EMITTERS: u64 = 304;
/// The blocks' sizes in the globals (`rscale`, `gscale`).
const GLOBALS_SCALES: u64 = 308;
/// Pixels may trace their own reflection ray (`ssr.comp`'s upsample) only
/// while the blocks are their finest, 2 by 2.
const FLAG_FULL_RATE: u32 = 1 << 15;
/// The reflections trace on a grid at most this wide over two; wider
/// outputs get bigger blocks.
const TARGET_WIDTH: u32 = 1920;
/// The GPU time the build and the trace together should stay under (ms),
/// looked at every `BUDGET_FRAMES` frames; `MAX_COARSER` steps at most.
const BUDGET_MS: f64 = 3.0;
const BUDGET_FRAMES: u32 = 120;
const MAX_COARSER: u32 = 2;

fn env_switch() -> &'static AtomicBool {
    ENV_ON.get_or_init(|| AtomicBool::new(std::env::var(ENV_ENV).map_or(true, |v| v.trim() != "0")))
}

pub(crate) fn env_on() -> bool {
    env_switch().load(Relaxed)
}

pub(crate) fn set_env(on: bool) {
    env_switch().store(on, Relaxed);
}

/// A debug view in place of the reflections: devctl `rt view
/// off|normal|source|category` (`ssr.comp`: flags bits 6 and 7; category
/// colours what each ray hit by kind).
pub(crate) const VIEWS: [&str; 4] = ["off", "normal", "source", "category"];
static VIEW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub(crate) fn set_view(name: &str) -> Result<(), String> {
    let i = VIEWS.iter().position(|v| *v == name).ok_or_else(|| format!("rt view {}", VIEWS.join("|")))?;
    VIEW.store(i as u32, Relaxed);
    Ok(())
}

pub(crate) fn view_flags() -> u32 {
    VIEW.load(Relaxed) << 6
}

/// How far from the camera cached triangles are traced, in studs
/// (`INTOXICATED_RT_REACH`, 512 unless set; twice it they are dropped).
pub const REACH_ENV: &str = "INTOXICATED_RT_REACH";

fn reach() -> f32 {
    static R: OnceLock<f32> = OnceLock::new();
    *R.get_or_init(|| std::env::var(REACH_ENV).ok().and_then(|v| v.trim().parse::<f32>().ok()).filter(|r| *r >= 16.0).unwrap_or(512.0))
}

/// Diffuse indirect light (experimental): `INTOXICATED_RT_GI=1` at start
/// makes room for it and turns it on; devctl `rt gi on|off`.
pub const GI_ENV: &str = "INTOXICATED_RT_GI";
static GI_ON: OnceLock<AtomicBool> = OnceLock::new();

pub(crate) fn gi_requested() -> bool {
    std::env::var(GI_ENV).is_ok_and(|v| v.trim() == "1")
}

fn gi_switch() -> &'static AtomicBool {
    GI_ON.get_or_init(|| AtomicBool::new(gi_requested()))
}

pub(crate) fn gi_on() -> bool {
    gi_switch().load(Relaxed)
}

pub(crate) fn set_gi(on: bool) -> Result<(), String> {
    if on && !gi_requested() {
        return Err(format!("the indirect light has no room in this run ({GI_ENV}=1 at start makes it)"));
    }
    gi_switch().store(on, Relaxed);
    Ok(())
}

/// devctl `rt dump-cache <path>`: armed for the next frame, then written.
static CACHE_DUMP_NEXT: AtomicBool = AtomicBool::new(false);

/// How many keys to sort for `triangles`: a power of two with room to grow,
/// at least one block and at most the capacity's power of two.
pub(crate) fn sort_size(triangles: u64, capacity: u64) -> u32 {
    let want = (triangles + triangles / 4 + 4096).max(BLOCK as u64);
    let cap = capacity.next_power_of_two().max(BLOCK as u64);
    want.next_power_of_two().min(cap) as u32
}

/// The dispatches of one bitonic sort of `n` keys: (pipeline, k, j, groups).
pub(crate) fn sort_plan(n: u32) -> Vec<(usize, u32, u32, u32)> {
    let mut plan = vec![(SORT_LOCAL, 0, 0, n / BLOCK)];
    let mut k = 2 * BLOCK;
    while k <= n {
        let mut j = k / 2;
        while j >= BLOCK {
            plan.push((SORT_GLOBAL, k, j, (n / 2).div_ceil(256)));
            j /= 2;
        }
        plan.push((SORT_LOCAL, k, 0, n / BLOCK));
        k *= 2;
    }
    plan
}

struct Fns {
    create_shader_module: CreateFn<ShaderModuleCreateInfo>,
    destroy_shader_module: DestroyFn,
    create_compute_pipelines: extern "C" fn(Dev, u64, u32, *const ComputePipelineCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_pipeline: DestroyFn,
    create_set_layout: CreateFn<DescriptorSetLayoutCreateInfo>,
    destroy_set_layout: DestroyFn,
    create_pipeline_layout: CreateFn<PipelineLayoutCreateInfo>,
    destroy_pipeline_layout: DestroyFn,
    create_descriptor_pool: CreateFn<DescriptorPoolCreateInfo>,
    destroy_descriptor_pool: DestroyFn,
    allocate_descriptor_sets: extern "C" fn(Dev, *const DescriptorSetAllocateInfo, *mut u64) -> i32,
    update_descriptor_sets: extern "C" fn(Dev, u32, *const WriteDescriptorSet, u32, *const c_void),
    create_query_pool: CreateFn<[u32; 8]>,
    destroy_query_pool: DestroyFn,
    #[allow(clippy::type_complexity)]
    get_query_pool_results: extern "C" fn(Dev, u64, u32, u32, usize, *mut c_void, u64, u32) -> i32,
    cmd_bind_pipeline: extern "C" fn(*mut c_void, u32, u64),
    #[allow(clippy::type_complexity)]
    cmd_bind_descriptor_sets: extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u64, u32, *const u32),
    cmd_push_constants: extern "C" fn(*mut c_void, u64, u32, u32, u32, *const c_void),
    cmd_dispatch: extern "C" fn(*mut c_void, u32, u32, u32),
    cmd_fill_buffer: extern "C" fn(*mut c_void, u64, u64, u64, u32),
    cmd_reset_query_pool: extern "C" fn(*mut c_void, u64, u32, u32),
    cmd_write_timestamp: extern "C" fn(*mut c_void, u32, u64, u32),
    cmd_update_buffer: extern "C" fn(*mut c_void, u64, u64, u64, *const c_void),
    create_image: CreateFn<[u32; 22]>,
    destroy_image: DestroyFn,
    image_requirements: extern "C" fn(Dev, u64, *mut super::super::frame_capture::MemoryRequirements),
    bind_image_memory: extern "C" fn(Dev, u64, u64, u64) -> i32,
    create_image_view: CreateFn<ImageViewCreateInfo>,
    destroy_image_view: DestroyFn,
    create_sampler: CreateFn<SamplerCreateInfo>,
    destroy_sampler: DestroyFn,
}

impl Fns {
    fn resolve(d: Dev) -> Result<Fns, String> {
        Ok(Fns {
            create_shader_module: device_fn(d, c"vkCreateShaderModule")?,
            destroy_shader_module: device_fn(d, c"vkDestroyShaderModule")?,
            create_compute_pipelines: device_fn(d, c"vkCreateComputePipelines")?,
            destroy_pipeline: device_fn(d, c"vkDestroyPipeline")?,
            create_set_layout: device_fn(d, c"vkCreateDescriptorSetLayout")?,
            destroy_set_layout: device_fn(d, c"vkDestroyDescriptorSetLayout")?,
            create_pipeline_layout: device_fn(d, c"vkCreatePipelineLayout")?,
            destroy_pipeline_layout: device_fn(d, c"vkDestroyPipelineLayout")?,
            create_descriptor_pool: device_fn(d, c"vkCreateDescriptorPool")?,
            destroy_descriptor_pool: device_fn(d, c"vkDestroyDescriptorPool")?,
            allocate_descriptor_sets: device_fn(d, c"vkAllocateDescriptorSets")?,
            update_descriptor_sets: device_fn(d, c"vkUpdateDescriptorSets")?,
            create_query_pool: device_fn(d, c"vkCreateQueryPool")?,
            destroy_query_pool: device_fn(d, c"vkDestroyQueryPool")?,
            get_query_pool_results: device_fn(d, c"vkGetQueryPoolResults")?,
            cmd_bind_pipeline: device_fn(d, c"vkCmdBindPipeline")?,
            cmd_bind_descriptor_sets: device_fn(d, c"vkCmdBindDescriptorSets")?,
            cmd_push_constants: device_fn(d, c"vkCmdPushConstants")?,
            cmd_dispatch: device_fn(d, c"vkCmdDispatch")?,
            cmd_fill_buffer: device_fn(d, c"vkCmdFillBuffer")?,
            cmd_reset_query_pool: device_fn(d, c"vkCmdResetQueryPool")?,
            cmd_write_timestamp: device_fn(d, c"vkCmdWriteTimestamp")?,
            cmd_update_buffer: device_fn(d, c"vkCmdUpdateBuffer")?,
            create_image: device_fn(d, c"vkCreateImage")?,
            destroy_image: device_fn(d, c"vkDestroyImage")?,
            image_requirements: device_fn(d, c"vkGetImageMemoryRequirements")?,
            bind_image_memory: device_fn(d, c"vkBindImageMemory")?,
            create_image_view: device_fn(d, c"vkCreateImageView")?,
            destroy_image_view: device_fn(d, c"vkDestroyImageView")?,
            create_sampler: device_fn(d, c"vkCreateSampler")?,
            destroy_sampler: device_fn(d, c"vkDestroySampler")?,
        })
    }
}

const RING: usize = 16;
const STAMPS: usize = 3;
const WINDOW: usize = 512;

#[derive(Default)]
pub(crate) struct Stats {
    pub frames: u64,
    pub skipped: u64,
    pub last_skip: Option<String>,
    /// Build and trace milliseconds, the last [`WINDOW`] frames.
    pub build_ms: std::collections::VecDeque<f64>,
    pub trace_ms: std::collections::VecDeque<f64>,
    pub keys: u32,
    /// The blocks' sizes in pixels a side: the reflections', the indirect
    /// light's.
    pub scales: (u32, u32),
    /// The globals as last read back (`build.comp`'s `Globals`).
    pub fit: Option<[u32; GLOBALS_READ]>,
    /// The frame the cache was last copied out at, for `rt dump-cache`.
    pub cache_dumped_at: u64,
    /// The engine's light was bound for the last trace.
    pub light: bool,
}

struct Gpu {
    device: usize,
    fns: Fns,
    buffers: capture::Fns,
    set_layout: u64,
    build_layout: u64,
    trace_layout: u64,
    shaders: [u64; PIPELINES],
    pipelines: [u64; PIPELINES],
    pool: u64,
    set: u64,
    /// The capture buffers the set names.
    xfb: u64,
    sorted: Owned,
    nodes: Owned,
    parents: Owned,
    flags: Owned,
    globals: Owned,
    blocks: Owned,
    tris: Owned,
    slots: Owned,
    draws: Owned,
    tex_map: Owned,
    env: Owned,
    /// The indirect light, this frame's and the last one's in turn (room
    /// for every block when `INTOXICATED_RT_GI=1` at start, else a token).
    gi: [Owned; 2],
    gi_room: u64,
    /// The GI filter's buffers, 8 bytes a block.
    gi_filtered: [Owned; 2],
    /// This frame's triangles that give off light, by index.
    emitters: Owned,
    /// The ring slot of the last trace, whose set 2 the apply pass binds.
    last_slot: usize,
    /// A zeroed stand-in for the engine's per-frame uniform block.
    no_frame_block: Owned,
    /// Set 2 of the trace: the frame's textures, one set per ring slot, and
    /// its layout; a 1 by 1 image of ours fills the places no texture takes.
    /// Host-visible: a copy of the cache, made for `rt dump-cache`.
    cache_copy: Owned,
    cache_copy_ptr: usize,
    tex_layout: u64,
    tex_pool: u64,
    tex_sets: [u64; RING],
    sampler: u64,
    dummy: u64,
    dummy_memory: u64,
    dummy_view: u64,
    dummy_ready: bool,
    /// The blocks were cleared once (they carry the last frame's results).
    blocks_cleared: bool,
    /// Steps the time budget has coarsened the blocks by (0 to
    /// `MAX_COARSER`), the frames since it last looked, and the scales the
    /// buffers' contents were made at.
    coarser: u32,
    budget_frames: u32,
    scales: (u32, u32),
    /// Host-visible: a copy of the globals (the camera FIT measured).
    readback: Owned,
    readback_ptr: usize,
    /// Blocks `blocks` has room for.
    block_room: u64,
    /// Keys the buffers have room for.
    keys: u32,
    query_pool: u64,
    pending: [bool; RING],
    next_slot: usize,
}

#[derive(Default)]
struct State {
    gpus: Vec<Gpu>,
    failed: Vec<(usize, String)>,
    stats: Stats,
    reported: std::collections::HashSet<String>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.get_or_init(|| Mutex::new(State::default())).lock().unwrap_or_else(|e| e.into_inner())
}

impl Gpu {
    fn destroy(self) {
        let (d, none) = (self.device as Dev, std::ptr::null());
        let f = &self.fns;
        for p in self.pipelines.into_iter().filter(|&p| p != 0) {
            (f.destroy_pipeline)(d, p, none);
        }
        for s in self.shaders.into_iter().filter(|&s| s != 0) {
            (f.destroy_shader_module)(d, s, none);
        }
        for l in [self.build_layout, self.trace_layout].into_iter().filter(|&l| l != 0) {
            (f.destroy_pipeline_layout)(d, l, none);
        }
        if self.set_layout != 0 {
            (f.destroy_set_layout)(d, self.set_layout, none);
        }
        if self.pool != 0 {
            (f.destroy_descriptor_pool)(d, self.pool, none);
        }
        if self.query_pool != 0 {
            (f.destroy_query_pool)(d, self.query_pool, none);
        }
        if self.tex_pool != 0 {
            (f.destroy_descriptor_pool)(d, self.tex_pool, none);
        }
        if self.tex_layout != 0 {
            (f.destroy_set_layout)(d, self.tex_layout, none);
        }
        if self.sampler != 0 {
            (f.destroy_sampler)(d, self.sampler, none);
        }
        if self.dummy_view != 0 {
            (f.destroy_image_view)(d, self.dummy_view, none);
        }
        if self.dummy != 0 {
            (f.destroy_image)(d, self.dummy, none);
        }
        if self.dummy_memory != 0 {
            (self.buffers.free_memory)(d, self.dummy_memory, none);
        }
        for o in [
            self.sorted,
            self.nodes,
            self.parents,
            self.flags,
            self.globals,
            self.blocks,
            self.tris,
            self.slots,
            self.draws,
            self.tex_map,
            self.env,
            self.gi[0],
            self.gi[1],
            self.gi_filtered[0],
            self.gi_filtered[1],
            self.emitters,
            self.no_frame_block,
            self.readback,
            self.cache_copy,
        ] {
            if o.buffer != 0 {
                (self.buffers.destroy_buffer)(d, o.buffer, none);
            }
            if o.memory != 0 {
                (self.buffers.free_memory)(d, o.memory, none);
            }
        }
    }

    fn barrier(&self, cmd: *mut c_void, src_stage: u32, src: u32) {
        let b = MemoryBarrier {
            s_type: ST_MEMORY_BARRIER,
            p_next: std::ptr::null(),
            src_access_mask: src,
            dst_access_mask: ACCESS_SHADER_READ | ACCESS_SHADER_WRITE,
        };
        (self.buffers.pipeline_barrier)(cmd, src_stage, STAGE_COMPUTE, 0, 1, &b, 0, std::ptr::null(), 0, std::ptr::null());
    }

    /// The build and trace milliseconds this slot held, when they are in.
    fn collect(&mut self, slot: usize, period_ns: f64, mask: u64) -> Option<(f64, f64)> {
        if !self.pending[slot] || self.query_pool == 0 {
            return None;
        }
        let mut out = [0u64; 2 * STAMPS];
        let rc = (self.fns.get_query_pool_results)(
            self.device as Dev,
            self.query_pool,
            (slot * STAMPS) as u32,
            STAMPS as u32,
            std::mem::size_of_val(&out),
            out.as_mut_ptr().cast(),
            16,
            QUERY_RESULT_64 | QUERY_RESULT_WITH_AVAILABILITY,
        );
        self.pending[slot] = false;
        if rc != 0 || out.chunks(2).any(|c| c[1] == 0) {
            return None;
        }
        let ms = |a: u64, b: u64| super::super::ssr::ticks_to_ms(a, b, mask, period_ns);
        Some((ms(out[0], out[2]), ms(out[2], out[4])))
    }
}

fn shader(f: &Fns, d: Dev, code: &[u32]) -> Result<u64, String> {
    let info = ShaderModuleCreateInfo {
        s_type: ST_SHADER_MODULE_CREATE_INFO,
        p_next: std::ptr::null(),
        flags: 0,
        code_size: std::mem::size_of_val(code),
        code: code.as_ptr(),
    };
    let mut m = 0;
    check((f.create_shader_module)(d, &info, std::ptr::null(), &mut m), "vkCreateShaderModule")?;
    Ok(m)
}

fn make_gpu(device: usize, ssr_set_layout: u64, timed: bool, extent: [u32; 2]) -> Result<Gpu, String> {
    let d = device as Dev;
    let c = capture::frame_captured(device).ok_or("the scene was not captured")?;
    let (xfb, capacity) = (c.xfb, c.capacity);
    let keys = sort_size(listed(), listed());
    let mut g = Gpu {
        device,
        fns: Fns::resolve(d)?,
        buffers: capture::Fns::resolve(d)?,
        set_layout: 0,
        build_layout: 0,
        trace_layout: 0,
        shaders: [0; PIPELINES],
        pipelines: [0; PIPELINES],
        pool: 0,
        set: 0,
        xfb,
        sorted: Owned::default(),
        nodes: Owned::default(),
        parents: Owned::default(),
        flags: Owned::default(),
        globals: Owned::default(),
        blocks: Owned::default(),
        tris: Owned::default(),
        slots: Owned::default(),
        draws: Owned::default(),
        tex_map: Owned::default(),
        env: Owned::default(),
        gi: [Owned::default(); 2],
        gi_room: 0,
        gi_filtered: [Owned::default(); 2],
        emitters: Owned::default(),
        last_slot: 0,
        no_frame_block: Owned::default(),
        cache_copy: Owned::default(),
        cache_copy_ptr: 0,
        tex_layout: 0,
        tex_pool: 0,
        tex_sets: [0; RING],
        sampler: 0,
        dummy: 0,
        dummy_memory: 0,
        dummy_view: 0,
        dummy_ready: false,
        blocks_cleared: false,
        coarser: 0,
        budget_frames: 0,
        scales: (0, 0),
        readback: Owned::default(),
        readback_ptr: 0,
        block_room: blocks_of([extent[0].max(ROOM_FOR[0]), extent[1].max(ROOM_FOR[1])]),
        keys,
        query_pool: 0,
        pending: [false; RING],
        next_slot: 0,
    };
    match fill_gpu(&mut g, ssr_set_layout, capacity, timed) {
        Ok(()) => Ok(g),
        Err(e) => {
            g.destroy();
            Err(e)
        }
    }
}

/// Set 2 of the trace (the frame's textures): its layout, a pool with one
/// set per ring slot, our sampler and the 1 by 1 image that fills the places
/// no texture takes.
fn make_textures(g: &mut Gpu, types: &[u32]) -> Result<(), String> {
    let d = g.device as Dev;
    let none: *const c_void = std::ptr::null();
    let f = &g.fns;
    // 0 the textures, 1 the engine's per-frame uniform block (its light).
    let bindings = [
        DescriptorSetLayoutBinding {
            binding: 0,
            kind: DESCRIPTOR_COMBINED_IMAGE_SAMPLER,
            count: TEXTURES,
            stages: SHADER_STAGE_COMPUTE,
            immutable_samplers: none.cast(),
        },
        DescriptorSetLayoutBinding {
            binding: 1,
            kind: DESCRIPTOR_UNIFORM_BUFFER,
            count: 1,
            stages: SHADER_STAGE_COMPUTE,
            immutable_samplers: none.cast(),
        },
    ];
    let layout = DescriptorSetLayoutCreateInfo {
        s_type: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        count: 2,
        bindings: bindings.as_ptr(),
    };
    check((f.create_set_layout)(d, &layout, none, &mut g.tex_layout), "vkCreateDescriptorSetLayout")?;
    let sizes = [
        DescriptorPoolSize { kind: DESCRIPTOR_COMBINED_IMAGE_SAMPLER, count: TEXTURES * RING as u32 },
        DescriptorPoolSize { kind: DESCRIPTOR_UNIFORM_BUFFER, count: RING as u32 },
    ];
    let pool = DescriptorPoolCreateInfo {
        s_type: ST_DESCRIPTOR_POOL_CREATE_INFO,
        p_next: none,
        flags: 0,
        max_sets: RING as u32,
        count: 2,
        sizes: sizes.as_ptr(),
    };
    check((f.create_descriptor_pool)(d, &pool, none, &mut g.tex_pool), "vkCreateDescriptorPool")?;
    let layouts = [g.tex_layout; RING];
    let alloc = DescriptorSetAllocateInfo {
        s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
        p_next: none,
        pool: g.tex_pool,
        count: RING as u32,
        layouts: layouts.as_ptr(),
    };
    check((f.allocate_descriptor_sets)(d, &alloc, g.tex_sets.as_mut_ptr()), "vkAllocateDescriptorSets")?;
    let sampler = SamplerCreateInfo {
        s_type: ST_SAMPLER_CREATE_INFO,
        p_next: none,
        flags: 0,
        mag_filter: 1,
        min_filter: 1,
        mipmap_mode: 1,
        address: [0; 3],
        mip_lod_bias: 0.0,
        anisotropy_enable: 0,
        max_anisotropy: 1.0,
        compare_enable: 0,
        compare_op: 0,
        min_lod: 0.0,
        max_lod: 16.0,
        border_color: 0,
        unnormalized: 0,
    };
    check((f.create_sampler)(d, &sampler, none, &mut g.sampler), "vkCreateSampler")?;
    // `VkImageCreateInfo` as words: sType, 2D, R8G8B8A8_UNORM, 1x1x1, one
    // level and layer, one sample, optimal tiling, sampled, undefined.
    let mut image = [0u32; 22];
    image[0] = ST_IMAGE_CREATE_INFO;
    image[5] = 1;
    image[6] = FORMAT_RGBA8;
    image[7] = 1;
    image[8] = 1;
    image[9] = 1;
    image[10] = 1;
    image[11] = 1;
    image[12] = 1;
    image[14] = USAGE_SAMPLED;
    check((f.create_image)(d, &image, none, &mut g.dummy), "vkCreateImage")?;
    let mut req = super::super::frame_capture::MemoryRequirements::default();
    (f.image_requirements)(d, g.dummy, &mut req);
    let index = (0..types.len())
        .find(|&i| req.memory_type_bits & (1 << i) != 0 && types[i] & 1 != 0)
        .ok_or("no device-local memory type fits the image")?;
    let alloc = super::super::frame_capture::MemoryAllocateInfo {
        s_type: ST_MEMORY_ALLOCATE_INFO,
        p_next: none,
        allocation_size: req.size,
        memory_type_index: index as u32,
    };
    check((g.buffers.allocate_memory)(d, &alloc, none, &mut g.dummy_memory), "vkAllocateMemory")?;
    check((f.bind_image_memory)(d, g.dummy, g.dummy_memory, 0), "vkBindImageMemory")?;
    let view = ImageViewCreateInfo {
        s_type: ST_IMAGE_VIEW_CREATE_INFO,
        p_next: none,
        flags: 0,
        image: g.dummy,
        view_type: 1,
        format: FORMAT_RGBA8,
        components: [0; 4],
        range: super::super::frame_capture::ImageSubresourceRange {
            aspect_mask: 1,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        },
    };
    check((f.create_image_view)(d, &view, none, &mut g.dummy_view), "vkCreateImageView")
}

fn fill_gpu(g: &mut Gpu, ssr_set_layout: u64, capacity: u64, timed: bool) -> Result<(), String> {
    let d = g.device as Dev;
    let none: *const c_void = std::ptr::null();
    let types = memory_types()?;
    let n = g.keys as u64;
    let usage = USAGE_STORAGE | USAGE_TRANSFER_DST;
    g.sorted = make_buffer(&g.buffers, d, &types, 8 * n, usage, false)?;
    g.nodes = make_buffer(&g.buffers, d, &types, 64 * n, usage, false)?;
    g.parents = make_buffer(&g.buffers, d, &types, 4 * 2 * n, usage, false)?;
    g.flags = make_buffer(&g.buffers, d, &types, 4 * n, usage, false)?;
    g.globals = make_buffer(&g.buffers, d, &types, GLOBALS_BYTES, usage, false)?;
    g.blocks = make_buffer(&g.buffers, d, &types, BLOCK_BYTES * g.block_room, usage, false)?;
    g.tris = make_buffer(&g.buffers, d, &types, TRI_BYTES * listed(), usage, false)?;
    g.slots = make_buffer(&g.buffers, d, &types, SLOT_BYTES * slots() as u64, usage, false)?;
    g.draws = make_buffer(&g.buffers, d, &types, 16 * MAX_DRAWS as u64, usage, false)?;
    g.tex_map = make_buffer(&g.buffers, d, &types, 8 * TEX_MAP as u64, usage, false)?;
    g.env = make_buffer(&g.buffers, d, &types, ENV_BYTES, usage, false)?;
    g.gi_room = if gi_requested() { GI_BYTES * g.block_room } else { 64 };
    g.gi = [
        make_buffer(&g.buffers, d, &types, g.gi_room, usage, false)?,
        make_buffer(&g.buffers, d, &types, g.gi_room, usage, false)?,
    ];
    g.gi_filtered = [
        make_buffer(&g.buffers, d, &types, g.gi_room / 4, usage, false)?,
        make_buffer(&g.buffers, d, &types, g.gi_room / 4, usage, false)?,
    ];
    g.emitters = make_buffer(&g.buffers, d, &types, EMITTERS_BYTES, usage, false)?;
    g.no_frame_block = make_buffer(&g.buffers, d, &types, FRAME_BLOCK_BYTES, USAGE_UNIFORM | USAGE_TRANSFER_DST, false)?;
    make_textures(g, &types)?;
    g.readback = make_buffer(&g.buffers, d, &types, READBACK_BYTES, USAGE_TRANSFER_DST, true)?;
    let mut mapped = std::ptr::null_mut();
    check((g.buffers.map_memory)(d, g.readback.memory, 0, READBACK_BYTES, 0, &mut mapped), "vkMapMemory")?;
    g.readback_ptr = mapped as usize;
    let f = &g.fns;
    let bindings: Vec<DescriptorSetLayoutBinding> = BINDINGS
        .into_iter()
        .map(|b| DescriptorSetLayoutBinding {
            binding: b,
            kind: DESCRIPTOR_STORAGE_BUFFER,
            count: 1,
            stages: SHADER_STAGE_COMPUTE,
            immutable_samplers: none.cast(),
        })
        .collect();
    let layout = DescriptorSetLayoutCreateInfo {
        s_type: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        count: bindings.len() as u32,
        bindings: bindings.as_ptr(),
    };
    check((f.create_set_layout)(d, &layout, none, &mut g.set_layout), "vkCreateDescriptorSetLayout")?;
    let build_push = PushConstantRange { stages: SHADER_STAGE_COMPUTE, offset: 0, size: std::mem::size_of::<BuildParams>() as u32 };
    let info = PipelineLayoutCreateInfo {
        s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        set_layout_count: 1,
        set_layouts: &g.set_layout,
        push_constant_count: 1,
        push_constants: &build_push,
    };
    check((f.create_pipeline_layout)(d, &info, none, &mut g.build_layout), "vkCreatePipelineLayout")?;
    // The trace: the reflections' own set and push constants, then ours,
    // then the textures.
    let trace_sets = [ssr_set_layout, g.set_layout, g.tex_layout];
    let trace_push = PushConstantRange {
        stages: SHADER_STAGE_COMPUTE,
        offset: 0,
        size: std::mem::size_of::<super::super::ssr::Params>() as u32,
    };
    let info = PipelineLayoutCreateInfo {
        s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        set_layout_count: 3,
        set_layouts: trace_sets.as_ptr(),
        push_constant_count: 1,
        push_constants: &trace_push,
    };
    check((f.create_pipeline_layout)(d, &info, none, &mut g.trace_layout), "vkCreatePipelineLayout")?;
    let codes: [&[u32]; PIPELINES] = [
        SPIRV_PREP,
        SPIRV_SORT_LOCAL,
        SPIRV_SORT_GLOBAL,
        SPIRV_BUILD,
        SPIRV_BOUNDS,
        SPIRV_TRACE,
        SPIRV_UPSAMPLE,
        SPIRV_FIT,
        SPIRV_INSERT,
        SPIRV_GATHER,
        SPIRV_ENV,
        SPIRV_GI,
        SPIRV_GI_APPLY,
        SPIRV_GI_FILTER,
    ];
    for (i, code) in codes.into_iter().enumerate() {
        g.shaders[i] = shader(f, d, code)?;
        let pipeline = ComputePipelineCreateInfo {
            s_type: ST_COMPUTE_PIPELINE_CREATE_INFO,
            p_next: none,
            flags: 0,
            stage: PipelineShaderStageCreateInfo {
                s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                p_next: none,
                flags: 0,
                stage: SHADER_STAGE_COMPUTE,
                module: g.shaders[i],
                name: c"main".as_ptr(),
                specialization: none,
            },
            layout: if matches!(i, TRACE | UPSAMPLE | GATHER | ENV | GI | GI_APPLY | GI_FILTER) { g.trace_layout } else { g.build_layout },
            base_pipeline: 0,
            base_pipeline_index: -1,
        };
        check((f.create_compute_pipelines)(d, 0, 1, &pipeline, none, &mut g.pipelines[i]), "vkCreateComputePipelines")?;
    }
    let size = DescriptorPoolSize { kind: DESCRIPTOR_STORAGE_BUFFER, count: BINDINGS.len() as u32 };
    let pool = DescriptorPoolCreateInfo { s_type: ST_DESCRIPTOR_POOL_CREATE_INFO, p_next: none, flags: 0, max_sets: 1, count: 1, sizes: &size };
    check((f.create_descriptor_pool)(d, &pool, none, &mut g.pool), "vkCreateDescriptorPool")?;
    let alloc = DescriptorSetAllocateInfo {
        s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
        p_next: none,
        pool: g.pool,
        count: 1,
        layouts: &g.set_layout,
    };
    check((f.allocate_descriptor_sets)(d, &alloc, &mut g.set), "vkAllocateDescriptorSets")?;
    let infos = [
        DescriptorBufferInfo { buffer: g.xfb, offset: 0, range: capacity },
        DescriptorBufferInfo { buffer: g.sorted.buffer, offset: 0, range: 8 * n },
        DescriptorBufferInfo { buffer: g.nodes.buffer, offset: 0, range: 64 * n },
        DescriptorBufferInfo { buffer: g.parents.buffer, offset: 0, range: 8 * n },
        DescriptorBufferInfo { buffer: g.flags.buffer, offset: 0, range: 4 * n },
        DescriptorBufferInfo { buffer: g.globals.buffer, offset: 0, range: GLOBALS_BYTES },
        DescriptorBufferInfo { buffer: g.blocks.buffer, offset: 0, range: BLOCK_BYTES * g.block_room },
        DescriptorBufferInfo { buffer: g.tris.buffer, offset: 0, range: TRI_BYTES * listed() },
        DescriptorBufferInfo { buffer: g.slots.buffer, offset: 0, range: SLOT_BYTES * slots() as u64 },
        DescriptorBufferInfo { buffer: g.draws.buffer, offset: 0, range: 16 * MAX_DRAWS as u64 },
        DescriptorBufferInfo { buffer: g.tex_map.buffer, offset: 0, range: 8 * TEX_MAP as u64 },
        DescriptorBufferInfo { buffer: g.env.buffer, offset: 0, range: ENV_BYTES },
        DescriptorBufferInfo { buffer: g.gi[0].buffer, offset: 0, range: g.gi_room },
        DescriptorBufferInfo { buffer: g.gi[1].buffer, offset: 0, range: g.gi_room },
        DescriptorBufferInfo { buffer: g.gi_filtered[0].buffer, offset: 0, range: g.gi_room / 4 },
        DescriptorBufferInfo { buffer: g.gi_filtered[1].buffer, offset: 0, range: g.gi_room / 4 },
        DescriptorBufferInfo { buffer: g.emitters.buffer, offset: 0, range: EMITTERS_BYTES },
    ];
    let writes: Vec<WriteDescriptorSet> = infos
        .iter()
        .zip(BINDINGS)
        .map(|(info, binding)| WriteDescriptorSet {
            s_type: ST_WRITE_DESCRIPTOR_SET,
            p_next: none,
            set: g.set,
            binding,
            element: 0,
            count: 1,
            kind: DESCRIPTOR_STORAGE_BUFFER,
            image: none.cast(),
            buffer: info,
            texel_views: none.cast(),
        })
        .collect();
    (f.update_descriptor_sets)(d, writes.len() as u32, writes.as_ptr(), 0, none);
    if timed {
        // `VkQueryPoolCreateInfo`: sType, pNext, flags, queryType,
        // queryCount, pipelineStatistics, as 32-bit words.
        let mut q = [0u32; 8];
        q[0] = ST_QUERY_POOL_CREATE_INFO as u32;
        q[5] = QUERY_TYPE_TIMESTAMP;
        q[6] = (RING * STAMPS) as u32;
        check((f.create_query_pool)(d, &q, none, &mut g.query_pool), "vkCreateQueryPool")?;
    }
    Ok(())
}

/// Record the build and the trace into `cmd` in place of the march, with
/// the reflections' `ssr_set` bound as set 0 and their push constants
/// `params`. `Ok(false)`: not traced this frame (off, or nothing captured),
/// and the march should run.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record(
    cmd: *mut c_void,
    device: usize,
    ssr_set_layout: u64,
    ssr_set: u64,
    params: &super::super::ssr::Params,
    extent: [u32; 2],
    period_ns: f64,
    mask: u64,
) -> Result<bool, String> {
    let _timed = capture::Timed::start();
    if !is_on() {
        return Ok(false);
    }
    let Some(cap) = capture::frame_captured(device) else {
        let mut st = state();
        st.stats.skipped += 1;
        st.stats.last_skip = Some("the scene was not captured this frame".into());
        return Ok(false);
    };
    let mut st = state();
    if let Some((_, why)) = st.failed.iter().find(|(d, _)| *d == device) {
        return Err(why.clone());
    }
    if !st.gpus.iter().any(|g| g.device == device) {
        match make_gpu(device, ssr_set_layout, mask != 0, extent) {
            Ok(g) => {
                let line = format!("ray tracing on device 0x{device:x}: up to {} triangles sorted", g.keys);
                if st.reported.insert(line.clone()) {
                    println!("[android] vulkan: rt: {line}");
                }
                st.gpus.push(g);
            }
            Err(why) => {
                st.failed.push((device, why.clone()));
                return Err(why);
            }
        }
    }
    let (xfb, capacity, bytes) = (cap.xfb, cap.capacity, cap.bytes);
    let triangles = bytes / capture::TRIANGLE_BYTES;
    let st = &mut *st;
    let g = st.gpus.iter_mut().find(|g| g.device == device).expect("made above");
    if g.xfb != xfb {
        return Err("the capture buffer changed".into());
    }
    if blocks_of(extent) > g.block_room {
        return Err(format!("the scene ({}x{}) is larger than the trace has room for", extent[0], extent[1]));
    }
    // This frame's triangles and the cache's from the last report (the
    // list is cut at `n` when the cache grew since).
    let cached_last = st.stats.fit.map_or(0, |w| w[3] as u64);
    let n = sort_size(triangles + cached_last, g.keys as u64);
    let slot = g.next_slot;
    g.next_slot = (slot + 1) % RING;
    if let Some((b, t)) = g.collect(slot, period_ns, mask) {
        for (q, v) in [(&mut st.stats.build_ms, b), (&mut st.stats.trace_ms, t)] {
            if q.len() == WINDOW {
                q.pop_front();
            }
            q.push_back(v);
        }
    }
    st.stats.keys = n;
    st.stats.frames += 1;
    let dump_cache = CACHE_DUMP_NEXT.swap(false, Relaxed);
    if dump_cache && g.cache_copy.buffer == 0 {
        let types = memory_types()?;
        g.cache_copy = make_buffer(&g.buffers, g.device as Dev, &types, SLOT_BYTES * slots() as u64, USAGE_TRANSFER_DST, true)?;
        let mut mapped = std::ptr::null_mut();
        check((g.buffers.map_memory)(g.device as Dev, g.cache_copy.memory, 0, SLOT_BYTES * slots() as u64, 0, &mut mapped), "vkMapMemory")?;
        g.cache_copy_ptr = mapped as usize;
    }
    // The blocks' size: the reflections' a side by the output's width (one
    // ray a block on a grid at most `TARGET_WIDTH` / 2 wide), the indirect
    // light's twice that; both coarser while the GPU time stays over the
    // budget, finer again well under it.
    g.budget_frames += 1;
    if g.budget_frames >= BUDGET_FRAMES {
        g.budget_frames = 0;
        let mean = |q: &std::collections::VecDeque<f64>| q.iter().rev().take(60).sum::<f64>() / q.len().clamp(1, 60) as f64;
        // The build does not shrink with the blocks: the trace gets what
        // the budget leaves it, never under 1 ms.
        let room = (BUDGET_MS - mean(&st.stats.build_ms)).max(1.0);
        let trace = mean(&st.stats.trace_ms);
        if trace > room && g.coarser < MAX_COARSER {
            g.coarser += 1;
        } else if trace < 0.5 * room && g.coarser > 0 {
            g.coarser -= 1;
        }
    }
    let rscale = extent[0].div_ceil(TARGET_WIDTH / 2).max(2) + g.coarser;
    let scales = (rscale, 2 * rscale);
    let rescaled = g.scales != scales;
    g.scales = scales;
    st.stats.scales = scales;
    let clear_blocks = !g.blocks_cleared || rescaled;
    let first_clear = !g.blocks_cleared;
    g.blocks_cleared = true;
    let dummy_was_ready = g.dummy_ready;
    g.dummy_ready = true;
    if g.readback_ptr != 0 {
        // SAFETY: our mapping of `READBACK_BYTES`, written by an earlier frame's
        // copy; a torn or old value only skews the report.
        let words: [u32; GLOBALS_READ] = unsafe { std::ptr::read_volatile(g.readback_ptr as *const [u32; GLOBALS_READ]) };
        st.stats.fit = Some(words);
    }
    let f = &g.fns;
    let timed = g.query_pool != 0;
    let stamp = |i: usize| {
        if timed {
            (f.cmd_write_timestamp)(cmd, STAGE_BOTTOM_OF_PIPE, g.query_pool, (slot * STAMPS + i) as u32);
        }
    };
    if timed {
        (f.cmd_reset_query_pool)(cmd, g.query_pool, (slot * STAMPS) as u32, STAMPS as u32);
    }
    stamp(0);
    // The last frame's trace has read our buffers before they are cleared.
    (g.buffers.pipeline_barrier)(cmd, STAGE_COMPUTE, STAGE_TRANSFER | STAGE_COMPUTE, 0, 0, std::ptr::null(), 0, std::ptr::null(), 0, std::ptr::null());
    // The globals' counts each frame (the first four words, and the
    // cache's drops and misses); the rest is kept, cleared once with the
    // cache and the blocks.
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, 0, 16, 0);
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, 132, 12, 0);
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, 160, 4, 0);
    // `full_rate`: std430 puts the `sky` vec4 before it at 192, so 208.
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, GLOBALS_FULL_RATE, 4, 0);
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, GLOBALS_EMITTERS, 4, 0);
    (f.cmd_fill_buffer)(cmd, g.flags.buffer, 0, 4 * n as u64, 0);
    if first_clear {
        (f.cmd_fill_buffer)(cmd, g.globals.buffer, 16, GLOBALS_BYTES - 16, 0);
        (f.cmd_fill_buffer)(cmd, g.slots.buffer, 0, SLOT_BYTES * slots() as u64, 0);
        (f.cmd_fill_buffer)(cmd, g.env.buffer, 0, ENV_BYTES, 0);
        (f.cmd_fill_buffer)(cmd, g.no_frame_block.buffer, 0, FRAME_BLOCK_BYTES, 0);
    }
    // The per-block buffers, when the blocks' size changed (their
    // contents index the old grid).
    if clear_blocks {
        (f.cmd_fill_buffer)(cmd, g.blocks.buffer, 0, BLOCK_BYTES * g.block_room, 0);
        (f.cmd_fill_buffer)(cmd, g.gi[0].buffer, 0, g.gi_room, 0);
        (f.cmd_fill_buffer)(cmd, g.gi[1].buffer, 0, g.gi_room, 0);
        (f.cmd_fill_buffer)(cmd, g.gi_filtered[0].buffer, 0, g.gi_room / 4, 0);
        (f.cmd_fill_buffer)(cmd, g.gi_filtered[1].buffer, 0, g.gi_room / 4, 0);
    }
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, GLOBALS_SCALES, 4, scales.0);
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, GLOBALS_SCALES + 4, 4, scales.1);
    g.barrier(cmd, STAGE_TRANSFER | STAGE_COMPUTE, ACCESS_TRANSFER_WRITE | ACCESS_SHADER_WRITE);
    let current = (triangles.min(capacity / capture::TRIANGLE_BYTES)) as u32;
    // This frame's draws and textures: the draws' codes by first triangle,
    // the views into set 2 (our image where there is none) and the table
    // from serial to place.
    let (draws, textures) = capture::frame_textures(device);
    // (first triangle, texture code, emission, blends).
    let draws: Vec<[u32; 4]> = draws.iter().take(MAX_DRAWS).map(|d| [d[0], d[1], d[3], d[2]]).collect();
    let mut map = vec![[0u32; 2]; TEX_MAP];
    for (place, &(serial, _, _)) in textures.iter().enumerate() {
        if let Some(e) = (0..4).map(|i| (serial as usize + i) % TEX_MAP).find(|&e| map[e][0] == 0) {
            map[e] = [serial, place as u32 + 1];
        }
    }
    let mut infos: Vec<DescriptorImageInfo> = textures
        .iter()
        .map(|&(_, view, layout)| DescriptorImageInfo { sampler: g.sampler, view, layout: layout as i32 })
        .collect();
    infos.resize_with(TEXTURES as usize, || DescriptorImageInfo { sampler: g.sampler, view: g.dummy_view, layout: LAYOUT_SHADER_READ_ONLY });
    let write = WriteDescriptorSet {
        s_type: ST_WRITE_DESCRIPTOR_SET,
        p_next: std::ptr::null(),
        set: g.tex_sets[slot],
        binding: 0,
        element: 0,
        count: TEXTURES,
        kind: DESCRIPTOR_COMBINED_IMAGE_SAMPLER,
        image: infos.as_ptr(),
        buffer: std::ptr::null(),
        texel_views: std::ptr::null(),
    };
    (f.update_descriptor_sets)(g.device as Dev, 1, &write, 0, std::ptr::null());
    // The engine's per-frame uniform block where a captured draw read one
    // large enough, else a zeroed one of ours.
    let block = capture::frame_block(device).filter(|b| b.2 >= FRAME_BLOCK_BYTES);
    let block_info = match block {
        Some((buffer, offset, _)) => DescriptorBufferInfo { buffer, offset, range: FRAME_BLOCK_BYTES },
        None => DescriptorBufferInfo { buffer: g.no_frame_block.buffer, offset: 0, range: FRAME_BLOCK_BYTES },
    };
    let write = WriteDescriptorSet {
        s_type: ST_WRITE_DESCRIPTOR_SET,
        p_next: std::ptr::null(),
        set: g.tex_sets[slot],
        binding: 1,
        element: 0,
        count: 1,
        kind: DESCRIPTOR_UNIFORM_BUFFER,
        image: std::ptr::null(),
        buffer: &block_info,
        texel_views: std::ptr::null(),
    };
    (f.update_descriptor_sets)(g.device as Dev, 1, &write, 0, std::ptr::null());
    st.stats.light = block.is_some();
    if !draws.is_empty() {
        (f.cmd_update_buffer)(cmd, g.draws.buffer, 0, 16 * draws.len() as u64, draws.as_ptr().cast());
    }
    (f.cmd_update_buffer)(cmd, g.tex_map.buffer, 0, 8 * TEX_MAP as u64, map.as_ptr().cast());
    if !dummy_was_ready {
        // Our image, never written, in the layout its descriptors name.
        let b = super::super::frame_capture::ImageMemoryBarrier {
            s_type: ST_IMAGE_MEMORY_BARRIER,
            p_next: std::ptr::null(),
            src_access_mask: 0,
            dst_access_mask: ACCESS_SHADER_READ,
            old_layout: 0,
            new_layout: LAYOUT_SHADER_READ_ONLY,
            src_queue_family_index: !0,
            dst_queue_family_index: !0,
            image: g.dummy,
            subresource_range: super::super::frame_capture::ImageSubresourceRange {
                aspect_mask: 1,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            },
        };
        (g.buffers.pipeline_barrier)(cmd, STAGE_TRANSFER, STAGE_COMPUTE, 0, 0, std::ptr::null(), 0, std::ptr::null(), 1, (&b as *const super::super::frame_capture::ImageMemoryBarrier).cast());
    }
    g.barrier(cmd, STAGE_TRANSFER | STAGE_COMPUTE, ACCESS_TRANSFER_WRITE | ACCESS_SHADER_WRITE);
    let mut bp = BuildParams {
        n,
        cap: listed() as u32,
        k: 0,
        j: 0,
        current,
        flags: u32::from(cap.whole),
        slot_count: slots(),
        size: extent[0] | (extent[1] << 16),
        draw_count: draws.len() as u32,
        pad: [reach().to_bits(), 0, 0],
    };
    let dispatch = |pipeline: usize, bp: &BuildParams, groups: u32, layout: u64| {
        (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[pipeline]);
        (f.cmd_push_constants)(
            cmd,
            layout,
            SHADER_STAGE_COMPUTE,
            0,
            std::mem::size_of::<BuildParams>() as u32,
            (bp as *const BuildParams).cast(),
        );
        (f.cmd_dispatch)(cmd, groups.max(1), 1, 1);
        g.barrier(cmd, STAGE_COMPUTE, ACCESS_SHADER_WRITE);
    };
    let all_sets = [ssr_set, g.set, g.tex_sets[slot]];
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.build_layout, 0, 1, &g.set, 0, std::ptr::null());
    dispatch(FIT, &bp, 1, g.build_layout);
    dispatch(INSERT, &bp, current.div_ceil(256), g.build_layout);
    // GATHER reads the reflections' copy of the depth too.
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.trace_layout, 0, 3, all_sets.as_ptr(), 0, std::ptr::null());
    dispatch(GATHER, &bp, slots().div_ceil(256), g.trace_layout);
    if dump_cache {
        // The cache as GATHER left it, for `rt dump-cache`.
        let to_copy = MemoryBarrier { s_type: ST_MEMORY_BARRIER, p_next: std::ptr::null(), src_access_mask: ACCESS_SHADER_WRITE, dst_access_mask: ACCESS_TRANSFER_READ };
        (g.buffers.pipeline_barrier)(cmd, STAGE_COMPUTE, STAGE_TRANSFER, 0, 1, &to_copy, 0, std::ptr::null(), 0, std::ptr::null());
        let region = [0u64, 0, SLOT_BYTES * slots() as u64];
        (g.buffers.copy_buffer)(cmd, g.slots.buffer, g.cache_copy.buffer, 1, region.as_ptr());
        st.stats.cache_dumped_at = st.stats.frames;
    }
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.build_layout, 0, 1, &g.set, 0, std::ptr::null());
    let b = g.build_layout;
    dispatch(PREP, &bp, n.div_ceil(256), b);
    for (pipeline, k, j, groups) in sort_plan(n) {
        bp.k = k;
        bp.j = j;
        dispatch(pipeline, &bp, groups, b);
    }
    bp.k = 0;
    bp.j = 0;
    dispatch(BUILD, &bp, n.div_ceil(256), b);
    dispatch(BOUNDS, &bp, n.div_ceil(256), b);
    // The camera FIT measured, for `rt stats`.
    let to_host = MemoryBarrier { s_type: ST_MEMORY_BARRIER, p_next: std::ptr::null(), src_access_mask: ACCESS_SHADER_WRITE, dst_access_mask: ACCESS_TRANSFER_READ };
    (g.buffers.pipeline_barrier)(cmd, STAGE_COMPUTE, STAGE_TRANSFER, 0, 1, &to_host, 0, std::ptr::null(), 0, std::ptr::null());
    let region = [0u64, 0, 4 * GLOBALS_READ as u64];
    (g.buffers.copy_buffer)(cmd, g.globals.buffer, g.readback.buffer, 1, region.as_ptr());
    stamp(1);
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.trace_layout, 0, 3, all_sets.as_ptr(), 0, std::ptr::null());
    let mut tp = *params;
    if !env_on() {
        tp.flags |= FLAG_NO_ENV;
    }
    if st.stats.light {
        tp.flags |= FLAG_LIGHT;
    }
    let gi_run = gi_on() && g.gi_room >= GI_BYTES * blocks_of(extent);
    if gi_run {
        tp.flags |= FLAG_GI;
    }
    if scales.0 == 2 {
        tp.flags |= FLAG_FULL_RATE;
    }
    let push = |q: &super::super::ssr::Params| {
        (f.cmd_push_constants)(
            cmd,
            g.trace_layout,
            SHADER_STAGE_COMPUTE,
            0,
            std::mem::size_of::<super::super::ssr::Params>() as u32,
            (q as *const super::super::ssr::Params).cast(),
        );
    };
    // One invocation a block, the reflections' or the indirect light's.
    let half = |pipeline: usize| {
        // (The environment pass takes a quarter of the pixels, 2 by 2.)
        let s = match pipeline {
            GI | GI_FILTER => scales.1,
            ENV => 2,
            _ => scales.0,
        };
        (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[pipeline]);
        (f.cmd_dispatch)(cmd, extent[0].div_ceil(s).div_ceil(8), extent[1].div_ceil(s).div_ceil(8), 1);
    };
    push(&tp);
    // The sky the opaque copy shows, into the environment, a quarter of the
    // pixels a frame.
    half(ENV);
    g.barrier(cmd, STAGE_COMPUTE, ACCESS_SHADER_WRITE);
    if gi_run {
        // The indirect light first, so the reflections can show it (flags
        // bit 14): one ray a block, accumulated over frames, then filtered
        // by two a-trous passes 1 and 2 blocks apart (the step in `steps`);
        // applied after the reflections' compose (see `record_gi_apply`).
        half(GI);
        for step in [1u32, 2] {
            g.barrier(cmd, STAGE_COMPUTE, ACCESS_SHADER_WRITE);
            push(&super::super::ssr::Params { steps: step, ..tp });
            half(GI_FILTER);
        }
        g.barrier(cmd, STAGE_COMPUTE, ACCESS_SHADER_WRITE);
        push(&tp);
    }
    // One ray per 2 by 2 block, then every pixel from the blocks around it.
    half(TRACE);
    g.barrier(cmd, STAGE_COMPUTE, ACCESS_SHADER_WRITE);
    (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[UPSAMPLE]);
    (f.cmd_dispatch)(cmd, extent[0].div_ceil(8), extent[1].div_ceil(8), 1);
    stamp(2);
    if timed {
        g.pending[slot] = true;
    }
    g.last_slot = slot;
    capture::traced();
    Ok(true)
}

/// After the reflections' compose has written its result: the indirect
/// light applied to it, when it is on and was traced this frame. The same
/// sets as the trace; `params` the reflections' push constants.
pub(crate) fn record_gi_apply(cmd: *mut c_void, device: usize, ssr_set: u64, params: &super::super::ssr::Params, extent: [u32; 2]) {
    if !gi_on() {
        return;
    }
    let st = state();
    let Some(g) = st.gpus.iter().find(|g| g.device == device) else { return };
    if g.gi_room < GI_BYTES * blocks_of(extent) || !st.stats.light {
        return;
    }
    let f = &g.fns;
    let sets = [ssr_set, g.set, g.tex_sets[g.last_slot]];
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.trace_layout, 0, 3, sets.as_ptr(), 0, std::ptr::null());
    let mut tp = *params;
    tp.flags |= FLAG_LIGHT;
    (f.cmd_push_constants)(
        cmd,
        g.trace_layout,
        SHADER_STAGE_COMPUTE,
        0,
        std::mem::size_of::<super::super::ssr::Params>() as u32,
        (&tp as *const super::super::ssr::Params).cast(),
    );
    (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[GI_APPLY]);
    (f.cmd_dispatch)(cmd, extent[0].div_ceil(8), extent[1].div_ceil(8), 1);
}

/// devctl `rt dump-cache <path>`: the cache's live triangles after the next
/// traced frame, one line each: frame first and last seen, frames in a row
/// its place showed empty, the light it gives off (packed), its
/// texture code, its longest edge in studs and its corners.
pub(crate) fn dump_cache(path: &str) -> Result<String, String> {
    let armed_at = state().stats.frames;
    CACHE_DUMP_NEXT.store(true, Relaxed);
    let t0 = std::time::Instant::now();
    loop {
        {
            let st = state();
            if st.stats.cache_dumped_at > armed_at && st.stats.frames >= st.stats.cache_dumped_at + 6 {
                break;
            }
        }
        if t0.elapsed() > std::time::Duration::from_secs(5) {
            CACHE_DUMP_NEXT.store(false, Relaxed);
            return Err("no frame was traced within 5 s".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let st = state();
    let g = st.gpus.first().ok_or("no trace on any device")?;
    let generation = st.stats.fit.map_or(0, |w| w[32]) & 0xff;
    // SAFETY: our mapping of the whole copy, written six frames ago.
    let words = unsafe { std::slice::from_raw_parts(g.cache_copy_ptr as *const u32, (SLOT_BYTES / 4 * slots() as u64) as usize) };
    let mut text = String::from("born\tseen\tmisses\temission\tcode\tedge\tA\tB\tC\n");
    let mut live = 0;
    for slot in words.chunks(32) {
        let tag = slot[0];
        if tag == 0 || tag >> 24 != generation {
            continue;
        }
        live += 1;
        let p = |i: usize| [f32::from_bits(slot[8 + 3 * i]), f32::from_bits(slot[9 + 3 * i]), f32::from_bits(slot[10 + 3 * i])];
        let (a, b, c) = (p(0), p(1), p(2));
        let d = |x: [f32; 3], y: [f32; 3]| ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)).sqrt();
        let edge = d(a, b).max(d(b, c)).max(d(c, a));
        text.push_str(&format!(
            "{}\t{}\t{}\t{:08x}\t{:08x}\t{edge:.2}\t{:.2},{:.2},{:.2}\t{:.2},{:.2},{:.2}\t{:.2},{:.2},{:.2}\n",
            slot[6], slot[2], slot[3], slot[5], slot[4], a[0], a[1], a[2], b[0], b[1], b[2], c[0], c[1], c[2]
        ));
    }
    drop(st);
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    Ok(format!("{live} cached triangles to {path}"))
}

/// A frame the trace was wanted for but not recorded.
pub(crate) fn failed(why: &str) {
    let mut st = state();
    st.stats.skipped += 1;
    st.stats.last_skip = Some(why.to_string());
    let line = format!("the trace was not recorded: {why} (said once)");
    if st.reported.insert(line.clone()) {
        println!("[android] vulkan: rt: {line}");
    }
}

fn mean(q: &std::collections::VecDeque<f64>) -> f64 {
    if q.is_empty() {
        0.0
    } else {
        q.iter().sum::<f64>() / q.len() as f64
    }
}

fn p95(q: &std::collections::VecDeque<f64>) -> f64 {
    let mut v: Vec<f64> = q.iter().copied().collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[(v.len() * 95 / 100).min(v.len() - 1)]
}

/// The camera FIT measured and the cache, as `rt stats` reports them.
fn fit_text(fit: Option<[u32; GLOBALS_READ]>) -> String {
    let Some(w) = fit else { return "fit=none".into() };
    let f = |i: usize| f32::from_bits(w[i]);
    if w[2] == 0 {
        return format!("fit=failed fit_samples={}", w[1]);
    }
    format!(
        "fit=ok fit_samples={} fov_x={:.2} fov_y={:.2} near={:.4} forward_len={:.4} world_up={:.3},{:.3},{:.3} exposure={:.3} \
         camera={:.1},{:.1},{:.1} cache_listed={} cache_new={} cache_dropped={} cache_full={} cache_generation={} \
         list_cut={} fit_failures={} full_rate_pixels={} emitters={}",
        w[1],
        2.0 * f(4).atan().to_degrees(),
        2.0 * f(5).atan().to_degrees(),
        f(6),
        f(7),
        f(8),
        f(9),
        f(10),
        f(14),
        f(16),
        f(17),
        f(18),
        w[3],
        w[35],
        w[33],
        w[34],
        w[32] & 0xff,
        w[40],
        w[41],
        w[GLOBALS_FULL_RATE as usize / 4],
        w[GLOBALS_EMITTERS as usize / 4]
    )
}

pub(crate) fn stats_line() -> String {
    let st = state();
    let s = &st.stats;
    format!(
        "on={} reflections={} shade={} engine_light={} environment={} gi={} view={} blocks={}x{} gi_blocks={}x{} traced={} skipped={} keys={} {} build_ms={:.3} build_p95={:.3} trace_ms={:.3} trace_p95={:.3} gpu_samples={}{}",
        is_on(),
        if super::super::ssr::reflections_on() { "on" } else { "off" },
        SHADES[shade().load(Relaxed)],
        if s.light { "bound" } else { "none" },
        if env_on() { "on" } else { "off" },
        if gi_on() { "on" } else { "off" },
        VIEWS[VIEW.load(Relaxed) as usize],
        s.scales.0,
        s.scales.0,
        s.scales.1,
        s.scales.1,
        s.frames,
        s.skipped,
        s.keys,
        fit_text(s.fit),
        mean(&s.build_ms),
        p95(&s.build_ms),
        mean(&s.trace_ms),
        p95(&s.trace_ms),
        s.build_ms.len(),
        s.last_skip.as_ref().map_or(String::new(), |w| format!(" last_skip={w:?}"))
    )
}

/// Before `vkDestroyDevice` is forwarded.
pub(crate) fn device_destroying(device: *mut c_void) {
    if !super::requested() {
        return;
    }
    let mut st = state();
    let (gone, keep): (Vec<Gpu>, Vec<Gpu>) = std::mem::take(&mut st.gpus).into_iter().partition(|g| g.device == device as usize);
    st.gpus = keep;
    for g in gone {
        g.destroy();
    }
    st.failed.retain(|(d, _)| *d != device as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sort_is_sized_with_room_and_planned_in_blocks() {
        assert_eq!(sort_size(0, 2 << 20), 4096);
        assert_eq!(sort_size(100_000, 2 << 20), 131_072);
        assert_eq!(sort_size(10 << 20, 2 << 20), 2 << 20);
        let plan = sort_plan(1024);
        assert_eq!(plan, vec![(SORT_LOCAL, 0, 0, 1)]);
        let plan = sort_plan(8192);
        // k = 2048: j 1024 then local; k = 4096: j 2048, 1024, local; k = 8192: three and local.
        assert_eq!(plan.len(), 1 + (1 + 1) + (2 + 1) + (3 + 1));
        assert!(plan.iter().all(|&(p, k, j, _)| (p == SORT_GLOBAL) == (j >= BLOCK) && (p == SORT_LOCAL || k > j)));
    }

    #[test]
    fn one_ray_per_two_by_two_block() {
        assert_eq!(blocks_of([1667, 1349]), 834 * 675);
        assert_eq!(blocks_of([2, 2]), 1);
        assert!(blocks_of([1667, 1349]) <= blocks_of(ROOM_FOR));
    }

    #[test]
    fn the_build_parameters_have_the_shader_layout() {
        assert_eq!(std::mem::size_of::<BuildParams>(), 48);
    }
}
