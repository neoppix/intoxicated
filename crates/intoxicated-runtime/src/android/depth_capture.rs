//! devctl `depthshot <path>`: the scene depth of the next frame, read on the
//! GPU at the end of the frame's 3D scene and written as a grayscale PNG
//! (near bright, far dark), with a colour screenshot of the same frame
//! beside it and the raw floats for whoever wants the numbers.
//!
//! Only Vulkan calls and our own GPU work. Nothing the engine records or
//! submits is changed: the depth is read by a command buffer of our own,
//! submitted on the engine's queue right after the engine's own submit, from
//! inside that submit (which is what makes using the queue there legal).
//!
//! # Which pass is the scene
//!
//! Decided from what each pass draws into, frame by frame, by [`Matcher`]:
//!
//! * a **scene pass** has a depth attachment whose image is the size of a
//!   live swapchain image, in any depth format and with any sample count.
//!   The last one before the HUD pass ends the scene; a frame may have
//!   several (opaque, then transparent, both on the same depth).
//! * the **HUD pass** is the first pass after a scene pass that has no depth
//!   attachment and draws into a swapchain image.
//!
//! Nothing is keyed to a pass index, a pass count, a handle that lives
//! longer than a frame, a pipeline or a shader, so a Roblox update that
//! adds, removes or reorders other passes leaves the match alone. A frame
//! that does not have this shape (no full-size depth, no HUD pass after it,
//! the scene depth drawn to again after the HUD started or overwritten by a
//! copy or clear, the HUD's command buffer never submitted) is answered with
//! that reason and nothing is captured or submitted.
//!
//! # How it is read
//!
//! When the submit that carries the HUD pass's command buffer (and, by
//! then, the scene's) has been forwarded, one command buffer of ours goes
//! to the same queue: a barrier that orders it after everything submitted
//! before it, a compute dispatch that reads sample 0 of every depth texel
//! through a view of the engine's depth image into a host-visible buffer
//! ([`SPIRV_MS`], [`SPIRV_1X`]; GLSL in `depth_capture/resolve.comp`), and a
//! barrier that makes the buffer visible to the host and holds the engine's
//! next frame until the read is done. The depth image stays in the layout
//! the engine left it in when that layout can be sampled, and is otherwise
//! moved to `SHADER_READ_ONLY` and back around the read.
//!
//! Everything else (pipelines, descriptor set, buffer, command pool, fence)
//! is made by the devctl thread before the request is armed and destroyed by
//! it after the fence; the render thread creates one image view, writes one
//! descriptor set and records and submits one command buffer.

use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::frame_capture::{
    self, device_fn, BufferCreateInfo, CommandBufferAllocateInfo, CommandBufferBeginInfo, CommandPoolCreateInfo, Dev,
    ImageMemoryBarrier, ImageSubresourceRange, MappedMemoryRange, MemoryAllocateInfo, MemoryBarrier, MemoryRequirements,
    PhysicalDeviceMemoryProperties, PlainCreateInfo, SubmitInfo, GATE_CANCELLED, GATE_OPEN, GATE_WAIT,
};
use super::vk_layer::passes::{self, Pass, Target, WATCH, WATCH_DEPTH};
use super::vk_layer::registry::{self, Registry, UNUSED};

// ------------------------------------------------------------- the matcher

const LOAD_OP_CLEAR: u32 = 1;

/// The depth attachment of one pass, as far as the registry knows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Depth {
    pub image: u64,
    pub device: usize,
    pub format: u32,
    pub samples: u32,
    pub usage: u32,
    pub extent: [u32; 2],
    /// The layout the pass leaves it in.
    pub layout_after: u32,
    /// The clear value, when the pass clears it.
    pub clear: Option<f32>,
}

/// What the matcher needs to know about one pass.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shape {
    pub cmd: usize,
    /// Whether the pass has a depth attachment at all.
    pub has_depth: bool,
    /// That attachment, when its image is known.
    pub depth: Option<Depth>,
    /// Whether a colour (or resolve) attachment is a swapchain image.
    pub to_swapchain: bool,
}

/// The end of the scene, as found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scene {
    pub depth: Depth,
    /// The command buffer of the last scene pass.
    pub cmd: usize,
    /// The depth's layout as the frame leaves it.
    pub layout: u32,
    /// The first clear value a scene pass used.
    pub clear: Option<f32>,
    /// How many scene passes there were.
    pub passes: u32,
    pub hud_cmd: usize,
}

/// One frame, fed in recording order. See the module doc for the rules.
pub(crate) struct Matcher {
    /// The extents of the live swapchains.
    full: Vec<[u32; 2]>,
    scene: Option<Scene>,
    hud: Option<usize>,
    broken: Option<String>,
    submitted: HashSet<usize>,
    taken: bool,
}

impl Matcher {
    pub(crate) fn new(full: Vec<[u32; 2]>) -> Matcher {
        Matcher { full, scene: None, hud: None, broken: None, submitted: HashSet::new(), taken: false }
    }

    fn is_scene_depth(&self, d: &Depth) -> bool {
        registry::is_depth_format(d.format) && self.full.contains(&d.extent)
    }

    pub(crate) fn pass(&mut self, s: &Shape) {
        if self.broken.is_some() || self.taken {
            return;
        }
        if let Some(d) = s.depth.as_ref().filter(|d| self.is_scene_depth(d)) {
            if self.hud.is_some() {
                self.broken = Some("the scene depth is drawn to again after the pass that draws to the swapchain".into());
                return;
            }
            let (clear, passes) = match &self.scene {
                Some(old) => (old.clear.or(d.clear), old.passes + 1),
                None => (d.clear, 1),
            };
            self.scene = Some(Scene { depth: d.clone(), cmd: s.cmd, layout: d.layout_after, clear, passes, hud_cmd: 0 });
            return;
        }
        if !s.has_depth && s.to_swapchain && self.scene.is_some() && self.hud.is_none() {
            self.hud = Some(s.cmd);
        }
    }

    /// An image barrier moved `image` to `layout`.
    pub(crate) fn barrier(&mut self, image: u64, layout: u32) {
        if let Some(s) = self.scene.as_mut().filter(|s| s.depth.image == image) {
            s.layout = layout;
        }
    }

    /// A copy, blit, resolve or clear wrote `image`.
    pub(crate) fn written(&mut self, image: u64) {
        if self.broken.is_none() && self.scene.as_ref().is_some_and(|s| s.depth.image == image) {
            self.broken = Some("the scene depth is overwritten by a copy or clear after the scene pass".into());
        }
    }

    /// A submit of `cmds` was forwarded. The scene, once: when this submit
    /// carries the HUD pass and the scene's own command buffer has been
    /// submitted by now.
    pub(crate) fn submit(&mut self, cmds: &[usize]) -> Option<Scene> {
        if self.broken.is_some() || self.taken {
            return None;
        }
        self.submitted.extend(cmds);
        let hud = self.hud?;
        let scene = self.scene.as_ref()?;
        if cmds.contains(&hud) && self.submitted.contains(&scene.cmd) {
            self.taken = true;
            return Some(Scene { hud_cmd: hud, ..scene.clone() });
        }
        None
    }

    /// Why a frame that ended without [`Matcher::submit`] returning was not
    /// captured.
    pub(crate) fn why_not(&self) -> String {
        if let Some(b) = &self.broken {
            return b.clone();
        }
        let sizes: Vec<String> = self.full.iter().map(|e| format!("{}x{}", e[0], e[1])).collect();
        match (&self.scene, self.hud) {
            (None, _) => format!(
                "no pass drew into a depth attachment the size of the swapchain ({})",
                if sizes.is_empty() { "none is known".to_string() } else { sizes.join(", ") }
            ),
            (Some(_), None) => "no pass after the scene drew to the swapchain without a depth attachment".into(),
            (Some(_), Some(_)) => "the command buffer that draws to the swapchain was not submitted before the present".into(),
        }
    }
}

/// A pass as the matcher sees it, through the registry.
pub(crate) fn shape_of(pass: &Pass, reg: &Registry) -> Shape {
    let clear_of = |load: u32, index: usize| -> Option<f32> {
        (load == LOAD_OP_CLEAR).then(|| pass.clears.get(index).map(|c| f32::from_bits(c[0]))).flatten()
    };
    let described = |image: u64, info: &registry::Image, layout_after: u32, clear: Option<f32>| Depth {
        image,
        device: info.device,
        format: info.format,
        samples: info.samples,
        usage: info.usage,
        extent: [info.extent[0], info.extent[1]],
        layout_after,
        clear,
    };
    match &pass.target {
        Target::RenderPass { render_pass, framebuffer, views, .. } => {
            let atts = reg.attachments(*render_pass, *framebuffer, views);
            let mut depth_index = None;
            let mut colors = Vec::new();
            if let Some(rp) = reg.render_passes.get(render_pass) {
                for s in &rp.subpasses {
                    if depth_index.is_none() {
                        depth_index = s.depth.map(|r| r.attachment).filter(|&a| a != UNUSED);
                    }
                    colors.extend(s.color.iter().chain(&s.resolve).map(|r| r.attachment).filter(|&a| a != UNUSED));
                }
            }
            let to_swapchain =
                colors.iter().any(|&i| atts.get(i as usize).and_then(|a| a.info.as_ref()).is_some_and(|i| i.swapchain));
            let depth = depth_index.and_then(|i| {
                let a = atts.get(i as usize)?;
                let info = a.info.as_ref()?;
                Some(described(a.image, info, a.desc.final_layout, clear_of(a.desc.load, i as usize)))
            });
            Shape { cmd: pass.cmd, has_depth: depth_index.is_some(), depth, to_swapchain }
        }
        Target::Dynamic { colors, depth, .. } => {
            let swapchain_view = |view: u64| reg.image_of_view(view).1.is_some_and(|i| i.swapchain);
            let to_swapchain = colors.iter().any(|c| swapchain_view(c.view) || (c.resolve_view != 0 && swapchain_view(c.resolve_view)));
            let described_depth = depth.and_then(|d| {
                let (image, info) = reg.image_of_view(d.view);
                // A dynamic pass's clear values are its colour attachments'
                // followed by the depth's.
                Some(described(image, info?, d.layout, clear_of(d.load, colors.len())))
            });
            Shape { cmd: pass.cmd, has_depth: depth.is_some_and(|d| d.view != 0), depth: described_depth, to_swapchain }
        }
    }
}

// ------------------------------------------------------------- Vulkan ABI

const VK_SUCCESS: i32 = 0;
const VK_TIMEOUT: i32 = 2;

const ST_SUBMIT_INFO: i32 = 4;
const ST_MEMORY_ALLOCATE_INFO: i32 = 5;
const ST_MAPPED_MEMORY_RANGE: i32 = 6;
const ST_FENCE_CREATE_INFO: i32 = 8;
const ST_BUFFER_CREATE_INFO: i32 = 12;
const ST_IMAGE_VIEW_CREATE_INFO: i32 = 15;
const ST_SHADER_MODULE_CREATE_INFO: i32 = 16;
const ST_PIPELINE_SHADER_STAGE_CREATE_INFO: i32 = 18;
const ST_COMPUTE_PIPELINE_CREATE_INFO: i32 = 29;
const ST_PIPELINE_LAYOUT_CREATE_INFO: i32 = 30;
const ST_SAMPLER_CREATE_INFO: i32 = 31;
const ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: i32 = 32;
const ST_DESCRIPTOR_POOL_CREATE_INFO: i32 = 33;
const ST_DESCRIPTOR_SET_ALLOCATE_INFO: i32 = 34;
const ST_WRITE_DESCRIPTOR_SET: i32 = 35;
const ST_COMMAND_POOL_CREATE_INFO: i32 = 39;
const ST_COMMAND_BUFFER_ALLOCATE_INFO: i32 = 40;
const ST_COMMAND_BUFFER_BEGIN_INFO: i32 = 42;
const ST_IMAGE_MEMORY_BARRIER: i32 = 45;
const ST_MEMORY_BARRIER: i32 = 46;

const LAYOUT_SHADER_READ_ONLY: i32 = 5;
const STAGE_COMPUTE_SHADER: u32 = 0x800;
const STAGE_HOST: u32 = 0x4000;
const STAGE_ALL_COMMANDS: u32 = 0x1_0000;
const ACCESS_SHADER_READ: u32 = 0x20;
const ACCESS_SHADER_WRITE: u32 = 0x40;
const ACCESS_HOST_READ: u32 = 0x2000;
const ACCESS_MEMORY_WRITE: u32 = 0x1_0000;
const ASPECT_DEPTH: u32 = 0x2;
const ASPECT_STENCIL: u32 = 0x4;
const SHADER_STAGE_COMPUTE: u32 = 0x20;
const BIND_POINT_COMPUTE: u32 = 1;
const DESCRIPTOR_COMBINED_IMAGE_SAMPLER: u32 = 1;
const DESCRIPTOR_STORAGE_BUFFER: u32 = 7;
const BUFFER_USAGE_STORAGE: u32 = 0x20;
const QUEUE_FAMILY_IGNORED: u32 = !0;
const WHOLE_SIZE: u64 = !0;
const COMMAND_POOL_TRANSIENT: u32 = 0x1;
const COMMAND_BUFFER_ONE_TIME_SUBMIT: u32 = 0x1;
const VIEW_TYPE_2D: u32 = 1;

/// Layouts a sampled depth image may be read in.
pub(super) fn sampleable(layout: u32) -> bool {
    matches!(layout, 1 | 4 | 5 | 1_000_117_000 | 1_000_241_001 | 1_000_314_000)
}

/// The aspects a layout transition of `format` has to name.
pub(super) fn barrier_aspects(format: u32) -> u32 {
    match format {
        128..=130 => ASPECT_DEPTH | ASPECT_STENCIL,
        _ => ASPECT_DEPTH,
    }
}

/// `depth_capture/resolve.comp` for a multisampled depth image, as build.rs
/// made it (glslc, or the committed `resolve_ms.spv.rs` without it).
const SPIRV_MS: &[u32] = include!(concat!(env!("OUT_DIR"), "/depth_capture_resolve_ms.spv.rs"));
/// The same for a single-sample one.
const SPIRV_1X: &[u32] = include!(concat!(env!("OUT_DIR"), "/depth_capture_resolve_1x.spv.rs"));

#[repr(C)]
pub(super) struct ImageViewCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) image: u64,
    pub(super) view_type: u32,
    pub(super) format: u32,
    pub(super) components: [u32; 4],
    pub(super) range: ImageSubresourceRange,
}

#[repr(C)]
pub(super) struct SamplerCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) mag_filter: u32,
    pub(super) min_filter: u32,
    pub(super) mipmap_mode: u32,
    pub(super) address: [u32; 3],
    pub(super) mip_lod_bias: f32,
    pub(super) anisotropy_enable: u32,
    pub(super) max_anisotropy: f32,
    pub(super) compare_enable: u32,
    pub(super) compare_op: u32,
    pub(super) min_lod: f32,
    pub(super) max_lod: f32,
    pub(super) border_color: u32,
    pub(super) unnormalized: u32,
}

#[repr(C)]
pub(super) struct DescriptorSetLayoutBinding {
    pub(super) binding: u32,
    pub(super) kind: u32,
    pub(super) count: u32,
    pub(super) stages: u32,
    pub(super) immutable_samplers: *const u64,
}

#[repr(C)]
pub(super) struct DescriptorSetLayoutCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) count: u32,
    pub(super) bindings: *const DescriptorSetLayoutBinding,
}

#[repr(C)]
pub(super) struct PushConstantRange {
    pub(super) stages: u32,
    pub(super) offset: u32,
    pub(super) size: u32,
}

#[repr(C)]
pub(super) struct PipelineLayoutCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) set_layout_count: u32,
    pub(super) set_layouts: *const u64,
    pub(super) push_constant_count: u32,
    pub(super) push_constants: *const PushConstantRange,
}

#[repr(C)]
pub(super) struct ShaderModuleCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) code_size: usize,
    pub(super) code: *const u32,
}

#[repr(C)]
pub(super) struct PipelineShaderStageCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) stage: u32,
    pub(super) module: u64,
    pub(super) name: *const c_char,
    pub(super) specialization: *const c_void,
}

#[repr(C)]
pub(super) struct ComputePipelineCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) stage: PipelineShaderStageCreateInfo,
    pub(super) layout: u64,
    pub(super) base_pipeline: u64,
    pub(super) base_pipeline_index: i32,
}

#[repr(C)]
pub(super) struct DescriptorPoolSize {
    pub(super) kind: u32,
    pub(super) count: u32,
}

#[repr(C)]
pub(super) struct DescriptorPoolCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) max_sets: u32,
    pub(super) count: u32,
    pub(super) sizes: *const DescriptorPoolSize,
}

#[repr(C)]
pub(super) struct DescriptorSetAllocateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) pool: u64,
    pub(super) count: u32,
    pub(super) layouts: *const u64,
}

#[repr(C)]
pub(super) struct DescriptorImageInfo {
    pub(super) sampler: u64,
    pub(super) view: u64,
    pub(super) layout: i32,
}

#[repr(C)]
pub(super) struct DescriptorBufferInfo {
    pub(super) buffer: u64,
    pub(super) offset: u64,
    pub(super) range: u64,
}

#[repr(C)]
pub(super) struct WriteDescriptorSet {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) set: u64,
    pub(super) binding: u32,
    pub(super) element: u32,
    pub(super) count: u32,
    pub(super) kind: u32,
    pub(super) image: *const DescriptorImageInfo,
    pub(super) buffer: *const DescriptorBufferInfo,
    pub(super) texel_views: *const u64,
}

pub(super) type CreateFn<T> = extern "C" fn(Dev, *const T, *const c_void, *mut u64) -> i32;
pub(super) type DestroyFn = extern "C" fn(Dev, u64, *const c_void);

/// The device-level commands a depth capture uses, straight from the
/// driver, so none of them passes through the layer's own hooks.
struct Fns {
    device: usize,
    create_buffer: CreateFn<BufferCreateInfo>,
    destroy_buffer: DestroyFn,
    buffer_requirements: extern "C" fn(Dev, u64, *mut MemoryRequirements),
    allocate_memory: CreateFn<MemoryAllocateInfo>,
    free_memory: DestroyFn,
    bind_buffer_memory: extern "C" fn(Dev, u64, u64, u64) -> i32,
    map_memory: extern "C" fn(Dev, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    unmap_memory: extern "C" fn(Dev, u64),
    invalidate_ranges: extern "C" fn(Dev, u32, *const MappedMemoryRange) -> i32,
    create_command_pool: CreateFn<CommandPoolCreateInfo>,
    destroy_command_pool: DestroyFn,
    allocate_command_buffers: extern "C" fn(Dev, *const CommandBufferAllocateInfo, *mut *mut c_void) -> i32,
    begin_command_buffer: extern "C" fn(*mut c_void, *const CommandBufferBeginInfo) -> i32,
    end_command_buffer: extern "C" fn(*mut c_void) -> i32,
    #[allow(clippy::type_complexity)]
    cmd_pipeline_barrier: extern "C" fn(
        *mut c_void,
        u32,
        u32,
        u32,
        u32,
        *const MemoryBarrier,
        u32,
        *const c_void,
        u32,
        *const ImageMemoryBarrier,
    ),
    queue_submit: extern "C" fn(*mut c_void, u32, *const SubmitInfo, u64) -> i32,
    create_fence: CreateFn<PlainCreateInfo>,
    destroy_fence: DestroyFn,
    wait_for_fences: extern "C" fn(Dev, u32, *const u64, u32, u64) -> i32,
    create_image_view: CreateFn<ImageViewCreateInfo>,
    destroy_image_view: DestroyFn,
    create_sampler: CreateFn<SamplerCreateInfo>,
    destroy_sampler: DestroyFn,
    create_set_layout: CreateFn<DescriptorSetLayoutCreateInfo>,
    destroy_set_layout: DestroyFn,
    create_pipeline_layout: CreateFn<PipelineLayoutCreateInfo>,
    destroy_pipeline_layout: DestroyFn,
    create_shader_module: CreateFn<ShaderModuleCreateInfo>,
    destroy_shader_module: DestroyFn,
    create_compute_pipelines: extern "C" fn(Dev, u64, u32, *const ComputePipelineCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_pipeline: DestroyFn,
    create_descriptor_pool: CreateFn<DescriptorPoolCreateInfo>,
    destroy_descriptor_pool: DestroyFn,
    allocate_descriptor_sets: extern "C" fn(Dev, *const DescriptorSetAllocateInfo, *mut u64) -> i32,
    update_descriptor_sets: extern "C" fn(Dev, u32, *const WriteDescriptorSet, u32, *const c_void),
    cmd_bind_pipeline: extern "C" fn(*mut c_void, u32, u64),
    #[allow(clippy::type_complexity)]
    cmd_bind_descriptor_sets: extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u64, u32, *const u32),
    cmd_push_constants: extern "C" fn(*mut c_void, u64, u32, u32, u32, *const c_void),
    cmd_dispatch: extern "C" fn(*mut c_void, u32, u32, u32),
}

impl Fns {
    fn resolve(device: Dev) -> Result<Fns, String> {
        Ok(Fns {
            device: device as usize,
            create_buffer: device_fn(device, c"vkCreateBuffer")?,
            destroy_buffer: device_fn(device, c"vkDestroyBuffer")?,
            buffer_requirements: device_fn(device, c"vkGetBufferMemoryRequirements")?,
            allocate_memory: device_fn(device, c"vkAllocateMemory")?,
            free_memory: device_fn(device, c"vkFreeMemory")?,
            bind_buffer_memory: device_fn(device, c"vkBindBufferMemory")?,
            map_memory: device_fn(device, c"vkMapMemory")?,
            unmap_memory: device_fn(device, c"vkUnmapMemory")?,
            invalidate_ranges: device_fn(device, c"vkInvalidateMappedMemoryRanges")?,
            create_command_pool: device_fn(device, c"vkCreateCommandPool")?,
            destroy_command_pool: device_fn(device, c"vkDestroyCommandPool")?,
            allocate_command_buffers: device_fn(device, c"vkAllocateCommandBuffers")?,
            begin_command_buffer: device_fn(device, c"vkBeginCommandBuffer")?,
            end_command_buffer: device_fn(device, c"vkEndCommandBuffer")?,
            cmd_pipeline_barrier: device_fn(device, c"vkCmdPipelineBarrier")?,
            queue_submit: device_fn(device, c"vkQueueSubmit")?,
            create_fence: device_fn(device, c"vkCreateFence")?,
            destroy_fence: device_fn(device, c"vkDestroyFence")?,
            wait_for_fences: device_fn(device, c"vkWaitForFences")?,
            create_image_view: device_fn(device, c"vkCreateImageView")?,
            destroy_image_view: device_fn(device, c"vkDestroyImageView")?,
            create_sampler: device_fn(device, c"vkCreateSampler")?,
            destroy_sampler: device_fn(device, c"vkDestroySampler")?,
            create_set_layout: device_fn(device, c"vkCreateDescriptorSetLayout")?,
            destroy_set_layout: device_fn(device, c"vkDestroyDescriptorSetLayout")?,
            create_pipeline_layout: device_fn(device, c"vkCreatePipelineLayout")?,
            destroy_pipeline_layout: device_fn(device, c"vkDestroyPipelineLayout")?,
            create_shader_module: device_fn(device, c"vkCreateShaderModule")?,
            destroy_shader_module: device_fn(device, c"vkDestroyShaderModule")?,
            create_compute_pipelines: device_fn(device, c"vkCreateComputePipelines")?,
            destroy_pipeline: device_fn(device, c"vkDestroyPipeline")?,
            create_descriptor_pool: device_fn(device, c"vkCreateDescriptorPool")?,
            destroy_descriptor_pool: device_fn(device, c"vkDestroyDescriptorPool")?,
            allocate_descriptor_sets: device_fn(device, c"vkAllocateDescriptorSets")?,
            update_descriptor_sets: device_fn(device, c"vkUpdateDescriptorSets")?,
            cmd_bind_pipeline: device_fn(device, c"vkCmdBindPipeline")?,
            cmd_bind_descriptor_sets: device_fn(device, c"vkCmdBindDescriptorSets")?,
            cmd_push_constants: device_fn(device, c"vkCmdPushConstants")?,
            cmd_dispatch: device_fn(device, c"vkCmdDispatch")?,
        })
    }

    fn dev(&self) -> Dev {
        self.device as Dev
    }
}

// ------------------------------------------------------------- our objects

/// Everything one depth capture owns on the device. Zero is "not made".
struct Prep {
    fns: Fns,
    buffer: u64,
    memory: u64,
    mapped: usize,
    size: u64,
    cached: bool,
    coherent: bool,
    family: u32,
    pool: u64,
    cmd: usize,
    fence: u64,
    sampler: u64,
    set_layout: u64,
    pipeline_layout: u64,
    /// Multisampled, then single-sample.
    shaders: [u64; 2],
    pipelines: [u64; 2],
    descriptor_pool: u64,
    set: u64,
    /// The view of the engine's depth image, made on the render thread.
    view: u64,
}

impl Prep {
    fn destroy(self) {
        let f = &self.fns;
        let (d, none) = (f.dev(), std::ptr::null());
        let destroy = |what: DestroyFn, h: u64| {
            if h != 0 {
                what(d, h, none);
            }
        };
        destroy(f.destroy_image_view, self.view);
        destroy(f.destroy_descriptor_pool, self.descriptor_pool);
        for p in self.pipelines {
            destroy(f.destroy_pipeline, p);
        }
        for s in self.shaders {
            destroy(f.destroy_shader_module, s);
        }
        destroy(f.destroy_pipeline_layout, self.pipeline_layout);
        destroy(f.destroy_set_layout, self.set_layout);
        destroy(f.destroy_sampler, self.sampler);
        destroy(f.destroy_fence, self.fence);
        // Frees the command buffer with it.
        destroy(f.destroy_command_pool, self.pool);
        if self.mapped != 0 {
            (f.unmap_memory)(d, self.memory);
        }
        destroy(f.destroy_buffer, self.buffer);
        destroy(f.free_memory, self.memory);
    }
}

/// Make everything but the view, on the devctl thread.
fn prepare(device: usize, physical: usize, family: u32, bytes: u64) -> Result<Prep, String> {
    let fns = Fns::resolve(device as Dev)?;
    let props = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties");
    if props.is_null() || physical == 0 {
        return Err("vkGetPhysicalDeviceMemoryProperties is unavailable".into());
    }
    // SAFETY: resolved by the host loader for exactly this name.
    let props: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties) = unsafe { std::mem::transmute(props) };
    let mut mp = PhysicalDeviceMemoryProperties {
        memory_type_count: 0,
        memory_types: [[0; 2]; 32],
        memory_heap_count: 0,
        memory_heaps: [[0; 2]; 16],
    };
    props(physical as *mut c_void, &mut mp);
    let types: Vec<u32> = mp.memory_types[..(mp.memory_type_count as usize).min(32)].iter().map(|t| t[0]).collect();
    let mut p = Prep {
        fns,
        buffer: 0,
        memory: 0,
        mapped: 0,
        size: bytes,
        cached: false,
        coherent: false,
        family,
        pool: 0,
        cmd: 0,
        fence: 0,
        sampler: 0,
        set_layout: 0,
        pipeline_layout: 0,
        shaders: [0; 2],
        pipelines: [0; 2],
        descriptor_pool: 0,
        set: 0,
        view: 0,
    };
    match build(&mut p, &types) {
        Ok(()) => Ok(p),
        Err(e) => {
            p.destroy();
            Err(e)
        }
    }
}

fn build(p: &mut Prep, types: &[u32]) -> Result<(), String> {
    let f = &p.fns;
    let d = f.dev();
    let none = std::ptr::null();
    let check = |rc: i32, what: &str| if rc == VK_SUCCESS { Ok(()) } else { Err(format!("{what} failed ({rc})")) };

    let info = BufferCreateInfo {
        s_type: ST_BUFFER_CREATE_INFO,
        p_next: none,
        flags: 0,
        size: p.size,
        usage: BUFFER_USAGE_STORAGE,
        sharing_mode: 0,
        queue_family_index_count: 0,
        p_queue_family_indices: none.cast(),
    };
    check((f.create_buffer)(d, &info, none, &mut p.buffer), "vkCreateBuffer")?;
    let mut req = MemoryRequirements::default();
    (f.buffer_requirements)(d, p.buffer, &mut req);
    let choice = frame_capture::readback_memory_type(types, req.memory_type_bits)?;
    (p.cached, p.coherent) = (choice.cached, choice.coherent);
    let alloc = MemoryAllocateInfo {
        s_type: ST_MEMORY_ALLOCATE_INFO,
        p_next: none,
        allocation_size: req.size,
        memory_type_index: choice.index,
    };
    check((f.allocate_memory)(d, &alloc, none, &mut p.memory), "vkAllocateMemory")?;
    check((f.bind_buffer_memory)(d, p.buffer, p.memory, 0), "vkBindBufferMemory")?;
    let mut ptr = std::ptr::null_mut();
    check((f.map_memory)(d, p.memory, 0, WHOLE_SIZE, 0, &mut ptr), "vkMapMemory")?;
    p.mapped = ptr as usize;

    let pool = CommandPoolCreateInfo {
        s_type: ST_COMMAND_POOL_CREATE_INFO,
        p_next: none,
        flags: COMMAND_POOL_TRANSIENT,
        queue_family_index: p.family,
    };
    check((f.create_command_pool)(d, &pool, none, &mut p.pool), "vkCreateCommandPool")?;
    let alloc = CommandBufferAllocateInfo {
        s_type: ST_COMMAND_BUFFER_ALLOCATE_INFO,
        p_next: none,
        command_pool: p.pool,
        level: 0,
        command_buffer_count: 1,
    };
    let mut cmd = std::ptr::null_mut();
    check((f.allocate_command_buffers)(d, &alloc, &mut cmd), "vkAllocateCommandBuffers")?;
    p.cmd = cmd as usize;
    let fence = PlainCreateInfo { s_type: ST_FENCE_CREATE_INFO, p_next: none, flags: 0 };
    check((f.create_fence)(d, &fence, none, &mut p.fence), "vkCreateFence")?;

    let sampler = SamplerCreateInfo {
        s_type: ST_SAMPLER_CREATE_INFO,
        p_next: none,
        flags: 0,
        mag_filter: 0,
        min_filter: 0,
        mipmap_mode: 0,
        // CLAMP_TO_EDGE; a fetch never wraps anyway.
        address: [2; 3],
        mip_lod_bias: 0.0,
        anisotropy_enable: 0,
        max_anisotropy: 1.0,
        compare_enable: 0,
        compare_op: 0,
        min_lod: 0.0,
        max_lod: 0.0,
        border_color: 0,
        unnormalized: 0,
    };
    check((f.create_sampler)(d, &sampler, none, &mut p.sampler), "vkCreateSampler")?;
    let bindings = [
        DescriptorSetLayoutBinding {
            binding: 0,
            kind: DESCRIPTOR_COMBINED_IMAGE_SAMPLER,
            count: 1,
            stages: SHADER_STAGE_COMPUTE,
            immutable_samplers: none.cast(),
        },
        DescriptorSetLayoutBinding {
            binding: 1,
            kind: DESCRIPTOR_STORAGE_BUFFER,
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
    check((f.create_set_layout)(d, &layout, none, &mut p.set_layout), "vkCreateDescriptorSetLayout")?;
    let push = PushConstantRange { stages: SHADER_STAGE_COMPUTE, offset: 0, size: 8 };
    let pipeline_layout = PipelineLayoutCreateInfo {
        s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        set_layout_count: 1,
        set_layouts: &p.set_layout,
        push_constant_count: 1,
        push_constants: &push,
    };
    check((f.create_pipeline_layout)(d, &pipeline_layout, none, &mut p.pipeline_layout), "vkCreatePipelineLayout")?;
    for (i, code) in [SPIRV_MS, SPIRV_1X].into_iter().enumerate() {
        let module = ShaderModuleCreateInfo {
            s_type: ST_SHADER_MODULE_CREATE_INFO,
            p_next: none,
            flags: 0,
            code_size: std::mem::size_of_val(code),
            code: code.as_ptr(),
        };
        check((f.create_shader_module)(d, &module, none, &mut p.shaders[i]), "vkCreateShaderModule")?;
        let pipeline = ComputePipelineCreateInfo {
            s_type: ST_COMPUTE_PIPELINE_CREATE_INFO,
            p_next: none,
            flags: 0,
            stage: PipelineShaderStageCreateInfo {
                s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                p_next: none,
                flags: 0,
                stage: SHADER_STAGE_COMPUTE,
                module: p.shaders[i],
                name: c"main".as_ptr(),
                specialization: none,
            },
            layout: p.pipeline_layout,
            base_pipeline: 0,
            base_pipeline_index: -1,
        };
        check((f.create_compute_pipelines)(d, 0, 1, &pipeline, none, &mut p.pipelines[i]), "vkCreateComputePipelines")?;
    }
    let sizes = [
        DescriptorPoolSize { kind: DESCRIPTOR_COMBINED_IMAGE_SAMPLER, count: 1 },
        DescriptorPoolSize { kind: DESCRIPTOR_STORAGE_BUFFER, count: 1 },
    ];
    let pool = DescriptorPoolCreateInfo {
        s_type: ST_DESCRIPTOR_POOL_CREATE_INFO,
        p_next: none,
        flags: 0,
        max_sets: 1,
        count: 2,
        sizes: sizes.as_ptr(),
    };
    check((f.create_descriptor_pool)(d, &pool, none, &mut p.descriptor_pool), "vkCreateDescriptorPool")?;
    let alloc = DescriptorSetAllocateInfo {
        s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
        p_next: none,
        pool: p.descriptor_pool,
        count: 1,
        layouts: &p.set_layout,
    };
    check((f.allocate_descriptor_sets)(d, &alloc, &mut p.set), "vkAllocateDescriptorSets")
}

/// On the render thread, inside the engine's submit on `queue`: read the
/// scene depth into the buffer. Nothing is submitted when this fails.
fn capture(p: &mut Prep, queue: usize, scene: &Scene) -> Result<(), String> {
    let depth = &scene.depth;
    let f = &p.fns;
    let (d, none) = (f.dev(), std::ptr::null());
    if depth.device != f.device {
        return Err("the scene depth belongs to another device than the one the capture was prepared on".into());
    }
    if let Some(family) = frame_capture::queue_family(queue) {
        if family != p.family {
            return Err(format!("the frame went to queue family {family}, the capture was prepared for {}", p.family));
        }
    }
    let [w, h] = depth.extent;
    if w as u64 * h as u64 * 4 > p.size {
        return Err(format!("the scene depth is {w}x{h}, larger than the buffer prepared"));
    }
    if depth.usage & registry::USAGE_SAMPLED == 0 {
        return Err(format!("the scene depth image was not created for sampling (usage 0x{:x}), so it cannot be read", depth.usage));
    }
    let range = |aspect| ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    let view = ImageViewCreateInfo {
        s_type: ST_IMAGE_VIEW_CREATE_INFO,
        p_next: none,
        flags: 0,
        image: depth.image,
        view_type: VIEW_TYPE_2D,
        format: depth.format,
        components: [0; 4],
        range: range(ASPECT_DEPTH),
    };
    let rc = (f.create_image_view)(d, &view, none, &mut p.view);
    if rc != VK_SUCCESS {
        return Err(format!("vkCreateImageView of the scene depth failed ({rc})"));
    }
    let left = scene.layout as i32;
    let read = if sampleable(scene.layout) { left } else { LAYOUT_SHADER_READ_ONLY };
    let image_info = DescriptorImageInfo { sampler: p.sampler, view: p.view, layout: read };
    let buffer_info = DescriptorBufferInfo { buffer: p.buffer, offset: 0, range: WHOLE_SIZE };
    let write = |binding, kind, image: *const DescriptorImageInfo, buffer: *const DescriptorBufferInfo| WriteDescriptorSet {
        s_type: ST_WRITE_DESCRIPTOR_SET,
        p_next: none,
        set: p.set,
        binding,
        element: 0,
        count: 1,
        kind,
        image,
        buffer,
        texel_views: none.cast(),
    };
    let writes = [
        write(0, DESCRIPTOR_COMBINED_IMAGE_SAMPLER, &image_info, none.cast()),
        write(1, DESCRIPTOR_STORAGE_BUFFER, none.cast(), &buffer_info),
    ];
    (f.update_descriptor_sets)(d, 2, writes.as_ptr(), 0, none);

    let cmd = p.cmd as *mut c_void;
    let begin = CommandBufferBeginInfo {
        s_type: ST_COMMAND_BUFFER_BEGIN_INFO,
        p_next: none,
        flags: COMMAND_BUFFER_ONE_TIME_SUBMIT,
        p_inheritance_info: none,
    };
    let rc = (f.begin_command_buffer)(cmd, &begin);
    if rc != VK_SUCCESS {
        return Err(format!("vkBeginCommandBuffer failed ({rc})"));
    }
    // Before: everything submitted earlier on this queue (the engine's
    // frame) is done writing the depth. The first scope of a barrier reaches
    // back through submission order, which is what orders this after it.
    let to_read = ImageMemoryBarrier {
        s_type: ST_IMAGE_MEMORY_BARRIER,
        p_next: none,
        src_access_mask: ACCESS_MEMORY_WRITE,
        dst_access_mask: ACCESS_SHADER_READ,
        old_layout: left,
        new_layout: read,
        src_queue_family_index: QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: QUEUE_FAMILY_IGNORED,
        image: depth.image,
        subresource_range: range(barrier_aspects(depth.format)),
    };
    (f.cmd_pipeline_barrier)(cmd, STAGE_ALL_COMMANDS, STAGE_COMPUTE_SHADER, 0, 0, none.cast(), 0, none, 1, &to_read);
    let pipeline = if depth.samples > 1 { p.pipelines[0] } else { p.pipelines[1] };
    (f.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, pipeline);
    (f.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, p.pipeline_layout, 0, 1, &p.set, 0, none.cast());
    let size = [w, h];
    (f.cmd_push_constants)(cmd, p.pipeline_layout, SHADER_STAGE_COMPUTE, 0, 8, size.as_ptr().cast());
    (f.cmd_dispatch)(cmd, w.div_ceil(8), h.div_ceil(8), 1);
    // After: the buffer is visible to the host, the depth is back in the
    // engine's layout, and nothing submitted later (the engine's next frame
    // clearing this depth) starts before the read is done.
    let to_host = MemoryBarrier {
        s_type: ST_MEMORY_BARRIER,
        p_next: none,
        src_access_mask: ACCESS_SHADER_WRITE,
        dst_access_mask: ACCESS_HOST_READ,
    };
    let back = ImageMemoryBarrier {
        src_access_mask: 0,
        dst_access_mask: 0,
        old_layout: read,
        new_layout: left,
        subresource_range: range(barrier_aspects(depth.format)),
        ..to_read
    };
    let images = if read != left { 1 } else { 0 };
    (f.cmd_pipeline_barrier)(cmd, STAGE_COMPUTE_SHADER, STAGE_HOST | STAGE_ALL_COMMANDS, 0, 1, &to_host, 0, none, images, &back);
    let rc = (f.end_command_buffer)(cmd);
    if rc != VK_SUCCESS {
        return Err(format!("vkEndCommandBuffer failed ({rc})"));
    }
    let cmds = [cmd];
    let submit = SubmitInfo {
        s_type: ST_SUBMIT_INFO,
        p_next: none,
        wait_semaphore_count: 0,
        p_wait_semaphores: none.cast(),
        p_wait_dst_stage_mask: none.cast(),
        command_buffer_count: 1,
        p_command_buffers: cmds.as_ptr(),
        signal_semaphore_count: 0,
        p_signal_semaphores: none.cast(),
    };
    let rc = (f.queue_submit)(queue as *mut c_void, 1, &submit, p.fence);
    if rc != VK_SUCCESS {
        return Err(format!("vkQueueSubmit failed ({rc})"));
    }
    Ok(())
}

// ------------------------------------------------------------- the request

/// What the devctl thread waiting on a request hears.
enum Outcome {
    Submitted { prep: Prep, scene: Scene, frame: u64, render_thread: Duration },
    /// The objects come back unless the thread destroying the device has
    /// already destroyed them.
    Failed { prep: Option<Prep>, why: String },
}

struct Pending {
    prep: Prep,
    gate: Arc<AtomicU8>,
    reply: Sender<Outcome>,
    /// Set at the first present after arming: one whole frame is observed.
    matcher: Option<Matcher>,
    /// The present that frame came after.
    after: u64,
}

static STATE: Mutex<Option<Pending>> = Mutex::new(None);

/// How many [`Prep`]s exist outside [`LEFTOVERS`], so device destruction can
/// wait for the devctl threads that own them.
static IN_FLIGHT: Mutex<usize> = Mutex::new(0);
static IN_FLIGHT_CHANGED: Condvar = Condvar::new();

/// Objects whose capture nobody is waiting for any more, destroyed with the
/// device.
static LEFTOVERS: Mutex<Vec<Prep>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn in_flight(delta: isize) {
    let mut n = lock(&IN_FLIGHT);
    *n = n.saturating_add_signed(delta);
    IN_FLIGHT_CHANGED.notify_all();
}

/// Destroy a capture's objects on the thread that owns them.
fn retire(prep: Prep) {
    prep.destroy();
    in_flight(-1);
}

/// Keep a capture's objects for device destruction: nobody waits for them
/// any more, and the GPU may still use them.
fn set_aside(prep: Prep) {
    lock(&LEFTOVERS).push(prep);
    in_flight(-1);
}

/// End the pending request with `why`, from a thread that may not wait.
fn fail(p: Pending, why: String) {
    WATCH.fetch_and(!WATCH_DEPTH, Relaxed);
    p.gate.store(GATE_CANCELLED, Relaxed);
    if let Err(mpsc::SendError(Outcome::Failed { prep: Some(prep), .. })) = p.reply.send(Outcome::Failed { prep: Some(p.prep), why }) {
        set_aside(prep);
    }
}

/// From `vkQueuePresentKHR` while a request is armed.
pub(crate) fn frame_end(presents: u64) {
    let mut state = lock(&STATE);
    let Some(p) = state.as_mut() else { return };
    match &p.matcher {
        None => {
            p.matcher = Some(Matcher::new(passes::registry().swapchain_extents()));
            p.after = presents;
        }
        Some(m) => {
            let why = format!("the frame after present {} does not have the expected shape: {}", p.after, m.why_not());
            let p = state.take().expect("checked above");
            fail(p, why);
        }
    }
}

pub(crate) fn pass_begun(pass: &Pass) {
    let mut state = lock(&STATE);
    if let Some(m) = state.as_mut().and_then(|p| p.matcher.as_mut()) {
        let shape = shape_of(pass, &passes::registry());
        m.pass(&shape);
    }
}

pub(crate) fn barrier(image: u64, layout: u32) {
    if let Some(m) = lock(&STATE).as_mut().and_then(|p| p.matcher.as_mut()) {
        m.barrier(image, layout);
    }
}

pub(crate) fn written(image: u64) {
    if let Some(m) = lock(&STATE).as_mut().and_then(|p| p.matcher.as_mut()) {
        m.written(image);
    }
}

/// After the engine's submit of `cmds` on `queue` was forwarded and
/// succeeded; still inside it.
pub(crate) fn submitted(queue: *mut c_void, cmds: &[usize]) {
    let mut state = lock(&STATE);
    let Some(scene) = state.as_mut().and_then(|p| p.matcher.as_mut()).and_then(|m| m.submit(cmds)) else { return };
    let started = Instant::now();
    let Pending { mut prep, gate, reply, after, .. } = state.take().expect("checked above");
    WATCH.fetch_and(!WATCH_DEPTH, Relaxed);
    match capture(&mut prep, queue as usize, &scene) {
        Ok(()) => {
            gate.store(GATE_OPEN, Relaxed);
            let render_thread = started.elapsed();
            if let Err(mpsc::SendError(Outcome::Submitted { prep, .. })) =
                reply.send(Outcome::Submitted { prep, scene, frame: after, render_thread })
            {
                // Nobody waits; the GPU may still be writing the buffer.
                set_aside(prep);
            }
        }
        Err(why) => {
            gate.store(GATE_CANCELLED, Relaxed);
            if let Err(mpsc::SendError(Outcome::Failed { prep: Some(prep), .. })) =
                reply.send(Outcome::Failed { prep: Some(prep), why })
            {
                set_aside(prep);
            }
        }
    }
}

/// Before `vkDestroyDevice` is forwarded.
pub(crate) fn device_destroying(device: *mut c_void) {
    let device = device as usize;
    let pending = {
        let mut state = lock(&STATE);
        if state.as_ref().is_some_and(|p| p.prep.fns.device == device) {
            state.take()
        } else {
            None
        }
    };
    if let Some(p) = pending {
        // Nothing was submitted with them; destroyed here, before the
        // device, rather than handed to a thread that could be too late.
        WATCH.fetch_and(!WATCH_DEPTH, Relaxed);
        p.gate.store(GATE_CANCELLED, Relaxed);
        retire(p.prep);
        let _ = p.reply.send(Outcome::Failed { prep: None, why: "the Vulkan device was destroyed before the capture was taken".into() });
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut n = lock(&IN_FLIGHT);
    while *n > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        n = IN_FLIGHT_CHANGED.wait_timeout(n, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    drop(n);
    let all = std::mem::take(&mut *lock(&LEFTOVERS));
    for p in all {
        if p.fns.device == device {
            let _ = (p.fns.wait_for_fences)(p.fns.dev(), 1, &p.fence, 1, 1_000_000_000);
            p.destroy();
        } else {
            lock(&LEFTOVERS).push(p);
        }
    }
}

const FRAME_WAIT: Duration = Duration::from_secs(3);
const FENCE_WAIT_NS: u64 = 2_000_000_000;

/// The paths written next to `path`: the colour screenshot and the floats.
fn companions(path: &str) -> (String, String) {
    let stem = path.strip_suffix(".png").unwrap_or(path);
    (format!("{stem}-color.png"), format!("{stem}.f32"))
}

/// devctl `depthshot <path>`. `Ok` carries the paths and the depth facts.
pub fn request(path: &str) -> Result<String, String> {
    let r = run_request(path);
    match &r {
        Ok(line) => println!("[android] vulkan: depthshot {line}"),
        Err(e) => println!("[android] vulkan: depthshot failed: {e}"),
    }
    r
}

fn run_request(path: &str) -> Result<String, String> {
    if !passes::wanted() {
        return Err("the pass hooks are not handed out in this run".into());
    }
    let (device, physical) = frame_capture::current_device().ok_or("no Vulkan device yet")?;
    let family = frame_capture::likely_queue_family().unwrap_or(0);
    let biggest = passes::registry().swapchain_extents().iter().map(|e| e[0] as u64 * e[1] as u64).max();
    let biggest = biggest.ok_or("no swapchain has been created yet")?;
    let prep = prepare(device, physical, family, biggest * 4)?;
    in_flight(1);
    let (color_path, float_path) = companions(path);

    // The colour screenshot is armed first and waits, gated, for the frame
    // the depth is taken from.
    let gate = Arc::new(AtomicU8::new(GATE_WAIT));
    let (armed_tx, armed_rx) = mpsc::channel::<()>();
    let shot = {
        let (gate, color_path) = (gate.clone(), color_path.clone());
        std::thread::Builder::new()
            .name("intoxicated-depthshot".into())
            .spawn(move || frame_capture::request_gated(&color_path, Some(gate), move || {
                let _ = armed_tx.send(());
            }))
            .map_err(|e| format!("could not start the screenshot thread: {e}"))?
    };
    if armed_rx.recv_timeout(FRAME_WAIT).is_err() {
        retire(prep);
        gate.store(GATE_CANCELLED, Relaxed);
        let why = shot.join().unwrap_or_else(|_| Err("the screenshot thread panicked".into())).err();
        return Err(format!("the colour screenshot could not be armed: {}", why.unwrap_or_default()));
    }

    let (tx, rx) = mpsc::channel();
    *lock(&STATE) = Some(Pending { prep, gate: gate.clone(), reply: tx, matcher: None, after: 0 });
    WATCH.fetch_or(WATCH_DEPTH, Relaxed);
    let outcome = match rx.recv_timeout(FRAME_WAIT) {
        Ok(o) => o,
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
            let withdrawn = lock(&STATE).take();
            match withdrawn {
                Some(p) => {
                    WATCH.fetch_and(!WATCH_DEPTH, Relaxed);
                    gate.store(GATE_CANCELLED, Relaxed);
                    retire(p.prep);
                    let _ = shot.join();
                    return Err("no frame was presented within three seconds".into());
                }
                // Taken meanwhile: its outcome is on its way.
                None => rx.recv_timeout(FRAME_WAIT).map_err(|_| "the capture was taken but never answered".to_string())?,
            }
        }
    };
    let (prep, scene, frame, render_thread) = match outcome {
        Outcome::Failed { prep, why } => {
            if let Some(prep) = prep {
                retire(prep);
            }
            let _ = shot.join();
            return Err(why);
        }
        Outcome::Submitted { prep, scene, frame, render_thread } => (prep, scene, frame, render_thread),
    };
    let read = read_back(&prep, &scene);
    match read {
        // Done with the GPU either way except on a fence that never came.
        Err(_) if (prep.fns.wait_for_fences)(prep.fns.dev(), 1, &prep.fence, 1, 0) != VK_SUCCESS => set_aside(prep),
        _ => retire(prep),
    }
    let values = read?;
    let color = shot.join().unwrap_or_else(|_| Err("the screenshot thread panicked".into()));

    let [w, h] = scene.depth.extent;
    let st = stats(&values, w, scene.clear);
    let png = frame_capture::encode_gray_png(&gray(&values, &st), w, h)?;
    std::fs::write(path, &png).map_err(|e| format!("writing {path}: {e}"))?;
    let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(&float_path, &raw).map_err(|e| format!("writing {float_path}: {e}"))?;
    let center = values[(h as usize / 2) * w as usize + w as usize / 2];
    Ok(format!(
        "{path} {w}x{h} {} x{} (sample 0), frame after present {frame}, {} scene pass(es), layout {}; colour {}; \
         floats {float_path}; clear {} ({}); background {} px; drawn {} px min {:e} p1 {:e} p50 {:e} p99 {:e} max {:e}; \
         nearest at {},{}; center {:e}; render thread {:.3} ms",
        registry::format_name(scene.depth.format),
        scene.depth.samples,
        scene.passes,
        registry::layout_name(scene.layout),
        match &color {
            Ok(c) => c.clone(),
            Err(e) => format!("not taken: {e}"),
        },
        scene.clear.map_or("none".to_string(), |c| format!("{c}")),
        if st.reversed { "reversed-Z: far is 0" } else { "far is 1" },
        st.background,
        st.drawn,
        st.min,
        st.p01,
        st.p50,
        st.p99,
        st.max,
        st.nearest.0,
        st.nearest.1,
        center,
        render_thread.as_secs_f64() * 1e3,
    ))
}

/// Wait for our submit and copy the floats out.
fn read_back(p: &Prep, scene: &Scene) -> Result<Vec<f32>, String> {
    let f = &p.fns;
    let rc = (f.wait_for_fences)(f.dev(), 1, &p.fence, 1, FENCE_WAIT_NS);
    if rc != VK_SUCCESS {
        return Err(if rc == VK_TIMEOUT {
            "the depth read did not finish within two seconds".into()
        } else {
            format!("vkWaitForFences failed ({rc})")
        });
    }
    if !p.coherent {
        let range = MappedMemoryRange {
            s_type: ST_MAPPED_MEMORY_RANGE,
            p_next: std::ptr::null(),
            memory: p.memory,
            offset: 0,
            size: WHOLE_SIZE,
        };
        let rc = (f.invalidate_ranges)(f.dev(), 1, &range);
        if rc != VK_SUCCESS {
            return Err(format!("vkInvalidateMappedMemoryRanges failed ({rc})"));
        }
    }
    let n = scene.depth.extent[0] as usize * scene.depth.extent[1] as usize;
    // SAFETY: `mapped` maps `size` bytes, at least `4 n` (checked before the
    // submit); the fence and the host barrier make the shader's writes
    // visible. One sequential copy, which uncached memory needs.
    let bytes = unsafe { std::slice::from_raw_parts(p.mapped as *const u8, n * 4) }.to_vec();
    Ok(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

// ------------------------------------------------------------- the numbers

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stats {
    /// Larger is nearer: the depth was cleared to 0.
    pub reversed: bool,
    /// Pixels still holding the clear value (nothing drawn: the sky), or
    /// not a number.
    pub background: usize,
    pub drawn: usize,
    pub min: f32,
    pub p01: f32,
    pub p50: f32,
    pub p99: f32,
    pub max: f32,
    /// Where the nearest drawn pixel is.
    pub nearest: (u32, u32),
}

pub(crate) fn stats(values: &[f32], width: u32, clear: Option<f32>) -> Stats {
    let reversed = clear.is_some_and(|c| c < 0.5);
    let is_background = |v: f32| !v.is_finite() || clear == Some(v);
    let mut drawn: Vec<f32> = values.iter().copied().filter(|&v| !is_background(v)).collect();
    drawn.sort_unstable_by(f32::total_cmp);
    let at = |q: f64| drawn.get(((drawn.len().max(1) - 1) as f64 * q).round() as usize).copied().unwrap_or(f32::NAN);
    let nearest = values
        .iter()
        .enumerate()
        .filter(|(_, v)| !is_background(**v))
        .max_by(|a, b| if reversed { a.1.total_cmp(b.1) } else { b.1.total_cmp(a.1) })
        .map_or((0, 0), |(i, _)| ((i % width.max(1) as usize) as u32, (i / width.max(1) as usize) as u32));
    Stats {
        reversed,
        background: values.len() - drawn.len(),
        drawn: drawn.len(),
        min: at(0.0),
        p01: at(0.01),
        p50: at(0.5),
        p99: at(0.99),
        max: at(1.0),
        nearest,
    }
}

/// One byte per pixel: background black, drawn pixels from dim (far) to
/// white (near) on a log scale of nearness, which is what a perspective
/// depth needs to show more than the nearest few metres.
pub(crate) fn gray(values: &[f32], st: &Stats) -> Vec<u8> {
    let near = |v: f32| if st.reversed { v } else { 1.0 - v };
    let (lo, hi) = if st.reversed { (st.min, st.max) } else { (1.0 - st.max, 1.0 - st.min) };
    let (lo, hi) = (lo.max(f32::MIN_POSITIVE).ln(), hi.max(f32::MIN_POSITIVE).ln());
    let span = hi - lo;
    values
        .iter()
        .map(|&v| {
            let k = near(v);
            // The clear value (and anything else outside what was drawn) is
            // background.
            if st.drawn == 0 || !v.is_finite() || k <= 0.0 || v < st.min || v > st.max {
                return 0;
            }
            let t = if span > 0.0 { ((k.ln() - lo) / span).clamp(0.0, 1.0) } else { 1.0 };
            (24.0 + t * 231.0).round() as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: [u32; 2] = [1667, 1349];
    const SCENE_DEPTH: u64 = 0xd0;
    const SHADER_READ_ONLY: u32 = 5;

    fn depth(image: u64, extent: [u32; 2], samples: u32, clear: Option<f32>) -> Depth {
        Depth {
            image,
            device: 1,
            format: registry::FORMAT_D32_SFLOAT,
            samples,
            usage: 0x27,
            extent,
            layout_after: SHADER_READ_ONLY,
            clear,
        }
    }

    fn with_depth(cmd: usize, d: Depth) -> Shape {
        Shape { cmd, has_depth: true, depth: Some(d), to_swapchain: false }
    }

    fn color(cmd: usize, to_swapchain: bool) -> Shape {
        Shape { cmd, has_depth: false, depth: None, to_swapchain }
    }

    /// The frame traced on Natural Disaster Survival at quality 21, MSAA 4:
    /// a 512x112 pass, three 256x256 ones (one with a small D24S8 depth),
    /// the opaque scene, five post passes, the transparent scene on the same
    /// depth, seven bloom passes and the swapchain pass with the HUD.
    fn traced_frame() -> Vec<Shape> {
        let mut f = vec![color(1, false)];
        for _ in 0..3 {
            f.push(with_depth(1, Depth { format: 129, ..depth(0x24, [256, 256], 1, Some(1.0)) }));
        }
        f.push(with_depth(1, depth(SCENE_DEPTH, FULL, 4, Some(0.0))));
        for _ in 0..5 {
            f.push(color(1, false));
        }
        f.push(with_depth(1, depth(SCENE_DEPTH, FULL, 4, None)));
        for _ in 0..7 {
            f.push(color(1, false));
        }
        f.push(color(1, true));
        f
    }

    fn run(m: &mut Matcher, frame: &[Shape]) {
        for s in frame {
            m.pass(s);
        }
    }

    #[test]
    fn the_traced_frame_matches_at_its_last_scene_pass() {
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &traced_frame());
        let s = m.submit(&[7, 1]).expect("the submit carrying the HUD pass");
        assert_eq!((s.depth.image, s.depth.samples, s.passes, s.cmd, s.hud_cmd), (SCENE_DEPTH, 4, 2, 1, 1));
        assert_eq!(s.clear, Some(0.0), "the first scene pass's clear");
        assert_eq!(s.layout, SHADER_READ_ONLY);
        assert!(m.submit(&[1]).is_none(), "one capture per frame");
    }

    #[test]
    fn added_or_reordered_passes_do_not_move_the_match() {
        // Extra passes anywhere outside the scene, a pass with another small
        // depth after the scene, the HUD split over two command buffers and
        // swapchain passes before the scene: the same scene is found.
        let mut f = vec![color(1, true), color(1, false)];
        f.extend(traced_frame().into_iter().map(|s| Shape { cmd: 2, ..s }));
        let hud = f.pop().unwrap();
        f.push(with_depth(2, depth(0x99, [833, 674], 1, Some(0.0))));
        f.push(color(2, false));
        f.push(Shape { cmd: 3, ..hud });
        f.push(color(3, true));
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &f);
        assert!(m.submit(&[3]).is_none(), "the scene's command buffer is not submitted yet");
        let s = m.submit(&[2]);
        assert!(s.is_none(), "this submit does not carry the HUD pass");
        let s = m.submit(&[3]).expect("now both are submitted");
        assert_eq!((s.depth.image, s.cmd, s.hud_cmd), (SCENE_DEPTH, 2, 3));
    }

    #[test]
    fn frames_that_do_not_have_the_shape_say_why() {
        // A loading screen: only the swapchain.
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &[color(1, false), color(1, true)]);
        assert!(m.submit(&[1]).is_none());
        assert!(m.why_not().starts_with("no pass drew into a depth attachment the size of the swapchain (1667x1349)"));

        // The scene renders at a lower resolution than the swapchain.
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &[with_depth(1, depth(SCENE_DEPTH, [833, 674], 4, Some(0.0))), color(1, true)]);
        assert!(m.submit(&[1]).is_none());
        assert!(m.why_not().starts_with("no pass drew into a depth attachment the size"));

        // The HUD goes to an offscreen target, never to the swapchain.
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &[with_depth(1, depth(SCENE_DEPTH, FULL, 4, Some(0.0))), color(1, false)]);
        assert!(m.submit(&[1]).is_none());
        assert_eq!(m.why_not(), "no pass after the scene drew to the swapchain without a depth attachment");

        // The scene depth drawn to again after the HUD started.
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &[with_depth(1, depth(SCENE_DEPTH, FULL, 4, Some(0.0))), color(1, true), with_depth(1, depth(SCENE_DEPTH, FULL, 4, None))]);
        assert!(m.submit(&[1]).is_none());
        assert_eq!(m.why_not(), "the scene depth is drawn to again after the pass that draws to the swapchain");

        // Overwritten by a copy after the scene.
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &[with_depth(1, depth(SCENE_DEPTH, FULL, 4, Some(0.0)))]);
        m.written(SCENE_DEPTH);
        m.pass(&color(1, true));
        assert!(m.submit(&[1]).is_none());
        assert!(m.why_not().contains("overwritten"));

        // The HUD's command buffer is never submitted.
        let mut m = Matcher::new(vec![FULL]);
        run(&mut m, &[with_depth(1, depth(SCENE_DEPTH, FULL, 4, Some(0.0))), color(2, true)]);
        assert!(m.submit(&[1]).is_none());
        assert!(m.why_not().contains("not submitted"));

        // No swapchain known at all.
        let m = Matcher::new(vec![]);
        assert!(m.why_not().contains("none is known"));
    }

    #[test]
    fn a_pass_with_an_unknown_depth_is_neither_scene_nor_hud() {
        let mut m = Matcher::new(vec![FULL]);
        m.pass(&with_depth(1, depth(SCENE_DEPTH, FULL, 1, Some(0.0))));
        m.pass(&Shape { cmd: 1, has_depth: true, depth: None, to_swapchain: true });
        assert!(m.submit(&[1]).is_none());
        m.pass(&color(1, true));
        assert!(m.submit(&[1]).is_some(), "a single-sample full-size depth is a scene too");
    }

    #[test]
    fn barriers_on_the_scene_depth_move_its_layout() {
        let mut m = Matcher::new(vec![FULL]);
        m.barrier(SCENE_DEPTH, 3); // before the scene is known: ignored
        m.pass(&with_depth(1, depth(SCENE_DEPTH, FULL, 4, Some(0.0))));
        m.barrier(0x77, 7);
        m.barrier(SCENE_DEPTH, 4);
        m.pass(&color(1, true));
        assert_eq!(m.submit(&[1]).unwrap().layout, 4);
    }

    #[test]
    fn shapes_are_read_through_the_registry() {
        use registry::*;
        let mut reg = Registry::default();
        let att = |format, samples, load, final_layout| Attachment {
            format,
            samples,
            load,
            store: 0,
            stencil_load: 2,
            stencil_store: 1,
            initial_layout: 0,
            final_layout,
        };
        reg.render_passes.insert(
            0x50,
            RenderPass {
                device: 1,
                version: 1,
                attachments: vec![att(64, 4, 1, 2), att(126, 4, 1, 5)],
                subpasses: vec![Subpass {
                    color: vec![Ref { attachment: 0, layout: 2 }],
                    depth: Some(Ref { attachment: 1, layout: 3 }),
                    ..Subpass::default()
                }],
                dependencies: vec![],
            },
        );
        reg.render_passes.insert(
            0x51,
            RenderPass {
                device: 1,
                version: 1,
                attachments: vec![att(44, 1, 2, 2)],
                subpasses: vec![Subpass { color: vec![Ref { attachment: 0, layout: 2 }], ..Subpass::default() }],
                dependencies: vec![],
            },
        );
        reg.framebuffers.insert(0x60, Framebuffer { device: 1, render_pass: 0x50, width: 8, height: 8, layers: 1, views: vec![0x70, 0x71], imageless: false });
        reg.framebuffers.insert(0x61, Framebuffer { device: 1, render_pass: 0x51, width: 8, height: 8, layers: 1, views: vec![0x72], imageless: false });
        let img = |format, samples, usage| Image { device: 1, flags: 0, format, extent: [8, 8, 1], mips: 1, layers: 1, samples, usage, swapchain: false };
        reg.images.insert(0x80, img(64, 4, 0x13));
        reg.images.insert(0x81, img(126, 4, 0x27));
        reg.add_swapchain(1, 0x5, &[0x82], [8, 8], 44);
        let view = |image| View { device: 1, image, view_type: 1, format: 0, aspect: 0, base_mip: 0, levels: 1, base_layer: 0, layers: 1 };
        reg.views.insert(0x70, view(0x80));
        reg.views.insert(0x71, view(0x81));
        reg.views.insert(0x72, view(0x82));

        let scene = Pass::new(9, Target::RenderPass { render_pass: 0x50, framebuffer: 0x60, views: vec![], contents: 0, version: 1 }, (0, 0, 8, 8), vec![[0; 4], [0; 4]]);
        let s = shape_of(&scene, &reg);
        assert!(s.has_depth && !s.to_swapchain);
        assert_eq!(s.depth, Some(Depth { image: 0x81, device: 1, format: 126, samples: 4, usage: 0x27, extent: [8, 8], layout_after: 5, clear: Some(0.0) }));
        let hud = Pass::new(9, Target::RenderPass { render_pass: 0x51, framebuffer: 0x61, views: vec![], contents: 0, version: 1 }, (0, 0, 8, 8), vec![]);
        let s = shape_of(&hud, &reg);
        assert!(!s.has_depth && s.to_swapchain && s.depth.is_none());

        let mut m = Matcher::new(reg.swapchain_extents());
        m.pass(&shape_of(&scene, &reg));
        m.pass(&shape_of(&hud, &reg));
        assert_eq!(m.submit(&[9]).map(|s| s.depth.image), Some(0x81));

        // Dynamic rendering: the depth is the clear value after the colours.
        let dynamic = Pass::new(
            9,
            Target::Dynamic {
                flags: 0,
                layers: 1,
                colors: vec![passes::DynAttachment { view: 0x70, ..Default::default() }],
                depth: Some(passes::DynAttachment { view: 0x71, layout: 3, load: 1, ..Default::default() }),
                stencil: None,
            },
            (0, 0, 8, 8),
            vec![[0; 4], [1.0f32.to_bits(), 0, 0, 0]],
        );
        let s = shape_of(&dynamic, &reg);
        assert_eq!(s.depth.map(|d| (d.image, d.layout_after, d.clear)), Some((0x81, 3, Some(1.0))));
    }

    #[test]
    fn stats_and_gray_follow_the_clear_value() {
        // Reversed-Z: 0 is the cleared far plane, larger is nearer.
        let values = [0.0, 0.0, 0.001, 0.01, 0.1, 1.0];
        let st = stats(&values, 3, Some(0.0));
        assert!(st.reversed);
        assert_eq!((st.background, st.drawn, st.min, st.max, st.nearest), (2, 4, 0.001, 1.0, (2, 1)));
        let g = gray(&values, &st);
        assert_eq!(&g[..2], &[0, 0], "background is black");
        assert_eq!((g[2], g[5]), (24, 255), "farthest drawn is dim, nearest white");
        assert!(g[2] < g[3] && g[3] < g[4] && g[4] < g[5]);
        assert_eq!(g[3] - g[2], g[4] - g[3], "log steps are even");

        // The usual way round: 1 is far.
        let values = [1.0, 0.99, 0.5];
        let st = stats(&values, 3, Some(1.0));
        assert!(!st.reversed);
        assert_eq!((st.background, st.nearest), (1, (2, 0)));
        let g = gray(&values, &st);
        assert!(g[0] == 0 && g[1] < g[2] && g[2] == 255);

        // Nothing drawn, and a NaN: all black, no panic.
        let st = stats(&[0.0, f32::NAN], 2, Some(0.0));
        assert_eq!((st.drawn, st.background), (0, 2));
        assert_eq!(gray(&[0.0, f32::NAN], &st), vec![0, 0]);
    }

    #[test]
    fn companion_paths_sit_beside_the_depth() {
        assert_eq!(companions("/x/d.png"), ("/x/d-color.png".to_string(), "/x/d.f32".to_string()));
        assert_eq!(companions("/x/d"), ("/x/d-color.png".to_string(), "/x/d.f32".to_string()));
    }

    #[test]
    fn the_shaders_are_spirv_for_vulkan_one_point_oh() {
        for code in [SPIRV_MS, SPIRV_1X] {
            assert_eq!(code[0], 0x0723_0203, "magic");
            assert_eq!(code[1], 0x0001_0000, "SPIR-V 1.0");
            assert!(code.len() > 100);
        }
        assert_ne!(SPIRV_MS, SPIRV_1X);
    }

    #[test]
    fn the_structs_have_the_c_layout() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<ImageViewCreateInfo>(), 80);
        assert_eq!(offset_of!(ImageViewCreateInfo, range), 56);
        assert_eq!(size_of::<SamplerCreateInfo>(), 80);
        assert_eq!(offset_of!(SamplerCreateInfo, border_color), 72);
        assert_eq!(size_of::<DescriptorSetLayoutBinding>(), 24);
        assert_eq!(size_of::<DescriptorSetLayoutCreateInfo>(), 32);
        assert_eq!(size_of::<PushConstantRange>(), 12);
        assert_eq!(size_of::<PipelineLayoutCreateInfo>(), 48);
        assert_eq!(offset_of!(PipelineLayoutCreateInfo, push_constants), 40);
        assert_eq!(size_of::<ShaderModuleCreateInfo>(), 40);
        assert_eq!(size_of::<PipelineShaderStageCreateInfo>(), 48);
        assert_eq!(size_of::<ComputePipelineCreateInfo>(), 96);
        assert_eq!(offset_of!(ComputePipelineCreateInfo, stage), 24);
        assert_eq!(offset_of!(ComputePipelineCreateInfo, layout), 72);
        assert_eq!(size_of::<DescriptorPoolCreateInfo>(), 40);
        assert_eq!(offset_of!(DescriptorPoolCreateInfo, sizes), 32);
        assert_eq!(size_of::<DescriptorSetAllocateInfo>(), 40);
        assert_eq!(size_of::<DescriptorImageInfo>(), 24);
        assert_eq!(size_of::<DescriptorBufferInfo>(), 24);
        assert_eq!(size_of::<WriteDescriptorSet>(), 64);
        assert_eq!(offset_of!(WriteDescriptorSet, kind), 36);
        assert_eq!(offset_of!(WriteDescriptorSet, buffer), 48);
    }

    #[test]
    fn a_request_without_the_hooks_or_a_device_fails_at_once() {
        let started = Instant::now();
        assert!(request("/nonexistent/d.png").is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(WATCH.load(Relaxed) & WATCH_DEPTH, 0);
    }
}
