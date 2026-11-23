//! ETC2/EAC served on Vulkan drivers that do not have it.
//!
//! **The failure.** NVIDIA's driver reports `textureCompressionETC2` false. The
//! engine reads that feature, logs `ETC1 0 ETC2 0`, and stops using ETC -- but
//! some of its built-in textures exist only as ETC1, so they go missing rather
//! than falling back (the plastic detail texture is the one that was noticed).
//! This module answers it by emulating the format, and `etc2.rs` is its
//! decoder.
//!
//! **What is done, on a physical device that lacks the feature and nowhere
//! else.** The feature is reported set in `vkGetPhysicalDeviceFeatures(2)` and
//! cleared again on the way into `vkCreateDevice`, so the driver is never
//! asked to enable what it lacks. Format and image-format queries for the ten
//! ETC2/EAC formats are answered from an uncompressed substitute (RGBA8 for
//! the colour formats, R16/RG16 for EAC), offering sampling and transfer
//! destination only. Images and views are created in the substitute format.
//! `vkCmdCopyBufferToImage(2)` into one is recorded as a copy from a staging
//! buffer this module owns, and the engine's blocks are decoded into that
//! staging at `vkQueueSubmit`.
//!
//! **Why at submit and not at record.** Vulkan lets an application write the
//! source buffer after recording the copy, right up to the submit, and the
//! submit is what makes those host writes visible to the device. Decoding at
//! record would read whatever happened to be in the buffer then. Decoding
//! once, at the moment the specification says the data is final, needs no
//! second pass and no guess. Secondary command buffers are followed through
//! `vkCmdExecuteCommands`, since a submit names only primaries.
//!
//! **The gate is the feature bit, not the vendor.** A driver without ETC2 that
//! is not NVIDIA's has the same problem, and an NVIDIA driver that grows the
//! feature should be left alone. `INTOXICATED_ETC_EMULATION=off` is the
//! control: the engine then sees `ETC1 0 ETC2 0` again on such a driver.
//! `=force` runs the emulation on a driver that has ETC2 natively, so decoded
//! and native output can be compared on a machine with no NVIDIA card.
//!
//! **Deliberately not handled**, and not seen to be needed: copies out of an
//! emulated image (`vkCmdCopyImageToBuffer`), blits from one, copies between an
//! emulated image and an uncompressed one of a size-compatible format, and 3D
//! ETC images. Each would act on decoded texels where the engine expects
//! blocks. A `VkImageFormatListCreateInfo` naming ETC formats is forwarded and
//! counted. Anything this module cannot serve is counted in `ETC_UNHANDLED`
//! and traced, so a non-zero count after joining a game is the thing to
//! report.
//!
//! **Not in-process hooking.** Every call here is one the engine makes through a
//! function pointer this runtime handed it, answered at that boundary -- the
//! same as the surface translation in `vk_layer.rs`. Nothing reads or writes the
//! engine's code.
//!
//! Struct offsets below are the specification's C layout on LP64, which is
//! what both the Android engine and the FreeBSD host driver use.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::etc2::{self, Layout};
use super::glcount;

const VK_SUCCESS: i32 = 0;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_FORMAT_NOT_SUPPORTED: i32 = -11;

const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_BUFFER_CREATE_INFO: u32 = 12;
const ST_PHYSICAL_DEVICE_FEATURES_2: u32 = 1_000_059_000;
const ST_IMAGE_FORMAT_LIST_CREATE_INFO: u32 = 1_000_147_000;
const ST_FORMAT_PROPERTIES_3: u32 = 1_000_360_000;

/// `VkPhysicalDeviceFeatures` is 55 `VkBool32`s; `textureCompressionETC2` is
/// the twenty-first.
const FEATURE_WORDS: usize = 55;
const ETC2_FEATURE: usize = 20;

const FORMAT_FEATURE_SAMPLED_IMAGE: u32 = 0x1;
const FORMAT_FEATURE_TRANSFER_DST: u32 = 0x8000;
const FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR: u32 = 0x1000;
/// What an emulated format offers: it can be sampled, filtered and uploaded
/// to. Not blitted, not rendered to, not stored to, and not read back.
const OFFERED_FEATURES: u32 =
    FORMAT_FEATURE_SAMPLED_IMAGE | FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR | FORMAT_FEATURE_TRANSFER_DST;

/// Image usages an emulated image may be created with. Transfer source is
/// accepted because engines routinely set it on every texture; it is not a
/// promise that a copy out of one is served.
const OFFERED_USAGE: u32 = 0x1 | 0x2 | 0x4;
const IMAGE_CREATE_MUTABLE_FORMAT: u32 = 0x8;
const IMAGE_CREATE_CUBE_COMPATIBLE: u32 = 0x10;
const IMAGE_CREATE_BLOCK_TEXEL_VIEW_COMPATIBLE: u32 = 0x80;
const OFFERED_FLAGS: u32 = IMAGE_CREATE_MUTABLE_FORMAT | IMAGE_CREATE_CUBE_COMPATIBLE;
const IMAGE_TILING_OPTIMAL: u32 = 0;
const SAMPLE_COUNT_1: u32 = 0x1;

const BUFFER_USAGE_TRANSFER_SRC: u32 = 0x1;
const MEMORY_PROPERTY_DEVICE_LOCAL: u32 = 0x1;
const MEMORY_PROPERTY_HOST_VISIBLE: u32 = 0x2;
const MEMORY_PROPERTY_HOST_COHERENT: u32 = 0x4;
const WHOLE_SIZE: u64 = u64::MAX;

/// Staging is carved out of buffers this size, one set per command buffer,
/// and released when the command buffer is reset or freed. Large enough that
/// a 2048x2048 RGBA8 mip chain fits in two; a region larger than this gets a
/// buffer of its own size.
const STAGING_CHUNK: u64 = 8 << 20;
const STAGING_ALIGN: u64 = 16;

/// Released chunks of [`STAGING_CHUNK`] are kept, still bound and mapped, for
/// the next recording that needs one. On NVIDIA a new chunk costs about 6.5 ms
/// to allocate, bind and map and a free up to a few ms, all on the render
/// thread, and the engine begins each command buffer again every frame:
/// freeing on release cost a spawn 1087 allocations and 777 frees in 5 s, over
/// a second of it (measured with DTrace). Decoding expands ETC four to eight
/// times, so a burst is large: one submit while a map loaded needed 303 MB.
///
/// Up to [`SPARE_MAX`] chunks (384 MB of host memory) are kept, so the next
/// burst finds them; once a chunk has sat unused for [`SPARE_IDLE`], the
/// spares shrink back to [`SPARE_KEEP`], one free per [`SPARE_TRIM_EVERY`] so
/// that trimming never costs a frame more than one free.
const SPARE_MAX: usize = 48;
const SPARE_KEEP: usize = 8;
const SPARE_IDLE: std::time::Duration = std::time::Duration::from_secs(30);
const SPARE_TRIM_EVERY: std::time::Duration = std::time::Duration::from_millis(200);

// ------------------------------------------------------------------ switch

/// The environment variable that turns this off or forces it on.
pub const ETC_EMULATION_ENV: &str = "INTOXICATED_ETC_EMULATION";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Emulate where the driver lacks the feature. The default.
    Auto,
    /// Never: the control.
    Off,
    /// Even where the driver has the feature: a test lever.
    Force,
}

fn parse_mode(value: Option<&str>) -> Result<Mode, String> {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("auto") | Some("on") | Some("1") => Ok(Mode::Auto),
        Some("off") | Some("0") | Some("no") | Some("false") => Ok(Mode::Off),
        Some("force") => Ok(Mode::Force),
        Some(other) => Err(other.to_string()),
    }
}

fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let raw = std::env::var(ETC_EMULATION_ENV).ok();
        parse_mode(raw.as_deref()).unwrap_or_else(|bad| {
            // Said, not swallowed: a typo that silently meant "auto" would
            // make the control look as though it had no effect.
            println!("[android] vulkan-etc: {ETC_EMULATION_ENV}={bad} is not off, auto or force; using auto");
            Mode::Auto
        })
    })
}

fn wants_emulation(mode: Mode, native_etc2: bool) -> bool {
    match mode {
        Mode::Off => false,
        Mode::Force => true,
        Mode::Auto => !native_etc2,
    }
}

fn trace(args: std::fmt::Arguments<'_>) {
    crate::android::trace(format_args!("vulkan-etc: {args}"));
}

/// Something the engine asked of an emulated image that is not served.
fn unhandled(args: std::fmt::Arguments<'_>) {
    glcount::ETC_UNHANDLED.fetch_add(1, Relaxed);
    trace(format_args!("NOT SERVED: {args}"));
}

/// The block layout behind an ETC2/EAC `VkFormat`, and the uncompressed format
/// standing in for it. sRGB stays sRGB and signed stays signed, so the
/// sampler does the same conversion it would have done on the real format.
fn substitute(format: u32) -> Option<(Layout, u32)> {
    Some(match format {
        147 => (Layout::Rgb8, 37),
        148 => (Layout::Rgb8, 43),
        149 => (Layout::Rgb8A1, 37),
        150 => (Layout::Rgb8A1, 43),
        151 => (Layout::Rgba8, 37),
        152 => (Layout::Rgba8, 43),
        153 => (Layout::R11, 70),
        154 => (Layout::R11Signed, 71),
        155 => (Layout::Rg11, 77),
        156 => (Layout::Rg11Signed, 78),
        _ => return None,
    })
}

fn request_is_offered(tiling: u32, usage: u32, flags: u32) -> bool {
    tiling == IMAGE_TILING_OPTIMAL && usage & !OFFERED_USAGE == 0 && flags & !OFFERED_FLAGS == 0
}

// ------------------------------------------------------- raw struct access

/// Read a `T` at `offset` bytes into a Vulkan structure.
///
/// # Safety
/// `base + offset .. + size_of::<T>()` must be readable.
unsafe fn get<T: Copy>(base: *const c_void, offset: usize) -> T {
    std::ptr::read_unaligned((base as *const u8).add(offset) as *const T)
}

/// # Safety
/// `base + offset .. + size_of::<T>()` must be writable.
unsafe fn put<T: Copy>(base: *mut c_void, offset: usize, value: T) {
    std::ptr::write_unaligned((base as *mut u8).add(offset) as *mut T, value)
}

/// The first structure of `s_type` in a `pNext` chain.
///
/// # Safety
/// `node` must be null or the head of a valid chain.
unsafe fn find_in_chain(mut node: *const c_void, s_type: u32) -> *const c_void {
    while !node.is_null() {
        if get::<u32>(node, 0) == s_type {
            return node;
        }
        node = get(node, 8);
    }
    std::ptr::null()
}

/// One host entry point, resolved when the engine asked for it.
struct Slot(AtomicUsize);

impl Slot {
    const fn new() -> Slot {
        Slot(AtomicUsize::new(0))
    }

    fn set(&self, p: *mut c_void) {
        self.0.store(p as usize, Relaxed);
    }

    /// The host function as `F`, which must be an `extern "C" fn` type
    /// matching the specification's prototype for the name it came from.
    fn get<F: Copy>(&self) -> Option<F> {
        fn_from(self.0.load(Relaxed))
    }
}

fn fn_from<F: Copy>(p: usize) -> Option<F> {
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<usize>());
    // SAFETY: `F` is a function pointer type the same size as `usize`, and a
    // non-zero `p` came from the Vulkan loader for the name `F` describes.
    (p != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&p) })
}

macro_rules! slots {
    ($($name:ident),* $(,)?) => { $(static $name: Slot = Slot::new();)* };
}

slots!(
    FEATURES,
    FEATURES2,
    FORMAT_PROPS,
    FORMAT_PROPS2,
    IMAGE_FORMAT_PROPS,
    IMAGE_FORMAT_PROPS2,
    CREATE_IMAGE,
    DESTROY_IMAGE,
    CREATE_IMAGE_VIEW,
    DEVICE_IMAGE_REQS,
    DEVICE_IMAGE_SPARSE_REQS,
    COPY_BUFFER_TO_IMAGE,
    COPY_BUFFER_TO_IMAGE2,
    EXECUTE_COMMANDS,
    BIND_BUFFER,
    BIND_BUFFER2,
    DESTROY_BUFFER,
    MAP,
    MAP2,
    UNMAP,
    UNMAP2,
    FREE_MEMORY,
    QUEUE_SUBMIT,
    QUEUE_SUBMIT2,
    ALLOCATE_CMDS,
    BEGIN_CMD,
    RESET_CMD,
    FREE_CMDS,
    RESET_POOL,
    DESTROY_POOL,
    DESTROY_DEVICE,
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Takes a `VkPhysicalDevice`; only reachable through the instance getter.
    Physical,
    /// Takes a `VkDevice`, or something owned by one.
    Device,
}

/// Which hook answers `name`, if any.
fn lookup(name: &[u8]) -> Option<(&'static Slot, *const (), Level)> {
    use Level::*;
    Some(match name {
        b"vkGetPhysicalDeviceFeatures" => (&FEATURES, get_features as *const (), Physical),
        b"vkGetPhysicalDeviceFeatures2" | b"vkGetPhysicalDeviceFeatures2KHR" => {
            (&FEATURES2, get_features2 as *const (), Physical)
        }
        b"vkGetPhysicalDeviceFormatProperties" => (&FORMAT_PROPS, get_format_props as *const (), Physical),
        b"vkGetPhysicalDeviceFormatProperties2" | b"vkGetPhysicalDeviceFormatProperties2KHR" => {
            (&FORMAT_PROPS2, get_format_props2 as *const (), Physical)
        }
        b"vkGetPhysicalDeviceImageFormatProperties" => {
            (&IMAGE_FORMAT_PROPS, get_image_format_props as *const (), Physical)
        }
        b"vkGetPhysicalDeviceImageFormatProperties2" | b"vkGetPhysicalDeviceImageFormatProperties2KHR" => {
            (&IMAGE_FORMAT_PROPS2, get_image_format_props2 as *const (), Physical)
        }
        b"vkCreateImage" => (&CREATE_IMAGE, create_image as *const (), Device),
        b"vkDestroyImage" => (&DESTROY_IMAGE, destroy_image as *const (), Device),
        b"vkCreateImageView" => (&CREATE_IMAGE_VIEW, create_image_view as *const (), Device),
        b"vkGetDeviceImageMemoryRequirements" | b"vkGetDeviceImageMemoryRequirementsKHR" => {
            (&DEVICE_IMAGE_REQS, device_image_reqs as *const (), Device)
        }
        b"vkGetDeviceImageSparseMemoryRequirements" | b"vkGetDeviceImageSparseMemoryRequirementsKHR" => {
            (&DEVICE_IMAGE_SPARSE_REQS, device_image_sparse_reqs as *const (), Device)
        }
        b"vkCmdCopyBufferToImage" => (&COPY_BUFFER_TO_IMAGE, copy_buffer_to_image as *const (), Device),
        b"vkCmdCopyBufferToImage2" | b"vkCmdCopyBufferToImage2KHR" => {
            (&COPY_BUFFER_TO_IMAGE2, copy_buffer_to_image2 as *const (), Device)
        }
        b"vkCmdExecuteCommands" => (&EXECUTE_COMMANDS, execute_commands as *const (), Device),
        b"vkBindBufferMemory" => (&BIND_BUFFER, bind_buffer as *const (), Device),
        b"vkBindBufferMemory2" | b"vkBindBufferMemory2KHR" => {
            (&BIND_BUFFER2, bind_buffer2 as *const (), Device)
        }
        b"vkDestroyBuffer" => (&DESTROY_BUFFER, destroy_buffer as *const (), Device),
        b"vkMapMemory" => (&MAP, map_memory as *const (), Device),
        b"vkMapMemory2" | b"vkMapMemory2KHR" => (&MAP2, map_memory2 as *const (), Device),
        b"vkUnmapMemory" => (&UNMAP, unmap_memory as *const (), Device),
        b"vkUnmapMemory2" | b"vkUnmapMemory2KHR" => (&UNMAP2, unmap_memory2 as *const (), Device),
        b"vkFreeMemory" => (&FREE_MEMORY, free_memory as *const (), Device),
        b"vkQueueSubmit" => (&QUEUE_SUBMIT, queue_submit as *const (), Device),
        b"vkQueueSubmit2" | b"vkQueueSubmit2KHR" => (&QUEUE_SUBMIT2, queue_submit2 as *const (), Device),
        b"vkAllocateCommandBuffers" => (&ALLOCATE_CMDS, allocate_cmds as *const (), Device),
        b"vkBeginCommandBuffer" => (&BEGIN_CMD, begin_cmd as *const (), Device),
        b"vkResetCommandBuffer" => (&RESET_CMD, reset_cmd as *const (), Device),
        b"vkFreeCommandBuffers" => (&FREE_CMDS, free_cmds as *const (), Device),
        b"vkResetCommandPool" => (&RESET_POOL, reset_pool as *const (), Device),
        b"vkDestroyCommandPool" => (&DESTROY_POOL, destroy_pool as *const (), Device),
        b"vkDestroyDevice" => (&DESTROY_DEVICE, destroy_device as *const (), Device),
        _ => return None,
    })
}

// -------------------------------------------------------------- the gate

/// Whether one physical device is emulated, decided once per handle.
///
/// Decided, and said, even when the switch is off, so that a run with the
/// control set still names the device that would have needed it.
fn physical_device_emulates(pd: *mut c_void) -> bool {
    if pd.is_null() {
        return false;
    }
    static DECIDED: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
    let mut decided = DECIDED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, e)) = decided.iter().find(|(p, _)| *p == pd as usize) {
        return e;
    }
    let native = host_feature_words(pd).map(|w| w[ETC2_FEATURE] != 0);
    let emulate = native.is_some_and(|n| wants_emulation(mode(), n));
    let name = super::vk_layer::device_name(pd);
    match (native, emulate) {
        (None, _) => trace(format_args!("{name}: features could not be read; not emulating")),
        // Per physical device, so a machine with a software rasteriser beside
        // the real GPU gets a line for each; the "rendering on" line from
        // `vkCreateDevice` says which one the engine actually used.
        (Some(false), true) => println!(
            "[android] vulkan-etc: \"{name}\" has no textureCompressionETC2; ETC2/EAC will be reported \
             on it and decoded on the CPU ({ETC_EMULATION_ENV}=off to stop)"
        ),
        (Some(true), true) => println!(
            "[android] vulkan-etc: \"{name}\" has ETC2 natively; decoding it on the CPU anyway \
             ({ETC_EMULATION_ENV}=force, test only)"
        ),
        (Some(false), false) => println!(
            "[android] vulkan-etc: \"{name}\" has no textureCompressionETC2 and emulation is off \
             ({ETC_EMULATION_ENV}=off); the engine will see ETC2 unsupported"
        ),
        (Some(true), false) => trace(format_args!("{name}: native ETC2, nothing to do")),
    }
    decided.push((pd as usize, emulate));
    emulate
}

fn host_feature_words(pd: *mut c_void) -> Option<[u32; FEATURE_WORDS]> {
    let f: extern "C" fn(*mut c_void, *mut u32) =
        fn_from(super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceFeatures") as usize)?;
    let mut words = [0u32; FEATURE_WORDS];
    f(pd, words.as_mut_ptr());
    Some(words)
}

/// Whether any physical device of `instance` would be emulated. Decides
/// whether the instance getter hands out hooks at all, so a machine whose only
/// GPU has ETC2 natively resolves every one of these names straight to the
/// driver, exactly as before this module existed.
fn instance_may_emulate(instance: *mut c_void) -> bool {
    if instance.is_null() {
        return false;
    }
    static DECIDED: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
    if let Some(&(_, e)) =
        DECIDED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(i, _)| *i == instance as usize)
    {
        return e;
    }
    type Enumerate = extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32;
    let any =
        fn_from::<Enumerate>(super::vk_layer::host_proc(instance, c"vkEnumeratePhysicalDevices") as usize)
            .map(|f| {
                let mut n = 0u32;
                if f(instance, &mut n, std::ptr::null_mut()) < 0 {
                    return false;
                }
                let mut pds = vec![std::ptr::null_mut(); n as usize];
                if f(instance, &mut n, pds.as_mut_ptr()) < 0 {
                    return false;
                }
                pds.truncate(n as usize);
                // Every device decided, not just the first that would emulate, so
                // each gets its line in the log.
                pds.iter().fold(false, |any, &pd| physical_device_emulates(pd) | any)
            })
            .unwrap_or(false);
    DECIDED.lock().unwrap_or_else(|e| e.into_inner()).push((instance as usize, any));
    any
}

/// The device created on an emulating physical device, whether or not the
/// state to serve it could be built. Zero when there is none.
static EMULATING_DEVICE: AtomicUsize = AtomicUsize::new(0);

fn device_is_emulating(device: *mut c_void) -> bool {
    !device.is_null() && EMULATING_DEVICE.load(Relaxed) == device as usize
}

/// From `vkGetInstanceProcAddr`'s fall-through: our hook for `name`, or `None`
/// to hand out the host's own.
///
/// The instance getter cannot know which device a device-level pointer will
/// be used on, so on an instance that may emulate, the device-level hooks are
/// handed out too, and each forwards straight through for a device that is
/// not the emulating one.
pub(super) fn instance_hook(instance: *mut c_void, name: &[u8], host: *mut c_void) -> Option<*mut c_void> {
    if host.is_null() {
        return None;
    }
    let (slot, ours, _) = lookup(name)?;
    if !instance_may_emulate(instance) {
        return None;
    }
    slot.set(host);
    Some(ours as *mut c_void)
}

/// From `vkGetDeviceProcAddr`'s fall-through. A device that is not emulating
/// gets the driver's own function and never passes through this file.
pub(super) fn device_hook(device: *mut c_void, name: &[u8], host: *mut c_void) -> Option<*mut c_void> {
    if host.is_null() || !device_is_emulating(device) {
        return None;
    }
    let (slot, ours, level) = lookup(name)?;
    if level != Level::Device {
        return None;
    }
    slot.set(host);
    Some(ours as *mut c_void)
}

// ------------------------------------------------------ physical device

extern "C" fn get_features(pd: *mut c_void, out: *mut u32) {
    let Some(f) = FEATURES.get::<extern "C" fn(*mut c_void, *mut u32)>() else { return };
    f(pd, out);
    if !out.is_null() {
        report_etc2(pd, out as *mut c_void);
    }
}

extern "C" fn get_features2(pd: *mut c_void, out: *mut c_void) {
    let Some(f) = FEATURES2.get::<extern "C" fn(*mut c_void, *mut c_void)>() else { return };
    f(pd, out);
    if !out.is_null() {
        // `VkPhysicalDeviceFeatures2.features` follows sType and pNext.
        report_etc2(pd, (out as *mut u8).wrapping_add(16) as *mut c_void);
    }
}

/// Set `textureCompressionETC2` in a features structure the driver has just
/// filled, if this device is emulated.
fn report_etc2(pd: *mut c_void, features: *mut c_void) {
    // SAFETY: `features` is the driver-filled `VkPhysicalDeviceFeatures`.
    let native: u32 = unsafe { get(features, ETC2_FEATURE * 4) };
    if native == 0 && physical_device_emulates(pd) {
        // SAFETY: as above; the caller owns it and it is writable.
        unsafe { put::<u32>(features, ETC2_FEATURE * 4, 1) };
        trace(format_args!("textureCompressionETC2 reported set for {pd:p}"));
    }
}

/// What a format query says about an emulated format, given what the driver
/// said about its substitute.
fn offered(substitute_optimal: u32) -> [u32; 3] {
    [0, substitute_optimal & OFFERED_FEATURES, 0]
}

extern "C" fn get_format_props(pd: *mut c_void, format: u32, out: *mut c_void) {
    let Some(f) = FORMAT_PROPS.get::<extern "C" fn(*mut c_void, u32, *mut c_void)>() else { return };
    match substitute(format).filter(|_| !out.is_null() && physical_device_emulates(pd)) {
        Some((_, sub)) => {
            f(pd, sub, out);
            // SAFETY: `out` is the caller's `VkFormatProperties`, just filled.
            unsafe { put(out, 0, offered(get::<u32>(out, 4))) };
        }
        None => f(pd, format, out),
    }
}

extern "C" fn get_format_props2(pd: *mut c_void, format: u32, out: *mut c_void) {
    let Some(f) = FORMAT_PROPS2.get::<extern "C" fn(*mut c_void, u32, *mut c_void)>() else { return };
    let Some((_, sub)) = substitute(format).filter(|_| !out.is_null() && physical_device_emulates(pd)) else {
        return f(pd, format, out);
    };
    f(pd, sub, out);
    // SAFETY: `out` is the caller's `VkFormatProperties2`, just filled; its
    // `formatProperties` follows sType and pNext, and a
    // `VkFormatProperties3` in its chain has three 64-bit masks after them.
    unsafe {
        put(out, 16, offered(get::<u32>(out, 20)));
        let p3 = find_in_chain(get(out, 8), ST_FORMAT_PROPERTIES_3) as *mut c_void;
        if !p3.is_null() {
            let optimal = get::<u64>(p3, 24) & u64::from(OFFERED_FEATURES);
            put::<[u64; 3]>(p3, 16, [0, optimal, 0]);
        }
    }
}

type ImageFormatPropsFn = extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, *mut c_void) -> i32;

extern "C" fn get_image_format_props(
    pd: *mut c_void,
    format: u32,
    kind: u32,
    tiling: u32,
    usage: u32,
    flags: u32,
    out: *mut c_void,
) -> i32 {
    let Some(f) = IMAGE_FORMAT_PROPS.get::<ImageFormatPropsFn>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some((_, sub)) = substitute(format).filter(|_| physical_device_emulates(pd)) else {
        return f(pd, format, kind, tiling, usage, flags, out);
    };
    if !request_is_offered(tiling, usage, flags) {
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    let rc = f(pd, sub, kind, tiling, usage, flags, out);
    if rc == VK_SUCCESS && !out.is_null() {
        // SAFETY: `out` is the caller's `VkImageFormatProperties`, just
        // filled. Compressed formats are single-sample, and so are these.
        unsafe { put::<u32>(out, 20, SAMPLE_COUNT_1) };
    }
    rc
}

extern "C" fn get_image_format_props2(pd: *mut c_void, info: *const c_void, out: *mut c_void) -> i32 {
    let Some(f) = IMAGE_FORMAT_PROPS2.get::<extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> i32>()
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() {
        return f(pd, info, out);
    }
    // SAFETY: `info` is the caller's `VkPhysicalDeviceImageFormatInfo2`:
    // format, type, tiling, usage and flags follow sType and pNext.
    let (format, tiling, usage, flags): (u32, u32, u32, u32) =
        unsafe { (get(info, 16), get(info, 24), get(info, 28), get(info, 32)) };
    let Some((_, sub)) = substitute(format).filter(|_| physical_device_emulates(pd)) else {
        return f(pd, info, out);
    };
    if !request_is_offered(tiling, usage, flags) {
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    // The 40-byte head copied with the format swapped; its pNext still
    // points at the caller's own chain, which is only read.
    // SAFETY: `info` is at least the 40 bytes of the structure's head.
    let mut copy: [u64; 5] = unsafe { get(info, 0) };
    // SAFETY: `copy` is 40 bytes; the format sits at 16.
    unsafe { put(copy.as_mut_ptr() as *mut c_void, 16, sub) };
    let rc = f(pd, copy.as_ptr() as *const c_void, out);
    if rc == VK_SUCCESS && !out.is_null() {
        // SAFETY: `out` is the caller's `VkImageFormatProperties2`; the
        // properties follow sType and pNext, sampleCounts 20 bytes in.
        unsafe { put::<u32>(out, 16 + 20, SAMPLE_COUNT_1) };
    }
    rc
}

// --------------------------------------------------------- vkCreateDevice

/// A `VkDeviceCreateInfo` with `textureCompressionETC2` taken out, for the
/// length of one `vkCreateDevice`.
///
/// `pEnabledFeatures` is copied, because it is a `const` pointer the engine
/// may well have aimed at read-only data. A `VkPhysicalDeviceFeatures2` in the
/// `pNext` chain cannot be copied the same way unless it happens to be first:
/// relinking around it means writing the node before it, which is the
/// engine's memory either way. So that one word is cleared where it lies and
/// put back by `Drop`, which runs before `vkCreateDevice` returns to the
/// engine. A chain is built at run time, so it is writable; a driver asked to
/// enable a feature it lacks fails device creation outright, which is the one
/// outcome this cannot be allowed to produce.
pub(super) struct StrippedDeviceInfo {
    info: Box<[u64; 9]>,
    features: Option<Box<[u32; FEATURE_WORDS]>>,
    restore: Option<usize>,
}

impl StrippedDeviceInfo {
    pub(super) fn as_ptr(&self) -> *const c_void {
        self.info.as_ptr() as *const c_void
    }
}

impl Drop for StrippedDeviceInfo {
    fn drop(&mut self) {
        if let Some(at) = self.restore {
            // SAFETY: `at` is the ETC2 word of the caller's Features2, cleared
            // by `strip` within the same `vkCreateDevice` call.
            unsafe { std::ptr::write_unaligned(at as *mut u32, 1) };
        }
    }
}

pub(super) fn strip_device_features(pd: *mut c_void, info: *const c_void) -> Option<StrippedDeviceInfo> {
    if info.is_null() || !physical_device_emulates(pd) {
        return None;
    }
    // SAFETY: `info` is the engine's own `VkDeviceCreateInfo`.
    unsafe { strip(info) }
}

/// # Safety
/// `info` must be a valid `VkDeviceCreateInfo`.
unsafe fn strip(info: *const c_void) -> Option<StrippedDeviceInfo> {
    let enabled: *const c_void = get(info, 64);
    let features2 = find_in_chain(get(info, 8), ST_PHYSICAL_DEVICE_FEATURES_2);
    let in_enabled = !enabled.is_null() && get::<u32>(enabled, ETC2_FEATURE * 4) != 0;
    let at2 = (features2 as usize).wrapping_add(16 + ETC2_FEATURE * 4);
    let in_features2 = !features2.is_null() && std::ptr::read_unaligned(at2 as *const u32) != 0;
    if !in_enabled && !in_features2 {
        return None;
    }
    let mut stripped = StrippedDeviceInfo { info: Box::new(get(info, 0)), features: None, restore: None };
    if in_enabled {
        let mut copy: Box<[u32; FEATURE_WORDS]> = Box::new(get(enabled, 0));
        copy[ETC2_FEATURE] = 0;
        put(stripped.info.as_mut_ptr() as *mut c_void, 64, copy.as_ptr());
        stripped.features = Some(copy);
    }
    if in_features2 {
        std::ptr::write_unaligned(at2 as *mut u32, 0);
        stripped.restore = Some(at2);
    }
    trace(format_args!(
        "vkCreateDevice: textureCompressionETC2 withheld from the driver (pEnabledFeatures {in_enabled}, Features2 {in_features2})"
    ));
    Some(stripped)
}

// ------------------------------------------------------------ device state

/// The host calls staging needs, resolved straight from the driver so that
/// this module's own buffers never pass through its own hooks.
struct Fns {
    create_buffer: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
    destroy_buffer: extern "C" fn(usize, u64, *const c_void),
    buffer_requirements: extern "C" fn(usize, u64, *mut [u64; 3]),
    allocate_memory: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
    free_memory: extern "C" fn(usize, u64, *const c_void),
    bind_buffer_memory: extern "C" fn(usize, u64, u64, u64) -> i32,
    map_memory: extern "C" fn(usize, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    unmap_memory: extern "C" fn(usize, u64),
    copy_buffer_to_image: extern "C" fn(usize, u64, u64, i32, u32, *const BufferImageCopy),
}

#[derive(Clone, Copy)]
struct Image {
    layout: Layout,
    format: u32,
    substitute: u32,
}

#[derive(Clone, Copy)]
struct Mapping {
    ptr: usize,
    offset: u64,
    size: u64,
}

struct Chunk {
    buffer: u64,
    memory: u64,
    ptr: usize,
    size: u64,
    used: u64,
}

/// One region's worth of decoding, to be done at submit.
struct Upload {
    memory: u64,
    offset: u64,
    plan: Plan,
    /// Where in staging the texels go.
    dst: usize,
    /// The source as last decoded, so a command buffer submitted again with
    /// its source untouched is not decoded twice.
    decoded: Option<u64>,
}

#[derive(Default)]
struct Recording {
    chunks: Vec<Chunk>,
    uploads: Vec<Upload>,
    /// Secondary command buffers this one executes that have uploads of
    /// their own; a submit names only the primary.
    executes: Vec<usize>,
}

struct Emu {
    device: usize,
    fns: Fns,
    memory_types: Vec<u32>,
    images: HashMap<u64, Image>,
    /// Buffer to (memory, offset of the buffer in it).
    buffers: HashMap<u64, (u64, u64)>,
    mapped: HashMap<u64, Mapping>,
    pool_of: HashMap<usize, u64>,
    recordings: HashMap<usize, Recording>,
    /// Idle chunks for reuse, oldest first, each with when it went idle;
    /// see [`SPARE_MAX`].
    spare: Vec<(std::time::Instant, Chunk)>,
    /// When a spare chunk was last freed by trimming.
    trimmed: Option<std::time::Instant>,
}

static EMU: Mutex<Option<Emu>> = Mutex::new(None);

/// How many images and recordings exist, so that the per-frame hooks on a
/// device that is emulating but has nothing in flight skip the lock.
static LIVE_IMAGES: AtomicUsize = AtomicUsize::new(0);
static LIVE_RECORDINGS: AtomicUsize = AtomicUsize::new(0);

fn emu() -> MutexGuard<'static, Option<Emu>> {
    EMU.lock().unwrap_or_else(|e| e.into_inner())
}

/// After the host `vkCreateDevice` succeeded: if this device's physical device
/// is emulated, build the state that serves it.
pub(super) fn device_created(pd: *mut c_void, device: *mut c_void) {
    if device.is_null() || !physical_device_emulates(pd) {
        return;
    }
    let mut slot = emu();
    if let Some(old) = slot.as_ref() {
        // One emulating device at a time; the engine has only ever been seen
        // to create one. A second is left unserved rather than handed half of
        // the first one's state, and this line is how a log would show it.
        unhandled(format_args!(
            "a second emulating device {device:p} while 0x{:x} is live; it is not served",
            old.device
        ));
        return;
    }
    EMULATING_DEVICE.store(device as usize, Relaxed);
    match build_emu(pd, device) {
        Some(e) => {
            trace(format_args!("device {device:p}: ETC2/EAC emulation ready"));
            *slot = Some(e);
        }
        None => unhandled(format_args!(
            "device {device:p}: the host calls staging needs could not be resolved; ETC images will be refused"
        )),
    }
}

fn build_emu(pd: *mut c_void, device: *mut c_void) -> Option<Emu> {
    let gdpa: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
        fn_from(super::vk_layer::host_instance_proc(c"vkGetDeviceProcAddr") as usize)?;
    let resolve = |name: &CStr| gdpa(device, name.as_ptr()) as usize;
    let memory_properties: extern "C" fn(*mut c_void, *mut c_void) =
        fn_from(super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties") as usize)?;
    // Read, never altered: only to choose a host-visible type for staging.
    // 520 bytes; `u64`s because the heap sizes in it are, and a driver may
    // store them aligned.
    let mut props = [0u64; 65];
    memory_properties(pd, props.as_mut_ptr() as *mut c_void);
    let p = props.as_ptr() as *const c_void;
    // SAFETY: memoryTypeCount at 0, then 32 (propertyFlags, heapIndex) pairs.
    let count = (unsafe { get::<u32>(p, 0) } as usize).min(32);
    let memory_types = (0..count).map(|i| unsafe { get::<u32>(p, 4 + i * 8) }).collect();
    Some(Emu {
        device: device as usize,
        fns: Fns {
            create_buffer: fn_from(resolve(c"vkCreateBuffer"))?,
            destroy_buffer: fn_from(resolve(c"vkDestroyBuffer"))?,
            buffer_requirements: fn_from(resolve(c"vkGetBufferMemoryRequirements"))?,
            allocate_memory: fn_from(resolve(c"vkAllocateMemory"))?,
            free_memory: fn_from(resolve(c"vkFreeMemory"))?,
            bind_buffer_memory: fn_from(resolve(c"vkBindBufferMemory"))?,
            map_memory: fn_from(resolve(c"vkMapMemory"))?,
            unmap_memory: fn_from(resolve(c"vkUnmapMemory"))?,
            copy_buffer_to_image: fn_from(resolve(c"vkCmdCopyBufferToImage"))?,
        },
        memory_types,
        images: HashMap::new(),
        buffers: HashMap::new(),
        mapped: HashMap::new(),
        pool_of: HashMap::new(),
        spare: Vec::new(),
        trimmed: None,
        recordings: HashMap::new(),
    })
}

impl Emu {
    fn release(&mut self, cmd: usize) {
        if let Some(r) = self.recordings.remove(&cmd) {
            LIVE_RECORDINGS.fetch_sub(1, Relaxed);
            let now = std::time::Instant::now();
            for c in r.chunks {
                if c.size == STAGING_CHUNK && self.spare.len() < SPARE_MAX {
                    self.spare.push((now, c));
                } else {
                    self.destroy_chunk(c);
                }
            }
        }
        self.trim_spare();
    }

    /// Free the oldest spare chunk if the spares are above [`SPARE_KEEP`], it
    /// has been idle for [`SPARE_IDLE`] and nothing was trimmed in the last
    /// [`SPARE_TRIM_EVERY`].
    fn trim_spare(&mut self) {
        let Some(&(idle_since, _)) = self.spare.first() else { return };
        let now = std::time::Instant::now();
        if self.spare.len() <= SPARE_KEEP
            || now.duration_since(idle_since) < SPARE_IDLE
            || self.trimmed.is_some_and(|t| now.duration_since(t) < SPARE_TRIM_EVERY)
        {
            return;
        }
        let (_, c) = self.spare.remove(0);
        self.destroy_chunk(c);
        self.trimmed = Some(now);
    }

    fn destroy_chunk(&self, c: Chunk) {
        (self.fns.destroy_buffer)(self.device, c.buffer, std::ptr::null());
        (self.fns.free_memory)(self.device, c.memory, std::ptr::null());
    }

    fn release_all(&mut self) {
        let cmds: Vec<usize> = self.recordings.keys().copied().collect();
        for c in cmds {
            self.release(c);
        }
        for (_, c) in std::mem::take(&mut self.spare) {
            self.destroy_chunk(c);
        }
        LIVE_IMAGES.fetch_sub(self.images.len(), Relaxed);
        self.images.clear();
    }

    fn recording(&mut self, cmd: usize) -> &mut Recording {
        self.recordings.entry(cmd).or_insert_with(|| {
            LIVE_RECORDINGS.fetch_add(1, Relaxed);
            Recording::default()
        })
    }

    /// `len` bytes of host-visible staging owned by `cmd`, as (buffer, offset
    /// in it, host address).
    fn staging(&mut self, cmd: usize, len: u64) -> Result<(u64, u64, usize), String> {
        if let Some(c) = self.recordings.get_mut(&cmd).and_then(|r| r.chunks.last_mut()) {
            let at = c.used.div_ceil(STAGING_ALIGN) * STAGING_ALIGN;
            if at + len <= c.size {
                c.used = at + len;
                return Ok((c.buffer, at, c.ptr + at as usize));
            }
        }
        if len <= STAGING_CHUNK {
            // The most recently idle, so the oldest age out.
            if let Some((_, mut c)) = self.spare.pop() {
                c.used = len;
                let reused = (c.buffer, 0, c.ptr);
                self.recording(cmd).chunks.push(c);
                return Ok(reused);
            }
        }
        let (device, size) = (self.device, len.max(STAGING_CHUNK));
        let mut info = [0u64; 7];
        // SAFETY: `info` is the 56 bytes of a `VkBufferCreateInfo`.
        unsafe {
            let p = info.as_mut_ptr() as *mut c_void;
            put(p, 0, ST_BUFFER_CREATE_INFO);
            put(p, 24, size);
            put(p, 32, BUFFER_USAGE_TRANSFER_SRC);
        }
        let mut buffer = 0u64;
        let rc =
            (self.fns.create_buffer)(device, info.as_ptr() as *const c_void, std::ptr::null(), &mut buffer);
        if rc != VK_SUCCESS {
            return Err(format!("staging vkCreateBuffer({size}) returned {rc}"));
        }
        let mut req = [0u64; 3];
        (self.fns.buffer_requirements)(device, buffer, &mut req);
        let Some(type_index) = staging_memory_type(&self.memory_types, req[2] as u32) else {
            (self.fns.destroy_buffer)(device, buffer, std::ptr::null());
            return Err("no host-visible, host-coherent memory type for staging".into());
        };
        let mut alloc = [0u64; 4];
        // SAFETY: `alloc` is the 32 bytes of a `VkMemoryAllocateInfo`.
        unsafe {
            let p = alloc.as_mut_ptr() as *mut c_void;
            put(p, 0, ST_MEMORY_ALLOCATE_INFO);
            put(p, 16, req[0]);
            put(p, 24, type_index);
        }
        let mut memory = 0u64;
        let rc = (self.fns.allocate_memory)(
            device,
            alloc.as_ptr() as *const c_void,
            std::ptr::null(),
            &mut memory,
        );
        if rc != VK_SUCCESS {
            (self.fns.destroy_buffer)(device, buffer, std::ptr::null());
            return Err(format!("staging vkAllocateMemory({}) returned {rc}", req[0]));
        }
        let mut ptr = std::ptr::null_mut();
        let bound = (self.fns.bind_buffer_memory)(device, buffer, memory, 0);
        let mapped = (self.fns.map_memory)(device, memory, 0, WHOLE_SIZE, 0, &mut ptr);
        if bound != VK_SUCCESS || mapped != VK_SUCCESS || ptr.is_null() {
            (self.fns.destroy_buffer)(device, buffer, std::ptr::null());
            (self.fns.free_memory)(device, memory, std::ptr::null());
            return Err(format!("staging bind returned {bound}, map {mapped}"));
        }
        self.recording(cmd).chunks.push(Chunk { buffer, memory, ptr: ptr as usize, size, used: len });
        Ok((buffer, 0, ptr as usize))
    }
}

/// A host-visible, coherent memory type out of `allowed`, preferring one that
/// is not device-local so staging does not compete for VRAM.
fn staging_memory_type(types: &[u32], allowed: u32) -> Option<u32> {
    let want = MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT;
    let pick = |avoid: u32| {
        types
            .iter()
            .enumerate()
            .find(|&(i, &f)| allowed & (1 << i) != 0 && f & want == want && f & avoid == 0)
            .map(|(i, _)| i as u32)
    };
    pick(MEMORY_PROPERTY_DEVICE_LOCAL).or_else(|| pick(0))
}

extern "C" fn destroy_device(device: *mut c_void, alloc: *const c_void) {
    if device_is_emulating(device) {
        if let Some(mut e) = emu().take() {
            e.release_all();
        }
        EMULATING_DEVICE.store(0, Relaxed);
    }
    if let Some(f) = DESTROY_DEVICE.get::<extern "C" fn(*mut c_void, *const c_void)>() {
        f(device, alloc);
    }
}

// ---------------------------------------------------------------- images

type CreateFn = extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32;

extern "C" fn create_image(
    device: *mut c_void,
    info: *const c_void,
    alloc: *const c_void,
    out: *mut u64,
) -> i32 {
    let Some(f) = CREATE_IMAGE.get::<CreateFn>() else { return VK_ERROR_INITIALIZATION_FAILED };
    if info.is_null() || !device_is_emulating(device) {
        return f(device, info, alloc, out);
    }
    // SAFETY: `info` is the caller's 88-byte `VkImageCreateInfo`.
    let mut copy: [u64; 11] = unsafe { get(info, 0) };
    let p = copy.as_mut_ptr() as *mut c_void;
    // SAFETY: offsets within the 88-byte copy: flags 16, format 24, extent 28,
    // tiling 52, usage 56.
    let (flags, format, extent, tiling, usage): (u32, u32, [u32; 3], u32, u32) =
        unsafe { (get(p, 16), get(p, 24), get(p, 28), get(p, 52), get(p, 56)) };
    let Some((layout, sub)) = substitute(format) else { return f(device, info, alloc, out) };
    let describe = format!(
        "format {format} {}x{}x{} usage 0x{usage:x} flags 0x{flags:x}",
        extent[0], extent[1], extent[2]
    );

    let mut state = emu();
    let Some(e) = state.as_mut().filter(|e| e.device == device as usize) else {
        // The engine was told ETC2 works here and there is nothing to serve
        // it with. The raw format would be a driver asked to create what it
        // does not support; this says so by name instead.
        unhandled(format_args!("vkCreateImage({describe}): no emulation state for {device:p}; refused"));
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    };
    if !request_is_offered(tiling, usage, flags & !IMAGE_CREATE_BLOCK_TEXEL_VIEW_COMPATIBLE) {
        unhandled(format_args!("vkCreateImage({describe}): asks for more than sampling and upload"));
    }
    // SAFETY: the copy's pNext is the caller's chain, only read.
    if unsafe { !find_in_chain(get(p, 8), ST_IMAGE_FORMAT_LIST_CREATE_INFO).is_null() } {
        unhandled(format_args!("vkCreateImage({describe}): its format list is forwarded unchanged"));
    }
    // SAFETY: as above. Block-texel views are meaningless on an uncompressed
    // image and invalid on one, so the flag goes.
    unsafe {
        put(p, 24, sub);
        put(p, 16, flags & !IMAGE_CREATE_BLOCK_TEXEL_VIEW_COMPATIBLE);
    }
    let rc = f(device, copy.as_ptr() as *const c_void, alloc, out);
    if rc == VK_SUCCESS && !out.is_null() {
        // SAFETY: the driver reported success, so `out` holds the handle.
        let image = unsafe { *out };
        if e.images.insert(image, Image { layout, format, substitute: sub }).is_none() {
            LIVE_IMAGES.fetch_add(1, Relaxed);
        }
        glcount::ETC_IMAGES.fetch_add(1, Relaxed);
        trace(format_args!("vkCreateImage({describe}) -> 0x{image:x} as format {sub}"));
    } else {
        trace(format_args!("vkCreateImage({describe}) as format {sub} returned {rc}"));
    }
    rc
}

extern "C" fn destroy_image(device: *mut c_void, image: u64, alloc: *const c_void) {
    if LIVE_IMAGES.load(Relaxed) != 0 && device_is_emulating(device) {
        if let Some(e) = emu().as_mut() {
            if e.images.remove(&image).is_some() {
                LIVE_IMAGES.fetch_sub(1, Relaxed);
            }
        }
    }
    if let Some(f) = DESTROY_IMAGE.get::<extern "C" fn(*mut c_void, u64, *const c_void)>() {
        f(device, image, alloc);
    }
}

fn emulated_image(image: u64) -> Option<Image> {
    if LIVE_IMAGES.load(Relaxed) == 0 {
        return None;
    }
    emu().as_ref()?.images.get(&image).copied()
}

extern "C" fn create_image_view(
    device: *mut c_void,
    info: *const c_void,
    alloc: *const c_void,
    out: *mut u64,
) -> i32 {
    let Some(f) = CREATE_IMAGE_VIEW.get::<CreateFn>() else { return VK_ERROR_INITIALIZATION_FAILED };
    if info.is_null() || !device_is_emulating(device) {
        return f(device, info, alloc, out);
    }
    // SAFETY: `info` is the caller's 80-byte `VkImageViewCreateInfo`: image
    // at 24, format at 36.
    let (image, format): (u64, u32) = unsafe { (get(info, 24), get(info, 36)) };
    let Some(img) = emulated_image(image) else { return f(device, info, alloc, out) };
    match substitute(format) {
        Some((_, sub)) => {
            // SAFETY: as above.
            let mut copy: [u64; 10] = unsafe { get(info, 0) };
            // SAFETY: the format is at 36 of the 80-byte copy.
            unsafe { put(copy.as_mut_ptr() as *mut c_void, 36, sub) };
            f(device, copy.as_ptr() as *const c_void, alloc, out)
        }
        None => {
            if format != img.substitute {
                unhandled(format_args!(
                    "vkCreateImageView of ETC image 0x{image:x} (format {}) as format {format}; forwarded",
                    img.format
                ));
            }
            f(device, info, alloc, out)
        }
    }
}

/// A `VkDeviceImageMemoryRequirements` with its `VkImageCreateInfo` copied
/// and the format substituted, so the driver sizes the image it will actually
/// be given. `None` when the image is not an ETC one.
///
/// # Safety
/// `info` must be null or a valid `VkDeviceImageMemoryRequirements`.
unsafe fn substituted_requirements(info: *const c_void) -> Option<Box<([u64; 4], [u64; 11])>> {
    if info.is_null() {
        return None;
    }
    let create: *const c_void = get(info, 16);
    if create.is_null() {
        return None;
    }
    let (_, sub) = substitute(get(create, 24))?;
    let mut both = Box::new((get::<[u64; 4]>(info, 0), get::<[u64; 11]>(create, 0)));
    put(both.1.as_mut_ptr() as *mut c_void, 24, sub);
    let inner = both.1.as_ptr();
    put(both.0.as_mut_ptr() as *mut c_void, 16, inner);
    Some(both)
}

extern "C" fn device_image_reqs(device: *mut c_void, info: *const c_void, out: *mut c_void) {
    let Some(f) = DEVICE_IMAGE_REQS.get::<extern "C" fn(*mut c_void, *const c_void, *mut c_void)>() else {
        return;
    };
    // SAFETY: `info` is the caller's.
    match device_is_emulating(device).then(|| unsafe { substituted_requirements(info) }).flatten() {
        Some(s) => f(device, s.0.as_ptr() as *const c_void, out),
        None => f(device, info, out),
    }
}

extern "C" fn device_image_sparse_reqs(
    device: *mut c_void,
    info: *const c_void,
    count: *mut u32,
    out: *mut c_void,
) {
    let Some(f) =
        DEVICE_IMAGE_SPARSE_REQS.get::<extern "C" fn(*mut c_void, *const c_void, *mut u32, *mut c_void)>()
    else {
        return;
    };
    // SAFETY: `info` is the caller's.
    match device_is_emulating(device).then(|| unsafe { substituted_requirements(info) }).flatten() {
        Some(s) => f(device, s.0.as_ptr() as *const c_void, count, out),
        None => f(device, info, count, out),
    }
}

// ---------------------------------------------------------------- uploads

/// `VkBufferImageCopy`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct BufferImageCopy {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
    image_offset: [i32; 3],
    image_extent: [u32; 3],
}

/// The geometry of one region, in both of its forms: the blocks the engine
/// put in its buffer, and the texels that go to the substitute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    layout: Layout,
    width: usize,
    height: usize,
    layers: usize,
    /// Blocks per row in the engine's buffer, which `bufferRowLength` can make
    /// wider than the region.
    row_blocks: usize,
    /// Bytes from one layer's blocks to the next's.
    layer_stride: usize,
    /// Bytes of source the region reads, from its first block to its last.
    src_len: usize,
    /// Bytes of decoded texels, tightly packed, every layer.
    dst_len: usize,
}

fn plan(layout: Layout, r: &BufferImageCopy) -> Result<Plan, String> {
    let [w, h, d] = r.image_extent;
    if d != 1 {
        return Err(format!("a depth of {d} is not served"));
    }
    if r.layer_count == 0 || r.layer_count == u32::MAX {
        return Err(format!("layerCount {} is not served", r.layer_count));
    }
    let (w, h, layers) = (w as usize, h as usize, r.layer_count as usize);
    if w == 0 || h == 0 {
        return Err("an empty region".into());
    }
    // Zero means "as wide/tall as the region", in texels; the buffer holds
    // whole blocks either way.
    let row_texels = if r.buffer_row_length == 0 { w } else { r.buffer_row_length as usize };
    let column_texels = if r.buffer_image_height == 0 { h } else { r.buffer_image_height as usize };
    let row_blocks = row_texels.div_ceil(4);
    let bb = layout.block_bytes();
    let layer_stride = row_blocks * column_texels.div_ceil(4) * bb;
    let last_layer = ((h.div_ceil(4) - 1) * row_blocks + w.div_ceil(4)) * bb;
    Ok(Plan {
        layout,
        width: w,
        height: h,
        layers,
        row_blocks,
        layer_stride,
        src_len: (layers - 1) * layer_stride + last_layer,
        dst_len: w * h * layers * layout.texel_bytes(),
    })
}

/// Decode every layer of `plan` from `src` into `dst`.
fn run_plan(plan: &Plan, src: &[u8], dst: &mut [u8]) -> Result<(), String> {
    let per_layer = plan.width * plan.height * plan.layout.texel_bytes();
    for layer in 0..plan.layers {
        let from = src.get(layer * plan.layer_stride..).ok_or("layer outside the source")?;
        let to = &mut dst[layer * per_layer..(layer + 1) * per_layer];
        etc2::decode_rect(plan.layout, from, plan.row_blocks, plan.width, plan.height, to)?;
    }
    Ok(())
}

/// Record `regions` of a copy into an emulated image as copies out of
/// staging, and leave the decoding for submit.
fn record_upload(
    cmd: *mut c_void,
    src: u64,
    image: u64,
    img: Image,
    layout: i32,
    regions: &[BufferImageCopy],
) {
    let mut state = emu();
    let Some(e) = state.as_mut() else { return };
    let Some(&(memory, bound_at)) = e.buffers.get(&src) else {
        unhandled(format_args!(
            "copy into ETC image 0x{image:x} from buffer 0x{src:x} with no recorded memory"
        ));
        return;
    };
    let mut out = Vec::with_capacity(regions.len());
    for r in regions {
        let plan = match plan(img.layout, r) {
            Ok(p) => p,
            Err(why) => {
                unhandled(format_args!("copy into ETC image 0x{image:x}: {why}; region skipped"));
                continue;
            }
        };
        let (buffer, offset, dst) = match e.staging(cmd as usize, plan.dst_len as u64) {
            Ok(s) => s,
            Err(why) => {
                unhandled(format_args!("copy into ETC image 0x{image:x}: {why}; region skipped"));
                continue;
            }
        };
        // Not zeroed here: every upload is decoded at submit, and a region
        // that cannot be is zeroed there (`decode_job`), so it uploads black
        // rather than whatever reused staging last held. Zeroing every region
        // first wrote each one twice on the render thread.
        e.recording(cmd as usize).uploads.push(Upload {
            memory,
            offset: bound_at + r.buffer_offset,
            plan,
            dst,
            decoded: None,
        });
        out.push((
            buffer,
            BufferImageCopy { buffer_offset: offset, buffer_row_length: 0, buffer_image_height: 0, ..*r },
        ));
        trace(format_args!(
            "copy into ETC image 0x{image:x} mip {} {:?}: staged, decoding at submit",
            r.mip_level, r.image_extent
        ));
    }
    let copy = e.fns.copy_buffer_to_image;
    drop(state);
    for (buffer, r) in out {
        copy(cmd as usize, buffer, image, layout, 1, &r);
    }
}

extern "C" fn copy_buffer_to_image(
    cmd: *mut c_void,
    src: u64,
    image: u64,
    layout: i32,
    count: u32,
    regions: *const BufferImageCopy,
) {
    let Some(f) =
        COPY_BUFFER_TO_IMAGE.get::<extern "C" fn(*mut c_void, u64, u64, i32, u32, *const BufferImageCopy)>()
    else {
        return;
    };
    let Some(img) = emulated_image(image).filter(|_| !regions.is_null()) else {
        return f(cmd, src, image, layout, count, regions);
    };
    // SAFETY: `regions` is the caller's array of `count`.
    let list: Vec<BufferImageCopy> =
        (0..count as usize).map(|i| unsafe { std::ptr::read_unaligned(regions.add(i)) }).collect();
    record_upload(cmd, src, image, img, layout, &list);
}

extern "C" fn copy_buffer_to_image2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = COPY_BUFFER_TO_IMAGE2.get::<extern "C" fn(*mut c_void, *const c_void)>() else { return };
    if info.is_null() {
        return f(cmd, info);
    }
    // SAFETY: `info` is the caller's `VkCopyBufferToImageInfo2`: srcBuffer 16,
    // dstImage 24, layout 32, regionCount 36, pRegions 40.
    let (src, image, layout, count, regions): (u64, u64, i32, u32, *const c_void) =
        unsafe { (get(info, 16), get(info, 24), get(info, 32), get(info, 36), get(info, 40)) };
    let Some(img) = emulated_image(image).filter(|_| !regions.is_null()) else { return f(cmd, info) };
    // `VkBufferImageCopy2` is the v1 region behind sType and pNext, 72 bytes
    // each. A pNext on a region (a copy commands transform, say) is not
    // followed, and is counted if present.
    let list: Vec<BufferImageCopy> = (0..count as usize)
        .map(|i| {
            let r = (regions as *const u8).wrapping_add(i * 72) as *const c_void;
            // SAFETY: element `i` of the caller's array.
            unsafe {
                if !get::<*const c_void>(r, 8).is_null() {
                    unhandled(format_args!("vkCmdCopyBufferToImage2 region pNext ignored"));
                }
                BufferImageCopy {
                    buffer_offset: get(r, 16),
                    buffer_row_length: get(r, 24),
                    buffer_image_height: get(r, 28),
                    aspect_mask: get(r, 32),
                    mip_level: get(r, 36),
                    base_array_layer: get(r, 40),
                    layer_count: get(r, 44),
                    image_offset: get(r, 48),
                    image_extent: get(r, 60),
                }
            }
        })
        .collect();
    record_upload(cmd, src, image, img, layout, &list);
}

extern "C" fn execute_commands(cmd: *mut c_void, count: u32, secondaries: *const usize) {
    let Some(f) = EXECUTE_COMMANDS.get::<extern "C" fn(*mut c_void, u32, *const usize)>() else { return };
    if LIVE_RECORDINGS.load(Relaxed) != 0 && !secondaries.is_null() {
        let mut state = emu();
        if let Some(e) = state.as_mut() {
            // SAFETY: the caller's array of `count` handles.
            let list = unsafe { std::slice::from_raw_parts(secondaries, count as usize) };
            let carrying: Vec<usize> =
                list.iter().copied().filter(|s| e.recordings.contains_key(s)).collect();
            if !carrying.is_empty() {
                e.recording(cmd as usize).executes.extend(carrying);
            }
        }
    }
    f(cmd, count, secondaries);
}

/// Decode everything `cmds` (and the secondaries they execute) uploads, now
/// that the submit has made the engine's writes final.
/// Runs a closure when dropped, so a measurement covers every return path.
struct Defer<F: FnMut()>(F);

impl<F: FnMut()> Drop for Defer<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

fn decode_for_submit(cmds: &[usize]) {
    // Timed, because this runs synchronously on the submitting thread: a burst
    // of new textures (a menu opening, a deploy, turning to face new geometry)
    // was seen dropping the frame rate to ~1 fps, and this is the only work
    // that grows with how much ETC content arrives at once.
    let started = std::time::Instant::now();
    let regions_before = glcount::ETC_REGIONS.load(Relaxed);
    let bytes = std::cell::Cell::new(0usize);
    let _report = Defer(|| {
        let took = started.elapsed();
        if took >= std::time::Duration::from_millis(16) {
            eprintln!(
                "[etc] submit spent {:.1} ms decoding {} region(s), {:.1} MB of ETC source",
                took.as_secs_f64() * 1000.0,
                glcount::ETC_REGIONS.load(Relaxed) - regions_before,
                bytes.get() as f64 / 1_048_576.0
            );
        }
    });
    let mut state = emu();
    let Some(e) = state.as_mut() else { return };

    // Gather every upload this submit carries, primaries and the secondaries
    // they execute, before decoding any of them.
    let mut keys: Vec<(usize, usize)> = Vec::new();
    let mut todo: Vec<(usize, u32)> = cmds.iter().map(|&c| (c, 0)).collect();
    while let Some((cmd, depth)) = todo.pop() {
        let Some(rec) = e.recordings.get(&cmd) else { continue };
        // Bounded, because nested secondaries are an extension and a cycle
        // would be an engine bug this should not turn into a hang.
        if depth < 4 {
            todo.extend(rec.executes.iter().map(|&s| (s, depth + 1)));
        }
        keys.extend((0..rec.uploads.len()).map(|i| (cmd, i)));
    }
    if keys.is_empty() {
        return;
    }

    // Resolve each source to a readable address. Memory the engine has not
    // got mapped right now (it wrote, unmapped, then submitted, which is
    // allowed) is mapped here once for the whole batch: Vulkan forbids
    // mapping one allocation twice, and the per-upload map/unmap this replaces
    // paid that round trip for every region. The lock held here is the one the
    // engine's own map hooks take, so it cannot map them meanwhile.
    let mut ours: HashMap<u64, usize> = HashMap::new();
    let mut jobs: Vec<Job> = Vec::with_capacity(keys.len());
    for &(cmd, i) in &keys {
        let u = &e.recordings[&cmd].uploads[i];
        let len = u.plan.src_len;
        bytes.set(bytes.get() + len);
        let src = match e.mapped.get(&u.memory) {
            Some(m) => {
                let inside = u.offset >= m.offset
                    && (m.size == WHOLE_SIZE || u.offset + len as u64 <= m.offset.saturating_add(m.size));
                if inside {
                    Ok(m.ptr + (u.offset - m.offset) as usize)
                } else {
                    Err(format!("source {}+{len} is outside the engine's mapping", u.offset))
                }
            }
            None => match ours.get(&u.memory) {
                Some(&base) => Ok(base + u.offset as usize),
                None => {
                    let mut p = std::ptr::null_mut();
                    let rc = (e.fns.map_memory)(e.device, u.memory, 0, WHOLE_SIZE, 0, &mut p);
                    if rc != VK_SUCCESS || p.is_null() {
                        Err(format!(
                            "source memory 0x{:x} is not mapped and mapping it returned {rc}",
                            u.memory
                        ))
                    } else {
                        ours.insert(u.memory, p as usize);
                        Ok(p as usize + u.offset as usize)
                    }
                }
            },
        };
        jobs.push(Job { src, plan: u.plan, dst: u.dst, seen: u.decoded, outcome: None });
    }

    decode_jobs(&mut jobs);

    for (&(cmd, i), job) in keys.iter().zip(&jobs) {
        match &job.outcome {
            Some(Ok(hash)) => {
                if let Some(u) = e.recordings.get_mut(&cmd).and_then(|r| r.uploads.get_mut(i)) {
                    u.decoded = Some(*hash);
                }
            }
            Some(Err(why)) => unhandled(format_args!(
                "submit: an ETC region could not be decoded ({why}); it uploads black"
            )),
            None => {}
        }
    }
    for (memory, _) in ours {
        (e.fns.unmap_memory)(e.device, memory);
    }
}

/// One region of one submit, resolved and ready to decode. Addresses are
/// plain integers so a job can cross to a worker thread; every `dst` is a
/// distinct slice of staging this module owns, so no two jobs overlap.
struct Job {
    src: Result<usize, String>,
    plan: Plan,
    dst: usize,
    /// The hash the region was last decoded from, if any.
    seen: Option<u64>,
    /// `Ok(hash)` once decoded or found unchanged.
    outcome: Option<Result<u64, String>>,
}

/// Below this much source a submit decodes on the calling thread: spawning
/// workers costs more than it saves on a handful of small mips.
const PARALLEL_FROM_BYTES: usize = 256 << 10;

/// Decode `jobs`, spread over worker threads when there is enough of it.
///
/// Single-threaded, 5 MB of ETC2 RGB took ~108 ms in the decoder alone and the
/// whole submit ~280 ms (measured on a home-page load), which is the 1 fps
/// stall. Jobs are independent, so they are handed out from a shared counter
/// to at most eight workers.
fn decode_jobs(jobs: &mut [Job]) {
    let total: usize = jobs.iter().map(|j| j.plan.src_len).sum();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8).min(jobs.len());
    if workers <= 1 || total < PARALLEL_FROM_BYTES {
        jobs.iter_mut().for_each(decode_job);
        return;
    }
    let next = AtomicUsize::new(0);
    let cells: Vec<std::sync::Mutex<&mut Job>> = jobs.iter_mut().map(std::sync::Mutex::new).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Relaxed);
                let Some(cell) = cells.get(i) else { break };
                decode_job(&mut cell.lock().unwrap_or_else(|e| e.into_inner()));
            });
        }
    });
}

fn decode_job(job: &mut Job) {
    let src = match &job.src {
        Ok(p) => *p,
        Err(why) => {
            blank(job);
            job.outcome = Some(Err(why.clone()));
            return;
        }
    };
    // One sequential copy out of the engine's memory first. Host-visible
    // memory is often write-combined and uncached, where the decoder's
    // scattered eight-byte reads are the slowest access there is.
    // SAFETY: `src` was resolved inside a live mapping for `src_len` bytes, and
    // the caller holds the lock that keeps it mapped.
    let source = unsafe { std::slice::from_raw_parts(src as *const u8, job.plan.src_len) }.to_vec();
    let hash = quick_hash(&source);
    if job.seen == Some(hash) {
        job.outcome = Some(Ok(hash));
        return;
    }
    // SAFETY: `dst` is `dst_len` bytes of staging this module mapped and still
    // holds, distinct from every other job's; its command buffer has not been
    // reset.
    let dst = unsafe { std::slice::from_raw_parts_mut(job.dst as *mut u8, job.plan.dst_len) };
    let outcome = run_plan(&job.plan, &source, dst).map(|()| {
        glcount::ETC_REGIONS.fetch_add(1, Relaxed);
        hash
    });
    if outcome.is_err() {
        blank(job);
    }
    job.outcome = Some(outcome);
}

/// Zero a region that could not be decoded, so it uploads black rather than
/// what reused staging last held.
fn blank(job: &Job) {
    // SAFETY: as in `decode_job`: `dst` is `dst_len` bytes of staging this
    // module mapped and still holds, distinct from every other job's.
    unsafe { std::ptr::write_bytes(job.dst as *mut u8, 0, job.plan.dst_len) };
}

/// A fast 64-bit hash, only to notice a resubmitted command buffer whose
/// source has not changed. It needs to tell two uploads apart, not resist an
/// adversary, so SipHash's cost (the std default this replaces) bought nothing.
fn quick_hash(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = bytes.len() as u64 ^ K;
    let mut words = bytes.chunks_exact(8);
    for w in &mut words {
        let v = u64::from_le_bytes(w.try_into().expect("chunks_exact(8)"));
        h = (h ^ v).wrapping_mul(K).rotate_left(29);
    }
    for &b in words.remainder() {
        h = (h ^ u64::from(b)).wrapping_mul(K);
    }
    h ^ (h >> 32)
}

/// The command buffers named by `count` submit infos, each `stride` bytes
/// with its count and array at `at_count` and `at_array`, each element
/// `elem` bytes with the handle `handle_at` bytes in.
///
/// # Safety
/// `submits` must be the caller's array of `count` such structures.
unsafe fn submitted(
    submits: *const c_void,
    count: u32,
    layout: (usize, usize, usize, usize, usize),
) -> Vec<usize> {
    let (stride, at_count, at_array, elem, handle_at) = layout;
    let mut all = Vec::new();
    for i in 0..count as usize {
        let s = (submits as *const u8).add(i * stride) as *const c_void;
        let n: u32 = get(s, at_count);
        let list: *const c_void = get(s, at_array);
        if !list.is_null() {
            all.extend((0..n as usize).map(|j| get::<usize>(list, j * elem + handle_at)));
        }
    }
    all
}

type SubmitFn = extern "C" fn(*mut c_void, u32, *const c_void, u64) -> i32;

extern "C" fn queue_submit(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    let Some(f) = QUEUE_SUBMIT.get::<SubmitFn>() else { return VK_ERROR_INITIALIZATION_FAILED };
    if LIVE_RECORDINGS.load(Relaxed) != 0 && !submits.is_null() {
        // `VkSubmitInfo`: 72 bytes, commandBufferCount at 40, the array at 48.
        // SAFETY: the caller's array of `count`.
        decode_for_submit(&unsafe { submitted(submits, count, (72, 40, 48, 8, 0)) });
    }
    f(queue, count, submits, fence)
}

extern "C" fn queue_submit2(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    let Some(f) = QUEUE_SUBMIT2.get::<SubmitFn>() else { return VK_ERROR_INITIALIZATION_FAILED };
    if LIVE_RECORDINGS.load(Relaxed) != 0 && !submits.is_null() {
        // `VkSubmitInfo2`: 64 bytes, commandBufferInfoCount at 32, the array
        // at 40; each `VkCommandBufferSubmitInfo` is 32 bytes, handle at 16.
        // SAFETY: the caller's array of `count`.
        decode_for_submit(&unsafe { submitted(submits, count, (64, 32, 40, 32, 16)) });
    }
    f(queue, count, submits, fence)
}

// ----------------------------------------------- where the source lives

extern "C" fn bind_buffer(device: *mut c_void, buffer: u64, memory: u64, offset: u64) -> i32 {
    let Some(f) = BIND_BUFFER.get::<extern "C" fn(*mut c_void, u64, u64, u64) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, buffer, memory, offset);
    if rc == VK_SUCCESS && device_is_emulating(device) {
        if let Some(e) = emu().as_mut() {
            e.buffers.insert(buffer, (memory, offset));
        }
    }
    rc
}

extern "C" fn bind_buffer2(device: *mut c_void, count: u32, infos: *const c_void) -> i32 {
    let Some(f) = BIND_BUFFER2.get::<extern "C" fn(*mut c_void, u32, *const c_void) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, count, infos);
    if rc == VK_SUCCESS && !infos.is_null() && device_is_emulating(device) {
        if let Some(e) = emu().as_mut() {
            for i in 0..count as usize {
                // `VkBindBufferMemoryInfo`: 40 bytes, buffer 16, memory 24,
                // offset 32.
                let b = (infos as *const u8).wrapping_add(i * 40) as *const c_void;
                // SAFETY: element `i` of the caller's array.
                unsafe { e.buffers.insert(get(b, 16), (get(b, 24), get(b, 32))) };
            }
        }
    }
    rc
}

extern "C" fn destroy_buffer(device: *mut c_void, buffer: u64, alloc: *const c_void) {
    if device_is_emulating(device) {
        if let Some(e) = emu().as_mut() {
            e.buffers.remove(&buffer);
        }
    }
    if let Some(f) = DESTROY_BUFFER.get::<extern "C" fn(*mut c_void, u64, *const c_void)>() {
        f(device, buffer, alloc);
    }
}

// The map hooks hold the state lock *across* the host call, unlike every
// other hook here. `decode_for_submit` maps memory the engine has not mapped,
// and the engine mapping the same memory in between would be the driver
// refusing one of the two; serialising them on one lock is what rules it out.

extern "C" fn map_memory(
    device: *mut c_void,
    memory: u64,
    offset: u64,
    size: u64,
    flags: u32,
    out: *mut *mut c_void,
) -> i32 {
    let Some(f) = MAP.get::<extern "C" fn(*mut c_void, u64, u64, u64, u32, *mut *mut c_void) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !device_is_emulating(device) {
        return f(device, memory, offset, size, flags, out);
    }
    let mut state = emu();
    let rc = f(device, memory, offset, size, flags, out);
    if rc == VK_SUCCESS && !out.is_null() {
        if let Some(e) = state.as_mut() {
            // SAFETY: success means the driver wrote the address.
            e.mapped.insert(memory, Mapping { ptr: unsafe { *out } as usize, offset, size });
        }
    }
    rc
}

extern "C" fn map_memory2(device: *mut c_void, info: *const c_void, out: *mut *mut c_void) -> i32 {
    let Some(f) = MAP2.get::<extern "C" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() || !device_is_emulating(device) {
        return f(device, info, out);
    }
    let mut state = emu();
    let rc = f(device, info, out);
    if rc == VK_SUCCESS && !out.is_null() {
        if let Some(e) = state.as_mut() {
            // SAFETY: `VkMemoryMapInfo`: memory 24, offset 32, size 40; and
            // success means the driver wrote the address.
            unsafe {
                e.mapped.insert(
                    get(info, 24),
                    Mapping { ptr: *out as usize, offset: get(info, 32), size: get(info, 40) },
                );
            }
        }
    }
    rc
}

extern "C" fn unmap_memory(device: *mut c_void, memory: u64) {
    let Some(f) = UNMAP.get::<extern "C" fn(*mut c_void, u64)>() else { return };
    if !device_is_emulating(device) {
        return f(device, memory);
    }
    let mut state = emu();
    if let Some(e) = state.as_mut() {
        e.mapped.remove(&memory);
    }
    f(device, memory);
}

extern "C" fn unmap_memory2(device: *mut c_void, info: *const c_void) -> i32 {
    let Some(f) = UNMAP2.get::<extern "C" fn(*mut c_void, *const c_void) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() || !device_is_emulating(device) {
        return f(device, info);
    }
    let mut state = emu();
    if let Some(e) = state.as_mut() {
        // SAFETY: `VkMemoryUnmapInfo`: memory at 24.
        e.mapped.remove(&unsafe { get::<u64>(info, 24) });
    }
    f(device, info)
}

extern "C" fn free_memory(device: *mut c_void, memory: u64, alloc: *const c_void) {
    let Some(f) = FREE_MEMORY.get::<extern "C" fn(*mut c_void, u64, *const c_void)>() else { return };
    if !device_is_emulating(device) {
        return f(device, memory, alloc);
    }
    let mut state = emu();
    if let Some(e) = state.as_mut() {
        e.mapped.remove(&memory);
    }
    f(device, memory, alloc);
}

// ------------------------------------------- command buffer lifetimes
//
// Staging lives exactly as long as the recording that copies out of it. The
// engine must not reset or free a command buffer the device is still
// executing, so each of these is a point where the staging is provably idle.

extern "C" fn allocate_cmds(device: *mut c_void, info: *const c_void, out: *mut usize) -> i32 {
    let Some(f) = ALLOCATE_CMDS.get::<extern "C" fn(*mut c_void, *const c_void, *mut usize) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, info, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() && device_is_emulating(device) {
        if let Some(e) = emu().as_mut() {
            // SAFETY: `VkCommandBufferAllocateInfo`: pool 16, count 28; and
            // success means `out` holds that many handles.
            unsafe {
                let (pool, n): (u64, u32) = (get(info, 16), get(info, 28));
                for i in 0..n as usize {
                    e.pool_of.insert(*out.add(i), pool);
                }
            }
        }
    }
    rc
}

fn release(cmds: impl IntoIterator<Item = usize>) {
    if LIVE_RECORDINGS.load(Relaxed) == 0 {
        return;
    }
    if let Some(e) = emu().as_mut() {
        for c in cmds {
            e.release(c);
        }
    }
}

extern "C" fn begin_cmd(cmd: *mut c_void, info: *const c_void) -> i32 {
    let Some(f) = BEGIN_CMD.get::<extern "C" fn(*mut c_void, *const c_void) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // Beginning resets implicitly, so the old recording's staging is idle.
    release([cmd as usize]);
    f(cmd, info)
}

extern "C" fn reset_cmd(cmd: *mut c_void, flags: u32) -> i32 {
    let Some(f) = RESET_CMD.get::<extern "C" fn(*mut c_void, u32) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    release([cmd as usize]);
    f(cmd, flags)
}

extern "C" fn free_cmds(device: *mut c_void, pool: u64, count: u32, cmds: *const usize) {
    let Some(f) = FREE_CMDS.get::<extern "C" fn(*mut c_void, u64, u32, *const usize)>() else { return };
    if !cmds.is_null() && device_is_emulating(device) {
        // SAFETY: the caller's array of `count`.
        let list = unsafe { std::slice::from_raw_parts(cmds, count as usize) };
        if let Some(e) = emu().as_mut() {
            for &c in list {
                e.release(c);
                e.pool_of.remove(&c);
            }
        }
    }
    f(device, pool, count, cmds);
}

fn cmds_of(pool: u64, forget: bool) -> Vec<usize> {
    let mut state = emu();
    let Some(e) = state.as_mut() else { return Vec::new() };
    let list: Vec<usize> = e.pool_of.iter().filter(|&(_, &p)| p == pool).map(|(&c, _)| c).collect();
    if forget {
        for c in &list {
            e.pool_of.remove(c);
        }
    }
    list
}

extern "C" fn reset_pool(device: *mut c_void, pool: u64, flags: u32) -> i32 {
    let Some(f) = RESET_POOL.get::<extern "C" fn(*mut c_void, u64, u32) -> i32>() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if LIVE_RECORDINGS.load(Relaxed) != 0 && device_is_emulating(device) {
        release(cmds_of(pool, false));
    }
    f(device, pool, flags)
}

extern "C" fn destroy_pool(device: *mut c_void, pool: u64, alloc: *const c_void) {
    let Some(f) = DESTROY_POOL.get::<extern "C" fn(*mut c_void, u64, *const c_void)>() else { return };
    if device_is_emulating(device) {
        release(cmds_of(pool, true));
    }
    f(device, pool, alloc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_reads_off_auto_and_force() {
        assert_eq!(parse_mode(None), Ok(Mode::Auto));
        assert_eq!(parse_mode(Some("auto")), Ok(Mode::Auto));
        assert_eq!(parse_mode(Some(" OFF ")), Ok(Mode::Off));
        assert_eq!(parse_mode(Some("0")), Ok(Mode::Off));
        assert_eq!(parse_mode(Some("force")), Ok(Mode::Force));
        assert_eq!(parse_mode(Some("forse")), Err("forse".into()));
    }

    /// The gate is the missing feature, never the vendor; off always wins.
    #[test]
    fn emulation_follows_the_feature_bit() {
        assert!(wants_emulation(Mode::Auto, false));
        assert!(!wants_emulation(Mode::Auto, true));
        assert!(!wants_emulation(Mode::Off, false));
        assert!(wants_emulation(Mode::Force, true));
    }

    #[test]
    fn substitutes_keep_srgb_and_signedness() {
        let subs: Vec<u32> = (147..=156).map(|f| substitute(f).unwrap().1).collect();
        assert_eq!(subs, [37, 43, 37, 43, 37, 43, 70, 71, 77, 78]);
        assert!(substitute(146).is_none() && substitute(157).is_none());
    }

    #[test]
    fn only_sampling_and_upload_are_offered() {
        assert!(request_is_offered(IMAGE_TILING_OPTIMAL, 0x7, 0));
        assert!(request_is_offered(IMAGE_TILING_OPTIMAL, 0x4, IMAGE_CREATE_CUBE_COMPATIBLE));
        assert!(!request_is_offered(1, 0x4, 0), "linear tiling");
        assert!(!request_is_offered(IMAGE_TILING_OPTIMAL, 0x10, 0), "colour attachment");
        assert!(!request_is_offered(IMAGE_TILING_OPTIMAL, 0x4, IMAGE_CREATE_BLOCK_TEXEL_VIEW_COMPATIBLE));
        assert_eq!(offered(0xFFFF_FFFF), [0, OFFERED_FEATURES, 0]);
    }

    // Stand-ins for the host calls staging makes, counting allocations and
    // frees. Only `pool_*` tests use them.
    static POOL_ALLOCS: AtomicUsize = AtomicUsize::new(0);
    static POOL_FREES: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn pool_create(_: usize, _: *const c_void, _: *const c_void, out: *mut u64) -> i32 {
        // SAFETY: the caller's out-pointer.
        unsafe { *out = 1 };
        VK_SUCCESS
    }
    extern "C" fn pool_destroy(_: usize, _: u64, _: *const c_void) {}
    extern "C" fn pool_requirements(_: usize, _: u64, out: *mut [u64; 3]) {
        // SAFETY: the caller's out-pointer. Size, alignment, any memory type.
        unsafe { *out = [STAGING_CHUNK, 16, 1] };
    }
    extern "C" fn pool_allocate(_: usize, _: *const c_void, _: *const c_void, out: *mut u64) -> i32 {
        let n = POOL_ALLOCS.fetch_add(1, Relaxed) as u64 + 1;
        // SAFETY: the caller's out-pointer.
        unsafe { *out = n };
        VK_SUCCESS
    }
    extern "C" fn pool_free(_: usize, _: u64, _: *const c_void) {
        POOL_FREES.fetch_add(1, Relaxed);
    }
    extern "C" fn pool_bind(_: usize, _: u64, _: u64, _: u64) -> i32 {
        VK_SUCCESS
    }
    extern "C" fn pool_map(_: usize, _: u64, _: u64, _: u64, _: u32, out: *mut *mut c_void) -> i32 {
        // SAFETY: the caller's out-pointer; staging never writes through it.
        unsafe { *out = 0x1000 as *mut c_void };
        VK_SUCCESS
    }
    extern "C" fn pool_unmap(_: usize, _: u64) {}
    extern "C" fn pool_copy(_: usize, _: u64, _: u64, _: i32, _: u32, _: *const BufferImageCopy) {}

    fn pool_emu() -> Emu {
        Emu {
            device: 0x7001,
            fns: Fns {
                create_buffer: pool_create,
                destroy_buffer: pool_destroy,
                buffer_requirements: pool_requirements,
                allocate_memory: pool_allocate,
                free_memory: pool_free,
                bind_buffer_memory: pool_bind,
                map_memory: pool_map,
                unmap_memory: pool_unmap,
                copy_buffer_to_image: pool_copy,
            },
            memory_types: vec![MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT],
            images: HashMap::new(),
            buffers: HashMap::new(),
            mapped: HashMap::new(),
            pool_of: HashMap::new(),
            recordings: HashMap::new(),
            spare: Vec::new(),
            trimmed: None,
        }
    }

    /// Released chunks are reused instead of freed, up to [`SPARE_MAX`];
    /// an oversized one is freed; idle spares shrink to [`SPARE_KEEP`] one
    /// free per [`SPARE_TRIM_EVERY`]; destroying the device frees the rest.
    #[test]
    fn pool_reuses_released_staging_and_trims_it_when_idle() {
        let mut e = pool_emu();
        let (allocs, frees) = (|| POOL_ALLOCS.load(Relaxed), || POOL_FREES.load(Relaxed));
        let mb = 1 << 20;

        e.staging(1, mb).unwrap();
        e.staging(1, mb).unwrap();
        assert_eq!(allocs(), 1, "two regions share one chunk");
        e.release(1);
        assert_eq!((frees(), e.spare.len()), (0, 1), "released into the spares, not freed");
        e.staging(2, mb).unwrap();
        assert_eq!(allocs(), 1, "the spare is reused");
        e.release(2);

        e.staging(3, STAGING_CHUNK + 1).unwrap();
        assert_eq!(allocs(), 2);
        e.release(3);
        assert_eq!((frees(), e.spare.len()), (1, 1), "an oversized chunk is not kept");

        for cmd in 10..10 + SPARE_MAX + 2 {
            e.staging(cmd, mb).unwrap();
        }
        let before = frees();
        for cmd in 10..10 + SPARE_MAX + 2 {
            e.release(cmd);
        }
        assert_eq!((e.spare.len(), frees() - before), (SPARE_MAX, 2), "kept up to the cap");

        // Nothing is trimmed while the spares are fresh.
        e.release(999);
        assert_eq!(e.spare.len(), SPARE_MAX);
        let old = std::time::Instant::now() - SPARE_IDLE - std::time::Duration::from_secs(1);
        e.spare.iter_mut().for_each(|(t, _)| *t = old);
        e.release(999);
        e.release(999);
        assert_eq!(e.spare.len(), SPARE_MAX - 1, "one free per trim interval");
        while e.spare.len() > SPARE_KEEP {
            e.trimmed = Some(old);
            e.release(999);
        }
        e.trimmed = Some(old);
        e.release(999);
        assert_eq!(e.spare.len(), SPARE_KEEP, "never below the floor");

        let before = frees();
        e.release_all();
        assert_eq!((e.spare.len(), frees() - before), (0, SPARE_KEEP));
        assert_eq!(allocs() - frees(), 0, "everything allocated was freed");
    }

    #[test]
    fn staging_prefers_host_memory_that_is_not_vram() {
        let vram = MEMORY_PROPERTY_DEVICE_LOCAL;
        let bar = MEMORY_PROPERTY_DEVICE_LOCAL | MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT;
        let host = MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT;
        assert_eq!(staging_memory_type(&[vram, bar, host], 0b111), Some(2));
        assert_eq!(staging_memory_type(&[vram, bar, host], 0b011), Some(1));
        assert_eq!(staging_memory_type(&[vram, bar, host], 0b001), None);
    }

    fn region(extent: [u32; 3], row_length: u32, image_height: u32, layers: u32) -> BufferImageCopy {
        BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: row_length,
            buffer_image_height: image_height,
            aspect_mask: 1,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: layers,
            image_offset: [0; 3],
            image_extent: extent,
        }
    }

    /// A 6x3 two-layer region of 16-byte blocks out of a buffer laid out 12
    /// texels wide and 8 tall: three blocks a row, two rows a layer, and the
    /// second layer read only as far as its last needed block.
    #[test]
    fn a_region_plan_counts_whole_blocks_in_the_source() {
        let p = plan(Layout::Rgba8, &region([6, 3, 1], 12, 8, 2)).unwrap();
        assert_eq!((p.row_blocks, p.layer_stride), (3, 3 * 2 * 16));
        assert_eq!(p.src_len, 96 + 2 * 16);
        assert_eq!(p.dst_len, 6 * 3 * 4 * 2);
        let tight = plan(Layout::R11, &region([2, 1, 1], 0, 0, 1)).unwrap();
        assert_eq!((tight.row_blocks, tight.src_len, tight.dst_len), (1, 8, 4));
        assert!(plan(Layout::Rgb8, &region([4, 4, 2], 0, 0, 1)).is_err());
        assert!(plan(Layout::Rgb8, &region([4, 4, 1], 0, 0, u32::MAX)).is_err());
    }

    #[test]
    fn a_plan_decodes_each_layer_from_its_own_blocks() {
        let a = [0xA5, 0x0F, 0x3C, 0x1C, 0x02, 0x01, 0x80, 0x01];
        let b = [0x41, 0x00, 0xFA, 0x7F, 0x00, 0x00, 0x1F, 0xFF];
        let p = plan(Layout::Rgb8, &region([4, 4, 1], 0, 0, 2)).unwrap();
        let src = [a, b].concat();
        let mut dst = vec![0u8; p.dst_len];
        run_plan(&p, &src, &mut dst).unwrap();
        let mut one = [0u8; 64];
        etc2::decode_block(Layout::Rgb8, &b, &mut one);
        assert_eq!(&dst[64..], &one[..]);
        assert!(run_plan(&p, &src[..8], &mut dst).is_err());
    }

    fn word(buf: &mut [u64], byte: usize, v: u32) {
        let (at, shift) = (byte / 8, (byte % 8) * 8);
        buf[at] = (buf[at] & !(0xffff_ffffu64 << shift)) | (u64::from(v) << shift);
    }

    fn read(buf: &[u64], byte: usize) -> u32 {
        (buf[byte / 8] >> ((byte % 8) * 8)) as u32
    }

    #[test]
    fn the_device_create_info_is_stripped_and_the_engines_own_put_back() {
        let at = 16 + ETC2_FEATURE * 4;
        let mut features = vec![0u64; 28];
        word(&mut features, ETC2_FEATURE * 4, 1);
        let mut f2 = vec![0u64; 30];
        word(&mut f2, 0, ST_PHYSICAL_DEVICE_FEATURES_2);
        word(&mut f2, at, 1);
        // Features2 second in the chain, behind an unrelated node.
        let mut other = vec![0u64; 2];
        word(&mut other, 0, 1_000_000_001);
        other[1] = f2.as_ptr() as u64;
        let mut info = vec![0u64; 9];
        word(&mut info, 0, 3);
        info[1] = other.as_ptr() as u64;
        info[8] = features.as_ptr() as u64;

        // SAFETY: a well-formed create info built above.
        let stripped = unsafe { strip(info.as_ptr() as *const c_void) }.expect("ETC2 was requested");
        let given = stripped.as_ptr();
        // SAFETY: `given` is the stripped copy.
        let given_features: *const c_void = unsafe { get(given, 64) };
        assert_ne!(given_features, features.as_ptr() as *const c_void, "pEnabledFeatures is a copy");
        // SAFETY: the copy is 55 words.
        assert_eq!(unsafe { get::<u32>(given_features, ETC2_FEATURE * 4) }, 0);
        assert_eq!(read(&features, ETC2_FEATURE * 4), 1, "the engine's array is not written");
        assert_eq!(read(&f2, at), 0, "the driver sees Features2 without ETC2");
        drop(stripped);
        assert_eq!(read(&f2, at), 1, "and the engine gets it back");

        word(&mut features, ETC2_FEATURE * 4, 0);
        word(&mut f2, at, 0);
        // SAFETY: as above.
        assert!(
            unsafe { strip(info.as_ptr() as *const c_void) }.is_none(),
            "nothing asked, nothing stripped"
        );
    }

    #[test]
    fn a_device_image_query_is_sized_as_the_substitute() {
        let mut create = [0u64; 11];
        word(&mut create, 24, 152);
        let mut info = [0u64; 4];
        info[2] = create.as_ptr() as u64;
        // SAFETY: well-formed structures built above.
        let s = unsafe { substituted_requirements(info.as_ptr() as *const c_void) }.unwrap();
        assert_eq!(read(&s.1, 24), 43);
        assert_eq!(s.0[2], s.1.as_ptr() as u64);
        assert_eq!(read(&create, 24), 152, "the engine's create info is untouched");
        word(&mut create, 24, 37);
        // SAFETY: as above.
        assert!(unsafe { substituted_requirements(info.as_ptr() as *const c_void) }.is_none());
    }

    /// The whole path on a real driver, headless: an ETC2 RGBA8 image is
    /// created, a copy into it recorded, the engine's blocks written only
    /// *after* recording (which is legal, and is why decoding happens at
    /// submit), submitted, and the substitute read back with the driver's own
    /// copy. Needs a Vulkan GPU without ETC2 (NVIDIA, or llvmpipe), so it is
    /// ignored by default:
    ///
    /// `INTOXICATED_GRAPHICS=vulkan cargo test --release -p intoxicated-runtime --lib -- --ignored etc2_round_trip`
    ///
    /// With `INTOXICATED_ETC_EMULATION=off` it checks the control instead:
    /// the feature stays unreported.
    #[test]
    #[ignore]
    fn etc2_round_trip_on_this_gpu() {
        type Gpa = extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
        let gipa: Gpa =
            fn_from(super::super::vk_layer::get_instance_proc_addr_symbol().expect("no Vulkan") as usize)
                .unwrap();
        fn call<F: Copy>(p: *mut c_void) -> F {
            fn_from(p as usize).expect("missing entry point")
        }
        let p = |b: &mut [u64]| b.as_mut_ptr() as *mut c_void;
        let null = std::ptr::null_mut::<c_void>();

        // Instance, through this runtime's own vkCreateInstance.
        let mut app = [0u64; 6];
        let mut ici = [0u64; 8];
        unsafe {
            put(p(&mut app), 44, (1u32 << 22) | (1 << 12));
            put(p(&mut ici), 0, 1u32);
            put(p(&mut ici), 24, app.as_ptr());
        }
        let mut instance = null;
        let create_instance: extern "C" fn(*const c_void, *const c_void, *mut *mut c_void) -> i32 =
            call(gipa(null, c"vkCreateInstance".as_ptr()));
        assert_eq!(create_instance(ici.as_ptr() as *const c_void, null, &mut instance), VK_SUCCESS);
        let inst = |n: &CStr| gipa(instance, n.as_ptr());

        // The physical device: NVIDIA if there is one, else the first that lacks ETC2.
        let enumerate: extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32 =
            call(inst(c"vkEnumeratePhysicalDevices"));
        let mut n = 0u32;
        enumerate(instance, &mut n, std::ptr::null_mut());
        let mut pds = vec![null; n as usize];
        enumerate(instance, &mut n, pds.as_mut_ptr());
        let native = |pd| host_feature_words(pd).unwrap()[ETC2_FEATURE] != 0;
        let pd = *pds
            .iter()
            .find(|&&pd| super::super::vk_layer::device_name(pd).contains("NVIDIA") && !native(pd))
            .or_else(|| pds.iter().find(|&&pd| !native(pd)))
            .expect("every GPU here has ETC2 natively; nothing to test");
        eprintln!("testing on {}", super::super::vk_layer::device_name(pd));

        let get_features: extern "C" fn(*mut c_void, *mut u32) = call(inst(c"vkGetPhysicalDeviceFeatures"));
        let mut words = [0u32; FEATURE_WORDS];
        get_features(pd, words.as_mut_ptr());
        let off = mode() == Mode::Off;
        assert_eq!(words[ETC2_FEATURE], u32::from(!off), "reported exactly when emulating");
        let format_props: extern "C" fn(*mut c_void, u32, *mut [u32; 3]) =
            call(inst(c"vkGetPhysicalDeviceFormatProperties"));
        let mut fp = [0u32; 3];
        format_props(pd, 151, &mut fp);
        if !off {
            assert_eq!(
                fp[1] & OFFERED_FEATURES,
                OFFERED_FEATURES,
                "optimal tiling offers sampling and upload"
            );
        }

        // A queue that can transfer.
        let families: extern "C" fn(*mut c_void, *mut u32, *mut [u32; 6]) =
            call(inst(c"vkGetPhysicalDeviceQueueFamilyProperties"));
        let mut nf = 0u32;
        families(pd, &mut nf, std::ptr::null_mut());
        let mut fam = vec![[0u32; 6]; nf as usize];
        families(pd, &mut nf, fam.as_mut_ptr());
        let family = fam.iter().position(|f| f[0] & 0x7 != 0).unwrap() as u32;

        // A device that asks for ETC2, which the driver lacks: the strip is
        // what lets this succeed.
        let priority = 1.0f32;
        let mut qci = [0u64; 5];
        let mut enabled = [0u32; FEATURE_WORDS];
        enabled[ETC2_FEATURE] = 1;
        let mut dci = [0u64; 9];
        unsafe {
            put(p(&mut qci), 0, 2u32);
            put(p(&mut qci), 20, family);
            put(p(&mut qci), 24, 1u32);
            put(p(&mut qci), 32, &priority as *const f32);
            put(p(&mut dci), 0, 3u32);
            put(p(&mut dci), 20, 1u32);
            put(p(&mut dci), 24, qci.as_ptr());
            put(p(&mut dci), 64, enabled.as_ptr());
        }
        let create_device: extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut *mut c_void) -> i32 =
            call(inst(c"vkCreateDevice"));
        let mut device = null;
        let rc = create_device(pd, dci.as_ptr() as *const c_void, null, &mut device);
        if off {
            // Nothing stripped: the driver is asked for a feature it lacks.
            assert_ne!(rc, VK_SUCCESS, "the driver refuses ETC2 when nothing withholds it");
            eprintln!("control: ETC2 not reported, and vkCreateDevice asking for it returned {rc}");
            return;
        }
        assert_eq!(rc, VK_SUCCESS);
        assert_eq!(enabled[ETC2_FEATURE], 1, "the caller's features are left as they were");
        let gdpa: Gpa = call(inst(c"vkGetDeviceProcAddr"));
        let dev = |n: &CStr| gdpa(device, n.as_ptr());
        let get_queue: extern "C" fn(*mut c_void, u32, u32, *mut *mut c_void) =
            call(dev(c"vkGetDeviceQueue"));
        let mut queue = null;
        get_queue(device, family, 0, &mut queue);

        let memory_props: extern "C" fn(*mut c_void, *mut c_void) =
            call(inst(c"vkGetPhysicalDeviceMemoryProperties"));
        let mut mp = [0u64; 65];
        memory_props(pd, p(&mut mp));
        let types: Vec<u32> = (0..unsafe { get::<u32>(p(&mut mp), 0) } as usize)
            .map(|i| unsafe { get::<u32>(p(&mut mp), 4 + i * 8) })
            .collect();
        let allocate: extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32 =
            call(dev(c"vkAllocateMemory"));
        let alloc_for = |req: [u64; 3], host: bool| {
            let want = if host { MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT } else { 0 };
            let index =
                (0..types.len()).find(|&i| req[2] as u32 & (1 << i) != 0 && types[i] & want == want).unwrap();
            let mut mai = [0u64; 4];
            unsafe {
                put(p(&mut mai), 0, ST_MEMORY_ALLOCATE_INFO);
                put(p(&mut mai), 16, req[0]);
                put(p(&mut mai), 24, index as u32);
            }
            let mut m = 0u64;
            assert_eq!(allocate(device, mai.as_ptr() as *const c_void, null, &mut m), VK_SUCCESS);
            m
        };
        let create_buffer: extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32 =
            call(dev(c"vkCreateBuffer"));
        let buffer_reqs: extern "C" fn(*mut c_void, u64, *mut [u64; 3]) =
            call(dev(c"vkGetBufferMemoryRequirements"));
        let bind_buffer: extern "C" fn(*mut c_void, u64, u64, u64) -> i32 = call(dev(c"vkBindBufferMemory"));
        let map: extern "C" fn(*mut c_void, u64, u64, u64, u32, *mut *mut c_void) -> i32 =
            call(dev(c"vkMapMemory"));
        let host_buffer = |size: u64, usage: u32| {
            let mut bci = [0u64; 7];
            unsafe {
                put(p(&mut bci), 0, ST_BUFFER_CREATE_INFO);
                put(p(&mut bci), 24, size);
                put(p(&mut bci), 32, usage);
            }
            let mut b = 0u64;
            assert_eq!(create_buffer(device, bci.as_ptr() as *const c_void, null, &mut b), VK_SUCCESS);
            let mut req = [0u64; 3];
            buffer_reqs(device, b, &mut req);
            let m = alloc_for(req, true);
            assert_eq!(bind_buffer(device, b, m, 0), VK_SUCCESS);
            let mut ptr = null;
            assert_eq!(map(device, m, 0, WHOLE_SIZE, 0, &mut ptr), VK_SUCCESS);
            (b, ptr as *mut u8)
        };

        // The ETC2 image: 8x8, four RGBA8-with-EAC blocks.
        let mut ic = [0u64; 11];
        unsafe {
            put(p(&mut ic), 0, 14u32);
            put(p(&mut ic), 20, 1u32);
            put(p(&mut ic), 24, 151u32);
            put(p(&mut ic), 28, [8u32, 8, 1]);
            put(p(&mut ic), 40, [1u32, 1, SAMPLE_COUNT_1, IMAGE_TILING_OPTIMAL, 0x1 | 0x2 | 0x4]);
        }
        let create_image: CreateFn = call(dev(c"vkCreateImage"));
        let mut image = 0u64;
        assert_eq!(create_image(device, ic.as_ptr() as *const c_void, null, &mut image), VK_SUCCESS);
        assert!(emulated_image(image).is_some(), "the image is a substitute");
        let image_reqs: extern "C" fn(*mut c_void, u64, *mut [u64; 3]) =
            call(dev(c"vkGetImageMemoryRequirements"));
        let mut req = [0u64; 3];
        image_reqs(device, image, &mut req);
        let bind_image: extern "C" fn(*mut c_void, u64, u64, u64) -> i32 = call(dev(c"vkBindImageMemory"));
        assert_eq!(bind_image(device, image, alloc_for(req, false), 0), VK_SUCCESS);

        let (src, src_ptr) = host_buffer(64, 0x1);
        let (readback, readback_ptr) = host_buffer(8 * 8 * 4, 0x2);

        let mut pci = [0u64; 3];
        unsafe {
            put(p(&mut pci), 0, 39u32);
            put(p(&mut pci), 20, family);
        }
        let create_pool: extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32 =
            call(dev(c"vkCreateCommandPool"));
        let mut pool = 0u64;
        assert_eq!(create_pool(device, pci.as_ptr() as *const c_void, null, &mut pool), VK_SUCCESS);
        let mut cai = [0u64; 4];
        unsafe {
            put(p(&mut cai), 0, 40u32);
            put(p(&mut cai), 16, pool);
            put(p(&mut cai), 28, 1u32);
        }
        let allocate_cmds: extern "C" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32 =
            call(dev(c"vkAllocateCommandBuffers"));
        let mut cmd = null;
        assert_eq!(allocate_cmds(device, cai.as_ptr() as *const c_void, &mut cmd), VK_SUCCESS);
        let mut cbi = [0u64; 4];
        unsafe { put(p(&mut cbi), 0, 42u32) };
        let begin: extern "C" fn(*mut c_void, *const c_void) -> i32 = call(dev(c"vkBeginCommandBuffer"));
        assert_eq!(begin(cmd, cbi.as_ptr() as *const c_void), VK_SUCCESS);

        let barrier: extern "C" fn(
            *mut c_void,
            u32,
            u32,
            u32,
            u32,
            *const c_void,
            u32,
            *const c_void,
            u32,
            *const c_void,
        ) = call(dev(c"vkCmdPipelineBarrier"));
        let transition = |old: u32, new: u32, src_access: u32, dst_access: u32| {
            let mut b = [0u64; 9];
            unsafe {
                put(p(&mut b), 0, 45u32);
                put(p(&mut b), 16, [src_access, dst_access, old, new, u32::MAX, u32::MAX]);
                put(p(&mut b), 40, image);
                put(p(&mut b), 48, [1u32, 0, 1, 0, 1]);
            }
            barrier(cmd, 0x1 | 0x1000, 0x1000, 0, 0, null, 0, null, 1, b.as_ptr() as *const c_void);
        };
        let whole = BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: 0,
            buffer_image_height: 0,
            aspect_mask: 1,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
            image_offset: [0; 3],
            image_extent: [8, 8, 1],
        };
        transition(0, 7, 0, 0x1000);
        let copy_in: extern "C" fn(*mut c_void, u64, u64, i32, u32, *const BufferImageCopy) =
            call(dev(c"vkCmdCopyBufferToImage"));
        copy_in(cmd, src, image, 7, 1, &whole);
        transition(7, 6, 0x1000, 0x800);
        let copy_out: extern "C" fn(*mut c_void, u64, i32, u64, u32, *const BufferImageCopy) =
            call(dev(c"vkCmdCopyImageToBuffer"));
        copy_out(cmd, image, 6, readback, 1, &whole);
        let end: extern "C" fn(*mut c_void) -> i32 = call(dev(c"vkEndCommandBuffer"));
        assert_eq!(end(cmd), VK_SUCCESS);

        // Only now the blocks: two hand-checked ones and two arbitrary.
        let mut blocks = Vec::new();
        blocks.extend_from_slice(&[
            0x80, 0x3D, 0x7E, 0x49, 0x24, 0x92, 0x49, 0x20, 0xA5, 0x0F, 0x3C, 0x1C, 0x02, 0x01, 0x80, 0x01,
        ]);
        blocks.extend_from_slice(&[
            0xFF, 0x00, 0, 0, 0, 0, 0, 0, 0x41, 0x00, 0xFA, 0x7F, 0x00, 0x00, 0x1F, 0xFF,
        ]);
        blocks.extend((0..32u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8));
        unsafe { std::ptr::copy_nonoverlapping(blocks.as_ptr(), src_ptr, 64) };

        let mut si = [0u64; 9];
        unsafe {
            put(p(&mut si), 0, 4u32);
            put(p(&mut si), 40, 1u32);
            put(p(&mut si), 48, &cmd as *const *mut c_void);
        }
        let submit: SubmitFn = call(dev(c"vkQueueSubmit"));
        let regions_before = glcount::ETC_REGIONS.load(Relaxed);
        assert_eq!(submit(queue, 1, si.as_ptr() as *const c_void, 0), VK_SUCCESS);
        let wait: extern "C" fn(*mut c_void) -> i32 = call(dev(c"vkQueueWaitIdle"));
        assert_eq!(wait(queue), VK_SUCCESS);
        assert_eq!(glcount::ETC_REGIONS.load(Relaxed), regions_before + 1);

        let mut want = vec![0u8; 8 * 8 * 4];
        etc2::decode_rect(Layout::Rgba8, &blocks, 2, 8, 8, &mut want).unwrap();
        let got = unsafe { std::slice::from_raw_parts(readback_ptr, 8 * 8 * 4) };
        assert_eq!(got, &want[..], "what the driver holds is what the decoder produced");
        assert_eq!(&got[..4], &[162, 0, 43, 98], "texel (0,0) of the hand-checked block");
        eprintln!("round trip ok: 8x8 ETC2 RGBA8 decoded at submit and read back from the substitute");

        let destroy_pool: extern "C" fn(*mut c_void, u64, *const c_void) = call(dev(c"vkDestroyCommandPool"));
        destroy_pool(device, pool, null);
        assert_eq!(LIVE_RECORDINGS.load(Relaxed), 0, "the pool took its staging with it");
        let destroy_image_fn: extern "C" fn(*mut c_void, u64, *const c_void) = call(dev(c"vkDestroyImage"));
        destroy_image_fn(device, image, null);
        let destroy_device_fn: extern "C" fn(*mut c_void, *const c_void) = call(dev(c"vkDestroyDevice"));
        destroy_device_fn(device, null);
        let destroy_instance: extern "C" fn(*mut c_void, *const c_void) = call(inst(c"vkDestroyInstance"));
        destroy_instance(instance, null);
    }

    static CREATED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn fake_create(_: *mut c_void, _: *const c_void, _: *const c_void, _: *mut u64) -> i32 {
        CREATED.fetch_add(1, Relaxed);
        VK_SUCCESS
    }

    /// The engine was told ETC2 works on an emulating device; with no state to
    /// serve it, an ETC image is refused by name and never reaches the driver,
    /// while a device that is not emulating is passed straight through. Also
    /// checks the device-level gate, since both read `EMULATING_DEVICE`.
    #[test]
    fn an_etc_image_without_state_is_refused_and_hooks_follow_the_device() {
        CREATE_IMAGE.set(fake_create as *const () as *mut c_void);
        let (emulating, native) = (0x6001usize as *mut c_void, 0x6002usize as *mut c_void);
        EMULATING_DEVICE.store(emulating as usize, Relaxed);
        let mut create = [0u64; 11];
        word(&mut create, 24, 147);
        let info = create.as_ptr() as *const c_void;
        let before = (CREATED.load(Relaxed), glcount::ETC_UNHANDLED.load(Relaxed));
        let mut out = 0u64;
        assert_eq!(create_image(emulating, info, std::ptr::null(), &mut out), VK_ERROR_FORMAT_NOT_SUPPORTED);
        assert_eq!(CREATED.load(Relaxed), before.0, "the driver was never asked");
        assert!(glcount::ETC_UNHANDLED.load(Relaxed) > before.1);
        assert_eq!(create_image(native, info, std::ptr::null(), &mut out), VK_SUCCESS);
        assert_eq!(CREATED.load(Relaxed), before.0 + 1);

        let host = 0x10usize as *mut c_void;
        assert!(device_hook(native, b"vkCmdCopyBufferToImage", host).is_none());
        assert!(device_hook(emulating, b"vkCmdCopyBufferToImage", host).is_some());
        assert!(device_hook(emulating, b"vkGetPhysicalDeviceFeatures", host).is_none(), "not a device call");
        assert!(device_hook(emulating, b"vkCmdDraw", host).is_none());
        EMULATING_DEVICE.store(0, Relaxed);
    }
}
