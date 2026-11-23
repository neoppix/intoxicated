//! The Vulkan the engine sees: the host's loader, with a thin layer of our
//! own in front of it.
//!
//! `libroblox.so` does not link Vulkan. It `dlopen`s `libvulkan.so` (then
//! `libvulkan.so.1`) through the bionic linker, takes `vkGetInstanceProcAddr`
//! and fetches everything else through it. `symtab` registers both sonames as
//! virtual libraries whose only export is [`vk_get_instance_proc_addr`] when
//! [`get_instance_proc_addr_symbol`] says there is a host loader, and leaves
//! them out otherwise, which the engine answers by taking GLES.
//!
//! What is interposed, and why:
//!
//! * The Android surface. The engine asks for `VK_KHR_android_surface` and an
//!   Android surface; the host has `VK_KHR_xlib_surface` and makes an Xlib
//!   surface. [`vk_enumerate_instance_extension_properties`] appends
//!   `VK_KHR_android_surface` to the host's real list whenever
//!   `VK_KHR_xlib_surface` is in it, [`vk_create_instance`] enables the Xlib
//!   extension in its place, and [`vk_create_android_surface_khr`] builds
//!   `vkCreateXlibSurfaceKHR` from the client's own window,
//!   `android::x11_window::current()`: the same handles
//!   `eglCreateWindowSurface` substitutes for EGL in `native_window.rs`.
//! * `vkCreateDevice`: which GPU (the "rendering on" line), and the one ETC2
//!   feature `vulkan_etc` withholds from a driver that lacks it.
//! * `vkCreateSwapchainKHR`: the present-mode policy ([`present_mode`]), and
//!   `TRANSFER_SRC` for devctl's screenshot.
//! * `vkQueuePresentKHR`: the frame count and frame pacing, the screenshot,
//!   and a report of every non-success result.
//! * `vkAcquireNextImageKHR` -- `VK_SUBOPTIMAL_KHR` is reported as
//!   `VK_SUCCESS`; see [`vk_acquire_next_image_khr`] for the crash behind it.
//! * `vkGetPhysicalDeviceSurfacePresentModesKHR` -- asked again on NVIDIA when
//!   it fails; see [`vk_get_physical_device_surface_present_modes_khr`].
//! * ETC2/EAC, on a GPU whose driver lacks it: a fixed set of format, image
//!   and upload calls, answered by `vulkan_etc` and nowhere else.
//! * Four device-level calls for the screenshot ([`capture_hook`]).
//! * Only when `INTOXICATED_TRACE_PASSES` or dev control is on: render
//!   passes, framebuffers, the commands that begin and end a pass, draws,
//!   copies, image barriers and `vkQueueSubmit` ([`passes`]), and images and
//!   views through `vulkan_etc`'s own hooks, which then serve every device.
//!   For the frame trace and devctl's `depthshot`; each records what was
//!   created, and costs a recording command one relaxed load while nothing
//!   is watching.
//!
//! Nothing else is touched: without those switches `vkQueueSubmit` and
//! command recording go straight to the driver (ETC decodes at submit through
//! its own hook). Vulkan objects of our own are made only for a devctl
//! capture that was asked for (`frame_capture`, `depth_capture`).
//!
//! Every slot below is process-wide and last write wins. The engine builds
//! three RenderViews per session, each with its own instance, device, surface
//! and swapchain, one at a time on its render thread; the loader's entry
//! points are the same for all of them.

pub(super) mod passes;
mod present_mode;
pub(super) mod registry;

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::OnceLock;

// ------------------------------------------------------------- Vulkan ABI
//
// Values and layouts as `vulkan_core.h` defines them, LP64.

const VK_SUCCESS: i32 = 0;
const VK_INCOMPLETE: i32 = 5;
const VK_SUBOPTIMAL_KHR: i32 = 1_000_001_003;
const VK_ERROR_OUT_OF_HOST_MEMORY: i32 = -1;
const VK_ERROR_OUT_OF_DEVICE_MEMORY: i32 = -2;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_DEVICE_LOST: i32 = -4;
const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;
const VK_ERROR_SURFACE_LOST_KHR: i32 = -1_000_000_000;
const VK_ERROR_OUT_OF_DATE_KHR: i32 = -1_000_001_004;
const VK_ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT: i32 = -1_000_255_000;

const VK_PRESENT_MODE_IMMEDIATE: i32 = 0;
const VK_PRESENT_MODE_MAILBOX: i32 = 1;
const VK_PRESENT_MODE_FIFO: i32 = 2;
const VK_PRESENT_MODE_FIFO_RELAXED: i32 = 3;

const VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR: i32 = 1_000_004_000;

/// The spec version `VK_KHR_android_surface` is advertised with.
const ANDROID_SURFACE_SPEC_VERSION: u32 = 6;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct VkExtent2D {
    width: u32,
    height: u32,
}

/// `VkExtent2D` and `VkSurfaceCapabilitiesKHR`, as
/// `vkGetPhysicalDeviceSurfaceCapabilitiesKHR` fills them in.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct VkSurfaceCapabilitiesKHR {
    min_image_count: u32,
    max_image_count: u32,
    current_extent: VkExtent2D,
    min_image_extent: VkExtent2D,
    max_image_extent: VkExtent2D,
    max_image_array_layers: u32,
    supported_transforms: u32,
    current_transform: u32,
    supported_composite_alpha: u32,
    supported_usage_flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct VkSwapchainCreateInfoKHR {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    surface: u64,
    min_image_count: u32,
    image_format: i32,
    image_color_space: i32,
    image_extent: VkExtent2D,
    image_array_layers: u32,
    image_usage: u32,
    image_sharing_mode: u32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
    pre_transform: u32,
    composite_alpha: u32,
    present_mode: i32,
    clipped: u32,
    old_swapchain: u64,
}

#[repr(C)]
struct VkXlibSurfaceCreateInfoKHR {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    dpy: *mut c_void,
    window: c_ulong,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkExtensionProperties {
    extension_name: [c_char; 256],
    spec_version: u32,
}

impl VkExtensionProperties {
    fn new(name: &CStr, spec_version: u32) -> Self {
        let mut p = VkExtensionProperties { extension_name: [0; 256], spec_version };
        for (d, s) in p.extension_name.iter_mut().zip(name.to_bytes().iter().take(255)) {
            *d = *s as c_char;
        }
        p
    }

    fn name(&self) -> &CStr {
        // SAFETY: a fixed array; `from_bytes_until_nul` stops at the first
        // NUL, and a full array without one is no name.
        let bytes = unsafe { std::slice::from_raw_parts(self.extension_name.as_ptr() as *const u8, 256) };
        CStr::from_bytes_until_nul(bytes).unwrap_or(c"")
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkInstanceCreateInfo {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    p_application_info: *const c_void,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
}

// ------------------------------------------------------------- host loader

type GetInstanceProcAddr = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type CreateInstance = unsafe extern "C" fn(*const VkInstanceCreateInfo, *const c_void, *mut *mut c_void) -> i32;
type EnumerateInstanceExtensions =
    unsafe extern "C" fn(*const c_char, *mut u32, *mut VkExtensionProperties) -> i32;

struct Host {
    get_instance_proc_addr: GetInstanceProcAddr,
    create_instance: CreateInstance,
    enumerate_instance_extension_properties: EnumerateInstanceExtensions,
}

const RTLD_NOW: c_int = 2;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// The host loader, decided once per process.
fn host() -> Option<&'static Host> {
    static HOST: OnceLock<Option<Host>> = OnceLock::new();
    HOST.get_or_init(load_host).as_ref()
}

fn load_host() -> Option<Host> {
    if std::env::var_os("INTOXICATED_NO_VULKAN").is_some() {
        return None;
    }
    if !crate::graphics::choice().backend.offers_vulkan() {
        return None;
    }
    // SAFETY: literal sonames, immediate binding; the one that opens is
    // never closed.
    let lib = [c"libvulkan.so.1", c"libvulkan.so"]
        .iter()
        .map(|name| unsafe { dlopen(name.as_ptr(), RTLD_NOW) })
        .find(|h| !h.is_null())?;
    // SAFETY: an open handle and a literal name.
    let gipa = unsafe { dlsym(lib, c"vkGetInstanceProcAddr".as_ptr()) };
    if gipa.is_null() {
        return None;
    }
    // SAFETY: the loader's `vkGetInstanceProcAddr`.
    let gipa: GetInstanceProcAddr = unsafe { std::mem::transmute::<*mut c_void, GetInstanceProcAddr>(gipa) };
    // SAFETY: global commands, asked with a null instance as the
    // specification says.
    let (create, enumerate) = unsafe {
        (
            gipa(std::ptr::null_mut(), c"vkCreateInstance".as_ptr()),
            gipa(std::ptr::null_mut(), c"vkEnumerateInstanceExtensionProperties".as_ptr()),
        )
    };
    if create.is_null() || enumerate.is_null() {
        return None;
    }
    // SAFETY: each pointer is the loader's entry for exactly that name.
    unsafe {
        Some(Host {
            get_instance_proc_addr: gipa,
            create_instance: std::mem::transmute::<*mut c_void, CreateInstance>(create),
            enumerate_instance_extension_properties: std::mem::transmute::<*mut c_void, EnumerateInstanceExtensions>(
                enumerate,
            ),
        })
    }
}

/// The sonames the engine opens Vulkan by.
pub const LIBRARY_NAMES: [&str; 2] = ["libvulkan.so", "libvulkan.so.1"];

/// What `symtab` exports as `vkGetInstanceProcAddr` from [`LIBRARY_NAMES`],
/// or `None` when the engine is to find no Vulkan at all.
pub fn get_instance_proc_addr_symbol() -> Option<*mut c_void> {
    host().map(|_| vk_get_instance_proc_addr as *const () as *mut c_void)
}

/// The real surface extension `VK_KHR_android_surface` is substituted for.
fn real_surface_extension_name() -> &'static CStr {
    c"VK_KHR_xlib_surface"
}

const ANDROID_SURFACE: &CStr = c"VK_KHR_android_surface";

// ------------------------------------------------------------- slots

/// The last non-null instance the getter was asked with.
static INSTANCE: AtomicUsize = AtomicUsize::new(0);
/// The instance `vkCreateInstance` last created. Preferred over [`INSTANCE`]
/// for lookups other modules make.
static HOST_INSTANCE: AtomicUsize = AtomicUsize::new(0);
/// The physical device the engine last built a device on.
static PHYSICAL_DEVICE: AtomicUsize = AtomicUsize::new(0);
static HOST_GET_DEVICE_PROC_ADDR: AtomicUsize = AtomicUsize::new(0);
static HOST_QUEUE_PRESENT: AtomicUsize = AtomicUsize::new(0);
static HOST_CREATE_DEVICE: AtomicUsize = AtomicUsize::new(0);
static HOST_CREATE_SWAPCHAIN: AtomicUsize = AtomicUsize::new(0);

/// Record the host's entry in `slot` and hand out ours instead; a command the
/// host lacks stays missing.
fn interpose(slot: &AtomicUsize, host: *mut c_void, ours: *const ()) -> *mut c_void {
    if host.is_null() {
        return host;
    }
    slot.store(host as usize, Relaxed);
    ours as *mut c_void
}

// ------------------------------------------------------------- getters

/// `vkGetInstanceProcAddr`, as the engine sees it.
extern "C" fn vk_get_instance_proc_addr(instance: *mut c_void, name: *const c_char) -> *mut c_void {
    if !instance.is_null() {
        INSTANCE.store(instance as usize, Relaxed);
    }
    let Some(h) = host() else {
        return std::ptr::null_mut();
    };
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the engine's NUL-terminated name.
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
    // SAFETY: the host getter, called with the engine's own arguments.
    let host_entry = || unsafe { (h.get_instance_proc_addr)(instance, name) };
    match bytes {
        b"vkGetInstanceProcAddr" => vk_get_instance_proc_addr as *const () as *mut c_void,
        b"vkCreateInstance" => vk_create_instance as *const () as *mut c_void,
        b"vkEnumerateInstanceExtensionProperties" => {
            vk_enumerate_instance_extension_properties as *const () as *mut c_void
        }
        b"vkCreateAndroidSurfaceKHR" => vk_create_android_surface_khr as *const () as *mut c_void,
        b"vkGetDeviceProcAddr" => {
            interpose(&HOST_GET_DEVICE_PROC_ADDR, host_entry(), vk_get_device_proc_addr as *const ())
        }
        b"vkQueuePresentKHR" => interpose(&HOST_QUEUE_PRESENT, host_entry(), vk_queue_present_khr as *const ()),
        b"vkCreateDevice" => interpose(&HOST_CREATE_DEVICE, host_entry(), vk_create_device as *const ()),
        b"vkCreateSwapchainKHR" => {
            interpose(&HOST_CREATE_SWAPCHAIN, host_entry(), vk_create_swapchain_khr as *const ())
        }
        // One status code changed on the way back; see
        // [`vk_acquire_next_image_khr`]. Answered here as well as in the device
        // getter for the reason `vkCreateSwapchainKHR` is.
        b"vkAcquireNextImageKHR" => {
            HOST_ACQUIRE_NEXT_IMAGE.store(
                unsafe { (h.get_instance_proc_addr)(instance, name) } as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            vk_acquire_next_image_khr as *const () as *mut c_void
        }
        // Asked again on NVIDIA when it fails; forwarded at once everywhere
        // else. See [`vk_get_physical_device_surface_present_modes_khr`].
        b"vkGetPhysicalDeviceSurfacePresentModesKHR" => {
            HOST_GET_PRESENT_MODES.store(
                unsafe { (h.get_instance_proc_addr)(instance, name) } as usize,
                std::sync::atomic::Ordering::Relaxed,
            );
            vk_get_physical_device_surface_present_modes_khr as *const () as *mut c_void
        }
        // Device-level commands the screenshot needs to see, answered here as
        // well as in the device getter for the reason `vkCreateSwapchainKHR`
        // is. See [`capture_hook`].
        b"vkDestroyDevice" | b"vkDestroySwapchainKHR" | b"vkGetDeviceQueue" | b"vkGetDeviceQueue2" => {
            // SAFETY: the host getter, called with the engine's own arguments.
            let p = unsafe { (h.get_instance_proc_addr)(instance, name) };
            let next = super::vulkan_etc::instance_hook(instance, bytes, p).unwrap_or(p);
            capture_hook(bytes, next)
        }
        // Everything else is the host's own entry point. The engine may fetch
        // a device-level command through this getter rather than through
        // `vkGetDeviceProcAddr`, and gets the loader's dispatching trampoline,
        // which is correct for any device.
        //
        // The one exception is ETC2/EAC on a driver that lacks it, where
        // `vulkan_etc` answers a fixed set of names itself. It declines on any
        // instance whose GPUs all have the feature, so there this arm is the
        // plain forward it always was.
        //
        // The render pass and frame hooks of [`passes`] go in front of
        // whichever of the two that is, when they are wanted at all.
        _ => {
            // SAFETY: the host getter, called with the engine's own arguments.
            let p = unsafe { (h.get_instance_proc_addr)(instance, name) };
            passes::hook(bytes, super::vulkan_etc::instance_hook(instance, bytes, p).unwrap_or(p))
        }
    }
}

/// An instance-level command from the host, for `instance`.
pub(super) fn host_proc(instance: *mut c_void, name: &CStr) -> *mut c_void {
    match host() {
        // SAFETY: the host getter with a NUL-terminated name.
        Some(h) => unsafe { (h.get_instance_proc_addr)(instance, name.as_ptr()) },
        None => std::ptr::null_mut(),
    }
}

/// An instance-level command from the host, for the instance the engine
/// created. Null before there is one, which every caller treats as "cannot
/// ask" rather than as an answer. The
/// screenshot in `frame_capture` needs `vkGetPhysicalDeviceMemoryProperties`,
/// which a null instance does not resolve.
pub(super) fn host_instance_proc(name: &CStr) -> *mut c_void {
    let mut instance = HOST_INSTANCE.load(std::sync::atomic::Ordering::Relaxed);
    if instance == 0 {
        instance = INSTANCE.load(std::sync::atomic::Ordering::Relaxed);
    }
    if instance == 0 {
        return std::ptr::null_mut();
    }
    host_proc(instance as *mut c_void, name)
}

/// `vkGetDeviceProcAddr`, as the engine sees it: answered only once the
/// host's was recorded through the instance getter.
extern "C" fn vk_get_device_proc_addr(device: *mut c_void, name: *const c_char) -> *mut c_void {
    let f = HOST_GET_DEVICE_PROC_ADDR.load(Relaxed);
    if f == 0 || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host's `vkGetDeviceProcAddr`, recorded for exactly that name.
    let host: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void = unsafe { std::mem::transmute(f) };
    // SAFETY: the engine's NUL-terminated name.
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        // Ours again, so a device getter fetched through a device getter
        // does not step around this layer.
        b"vkGetDeviceProcAddr" => vk_get_device_proc_addr as *const () as *mut c_void,
        b"vkQueuePresentKHR" => interpose(&HOST_QUEUE_PRESENT, host(device, name), vk_queue_present_khr as *const ()),
        b"vkCreateSwapchainKHR" => {
            interpose(&HOST_CREATE_SWAPCHAIN, host(device, name), vk_create_swapchain_khr as *const ())
        }
        b"vkAcquireNextImageKHR" => {
            HOST_ACQUIRE_NEXT_IMAGE
                .store(host(device, name) as usize, std::sync::atomic::Ordering::Relaxed);
            vk_acquire_next_image_khr as *const () as *mut c_void
        }
        bytes @ (b"vkDestroyDevice" | b"vkDestroySwapchainKHR" | b"vkGetDeviceQueue" | b"vkGetDeviceQueue2") => {
            let p = host(device, name);
            let next = super::vulkan_etc::device_hook(device, bytes, p).unwrap_or(p);
            capture_hook(bytes, next)
        }
        bytes => {
            let p = host(device, name);
            passes::hook(bytes, super::vulkan_etc::device_hook(device, bytes, p).unwrap_or(p))
        }
    }
}

/// A device-level command from the host's `vkGetDeviceProcAddr`, for the
/// screenshot's own calls. Null when the getter was never resolved or the
/// driver lacks the command.
pub(super) fn host_device_proc(device: *mut c_void, name: &CStr) -> *mut c_void {
    let mut f = HOST_GET_DEVICE_PROC_ADDR.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        f = host_instance_proc(c"vkGetDeviceProcAddr") as usize;
    }
    if f == 0 || device.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host's `vkGetDeviceProcAddr`, with a NUL-terminated name.
    let host: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void = unsafe { std::mem::transmute(f) };
    host(device, name.as_ptr())
}

// ------------------------------------------------------ screenshot hooks
//
// Four device-level commands are interposed for `frame_capture` and nothing
// else: queue lookups, so a capture's command pool is made for the family of
// the queue that presents, and the two destructions, which have to wait for a
// copy still reading the image and destroy what the capture owns before the
// device goes. Each records and forwards; none changes an argument.
// `vkDestroyDevice` forwards to whatever the getter would otherwise have
// handed out, which is `vulkan_etc`'s hook on a GPU whose ETC2 is emulated.

static NEXT_DESTROY_DEVICE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HOST_DESTROY_SWAPCHAIN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HOST_GET_DEVICE_QUEUE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static HOST_GET_DEVICE_QUEUE2: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Our entry point for one of the four names, with `next` recorded as what it
/// forwards to. A command the driver lacks stays missing.
fn capture_hook(name: &[u8], next: *mut c_void) -> *mut c_void {
    if next.is_null() {
        return next;
    }
    let (slot, ours) = match name {
        b"vkDestroyDevice" => (&NEXT_DESTROY_DEVICE, vk_destroy_device as *const ()),
        b"vkDestroySwapchainKHR" => (&HOST_DESTROY_SWAPCHAIN, vk_destroy_swapchain_khr as *const ()),
        b"vkGetDeviceQueue" => (&HOST_GET_DEVICE_QUEUE, vk_get_device_queue as *const ()),
        b"vkGetDeviceQueue2" => (&HOST_GET_DEVICE_QUEUE2, vk_get_device_queue2 as *const ()),
        _ => return next,
    };
    slot.store(next as usize, std::sync::atomic::Ordering::Relaxed);
    ours as *mut c_void
}

extern "C" fn vk_destroy_device(device: *mut c_void, allocator: *const c_void) {
    super::frame_capture::device_destroying(device);
    super::depth_capture::device_destroying(device);
    passes::device_destroying(device);
    let f = NEXT_DESTROY_DEVICE.load(std::sync::atomic::Ordering::Relaxed);
    if f != 0 {
        // SAFETY: the host's (or `vulkan_etc`'s) `vkDestroyDevice`.
        let f: extern "C" fn(*mut c_void, *const c_void) = unsafe { std::mem::transmute(f) };
        f(device, allocator);
    }
}

extern "C" fn vk_destroy_swapchain_khr(device: *mut c_void, swapchain: u64, allocator: *const c_void) {
    super::frame_capture::swapchain_destroying(device, swapchain);
    let f = HOST_DESTROY_SWAPCHAIN.load(std::sync::atomic::Ordering::Relaxed);
    if f != 0 {
        // SAFETY: the host's `vkDestroySwapchainKHR`.
        let f: extern "C" fn(*mut c_void, u64, *const c_void) = unsafe { std::mem::transmute(f) };
        f(device, swapchain, allocator);
    }
}

extern "C" fn vk_get_device_queue(device: *mut c_void, family: u32, index: u32, queue: *mut *mut c_void) {
    let f = HOST_GET_DEVICE_QUEUE.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return;
    }
    // SAFETY: the host's `vkGetDeviceQueue`, with the engine's arguments.
    let f: extern "C" fn(*mut c_void, u32, u32, *mut *mut c_void) = unsafe { std::mem::transmute(f) };
    f(device, family, index, queue);
    if !queue.is_null() {
        // SAFETY: the driver has just written the handle.
        super::frame_capture::queue_obtained(device, family, unsafe { *queue });
    }
}

extern "C" fn vk_get_device_queue2(device: *mut c_void, info: *const c_void, queue: *mut *mut c_void) {
    let f = HOST_GET_DEVICE_QUEUE2.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return;
    }
    // SAFETY: the host's `vkGetDeviceQueue2`, with the engine's arguments.
    let f: extern "C" fn(*mut c_void, *const c_void, *mut *mut c_void) = unsafe { std::mem::transmute(f) };
    f(device, info, queue);
    if !queue.is_null() && !info.is_null() {
        // SAFETY: `queueFamilyIndex` is 20 bytes into `VkDeviceQueueInfo2`,
        // and the driver has just written the handle.
        let family = unsafe { std::ptr::read_unaligned((info as *const u8).add(20) as *const u32) };
        super::frame_capture::queue_obtained(device, family, unsafe { *queue });
    }
}

// ------------------------------------------------------------- instance

/// `vkEnumerateInstanceExtensionProperties`: the host's list, plus
/// `VK_KHR_android_surface` when the host can back it.
extern "C" fn vk_enumerate_instance_extension_properties(
    layer: *const c_char,
    count: *mut u32,
    props: *mut VkExtensionProperties,
) -> i32 {
    if count.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let Some(h) = host() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // SAFETY: the host's entry with the engine's layer name and our buffers.
    let mut list = match host_extension_list(|n, p| unsafe { (h.enumerate_instance_extension_properties)(layer, n, p) }) {
        Ok(list) => list,
        Err(rc) => return rc,
    };
    // Mesa and NVIDIA report this capability under its desktop name,
    // `VK_KHR_xlib_surface` (see `real_surface_extension_name`). Roblox will
    // only ever ask for the Android one, and gives up on Vulkan without it.
    if advertise_android_surface(&mut list, !layer.is_null()) {
        crate::android::trace(format_args!(
            "vkEnumerateInstanceExtensionProperties: advertising VK_KHR_android_surface (backing {})",
            real_surface_extension_name().to_string_lossy()
        ));
    }
    // SAFETY: `count` is the engine's, checked; `props` is null or an array
    // of `*count` entries, as the specification requires.
    unsafe { copy_extensions_out(&list, count, props) }
}

/// The host's whole list, by the two-call idiom. A host error is returned
/// as it is.
fn host_extension_list(
    mut ask: impl FnMut(*mut u32, *mut VkExtensionProperties) -> i32,
) -> Result<Vec<VkExtensionProperties>, i32> {
    // The list can grow between the two calls (a layer installed meanwhile);
    // ask again a few times rather than return a short one.
    for _ in 0..4 {
        let mut n = 0u32;
        let rc = ask(&mut n, std::ptr::null_mut());
        if rc != VK_SUCCESS {
            return Err(rc);
        }
        let mut list = vec![VkExtensionProperties::new(c"", 0); n as usize];
        let mut got = n;
        match ask(&mut got, list.as_mut_ptr()) {
            VK_SUCCESS => {
                list.truncate(got as usize);
                return Ok(list);
            }
            VK_INCOMPLETE => continue,
            rc => return Err(rc),
        }
    }
    Err(VK_INCOMPLETE)
}

/// Append `VK_KHR_android_surface` when the list is the instance's own (not
/// a layer's), has the Xlib surface and lacks the Android one. Whether it was
/// appended.
fn advertise_android_surface(list: &mut Vec<VkExtensionProperties>, for_layer: bool) -> bool {
    let has = |name: &CStr| list.iter().any(|p| p.name() == name);
    if for_layer || !has(real_surface_extension_name()) || has(ANDROID_SURFACE) {
        return false;
    }
    list.push(VkExtensionProperties::new(ANDROID_SURFACE, ANDROID_SURFACE_SPEC_VERSION));
    true
}

/// The second half of the two-call idiom.
///
/// SAFETY: `count` must be valid; `props`, when not null, must have room for
/// `*count` entries.
unsafe fn copy_extensions_out(list: &[VkExtensionProperties], count: *mut u32, props: *mut VkExtensionProperties) -> i32 {
    // SAFETY: per this function's contract.
    unsafe {
        if props.is_null() {
            *count = list.len() as u32;
            return VK_SUCCESS;
        }
        let n = (*count as usize).min(list.len());
        std::ptr::copy_nonoverlapping(list.as_ptr(), props, n);
        *count = n as u32;
        if n < list.len() {
            VK_INCOMPLETE
        } else {
            VK_SUCCESS
        }
    }
}

/// The engine's extension names with `VK_KHR_android_surface` replaced by
/// `VK_KHR_xlib_surface`, and whether any was. Every other name is kept as
/// it is, so a name the host lacks still fails in the host.
fn substitute_surface_extension(names: &[*const c_char]) -> (Vec<*const c_char>, bool) {
    let mut replaced = false;
    let out = names
        .iter()
        .map(|&n| {
            // SAFETY: the engine's NUL-terminated names; null is passed on.
            if !n.is_null() && unsafe { CStr::from_ptr(n) } == ANDROID_SURFACE {
                replaced = true;
                real_surface_extension_name().as_ptr()
            } else {
                n
            }
        })
        .collect();
    (out, replaced)
}

/// `vkCreateInstance`, with the Xlib surface extension enabled in place of
/// the Android one and nothing else changed.
extern "C" fn vk_create_instance(
    info: *const VkInstanceCreateInfo,
    allocator: *const c_void,
    instance_out: *mut *mut c_void,
) -> i32 {
    let Some(h) = host() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // SAFETY: the engine's own struct, or null, which the host is handed.
    let Some(engine) = (unsafe { info.as_ref() }) else {
        // SAFETY: forwarded as it came.
        return unsafe { (h.create_instance)(info, allocator, instance_out) };
    };
    let names: &[*const c_char] = if engine.enabled_extension_count == 0 || engine.pp_enabled_extension_names.is_null() {
        &[]
    } else {
        // SAFETY: `enabledExtensionCount` names, per the specification.
        unsafe { std::slice::from_raw_parts(engine.pp_enabled_extension_names, engine.enabled_extension_count as usize) }
    };
    let (rewritten, replaced) = substitute_surface_extension(names);
    crate::android::trace(format_args!(
        "vkCreateInstance: {} extension(s) requested, VK_KHR_android_surface -> VK_KHR_xlib_surface: {replaced}",
        names.len()
    ));
    let copy = VkInstanceCreateInfo { pp_enabled_extension_names: rewritten.as_ptr(), ..*engine };
    let used: *const VkInstanceCreateInfo = if replaced { &copy } else { info };
    // SAFETY: the engine's create info or our copy of it, whose name list
    // outlives the call.
    let rc = unsafe { (h.create_instance)(used, allocator, instance_out) };
    if rc == VK_SUCCESS && !instance_out.is_null() {
        // SAFETY: success means the loader wrote the handle.
        HOST_INSTANCE.store(unsafe { *instance_out } as usize, Relaxed);
    }
    rc
}

// ------------------------------------------------------------- surface

/// `vkCreateAndroidSurfaceKHR`, answered with `vkCreateXlibSurfaceKHR`.
///
/// The `ANativeWindow*` inside `pCreateInfo` is the client's own (there is
/// exactly one window), so it is not read; the real handles come from the
/// window singleton, the same pair `egl_create_window_surface` already
/// substitutes for EGL in `native_window.rs`.
extern "C" fn vk_create_android_surface_khr(
    instance: *mut c_void,
    _info: *const c_void,
    allocator: *const c_void,
    surface_out: *mut *mut c_void,
) -> i32 {
    let Some(h) = host() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some(win) = crate::android::x11_window::current() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // SAFETY: `instance` is a real host `VkInstance` by the time the engine
    // can reach this call, and the name is Mesa's own documented export.
    let f =
        unsafe { (h.get_instance_proc_addr)(instance, c"vkCreateXlibSurfaceKHR".as_ptr()) };
    if f.is_null() {
        return VK_ERROR_EXTENSION_NOT_PRESENT;
    }
    type Fn_ = unsafe extern "C" fn(
        *mut c_void,
        *const VkXlibSurfaceCreateInfoKHR,
        *const c_void,
        *mut *mut c_void,
    ) -> i32;
    // SAFETY: resolved from the host for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let xlib_info = VkXlibSurfaceCreateInfoKHR {
        s_type: VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR,
        p_next: std::ptr::null(),
        flags: 0,
        dpy: win.egl_native_display(),
        window: win.egl_native_window(),
    };
    crate::android::trace(format_args!("vkCreateAndroidSurfaceKHR -> vkCreateXlibSurfaceKHR"));
    // SAFETY: `xlib_info` matches Mesa's `VkXlibSurfaceCreateInfoKHR`
    // layout exactly (see the module doc); `instance`, `allocator`
    // and `surface_out` are the caller's own arguments, forwarded
    // unchanged.
    unsafe { f(instance, &xlib_info, allocator, surface_out) }
}

// ------------------------------------------------------------- device

/// `vkCreateDevice`: which GPU, the ETC2 feature strip, and the device for
/// the ETC and capture modules.
extern "C" fn vk_create_device(
    physical_device: *mut c_void,
    create_info: *const c_void,
    allocator: *const c_void,
    device_out: *mut *mut c_void,
) -> i32 {
    let f = HOST_CREATE_DEVICE.load(Relaxed);
    if f == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    type Fn_ = extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut *mut c_void) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    PHYSICAL_DEVICE.store(physical_device as usize, Relaxed);
    // The first queue family the engine asked for, read out of its own
    // `VkDeviceCreateInfo`: a screenshot's command pool is per family, and
    // this is its guess until a queue lookup or a present says which family
    // presents. Offsets are the specification's C layout:
    // `queueCreateInfoCount` at 20, the array pointer at 24, and
    // `queueFamilyIndex` 20 bytes into the first element.
    let mut first_family = None;
    if !create_info.is_null() {
        // SAFETY: the engine's `VkDeviceCreateInfo`, read at the offsets
        // above; the first element is read only when there is one.
        unsafe {
            let base = create_info as *const u8;
            let count = std::ptr::read_unaligned(base.add(20) as *const u32);
            let infos = std::ptr::read_unaligned(base.add(24) as *const *const u8);
            if count > 0 && !infos.is_null() {
                first_family = Some(std::ptr::read_unaligned(infos.add(20) as *const u32));
            }
        }
    }
    announce_physical_device(physical_device);
    // The one change made to the create info, and only on a GPU whose ETC2 is
    // emulated: the feature the engine was told exists is withheld from the
    // driver that lacks it. Dropped straight after the call, which puts back
    // anything it cleared in the engine's own structures.
    let stripped = super::vulkan_etc::strip_device_features(physical_device, create_info);
    let info = stripped.as_ref().map_or(create_info, |s| s.as_ptr());
    let rc = f(physical_device, info, allocator, device_out);
    drop(stripped);
    if rc == VK_SUCCESS && !device_out.is_null() {
        // SAFETY: success means the driver wrote the handle.
        let device = unsafe { *device_out };
        super::vulkan_etc::device_created(physical_device, device);
        super::frame_capture::device_created(physical_device, device, first_family);
    }
    rc
}

/// What a physical device says it is, read once per handle.
#[derive(Clone)]
struct DeviceIdentity {
    name: String,
    vendor_id: u32,
    device_id: u32,
    driver_version: u32,
    api_version: u32,
}

const VENDOR_NVIDIA: u32 = 0x10DE;

fn device_identity(physical_device: *mut c_void) -> Option<DeviceIdentity> {
    static KNOWN: std::sync::Mutex<Vec<(usize, DeviceIdentity)>> = std::sync::Mutex::new(Vec::new());
    if physical_device.is_null() {
        return None;
    }
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, id)) = known.iter().find(|(pd, _)| *pd == physical_device as usize) {
        return Some(id.clone());
    }
    let f = host_instance_proc(c"vkGetPhysicalDeviceProperties");
    if f.is_null() {
        return None;
    }
    type Fn_ = extern "C" fn(*mut c_void, *mut u8);
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    // `VkPhysicalDeviceProperties` is 824 bytes on LP64. The buffer is larger
    // so a driver can never write past it, and `u64` so `limits` is aligned.
    let mut buffer = [0u64; 128];
    f(physical_device, buffer.as_mut_ptr() as *mut u8);
    // SAFETY: the buffer is 1024 initialised bytes.
    let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const u8, 1024) };
    // apiVersion 0, driverVersion 4, vendorID 8, deviceID 12, deviceType 16,
    // deviceName 20..276.
    let word = |at: usize| u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let name = &bytes[20..276];
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let id = DeviceIdentity {
        name: String::from_utf8_lossy(&name[..end]).into_owned(),
        api_version: word(0),
        driver_version: word(4),
        vendor_id: word(8),
        device_id: word(12),
    };
    known.push((physical_device as usize, id.clone()));
    Some(id)
}

/// The device's own name, or its handle when it cannot be asked, for log
/// lines elsewhere in this directory.
pub(super) fn device_name(physical_device: *mut c_void) -> String {
    device_identity(physical_device).map_or_else(|| format!("{physical_device:p}"), |id| id.name)
}

/// `driverVersion` as text. It is vendor-defined: Mesa packs it like an API
/// version, NVIDIA as 10.8.8.6 bits -- 575.64.5 reads as nonsense the other
/// way, which is the mistake this exists to not make in a bug report.
fn driver_version_text(vendor_id: u32, v: u32) -> String {
    if vendor_id == VENDOR_NVIDIA {
        format!("{}.{}.{:02}", v >> 22, (v >> 14) & 0xff, (v >> 6) & 0xff)
    } else {
        format!("{}.{}.{}", (v >> 22) & 0x7f, (v >> 12) & 0x3ff, v & 0xfff)
    }
}

/// Say which GPU the engine built its device on, once per device, for every
/// vendor.
///
/// Which GPU a session ran on is the first thing a graphics report needs, and
/// on a machine with two it is not the one anybody assumes. From
/// `vkCreateDevice`, so it names the device actually used rather than one the
/// engine merely enumerated.
fn announce_physical_device(physical_device: *mut c_void) {
    let Some(id) = device_identity(physical_device) else { return };
    println!(
        "[android] vulkan: rendering on \"{}\" (vendor 0x{:04x}, device 0x{:04x}, driver {}, Vulkan {})",
        id.name,
        id.vendor_id,
        id.device_id,
        driver_version_text(id.vendor_id, id.driver_version),
        driver_version_text(0, id.api_version),
    );
}

// ------------------------------------------------------ present modes

static HOST_GET_PRESENT_MODES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Status codes worth asking again for: `VK_ERROR_UNKNOWN`,
/// `VK_ERROR_INITIALIZATION_FAILED` and `VK_ERROR_SURFACE_LOST_KHR`, the three
/// reported for this call failing on NVIDIA laptops. Out-of-memory and device
/// loss are not, because a second ask cannot change either.
fn present_modes_failure_is_retryable(rc: i32) -> bool {
    matches!(rc, -13 | -3 | -1_000_000_000)
}

/// Pauses before each further attempt, in milliseconds: under two seconds in
/// all, spent only on a path that was about to end the session anyway.
const PRESENT_MODES_RETRY_MS: [u64; 4] = [100, 250, 500, 1000];

/// Call `ask` until it stops failing retryably or the pauses run out. The
/// result and how many calls were made. Generic over `pause` so a test can
/// watch the schedule without waiting through it.
fn ask_with_retries(mut ask: impl FnMut() -> i32, mut pause: impl FnMut(u64)) -> (i32, u32) {
    let mut rc = ask();
    let mut calls = 1;
    for ms in PRESENT_MODES_RETRY_MS {
        if !present_modes_failure_is_retryable(rc) {
            break;
        }
        pause(ms);
        rc = ask();
        calls += 1;
    }
    (rc, calls)
}

/// `INTOXICATED_TEST_FAIL_PRESENT_MODES=N`: fail the first N calls ourselves
/// with `VK_ERROR_UNKNOWN`, and retry them on any vendor, so the schedule can
/// be watched on a driver that never fails. It shows the retry works; it says
/// nothing about whether retrying cures the real failure.
fn injected_present_mode_failures() -> &'static std::sync::atomic::AtomicU32 {
    static LEFT: OnceLock<std::sync::atomic::AtomicU32> = OnceLock::new();
    LEFT.get_or_init(|| {
        let n = std::env::var("INTOXICATED_TEST_FAIL_PRESENT_MODES")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        std::sync::atomic::AtomicU32::new(n)
    })
}

/// `vkGetPhysicalDeviceSurfacePresentModesKHR`, asked again on NVIDIA when it
/// fails, and forwarded untouched on every other vendor.
///
/// The report this answers comes from Sober's issue tracker: on
/// hybrid NVIDIA laptops the first present-modes query of a boot fails with
/// `VK_ERROR_UNKNOWN`, the engine treats it as fatal, and reporters got past
/// it by running anything on the GPU once first. **That a pause and a second
/// ask stands in for "running something first" is `INFERRED`** -- the
/// mechanism was never named, and none of it has run on NVIDIA under FreeBSD.
/// The cost of being wrong is under two seconds on a call that was going to
/// end the session regardless; a call that succeeds is never retried.
extern "C" fn vk_get_physical_device_surface_present_modes_khr(
    physical_device: *mut c_void,
    surface: u64,
    count: *mut u32,
    modes: *mut i32,
) -> i32 {
    let f = HOST_GET_PRESENT_MODES.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    type Fn_ = extern "C" fn(*mut c_void, u64, *mut u32, *mut i32) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let injected = injected_present_mode_failures();
    let nvidia = device_identity(physical_device).is_some_and(|id| id.vendor_id == VENDOR_NVIDIA);
    if !nvidia && injected.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        return f(physical_device, surface, count, modes);
    }
    let (rc, calls) = ask_with_retries(
        || {
            let take = injected.fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| n.checked_sub(1),
            );
            if take.is_ok() {
                -13
            } else {
                f(physical_device, surface, count, modes)
            }
        },
        |ms| std::thread::sleep(std::time::Duration::from_millis(ms)),
    );
    if calls > 1 {
        println!(
            "[android] vulkan: vkGetPhysicalDeviceSurfacePresentModesKHR took {calls} calls and returned {rc}{}",
            if present_modes_failure_is_retryable(rc) { "; giving up, the engine will see the failure" } else { "" }
        );
    }
    rc
}

/// The driver's present modes for `surface` on the device the engine built,
/// from the host's own entry (not the retry hook), or `None` when they
/// cannot be asked.
fn driver_present_modes(surface: u64) -> Option<Vec<i32>> {
    let pd = PHYSICAL_DEVICE.load(Relaxed) as *mut c_void;
    let f = host_instance_proc(c"vkGetPhysicalDeviceSurfacePresentModesKHR");
    if pd.is_null() || f.is_null() {
        return None;
    }
    type Fn_ = extern "C" fn(*mut c_void, u64, *mut u32, *mut i32) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let mut n = 0u32;
    if f(pd, surface, &mut n, std::ptr::null_mut()) != VK_SUCCESS {
        return None;
    }
    let mut modes = vec![0i32; n as usize];
    match f(pd, surface, &mut n, modes.as_mut_ptr()) {
        VK_SUCCESS | VK_INCOMPLETE => {
            modes.truncate(n as usize);
            Some(modes)
        }
        _ => None,
    }
}

/// The surface's capabilities, or `None` when they could not be asked for.
fn surface_capabilities(surface: u64) -> Option<VkSurfaceCapabilitiesKHR> {
    let pd = PHYSICAL_DEVICE.load(Relaxed) as *mut c_void;
    let f = host_instance_proc(c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR");
    if pd.is_null() || surface == 0 || f.is_null() {
        return None;
    }
    type Fn_ = extern "C" fn(*mut c_void, u64, *mut VkSurfaceCapabilitiesKHR) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let mut caps = VkSurfaceCapabilitiesKHR::default();
    if f(pd, surface, &mut caps) != VK_SUCCESS {
        return None;
    }
    Some(caps)
}

// ------------------------------------------------------------- swapchain

/// The present-mode setting, resolved at the first swapchain and printed
/// once.
fn present_mode_policy() -> &'static present_mode::Policy {
    static POLICY: OnceLock<present_mode::Policy> = OnceLock::new();
    POLICY.get_or_init(|| {
        let env = std::env::var(present_mode::ENV).ok();
        let resolved = crate::flag_layers::resolve(crate::flag_layers::collect());
        let flag = resolved.get(present_mode::FLAG_KEY).map(|e| (e.source.describe(), e.value.clone()));
        let r = present_mode::resolve(env.as_deref(), flag.as_ref().map(|(s, v)| (s.as_str(), v.as_str())));
        for w in &r.warnings {
            println!("{w}");
        }
        println!("[android] vulkan: present mode setting is {}", r.source);
        r.policy
    })
}

/// The present mode the last swapchain was created with, for devctl.
static LAST_PRESENT_MODE: AtomicUsize = AtomicUsize::new(usize::MAX);

/// `vkCreateSwapchainKHR`.
extern "C" fn vk_create_swapchain_khr(
    device: *mut c_void,
    create_info: *const VkSwapchainCreateInfoKHR,
    allocator: *const c_void,
    swapchain_out: *mut u64,
) -> i32 {
    // Diagnostic: log the engine function that creates the swapchain (the render
    // thread's render path), so it can be decompiled. Reads the saved return
    // address at [rbp+8]; subtract the run's libroblox base for the VA.
    if std::env::var_os("INTOXICATED_LOG_SWC_CALLER").is_some() {
        let ra: usize;
        // SAFETY: reading the caller's return address from the standard frame.
        unsafe { std::arch::asm!("mov {}, [rbp+8]", out(reg) ra, options(nostack, preserves_flags)); }
        eprintln!("[swc-caller] vkCreateSwapchainKHR called from return-address {ra:#x}");
    }
    // A devctl screenshot copies out of the swapchain image, and
    // `vkCmdCopyImageToBuffer` needs `VK_IMAGE_USAGE_TRANSFER_SRC_BIT` on it.
    // Whether the engine asks for it is not known, so it is added, but only
    // when dev control is on (the only way to ask for a screenshot) and the
    // surface supports it. Every other field is the engine's.
    let widened = with_transfer_src(create_info);
    let create_info = widened.as_ref().map_or(create_info, |w| w as *const VkSwapchainCreateInfoKHR);
    let rc = create_swapchain(device, create_info, allocator, swapchain_out);
    if rc == VK_SUCCESS && !create_info.is_null() && !swapchain_out.is_null() {
        // SAFETY: both pointers are the caller's (or our copy of the caller's
        // struct), checked for null, and the driver has just reported success
        // so `swapchain_out` is written.
        let (info, swapchain) = unsafe { (&*create_info, *swapchain_out) };
        super::frame_capture::swapchain_created(
            device,
            swapchain,
            info.image_extent.width,
            info.image_extent.height,
            info.image_format,
        );
    }
    rc
}

/// The swapchain itself: one line about it, then the present-mode policy.
/// Only `presentMode` and `minImageCount` can differ from what the engine
/// passed; every pointer inside stays the engine's.
fn create_swapchain(
    device: *mut c_void,
    create_info: *const VkSwapchainCreateInfoKHR,
    allocator: *const c_void,
    swapchain_out: *mut u64,
) -> i32 {
    let f = HOST_CREATE_SWAPCHAIN.load(Relaxed);
    if f == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    type Fn_ = extern "C" fn(*mut c_void, *const VkSwapchainCreateInfoKHR, *const c_void, *mut u64) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    // SAFETY: the caller's struct (or our copy of it); null is forwarded.
    let Some(engine) = (unsafe { create_info.as_ref() }) else {
        return f(device, create_info, allocator, swapchain_out);
    };
    println!(
        "[android] vulkan: vkCreateSwapchainKHR extent {}x{}, minImageCount {} (old swapchain {})",
        engine.image_extent.width,
        engine.image_extent.height,
        engine.min_image_count,
        if engine.old_swapchain == 0 { "none" } else { "recreated" }
    );
    let policy = present_mode_policy();
    let driver = match policy {
        present_mode::Policy::Engine => None,
        _ => driver_present_modes(engine.surface),
    };
    let d = present_mode::decide(policy, engine.present_mode, engine.min_image_count, driver.as_deref(), || {
        surface_capabilities(engine.surface).map(|c| c.max_image_count)
    });
    for line in &d.lines {
        println!("{line}");
    }
    LAST_PRESENT_MODE.store(d.mode as u32 as usize, Relaxed);
    if (d.mode, d.min_image_count) == (engine.present_mode, engine.min_image_count) {
        return f(device, create_info, allocator, swapchain_out);
    }
    let adjusted = VkSwapchainCreateInfoKHR { present_mode: d.mode, min_image_count: d.min_image_count, ..*engine };
    f(device, &adjusted, allocator, swapchain_out)
}

/// The present mode of the last swapchain created, by name; `none` before
/// the first.
pub fn last_present_mode() -> &'static str {
    match LAST_PRESENT_MODE.load(Relaxed) {
        usize::MAX => "none",
        m => present_mode::mode_name(m as u32 as i32),
    }
}

/// `VK_IMAGE_USAGE_TRANSFER_SRC_BIT`.
const IMAGE_USAGE_TRANSFER_SRC: u32 = 0x1;

/// The engine's swapchain create info with `TRANSFER_SRC` added to its image
/// usage, or `None` to pass the engine's own through. See the caller.
fn with_transfer_src(create_info: *const VkSwapchainCreateInfoKHR) -> Option<VkSwapchainCreateInfoKHR> {
    // SAFETY: the caller's own struct; a null one is forwarded untouched.
    let info = unsafe { create_info.as_ref() }?;
    if info.image_usage & IMAGE_USAGE_TRANSFER_SRC != 0 || !crate::devctl::enabled() {
        return None;
    }
    match surface_capabilities(info.surface) {
        Some(caps) if caps.supported_usage_flags & IMAGE_USAGE_TRANSFER_SRC != 0 => {
            println!(
                "[android] vulkan: swapchain imageUsage 0x{:x} -> 0x{:x} (TRANSFER_SRC, for devctl screenshot)",
                info.image_usage,
                info.image_usage | IMAGE_USAGE_TRANSFER_SRC
            );
            Some(VkSwapchainCreateInfoKHR { image_usage: info.image_usage | IMAGE_USAGE_TRANSFER_SRC, ..*info })
        }
        Some(_) => {
            println!(
                "[android] vulkan: swapchain imageUsage 0x{:x}; the surface does not offer TRANSFER_SRC, so a \
                 devctl screenshot copies from an image not created for it",
                info.image_usage
            );
            None
        }
        None => None,
    }
}

// ------------------------------------------------------------- acquire

/// The real `vkAcquireNextImageKHR`. Interposed for one status code: the
/// semaphore, the fence and the image index pass through untouched.
///
/// **`VK_SUBOPTIMAL_KHR` is reported to the engine as `VK_SUCCESS`.** Both are
/// success codes and both hand back a valid, acquired image; suboptimal adds
/// only "the swapchain could be rebuilt to fit the surface better". A report
/// from outside this project traced a crash on a Wayland fullscreen exit on
/// RADV to the engine's answer to that hint -- a `PRESENT_SRC_KHR` barrier
/// recorded against `VK_NULL_HANDLE` -- and showed that returning `VK_SUCCESS`
/// for the same acquire, with nothing else changed, removed the barrier. That
/// report is the evidence this rests on. It has not been reproduced on this
/// host, so that
/// FreeBSD's RADV reaches the same path is `INFERRED`.
///
/// What is given up is the hint, and nothing here relies on it: a size change
/// reaches the engine as a new surface extent from
/// `vkGetPhysicalDeviceSurfaceCapabilitiesKHR`, which is what it rebuilds
/// from. `VK_ERROR_OUT_OF_DATE_KHR` is left alone, because there no
/// image was acquired and calling it success would be a lie the engine then
/// renders into. `vkQueuePresentKHR`'s suboptimal is left alone too; nobody
/// has shown the engine mishandling it there.
///
/// `INTOXICATED_ACQUIRE_SUBOPTIMAL=pass` forwards the driver's status
/// unchanged, which is the control. Every withheld code is counted and printed
/// at powers of ten, so a log shows the substitution happening.
static HOST_ACQUIRE_NEXT_IMAGE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

extern "C" fn vk_acquire_next_image_khr(
    device: *mut c_void,
    swapchain: u64,
    timeout: u64,
    semaphore: u64,
    fence: u64,
    image_index: *mut u32,
) -> i32 {
    let f = HOST_ACQUIRE_NEXT_IMAGE.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    type Fn_ = extern "C" fn(*mut c_void, u64, u64, u64, u64, *mut u32) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let rc = f(device, swapchain, timeout, semaphore, fence, image_index);
    let reported = acquire_status_for_engine(rc, pass_suboptimal_acquire());
    if reported != rc {
        use std::sync::atomic::{AtomicU64, Ordering};
        static WITHHELD: AtomicU64 = AtomicU64::new(0);
        let n = WITHHELD.fetch_add(1, Ordering::Relaxed) + 1;
        if is_first_or_power_of_ten(n) {
            println!(
                "[android] vulkan: vkAcquireNextImageKHR returned VK_SUBOPTIMAL_KHR, reported to the \
                 engine as VK_SUCCESS ({n} so far; INTOXICATED_ACQUIRE_SUBOPTIMAL=pass forwards it)"
            );
        }
    }
    reported
}

fn pass_suboptimal_acquire() -> bool {
    static PASS: OnceLock<bool> = OnceLock::new();
    *PASS.get_or_init(|| {
        std::env::var("INTOXICATED_ACQUIRE_SUBOPTIMAL").is_ok_and(|v| v.trim().eq_ignore_ascii_case("pass"))
    })
}

/// The status the engine is given for an acquire the driver answered `rc`.
fn acquire_status_for_engine(rc: i32, pass: bool) -> i32 {
    if rc == VK_SUBOPTIMAL_KHR && !pass {
        VK_SUCCESS
    } else {
        rc
    }
}

/// 1, 10, 100, ...: the counts at which a repeated condition is printed.
fn is_first_or_power_of_ten(n: u64) -> bool {
    let mut p = 1;
    while p < n {
        match p.checked_mul(10) {
            Some(next) => p = next,
            None => return false,
        }
    }
    p == n
}

// ------------------------------------------------------------- present

/// `vkQueuePresentKHR`: counted and paced once per engine call, the
/// screenshot if one is armed, and any result other than success reported.
extern "C" fn vk_queue_present_khr(queue: *mut c_void, info: *const c_void) -> i32 {
    let presents = super::glcount::QUEUE_PRESENT.fetch_add(1, Relaxed) + 1;
    super::present_stats::record();
    passes::on_present(presents);
    let f = HOST_QUEUE_PRESENT.load(Relaxed);
    if f == 0 {
        // Not reachable: this hook is handed out only once the host's entry
        // is recorded. An error rather than a success that presented nothing.
        return VK_ERROR_DEVICE_LOST;
    }
    // SAFETY: resolved from the host loader for exactly this name.
    let f: extern "C" fn(*mut c_void, *const c_void) -> i32 = unsafe { std::mem::transmute(f) };
    // A screenshot, if one was asked for, before the frame goes anywhere.
    // The copy has to be ordered after the engine's rendering and before
    // the presentation engine owns the image again; anything done to the
    // image after this present returns would be racing the display, and
    // reading it is a race. `frame_capture::present` submits the copy and
    // forwards the present itself, with its wait list swapped for the copy's
    // semaphore; on every other frame this is one atomic load.
    let rc = if super::frame_capture::pending() {
        super::frame_capture::present(queue, info, |i| f(queue, i))
    } else {
        f(queue, info)
    };
    if rc != VK_SUCCESS {
        report_present_result(rc);
    }
    rc
}

/// The name of a `vkQueuePresentKHR` result, or a description.
fn present_result_name(rc: i32) -> &'static str {
    match rc {
        VK_SUBOPTIMAL_KHR => "VK_SUBOPTIMAL_KHR",
        VK_ERROR_OUT_OF_DATE_KHR => "VK_ERROR_OUT_OF_DATE_KHR",
        VK_ERROR_SURFACE_LOST_KHR => "VK_ERROR_SURFACE_LOST_KHR",
        VK_ERROR_DEVICE_LOST => "VK_ERROR_DEVICE_LOST",
        VK_ERROR_OUT_OF_HOST_MEMORY => "VK_ERROR_OUT_OF_HOST_MEMORY",
        VK_ERROR_OUT_OF_DEVICE_MEMORY => "VK_ERROR_OUT_OF_DEVICE_MEMORY",
        VK_ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT => "VK_ERROR_FULL_SCREEN_EXCLUSIVE_MODE_LOST_EXT",
        _ => "a non-success code",
    }
}

/// Count a non-success present in its class (suboptimal, out of date,
/// anything else) and print it at the first and every power of ten.
fn report_present_result(rc: i32) {
    use std::sync::atomic::AtomicU64;
    static SUBOPTIMAL: AtomicU64 = AtomicU64::new(0);
    static OUT_OF_DATE: AtomicU64 = AtomicU64::new(0);
    static OTHER: AtomicU64 = AtomicU64::new(0);
    let class = match rc {
        VK_SUBOPTIMAL_KHR => &SUBOPTIMAL,
        VK_ERROR_OUT_OF_DATE_KHR => &OUT_OF_DATE,
        _ => &OTHER,
    };
    let n = class.fetch_add(1, Relaxed) + 1;
    if is_first_or_power_of_ten(n) {
        println!("[android] vulkan: vkQueuePresentKHR returned {} ({rc}), {n} so far", present_result_name(rc));
    }
}

// ------------------------------------------------------------- devctl

/// devctl `screenshot <path>`: the next frame, written as a PNG, without
/// holding the render thread for the encoding. Bounded, because a capture
/// that never completes (no frame is being presented, say) would otherwise
/// hang the harness that came to look at it. See
/// `frame_capture::request` for the bounds and what each thread does.
pub fn request_capture(path: &str) -> Result<String, String> {
    super::frame_capture::request(path)
}

/// devctl `passes`: the next frame's render passes to stderr, and its
/// summary line back. See [`passes`].
pub fn request_pass_trace() -> Result<String, String> {
    passes::request_trace()
}

/// devctl `depthshot <path>`: the scene depth of the next frame that has the
/// expected shape, as a grayscale PNG, and the colour of the same frame
/// beside it. See `depth_capture`.
pub fn request_depth_capture(path: &str) -> Result<String, String> {
    super::depth_capture::request(path)
}

/// The extent of the most recently created swapchain, for callers reporting
/// state; `(0, 0)` before the first.
pub fn last_extent() -> (u32, u32) {
    super::frame_capture::extent()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_suboptimal_acquire_is_reported_as_success() {
        assert_eq!(acquire_status_for_engine(VK_SUBOPTIMAL_KHR, false), VK_SUCCESS);
        assert_eq!(acquire_status_for_engine(VK_SUBOPTIMAL_KHR, true), VK_SUBOPTIMAL_KHR, "the control");
        assert_eq!(acquire_status_for_engine(VK_SUCCESS, false), VK_SUCCESS);
        assert_eq!(acquire_status_for_engine(VK_ERROR_OUT_OF_DATE_KHR, false), VK_ERROR_OUT_OF_DATE_KHR);
        assert_eq!(acquire_status_for_engine(VK_INCOMPLETE, false), VK_INCOMPLETE);
    }

    #[test]
    fn present_modes_are_asked_again_on_the_schedule_and_then_given_up() {
        let mut slept = Vec::new();
        let mut left = 2;
        let (rc, calls) = ask_with_retries(
            || if left > 0 { left -= 1; -13 } else { VK_SUCCESS },
            |ms| slept.push(ms),
        );
        assert_eq!((rc, calls, slept.as_slice()), (VK_SUCCESS, 3, &[100, 250][..]));

        let mut slept = Vec::new();
        let (rc, calls) = ask_with_retries(|| -13, |ms| slept.push(ms));
        assert_eq!((rc, calls, slept.iter().sum::<u64>()), (-13, 5, 1850));

        // Out of memory is not retried: a second ask cannot change it.
        let (rc, calls) = ask_with_retries(|| -2, |_| panic!("no pause"));
        assert_eq!((rc, calls), (-2, 1));
    }

    #[test]
    fn driver_versions_are_read_the_vendors_way() {
        // NVIDIA 550.163.01 packed 10.8.8.6.
        let nv = (550 << 22) | (163 << 14) | (1 << 6);
        assert_eq!(driver_version_text(VENDOR_NVIDIA, nv), "550.163.01");
        // Mesa 25.1.4 packed as an API version.
        let mesa = (25 << 22) | (1 << 12) | 4;
        assert_eq!(driver_version_text(0x8086, mesa), "25.1.4");
    }

    #[test]
    fn reports_come_at_the_first_and_every_power_of_ten() {
        for n in [1, 10, 100, 1000, 10_000_000_000_000_000_000] {
            assert!(is_first_or_power_of_ten(n), "{n}");
        }
        for n in [0, 2, 9, 11, 99, 101, u64::MAX] {
            assert!(!is_first_or_power_of_ten(n), "{n}");
        }
    }

    #[test]
    fn present_results_are_named() {
        assert_eq!(present_result_name(VK_ERROR_OUT_OF_DATE_KHR), "VK_ERROR_OUT_OF_DATE_KHR");
        assert_eq!(present_result_name(VK_SUBOPTIMAL_KHR), "VK_SUBOPTIMAL_KHR");
        assert_eq!(present_result_name(-12345), "a non-success code");
    }

    fn props(names: &[&CStr]) -> Vec<VkExtensionProperties> {
        names.iter().map(|n| VkExtensionProperties::new(n, 1)).collect()
    }

    fn names(list: &[VkExtensionProperties]) -> Vec<String> {
        list.iter().map(|p| p.name().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn the_android_surface_is_advertised_when_the_xlib_one_backs_it() {
        let mut l = props(&[c"VK_KHR_surface", c"VK_KHR_xlib_surface"]);
        assert!(advertise_android_surface(&mut l, false));
        assert_eq!(names(&l), ["VK_KHR_surface", "VK_KHR_xlib_surface", "VK_KHR_android_surface"]);
        assert_eq!(l[2].spec_version, 6);

        let mut l = props(&[c"VK_KHR_surface", c"VK_KHR_xlib_surface"]);
        assert!(!advertise_android_surface(&mut l, true), "a layer's list is its own");
        assert_eq!(l.len(), 2);
        let mut l = props(&[c"VK_KHR_surface", c"VK_KHR_xlib_surface", c"VK_KHR_android_surface"]);
        assert!(!advertise_android_surface(&mut l, false), "already there");
        assert_eq!(l.len(), 3);
        let mut l = props(&[c"VK_KHR_surface", c"VK_KHR_xcb_surface"]);
        assert!(!advertise_android_surface(&mut l, false), "nothing to back it with");
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn the_host_list_is_read_whole_by_the_two_call_idiom() {
        let host = props(&[c"VK_KHR_surface", c"VK_KHR_xlib_surface"]);
        // SAFETY: the host stand-in is the same two-call answer.
        let list = host_extension_list(|n, p| unsafe { copy_extensions_out(&host, n, p) }).unwrap();
        assert_eq!(names(&list), names(&host));
    }

    #[test]
    fn the_two_call_idiom() {
        let list = props(&[c"VK_KHR_surface", c"VK_KHR_xlib_surface", c"VK_KHR_android_surface"]);
        let mut n = 0u32;
        // SAFETY: a live count and no array.
        assert_eq!(unsafe { copy_extensions_out(&list, &mut n, std::ptr::null_mut()) }, VK_SUCCESS);
        assert_eq!(n, 3);
        let mut out = vec![VkExtensionProperties::new(c"", 0); 2];
        let mut n = 2u32;
        // SAFETY: room for `n` entries.
        assert_eq!(unsafe { copy_extensions_out(&list, &mut n, out.as_mut_ptr()) }, VK_INCOMPLETE);
        assert_eq!((n, names(&out)), (2, vec!["VK_KHR_surface".to_string(), "VK_KHR_xlib_surface".to_string()]));
        let mut out = vec![VkExtensionProperties::new(c"", 0); 4];
        let mut n = 4u32;
        // SAFETY: as above.
        assert_eq!(unsafe { copy_extensions_out(&list, &mut n, out.as_mut_ptr()) }, VK_SUCCESS);
        assert_eq!(n, 3);
        // A null count is refused before anything else is asked.
        assert_eq!(
            vk_enumerate_instance_extension_properties(std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut()),
            VK_ERROR_INITIALIZATION_FAILED
        );
    }

    #[test]
    fn a_host_error_while_listing_is_returned_as_it_is() {
        assert_eq!(host_extension_list(|_, _| VK_ERROR_OUT_OF_HOST_MEMORY).err(), Some(VK_ERROR_OUT_OF_HOST_MEMORY));
    }

    #[test]
    fn the_instance_gets_the_xlib_surface_in_place_of_the_android_one() {
        let asked = [c"VK_KHR_surface".as_ptr(), c"VK_KHR_android_surface".as_ptr(), c"VK_KHR_get_physical_device_properties2".as_ptr()];
        let (out, replaced) = substitute_surface_extension(&asked);
        assert!(replaced);
        // SAFETY: NUL-terminated literals.
        let out: Vec<&CStr> = out.iter().map(|&p| unsafe { CStr::from_ptr(p) }).collect();
        assert_eq!(out, [c"VK_KHR_surface", c"VK_KHR_xlib_surface", c"VK_KHR_get_physical_device_properties2"]);
        // The other names are the engine's own pointers.
        let (out, _) = substitute_surface_extension(&asked);
        assert_eq!((out[0], out[2]), (asked[0], asked[2]));

        let (out, replaced) = substitute_surface_extension(&[]);
        assert!(out.is_empty() && !replaced, "zero extensions pass through");
    }

    #[test]
    fn layouts_match_the_specification() {
        assert_eq!(std::mem::size_of::<VkSwapchainCreateInfoKHR>(), 104);
        assert_eq!(std::mem::offset_of!(VkSwapchainCreateInfoKHR, image_usage), 56);
        assert_eq!(std::mem::offset_of!(VkSwapchainCreateInfoKHR, present_mode), 88);
        assert_eq!(std::mem::size_of::<VkSurfaceCapabilitiesKHR>(), 52);
        assert_eq!(std::mem::offset_of!(VkSurfaceCapabilitiesKHR, supported_usage_flags), 48);
        assert_eq!(std::mem::size_of::<VkExtensionProperties>(), 260);
        assert_eq!(std::mem::size_of::<VkInstanceCreateInfo>(), 64);
        assert_eq!(std::mem::offset_of!(VkInstanceCreateInfo, pp_enabled_extension_names), 56);
        assert_eq!(std::mem::size_of::<VkXlibSurfaceCreateInfoKHR>(), 40);
    }
}
