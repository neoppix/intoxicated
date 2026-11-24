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
    ComputePipelineCreateInfo, CreateFn, DescriptorBufferInfo, DescriptorPoolCreateInfo, DescriptorPoolSize,
    DescriptorSetAllocateInfo, DescriptorSetLayoutBinding, DescriptorSetLayoutCreateInfo, DestroyFn,
    PipelineLayoutCreateInfo, PipelineShaderStageCreateInfo, PushConstantRange, ShaderModuleCreateInfo,
    WriteDescriptorSet,
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
const SPIRV_PREP: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_prep.spv.rs"));
const SPIRV_SORT_LOCAL: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_sort_local.spv.rs"));
const SPIRV_SORT_GLOBAL: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_sort_global.spv.rs"));
const SPIRV_BUILD: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_build.spv.rs"));
const SPIRV_BOUNDS: &[u32] = include!(concat!(env!("OUT_DIR"), "/rt_build_bounds.spv.rs"));
/// `ssr/ssr.comp` with `RT`, and with `RT` and `UPSAMPLE`.
const SPIRV_TRACE: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_trace.spv.rs"));
const SPIRV_UPSAMPLE: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_rt_upsample.spv.rs"));

const PREP: usize = 0;
const SORT_LOCAL: usize = 1;
const SORT_GLOBAL: usize = 2;
const BUILD: usize = 3;
const BOUNDS: usize = 4;
const TRACE: usize = 5;
const UPSAMPLE: usize = 6;
const FIT: usize = 7;
const PIPELINES: usize = 8;
/// The bindings of our set: the capture, its counter, the sorted keys, the
/// nodes, the parents, the flags, the globals and the traced blocks.
const BINDINGS: u32 = 8;
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
    pub tan_half: [f32; 2],
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
    /// The globals as last read back: leaves, captured, fit_ok,
    /// fit_samples, tan_x, tan_y, near, |forward|, the world's up (4),
    /// the trace's two sums, the exposure and a spare word.
    pub fit: Option<[u32; 16]>,
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
    /// The blocks were cleared once (they carry the last frame's results).
    blocks_cleared: bool,
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
        for o in [self.sorted, self.nodes, self.parents, self.flags, self.globals, self.blocks, self.readback] {
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
    let (xfb, counter, capacity, _) = capture::frame_captured(device).ok_or("the scene was not captured")?;
    let keys = sort_size(capacity / capture::TRIANGLE_BYTES, capacity / capture::TRIANGLE_BYTES);
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
        blocks_cleared: false,
        readback: Owned::default(),
        readback_ptr: 0,
        block_room: blocks_of([extent[0].max(ROOM_FOR[0]), extent[1].max(ROOM_FOR[1])]),
        keys,
        query_pool: 0,
        pending: [false; RING],
        next_slot: 0,
    };
    match fill_gpu(&mut g, ssr_set_layout, counter, capacity, timed) {
        Ok(()) => Ok(g),
        Err(e) => {
            g.destroy();
            Err(e)
        }
    }
}

fn fill_gpu(g: &mut Gpu, ssr_set_layout: u64, counter: u64, capacity: u64, timed: bool) -> Result<(), String> {
    let d = g.device as Dev;
    let none: *const c_void = std::ptr::null();
    let types = memory_types()?;
    let n = g.keys as u64;
    let usage = USAGE_STORAGE | USAGE_TRANSFER_DST;
    g.sorted = make_buffer(&g.buffers, d, &types, 8 * n, usage, false)?;
    g.nodes = make_buffer(&g.buffers, d, &types, 64 * n, usage, false)?;
    g.parents = make_buffer(&g.buffers, d, &types, 4 * 2 * n, usage, false)?;
    g.flags = make_buffer(&g.buffers, d, &types, 4 * n, usage, false)?;
    g.globals = make_buffer(&g.buffers, d, &types, 256, usage, false)?;
    g.blocks = make_buffer(&g.buffers, d, &types, BLOCK_BYTES * g.block_room, usage, false)?;
    g.readback = make_buffer(&g.buffers, d, &types, 256, USAGE_TRANSFER_DST, true)?;
    let mut mapped = std::ptr::null_mut();
    check((g.buffers.map_memory)(d, g.readback.memory, 0, 256, 0, &mut mapped), "vkMapMemory")?;
    g.readback_ptr = mapped as usize;
    let f = &g.fns;
    let bindings: Vec<DescriptorSetLayoutBinding> = (0..BINDINGS)
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
    // The trace: the reflections' own set and push constants, then ours.
    let trace_sets = [ssr_set_layout, g.set_layout];
    let trace_push = PushConstantRange {
        stages: SHADER_STAGE_COMPUTE,
        offset: 0,
        size: std::mem::size_of::<super::super::ssr::Params>() as u32,
    };
    let info = PipelineLayoutCreateInfo {
        s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        set_layout_count: 2,
        set_layouts: trace_sets.as_ptr(),
        push_constant_count: 1,
        push_constants: &trace_push,
    };
    check((f.create_pipeline_layout)(d, &info, none, &mut g.trace_layout), "vkCreatePipelineLayout")?;
    let codes: [&[u32]; PIPELINES] =
        [SPIRV_PREP, SPIRV_SORT_LOCAL, SPIRV_SORT_GLOBAL, SPIRV_BUILD, SPIRV_BOUNDS, SPIRV_TRACE, SPIRV_UPSAMPLE, SPIRV_FIT];
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
            layout: if i == TRACE || i == UPSAMPLE { g.trace_layout } else { g.build_layout },
            base_pipeline: 0,
            base_pipeline_index: -1,
        };
        check((f.create_compute_pipelines)(d, 0, 1, &pipeline, none, &mut g.pipelines[i]), "vkCreateComputePipelines")?;
    }
    let size = DescriptorPoolSize { kind: DESCRIPTOR_STORAGE_BUFFER, count: BINDINGS };
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
        DescriptorBufferInfo { buffer: counter, offset: 0, range: 16 },
        DescriptorBufferInfo { buffer: g.sorted.buffer, offset: 0, range: 8 * n },
        DescriptorBufferInfo { buffer: g.nodes.buffer, offset: 0, range: 64 * n },
        DescriptorBufferInfo { buffer: g.parents.buffer, offset: 0, range: 8 * n },
        DescriptorBufferInfo { buffer: g.flags.buffer, offset: 0, range: 4 * n },
        DescriptorBufferInfo { buffer: g.globals.buffer, offset: 0, range: 256 },
        DescriptorBufferInfo { buffer: g.blocks.buffer, offset: 0, range: BLOCK_BYTES * g.block_room },
    ];
    let writes: Vec<WriteDescriptorSet> = infos
        .iter()
        .enumerate()
        .map(|(i, info)| WriteDescriptorSet {
            s_type: ST_WRITE_DESCRIPTOR_SET,
            p_next: none,
            set: g.set,
            binding: i as u32,
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
    if !is_on() {
        return Ok(false);
    }
    let Some((xfb, counter, capacity, bytes)) = capture::frame_captured(device) else {
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
    let triangles = bytes / capture::TRIANGLE_BYTES;
    let st = &mut *st;
    let g = st.gpus.iter_mut().find(|g| g.device == device).expect("made above");
    if g.xfb != xfb {
        return Err("the capture buffer changed".into());
    }
    if blocks_of(extent) > g.block_room {
        return Err(format!("the scene ({}x{}) is larger than the trace has room for", extent[0], extent[1]));
    }
    let n = sort_size(triangles, g.keys as u64);
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
    let clear_blocks = !g.blocks_cleared;
    g.blocks_cleared = true;
    if g.readback_ptr != 0 {
        // SAFETY: our mapping of 256 bytes, written by an earlier frame's
        // copy; a torn or old value only skews the report.
        let words: [u32; 16] = unsafe { std::ptr::read_volatile(g.readback_ptr as *const [u32; 16]) };
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
    // The globals' counts each frame; the exposure and the trace's sums
    // after them are kept (cleared once, below).
    (f.cmd_fill_buffer)(cmd, g.globals.buffer, 0, 16, 0);
    (f.cmd_fill_buffer)(cmd, g.flags.buffer, 0, 4 * n as u64, 0);
    // What this frame's draws wrote, as counted when they were recorded.
    (f.cmd_fill_buffer)(cmd, counter, 0, 4, bytes as u32);
    if clear_blocks {
        (f.cmd_fill_buffer)(cmd, g.blocks.buffer, 0, BLOCK_BYTES * g.block_room, 0);
        (f.cmd_fill_buffer)(cmd, g.globals.buffer, 16, 240, 0);
    }
    g.barrier(cmd, STAGE_TRANSFER | STAGE_COMPUTE, ACCESS_TRANSFER_WRITE | ACCESS_SHADER_WRITE);
    let mut bp = BuildParams {
        n,
        cap: (capacity / capture::TRIANGLE_BYTES) as u32,
        k: 0,
        j: 0,
        tan_half: params.tan_half,
    };
    let dispatch = |pipeline: usize, bp: &BuildParams, groups: u32| {
        (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[pipeline]);
        (f.cmd_push_constants)(
            cmd,
            g.build_layout,
            SHADER_STAGE_COMPUTE,
            0,
            std::mem::size_of::<BuildParams>() as u32,
            (bp as *const BuildParams).cast(),
        );
        (f.cmd_dispatch)(cmd, groups.max(1), 1, 1);
        g.barrier(cmd, STAGE_COMPUTE, ACCESS_SHADER_WRITE);
    };
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.build_layout, 0, 1, &g.set, 0, std::ptr::null());
    dispatch(FIT, &bp, 1);
    dispatch(PREP, &bp, n.div_ceil(256));
    for (pipeline, k, j, groups) in sort_plan(n) {
        bp.k = k;
        bp.j = j;
        dispatch(pipeline, &bp, groups);
    }
    bp.k = 0;
    bp.j = 0;
    dispatch(BUILD, &bp, n.div_ceil(256));
    dispatch(BOUNDS, &bp, n.div_ceil(256));
    // The camera FIT measured, for `rt stats`.
    let to_host = MemoryBarrier { s_type: ST_MEMORY_BARRIER, p_next: std::ptr::null(), src_access_mask: ACCESS_SHADER_WRITE, dst_access_mask: ACCESS_TRANSFER_READ };
    (g.buffers.pipeline_barrier)(cmd, STAGE_COMPUTE, STAGE_TRANSFER, 0, 1, &to_host, 0, std::ptr::null(), 0, std::ptr::null());
    let region = [0u64, 0, 64];
    (g.buffers.copy_buffer)(cmd, g.globals.buffer, g.readback.buffer, 1, region.as_ptr());
    stamp(1);
    (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[TRACE]);
    let sets = [ssr_set, g.set];
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.trace_layout, 0, 2, sets.as_ptr(), 0, std::ptr::null());
    (f.cmd_push_constants)(
        cmd,
        g.trace_layout,
        SHADER_STAGE_COMPUTE,
        0,
        std::mem::size_of::<super::super::ssr::Params>() as u32,
        (params as *const super::super::ssr::Params).cast(),
    );
    // One ray per 2 by 2 block, then every pixel from the blocks around it.
    (f.cmd_dispatch)(cmd, extent[0].div_ceil(2).div_ceil(8), extent[1].div_ceil(2).div_ceil(8), 1);
    g.barrier(cmd, STAGE_COMPUTE, ACCESS_SHADER_WRITE);
    (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[UPSAMPLE]);
    (f.cmd_dispatch)(cmd, extent[0].div_ceil(8), extent[1].div_ceil(8), 1);
    stamp(2);
    if timed {
        g.pending[slot] = true;
    }
    capture::traced();
    Ok(true)
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

/// The camera FIT measured, as `rt stats` reports it.
fn fit_text(fit: Option<[u32; 16]>) -> String {
    let Some(w) = fit else { return "fit=none".into() };
    let f = |i: usize| f32::from_bits(w[i]);
    if w[2] == 0 {
        return format!("fit=failed fit_samples={}", w[3]);
    }
    format!(
        "fit=ok fit_samples={} fov_x={:.2} fov_y={:.2} near={:.4} forward_len={:.4} world_up={:.3},{:.3},{:.3} exposure={:.3}",
        w[3],
        2.0 * f(4).atan().to_degrees(),
        2.0 * f(5).atan().to_degrees(),
        f(6),
        f(7),
        f(8),
        f(9),
        f(10),
        f(14)
    )
}

pub(crate) fn stats_line() -> String {
    let st = state();
    let s = &st.stats;
    format!(
        "on={} reflections={} traced={} skipped={} keys={} {} build_ms={:.3} build_p95={:.3} trace_ms={:.3} trace_p95={:.3} gpu_samples={}{}",
        is_on(),
        if super::super::ssr::reflections_on() { "on" } else { "off" },
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
        assert_eq!(std::mem::size_of::<BuildParams>(), 24);
    }
}
