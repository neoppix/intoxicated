//! What a frame is drawn into: the images, views, render passes and
//! framebuffers the engine created, read out of their create infos as the
//! calls pass through the layer.
//!
//! Only descriptions are kept, never anything the engine's pointers lead to
//! after the call returns. Handles are keyed as the driver returned them;
//! a destroyed handle is forgotten, so a driver that reuses it is described
//! afresh.
//!
//! Every offset below is the C layout `vulkan_core.h` gives on LP64, printed
//! with the system compiler, and pinned by the tests at the end.

use std::collections::HashMap;
use std::ffi::c_void;

pub(crate) const FORMAT_D16_UNORM: u32 = 124;
pub(crate) const FORMAT_D32_SFLOAT: u32 = 126;

pub(crate) const USAGE_SAMPLED: u32 = 0x4;
pub(crate) const USAGE_COLOR_ATTACHMENT: u32 = 0x10;
pub(crate) const USAGE_DEPTH_STENCIL_ATTACHMENT: u32 = 0x20;

/// `VK_ATTACHMENT_UNUSED`.
pub(crate) const UNUSED: u32 = !0;

const ST_SUBPASS_DEPTH_STENCIL_RESOLVE: u32 = 1_000_199_001;
const ST_RENDER_PASS_ATTACHMENT_BEGIN_INFO: u32 = 1_000_108_003;
const FRAMEBUFFER_CREATE_IMAGELESS: u32 = 0x1;

/// Read a `T` at `offset` bytes into a Vulkan structure.
///
/// # Safety
/// `base + offset .. + size_of::<T>()` must be readable.
pub(crate) unsafe fn get<T: Copy>(base: *const c_void, offset: usize) -> T {
    std::ptr::read_unaligned((base as *const u8).add(offset) as *const T)
}

/// The first structure of `s_type` in a `pNext` chain, or null.
///
/// # Safety
/// `node` must be null or the head of a valid chain.
pub(crate) unsafe fn find_in_chain(mut node: *const c_void, s_type: u32) -> *const c_void {
    while !node.is_null() {
        if get::<u32>(node, 0) == s_type {
            return node;
        }
        node = get(node, 8);
    }
    std::ptr::null()
}

/// `count` handles at `list`, or none when the list is null.
///
/// # Safety
/// `list` must be null or point at `count` readable `u64`s.
pub(crate) unsafe fn handles(list: *const c_void, count: u32) -> Vec<u64> {
    if list.is_null() {
        return Vec::new();
    }
    (0..count as usize).map(|i| get::<u64>(list, i * 8)).collect()
}

// ------------------------------------------------------------ descriptions

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Image {
    pub device: usize,
    pub flags: u32,
    pub format: u32,
    pub extent: [u32; 3],
    pub mips: u32,
    pub layers: u32,
    pub samples: u32,
    pub usage: u32,
    /// One of a swapchain's images, which the engine never creates itself.
    pub swapchain: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub device: usize,
    pub image: u64,
    pub view_type: u32,
    pub format: u32,
    pub aspect: u32,
    pub base_mip: u32,
    pub levels: u32,
    pub base_layer: u32,
    pub layers: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attachment {
    pub format: u32,
    pub samples: u32,
    pub load: u32,
    pub store: u32,
    pub stencil_load: u32,
    pub stencil_store: u32,
    pub initial_layout: u32,
    pub final_layout: u32,
}

/// An attachment index and the layout a subpass uses it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ref {
    pub attachment: u32,
    pub layout: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Subpass {
    pub input: Vec<Ref>,
    pub color: Vec<Ref>,
    /// Empty, or one per colour attachment.
    pub resolve: Vec<Ref>,
    pub depth: Option<Ref>,
    /// `VkSubpassDescriptionDepthStencilResolve` (render pass 2 only): the
    /// single-sample attachment the depth is resolved into, and the modes.
    pub depth_resolve: Option<Ref>,
    pub depth_resolve_mode: u32,
    pub stencil_resolve_mode: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Dependency {
    pub src_subpass: u32,
    pub dst_subpass: u32,
    pub src_stage: u32,
    pub dst_stage: u32,
    pub src_access: u32,
    pub dst_access: u32,
    pub flags: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderPass {
    pub device: usize,
    /// 1 for `vkCreateRenderPass`, 2 for `vkCreateRenderPass2`.
    pub version: u8,
    pub attachments: Vec<Attachment>,
    pub subpasses: Vec<Subpass>,
    pub dependencies: Vec<Dependency>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Framebuffer {
    pub device: usize,
    pub render_pass: u64,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    /// The views, in attachment order; empty for an imageless framebuffer,
    /// whose views come with each `vkCmdBeginRenderPass`.
    pub views: Vec<u64>,
    pub imageless: bool,
}

// ------------------------------------------------------------ parsing

/// # Safety
/// `info` must be a valid `VkImageCreateInfo`.
pub(crate) unsafe fn image_from(device: usize, info: *const c_void) -> Image {
    Image {
        device,
        flags: get(info, 16),
        format: get(info, 24),
        extent: get(info, 28),
        mips: get(info, 40),
        layers: get(info, 44),
        samples: get(info, 48),
        usage: get(info, 56),
        swapchain: false,
    }
}

/// # Safety
/// `info` must be a valid `VkImageViewCreateInfo`.
pub(crate) unsafe fn view_from(device: usize, info: *const c_void) -> View {
    View {
        device,
        image: get(info, 24),
        view_type: get(info, 32),
        format: get(info, 36),
        aspect: get(info, 56),
        base_mip: get(info, 60),
        levels: get(info, 64),
        base_layer: get(info, 68),
        layers: get(info, 72),
    }
}

/// One `VkAttachmentReference` (8 bytes) or `VkAttachmentReference2` (32).
unsafe fn reference(r: *const c_void, v2: bool) -> Ref {
    if v2 {
        Ref { attachment: get(r, 16), layout: get(r, 20) }
    } else {
        Ref { attachment: get(r, 0), layout: get(r, 4) }
    }
}

unsafe fn references(list: *const c_void, count: u32, v2: bool) -> Vec<Ref> {
    if list.is_null() {
        return Vec::new();
    }
    let stride = if v2 { 32 } else { 8 };
    (0..count as usize).map(|i| reference((list as *const u8).add(i * stride) as *const c_void, v2)).collect()
}

/// A `VkRenderPassCreateInfo` (`v2` false) or `VkRenderPassCreateInfo2`.
///
/// # Safety
/// `info` must be a valid structure of the kind `v2` names.
pub(crate) unsafe fn render_pass_from(device: usize, info: *const c_void, v2: bool) -> RenderPass {
    let at = |list: *const c_void, stride: usize, i: usize| (list as *const u8).add(i * stride) as *const c_void;
    let (n_att, p_att): (u32, *const c_void) = (get(info, 20), get(info, 24));
    let (n_sub, p_sub): (u32, *const c_void) = (get(info, 32), get(info, 40));
    let (n_dep, p_dep): (u32, *const c_void) = (get(info, 48), get(info, 56));
    let attachments = if p_att.is_null() {
        Vec::new()
    } else {
        (0..n_att as usize)
            .map(|i| {
                if v2 {
                    let a = at(p_att, 56, i);
                    Attachment {
                        format: get(a, 20),
                        samples: get(a, 24),
                        load: get(a, 28),
                        store: get(a, 32),
                        stencil_load: get(a, 36),
                        stencil_store: get(a, 40),
                        initial_layout: get(a, 44),
                        final_layout: get(a, 48),
                    }
                } else {
                    let a = at(p_att, 36, i);
                    Attachment {
                        format: get(a, 4),
                        samples: get(a, 8),
                        load: get(a, 12),
                        store: get(a, 16),
                        stencil_load: get(a, 20),
                        stencil_store: get(a, 24),
                        initial_layout: get(a, 28),
                        final_layout: get(a, 32),
                    }
                }
            })
            .collect()
    };
    let subpasses = if p_sub.is_null() {
        Vec::new()
    } else {
        (0..n_sub as usize)
            .map(|i| {
                // (input count, input, colour count, colour, resolve, depth)
                let (s, o) = if v2 { (at(p_sub, 88, i), [28, 32, 40, 48, 56, 64]) } else { (at(p_sub, 72, i), [8, 16, 24, 32, 40, 48]) };
                let n_color: u32 = get(s, o[2]);
                let depth: *const c_void = get(s, o[5]);
                let mut sp = Subpass {
                    input: references(get(s, o[1]), get(s, o[0]), v2),
                    color: references(get(s, o[3]), n_color, v2),
                    resolve: references(get(s, o[4]), n_color, v2),
                    depth: (!depth.is_null()).then(|| reference(depth, v2)),
                    ..Subpass::default()
                };
                if v2 {
                    let r = find_in_chain(get(s, 8), ST_SUBPASS_DEPTH_STENCIL_RESOLVE);
                    if !r.is_null() {
                        sp.depth_resolve_mode = get(r, 16);
                        sp.stencil_resolve_mode = get(r, 20);
                        let att: *const c_void = get(r, 24);
                        sp.depth_resolve = (!att.is_null()).then(|| reference(att, true));
                    }
                }
                sp
            })
            .collect()
    };
    let dependencies = if p_dep.is_null() {
        Vec::new()
    } else {
        (0..n_dep as usize)
            .map(|i| {
                let (d, b) = if v2 { (at(p_dep, 48, i), 16) } else { (at(p_dep, 28, i), 0) };
                Dependency {
                    src_subpass: get(d, b),
                    dst_subpass: get(d, b + 4),
                    src_stage: get(d, b + 8),
                    dst_stage: get(d, b + 12),
                    src_access: get(d, b + 16),
                    dst_access: get(d, b + 20),
                    flags: get(d, b + 24),
                }
            })
            .collect()
    };
    RenderPass { device, version: if v2 { 2 } else { 1 }, attachments, subpasses, dependencies }
}

/// # Safety
/// `info` must be a valid `VkFramebufferCreateInfo`.
pub(crate) unsafe fn framebuffer_from(device: usize, info: *const c_void) -> Framebuffer {
    let flags: u32 = get(info, 16);
    let imageless = flags & FRAMEBUFFER_CREATE_IMAGELESS != 0;
    Framebuffer {
        device,
        render_pass: get(info, 24),
        width: get(info, 48),
        height: get(info, 52),
        layers: get(info, 56),
        views: if imageless { Vec::new() } else { handles(get(info, 40), get(info, 32)) },
        imageless,
    }
}

/// The views a `VkRenderPassBeginInfo` names in its pNext chain, for an
/// imageless framebuffer; empty when there is no such structure.
///
/// # Safety
/// `begin` must be a valid `VkRenderPassBeginInfo`.
pub(crate) unsafe fn begin_info_views(begin: *const c_void) -> Vec<u64> {
    let a = find_in_chain(get(begin, 8), ST_RENDER_PASS_ATTACHMENT_BEGIN_INFO);
    if a.is_null() {
        return Vec::new();
    }
    handles(get(a, 24), get(a, 16))
}

// ------------------------------------------------------------ the registry

#[derive(Default)]
pub(crate) struct Registry {
    pub images: HashMap<u64, Image>,
    pub views: HashMap<u64, View>,
    pub render_passes: HashMap<u64, RenderPass>,
    pub framebuffers: HashMap<u64, Framebuffer>,
    /// Swapchain to (device, its images).
    pub swapchains: HashMap<u64, (usize, Vec<u64>)>,
}

/// One attachment of a render pass instance, followed from the render pass
/// through the framebuffer's view to its image.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Resolved {
    pub index: u32,
    pub desc: Attachment,
    pub view: u64,
    pub image: u64,
    /// `None` when the view or its image is not in the registry (created
    /// before the hooks were handed out, say).
    pub info: Option<Image>,
}

impl Registry {
    /// Everything created on `device` forgotten, as it is destroyed.
    pub(crate) fn forget_device(&mut self, device: usize) {
        self.images.retain(|_, i| i.device != device);
        self.views.retain(|_, v| v.device != device);
        self.render_passes.retain(|_, r| r.device != device);
        self.framebuffers.retain(|_, f| f.device != device);
        self.swapchains.retain(|_, s| s.0 != device);
    }

    /// A swapchain's images, which the engine never creates itself.
    pub(crate) fn add_swapchain(&mut self, device: usize, swapchain: u64, images: &[u64], extent: [u32; 2], format: u32) {
        for &image in images {
            self.images.insert(
                image,
                Image {
                    device,
                    flags: 0,
                    format,
                    extent: [extent[0], extent[1], 1],
                    mips: 1,
                    layers: 1,
                    samples: 1,
                    usage: 0,
                    swapchain: true,
                },
            );
        }
        self.swapchains.insert(swapchain, (device, images.to_vec()));
    }

    /// A swapchain destroyed. Its images go with it, except any another live
    /// swapchain also lists: a swapchain created with this one as its old
    /// swapchain may be handed the same image handles, and is created before
    /// this one is destroyed.
    pub(crate) fn forget_swapchain(&mut self, swapchain: u64) {
        let Some((_, images)) = self.swapchains.remove(&swapchain) else { return };
        for image in images {
            if !self.swapchains.values().any(|(_, other)| other.contains(&image)) {
                self.images.remove(&image);
            }
        }
    }

    /// The extents of the live swapchains: what "full size" means.
    pub(crate) fn swapchain_extents(&self) -> Vec<[u32; 2]> {
        let mut out: Vec<[u32; 2]> = Vec::new();
        for (_, images) in self.swapchains.values() {
            if let Some(i) = images.first().and_then(|h| self.images.get(h)) {
                let e = [i.extent[0], i.extent[1]];
                if !out.contains(&e) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// The image behind a view, and its description if known.
    pub(crate) fn image_of_view(&self, view: u64) -> (u64, Option<&Image>) {
        match self.views.get(&view) {
            Some(v) => (v.image, self.images.get(&v.image)),
            None => (0, None),
        }
    }

    /// The attachments of a render pass instance: `views` are the ones the
    /// begin info named for an imageless framebuffer, empty otherwise.
    pub(crate) fn attachments(&self, render_pass: u64, framebuffer: u64, views: &[u64]) -> Vec<Resolved> {
        let Some(rp) = self.render_passes.get(&render_pass) else { return Vec::new() };
        let fb_views: &[u64] = match self.framebuffers.get(&framebuffer) {
            Some(fb) if !fb.imageless => &fb.views,
            _ => views,
        };
        rp.attachments
            .iter()
            .enumerate()
            .map(|(i, desc)| {
                let view = fb_views.get(i).copied().unwrap_or(0);
                let (image, info) = self.image_of_view(view);
                Resolved { index: i as u32, desc: *desc, view, image, info: info.cloned() }
            })
            .collect()
    }
}

// ------------------------------------------------------------ names

/// A short name for the formats a frame is likely to use; the number
/// otherwise.
pub(crate) fn format_name(format: u32) -> String {
    let n = match format {
        0 => "UNDEFINED",
        9 => "R8_UNORM",
        13 => "R8_UINT",
        16 => "R8G8_UNORM",
        37 => "R8G8B8A8_UNORM",
        43 => "R8G8B8A8_SRGB",
        44 => "B8G8R8A8_UNORM",
        50 => "B8G8R8A8_SRGB",
        58 => "A2R10G10B10_UNORM",
        64 => "A2B10G10R10_UNORM",
        70 => "R16_UNORM",
        74 => "R16_UINT",
        76 => "R16_SFLOAT",
        77 => "R16G16_UNORM",
        83 => "R16G16_SFLOAT",
        91 => "R16G16B16A16_UNORM",
        97 => "R16G16B16A16_SFLOAT",
        98 => "R32_UINT",
        100 => "R32_SFLOAT",
        103 => "R32G32_SFLOAT",
        109 => "R32G32B32A32_SFLOAT",
        122 => "B10G11R11_UFLOAT",
        124 => "D16_UNORM",
        125 => "X8_D24_UNORM",
        126 => "D32_SFLOAT",
        127 => "S8_UINT",
        129 => "D24_UNORM_S8_UINT",
        130 => "D32_SFLOAT_S8_UINT",
        _ => return format!("format{format}"),
    };
    format!("{n}({format})")
}

pub(crate) fn is_depth_format(format: u32) -> bool {
    matches!(format, 124..=126 | 128..=130)
}

pub(crate) fn layout_name(layout: u32) -> &'static str {
    match layout {
        0 => "UNDEFINED",
        1 => "GENERAL",
        2 => "COLOR_ATTACHMENT",
        3 => "DEPTH_STENCIL_ATTACHMENT",
        4 => "DEPTH_STENCIL_READ_ONLY",
        5 => "SHADER_READ_ONLY",
        6 => "TRANSFER_SRC",
        7 => "TRANSFER_DST",
        8 => "PREINITIALIZED",
        1_000_117_000 => "DEPTH_READ_ONLY_STENCIL_ATTACHMENT",
        1_000_117_001 => "DEPTH_ATTACHMENT_STENCIL_READ_ONLY",
        1_000_241_000 => "DEPTH_ATTACHMENT",
        1_000_241_001 => "DEPTH_READ_ONLY",
        1_000_241_002 => "STENCIL_ATTACHMENT",
        1_000_241_003 => "STENCIL_READ_ONLY",
        1_000_314_000 => "READ_ONLY",
        1_000_314_001 => "ATTACHMENT",
        1_000_001_002 => "PRESENT_SRC",
        _ => "other",
    }
}

pub(crate) fn load_name(op: u32) -> &'static str {
    match op {
        0 => "LOAD",
        1 => "CLEAR",
        2 => "DONT_CARE",
        1_000_400_000 => "NONE",
        _ => "other",
    }
}

pub(crate) fn store_name(op: u32) -> &'static str {
    match op {
        0 => "STORE",
        1 => "DONT_CARE",
        1_000_301_000 => "NONE",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors of the C structures, laid out as the header lays them out;
    // the size and offset checks below hold them to the values the system
    // compiler printed.

    #[repr(C)]
    struct ImageCreateInfo {
        s_type: u32,
        p_next: *const c_void,
        flags: u32,
        image_type: u32,
        format: u32,
        extent: [u32; 3],
        mips: u32,
        layers: u32,
        samples: u32,
        tiling: u32,
        usage: u32,
        sharing: u32,
        queue_count: u32,
        queues: *const u32,
        initial_layout: u32,
    }

    #[repr(C)]
    struct ImageViewCreateInfo {
        s_type: u32,
        p_next: *const c_void,
        flags: u32,
        image: u64,
        view_type: u32,
        format: u32,
        components: [u32; 4],
        range: [u32; 5],
    }

    #[repr(C)]
    struct AttachmentDescription([u32; 9]);

    #[repr(C)]
    struct AttachmentDescription2 {
        s_type: u32,
        p_next: *const c_void,
        body: [u32; 9],
    }

    #[repr(C)]
    struct SubpassDescription {
        flags: u32,
        bind_point: u32,
        input_count: u32,
        input: *const [u32; 2],
        color_count: u32,
        color: *const [u32; 2],
        resolve: *const [u32; 2],
        depth: *const [u32; 2],
        preserve_count: u32,
        preserve: *const u32,
    }

    #[repr(C)]
    struct AttachmentReference2 {
        s_type: u32,
        p_next: *const c_void,
        attachment: u32,
        layout: u32,
        aspect: u32,
    }

    #[repr(C)]
    struct DepthStencilResolve {
        s_type: u32,
        p_next: *const c_void,
        depth_mode: u32,
        stencil_mode: u32,
        attachment: *const AttachmentReference2,
    }

    #[repr(C)]
    struct SubpassDescription2 {
        s_type: u32,
        p_next: *const c_void,
        flags: u32,
        bind_point: u32,
        view_mask: u32,
        input_count: u32,
        input: *const AttachmentReference2,
        color_count: u32,
        color: *const AttachmentReference2,
        resolve: *const AttachmentReference2,
        depth: *const AttachmentReference2,
        preserve_count: u32,
        preserve: *const u32,
    }

    #[repr(C)]
    struct SubpassDependency([u32; 7]);

    #[repr(C)]
    struct RenderPassCreateInfo {
        s_type: u32,
        p_next: *const c_void,
        flags: u32,
        attachment_count: u32,
        attachments: *const c_void,
        subpass_count: u32,
        subpasses: *const c_void,
        dependency_count: u32,
        dependencies: *const c_void,
    }

    #[repr(C)]
    struct RenderPassCreateInfo2 {
        head: RenderPassCreateInfo,
        correlated_count: u32,
        correlated: *const u32,
    }

    #[repr(C)]
    struct FramebufferCreateInfo {
        s_type: u32,
        p_next: *const c_void,
        flags: u32,
        render_pass: u64,
        attachment_count: u32,
        attachments: *const u64,
        width: u32,
        height: u32,
        layers: u32,
    }

    #[test]
    fn the_mirrors_have_the_c_layout() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<ImageCreateInfo>(), 88);
        assert_eq!(offset_of!(ImageCreateInfo, usage), 56);
        assert_eq!(offset_of!(ImageCreateInfo, initial_layout), 80);
        assert_eq!(size_of::<ImageViewCreateInfo>(), 80);
        assert_eq!(offset_of!(ImageViewCreateInfo, range), 56);
        assert_eq!(size_of::<AttachmentDescription>(), 36);
        assert_eq!(size_of::<AttachmentDescription2>(), 56);
        assert_eq!(size_of::<SubpassDescription>(), 72);
        assert_eq!(offset_of!(SubpassDescription, depth), 48);
        assert_eq!(size_of::<SubpassDescription2>(), 88);
        assert_eq!(offset_of!(SubpassDescription2, depth), 64);
        assert_eq!(size_of::<AttachmentReference2>(), 32);
        assert_eq!(size_of::<DepthStencilResolve>(), 32);
        assert_eq!(size_of::<SubpassDependency>(), 28);
        assert_eq!(size_of::<RenderPassCreateInfo>(), 64);
        assert_eq!(size_of::<RenderPassCreateInfo2>(), 80);
        assert_eq!(size_of::<FramebufferCreateInfo>(), 64);
        assert_eq!(offset_of!(FramebufferCreateInfo, width), 48);
    }

    #[test]
    fn an_image_and_a_view_are_read_from_their_create_infos() {
        let info = ImageCreateInfo {
            s_type: 14,
            p_next: std::ptr::null(),
            flags: 0,
            image_type: 1,
            format: FORMAT_D32_SFLOAT,
            extent: [1667, 1349, 1],
            mips: 1,
            layers: 1,
            samples: 4,
            tiling: 0,
            usage: USAGE_DEPTH_STENCIL_ATTACHMENT | 0x1,
            sharing: 0,
            queue_count: 0,
            queues: std::ptr::null(),
            initial_layout: 0,
        };
        // SAFETY: a complete mirror of the structure.
        let image = unsafe { image_from(7, &info as *const _ as *const c_void) };
        assert_eq!(
            image,
            Image {
                device: 7,
                flags: 0,
                format: 126,
                extent: [1667, 1349, 1],
                mips: 1,
                layers: 1,
                samples: 4,
                usage: 0x21,
                swapchain: false
            }
        );
        let view = ImageViewCreateInfo {
            s_type: 15,
            p_next: std::ptr::null(),
            flags: 0,
            image: 0xabc,
            view_type: 1,
            format: 126,
            components: [0; 4],
            range: [0x2, 0, 1, 0, 1],
        };
        // SAFETY: as above.
        let v = unsafe { view_from(7, &view as *const _ as *const c_void) };
        assert_eq!((v.image, v.format, v.aspect, v.levels, v.layers), (0xabc, 126, 2, 1, 1));
    }

    #[test]
    fn a_render_pass_one_with_msaa_colour_resolve_and_depth() {
        // Attachment 0: 4x colour, 1: its single-sample resolve, 2: 4x depth.
        let atts = [
            AttachmentDescription([0, 44, 4, 1, 1, 2, 2, 0, 2]),
            AttachmentDescription([0, 44, 1, 2, 0, 2, 2, 0, 1_000_001_002]),
            AttachmentDescription([0, 126, 4, 1, 1, 2, 2, 0, 3]),
        ];
        let color = [[0u32, 2]];
        let resolve = [[1u32, 2]];
        let depth = [2u32, 3];
        let sub = SubpassDescription {
            flags: 0,
            bind_point: 0,
            input_count: 0,
            input: std::ptr::null(),
            color_count: 1,
            color: color.as_ptr(),
            resolve: resolve.as_ptr(),
            depth: &depth,
            preserve_count: 0,
            preserve: std::ptr::null(),
        };
        let deps = [SubpassDependency([!0, 0, 0x400, 0x400, 0, 0x100, 0])];
        let info = RenderPassCreateInfo {
            s_type: 38,
            p_next: std::ptr::null(),
            flags: 0,
            attachment_count: 3,
            attachments: atts.as_ptr() as *const c_void,
            subpass_count: 1,
            subpasses: &sub as *const _ as *const c_void,
            dependency_count: 1,
            dependencies: deps.as_ptr() as *const c_void,
        };
        // SAFETY: every pointer in the mirror is live for the call.
        let rp = unsafe { render_pass_from(1, &info as *const _ as *const c_void, false) };
        assert_eq!(rp.version, 1);
        assert_eq!(rp.attachments.len(), 3);
        assert_eq!(
            rp.attachments[2],
            Attachment {
                format: 126,
                samples: 4,
                load: 1,
                store: 1,
                stencil_load: 2,
                stencil_store: 2,
                initial_layout: 0,
                final_layout: 3
            }
        );
        assert_eq!(rp.attachments[1].final_layout, 1_000_001_002);
        let s = &rp.subpasses[0];
        assert_eq!(s.color, vec![Ref { attachment: 0, layout: 2 }]);
        assert_eq!(s.resolve, vec![Ref { attachment: 1, layout: 2 }]);
        assert_eq!(s.depth, Some(Ref { attachment: 2, layout: 3 }));
        assert_eq!(s.depth_resolve, None);
        assert_eq!(rp.dependencies[0].src_subpass, !0);
        assert_eq!(rp.dependencies[0].dst_access, 0x100);
    }

    #[test]
    fn a_render_pass_two_with_a_depth_resolve_in_the_chain() {
        let att = |format, samples, store, final_layout| AttachmentDescription2 {
            s_type: 1_000_109_000,
            p_next: std::ptr::null(),
            body: [0, format, samples, 1, store, 2, 2, 0, final_layout],
        };
        let atts = [att(126, 4, 1, 3), att(126, 1, 0, 6)];
        let r = |attachment, layout| AttachmentReference2 {
            s_type: 1_000_109_001,
            p_next: std::ptr::null(),
            attachment,
            layout,
            aspect: 2,
        };
        let depth = r(0, 3);
        let resolved = r(1, 3);
        let ds = DepthStencilResolve {
            s_type: ST_SUBPASS_DEPTH_STENCIL_RESOLVE,
            p_next: std::ptr::null(),
            depth_mode: 1,
            stencil_mode: 0,
            attachment: &resolved,
        };
        let sub = SubpassDescription2 {
            s_type: 1_000_109_002,
            p_next: &ds as *const _ as *const c_void,
            flags: 0,
            bind_point: 0,
            view_mask: 0,
            input_count: 0,
            input: std::ptr::null(),
            color_count: 0,
            color: std::ptr::null(),
            resolve: std::ptr::null(),
            depth: &depth,
            preserve_count: 0,
            preserve: std::ptr::null(),
        };
        let info = RenderPassCreateInfo2 {
            head: RenderPassCreateInfo {
                s_type: 1_000_109_004,
                p_next: std::ptr::null(),
                flags: 0,
                attachment_count: 2,
                attachments: atts.as_ptr() as *const c_void,
                subpass_count: 1,
                subpasses: &sub as *const _ as *const c_void,
                dependency_count: 0,
                dependencies: std::ptr::null(),
            },
            correlated_count: 0,
            correlated: std::ptr::null(),
        };
        // SAFETY: every pointer in the mirror is live for the call.
        let rp = unsafe { render_pass_from(1, &info as *const _ as *const c_void, true) };
        assert_eq!(rp.version, 2);
        assert_eq!((rp.attachments[0].samples, rp.attachments[1].final_layout), (4, 6));
        let s = &rp.subpasses[0];
        assert_eq!(s.depth, Some(Ref { attachment: 0, layout: 3 }));
        assert_eq!(s.depth_resolve, Some(Ref { attachment: 1, layout: 3 }));
        assert_eq!(s.depth_resolve_mode, 1);
        assert!(s.color.is_empty() && s.resolve.is_empty());
    }

    #[test]
    fn attachments_are_followed_through_framebuffer_view_and_image() {
        let views = [0x10u64, 0x11];
        let fb = FramebufferCreateInfo {
            s_type: 37,
            p_next: std::ptr::null(),
            flags: 0,
            render_pass: 0x500,
            attachment_count: 2,
            attachments: views.as_ptr(),
            width: 1667,
            height: 1349,
            layers: 1,
        };
        // SAFETY: a complete mirror with a live view array.
        let fb = unsafe { framebuffer_from(1, &fb as *const _ as *const c_void) };
        assert_eq!((fb.render_pass, fb.width, fb.height, fb.views.clone(), fb.imageless), (0x500, 1667, 1349, views.to_vec(), false));

        let mut reg = Registry::default();
        let depth = Attachment { format: 126, samples: 4, load: 1, store: 1, stencil_load: 2, stencil_store: 2, initial_layout: 0, final_layout: 3 };
        let color = Attachment { format: 44, samples: 1, load: 1, store: 0, final_layout: 1_000_001_002, ..depth };
        reg.render_passes.insert(
            0x500,
            RenderPass { device: 1, version: 1, attachments: vec![color, depth], subpasses: vec![], dependencies: vec![] },
        );
        reg.framebuffers.insert(0x600, fb);
        let image = |format, samples, swapchain| Image {
            device: 1,
            flags: 0,
            format,
            extent: [1667, 1349, 1],
            mips: 1,
            layers: 1,
            samples,
            usage: 0x10,
            swapchain,
        };
        reg.images.insert(0xa0, image(44, 1, true));
        reg.images.insert(0xa1, image(126, 4, false));
        let view = |image| View { device: 1, image, view_type: 1, format: 0, aspect: 1, base_mip: 0, levels: 1, base_layer: 0, layers: 1 };
        reg.views.insert(0x10, view(0xa0));
        reg.views.insert(0x11, view(0xa1));

        let got = reg.attachments(0x500, 0x600, &[]);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].view, got[0].image), (0x10, 0xa0));
        assert!(got[0].info.as_ref().unwrap().swapchain);
        assert_eq!(got[1].info.as_ref().unwrap().samples, 4);
        assert_eq!(got[1].desc, depth);

        // Unknown render pass: nothing; unknown view: attachment kept, image unknown.
        assert!(reg.attachments(0x501, 0x600, &[]).is_empty());
        reg.views.remove(&0x11);
        let got = reg.attachments(0x500, 0x600, &[]);
        assert_eq!((got[1].image, got[1].info.clone()), (0, None));

        // An imageless framebuffer takes the views of the begin info.
        reg.framebuffers.get_mut(&0x600).unwrap().imageless = true;
        let got = reg.attachments(0x500, 0x600, &[0x10]);
        assert_eq!((got[0].image, got[1].view), (0xa0, 0));

        reg.forget_device(1);
        assert!(reg.images.is_empty() && reg.views.is_empty() && reg.render_passes.is_empty() && reg.framebuffers.is_empty());
    }

    #[test]
    fn a_recreated_swapchain_keeps_the_images_it_shares_with_the_old_one() {
        let mut reg = Registry::default();
        reg.add_swapchain(1, 0x1, &[0xa, 0xb, 0xc], [1667, 1349], 44);
        // Recreated with 0x1 as its old swapchain: two handles come back.
        reg.add_swapchain(1, 0x2, &[0xa, 0xb, 0xd], [1667, 1349], 44);
        reg.forget_swapchain(0x1);
        assert!(reg.images.contains_key(&0xa) && reg.images.contains_key(&0xb) && reg.images.contains_key(&0xd));
        assert!(!reg.images.contains_key(&0xc));
        assert!(reg.images[&0xa].swapchain);
        assert_eq!(reg.swapchain_extents(), vec![[1667, 1349]]);
        reg.forget_swapchain(0x2);
        assert!(reg.images.is_empty() && reg.swapchain_extents().is_empty());
        reg.forget_swapchain(0x2);
    }

    #[test]
    fn names_for_formats_layouts_and_ops() {
        assert_eq!(format_name(126), "D32_SFLOAT(126)");
        assert_eq!(format_name(1234), "format1234");
        assert!(is_depth_format(126) && is_depth_format(129) && !is_depth_format(127) && !is_depth_format(44));
        assert_eq!(layout_name(1_000_001_002), "PRESENT_SRC");
        assert_eq!((load_name(1), store_name(1)), ("CLEAR", "DONT_CARE"));
    }
}
