//! The devctl `screenshot`: the frame the engine presents, copied out of its
//! own swapchain inside `vkQueuePresentKHR` and written as a PNG.
//!
//! Written from a behavioural spec, the Vulkan specification and the PNG
//! specification.
//!
//! # Who does what
//!
//! Three threads take part, and the render thread does as little as Vulkan
//! allows:
//!
//! * The devctl thread ([`request`]) prepares everything a capture needs: a
//!   readback buffer sized for the largest live swapchain, its memory mapped
//!   once, a command pool and command buffer for the presenting queue's
//!   family, a fence and a binary semaphore. Then it arms the request and
//!   waits for the outcome.
//! * The engine's render thread ([`present`]), on the next present with a
//!   request armed, records a layout transition, `vkCmdCopyImageToBuffer` and
//!   the transition back, submits that on the presenting queue (waiting on the
//!   engine's present semaphores, signalling the capture's semaphore and
//!   fence), and forwards the present with its wait list replaced by the
//!   capture's semaphore. Nothing else: no allocation of Vulkan memory, no
//!   object created or destroyed, no wait, no pixel read, no file.
//! * The capture worker ([`finish`]) waits on the fence, encodes the PNG,
//!   writes the file and destroys what the devctl thread created.
//!
//! On a frame with no request armed the present pays one relaxed atomic load
//! ([`pending`]).
//!
//! # Why it is split
//!
//! The capture used to run whole inside the present, and the frame that
//! carried it took 840 ms. Measured outside the client on the same GPU, 790 ms
//! of that was the CPU reading uncached host memory a byte at a time while
//! converting pixels; the readback memory type is now chosen host-cached
//! first ([`readback_memory_type`]), and with an uncached type the mapping is
//! copied out in one sequential pass before any pixel is touched. The rest
//! (encoding, allocation, teardown) is real work too, which is why none of it
//! is left on the render thread.
//!
//! # Semaphores
//!
//! The copy waits on every semaphore the engine's present waited on, at the
//! transfer stage, and the forwarded present waits only on the capture's own
//! semaphore, which the copy signals. So the copy runs after the engine's
//! rendering and the presentation after the copy. A semaphore a present has
//! waited on cannot be reused or destroyed until that wait is known complete,
//! and nothing reports that without the swapchain maintenance extension, so
//! each capture's semaphore is retired and kept until the device is
//! destroyed: one small object per screenshot.
//!
//! # A screenshot of another capture's frame
//!
//! `depth_capture` wants the colour of the frame whose depth it reads.
//! [`request_gated`] arms a screenshot that presents step over until its gate
//! is opened (from inside the submit the depth is read after, so the next
//! present is that frame's), or fail at once if it is cancelled. The Vulkan
//! structures and helpers `depth_capture` shares are `pub(super)` here.

use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

// ------------------------------------------------------------- Vulkan ABI
//
// Values and layouts as `vulkan_core.h` defines them; every size and offset
// used here was printed from that header with the system compiler, and the
// sizes are pinned by a test below.

const VK_SUCCESS: i32 = 0;
const VK_TIMEOUT: i32 = 2;
const VK_INCOMPLETE: i32 = 5;

const ST_SUBMIT_INFO: i32 = 4;
const ST_MEMORY_ALLOCATE_INFO: i32 = 5;
const ST_MAPPED_MEMORY_RANGE: i32 = 6;
const ST_FENCE_CREATE_INFO: i32 = 8;
const ST_SEMAPHORE_CREATE_INFO: i32 = 9;
const ST_BUFFER_CREATE_INFO: i32 = 12;
const ST_COMMAND_POOL_CREATE_INFO: i32 = 39;
const ST_COMMAND_BUFFER_ALLOCATE_INFO: i32 = 40;
const ST_COMMAND_BUFFER_BEGIN_INFO: i32 = 42;
const ST_IMAGE_MEMORY_BARRIER: i32 = 45;
const ST_MEMORY_BARRIER: i32 = 46;

const LAYOUT_TRANSFER_SRC_OPTIMAL: i32 = 6;
const LAYOUT_PRESENT_SRC_KHR: i32 = 1_000_001_002;

const STAGE_TRANSFER: u32 = 0x1000;
const STAGE_BOTTOM_OF_PIPE: u32 = 0x2000;
const STAGE_HOST: u32 = 0x4000;
const STAGE_ALL_COMMANDS: u32 = 0x1_0000;

const ACCESS_COLOR_ATTACHMENT_WRITE: u32 = 0x100;
const ACCESS_TRANSFER_READ: u32 = 0x800;
const ACCESS_TRANSFER_WRITE: u32 = 0x1000;
const ACCESS_HOST_READ: u32 = 0x2000;
const ACCESS_MEMORY_WRITE: u32 = 0x1_0000;

const MEMORY_HOST_VISIBLE: u32 = 0x2;
const MEMORY_HOST_COHERENT: u32 = 0x4;
const MEMORY_HOST_CACHED: u32 = 0x8;

const BUFFER_USAGE_TRANSFER_DST: u32 = 0x2;
const ASPECT_COLOR: u32 = 0x1;
const QUEUE_FAMILY_IGNORED: u32 = !0;
const WHOLE_SIZE: u64 = !0;
const COMMAND_POOL_TRANSIENT: u32 = 0x1;
const COMMAND_BUFFER_ONE_TIME_SUBMIT: u32 = 0x1;

/// `VkPresentInfoKHR`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PresentInfo {
    s_type: i32,
    p_next: *const c_void,
    wait_semaphore_count: u32,
    p_wait_semaphores: *const u64,
    swapchain_count: u32,
    p_swapchains: *const u64,
    p_image_indices: *const u32,
    p_results: *mut i32,
}

#[repr(C)]
pub(super) struct SubmitInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) wait_semaphore_count: u32,
    pub(super) p_wait_semaphores: *const u64,
    pub(super) p_wait_dst_stage_mask: *const u32,
    pub(super) command_buffer_count: u32,
    pub(super) p_command_buffers: *const *mut c_void,
    pub(super) signal_semaphore_count: u32,
    pub(super) p_signal_semaphores: *const u64,
}

#[repr(C)]
pub(super) struct ImageSubresourceRange {
    pub(super) aspect_mask: u32,
    pub(super) base_mip_level: u32,
    pub(super) level_count: u32,
    pub(super) base_array_layer: u32,
    pub(super) layer_count: u32,
}

#[repr(C)]
pub(super) struct ImageMemoryBarrier {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) src_access_mask: u32,
    pub(super) dst_access_mask: u32,
    pub(super) old_layout: i32,
    pub(super) new_layout: i32,
    pub(super) src_queue_family_index: u32,
    pub(super) dst_queue_family_index: u32,
    pub(super) image: u64,
    pub(super) subresource_range: ImageSubresourceRange,
}

#[repr(C)]
pub(super) struct MemoryBarrier {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) src_access_mask: u32,
    pub(super) dst_access_mask: u32,
}

#[repr(C)]
struct ImageSubresourceLayers {
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
struct BufferImageCopy {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    image_subresource: ImageSubresourceLayers,
    image_offset: [i32; 3],
    image_extent: [u32; 3],
}

#[repr(C)]
pub(super) struct BufferCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) size: u64,
    pub(super) usage: u32,
    pub(super) sharing_mode: i32,
    pub(super) queue_family_index_count: u32,
    pub(super) p_queue_family_indices: *const u32,
}

#[repr(C)]
pub(super) struct MemoryAllocateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) allocation_size: u64,
    pub(super) memory_type_index: u32,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct MemoryRequirements {
    pub(super) size: u64,
    pub(super) alignment: u64,
    pub(super) memory_type_bits: u32,
}

#[repr(C)]
pub(super) struct MappedMemoryRange {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) memory: u64,
    pub(super) offset: u64,
    pub(super) size: u64,
}

#[repr(C)]
pub(super) struct CommandPoolCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) queue_family_index: u32,
}

#[repr(C)]
pub(super) struct CommandBufferAllocateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) command_pool: u64,
    pub(super) level: i32,
    pub(super) command_buffer_count: u32,
}

#[repr(C)]
pub(super) struct CommandBufferBeginInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
    pub(super) p_inheritance_info: *const c_void,
}

/// `VkFenceCreateInfo` and `VkSemaphoreCreateInfo`, which share a layout.
#[repr(C)]
pub(super) struct PlainCreateInfo {
    pub(super) s_type: i32,
    pub(super) p_next: *const c_void,
    pub(super) flags: u32,
}

/// `VkPhysicalDeviceMemoryProperties`: 32 (flags, heap) pairs, then the heaps.
#[repr(C)]
pub(super) struct PhysicalDeviceMemoryProperties {
    pub(super) memory_type_count: u32,
    pub(super) memory_types: [[u32; 2]; 32],
    pub(super) memory_heap_count: u32,
    pub(super) memory_heaps: [[u64; 2]; 16],
}

pub(super) type Dev = *mut c_void;

/// The device-level commands a capture uses, resolved once per device
/// through the host's `vkGetDeviceProcAddr`. Called directly, so a capture
/// never passes through the ETC2 hooks; none of its objects are ones they
/// track.
struct DeviceFns {
    device: usize,
    create_buffer: extern "C" fn(Dev, *const BufferCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_buffer: extern "C" fn(Dev, u64, *const c_void),
    buffer_requirements: extern "C" fn(Dev, u64, *mut MemoryRequirements),
    allocate_memory: extern "C" fn(Dev, *const MemoryAllocateInfo, *const c_void, *mut u64) -> i32,
    free_memory: extern "C" fn(Dev, u64, *const c_void),
    bind_buffer_memory: extern "C" fn(Dev, u64, u64, u64) -> i32,
    map_memory: extern "C" fn(Dev, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    unmap_memory: extern "C" fn(Dev, u64),
    invalidate_ranges: extern "C" fn(Dev, u32, *const MappedMemoryRange) -> i32,
    create_command_pool: extern "C" fn(Dev, *const CommandPoolCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_command_pool: extern "C" fn(Dev, u64, *const c_void),
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
    cmd_copy_image_to_buffer: extern "C" fn(*mut c_void, u64, i32, u64, u32, *const BufferImageCopy),
    queue_submit: extern "C" fn(*mut c_void, u32, *const SubmitInfo, u64) -> i32,
    create_fence: extern "C" fn(Dev, *const PlainCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_fence: extern "C" fn(Dev, u64, *const c_void),
    wait_for_fences: extern "C" fn(Dev, u32, *const u64, u32, u64) -> i32,
    create_semaphore: extern "C" fn(Dev, *const PlainCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_semaphore: extern "C" fn(Dev, u64, *const c_void),
}

/// One device-level command from the host, or the error naming it.
pub(super) fn device_fn<F: Copy>(device: Dev, name: &CStr) -> Result<F, String> {
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>());
    let p = super::vk_layer::host_device_proc(device, name);
    if p.is_null() {
        return Err(format!("driver has no {}", name.to_string_lossy()));
    }
    // SAFETY: `F` is the `extern "C"` signature the specification gives this
    // command, and `p` was resolved by the host loader for exactly this name.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
}

impl DeviceFns {
    fn resolve(device: Dev) -> Result<DeviceFns, String> {
        Ok(DeviceFns {
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
            cmd_copy_image_to_buffer: device_fn(device, c"vkCmdCopyImageToBuffer")?,
            queue_submit: device_fn(device, c"vkQueueSubmit")?,
            create_fence: device_fn(device, c"vkCreateFence")?,
            destroy_fence: device_fn(device, c"vkDestroyFence")?,
            wait_for_fences: device_fn(device, c"vkWaitForFences")?,
            create_semaphore: device_fn(device, c"vkCreateSemaphore")?,
            destroy_semaphore: device_fn(device, c"vkDestroySemaphore")?,
        })
    }

    fn dev(&self) -> Dev {
        self.device as Dev
    }
}

// ------------------------------------------------------------ bookkeeping

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The device the engine renders with, from `vkCreateDevice`.
struct DeviceCtx {
    device: usize,
    physical: usize,
    /// The family of the first `VkDeviceQueueCreateInfo`: the guess used
    /// until a queue lookup or a capture says which family presents.
    first_family: u32,
    /// Resolved on the first request, so a session without one pays nothing.
    resolved: Option<Resolved>,
}

struct Resolved {
    fns: Arc<DeviceFns>,
    /// `propertyFlags` by memory type index.
    memory_types: Vec<u32>,
}

static CTX: Mutex<Option<DeviceCtx>> = Mutex::new(None);

/// What one swapchain is, recorded when it is created.
struct Swapchain {
    handle: u64,
    device: usize,
    width: u32,
    height: u32,
    format: i32,
    images: Result<Vec<u64>, String>,
    seq: u64,
}

static SWAPCHAINS: Mutex<Vec<Swapchain>> = Mutex::new(Vec::new());
static SWAPCHAIN_SEQ: AtomicU64 = AtomicU64::new(0);

/// The extent of the most recently created swapchain, for `info`.
static LAST_EXTENT: AtomicU64 = AtomicU64::new(0);

/// `(device, queue, family)` for every queue the engine fetched.
static QUEUES: Mutex<Vec<(usize, usize, u32)>> = Mutex::new(Vec::new());

/// The family of the queue the last captured present was made on.
static PRESENT_FAMILY: AtomicU32 = AtomicU32::new(u32::MAX);

/// Everything one capture owns on the device.
struct Prepared {
    fns: Arc<DeviceFns>,
    buffer: u64,
    memory: u64,
    /// Host address of the mapping, 0 when not mapped.
    mapped: usize,
    size: u64,
    memory_type: u32,
    cached: bool,
    coherent: bool,
    family: u32,
    pool: u64,
    /// The `VkCommandBuffer`, a dispatchable handle.
    cmd: usize,
    fence: u64,
    semaphore: u64,
}

/// How many [`Prepared`] exist, so device destruction can wait for them.
static LIVE: Mutex<usize> = Mutex::new(0);
static LIVE_CHANGED: Condvar = Condvar::new();

fn live_add() {
    *lock(&LIVE) += 1;
}

fn live_sub() {
    let mut n = lock(&LIVE);
    *n = n.saturating_sub(1);
    LIVE_CHANGED.notify_all();
}

/// Objects that outlive their capture: retired semaphores (see the module
/// doc) and captures whose fence never signalled, which the GPU may still
/// write into. Destroyed with the device.
#[derive(Default)]
struct Leftovers {
    semaphores: Vec<(Arc<DeviceFns>, u64)>,
    unfinished: Vec<Prepared>,
}

static LEFTOVERS: Mutex<Leftovers> = Mutex::new(Leftovers { semaphores: Vec::new(), unfinished: Vec::new() });

impl Prepared {
    /// Destroy whatever exists, without touching the live count.
    fn destroy_parts(&self) {
        let f = &*self.fns;
        let d = f.dev();
        let none = std::ptr::null();
        if self.semaphore != 0 {
            (f.destroy_semaphore)(d, self.semaphore, none);
        }
        if self.fence != 0 {
            (f.destroy_fence)(d, self.fence, none);
        }
        if self.pool != 0 {
            // Frees the command buffer with it.
            (f.destroy_command_pool)(d, self.pool, none);
        }
        if self.mapped != 0 {
            (f.unmap_memory)(d, self.memory);
        }
        if self.buffer != 0 {
            (f.destroy_buffer)(d, self.buffer, none);
        }
        if self.memory != 0 {
            (f.free_memory)(d, self.memory, none);
        }
    }

    /// For a capture nothing was submitted with, or whose copy is complete
    /// and whose semaphore no present waited on.
    fn destroy(self) {
        self.destroy_parts();
        live_sub();
    }

    /// After a completed copy: the semaphore a present waited on is kept.
    fn destroy_keeping_semaphore(mut self) {
        let semaphore = std::mem::take(&mut self.semaphore);
        if semaphore != 0 {
            lock(&LEFTOVERS).semaphores.push((self.fns.clone(), semaphore));
        }
        self.destroy();
    }

    /// For a capture that may still be in use by the GPU, or that cannot be
    /// destroyed on the thread holding it. Recorded before the live count
    /// drops, so device destruction cannot miss it.
    fn set_aside(self) {
        lock(&LEFTOVERS).unfinished.push(self);
        live_sub();
    }
}

/// An armed request, waiting for a present.
struct Armed {
    id: u64,
    path: String,
    prep: Prepared,
    reply: Sender<Event>,
    /// Set by the devctl thread when it stops waiting, so a late result does
    /// not write a file after the harness was told the capture failed.
    given_up: Arc<AtomicBool>,
    /// For a screenshot that belongs to another capture's frame (the depth
    /// capture's): no present takes it while this reads [`GATE_WAIT`].
    gate: Option<Arc<AtomicU8>>,
}

/// A gated screenshot waits for its frame.
pub(super) const GATE_WAIT: u8 = 0;
/// The next present takes it.
pub(super) const GATE_OPEN: u8 = 1;
/// The next present hands it back as failed: its frame will not come.
pub(super) const GATE_CANCELLED: u8 = 2;

/// What the devctl thread waiting on a request hears.
enum Event {
    Done(Result<String, String>),
    /// The capture's objects, handed back unused, and why.
    Returned(Prepared, Why),
}

enum Why {
    Superseded,
    Failed(String),
    /// The present that took the request needs a capture prepared for
    /// another extent or queue family.
    Retry {
        reason: String,
        width: u32,
        height: u32,
        family: u32,
    },
}

static ARMED: AtomicBool = AtomicBool::new(false);
static SLOT: Mutex<Option<Armed>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// What the present that took a request is copying.
struct Target {
    swapchain: u64,
    image: u64,
    width: u32,
    height: u32,
    format: i32,
}

/// A submitted copy, on its way to the worker.
struct InFlight {
    armed: Armed,
    target: Target,
    render_thread: Duration,
}

/// The copies the GPU may still be doing, so a swapchain is not destroyed
/// under one.
struct Copying {
    id: u64,
    swapchain: u64,
    fence: u64,
    fns: Arc<DeviceFns>,
}

static COPYING: Mutex<Vec<Copying>> = Mutex::new(Vec::new());

static WORKER: OnceLock<Sender<InFlight>> = OnceLock::new();

const NO_FRAME: &str = "no frame was presented within three seconds (the engine may be wedged)";
const NO_SWAPCHAIN: &str = "no swapchain has been created yet";

/// How long a present has to take a request.
const PRESENT_WAIT: Duration = Duration::from_secs(3);
/// How long a taken capture has to finish. The work after the present
/// measured about 60 ms outside the client; the fence wait alone is bounded
/// at two seconds.
const OUTCOME_WAIT: Duration = Duration::from_secs(10);
const FENCE_WAIT_NS: u64 = 2_000_000_000;
const SWAPCHAIN_DESTROY_WAIT_NS: u64 = 1_000_000_000;
const DEVICE_DESTROY_WAIT: Duration = Duration::from_secs(3);
/// A present that needs a different preparation hands the request back; this
/// many times, then the request fails rather than chasing a moving target.
const MAX_RETRIES: u32 = 3;

// ----------------------------------------------- hooks from vk_layer.rs

/// `vkCreateDevice` succeeded.
pub(super) fn device_created(physical: *mut c_void, device: *mut c_void, first_family: Option<u32>) {
    if device.is_null() {
        return;
    }
    let mut ctx = lock(&CTX);
    if let Some(old) = ctx.as_ref() {
        println!(
            "[android] vulkan: a second device {device:p} while 0x{:x} is live; screenshots stay on the first",
            old.device
        );
        return;
    }
    *ctx = Some(DeviceCtx {
        device: device as usize,
        physical: physical as usize,
        first_family: first_family.unwrap_or(0),
        resolved: None,
    });
}

/// `vkGetDeviceQueue` or `vkGetDeviceQueue2` returned `queue`.
pub(super) fn queue_obtained(device: *mut c_void, family: u32, queue: *mut c_void) {
    if queue.is_null() {
        return;
    }
    let mut q = lock(&QUEUES);
    if !q.iter().any(|&(_, h, _)| h == queue as usize) {
        q.push((device as usize, queue as usize, family));
    }
}

/// `vkCreateSwapchainKHR` succeeded. Called on the creating thread, which is
/// where the image list is asked for.
pub(super) fn swapchain_created(device: *mut c_void, swapchain: u64, width: u32, height: u32, format: i32) {
    LAST_EXTENT.store(((width as u64) << 32) | height as u64, Relaxed);
    let images = swapchain_images(device, swapchain);
    if let Ok(list) = &images {
        super::vk_layer::passes::swapchain_images(device, swapchain, list, width, height, format);
    }
    let mut list = lock(&SWAPCHAINS);
    list.retain(|s| s.handle != swapchain);
    list.push(Swapchain {
        handle: swapchain,
        device: device as usize,
        width,
        height,
        format,
        images,
        seq: SWAPCHAIN_SEQ.fetch_add(1, Relaxed),
    });
}

fn swapchain_images(device: Dev, swapchain: u64) -> Result<Vec<u64>, String> {
    let get: extern "C" fn(Dev, u64, *mut u32, *mut u64) -> i32 = device_fn(device, c"vkGetSwapchainImagesKHR")?;
    loop {
        let mut n = 0u32;
        if get(device, swapchain, &mut n, std::ptr::null_mut()) != VK_SUCCESS {
            return Err("vkGetSwapchainImagesKHR failed".into());
        }
        let mut images = vec![0u64; n as usize];
        match get(device, swapchain, &mut n, images.as_mut_ptr()) {
            VK_SUCCESS => {
                images.truncate(n as usize);
                return Ok(images);
            }
            // The count changed between the two calls; ask again.
            VK_INCOMPLETE => continue,
            _ => return Err("vkGetSwapchainImagesKHR failed".into()),
        }
    }
}

/// Before `vkDestroySwapchainKHR` is forwarded: wait for any copy still
/// reading one of its images, then forget it.
pub(super) fn swapchain_destroying(device: *mut c_void, swapchain: u64) {
    if swapchain == 0 {
        return;
    }
    {
        // Held across the wait, so the worker cannot destroy the fence under
        // it; the worker takes this lock before it destroys anything.
        let copying = lock(&COPYING);
        for c in copying.iter().filter(|c| c.swapchain == swapchain && c.fns.device == device as usize) {
            let rc = (c.fns.wait_for_fences)(c.fns.dev(), 1, &c.fence, 1, SWAPCHAIN_DESTROY_WAIT_NS);
            if rc != VK_SUCCESS {
                println!(
                    "[android] vulkan: a screenshot's copy from a swapchain being destroyed had not finished \
                     after 1 s (vkWaitForFences returned {rc})"
                );
            }
        }
    }
    lock(&SWAPCHAINS).retain(|s| s.handle != swapchain);
    super::vk_layer::passes::swapchain_destroyed(swapchain);
}

/// Before `vkDestroyDevice` is forwarded: every object a capture owns goes
/// first, because the device's children must be gone before it is.
pub(super) fn device_destroying(device: *mut c_void) {
    let device = device as usize;
    let ours = {
        let mut ctx = lock(&CTX);
        if ctx.as_ref().is_some_and(|c| c.device == device) {
            ctx.take()
        } else {
            None
        }
    };
    if ours.is_some() {
        // No new capture can be prepared now. Withdraw the armed one.
        let armed = {
            let mut slot = lock(&SLOT);
            ARMED.store(false, Relaxed);
            slot.take()
        };
        if let Some(a) = armed {
            a.prep.destroy();
            let _ = a
                .reply
                .send(Event::Done(Err("the Vulkan device was destroyed before a present took the capture".into())));
        }
        // In-flight copies finish on the worker, and returned ones are torn
        // down by their devctl thread; both drop the live count.
        let deadline = Instant::now() + DEVICE_DESTROY_WAIT;
        let mut live = lock(&LIVE);
        while *live > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                println!(
                    "[android] vulkan: {} screenshot capture(s) still hold objects as the device is destroyed",
                    *live
                );
                break;
            }
            live = LIVE_CHANGED.wait_timeout(live, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
    let left = std::mem::take(&mut *lock(&LEFTOVERS));
    for p in left.unfinished {
        if p.fns.device == device {
            // The engine has to have let the device go idle before destroying
            // it, so this is a formality; bounded all the same.
            let _ = (p.fns.wait_for_fences)(p.fns.dev(), 1, &p.fence, 1, SWAPCHAIN_DESTROY_WAIT_NS);
            p.destroy_parts();
        } else {
            lock(&LEFTOVERS).unfinished.push(p);
        }
    }
    for (fns, semaphore) in left.semaphores {
        if fns.device == device {
            (fns.destroy_semaphore)(fns.dev(), semaphore, std::ptr::null());
        } else {
            lock(&LEFTOVERS).semaphores.push((fns, semaphore));
        }
    }
    lock(&SWAPCHAINS).retain(|s| s.device != device);
    lock(&QUEUES).retain(|&(d, _, _)| d != device);
    PRESENT_FAMILY.store(u32::MAX, Relaxed);
}

/// The device the engine renders with and its physical device, for the
/// depth capture, which works on the same one.
pub(super) fn current_device() -> Option<(usize, usize)> {
    lock(&CTX).as_ref().map(|c| (c.device, c.physical))
}

/// The family `queue` was fetched from, when the engine fetched it through
/// the hooks.
pub(super) fn queue_family(queue: usize) -> Option<u32> {
    family_of(queue)
}

/// The family the engine's frames most likely go to.
pub(super) fn likely_queue_family() -> Option<u32> {
    lock(&CTX).as_ref().map(likely_family)
}

/// The extent of the most recently created swapchain, `(0, 0)` before the
/// first.
pub fn extent() -> (u32, u32) {
    let v = LAST_EXTENT.load(Relaxed);
    ((v >> 32) as u32, v as u32)
}

/// Whether a request is armed. The whole cost of this module on a frame
/// nobody asked to capture.
#[inline]
pub(super) fn pending() -> bool {
    ARMED.load(Relaxed)
}

// --------------------------------------------------------- the present

/// `vkQueuePresentKHR` with a request armed. `forward` makes the real
/// present with the info it is given and returns its result.
pub(super) fn present(queue: *mut c_void, info: *const c_void, forward: impl FnOnce(*const c_void) -> i32) -> i32 {
    let started = Instant::now();
    let Some(armed) = take_armed() else {
        return forward(info);
    };
    if armed.gate.as_ref().is_some_and(|g| g.load(Relaxed) == GATE_CANCELLED) {
        give_back(armed, Why::Failed("the frame this screenshot belonged to was not captured".into()));
        return forward(info);
    }
    if info.is_null() {
        give_back(armed, Why::Failed("the present carried no VkPresentInfoKHR".into()));
        return forward(info);
    }
    // SAFETY: the engine's own `VkPresentInfoKHR`, read whole and unaligned;
    // its own structure is never written.
    let mut copy: PresentInfo = unsafe { std::ptr::read_unaligned(info as *const PresentInfo) };
    let target = match target_of(&copy, queue, &armed.prep) {
        Ok(t) => t,
        Err(why) => {
            give_back(armed, why);
            return forward(info);
        }
    };
    if let Err(why) = record_and_submit(&armed.prep, queue, &copy, &target) {
        // Nothing was submitted, so nothing consumed the engine's semaphores:
        // its own present info goes through as it was.
        give_back(armed, why);
        return forward(info);
    }
    let semaphore = armed.prep.semaphore;
    copy.wait_semaphore_count = 1;
    copy.p_wait_semaphores = &semaphore;
    lock(&COPYING).push(Copying {
        id: armed.id,
        swapchain: target.swapchain,
        fence: armed.prep.fence,
        fns: armed.prep.fns.clone(),
    });
    let job = InFlight { armed, target, render_thread: started.elapsed() };
    hand_to_worker(job);
    forward(&copy as *const PresentInfo as *const c_void)
}

/// Take the armed request, if the lock is free. A devctl thread holding it is
/// only ever doing so briefly; the next present takes the request instead.
fn take_armed() -> Option<Armed> {
    let mut slot = match SLOT.try_lock() {
        Ok(s) => s,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    if slot.as_ref().is_some_and(|a| a.gate.as_ref().is_some_and(|g| g.load(Relaxed) == GATE_WAIT)) {
        // Armed for a later frame; this present goes through untouched.
        return None;
    }
    let armed = slot.take();
    ARMED.store(false, Relaxed);
    armed
}

/// Hand a request back to the devctl thread that is waiting on it, which
/// tears the objects down off the render thread.
fn give_back(armed: Armed, why: Why) {
    if let Err(mpsc::SendError(Event::Returned(prep, _))) = armed.reply.send(Event::Returned(armed.prep, why)) {
        // Nobody is waiting any more; nothing on this thread may destroy it.
        prep.set_aside();
    }
}

fn hand_to_worker(job: InFlight) {
    let Some(worker) = WORKER.get() else {
        not_handed(job);
        return;
    };
    if let Err(mpsc::SendError(job)) = worker.send(job) {
        not_handed(job);
    }
}

/// The worker is gone. Never seen; handled so a request still gets an answer.
fn not_handed(job: InFlight) {
    let id = job.armed.id;
    lock(&COPYING).retain(|c| c.id != id);
    let _ = job.armed.reply.send(Event::Done(Err("the capture worker thread is not running".into())));
    job.armed.prep.set_aside();
}

/// Which image the present shows, checked against what was prepared.
fn target_of(p: &PresentInfo, queue: *mut c_void, prep: &Prepared) -> Result<Target, Why> {
    if p.swapchain_count == 0 || p.p_swapchains.is_null() || p.p_image_indices.is_null() {
        return Err(Why::Failed("the present names no swapchain".into()));
    }
    // SAFETY: a present naming at least one swapchain carries at least one
    // handle and one index. Only the first is captured.
    let (handle, index) =
        unsafe { (std::ptr::read_unaligned(p.p_swapchains), std::ptr::read_unaligned(p.p_image_indices)) };
    let (width, height, format, image) = {
        let list = lock(&SWAPCHAINS);
        let Some(s) = list.iter().find(|s| s.handle == handle) else {
            return Err(Why::Failed("the presented swapchain is unknown".into()));
        };
        let images = s.images.as_ref().map_err(|e| Why::Failed(e.clone()))?;
        let Some(&image) = images.get(index as usize) else {
            return Err(Why::Failed(format!("image index {index} is outside {} swapchain images", images.len())));
        };
        (s.width, s.height, s.format, image)
    };
    channel_order(format).map_err(Why::Failed)?;
    let family = family_of(queue as usize);
    if let Some(f) = family {
        PRESENT_FAMILY.store(f, Relaxed);
    }
    let family = family.unwrap_or(prep.family);
    let need = width as u64 * height as u64 * 4;
    if need > prep.size || family != prep.family {
        return Err(Why::Retry {
            reason: if family != prep.family {
                format!("the present came on queue family {family}, not {}", prep.family)
            } else {
                format!("the presented swapchain is {width}x{height}, larger than the buffer prepared")
            },
            width,
            height,
            family,
        });
    }
    Ok(Target { swapchain: handle, image, width, height, format })
}

fn family_of(queue: usize) -> Option<u32> {
    lock(&QUEUES).iter().find(|&&(_, q, _)| q == queue).map(|&(_, _, f)| f)
}

/// Record the copy and submit it. The only Vulkan work on the render thread.
fn record_and_submit(prep: &Prepared, queue: *mut c_void, p: &PresentInfo, t: &Target) -> Result<(), Why> {
    let f = &*prep.fns;
    let cmd = prep.cmd as *mut c_void;
    let none = std::ptr::null();
    let begin = CommandBufferBeginInfo {
        s_type: ST_COMMAND_BUFFER_BEGIN_INFO,
        p_next: none,
        flags: COMMAND_BUFFER_ONE_TIME_SUBMIT,
        p_inheritance_info: none,
    };
    let rc = (f.begin_command_buffer)(cmd, &begin);
    if rc != VK_SUCCESS {
        return Err(Why::Failed(format!("vkBeginCommandBuffer failed ({rc})")));
    }
    let waits = if p.p_wait_semaphores.is_null() { 0 } else { p.wait_semaphore_count };
    // With semaphores, the copy's submission waits on them at the transfer
    // stage, and a barrier whose source is that stage is ordered after them.
    // Without any, only submission order on this queue puts the copy after
    // the frame, so the source scope has to reach the engine's colour writes.
    let (src_stage, src_access) = if waits > 0 {
        (STAGE_TRANSFER, 0)
    } else {
        (STAGE_ALL_COMMANDS, ACCESS_COLOR_ATTACHMENT_WRITE | ACCESS_MEMORY_WRITE)
    };
    let range = || ImageSubresourceRange {
        aspect_mask: ASPECT_COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    let to_copy = ImageMemoryBarrier {
        s_type: ST_IMAGE_MEMORY_BARRIER,
        p_next: none,
        src_access_mask: src_access,
        dst_access_mask: ACCESS_TRANSFER_READ,
        old_layout: LAYOUT_PRESENT_SRC_KHR,
        new_layout: LAYOUT_TRANSFER_SRC_OPTIMAL,
        src_queue_family_index: QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: QUEUE_FAMILY_IGNORED,
        image: t.image,
        subresource_range: range(),
    };
    (f.cmd_pipeline_barrier)(cmd, src_stage, STAGE_TRANSFER, 0, 0, none.cast(), 0, none, 1, &to_copy);
    let region = BufferImageCopy {
        buffer_offset: 0,
        buffer_row_length: 0,
        buffer_image_height: 0,
        image_subresource: ImageSubresourceLayers {
            aspect_mask: ASPECT_COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        },
        image_offset: [0, 0, 0],
        image_extent: [t.width, t.height, 1],
    };
    (f.cmd_copy_image_to_buffer)(cmd, t.image, LAYOUT_TRANSFER_SRC_OPTIMAL, prep.buffer, 1, &region);
    // A fence makes device writes available, not visible to the host; this
    // barrier is what lets the worker read the buffer.
    let to_host = MemoryBarrier {
        s_type: ST_MEMORY_BARRIER,
        p_next: none,
        src_access_mask: ACCESS_TRANSFER_WRITE,
        dst_access_mask: ACCESS_HOST_READ,
    };
    (f.cmd_pipeline_barrier)(cmd, STAGE_TRANSFER, STAGE_HOST, 0, 1, &to_host, 0, none, 0, none.cast());
    // Back for the presentation engine, which waits on the semaphore this
    // submission signals after the transition.
    let back = ImageMemoryBarrier {
        src_access_mask: 0,
        dst_access_mask: 0,
        old_layout: LAYOUT_TRANSFER_SRC_OPTIMAL,
        new_layout: LAYOUT_PRESENT_SRC_KHR,
        subresource_range: range(),
        ..to_copy
    };
    (f.cmd_pipeline_barrier)(cmd, STAGE_TRANSFER, STAGE_BOTTOM_OF_PIPE, 0, 0, none.cast(), 0, none, 1, &back);
    let rc = (f.end_command_buffer)(cmd);
    if rc != VK_SUCCESS {
        return Err(Why::Failed(format!("vkEndCommandBuffer failed ({rc})")));
    }
    let few = [STAGE_TRANSFER; 8];
    let many;
    let stages = if waits as usize <= few.len() {
        few.as_ptr()
    } else {
        many = vec![STAGE_TRANSFER; waits as usize];
        many.as_ptr()
    };
    let cmds = [cmd];
    let signal = [prep.semaphore];
    let submit = SubmitInfo {
        s_type: ST_SUBMIT_INFO,
        p_next: none,
        wait_semaphore_count: waits,
        p_wait_semaphores: p.p_wait_semaphores,
        p_wait_dst_stage_mask: stages,
        command_buffer_count: 1,
        p_command_buffers: cmds.as_ptr(),
        signal_semaphore_count: 1,
        p_signal_semaphores: signal.as_ptr(),
    };
    // This thread is inside the engine's own vkQueuePresentKHR, which is what
    // makes touching the queue legal here and nowhere else in this module.
    let rc = (f.queue_submit)(queue, 1, &submit, prep.fence);
    if rc != VK_SUCCESS {
        return Err(Why::Failed(format!("vkQueueSubmit failed ({rc})")));
    }
    Ok(())
}

// ------------------------------------------------------------ the worker

fn ensure_worker() -> Result<(), String> {
    static START: Mutex<()> = Mutex::new(());
    let _one = lock(&START);
    if WORKER.get().is_some() {
        return Ok(());
    }
    let (tx, rx) = mpsc::channel::<InFlight>();
    std::thread::Builder::new()
        .name("intoxicated-capture".into())
        .spawn(move || {
            for job in rx {
                // A panic would lose one reply, not the worker.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| finish(job)));
            }
        })
        .map_err(|e| format!("could not start the capture worker: {e}"))?;
    let _ = WORKER.set(tx);
    Ok(())
}

/// Wait for the copy, encode, write, tear down, answer.
fn finish(job: InFlight) {
    let InFlight { armed, target, render_thread } = job;
    let Armed { id, path, prep, reply, given_up, .. } = armed;
    let fns = prep.fns.clone();
    let waited = Instant::now();
    let rc = (fns.wait_for_fences)(fns.dev(), 1, &prep.fence, 1, FENCE_WAIT_NS);
    let copy_time = waited.elapsed();
    lock(&COPYING).retain(|c| c.id != id);
    if rc != VK_SUCCESS {
        prep.set_aside();
        let why = if rc == VK_TIMEOUT {
            "the copy's fence did not signal within two seconds".to_string()
        } else {
            format!("vkWaitForFences failed ({rc})")
        };
        let _ = reply.send(Event::Done(Err(why)));
        return;
    }
    let encode_start = Instant::now();
    let png = read_and_encode(&prep, &target);
    let encode_time = encode_start.elapsed();
    let memory_type = prep.memory_type;
    let bulk = !prep.cached;
    prep.destroy_keeping_semaphore();
    let write_start = Instant::now();
    let result = png.and_then(|png| {
        if given_up.load(Relaxed) {
            return Err("the request had already been answered".to_string());
        }
        std::fs::write(&path, &png).map_err(|e| format!("writing {path}: {e}"))
    });
    let write_time = write_start.elapsed();
    if result.is_ok() {
        println!(
            "[android] vulkan: capture {}x{} (format {}, memory type {memory_type}{}) to {path}: render thread \
             {:.3} ms, copy {:.1} ms, encode {:.1} ms, write {:.1} ms",
            target.width,
            target.height,
            target.format,
            if bulk { ", copied out first" } else { "" },
            render_thread.as_secs_f64() * 1e3,
            copy_time.as_secs_f64() * 1e3,
            encode_time.as_secs_f64() * 1e3,
            write_time.as_secs_f64() * 1e3,
        );
    }
    let _ = reply.send(Event::Done(result.map(|()| format!("{path} {}x{}", target.width, target.height))));
}

fn read_and_encode(prep: &Prepared, t: &Target) -> Result<Vec<u8>, String> {
    let f = &*prep.fns;
    if !prep.coherent {
        let range = MappedMemoryRange {
            s_type: ST_MAPPED_MEMORY_RANGE,
            p_next: std::ptr::null(),
            memory: prep.memory,
            offset: 0,
            size: WHOLE_SIZE,
        };
        let rc = (f.invalidate_ranges)(f.dev(), 1, &range);
        if rc != VK_SUCCESS {
            return Err(format!("vkInvalidateMappedMemoryRanges failed ({rc})"));
        }
    }
    let len = t.width as usize * t.height as usize * 4;
    // SAFETY: `mapped` is the host address of a mapping of `prep.size` bytes,
    // at least `len` (checked by `target_of`); the copy into it has completed
    // (the fence) and been made visible to the host (the barrier, and the
    // invalidation above where the memory is not coherent).
    let mapping = unsafe { std::slice::from_raw_parts(prep.mapped as *const u8, len) };
    if prep.cached {
        encode_png(mapping, t.width, t.height, t.format)
    } else {
        // Uncached memory read a few bytes at a time is the 790 ms this
        // module was rewritten for; one sequential copy out costs tens of
        // milliseconds.
        let copy = mapping.to_vec();
        encode_png(&copy, t.width, t.height, t.format)
    }
}

// ------------------------------------------------------- the devctl side

/// Ask for the next presented frame to be written to `path`, and wait for
/// it. `Ok` carries `<path> <w>x<h>`, sent only once the file is written.
pub fn request(path: &str) -> Result<String, String> {
    request_gated(path, None, || {})
}

/// [`request`], taken only by a present after `gate` has been opened, and
/// failed by the first present after it was cancelled. `armed` runs once
/// the request is armed, so the caller can then let the frame come.
pub(super) fn request_gated(path: &str, gate: Option<Arc<AtomicU8>>, armed: impl FnOnce()) -> Result<String, String> {
    let r = run_request(path, gate, armed);
    if let Err(e) = &r {
        println!("[android] vulkan: capture failed: {e}");
    }
    r
}

fn run_request(path: &str, gate: Option<Arc<AtomicU8>>, armed: impl FnOnce()) -> Result<String, String> {
    let deadline = Instant::now() + PRESENT_WAIT;
    let id = NEXT_ID.fetch_add(1, Relaxed);
    let (tx, rx) = mpsc::channel::<Event>();
    let given_up = Arc::new(AtomicBool::new(false));
    ensure_worker()?;
    arm(id, path, &tx, &given_up, &gate, None)?;
    armed();
    let mut retries = 0;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Event::Done(r)) => return r,
            Ok(Event::Returned(prep, why)) => {
                prep.destroy();
                match why {
                    Why::Superseded => return Err("replaced by a newer screenshot request".into()),
                    Why::Failed(m) => return Err(m),
                    Why::Retry { reason, width, height, family } => {
                        retries += 1;
                        if retries > MAX_RETRIES {
                            return Err(reason);
                        }
                        if Instant::now() >= deadline {
                            return Err(NO_FRAME.into());
                        }
                        println!("[android] vulkan: capture prepared again: {reason}");
                        arm(id, path, &tx, &given_up, &gate, Some((width, height, family)))?;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(a) = withdraw(id) {
                    a.prep.destroy();
                    return Err(NO_FRAME.into());
                }
                // A present took it: wait for that capture's own outcome.
                return match rx.recv_timeout(OUTCOME_WAIT) {
                    Ok(Event::Done(r)) => r,
                    Ok(Event::Returned(prep, why)) => {
                        prep.destroy();
                        Err(match why {
                            Why::Failed(m) => m,
                            Why::Superseded => "replaced by a newer screenshot request".into(),
                            Why::Retry { .. } => NO_FRAME.into(),
                        })
                    }
                    Err(_) => {
                        given_up.store(true, Relaxed);
                        Err(format!(
                            "a present took the capture but it had no result within {} seconds",
                            OUTCOME_WAIT.as_secs()
                        ))
                    }
                };
            }
        }
    }
}

/// Take request `id` back out of the slot if no present has taken it.
fn withdraw(id: u64) -> Option<Armed> {
    let mut slot = lock(&SLOT);
    if slot.as_ref().is_some_and(|a| a.id == id) {
        ARMED.store(false, Relaxed);
        slot.take()
    } else {
        None
    }
}

/// Prepare a capture and arm it. `hint` is the extent and family a present
/// asked for when it handed an earlier preparation back.
///
/// Holds the device lock throughout, so a device being destroyed either sees
/// the armed request and withdraws it, or this finds the device gone.
fn arm(
    id: u64,
    path: &str,
    reply: &Sender<Event>,
    given_up: &Arc<AtomicBool>,
    gate: &Option<Arc<AtomicU8>>,
    hint: Option<(u32, u32, u32)>,
) -> Result<(), String> {
    let mut guard = lock(&CTX);
    let ctx = guard.as_mut().ok_or(NO_SWAPCHAIN)?;
    let (width, height, family) = match hint {
        Some(h) => h,
        None => {
            let (w, h) = largest_live_extent(ctx.device).ok_or(NO_SWAPCHAIN)?;
            (w, h, likely_family(ctx))
        }
    };
    let resolved = match ctx.resolved.take() {
        Some(r) => r,
        None => resolve(ctx.device, ctx.physical)?,
    };
    let resolved = ctx.resolved.insert(resolved);
    let prep = prepare(resolved, width as u64 * height as u64 * 4, family)?;
    let armed = Armed {
        id,
        path: path.to_string(),
        prep,
        reply: reply.clone(),
        given_up: given_up.clone(),
        gate: gate.clone(),
    };
    let mut slot = lock(&SLOT);
    // One request at a time: an untaken one is replaced, since the newer
    // frame is the one wanted.
    if let Some(old) = slot.take() {
        if let Err(mpsc::SendError(Event::Returned(p, _))) = old.reply.send(Event::Returned(old.prep, Why::Superseded))
        {
            p.destroy();
        }
    }
    *slot = Some(armed);
    ARMED.store(true, Relaxed);
    Ok(())
}

fn largest_live_extent(device: usize) -> Option<(u32, u32)> {
    lock(&SWAPCHAINS)
        .iter()
        .filter(|s| s.device == device && s.width > 0 && s.height > 0)
        .max_by_key(|s| (s.width as u64 * s.height as u64, s.seq))
        .map(|s| (s.width, s.height))
}

/// The family the next present will most likely come on.
fn likely_family(ctx: &DeviceCtx) -> u32 {
    let seen = PRESENT_FAMILY.load(Relaxed);
    if seen != u32::MAX {
        return seen;
    }
    let queues = lock(&QUEUES);
    let mut families = queues.iter().filter(|q| q.0 == ctx.device).map(|q| q.2);
    match families.next() {
        Some(first) if families.all(|f| f == first) => first,
        _ => ctx.first_family,
    }
}

fn resolve(device: usize, physical: usize) -> Result<Resolved, String> {
    let fns = DeviceFns::resolve(device as Dev)?;
    let props = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties");
    if props.is_null() || physical == 0 {
        return Err("vkGetPhysicalDeviceMemoryProperties is unavailable".into());
    }
    // SAFETY: resolved by the host loader for exactly this name.
    let props: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties) = unsafe { std::mem::transmute(props) };
    let mut p = PhysicalDeviceMemoryProperties {
        memory_type_count: 0,
        memory_types: [[0; 2]; 32],
        memory_heap_count: 0,
        memory_heaps: [[0; 2]; 16],
    };
    props(physical as *mut c_void, &mut p);
    let n = (p.memory_type_count as usize).min(32);
    Ok(Resolved { fns: Arc::new(fns), memory_types: p.memory_types[..n].iter().map(|t| t[0]).collect() })
}

/// Create everything one capture needs. Off the render thread; none of these
/// objects is shared with anything yet.
fn prepare(r: &Resolved, bytes: u64, family: u32) -> Result<Prepared, String> {
    let f = &*r.fns;
    let d = f.dev();
    let none = std::ptr::null();
    let mut p = Prepared {
        fns: r.fns.clone(),
        buffer: 0,
        memory: 0,
        mapped: 0,
        size: bytes,
        memory_type: 0,
        cached: false,
        coherent: false,
        family,
        pool: 0,
        cmd: 0,
        fence: 0,
        semaphore: 0,
    };
    let built = (|| -> Result<(), String> {
        let info = BufferCreateInfo {
            s_type: ST_BUFFER_CREATE_INFO,
            p_next: none,
            flags: 0,
            size: bytes,
            usage: BUFFER_USAGE_TRANSFER_DST,
            sharing_mode: 0,
            queue_family_index_count: 0,
            p_queue_family_indices: none.cast(),
        };
        if (f.create_buffer)(d, &info, none, &mut p.buffer) != VK_SUCCESS {
            return Err("vkCreateBuffer failed".into());
        }
        let mut req = MemoryRequirements::default();
        (f.buffer_requirements)(d, p.buffer, &mut req);
        let choice = readback_memory_type(&r.memory_types, req.memory_type_bits)?;
        (p.memory_type, p.cached, p.coherent) = (choice.index, choice.cached, choice.coherent);
        let alloc = MemoryAllocateInfo {
            s_type: ST_MEMORY_ALLOCATE_INFO,
            p_next: none,
            allocation_size: req.size,
            memory_type_index: choice.index,
        };
        if (f.allocate_memory)(d, &alloc, none, &mut p.memory) != VK_SUCCESS {
            return Err("vkAllocateMemory failed".into());
        }
        if (f.bind_buffer_memory)(d, p.buffer, p.memory, 0) != VK_SUCCESS {
            return Err("vkBindBufferMemory failed".into());
        }
        let mut ptr = std::ptr::null_mut();
        if (f.map_memory)(d, p.memory, 0, WHOLE_SIZE, 0, &mut ptr) != VK_SUCCESS || ptr.is_null() {
            return Err("vkMapMemory failed".into());
        }
        p.mapped = ptr as usize;
        let pool = CommandPoolCreateInfo {
            s_type: ST_COMMAND_POOL_CREATE_INFO,
            p_next: none,
            flags: COMMAND_POOL_TRANSIENT,
            queue_family_index: family,
        };
        if (f.create_command_pool)(d, &pool, none, &mut p.pool) != VK_SUCCESS {
            return Err("vkCreateCommandPool failed".into());
        }
        let alloc = CommandBufferAllocateInfo {
            s_type: ST_COMMAND_BUFFER_ALLOCATE_INFO,
            p_next: none,
            command_pool: p.pool,
            level: 0,
            command_buffer_count: 1,
        };
        let mut cmd = std::ptr::null_mut();
        if (f.allocate_command_buffers)(d, &alloc, &mut cmd) != VK_SUCCESS || cmd.is_null() {
            return Err("vkAllocateCommandBuffers failed".into());
        }
        p.cmd = cmd as usize;
        let fence = PlainCreateInfo { s_type: ST_FENCE_CREATE_INFO, p_next: none, flags: 0 };
        if (f.create_fence)(d, &fence, none, &mut p.fence) != VK_SUCCESS {
            return Err("vkCreateFence failed".into());
        }
        let semaphore = PlainCreateInfo { s_type: ST_SEMAPHORE_CREATE_INFO, p_next: none, flags: 0 };
        if (f.create_semaphore)(d, &semaphore, none, &mut p.semaphore) != VK_SUCCESS {
            return Err("vkCreateSemaphore failed".into());
        }
        Ok(())
    })();
    match built {
        Ok(()) => {
            live_add();
            Ok(p)
        }
        Err(e) => {
            p.destroy_parts();
            Err(e)
        }
    }
}

// ------------------------------------------------------------ pure parts

#[derive(Debug, PartialEq, Eq)]
pub(super) struct MemoryChoice {
    pub index: u32,
    pub cached: bool,
    pub coherent: bool,
}

/// The memory type for the readback buffer, from the device's types (flags
/// by index) and the buffer's allowed-type bits.
///
/// Host-cached first, coherent if such a type exists; otherwise the first
/// host-visible coherent type, which the worker copies out of in one pass.
pub(super) fn readback_memory_type(types: &[u32], allowed: u32) -> Result<MemoryChoice, String> {
    let first = |want: u32| {
        (0..types.len().min(32)).find(|&i| allowed & (1 << i) != 0 && types[i] & want == want).map(|i| i as u32)
    };
    let visible = MEMORY_HOST_VISIBLE;
    if let Some(index) = first(visible | MEMORY_HOST_CACHED | MEMORY_HOST_COHERENT) {
        return Ok(MemoryChoice { index, cached: true, coherent: true });
    }
    if let Some(index) = first(visible | MEMORY_HOST_CACHED) {
        return Ok(MemoryChoice { index, cached: true, coherent: false });
    }
    if let Some(index) = first(visible | MEMORY_HOST_COHERENT) {
        return Ok(MemoryChoice { index, cached: false, coherent: true });
    }
    Err("no host-visible memory type accepts this buffer".into())
}

/// Where R, G and B sit in a source pixel of `format`.
fn channel_order(format: i32) -> Result<[usize; 3], String> {
    match format {
        // VK_FORMAT_B8G8R8A8_UNORM, VK_FORMAT_B8G8R8A8_SRGB
        44 | 50 => Ok([2, 1, 0]),
        // VK_FORMAT_R8G8B8A8_UNORM, VK_FORMAT_R8G8B8A8_SRGB
        37 | 43 => Ok([0, 1, 2]),
        n => Err(format!("unhandled swapchain format {n}")),
    }
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// The largest stored deflate block.
const STORED_BLOCK: usize = 65_535;

/// Tightly packed 4-byte pixels to a PNG: 8-bit RGB, filter None on every
/// row, one IDAT holding a zlib stream of stored blocks, no other chunks.
/// Alpha is dropped and the bytes are copied as they are, sRGB or not.
///
/// The layout is fixed, not a choice made per image: the same frame always
/// encodes to the same bytes, which is what lets a capture be compared
/// against an earlier one byte for byte.
pub fn encode_png(pixels: &[u8], width: u32, height: u32, format: i32) -> Result<Vec<u8>, String> {
    let [r, g, b] = channel_order(format)?;
    let need = pixel_bytes(width, height, 4)?;
    if pixels.len() < need {
        return Err(format!("{} bytes of pixels for a {width}x{height} image", pixels.len()));
    }
    let w = width as usize;
    encode_rows(width, height, 2, 3, pixels[..need].chunks_exact(4 * w).map(|src| {
        move |row: &mut [u8]| {
            for (dst, px) in row.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                dst[0] = px[r];
                dst[1] = px[g];
                dst[2] = px[b];
            }
        }
    }))
}

/// One byte per pixel, row after row, to an 8-bit greyscale PNG laid out
/// the way [`encode_png`] lays out colour: filter None, stored blocks.
pub(super) fn encode_gray_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let need = pixel_bytes(width, height, 1)?;
    if pixels.len() < need {
        return Err(format!("{} bytes of pixels for a {width}x{height} image", pixels.len()));
    }
    let w = width as usize;
    encode_rows(width, height, 0, 1, pixels[..need].chunks_exact(w).map(|src| move |row: &mut [u8]| row.copy_from_slice(src)))
}

/// `width * height * bytes`, or why there is no such image.
fn pixel_bytes(width: u32, height: u32, bytes: usize) -> Result<usize, String> {
    if width == 0 || height == 0 {
        return Err(format!("a {width}x{height} image has no pixels"));
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(bytes))
        .ok_or_else(|| "the image is too large".to_string())
}

/// The PNG itself: signature, IHDR (8-bit samples of `colour_type`,
/// `channels` per pixel), one IDAT of stored deflate blocks, IEND. Each item
/// of `rows` fills one row's pixels; the filter byte before them stays 0.
fn encode_rows<F: FnOnce(&mut [u8])>(
    width: u32,
    height: u32,
    colour_type: u8,
    channels: usize,
    rows: impl Iterator<Item = F>,
) -> Result<Vec<u8>, String> {
    let (w, h) = (width as usize, height as usize);
    let row_len = channels * w + 1;
    let raw = row_len * h;
    let blocks = raw.div_ceil(STORED_BLOCK);
    let idat_len = 2 + raw + 5 * blocks + 4;
    if idat_len > i32::MAX as usize {
        return Err(format!("a {width}x{height} image does not fit one PNG chunk"));
    }
    let mut out = Vec::with_capacity(63 + raw + 5 * blocks);
    out.extend_from_slice(&PNG_SIGNATURE);

    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = colour_type; // 2 truecolour, 0 greyscale
    chunk(&mut out, b"IHDR", &ihdr);

    out.extend_from_slice(&(idat_len as u32).to_be_bytes());
    let idat_at = out.len();
    out.extend_from_slice(b"IDAT");
    // zlib: deflate, 32K window, no dictionary, fastest; 0x7801 % 31 == 0.
    out.extend_from_slice(&[0x78, 0x01]);
    let mut stored = Stored { left_total: raw, left_in_block: 0 };
    let mut adler = Adler32::default();
    let mut row = vec![0u8; row_len];
    for fill in rows {
        // row[0] stays 0: filter type None.
        fill(&mut row[1..]);
        adler.update(&row);
        stored.push(&mut out, &row);
    }
    out.extend_from_slice(&adler.value().to_be_bytes());
    let crc = crc32fast::hash(&out[idat_at..]);
    out.extend_from_slice(&crc.to_be_bytes());

    chunk(&mut out, b"IEND", &[]);
    debug_assert_eq!(out.len(), 63 + raw + 5 * blocks);
    Ok(out)
}

/// One PNG chunk: length, type, data, CRC-32 of type and data.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let at = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32fast::hash(&out[at..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Deflate stored blocks, written as the raw data arrives: a header before
/// each run of at most 65535 bytes, BFINAL on the last only.
struct Stored {
    left_total: usize,
    left_in_block: usize,
}

impl Stored {
    fn push(&mut self, out: &mut Vec<u8>, mut data: &[u8]) {
        while !data.is_empty() {
            if self.left_in_block == 0 {
                let len = self.left_total.min(STORED_BLOCK);
                out.push(u8::from(self.left_total <= STORED_BLOCK));
                out.extend_from_slice(&(len as u16).to_le_bytes());
                out.extend_from_slice(&(!(len as u16)).to_le_bytes());
                self.left_in_block = len;
            }
            let n = data.len().min(self.left_in_block);
            out.extend_from_slice(&data[..n]);
            data = &data[n..];
            self.left_in_block -= n;
            self.left_total -= n;
        }
    }
}

/// Adler-32 (RFC 1950), reduced once per 5552 bytes: the most that can be
/// summed before `b` could overflow 32 bits.
struct Adler32 {
    a: u32,
    b: u32,
}

impl Default for Adler32 {
    fn default() -> Self {
        Adler32 { a: 1, b: 0 }
    }
}

impl Adler32 {
    const MOD: u32 = 65_521;

    fn update(&mut self, data: &[u8]) {
        for run in data.chunks(5552) {
            for &x in run {
                self.a += x as u32;
                self.b += self.a;
            }
            self.a %= Self::MOD;
            self.b %= Self::MOD;
        }
    }

    fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn sha256(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    #[test]
    fn one_pixel_bgra_and_rgba_encode_to_the_spec_bytes() {
        let bgra = encode_png(&[0x10, 0x20, 0x30, 0x40], 1, 1, 44).unwrap();
        assert_eq!(bgra.len(), 72);
        assert_eq!(
            hex(&bgra),
            "89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de0000000f494441547801010400fbff0030\
             201000e400614e36457e0000000049454e44ae426082"
        );
        let rgba = encode_png(&[0x10, 0x20, 0x30, 0x40], 1, 1, 37).unwrap();
        assert_eq!(
            hex(&rgba),
            "89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de0000000f494441547801010400fbff0010\
             203000a4006130d0de260000000049454e44ae426082"
        );
        // The sRGB formats are copied the same way as their UNORM twins.
        assert_eq!(encode_png(&[0x10, 0x20, 0x30, 0x40], 1, 1, 50).unwrap(), bgra);
        assert_eq!(encode_png(&[0x10, 0x20, 0x30, 0x40], 1, 1, 43).unwrap(), rgba);
    }

    #[test]
    fn two_by_two_rgba_encodes_to_the_spec_bytes() {
        let pixels: Vec<u8> = (1..=16).collect();
        let png = encode_png(&pixels, 2, 2, 37).unwrap();
        assert_eq!(png.len(), 82);
        assert_eq!(
            hex(&png),
            "89504e470d0a1a0a0000000d4948445200000002000000020802000000fdd49a7300000019494441547801010e00f1ff000102\
             0305060700090a0b0d0e0f01da0061332463af0000000049454e44ae426082"
        );
    }

    fn ramp(width: u32, height: u32) -> Vec<u8> {
        (0..width as usize * height as usize * 4).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn stored_block_boundaries_match_the_spec_vectors() {
        for (w, h, len, digest) in [
            (28, 771, 65_603, "7f24bbd052b362a438f94d0dc8e3e2b194943f2bb4a1bf69db2808f1db226d02"),
            (28, 772, 65_693, "dfc333e046de18fbe3a68c2305018f37cb054ae6f39383a4019a6e4a7a4130ff"),
            (21_845, 1, 65_609, "2506db42832fe63be290bb4dad6daf3f531e67568667a73d0076692bfc4e074a"),
        ] {
            let png = encode_png(&ramp(w, h), w, h, 44).unwrap();
            assert_eq!(png.len(), len, "{w}x{h}");
            assert_eq!(sha256(&png), digest, "{w}x{h}");
        }
    }

    #[test]
    fn the_size_follows_the_formula_and_the_stream_inflates_to_the_rows() {
        let (w, h) = (1667u32, 9u32);
        let pixels = ramp(w, h);
        let png = encode_png(&pixels, w, h, 44).unwrap();
        let raw = (3 * w as usize + 1) * h as usize;
        assert_eq!(png.len(), 63 + raw + 5 * raw.div_ceil(65_535));
        // An independent inflater, checking the zlib framing and Adler-32 too.
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let data = miniz_oxide::inflate::decompress_to_vec_zlib(&png[41..41 + idat_len]).unwrap();
        assert_eq!(data.len(), raw);
        for (y, row) in data.chunks_exact(3 * w as usize + 1).enumerate() {
            assert_eq!(row[0], 0, "filter byte of row {y}");
            for x in [0usize, 1, w as usize - 1] {
                let src = &pixels[(y * w as usize + x) * 4..][..4];
                assert_eq!(&row[1 + 3 * x..][..3], &[src[2], src[1], src[0]], "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn the_ihdr_chunk_for_the_real_extent_matches() {
        let png = encode_png(&vec![0; 1667 * 1349 * 4], 1667, 1349, 44).unwrap();
        assert_eq!(hex(&png[8..33]), "0000000d49484452000006830000054508020000005cc3277c");
        assert_eq!(png.len(), 6_748_276);
    }

    #[test]
    fn a_greyscale_png_has_its_header_and_inflates_to_its_rows() {
        // More than one stored block's worth of rows.
        let (w, h) = (300u32, 230u32);
        let pixels: Vec<u8> = (0..w * h).map(|i| (i % 253) as u8).collect();
        let png = encode_gray_png(&pixels, w, h).unwrap();
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!((png[24], png[25]), (8, 0), "8-bit greyscale");
        let raw = (w as usize + 1) * h as usize;
        assert_eq!(png.len(), 63 + raw + 5 * raw.div_ceil(65_535));
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        let data = miniz_oxide::inflate::decompress_to_vec_zlib(&png[41..41 + idat_len]).unwrap();
        for (y, row) in data.chunks_exact(w as usize + 1).enumerate() {
            assert_eq!(row[0], 0, "filter byte of row {y}");
            assert_eq!(&row[1..], &pixels[y * w as usize..][..w as usize], "row {y}");
        }
        assert!(encode_gray_png(&pixels[..10], w, h).is_err(), "short pixel data");
        assert!(encode_gray_png(&[], 0, 1).is_err(), "empty image");
    }

    #[test]
    fn an_unknown_format_is_an_error_naming_it_not_a_guess() {
        let e = encode_png(&[0; 4], 1, 1, 3).unwrap_err();
        assert_eq!(e, "unhandled swapchain format 3");
        assert!(encode_png(&[0; 3], 1, 1, 44).is_err(), "short pixel data");
        assert!(encode_png(&[], 0, 1, 44).is_err(), "empty image");
    }

    #[test]
    fn the_readback_type_prefers_cached_memory() {
        let nvidia = [0x0, 0x1, 0x6, 0xe, 0x7];
        assert_eq!(readback_memory_type(&nvidia, !0), Ok(MemoryChoice { index: 3, cached: true, coherent: true }));
        assert_eq!(
            readback_memory_type(&nvidia, !(1 << 3)),
            Ok(MemoryChoice { index: 2, cached: false, coherent: true })
        );
        assert_eq!(readback_memory_type(&[0xf], !0), Ok(MemoryChoice { index: 0, cached: true, coherent: true }));
        assert!(readback_memory_type(&[0x1], !0).is_err());
        // Cached but not coherent beats uncached coherent; it is invalidated.
        assert_eq!(readback_memory_type(&[0x6, 0xa], !0), Ok(MemoryChoice { index: 1, cached: true, coherent: false }));
    }

    #[test]
    fn the_vulkan_structs_have_the_c_layout() {
        use std::mem::size_of;
        assert_eq!(size_of::<PresentInfo>(), 64);
        assert_eq!(size_of::<SubmitInfo>(), 72);
        assert_eq!(size_of::<ImageMemoryBarrier>(), 72);
        assert_eq!(size_of::<MemoryBarrier>(), 24);
        assert_eq!(size_of::<BufferImageCopy>(), 56);
        assert_eq!(size_of::<BufferCreateInfo>(), 56);
        assert_eq!(size_of::<MemoryAllocateInfo>(), 32);
        assert_eq!(size_of::<MemoryRequirements>(), 24);
        assert_eq!(size_of::<MappedMemoryRange>(), 40);
        assert_eq!(size_of::<CommandPoolCreateInfo>(), 24);
        assert_eq!(size_of::<CommandBufferAllocateInfo>(), 32);
        assert_eq!(size_of::<CommandBufferBeginInfo>(), 32);
        assert_eq!(size_of::<PlainCreateInfo>(), 24);
        assert_eq!(size_of::<PhysicalDeviceMemoryProperties>(), 520);
        assert_eq!(std::mem::offset_of!(PresentInfo, p_wait_semaphores), 24);
        assert_eq!(std::mem::offset_of!(PresentInfo, p_swapchains), 40);
        assert_eq!(std::mem::offset_of!(PresentInfo, p_image_indices), 48);
        assert_eq!(std::mem::offset_of!(ImageMemoryBarrier, image), 40);
        assert_eq!(std::mem::offset_of!(BufferImageCopy, image_extent), 44);
        assert_eq!(std::mem::offset_of!(SubmitInfo, p_signal_semaphores), 64);
    }

    #[test]
    fn a_request_with_no_device_fails_at_once_and_arms_nothing() {
        let started = Instant::now();
        assert_eq!(request("/nonexistent/x.png"), Err(NO_SWAPCHAIN.to_string()));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!pending());
    }
}
