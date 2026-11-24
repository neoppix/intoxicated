//! Ray-traced reflections, a prototype: `INTOXICATED_RT=1`. Off by default;
//! without the switch nothing here is reached and the engine's instance,
//! device and shaders are exactly what it asked for.
//!
//! # The device
//!
//! With the switch on, [`extend_device`] adds to the engine's own
//! `vkCreateDevice` what the reflections need and the GPU offers, and drops
//! nothing of the engine's:
//!
//! * hardware ray queries: `VK_KHR_acceleration_structure`,
//!   `VK_KHR_ray_query`, `VK_KHR_deferred_host_operations` and buffer device
//!   addresses (core from Vulkan 1.2; before it also `VK_KHR_buffer_device_address`,
//!   `VK_EXT_descriptor_indexing`, `VK_KHR_spirv_1_4` and
//!   `VK_KHR_shader_float_controls`), each with its feature turned on;
//! * `VK_EXT_transform_feedback`, by which the scene's geometry is captured.
//!
//! Each is added only when the driver lists the extension and reports the
//! feature, and either set is added only whole. A feature structure the
//! engine already chains has the one feature bit set in place, and put back
//! after the call, so the chain never holds a structure twice; a structure it
//! does not chain is put in front of its chain. What was enabled is printed.
//!
//! So that the device may have extensions that need Vulkan 1.1, the instance
//! is created with at least API version 1.2 when the switch is on and the
//! engine asked for less (see [`raise_api_version`]). That only allows more;
//! the engine keeps using the API it asked for.

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub(crate) mod capture;
pub(crate) mod spirv;
pub(crate) mod trace;


/// The switch.
pub const ENV: &str = "INTOXICATED_RT";

/// What is said once when the switch is on, and in `rt stats`.
pub const WARNING: &str = "ray-traced reflections are experimental: they replace the screen-space ones, \
off-screen hits show textures only where the game's shader samples them plainly (clothing) and flat colours elsewhere, in-world interface does not reflect, they can flicker while the camera moves, \
they cost up to an eighth of the video memory (at most about 1.6 GB, INTOXICATED_RT_MEMORY) and about 1 to 2 ms of GPU time (compute on the shader cores; NVIDIA's FreeBSD \
driver has no hardware ray tracing), and they were only tried on an RTX 4070 Ti with NVIDIA 620.70.06";

/// Whether the switch was on at start, read once; the first time it is on
/// the warning is printed.
pub(crate) fn requested() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var(ENV).is_ok_and(|v| v.trim() == "1");
        if on {
            println!("[android] vulkan: rt: WARNING: {WARNING} ({ENV}=1)");
        }
        on
    })
}

const VK_SUCCESS: i32 = 0;
const VK_INCOMPLETE: i32 = 5;

const fn api(major: u32, minor: u32) -> u32 {
    (major << 22) | (minor << 12)
}

const API_1_1: u32 = api(1, 1);
const API_1_2: u32 = api(1, 2);

const ST_APPLICATION_INFO: u32 = 0;
const ST_PHYSICAL_DEVICE_FEATURES_2: u32 = 1_000_059_000;
const ST_VULKAN_1_2_FEATURES: u32 = 51;
const ST_BUFFER_DEVICE_ADDRESS_FEATURES: u32 = 1_000_257_000;
const ST_ACCELERATION_STRUCTURE_FEATURES: u32 = 1_000_150_013;
const ST_RAY_QUERY_FEATURES: u32 = 1_000_348_013;
const ST_TRANSFORM_FEEDBACK_FEATURES: u32 = 1_000_028_000;

/// Byte offsets of the one feature bit wanted in each structure.
const AT_VULKAN_1_2_BUFFER_DEVICE_ADDRESS: usize = 16 + 38 * 4;
/// shaderSampledImageArrayNonUniformIndexing in the Vulkan 1.2 features and
/// in the descriptor indexing ones; shaderSampledImageArrayDynamicIndexing
/// in `VkPhysicalDeviceFeatures` and where that sits in its Features2.
const AT_VULKAN_1_2_SAMPLED_ARRAY_NON_UNIFORM: usize = 16 + 14 * 4;
const AT_DESCRIPTOR_INDEXING_SAMPLED_ARRAY_NON_UNIFORM: usize = 16 + 4 * 4;
const AT_FEATURES_SAMPLED_ARRAY_DYNAMIC: usize = 34 * 4;
const AT_FEATURES_2_SAMPLED_ARRAY_DYNAMIC: usize = 16 + AT_FEATURES_SAMPLED_ARRAY_DYNAMIC;
const ST_DESCRIPTOR_INDEXING_FEATURES: u32 = 1_000_161_001;
const AT_FIRST_FEATURE: usize = 16;

/// The API version the engine asked its instance for, 0 before it made one.
static ENGINE_API_VERSION: AtomicU32 = AtomicU32::new(0);
/// The API version the instance was made with.
static INSTANCE_API_VERSION: AtomicU32 = AtomicU32::new(0);

pub(crate) fn version_text(v: u32) -> String {
    format!("{}.{}.{}", (v >> 22) & 0x7f, (v >> 12) & 0x3ff, v & 0xfff)
}

/// `VkApplicationInfo` with `apiVersion` raised, kept alive across
/// `vkCreateInstance` with the create info that points at it.
pub(crate) struct RaisedInstanceInfo {
    info: Box<[u8; 64]>,
    app: Box<[u8; 48]>,
}

impl RaisedInstanceInfo {
    pub(crate) fn as_ptr(&self) -> *const c_void {
        self.info.as_ptr() as *const c_void
    }
}

/// With the switch on, a copy of the engine's `VkInstanceCreateInfo` (64
/// bytes) whose application info asks for API 1.2 when the engine asked for
/// less. Records what the engine asked for either way.
///
/// # Safety
/// `info` must be the engine's valid `VkInstanceCreateInfo` (or a copy).
pub(crate) unsafe fn raise_api_version(info: *const c_void) -> Option<RaisedInstanceInfo> {
    if info.is_null() {
        return None;
    }
    let app: *const c_void = get(info, 24);
    // An absent application info means 1.0.
    let asked = if app.is_null() { api(1, 0) } else { get::<u32>(app, 44) };
    ENGINE_API_VERSION.store(asked.max(1), Relaxed);
    INSTANCE_API_VERSION.store(asked, Relaxed);
    if !requested() || asked >= API_1_2 {
        return None;
    }
    let mut raised = RaisedInstanceInfo { info: Box::new(get(info, 0)), app: Box::new([0; 48]) };
    if !app.is_null() {
        *raised.app = get(app, 0);
    } else {
        put(raised.app.as_mut_ptr() as *mut c_void, 0, ST_APPLICATION_INFO);
    }
    put(raised.app.as_mut_ptr() as *mut c_void, 44, API_1_2);
    let app_ptr = raised.app.as_ptr();
    put(raised.info.as_mut_ptr() as *mut c_void, 24, app_ptr);
    INSTANCE_API_VERSION.store(API_1_2, Relaxed);
    println!(
        "[android] vulkan: rt: instance API version {} -> {} ({ENV}=1)",
        version_text(asked),
        version_text(API_1_2)
    );
    Some(raised)
}

/// What the reflections got on one device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Enabled {
    /// Hardware ray queries against acceleration structures.
    pub ray_query: bool,
    /// Transform feedback, for capturing the scene's geometry.
    pub transform_feedback: bool,
    /// Arrays of sampled images indexed per invocation, for the textures of
    /// what a ray hits.
    pub textures: bool,
}

static DEVICES: Mutex<Vec<(usize, Enabled)>> = Mutex::new(Vec::new());

/// What was enabled on `device`; nothing for a device made without the
/// switch.
pub(crate) fn enabled(device: usize) -> Enabled {
    DEVICES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(d, _)| *d == device).map_or_else(Enabled::default, |(_, e)| *e)
}

/// The engine's `VkDeviceCreateInfo` with our extensions and features, kept
/// alive across the call. Dropping it puts back the feature bits it set in
/// the engine's own structures.
pub(crate) struct ExtendedDeviceInfo {
    info: Box<[u8; 72]>,
    _names: Vec<*const c_char>,
    _structs: Vec<Box<[u64; 8]>>,
    /// `VkPhysicalDeviceFeatures` of ours, when the engine passed none.
    _features: Option<Box<[u64; 32]>>,
    restore: Vec<(usize, u32)>,
    added: Vec<String>,
    pub(crate) enabled: Enabled,
}

impl ExtendedDeviceInfo {
    pub(crate) fn as_ptr(&self) -> *const c_void {
        self.info.as_ptr() as *const c_void
    }
}

impl Drop for ExtendedDeviceInfo {
    fn drop(&mut self) {
        for &(at, old) in self.restore.iter().rev() {
            // SAFETY: an address in the engine's own feature structure that
            // was written during the call, put back as it was.
            unsafe { std::ptr::write_unaligned(at as *mut u32, old) };
        }
    }
}

/// The device's extensions by name.
fn device_extensions(pd: *mut c_void) -> Vec<String> {
    let f = super::vk_layer::host_instance_proc(c"vkEnumerateDeviceExtensionProperties");
    if f.is_null() {
        return Vec::new();
    }
    type Fn_ = extern "C" fn(*mut c_void, *const c_char, *mut u32, *mut [u8; 260]) -> i32;
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    for _ in 0..4 {
        let mut n = 0u32;
        if f(pd, std::ptr::null(), &mut n, std::ptr::null_mut()) != VK_SUCCESS {
            return Vec::new();
        }
        let mut list = vec![[0u8; 260]; n as usize];
        match f(pd, std::ptr::null(), &mut n, list.as_mut_ptr()) {
            VK_SUCCESS => {
                return list
                    .iter()
                    .take(n as usize)
                    .map(|p| {
                        let end = p[..256].iter().position(|&b| b == 0).unwrap_or(256);
                        String::from_utf8_lossy(&p[..end]).into_owned()
                    })
                    .collect()
            }
            VK_INCOMPLETE => continue,
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

/// The features this module cares about, as the driver reports them.
#[derive(Default, Debug)]
struct Supported {
    acceleration_structure: bool,
    ray_query: bool,
    buffer_device_address: bool,
    transform_feedback: bool,
    /// shaderSampledImageArrayDynamicIndexing and
    /// shaderSampledImageArrayNonUniformIndexing.
    textures: bool,
}

fn supported_features(pd: *mut c_void) -> Supported {
    let mut f = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceFeatures2");
    if f.is_null() {
        f = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceFeatures2KHR");
    }
    if f.is_null() {
        return Supported::default();
    }
    type Fn_ = extern "C" fn(*mut c_void, *mut u64);
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    // Each structure in its own zeroed buffer, larger than it is.
    let mut xfb = [0u64; 8];
    let mut bda = [0u64; 8];
    let mut rq = [0u64; 8];
    let mut acc = [0u64; 8];
    let mut f2 = [0u64; 64];
    let link = |s: &mut [u64; 8], st: u32, next: *mut u64| {
        s[0] = st as u64;
        s[1] = next as u64;
    };
    link(&mut xfb, ST_TRANSFORM_FEEDBACK_FEATURES, std::ptr::null_mut());
    link(&mut bda, ST_BUFFER_DEVICE_ADDRESS_FEATURES, xfb.as_mut_ptr());
    link(&mut rq, ST_RAY_QUERY_FEATURES, bda.as_mut_ptr());
    link(&mut acc, ST_ACCELERATION_STRUCTURE_FEATURES, rq.as_mut_ptr());
    let mut v12 = [0u64; 32];
    v12[0] = ST_VULKAN_1_2_FEATURES as u64;
    v12[1] = acc.as_mut_ptr() as u64;
    f2[0] = ST_PHYSICAL_DEVICE_FEATURES_2 as u64;
    f2[1] = v12.as_mut_ptr() as u64;
    f(pd, f2.as_mut_ptr());
    let first = |s: &[u64; 8]| s[2] as u32 != 0;
    let bit = |s: &[u64], at: usize| {
        // SAFETY: `at` is within the buffer, which is larger than the
        // structure it holds.
        unsafe { std::ptr::read_unaligned((s.as_ptr() as *const u8).add(at) as *const u32) != 0 }
    };
    Supported {
        acceleration_structure: first(&acc),
        ray_query: first(&rq),
        buffer_device_address: first(&bda),
        transform_feedback: first(&xfb),
        textures: bit(&f2, AT_FEATURES_2_SAMPLED_ARRAY_DYNAMIC) && bit(&v12, AT_VULKAN_1_2_SAMPLED_ARRAY_NON_UNIFORM),
    }
}

fn device_api_version(pd: *mut c_void) -> u32 {
    let f = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceProperties");
    if f.is_null() {
        return 0;
    }
    type Fn_ = extern "C" fn(*mut c_void, *mut u8);
    // SAFETY: resolved from the host loader for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let mut buffer = [0u64; 128];
    f(pd, buffer.as_mut_ptr() as *mut u8);
    buffer[0] as u32
}

/// What to add for one set, decided from what the driver offers.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    extensions: Vec<&'static CStr>,
    ray_query: bool,
    transform_feedback: bool,
    /// Why hardware ray queries are not enabled.
    no_ray_query: Option<String>,
    no_transform_feedback: Option<String>,
}

/// Which extensions to add, given the driver's list, its features and the
/// API version the device will run at. Pure, for the tests.
fn plan(have: &[String], sup: &Supported, effective_api: u32) -> Plan {
    let has = |n: &CStr| have.iter().any(|h| h.as_bytes() == n.to_bytes());
    let mut rq: Vec<&'static CStr> =
        vec![c"VK_KHR_acceleration_structure", c"VK_KHR_ray_query", c"VK_KHR_deferred_host_operations"];
    if effective_api < API_1_2 {
        rq.extend([
            c"VK_KHR_buffer_device_address",
            c"VK_EXT_descriptor_indexing",
            c"VK_KHR_spirv_1_4",
            c"VK_KHR_shader_float_controls",
        ]);
    }
    let missing: Vec<String> = rq.iter().filter(|n| !has(n)).map(|n| n.to_string_lossy().into_owned()).collect();
    let no_ray_query = if effective_api < API_1_1 {
        Some(format!("the device runs at Vulkan {}, below 1.1", version_text(effective_api)))
    } else if !missing.is_empty() {
        Some(format!("the driver lacks {}", missing.join(", ")))
    } else if !(sup.acceleration_structure && sup.ray_query && sup.buffer_device_address) {
        Some(format!(
            "the driver reports accelerationStructure {}, rayQuery {}, bufferDeviceAddress {}",
            sup.acceleration_structure, sup.ray_query, sup.buffer_device_address
        ))
    } else {
        None
    };
    let no_transform_feedback = if !has(c"VK_EXT_transform_feedback") {
        Some("the driver lacks VK_EXT_transform_feedback".to_string())
    } else if !sup.transform_feedback {
        Some("the driver reports transformFeedback false".to_string())
    } else {
        None
    };
    let mut extensions = Vec::new();
    if no_ray_query.is_none() {
        extensions.extend(rq);
    }
    if no_transform_feedback.is_none() {
        extensions.push(c"VK_EXT_transform_feedback");
    }
    Plan {
        extensions,
        ray_query: no_ray_query.is_none(),
        transform_feedback: no_transform_feedback.is_none(),
        no_ray_query,
        no_transform_feedback,
    }
}

/// With the switch on, the engine's `VkDeviceCreateInfo` (or the copy
/// `vulkan_etc` made of it) with what the reflections need added. `None`
/// with the switch off, or when there is nothing to add.
///
/// # Safety
/// `info` must be a valid `VkDeviceCreateInfo`.
pub(crate) unsafe fn extend_device(pd: *mut c_void, info: *const c_void) -> Option<ExtendedDeviceInfo> {
    if !requested() || info.is_null() {
        return None;
    }
    let instance_api = match INSTANCE_API_VERSION.load(Relaxed) {
        0 => api(1, 0),
        v => v,
    };
    let effective = instance_api.min(device_api_version(pd));
    let have = device_extensions(pd);
    let sup = supported_features(pd);
    let p = plan(&have, &sup, effective);
    println!(
        "[android] vulkan: rt: on \"{}\" (Vulkan {}): hardware ray queries {}; transform feedback {}",
        super::vk_layer::device_name(pd),
        version_text(effective),
        p.no_ray_query.as_ref().map_or("enabled".to_string(), |why| format!("not enabled, {why}")),
        p.no_transform_feedback.as_ref().map_or("enabled".to_string(), |why| format!("not enabled, {why}")),
    );
    if p.extensions.is_empty() {
        return None;
    }
    let enabled = Enabled { ray_query: p.ray_query, transform_feedback: p.transform_feedback, textures: p.transform_feedback && sup.textures };
    if p.transform_feedback && !sup.textures {
        println!("[android] vulkan: rt: the driver does not offer arrays of sampled images indexed per invocation; reflections show no textures");
    }
    let ext = apply(info, &p.extensions, enabled);
    println!(
        "[android] vulkan: rt: device extensions added: {} ({} feature bit(s) set in the engine's structures, {} structure(s) chained)",
        if ext.added.is_empty() { "none".to_string() } else { ext.added.join(", ") },
        ext.restore.len(),
        ext._structs.len(),
    );
    Some(ext)
}

/// The create info with `extensions` and the features of `enabled` added.
///
/// # Safety
/// `info` must be a valid `VkDeviceCreateInfo`.
unsafe fn apply(info: *const c_void, extensions: &[&'static CStr], enabled: Enabled) -> ExtendedDeviceInfo {
    let mut ext = ExtendedDeviceInfo {
        info: Box::new(get(info, 0)),
        _names: Vec::new(),
        _structs: Vec::new(),
        _features: None,
        restore: Vec::new(),
        added: Vec::new(),
        enabled,
    };
    // Extensions: the engine's, then each of ours it did not name itself.
    let (n, list): (u32, *const *const c_char) = (get(info, 48), get(info, 56));
    let engine: Vec<*const c_char> = if n == 0 || list.is_null() { Vec::new() } else { std::slice::from_raw_parts(list, n as usize).to_vec() };
    let named = |want: &CStr| engine.iter().any(|&e| !e.is_null() && CStr::from_ptr(e) == want);
    let mut names = engine.clone();
    for want in extensions {
        if !named(want) {
            ext.added.push(want.to_string_lossy().into_owned());
            names.push(want.as_ptr());
        }
    }
    let base = ext.info.as_mut_ptr() as *mut c_void;
    put(base, 48, names.len() as u32);
    put(base, 56, names.as_ptr());
    ext._names = names;
    // Features: set in place where the engine chains the structure, else a
    // structure of ours in front of its chain.
    let chain: *const c_void = get(info, 8);
    let mut head = chain;
    let mut want = |st: u32, at: usize, add_st: u32, ext: &mut ExtendedDeviceInfo| {
        let found = super::vk_layer::registry::find_in_chain(chain, st);
        if !found.is_null() {
            let addr = (found as usize) + at;
            let old = std::ptr::read_unaligned(addr as *const u32);
            if old == 0 {
                ext.restore.push((addr, old));
                std::ptr::write_unaligned(addr as *mut u32, 1);
            }
            return true;
        }
        if add_st == 0 {
            return false;
        }
        let mut s: Box<[u64; 8]> = Box::new([0; 8]);
        s[0] = add_st as u64;
        s[1] = head as u64;
        put(s.as_mut_ptr() as *mut c_void, AT_FIRST_FEATURE, 1u32);
        head = s.as_ptr() as *const c_void;
        ext._structs.push(s);
        true
    };
    if enabled.transform_feedback {
        want(ST_TRANSFORM_FEEDBACK_FEATURES, AT_FIRST_FEATURE, ST_TRANSFORM_FEEDBACK_FEATURES, &mut ext);
    }
    if enabled.ray_query {
        want(ST_ACCELERATION_STRUCTURE_FEATURES, AT_FIRST_FEATURE, ST_ACCELERATION_STRUCTURE_FEATURES, &mut ext);
        want(ST_RAY_QUERY_FEATURES, AT_FIRST_FEATURE, ST_RAY_QUERY_FEATURES, &mut ext);
        // Buffer device addresses: in the engine's Vulkan 1.2 structure when
        // it chains one (a second structure beside it is not allowed), else
        // in its own structure, else in ours.
        if !want(ST_VULKAN_1_2_FEATURES, AT_VULKAN_1_2_BUFFER_DEVICE_ADDRESS, 0, &mut ext) {
            want(ST_BUFFER_DEVICE_ADDRESS_FEATURES, AT_FIRST_FEATURE, ST_BUFFER_DEVICE_ADDRESS_FEATURES, &mut ext);
        }
    }
    if enabled.textures {
        // Dynamic indexing: in the engine's Features2, else in its
        // pEnabledFeatures, else in a copy of none.
        if !want(ST_PHYSICAL_DEVICE_FEATURES_2, AT_FEATURES_2_SAMPLED_ARRAY_DYNAMIC, 0, &mut ext) {
            let features: *const c_void = get(info, 64);
            if features.is_null() {
                let mut f: Box<[u64; 32]> = Box::new([0; 32]);
                put(f.as_mut_ptr() as *mut c_void, AT_FEATURES_SAMPLED_ARRAY_DYNAMIC, 1u32);
                put(base, 64, f.as_ptr());
                ext._features = Some(f);
            } else {
                let addr = features as usize + AT_FEATURES_SAMPLED_ARRAY_DYNAMIC;
                let old = std::ptr::read_unaligned(addr as *const u32);
                if old == 0 {
                    ext.restore.push((addr, old));
                    std::ptr::write_unaligned(addr as *mut u32, 1);
                }
            }
        }
        // Non-uniform indexing: in the engine's Vulkan 1.2 structure, else in
        // its descriptor indexing one, else in one of ours.
        if !want(ST_VULKAN_1_2_FEATURES, AT_VULKAN_1_2_SAMPLED_ARRAY_NON_UNIFORM, 0, &mut ext)
            && !want(ST_DESCRIPTOR_INDEXING_FEATURES, AT_DESCRIPTOR_INDEXING_SAMPLED_ARRAY_NON_UNIFORM, 0, &mut ext)
        {
            let mut s: Box<[u64; 8]> = Box::new([0; 8]);
            s[0] = ST_DESCRIPTOR_INDEXING_FEATURES as u64;
            s[1] = head as u64;
            put(s.as_mut_ptr() as *mut c_void, AT_DESCRIPTOR_INDEXING_SAMPLED_ARRAY_NON_UNIFORM, 1u32);
            head = s.as_ptr() as *const c_void;
            ext._structs.push(s);
        }
    }
    put(base, 8, head);
    ext
}

/// devctl `rt`: what the devices got.
pub fn control(args: &[&str]) -> Result<String, String> {
    match args {
        ["draws"] => capture::request_draws(),
        [w @ ("on" | "off")] => {
            if !requested() {
                return Err(format!("the ray-traced reflections are off ({ENV}=1 at start turns them on)"));
            }
            // The trace replaces the reflections' march, so `rt on` turns
            // them on as well; `rt off` leaves them as they are.
            if *w == "on" {
                super::ssr::reflections_on_for_rt()?;
            }
            trace::set_on(*w == "on");
            Ok(trace::stats_line())
        }
        ["dump", path] => capture::dump(path),
        ["dump-cache", path] => trace::dump_cache(path),
        ["env", w @ ("on" | "off")] => {
            trace::set_env(*w == "on");
            Ok(trace::stats_line())
        }
        ["gi", w @ ("on" | "off")] => {
            trace::set_gi(*w == "on")?;
            Ok(trace::stats_line())
        }
        ["view", name] => {
            trace::set_view(name)?;
            Ok(trace::stats_line())
        }
        ["kind", name, w @ ("on" | "off")] => {
            capture::set_kind(name, *w == "on")?;
            Ok(trace::stats_line())
        }
        ["shade", name] => {
            trace::set_shade(name)?;
            Ok(trace::stats_line())
        }
        [] | ["stats"] => {
            if !requested() {
                return Err(format!("the ray-traced reflections are off ({ENV}=1 at start turns them on)"));
            }
            let list: Vec<usize> = DEVICES.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(d, _)| *d).collect();
            let devices: Vec<String> = list
                .into_iter()
                .map(|dev| {
                    let e = enabled(dev);
                    format!("device=0x{dev:x} ray_query={} transform_feedback={}", e.ray_query, e.transform_feedback)
                })
                .collect();
            Ok(format!(
                "experimental=yes engine_api={} instance_api={} {} {} {} warning={WARNING:?}",
                version_text(ENGINE_API_VERSION.load(Relaxed)),
                version_text(INSTANCE_API_VERSION.load(Relaxed)),
                if devices.is_empty() { "no device".to_string() } else { devices.join(" ") },
                capture::stats_line(),
                trace::stats_line()
            ))
        }
        _ => Err("rt [stats] | on | off | shade lit|own|mix|screen | env on|off | gi on|off | view off|normal|source|category | kind neon|terrain on|off | draws | dump <path> | dump-cache <path>".into()),
    }
}

/// After a successful `vkCreateDevice` that [`extend_device`] changed.
pub(crate) fn device_created(device: *mut c_void, enabled: Enabled) {
    let mut d = DEVICES.lock().unwrap_or_else(|e| e.into_inner());
    d.retain(|(dev, _)| *dev != device as usize);
    d.push((device as usize, enabled));
}

/// Before `vkDestroyDevice` is forwarded.
pub(crate) fn device_destroying(device: *mut c_void) {
    if !requested() {
        return;
    }
    trace::device_destroying(device);
    DEVICES.lock().unwrap_or_else(|e| e.into_inner()).retain(|(dev, _)| *dev != device as usize);
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    const ALL_RT: [&str; 4] =
        ["VK_KHR_acceleration_structure", "VK_KHR_ray_query", "VK_KHR_deferred_host_operations", "VK_EXT_transform_feedback"];

    #[test]
    fn hardware_ray_queries_only_with_every_extension_and_feature() {
        let full =
            Supported { acceleration_structure: true, ray_query: true, buffer_device_address: true, transform_feedback: true, textures: true };
        let p = plan(&names(&ALL_RT), &full, API_1_2);
        assert!(p.ray_query && p.transform_feedback);
        assert_eq!(p.extensions.len(), 4);
        // Below 1.2 the extensions core 1.2 brings are needed too.
        let p = plan(&names(&ALL_RT), &full, API_1_1);
        assert!(!p.ray_query && p.transform_feedback);
        assert!(p.no_ray_query.as_ref().unwrap().contains("VK_KHR_spirv_1_4"));
        // The NVIDIA 580 driver on FreeBSD: transform feedback, no ray queries.
        let p = plan(&names(&["VK_EXT_transform_feedback", "VK_KHR_ray_tracing_position_fetch"]), &full, API_1_2);
        assert!(!p.ray_query && p.transform_feedback);
        assert_eq!(p.extensions, vec![c"VK_EXT_transform_feedback"]);
        assert!(p.no_ray_query.unwrap().contains("VK_KHR_acceleration_structure"));
        let none = Supported::default();
        let p = plan(&names(&ALL_RT), &none, API_1_2);
        assert!(!p.ray_query && !p.transform_feedback && p.extensions.is_empty());
        let p = plan(&names(&ALL_RT), &full, api(1, 0));
        assert!(!p.ray_query);
    }

    #[test]
    fn our_features_go_in_front_of_the_engines_chain_and_bits_are_put_back() {
        // The engine's chain: a Vulkan 1.2 structure with bufferDeviceAddress
        // off, then a transform feedback structure with its bit off.
        let mut engine_xfb: [u64; 4] = [ST_TRANSFORM_FEEDBACK_FEATURES as u64, 0, 0, 0];
        let mut v12 = [0u64; 32];
        v12[0] = ST_VULKAN_1_2_FEATURES as u64;
        v12[1] = engine_xfb.as_mut_ptr() as u64;
        let engine_names = [c"VK_KHR_swapchain".as_ptr(), c"VK_EXT_transform_feedback".as_ptr()];
        let mut info = [0u64; 9];
        info[1] = v12.as_mut_ptr() as u64;
        info[6] = engine_names.len() as u64;
        info[7] = engine_names.as_ptr() as u64;
        let wanted: [&CStr; 4] =
            [c"VK_KHR_acceleration_structure", c"VK_KHR_ray_query", c"VK_KHR_deferred_host_operations", c"VK_EXT_transform_feedback"];
        let both = Enabled { ray_query: true, transform_feedback: true, textures: false };
        unsafe {
            let ext = apply(info.as_ptr() as *const c_void, &wanted, both);
            let out = ext.as_ptr();
            // The engine's two names, then the three it did not name.
            let n: u32 = get(out, 48);
            let list: *const *const c_char = get(out, 56);
            let got: Vec<&CStr> = (0..n as usize).map(|i| CStr::from_ptr(*list.add(i))).collect();
            assert_eq!(got.len(), 5);
            assert_eq!(&got[..2], &[c"VK_KHR_swapchain", c"VK_EXT_transform_feedback"]);
            assert_eq!(ext.added.len(), 3);
            // Bits set in place in the engine's structures.
            assert_eq!(std::ptr::read_unaligned((v12.as_ptr() as *const u8).add(AT_VULKAN_1_2_BUFFER_DEVICE_ADDRESS) as *const u32), 1);
            assert_eq!(engine_xfb[2] as u32, 1);
            // Two structures of ours in front (acceleration structure, ray
            // query), and no second buffer device address structure.
            let mut node: *const c_void = get(out, 8);
            let mut types = Vec::new();
            while !node.is_null() {
                types.push(get::<u32>(node, 0));
                node = get(node, 8);
            }
            assert_eq!(
                types,
                vec![ST_RAY_QUERY_FEATURES, ST_ACCELERATION_STRUCTURE_FEATURES, ST_VULKAN_1_2_FEATURES, ST_TRANSFORM_FEEDBACK_FEATURES]
            );
            drop(ext);
        }
        // Dropped: the engine's structures are as they were.
        assert_eq!(v12.iter().skip(2).copied().max(), Some(0));
        assert_eq!(engine_xfb[2], 0);
        assert_eq!(info[1], v12.as_ptr() as u64, "the engine's own create info is not changed");
    }
}
