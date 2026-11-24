//! How a frame is built: render passes, dynamic rendering and the draws
//! inside them, seen through the commands the engine records, and the
//! registry ([`super::registry`]) of what they draw into.
//!
//! # When anything is handed out
//!
//! The hooks here, and the image and view hooks in `vulkan_etc` that feed
//! the registry, are handed out only when [`wanted`]: `INTOXICATED_TRACE_PASSES`
//! is set, `INTOXICATED_SSR=1`, `INTOXICATED_DOF=1` or `INTOXICATED_BLOOM=1`,
//! or dev control is on (devctl `passes`, `depthshot`, `ssr`, `dof` and
//! `bloom` need them). Otherwise the getters hand out exactly
//! what they did before.
//!
//! When they are handed out, the creation hooks (render passes,
//! framebuffers, images, views) always describe what was created, which is
//! rare and cheap next to the creation itself. The recording hooks (begin and
//! end of a pass, draws, copies, barriers, submits, and the command buffer
//! begin and compute binds the reflections follow) cost one relaxed load of
//! [`WATCH`] and the forward, until something is watching: a trace for one
//! frame, a depth capture until it is taken, or the effects recorded before
//! the swapchain pass (reflections, depth of field and bloom, all in
//! `android::ssr`) while any of them is on.
//!
//! # The trace
//!
//! `INTOXICATED_TRACE_PASSES=1` traces the frame after the 600th present;
//! any larger number names the present to trace after. devctl `passes`
//! traces the next frame. Everything recorded between that present and the
//! next is kept and written to stderr as one block of `[passes]` lines: per
//! pass its command buffer, its attachments (format, extent, samples,
//! load and store, layouts, swapchain or not), its subpasses and draw counts,
//! with copies, clears, dispatches, image barriers and submits between them
//! in the order they were recorded.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use super::registry::{self, get, Registry};

const VK_SUCCESS: i32 = 0;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;

/// The switch.
pub const TRACE_ENV: &str = "INTOXICATED_TRACE_PASSES";

/// The present a value of `1` traces after.
const DEFAULT_TRACE_AT: u64 = 600;

// ------------------------------------------------------------ switches

/// What the recording hooks are watching for. Zero: forward and nothing else.
pub(crate) static WATCH: AtomicU32 = AtomicU32::new(0);
pub(crate) const WATCH_TRACE: u32 = 0x1;
pub(crate) const WATCH_DEPTH: u32 = 0x2;
/// The reflections, the depth of field or the bloom (`android::ssr`).
pub(crate) const WATCH_POST: u32 = 0x4;
/// The scene's geometry captured for the ray-traced reflections
/// (`android::rt::capture`), with `INTOXICATED_RT=1` or its research switch.
pub(crate) const WATCH_RT: u32 = 0x8;

/// The present count at which the next trace starts; `u64::MAX` for none.
static TRACE_AT: AtomicU64 = AtomicU64::new(u64::MAX);

/// The present to trace after, from the switch's value: `1` (or any value
/// that is not a number above 1) is the default, a number above 1 is that
/// present, and unset, empty or `0` is off.
fn trace_start(value: Option<&str>) -> Option<u64> {
    let v = value?.trim();
    if v.is_empty() || v == "0" {
        return None;
    }
    Some(v.parse::<u64>().ok().filter(|&n| n > 1).unwrap_or(DEFAULT_TRACE_AT))
}

/// Whether the hooks are handed out at all, decided once.
pub(crate) fn wanted() -> bool {
    static WANTED: OnceLock<bool> = OnceLock::new();
    *WANTED.get_or_init(|| {
        let at = trace_start(std::env::var(TRACE_ENV).ok().as_deref());
        if let Some(n) = at {
            TRACE_AT.store(n, Relaxed);
            println!("[android] vulkan: the render passes of the frame after present {n} will be traced ({TRACE_ENV})");
        }
        let ssr = crate::android::ssr::requested_at_start();
        if ssr {
            WATCH.fetch_or(WATCH_POST, Relaxed);
        }
        let rt = crate::android::rt::capture::wanted();
        if rt {
            WATCH.fetch_or(WATCH_RT, Relaxed);
        }
        at.is_some() || ssr || rt || crate::devctl::enabled()
    })
}

// ------------------------------------------------------------ the registry

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

pub(crate) fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default())).lock().unwrap_or_else(|e| e.into_inner())
}

/// After a successful `vkCreateImage`; `info` is the engine's create info.
pub(crate) fn image_created(device: *mut c_void, info: *const c_void, image: u64) {
    if !wanted() || info.is_null() || image == 0 {
        return;
    }
    // SAFETY: the engine's `VkImageCreateInfo`, read during its own call.
    let i = unsafe { registry::image_from(device as usize, info) };
    registry().images.insert(image, i);
}

pub(crate) fn image_destroyed(image: u64) {
    if wanted() && image != 0 {
        registry().images.remove(&image);
        crate::android::ssr::image_destroyed(image);
    }
}

/// After a successful `vkCreateImageView`.
pub(crate) fn view_created(device: *mut c_void, info: *const c_void, view: u64) {
    if !wanted() || info.is_null() || view == 0 {
        return;
    }
    // SAFETY: the engine's `VkImageViewCreateInfo`, read during its own call.
    let v = unsafe { registry::view_from(device as usize, info) };
    registry().views.insert(view, v);
}

pub(crate) fn view_destroyed(view: u64) {
    if wanted() && view != 0 {
        registry().views.remove(&view);
    }
}

/// A swapchain's images, which the engine never creates itself.
pub(crate) fn swapchain_images(device: *mut c_void, swapchain: u64, images: &[u64], width: u32, height: u32, format: i32) {
    if !wanted() {
        return;
    }
    registry().add_swapchain(device as usize, swapchain, images, [width, height], format as u32);
}

pub(crate) fn swapchain_destroyed(swapchain: u64) {
    if !wanted() {
        return;
    }
    registry().forget_swapchain(swapchain);
}

/// Before `vkDestroyDevice` is forwarded.
pub(crate) fn device_destroying(device: *mut c_void) {
    if wanted() {
        registry().forget_device(device as usize);
    }
}

// ------------------------------------------------------------ what is recorded

/// Draw calls by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Draws {
    pub direct: u32,
    pub indexed: u32,
    /// Any of the indirect forms, counted per call.
    pub indirect: u32,
}

impl Draws {
    pub(crate) fn total(&self) -> u32 {
        self.direct + self.indexed + self.indirect
    }

    fn add(&mut self, other: Draws) {
        self.direct += other.direct;
        self.indexed += other.indexed;
        self.indirect += other.indirect;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawKind {
    Direct,
    Indexed,
    Indirect,
}

/// One attachment of a `vkCmdBeginRendering`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DynAttachment {
    pub view: u64,
    pub layout: u32,
    pub resolve_mode: u32,
    pub resolve_view: u64,
    pub resolve_layout: u32,
    pub load: u32,
    pub store: u32,
    pub clear: [u32; 4],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    RenderPass {
        render_pass: u64,
        framebuffer: u64,
        /// For an imageless framebuffer: the views the begin info named.
        views: Vec<u64>,
        contents: u32,
        version: u8,
    },
    Dynamic {
        flags: u32,
        layers: u32,
        colors: Vec<DynAttachment>,
        depth: Option<DynAttachment>,
        stencil: Option<DynAttachment>,
    },
}

/// One render pass instance, or one `vkCmdBeginRendering` to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pass {
    pub cmd: usize,
    pub target: Target,
    /// x, y, width, height.
    pub area: (i32, i32, u32, u32),
    /// Raw clear values, by attachment index.
    pub clears: Vec<[u32; 4]>,
    /// Draws per subpass, in the order the subpasses were begun.
    pub subpasses: Vec<Draws>,
    /// Secondary command buffers executed inside it.
    pub executed: Vec<usize>,
    pub ended: bool,
}

impl Pass {
    pub(crate) fn new(cmd: usize, target: Target, area: (i32, i32, u32, u32), clears: Vec<[u32; 4]>) -> Pass {
        Pass { cmd, target, area, clears, subpasses: vec![Draws::default()], executed: Vec::new(), ended: false }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Pass(Pass),
    /// A copy, blit or resolve between images, or an image to a buffer
    /// (`dst` 0).
    Transfer { cmd: usize, what: &'static str, src: u64, src_layout: u32, dst: u64, dst_layout: u32 },
    Clear { cmd: usize, what: &'static str, image: u64, layout: u32 },
    Barrier { cmd: usize, image: u64, old: u32, new: u32, src_stage: u64, dst_stage: u64 },
    Dispatch { cmd: usize, groups: [u32; 3] },
    Execute { cmd: usize, secondaries: Vec<usize> },
    Submit { queue: usize, cmds: Vec<usize> },
}

/// Everything recorded over one frame.
#[derive(Default)]
pub(crate) struct Recorder {
    pub events: Vec<Event>,
    /// Command buffer to the index of its open pass in `events`.
    open: HashMap<usize, usize>,
    /// Draws recorded outside any pass, by command buffer: in a secondary
    /// that continues a render pass, these are its draws.
    pub outside: HashMap<usize, Draws>,
}

impl Recorder {
    fn open_pass(&mut self, cmd: usize) -> Option<&mut Pass> {
        let i = *self.open.get(&cmd)?;
        match self.events.get_mut(i) {
            Some(Event::Pass(p)) => Some(p),
            _ => None,
        }
    }

    pub(crate) fn begin_pass(&mut self, pass: Pass) {
        self.open.insert(pass.cmd, self.events.len());
        self.events.push(Event::Pass(pass));
    }

    pub(crate) fn next_subpass(&mut self, cmd: usize) {
        if let Some(p) = self.open_pass(cmd) {
            p.subpasses.push(Draws::default());
        }
    }

    /// The pass `cmd` had open, now ended; `None` when it was begun before
    /// the recording started.
    pub(crate) fn end_pass(&mut self, cmd: usize) -> Option<&Pass> {
        let i = self.open.remove(&cmd)?;
        match self.events.get_mut(i) {
            Some(Event::Pass(p)) => {
                p.ended = true;
                Some(p)
            }
            _ => None,
        }
    }

    pub(crate) fn draw(&mut self, cmd: usize, kind: DrawKind) {
        let d = match self.open_pass(cmd) {
            Some(p) => p.subpasses.last_mut().expect("a pass has at least one subpass"),
            None => self.outside.entry(cmd).or_default(),
        };
        match kind {
            DrawKind::Direct => d.direct += 1,
            DrawKind::Indexed => d.indexed += 1,
            DrawKind::Indirect => d.indirect += 1,
        }
    }

    pub(crate) fn execute(&mut self, cmd: usize, secondaries: Vec<usize>) {
        if let Some(p) = self.open_pass(cmd) {
            p.executed.extend(&secondaries);
        }
        self.events.push(Event::Execute { cmd, secondaries });
    }

    /// A pass's draws, its own and those of the secondaries it executed.
    pub(crate) fn pass_draws(&self, p: &Pass) -> Draws {
        let mut d = Draws::default();
        p.subpasses.iter().for_each(|s| d.add(*s));
        for s in &p.executed {
            if let Some(o) = self.outside.get(s) {
                d.add(*o);
            }
        }
        d
    }
}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

fn recorder() -> MutexGuard<'static, Option<Recorder>> {
    RECORDER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` on the recorder, if one is recording.
fn record(f: impl FnOnce(&mut Recorder)) {
    if let Some(r) = recorder().as_mut() {
        f(r);
    }
}

// ------------------------------------------------------------ frames

/// Whoever asked for the trace in progress through devctl, waiting for its
/// one-line summary.
static TRACE_REPLY: Mutex<Option<Sender<String>>> = Mutex::new(None);

/// From `vkQueuePresentKHR`, with the presents so far counting this one:
/// ends a trace in progress, and starts one when its present has come.
pub(crate) fn on_present(presents: u64) {
    let w = WATCH.load(Relaxed);
    if w & WATCH_TRACE != 0 {
        finish_trace(presents);
    }
    if w & WATCH_DEPTH != 0 {
        crate::android::depth_capture::frame_end(presents);
    }
    // Also while off: objects it replaced are destroyed some presents later.
    crate::android::ssr::frame_end(presents);
    if w & WATCH_RT != 0 {
        crate::android::rt::capture::frame_end(presents);
    }
    if presents >= TRACE_AT.load(Relaxed) {
        TRACE_AT.store(u64::MAX, Relaxed);
        *recorder() = Some(Recorder::default());
        WATCH.fetch_or(WATCH_TRACE, Relaxed);
    }
}

fn finish_trace(presents: u64) {
    WATCH.fetch_and(!WATCH_TRACE, Relaxed);
    let Some(rec) = recorder().take() else { return };
    let lines = {
        let reg = registry();
        report(&rec, &reg, presents)
    };
    for l in &lines[1..] {
        eprintln!("[passes] {l}");
    }
    eprintln!("[passes] {}", lines[0]);
    if let Some(tx) = TRACE_REPLY.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = tx.send(lines[0].clone());
    }
}

/// devctl `passes`: trace the next frame and answer with its summary line;
/// the whole trace goes to stderr.
pub fn request_trace() -> Result<String, String> {
    if !wanted() {
        return Err("the pass hooks are not handed out in this run".into());
    }
    let (tx, rx) = mpsc::channel();
    *TRACE_REPLY.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    let now = super::super::glcount::QUEUE_PRESENT.load(Relaxed);
    TRACE_AT.store(now + 1, Relaxed);
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "no frame was traced within five seconds".to_string())
}

// ------------------------------------------------------------ the report

fn hex(h: impl Into<u64>) -> String {
    format!("0x{:x}", h.into())
}

fn clear_text(format: u32, c: [u32; 4]) -> String {
    if registry::is_depth_format(format) {
        format!("depth {} stencil {}", f32::from_bits(c[0]), c[1])
    } else {
        let f = c.map(f32::from_bits);
        format!("({}, {}, {}, {})", f[0], f[1], f[2], f[3])
    }
}

fn image_text(image: u64, info: Option<&registry::Image>) -> String {
    match info {
        Some(i) => format!(
            "{} {}x{} x{} usage 0x{:x} image {}{}",
            registry::format_name(i.format),
            i.extent[0],
            i.extent[1],
            i.samples,
            i.usage,
            hex(image),
            if i.swapchain { " SWAPCHAIN" } else { "" }
        ),
        None => format!("image {} (not in the registry)", hex(image)),
    }
}

fn refs_text(refs: &[registry::Ref]) -> String {
    let v: Vec<String> = refs
        .iter()
        .map(|r| if r.attachment == registry::UNUSED { "-".into() } else { r.attachment.to_string() })
        .collect();
    format!("[{}]", v.join(","))
}

/// The trace as lines: the summary first, then the passes and everything
/// between them in recording order.
pub(crate) fn report(rec: &Recorder, reg: &Registry, presents: u64) -> Vec<String> {
    let mut out = vec![String::new()];
    // Which submit carried each command buffer, for the pass lines.
    let mut submit_of: HashMap<usize, usize> = HashMap::new();
    let mut submits = 0;
    for e in &rec.events {
        if let Event::Submit { cmds, .. } = e {
            for c in cmds {
                submit_of.entry(*c).or_insert(submits);
            }
            submits += 1;
        }
    }
    let carried = |cmd: usize| submit_of.get(&cmd).map_or("not submitted in this frame".to_string(), |s| format!("submit {s}"));
    let (mut passes, mut draws, mut cmds) = (0, 0, Vec::<usize>::new());
    for e in &rec.events {
        match e {
            Event::Pass(p) => {
                if !cmds.contains(&p.cmd) {
                    cmds.push(p.cmd);
                }
                let d = rec.pass_draws(p);
                draws += d.total();
                let (x, y, w, h) = p.area;
                let head = format!(
                    "pass {passes}: cmd {} ({}), area {w}x{h}+{x}+{y}, {} draw(s) [direct {}, indexed {}, indirect {}]{}{}",
                    hex(p.cmd as u64),
                    carried(p.cmd),
                    d.total(),
                    d.direct,
                    d.indexed,
                    d.indirect,
                    if p.executed.is_empty() { String::new() } else { format!(", {} secondary cmd(s)", p.executed.len()) },
                    if p.ended { "" } else { ", NOT ENDED in this frame" },
                );
                passes += 1;
                match &p.target {
                    Target::RenderPass { render_pass, framebuffer, views, contents, version } => {
                        out.push(format!(
                            "{head}; vkCmdBeginRenderPass{} renderpass {} framebuffer {}{}",
                            if *version == 2 { "2" } else { "" },
                            hex(*render_pass),
                            hex(*framebuffer),
                            if *contents == 1 { ", contents SECONDARY_COMMAND_BUFFERS" } else { "" }
                        ));
                        let atts = reg.attachments(*render_pass, *framebuffer, views);
                        if atts.is_empty() {
                            out.push("  (render pass not in the registry)".into());
                        }
                        for a in &atts {
                            let d = &a.desc;
                            out.push(format!(
                                "  att {}: {} view {}; pass says {} x{}, load {} store {}{}, {} -> {}{}",
                                a.index,
                                image_text(a.image, a.info.as_ref()),
                                hex(a.view),
                                registry::format_name(d.format),
                                d.samples,
                                registry::load_name(d.load),
                                registry::store_name(d.store),
                                if registry::is_depth_format(d.format) && d.format != registry::FORMAT_D32_SFLOAT && d.format != registry::FORMAT_D16_UNORM {
                                    format!(" (stencil load {} store {})", registry::load_name(d.stencil_load), registry::store_name(d.stencil_store))
                                } else {
                                    String::new()
                                },
                                registry::layout_name(d.initial_layout),
                                registry::layout_name(d.final_layout),
                                match p.clears.get(a.index as usize) {
                                    Some(c) if d.load == 1 || d.stencil_load == 1 => format!(", clear {}", clear_text(d.format, *c)),
                                    _ => String::new(),
                                }
                            ));
                        }
                        if let Some(rp) = reg.render_passes.get(render_pass) {
                            for (i, s) in rp.subpasses.iter().enumerate() {
                                out.push(format!(
                                    "  subpass {i}: color {} resolve {} depth {} input {}{}, {} draw(s)",
                                    refs_text(&s.color),
                                    refs_text(&s.resolve),
                                    s.depth.map_or("-".to_string(), |r| format!("{} ({})", r.attachment, registry::layout_name(r.layout))),
                                    refs_text(&s.input),
                                    s.depth_resolve.map_or(String::new(), |r| format!(
                                        " depth-resolve {} (mode {}, stencil mode {})",
                                        r.attachment, s.depth_resolve_mode, s.stencil_resolve_mode
                                    )),
                                    p.subpasses.get(i).map_or(0, |d| d.total()),
                                ));
                            }
                            for d in &rp.dependencies {
                                let sub = |s: u32| if s == !0 { "EXTERNAL".to_string() } else { s.to_string() };
                                out.push(format!(
                                    "  dependency {} -> {}: stages 0x{:x} -> 0x{:x}, access 0x{:x} -> 0x{:x}, flags 0x{:x}",
                                    sub(d.src_subpass),
                                    sub(d.dst_subpass),
                                    d.src_stage,
                                    d.dst_stage,
                                    d.src_access,
                                    d.dst_access,
                                    d.flags
                                ));
                            }
                        }
                    }
                    Target::Dynamic { flags, layers, colors, depth, stencil } => {
                        out.push(format!("{head}; vkCmdBeginRendering flags 0x{flags:x} layers {layers}"));
                        let mut att = |what: String, a: &DynAttachment| {
                            let (image, info) = reg.image_of_view(a.view);
                            let fmt = info.map_or(0, |i| i.format);
                            out.push(format!(
                                "  {what}: {} view {}, {}, load {} store {}{}{}",
                                image_text(image, info),
                                hex(a.view),
                                registry::layout_name(a.layout),
                                registry::load_name(a.load),
                                registry::store_name(a.store),
                                if a.load == 1 { format!(", clear {}", clear_text(fmt, a.clear)) } else { String::new() },
                                if a.resolve_view != 0 {
                                    let (ri, rinfo) = reg.image_of_view(a.resolve_view);
                                    format!(", resolve mode {} into {} ({})", a.resolve_mode, image_text(ri, rinfo), registry::layout_name(a.resolve_layout))
                                } else {
                                    String::new()
                                }
                            ));
                        };
                        for (i, c) in colors.iter().enumerate() {
                            att(format!("color {i}"), c);
                        }
                        if let Some(d) = depth {
                            att("depth".into(), d);
                        }
                        if let Some(s) = stencil {
                            att("stencil".into(), s);
                        }
                    }
                }
            }
            Event::Transfer { cmd, what, src, src_layout, dst, dst_layout } => {
                let to = if *dst == 0 {
                    "a buffer".to_string()
                } else {
                    format!("{} ({})", image_text(*dst, reg.images.get(dst)), registry::layout_name(*dst_layout))
                };
                out.push(format!(
                    "{what} in cmd {}: {} ({}) -> {to}",
                    hex(*cmd as u64),
                    image_text(*src, reg.images.get(src)),
                    registry::layout_name(*src_layout)
                ));
            }
            Event::Clear { cmd, what, image, layout } => out.push(format!(
                "{what} in cmd {}: {} ({})",
                hex(*cmd as u64),
                image_text(*image, reg.images.get(image)),
                registry::layout_name(*layout)
            )),
            Event::Barrier { cmd, image, old, new, src_stage, dst_stage } => {
                // Only images something renders into; texture uploads would
                // bury the frame.
                let Some(info) = reg.images.get(image) else { continue };
                if info.usage & (registry::USAGE_COLOR_ATTACHMENT | registry::USAGE_DEPTH_STENCIL_ATTACHMENT) == 0 && !info.swapchain {
                    continue;
                }
                out.push(format!(
                    "barrier in cmd {}: {} {} -> {}, stages 0x{:x} -> 0x{:x}",
                    hex(*cmd as u64),
                    image_text(*image, Some(info)),
                    registry::layout_name(*old),
                    registry::layout_name(*new),
                    src_stage,
                    dst_stage
                ));
            }
            Event::Dispatch { cmd, groups } => {
                out.push(format!("dispatch in cmd {}: {}x{}x{}", hex(*cmd as u64), groups[0], groups[1], groups[2]))
            }
            Event::Execute { cmd, secondaries } => out.push(format!(
                "execute in cmd {}: {} secondary cmd(s), {} draw(s) recorded in them this frame",
                hex(*cmd as u64),
                secondaries.len(),
                secondaries.iter().filter_map(|s| rec.outside.get(s)).map(|d| d.total()).sum::<u32>()
            )),
            Event::Submit { queue, cmds } => out.push(format!(
                "submit to queue {}: cmd(s) {}",
                hex(*queue as u64),
                cmds.iter().map(|c| hex(*c as u64)).collect::<Vec<_>>().join(", ")
            )),
        }
    }
    let outside: u32 = rec.outside.values().map(|d| d.total()).sum();
    out[0] = format!(
        "frame after present {}: {passes} pass(es), {draws} draw(s) in passes, {outside} recorded outside one, \
         {} command buffer(s) with passes, {submits} submit(s), {} event(s)",
        presents.saturating_sub(1),
        cmds.len(),
        rec.events.len()
    );
    // The images a frame can draw into, so one that no pass used this frame
    // is still visible.
    let mut targets: Vec<(&u64, &registry::Image)> = reg
        .images
        .iter()
        .filter(|(_, i)| i.usage & (registry::USAGE_COLOR_ATTACHMENT | registry::USAGE_DEPTH_STENCIL_ATTACHMENT) != 0 || i.swapchain)
        .collect();
    targets.sort_by_key(|(h, i)| (std::cmp::Reverse(i.extent[0] as u64 * i.extent[1] as u64), i.format, **h));
    out.push(format!("{} image(s) that can be drawn into:", targets.len()));
    for (h, i) in targets {
        out.push(format!("  {}", image_text(*h, Some(i))));
    }
    out
}

// ------------------------------------------------------------ the hooks

macro_rules! slots {
    ($($name:ident),* $(,)?) => { $(static $name: AtomicUsize = AtomicUsize::new(0);)* };
}

slots!(
    CREATE_RENDER_PASS,
    CREATE_RENDER_PASS2,
    DESTROY_RENDER_PASS,
    CREATE_FRAMEBUFFER,
    DESTROY_FRAMEBUFFER,
    BEGIN_RENDER_PASS,
    BEGIN_RENDER_PASS2,
    NEXT_SUBPASS,
    NEXT_SUBPASS2,
    END_RENDER_PASS,
    END_RENDER_PASS2,
    BEGIN_RENDERING,
    END_RENDERING,
    DRAW,
    DRAW_INDEXED,
    DRAW_INDIRECT,
    DRAW_INDEXED_INDIRECT,
    DRAW_INDIRECT_COUNT,
    DRAW_INDEXED_INDIRECT_COUNT,
    EXECUTE_COMMANDS,
    COPY_IMAGE,
    COPY_IMAGE2,
    BLIT_IMAGE,
    BLIT_IMAGE2,
    RESOLVE_IMAGE,
    RESOLVE_IMAGE2,
    COPY_IMAGE_TO_BUFFER,
    COPY_IMAGE_TO_BUFFER2,
    CLEAR_COLOR_IMAGE,
    CLEAR_DEPTH_STENCIL_IMAGE,
    DISPATCH,
    PIPELINE_BARRIER,
    PIPELINE_BARRIER2,
    QUEUE_SUBMIT,
    QUEUE_SUBMIT2,
    BEGIN_COMMAND_BUFFER,
    BIND_PIPELINE,
    BIND_DESCRIPTOR_SETS,
    PUSH_CONSTANTS,
    PUSH_DESCRIPTOR_SET,
    PUSH_DESCRIPTOR_SET_WITH_TEMPLATE,
    BIND_DESCRIPTOR_BUFFERS,
    BIND_SHADERS,
);

/// The function in `slot` as `F`, an `extern "C" fn` type matching the
/// specification's prototype for the name the slot was filled for.
fn next<F: Copy>(slot: &AtomicUsize) -> Option<F> {
    let p = slot.load(Relaxed);
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<usize>());
    // SAFETY: `F` is a function pointer type the same size as `usize`, and a
    // non-zero `p` came from the getter for the name `F` describes.
    (p != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&p) })
}

/// Our entry point for `name`, if it is one of the commands traced here.
fn ours(name: &[u8]) -> Option<(&'static AtomicUsize, *const ())> {
    Some(match name {
        b"vkCreateRenderPass" => (&CREATE_RENDER_PASS, create_render_pass as *const ()),
        b"vkCreateRenderPass2" | b"vkCreateRenderPass2KHR" => (&CREATE_RENDER_PASS2, create_render_pass2 as *const ()),
        b"vkDestroyRenderPass" => (&DESTROY_RENDER_PASS, destroy_render_pass as *const ()),
        b"vkCreateFramebuffer" => (&CREATE_FRAMEBUFFER, create_framebuffer as *const ()),
        b"vkDestroyFramebuffer" => (&DESTROY_FRAMEBUFFER, destroy_framebuffer as *const ()),
        b"vkCmdBeginRenderPass" => (&BEGIN_RENDER_PASS, cmd_begin_render_pass as *const ()),
        b"vkCmdBeginRenderPass2" | b"vkCmdBeginRenderPass2KHR" => (&BEGIN_RENDER_PASS2, cmd_begin_render_pass2 as *const ()),
        b"vkCmdNextSubpass" => (&NEXT_SUBPASS, cmd_next_subpass as *const ()),
        b"vkCmdNextSubpass2" | b"vkCmdNextSubpass2KHR" => (&NEXT_SUBPASS2, cmd_next_subpass2 as *const ()),
        b"vkCmdEndRenderPass" => (&END_RENDER_PASS, cmd_end_render_pass as *const ()),
        b"vkCmdEndRenderPass2" | b"vkCmdEndRenderPass2KHR" => (&END_RENDER_PASS2, cmd_end_render_pass2 as *const ()),
        b"vkCmdBeginRendering" | b"vkCmdBeginRenderingKHR" => (&BEGIN_RENDERING, cmd_begin_rendering as *const ()),
        b"vkCmdEndRendering" | b"vkCmdEndRenderingKHR" => (&END_RENDERING, cmd_end_rendering as *const ()),
        b"vkCmdDraw" => (&DRAW, cmd_draw as *const ()),
        b"vkCmdDrawIndexed" => (&DRAW_INDEXED, cmd_draw_indexed as *const ()),
        b"vkCmdDrawIndirect" => (&DRAW_INDIRECT, cmd_draw_indirect as *const ()),
        b"vkCmdDrawIndexedIndirect" => (&DRAW_INDEXED_INDIRECT, cmd_draw_indexed_indirect as *const ()),
        b"vkCmdDrawIndirectCount" | b"vkCmdDrawIndirectCountKHR" | b"vkCmdDrawIndirectCountAMD" => {
            (&DRAW_INDIRECT_COUNT, cmd_draw_indirect_count as *const ())
        }
        b"vkCmdDrawIndexedIndirectCount" | b"vkCmdDrawIndexedIndirectCountKHR" | b"vkCmdDrawIndexedIndirectCountAMD" => {
            (&DRAW_INDEXED_INDIRECT_COUNT, cmd_draw_indexed_indirect_count as *const ())
        }
        b"vkCmdExecuteCommands" => (&EXECUTE_COMMANDS, cmd_execute_commands as *const ()),
        b"vkCmdCopyImage" => (&COPY_IMAGE, cmd_copy_image as *const ()),
        b"vkCmdCopyImage2" | b"vkCmdCopyImage2KHR" => (&COPY_IMAGE2, cmd_copy_image2 as *const ()),
        b"vkCmdBlitImage" => (&BLIT_IMAGE, cmd_blit_image as *const ()),
        b"vkCmdBlitImage2" | b"vkCmdBlitImage2KHR" => (&BLIT_IMAGE2, cmd_blit_image2 as *const ()),
        b"vkCmdResolveImage" => (&RESOLVE_IMAGE, cmd_resolve_image as *const ()),
        b"vkCmdResolveImage2" | b"vkCmdResolveImage2KHR" => (&RESOLVE_IMAGE2, cmd_resolve_image2 as *const ()),
        b"vkCmdCopyImageToBuffer" => (&COPY_IMAGE_TO_BUFFER, cmd_copy_image_to_buffer as *const ()),
        b"vkCmdCopyImageToBuffer2" | b"vkCmdCopyImageToBuffer2KHR" => {
            (&COPY_IMAGE_TO_BUFFER2, cmd_copy_image_to_buffer2 as *const ())
        }
        b"vkCmdClearColorImage" => (&CLEAR_COLOR_IMAGE, cmd_clear_color_image as *const ()),
        b"vkCmdClearDepthStencilImage" => (&CLEAR_DEPTH_STENCIL_IMAGE, cmd_clear_depth_stencil_image as *const ()),
        b"vkCmdDispatch" => (&DISPATCH, cmd_dispatch as *const ()),
        b"vkCmdPipelineBarrier" => (&PIPELINE_BARRIER, cmd_pipeline_barrier as *const ()),
        b"vkCmdPipelineBarrier2" | b"vkCmdPipelineBarrier2KHR" => (&PIPELINE_BARRIER2, cmd_pipeline_barrier2 as *const ()),
        b"vkQueueSubmit" => (&QUEUE_SUBMIT, queue_submit as *const ()),
        b"vkQueueSubmit2" | b"vkQueueSubmit2KHR" => (&QUEUE_SUBMIT2, queue_submit2 as *const ()),
        b"vkBeginCommandBuffer" => (&BEGIN_COMMAND_BUFFER, begin_command_buffer as *const ()),
        b"vkCmdBindPipeline" => (&BIND_PIPELINE, cmd_bind_pipeline as *const ()),
        b"vkCmdBindDescriptorSets" => (&BIND_DESCRIPTOR_SETS, cmd_bind_descriptor_sets as *const ()),
        b"vkCmdPushConstants" => (&PUSH_CONSTANTS, cmd_push_constants as *const ()),
        b"vkCmdPushDescriptorSetKHR" | b"vkCmdPushDescriptorSet" => (&PUSH_DESCRIPTOR_SET, cmd_push_descriptor_set as *const ()),
        b"vkCmdPushDescriptorSetWithTemplateKHR" | b"vkCmdPushDescriptorSetWithTemplate" => {
            (&PUSH_DESCRIPTOR_SET_WITH_TEMPLATE, cmd_push_descriptor_set_with_template as *const ())
        }
        b"vkCmdBindDescriptorBuffersEXT" => (&BIND_DESCRIPTOR_BUFFERS, cmd_bind_descriptor_buffers as *const ()),
        b"vkCmdBindShadersEXT" => (&BIND_SHADERS, cmd_bind_shaders as *const ()),
        _ => return None,
    })
}

/// From either getter's fall-through: `next` is what would otherwise have
/// been handed out (the driver's entry, or `vulkan_etc`'s hook in front of
/// it). Ours in its place for a traced command when the hooks are wanted;
/// `next` itself otherwise. A command the driver lacks stays missing.
pub(crate) fn hook(name: &[u8], next: *mut c_void) -> *mut c_void {
    if next.is_null() || !wanted() {
        return next;
    }
    match ours(name) {
        Some((slot, f)) => {
            slot.store(next as usize, Relaxed);
            f as *mut c_void
        }
        None => next,
    }
}

type CreateFn = extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32;
type DestroyFn = extern "C" fn(*mut c_void, u64, *const c_void);

extern "C" fn create_render_pass(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    created_render_pass(&CREATE_RENDER_PASS, false, device, info, alloc, out)
}

extern "C" fn create_render_pass2(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    created_render_pass(&CREATE_RENDER_PASS2, true, device, info, alloc, out)
}

fn created_render_pass(slot: &AtomicUsize, v2: bool, device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) = next::<CreateFn>(slot) else { return VK_ERROR_INITIALIZATION_FAILED };
    let rc = f(device, info, alloc, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() {
        // SAFETY: the engine's create info, read during its own call; success
        // means the driver wrote the handle.
        let (rp, handle) = unsafe { (registry::render_pass_from(device as usize, info, v2), *out) };
        registry().render_passes.insert(handle, rp);
    }
    rc
}

extern "C" fn destroy_render_pass(device: *mut c_void, rp: u64, alloc: *const c_void) {
    registry().render_passes.remove(&rp);
    if let Some(f) = next::<DestroyFn>(&DESTROY_RENDER_PASS) {
        f(device, rp, alloc);
    }
}

extern "C" fn create_framebuffer(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) = next::<CreateFn>(&CREATE_FRAMEBUFFER) else { return VK_ERROR_INITIALIZATION_FAILED };
    let rc = f(device, info, alloc, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() {
        // SAFETY: as for render passes.
        let (fb, handle) = unsafe { (registry::framebuffer_from(device as usize, info), *out) };
        registry().framebuffers.insert(handle, fb);
    }
    rc
}

extern "C" fn destroy_framebuffer(device: *mut c_void, fb: u64, alloc: *const c_void) {
    registry().framebuffers.remove(&fb);
    if let Some(f) = next::<DestroyFn>(&DESTROY_FRAMEBUFFER) {
        f(device, fb, alloc);
    }
}

/// A render pass instance from its `VkRenderPassBeginInfo`.
///
/// # Safety
/// `begin` must be a valid `VkRenderPassBeginInfo`.
unsafe fn pass_from_begin(cmd: usize, begin: *const c_void, contents: u32, version: u8) -> Pass {
    let (n_clear, p_clear): (u32, *const c_void) = (get(begin, 48), get(begin, 56));
    let clears = if p_clear.is_null() { Vec::new() } else { (0..n_clear as usize).map(|i| get(p_clear, i * 16)).collect() };
    Pass::new(
        cmd,
        Target::RenderPass {
            render_pass: get(begin, 16),
            framebuffer: get(begin, 24),
            views: registry::begin_info_views(begin),
            contents,
            version,
        },
        (get(begin, 32), get(begin, 36), get(begin, 40), get(begin, 44)),
        clears,
    )
}

/// # Safety
/// `a` must be null or a valid `VkRenderingAttachmentInfo`.
unsafe fn dyn_attachment(a: *const c_void) -> Option<DynAttachment> {
    if a.is_null() {
        return None;
    }
    Some(DynAttachment {
        view: get(a, 16),
        layout: get(a, 24),
        resolve_mode: get(a, 28),
        resolve_view: get(a, 32),
        resolve_layout: get(a, 40),
        load: get(a, 44),
        store: get(a, 48),
        clear: get(a, 52),
    })
}

/// # Safety
/// `info` must be a valid `VkRenderingInfo`.
unsafe fn pass_from_rendering(cmd: usize, info: *const c_void) -> Pass {
    let (n, list): (u32, *const c_void) = (get(info, 44), get(info, 48));
    let colors: Vec<DynAttachment> = if list.is_null() {
        Vec::new()
    } else {
        (0..n as usize).filter_map(|i| dyn_attachment((list as *const u8).add(i * 72) as *const c_void)).collect()
    };
    let depth = dyn_attachment(get(info, 56));
    let stencil = dyn_attachment(get(info, 64));
    let mut clears: Vec<[u32; 4]> = colors.iter().map(|c| c.clear).collect();
    if let Some(d) = depth {
        clears.push(d.clear);
    }
    Pass::new(
        cmd,
        Target::Dynamic { flags: get(info, 16), layers: get(info, 36), colors, depth, stencil },
        (get(info, 20), get(info, 24), get(info, 28), get(info, 32)),
        clears,
    )
}

/// Before the begin of a pass is forwarded. The reflections, when this is
/// the pass they go in front of, record their work here.
fn pass_begun(watch: u32, pass: Pass) {
    if watch & WATCH_RT != 0 {
        crate::android::rt::capture::pass_beginning(&pass);
    }
    if watch & WATCH_POST != 0 {
        crate::android::ssr::pass_begun(&pass);
    }
    if watch & WATCH_DEPTH != 0 {
        crate::android::depth_capture::pass_begun(&pass);
    }
    if watch & WATCH_TRACE != 0 {
        record(|r| r.begin_pass(pass));
    }
}

/// After the begin of a pass has been forwarded.
fn pass_started(watch: u32, cmd: *mut c_void) {
    if watch & WATCH_RT != 0 {
        crate::android::rt::capture::pass_started(cmd);
    }
}

/// Before the end of a pass is forwarded.
fn pass_ending(watch: u32, cmd: *mut c_void) {
    if watch & WATCH_RT != 0 {
        crate::android::rt::capture::pass_ending(cmd);
    }
}

/// After the end of a pass has been forwarded.
fn pass_ended(watch: u32, cmd: *mut c_void) {
    if watch & WATCH_RT != 0 {
        crate::android::rt::capture::pass_ended(cmd);
    }
    if watch & WATCH_TRACE != 0 {
        record(|r| {
            r.end_pass(cmd as usize);
        });
    }
}

extern "C" fn cmd_begin_render_pass(cmd: *mut c_void, begin: *const c_void, contents: u32) {
    let w = WATCH.load(Relaxed);
    if w != 0 && !begin.is_null() {
        // SAFETY: the engine's begin info, read during its own call.
        pass_begun(w, unsafe { pass_from_begin(cmd as usize, begin, contents, 1) });
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, *const c_void, u32)>(&BEGIN_RENDER_PASS) {
        f(cmd, begin, contents);
    }
    if w != 0 && !begin.is_null() {
        pass_started(w, cmd);
    }
}

extern "C" fn cmd_begin_render_pass2(cmd: *mut c_void, begin: *const c_void, subpass: *const c_void) {
    let w = WATCH.load(Relaxed);
    if w != 0 && !begin.is_null() {
        // SAFETY: the engine's structures, read during its own call;
        // `VkSubpassBeginInfo.contents` is 16 bytes in.
        let contents = if subpass.is_null() { 0 } else { unsafe { get::<u32>(subpass, 16) } };
        pass_begun(w, unsafe { pass_from_begin(cmd as usize, begin, contents, 2) });
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, *const c_void, *const c_void)>(&BEGIN_RENDER_PASS2) {
        f(cmd, begin, subpass);
    }
    if w != 0 && !begin.is_null() {
        pass_started(w, cmd);
    }
}

extern "C" fn cmd_next_subpass(cmd: *mut c_void, contents: u32) {
    if WATCH.load(Relaxed) & WATCH_RT != 0 {
        crate::android::rt::capture::interrupted(cmd);
    }
    if WATCH.load(Relaxed) & WATCH_TRACE != 0 {
        record(|r| r.next_subpass(cmd as usize));
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32)>(&NEXT_SUBPASS) {
        f(cmd, contents);
    }
}

extern "C" fn cmd_next_subpass2(cmd: *mut c_void, begin: *const c_void, end: *const c_void) {
    if WATCH.load(Relaxed) & WATCH_RT != 0 {
        crate::android::rt::capture::interrupted(cmd);
    }
    if WATCH.load(Relaxed) & WATCH_TRACE != 0 {
        record(|r| r.next_subpass(cmd as usize));
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, *const c_void, *const c_void)>(&NEXT_SUBPASS2) {
        f(cmd, begin, end);
    }
}

extern "C" fn cmd_end_render_pass(cmd: *mut c_void) {
    pass_ending(WATCH.load(Relaxed), cmd);
    if let Some(f) = next::<extern "C" fn(*mut c_void)>(&END_RENDER_PASS) {
        f(cmd);
    }
    let w = WATCH.load(Relaxed);
    if w != 0 {
        pass_ended(w, cmd);
    }
}

extern "C" fn cmd_end_render_pass2(cmd: *mut c_void, end: *const c_void) {
    pass_ending(WATCH.load(Relaxed), cmd);
    if let Some(f) = next::<extern "C" fn(*mut c_void, *const c_void)>(&END_RENDER_PASS2) {
        f(cmd, end);
    }
    let w = WATCH.load(Relaxed);
    if w != 0 {
        pass_ended(w, cmd);
    }
}

extern "C" fn cmd_begin_rendering(cmd: *mut c_void, info: *const c_void) {
    let w = WATCH.load(Relaxed);
    if w != 0 && !info.is_null() {
        // SAFETY: the engine's `VkRenderingInfo`, read during its own call.
        pass_begun(w, unsafe { pass_from_rendering(cmd as usize, info) });
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, *const c_void)>(&BEGIN_RENDERING) {
        f(cmd, info);
    }
    if w != 0 && !info.is_null() {
        pass_started(w, cmd);
    }
}

extern "C" fn cmd_end_rendering(cmd: *mut c_void) {
    pass_ending(WATCH.load(Relaxed), cmd);
    if let Some(f) = next::<extern "C" fn(*mut c_void)>(&END_RENDERING) {
        f(cmd);
    }
    let w = WATCH.load(Relaxed);
    if w != 0 {
        pass_ended(w, cmd);
    }
}

fn drew(cmd: *mut c_void, kind: DrawKind) {
    let w = WATCH.load(Relaxed);
    if w & WATCH_TRACE != 0 {
        record(|r| r.draw(cmd as usize, kind));
    }
    if w & WATCH_RT != 0 && kind == DrawKind::Indirect {
        crate::android::rt::capture::drew(cmd, "indirect", 0, 0);
    }
}

extern "C" fn cmd_draw(cmd: *mut c_void, vertices: u32, instances: u32, first_vertex: u32, first_instance: u32) {
    drew(cmd, DrawKind::Direct);
    if WATCH.load(Relaxed) & WATCH_RT != 0 {
        crate::android::rt::capture::drew(cmd, "draw", vertices, instances);
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u32, u32, u32)>(&DRAW) {
        f(cmd, vertices, instances, first_vertex, first_instance);
    }
}

extern "C" fn cmd_draw_indexed(cmd: *mut c_void, indices: u32, instances: u32, first_index: u32, vertex_offset: i32, first_instance: u32) {
    drew(cmd, DrawKind::Indexed);
    if WATCH.load(Relaxed) & WATCH_RT != 0 {
        crate::android::rt::capture::drew(cmd, "indexed", indices, instances);
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u32, u32, i32, u32)>(&DRAW_INDEXED) {
        f(cmd, indices, instances, first_index, vertex_offset, first_instance);
    }
}

type IndirectFn = extern "C" fn(*mut c_void, u64, u64, u32, u32);
type IndirectCountFn = extern "C" fn(*mut c_void, u64, u64, u64, u64, u32, u32);

extern "C" fn cmd_draw_indirect(cmd: *mut c_void, buffer: u64, offset: u64, count: u32, stride: u32) {
    drew(cmd, DrawKind::Indirect);
    if let Some(f) = next::<IndirectFn>(&DRAW_INDIRECT) {
        f(cmd, buffer, offset, count, stride);
    }
}

extern "C" fn cmd_draw_indexed_indirect(cmd: *mut c_void, buffer: u64, offset: u64, count: u32, stride: u32) {
    drew(cmd, DrawKind::Indirect);
    if let Some(f) = next::<IndirectFn>(&DRAW_INDEXED_INDIRECT) {
        f(cmd, buffer, offset, count, stride);
    }
}

extern "C" fn cmd_draw_indirect_count(cmd: *mut c_void, buffer: u64, offset: u64, count_buffer: u64, count_offset: u64, max: u32, stride: u32) {
    drew(cmd, DrawKind::Indirect);
    if let Some(f) = next::<IndirectCountFn>(&DRAW_INDIRECT_COUNT) {
        f(cmd, buffer, offset, count_buffer, count_offset, max, stride);
    }
}

extern "C" fn cmd_draw_indexed_indirect_count(cmd: *mut c_void, buffer: u64, offset: u64, count_buffer: u64, count_offset: u64, max: u32, stride: u32) {
    drew(cmd, DrawKind::Indirect);
    if let Some(f) = next::<IndirectCountFn>(&DRAW_INDEXED_INDIRECT_COUNT) {
        f(cmd, buffer, offset, count_buffer, count_offset, max, stride);
    }
}

extern "C" fn cmd_execute_commands(cmd: *mut c_void, count: u32, list: *const usize) {
    if WATCH.load(Relaxed) & WATCH_RT != 0 {
        crate::android::rt::capture::interrupted(cmd);
    }
    if WATCH.load(Relaxed) & WATCH_TRACE != 0 && !list.is_null() {
        // SAFETY: the engine's array of `count` command buffers.
        let secondaries = unsafe { std::slice::from_raw_parts(list, count as usize) }.to_vec();
        record(|r| r.execute(cmd as usize, secondaries));
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, *const usize)>(&EXECUTE_COMMANDS) {
        f(cmd, count, list);
    }
}

fn transfer(cmd: *mut c_void, what: &'static str, src: u64, src_layout: u32, dst: u64, dst_layout: u32) {
    let w = WATCH.load(Relaxed);
    if w & WATCH_TRACE != 0 {
        record(|r| r.events.push(Event::Transfer { cmd: cmd as usize, what, src, src_layout, dst, dst_layout }));
    }
    if w & WATCH_DEPTH != 0 && dst != 0 {
        crate::android::depth_capture::written(dst);
    }
    if w & WATCH_POST != 0 && dst != 0 {
        if what.starts_with("vkCmdResolveImage") {
            crate::android::ssr::resolved(cmd, src, dst, dst_layout);
        } else {
            crate::android::ssr::written(cmd, dst);
        }
    }
}

/// `VkCopyImageInfo2`, `VkBlitImageInfo2` and `VkResolveImageInfo2` share
/// their head: source image 16, its layout 24, destination 32, its layout 40.
fn transfer2(cmd: *mut c_void, what: &'static str, info: *const c_void, to_buffer: bool) {
    if WATCH.load(Relaxed) != 0 && !info.is_null() {
        // SAFETY: the engine's structure, read during its own call. For
        // `VkCopyImageToBufferInfo2` the destination is a buffer at 32.
        let (src, src_layout, dst, dst_layout): (u64, u32, u64, u32) = unsafe {
            if to_buffer {
                (get(info, 16), get(info, 24), 0, 0)
            } else {
                (get(info, 16), get(info, 24), get(info, 32), get(info, 40))
            }
        };
        transfer(cmd, what, src, src_layout, dst, dst_layout);
    }
}

type CopyImageFn = extern "C" fn(*mut c_void, u64, u32, u64, u32, u32, *const c_void);

extern "C" fn cmd_copy_image(cmd: *mut c_void, src: u64, src_layout: u32, dst: u64, dst_layout: u32, n: u32, regions: *const c_void) {
    transfer(cmd, "vkCmdCopyImage", src, src_layout, dst, dst_layout);
    if let Some(f) = next::<CopyImageFn>(&COPY_IMAGE) {
        f(cmd, src, src_layout, dst, dst_layout, n, regions);
    }
}

extern "C" fn cmd_resolve_image(cmd: *mut c_void, src: u64, src_layout: u32, dst: u64, dst_layout: u32, n: u32, regions: *const c_void) {
    transfer(cmd, "vkCmdResolveImage", src, src_layout, dst, dst_layout);
    if let Some(f) = next::<CopyImageFn>(&RESOLVE_IMAGE) {
        f(cmd, src, src_layout, dst, dst_layout, n, regions);
    }
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_blit_image(cmd: *mut c_void, src: u64, src_layout: u32, dst: u64, dst_layout: u32, n: u32, regions: *const c_void, filter: u32) {
    transfer(cmd, "vkCmdBlitImage", src, src_layout, dst, dst_layout);
    if let Some(f) = next::<extern "C" fn(*mut c_void, u64, u32, u64, u32, u32, *const c_void, u32)>(&BLIT_IMAGE) {
        f(cmd, src, src_layout, dst, dst_layout, n, regions, filter);
    }
}

extern "C" fn cmd_copy_image_to_buffer(cmd: *mut c_void, src: u64, src_layout: u32, dst: u64, n: u32, regions: *const c_void) {
    transfer(cmd, "vkCmdCopyImageToBuffer", src, src_layout, 0, 0);
    if let Some(f) = next::<extern "C" fn(*mut c_void, u64, u32, u64, u32, *const c_void)>(&COPY_IMAGE_TO_BUFFER) {
        f(cmd, src, src_layout, dst, n, regions);
    }
}

type Info2Fn = extern "C" fn(*mut c_void, *const c_void);

extern "C" fn cmd_copy_image2(cmd: *mut c_void, info: *const c_void) {
    transfer2(cmd, "vkCmdCopyImage2", info, false);
    if let Some(f) = next::<Info2Fn>(&COPY_IMAGE2) {
        f(cmd, info);
    }
}

extern "C" fn cmd_blit_image2(cmd: *mut c_void, info: *const c_void) {
    transfer2(cmd, "vkCmdBlitImage2", info, false);
    if let Some(f) = next::<Info2Fn>(&BLIT_IMAGE2) {
        f(cmd, info);
    }
}

extern "C" fn cmd_resolve_image2(cmd: *mut c_void, info: *const c_void) {
    transfer2(cmd, "vkCmdResolveImage2", info, false);
    if let Some(f) = next::<Info2Fn>(&RESOLVE_IMAGE2) {
        f(cmd, info);
    }
}

extern "C" fn cmd_copy_image_to_buffer2(cmd: *mut c_void, info: *const c_void) {
    transfer2(cmd, "vkCmdCopyImageToBuffer2", info, true);
    if let Some(f) = next::<Info2Fn>(&COPY_IMAGE_TO_BUFFER2) {
        f(cmd, info);
    }
}

type ClearFn = extern "C" fn(*mut c_void, u64, u32, *const c_void, u32, *const c_void);

fn cleared(cmd: *mut c_void, what: &'static str, image: u64, layout: u32) {
    let w = WATCH.load(Relaxed);
    if w & WATCH_TRACE != 0 {
        record(|r| r.events.push(Event::Clear { cmd: cmd as usize, what, image, layout }));
    }
    if w & WATCH_DEPTH != 0 {
        crate::android::depth_capture::written(image);
    }
    if w & WATCH_POST != 0 {
        crate::android::ssr::written(cmd, image);
    }
}

extern "C" fn cmd_clear_color_image(cmd: *mut c_void, image: u64, layout: u32, value: *const c_void, n: u32, ranges: *const c_void) {
    cleared(cmd, "vkCmdClearColorImage", image, layout);
    if let Some(f) = next::<ClearFn>(&CLEAR_COLOR_IMAGE) {
        f(cmd, image, layout, value, n, ranges);
    }
}

extern "C" fn cmd_clear_depth_stencil_image(cmd: *mut c_void, image: u64, layout: u32, value: *const c_void, n: u32, ranges: *const c_void) {
    cleared(cmd, "vkCmdClearDepthStencilImage", image, layout);
    if let Some(f) = next::<ClearFn>(&CLEAR_DEPTH_STENCIL_IMAGE) {
        f(cmd, image, layout, value, n, ranges);
    }
}

extern "C" fn cmd_dispatch(cmd: *mut c_void, x: u32, y: u32, z: u32) {
    if WATCH.load(Relaxed) & WATCH_TRACE != 0 {
        record(|r| r.events.push(Event::Dispatch { cmd: cmd as usize, groups: [x, y, z] }));
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u32, u32)>(&DISPATCH) {
        f(cmd, x, y, z);
    }
}

/// Image barriers, to the trace and to the depth capture and the
/// reflections, which follow the layouts of the scene images through them.
fn barriers(watch: u32, list: Vec<Event>) {
    if watch & WATCH_DEPTH != 0 {
        for e in &list {
            if let Event::Barrier { image, new, .. } = e {
                crate::android::depth_capture::barrier(*image, *new);
            }
        }
    }
    if watch & WATCH_POST != 0 {
        if let Some(Event::Barrier { cmd, .. }) = list.first() {
            let layouts = list.iter().filter_map(|e| match e {
                Event::Barrier { image, new, .. } => Some((*image, *new)),
                _ => None,
            });
            crate::android::ssr::barriers(*cmd as *mut c_void, layouts);
        }
    }
    if watch & WATCH_TRACE != 0 {
        record(|r| r.events.extend(list));
    }
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_pipeline_barrier(
    cmd: *mut c_void,
    src_stage: u32,
    dst_stage: u32,
    flags: u32,
    n_memory: u32,
    memory: *const c_void,
    n_buffer: u32,
    buffers: *const c_void,
    n_image: u32,
    images: *const c_void,
) {
    let w = WATCH.load(Relaxed);
    if w != 0 && !images.is_null() {
        // SAFETY: the engine's array of `n_image` `VkImageMemoryBarrier`s,
        // 72 bytes each: old layout 24, new 28, image 40.
        let list: Vec<Event> = (0..n_image as usize)
            .map(|i| unsafe {
                let b = (images as *const u8).add(i * 72) as *const c_void;
                Event::Barrier {
                    cmd: cmd as usize,
                    image: get(b, 40),
                    old: get(b, 24),
                    new: get(b, 28),
                    src_stage: src_stage as u64,
                    dst_stage: dst_stage as u64,
                }
            })
            .collect();
        barriers(w, list);
    }
    type Fn_ = extern "C" fn(*mut c_void, u32, u32, u32, u32, *const c_void, u32, *const c_void, u32, *const c_void);
    if let Some(f) = next::<Fn_>(&PIPELINE_BARRIER) {
        f(cmd, src_stage, dst_stage, flags, n_memory, memory, n_buffer, buffers, n_image, images);
    }
}

extern "C" fn cmd_pipeline_barrier2(cmd: *mut c_void, info: *const c_void) {
    let w = WATCH.load(Relaxed);
    if w != 0 && !info.is_null() {
        // SAFETY: the engine's `VkDependencyInfo` (image barrier count 48,
        // array 56) and its `VkImageMemoryBarrier2`s, 96 bytes each: stages
        // 16 and 32, old layout 48, new 52, image 64.
        let list: Vec<Event> = unsafe {
            let (n, images): (u32, *const c_void) = (get(info, 48), get(info, 56));
            if images.is_null() {
                Vec::new()
            } else {
                (0..n as usize)
                    .map(|i| {
                        let b = (images as *const u8).add(i * 96) as *const c_void;
                        Event::Barrier {
                            cmd: cmd as usize,
                            image: get(b, 64),
                            old: get(b, 48),
                            new: get(b, 52),
                            src_stage: get(b, 16),
                            dst_stage: get(b, 32),
                        }
                    })
                    .collect()
            }
        };
        barriers(w, list);
    }
    if let Some(f) = next::<Info2Fn>(&PIPELINE_BARRIER2) {
        f(cmd, info);
    }
}

/// The command buffers named by `count` submit infos, each `stride` bytes
/// with its count and array at `at_count` and `at_array`, each element
/// `elem` bytes with the handle `handle_at` bytes in.
///
/// # Safety
/// `submits` must be the caller's array of `count` such structures.
unsafe fn submitted(submits: *const c_void, count: u32, layout: (usize, usize, usize, usize, usize)) -> Vec<usize> {
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

fn submit(slot: &AtomicUsize, layout: (usize, usize, usize, usize, usize), queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    let Some(f) = next::<SubmitFn>(slot) else { return VK_ERROR_INITIALIZATION_FAILED };
    let w = WATCH.load(Relaxed);
    if w == 0 || submits.is_null() {
        return f(queue, count, submits, fence);
    }
    // SAFETY: the engine's array of `count` submit infos of this layout.
    let cmds = unsafe { submitted(submits, count, layout) };
    if w & WATCH_TRACE != 0 {
        let c = cmds.clone();
        record(|r| r.events.push(Event::Submit { queue: queue as usize, cmds: c }));
    }
    let rc = f(queue, count, submits, fence);
    if w & WATCH_DEPTH != 0 && rc == VK_SUCCESS {
        // Inside the engine's own submit on this queue, which is what lets
        // the depth capture use the queue here.
        crate::android::depth_capture::submitted(queue, &cmds);
    }
    rc
}

extern "C" fn queue_submit(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    // `VkSubmitInfo`: 72 bytes, commandBufferCount at 40, the array at 48.
    submit(&QUEUE_SUBMIT, (72, 40, 48, 8, 0), queue, count, submits, fence)
}

extern "C" fn queue_submit2(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    // `VkSubmitInfo2`: 64 bytes, commandBufferInfoCount at 32, the array at
    // 40; each `VkCommandBufferSubmitInfo` is 32 bytes, handle at 16.
    submit(&QUEUE_SUBMIT2, (64, 32, 40, 32, 16), queue, count, submits, fence)
}

// ------------------------------------------------------------ for the reflections, depth of field and bloom
//
// The start of a recording, and the engine's compute state, which our
// dispatch disturbs and puts back (see `android::ssr`). Graphics binds are
// forwarded after one comparison.

fn ssr_watching() -> bool {
    WATCH.load(Relaxed) & WATCH_POST != 0
}

extern "C" fn begin_command_buffer(cmd: *mut c_void, info: *const c_void) -> i32 {
    let Some(f) = next::<extern "C" fn(*mut c_void, *const c_void) -> i32>(&BEGIN_COMMAND_BUFFER) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(cmd, info);
    if rc == VK_SUCCESS && ssr_watching() {
        crate::android::ssr::cmd_begun(cmd);
    }
    if rc == VK_SUCCESS && WATCH.load(Relaxed) & WATCH_RT != 0 {
        crate::android::rt::capture::cmd_begun(cmd);
    }
    rc
}

extern "C" fn cmd_bind_pipeline(cmd: *mut c_void, bind_point: u32, pipeline: u64) {
    if bind_point == crate::android::ssr::BIND_POINT_COMPUTE && ssr_watching() {
        crate::android::ssr::compute(cmd, |c| c.bind_pipeline(pipeline));
    }
    let rt = WATCH.load(Relaxed) & WATCH_RT != 0;
    if rt {
        crate::android::rt::capture::binding(cmd, bind_point);
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u64)>(&BIND_PIPELINE) {
        f(cmd, bind_point, pipeline);
    }
    if rt {
        crate::android::rt::capture::bound(cmd, bind_point, pipeline);
    }
}

#[allow(clippy::too_many_arguments)]
extern "C" fn cmd_bind_descriptor_sets(
    cmd: *mut c_void,
    bind_point: u32,
    layout: u64,
    first: u32,
    count: u32,
    sets: *const u64,
    dynamic_count: u32,
    dynamic: *const u32,
) {
    if bind_point == crate::android::ssr::BIND_POINT_COMPUTE && ssr_watching() {
        // SAFETY: the engine's arrays of `count` sets and `dynamic_count`
        // offsets, read during its own call.
        let (sets_v, dynamic_v) = unsafe {
            (
                if sets.is_null() { Vec::new() } else { std::slice::from_raw_parts(sets, count as usize).to_vec() },
                if dynamic.is_null() { Vec::new() } else { std::slice::from_raw_parts(dynamic, dynamic_count as usize).to_vec() },
            )
        };
        crate::android::ssr::compute(cmd, |c| {
            c.bind_sets(crate::android::ssr::SetBind { layout, first, sets: sets_v, dynamic: dynamic_v })
        });
    }
    type Fn_ = extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u64, u32, *const u32);
    if let Some(f) = next::<Fn_>(&BIND_DESCRIPTOR_SETS) {
        f(cmd, bind_point, layout, first, count, sets, dynamic_count, dynamic);
    }
}

extern "C" fn cmd_push_constants(cmd: *mut c_void, layout: u64, stages: u32, offset: u32, size: u32, values: *const c_void) {
    if stages & crate::android::ssr::SHADER_STAGE_COMPUTE != 0 && !values.is_null() && ssr_watching() {
        // SAFETY: the engine's `size` bytes, read during its own call.
        let bytes = unsafe { std::slice::from_raw_parts(values as *const u8, size as usize) }.to_vec();
        crate::android::ssr::compute(cmd, |c| c.push(crate::android::ssr::Push { layout, stages, offset, bytes }));
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u64, u32, u32, u32, *const c_void)>(&PUSH_CONSTANTS) {
        f(cmd, layout, stages, offset, size, values);
    }
}

/// Compute state the reflections cannot put back: the command buffer is
/// left alone from then on.
fn unrestorable(cmd: *mut c_void, why: &'static str) {
    if ssr_watching() {
        crate::android::ssr::compute(cmd, |c| c.unrestorable = Some(why));
    }
}

extern "C" fn cmd_push_descriptor_set(cmd: *mut c_void, bind_point: u32, layout: u64, set: u32, count: u32, writes: *const c_void) {
    if bind_point == crate::android::ssr::BIND_POINT_COMPUTE {
        unrestorable(cmd, "the engine pushed descriptors for compute");
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u64, u32, u32, *const c_void)>(&PUSH_DESCRIPTOR_SET) {
        f(cmd, bind_point, layout, set, count, writes);
    }
}

extern "C" fn cmd_push_descriptor_set_with_template(cmd: *mut c_void, template: u64, layout: u64, set: u32, data: *const c_void) {
    // The bind point is inside the template, which is not followed.
    unrestorable(cmd, "the engine pushed descriptors with a template");
    if let Some(f) = next::<extern "C" fn(*mut c_void, u64, u64, u32, *const c_void)>(&PUSH_DESCRIPTOR_SET_WITH_TEMPLATE) {
        f(cmd, template, layout, set, data);
    }
}

extern "C" fn cmd_bind_descriptor_buffers(cmd: *mut c_void, count: u32, infos: *const c_void) {
    unrestorable(cmd, "the engine uses descriptor buffers");
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, *const c_void)>(&BIND_DESCRIPTOR_BUFFERS) {
        f(cmd, count, infos);
    }
}

extern "C" fn cmd_bind_shaders(cmd: *mut c_void, count: u32, stages: *const u32, shaders: *const u64) {
    if !stages.is_null() && ssr_watching() {
        // SAFETY: the engine's array of `count` stages.
        let list = unsafe { std::slice::from_raw_parts(stages, count as usize) };
        if list.iter().any(|&s| s & crate::android::ssr::SHADER_STAGE_COMPUTE != 0) {
            unrestorable(cmd, "the engine binds compute shader objects");
        }
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, *const u32, *const u64)>(&BIND_SHADERS) {
        f(cmd, count, stages, shaders);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_names_the_present_to_trace_after() {
        assert_eq!(trace_start(None), None);
        assert_eq!(trace_start(Some("")), None);
        assert_eq!(trace_start(Some("0")), None);
        assert_eq!(trace_start(Some("1")), Some(600));
        assert_eq!(trace_start(Some(" 20000 ")), Some(20000));
        assert_eq!(trace_start(Some("yes")), Some(600));
    }

    fn rp_pass(cmd: usize, rp: u64) -> Pass {
        Pass::new(cmd, Target::RenderPass { render_pass: rp, framebuffer: 0, views: vec![], contents: 0, version: 1 }, (0, 0, 8, 8), vec![])
    }

    #[test]
    fn draws_are_counted_per_pass_and_subpass_and_per_command_buffer() {
        let mut r = Recorder::default();
        r.draw(1, DrawKind::Indexed); // before any pass: outside
        r.begin_pass(rp_pass(1, 0xa));
        r.draw(1, DrawKind::Indexed);
        r.draw(1, DrawKind::Direct);
        r.begin_pass(rp_pass(2, 0xb)); // another command buffer, interleaved
        r.draw(2, DrawKind::Indirect);
        r.next_subpass(1);
        r.draw(1, DrawKind::Indexed);
        assert!(r.end_pass(1).is_some_and(|p| p.ended));
        assert!(r.end_pass(1).is_none(), "nothing open any more");
        r.draw(1, DrawKind::Direct); // after the pass: outside again
        let Event::Pass(p1) = &r.events[0] else { panic!() };
        assert_eq!(p1.subpasses, vec![Draws { direct: 1, indexed: 1, indirect: 0 }, Draws { direct: 0, indexed: 1, indirect: 0 }]);
        assert_eq!(r.pass_draws(p1).total(), 3);
        let Event::Pass(p2) = &r.events[1] else { panic!() };
        assert_eq!((p2.subpasses[0].indirect, p2.ended), (1, false));
        assert_eq!(r.outside[&1], Draws { direct: 1, indexed: 1, indirect: 0 });
    }

    #[test]
    fn a_pass_counts_the_draws_of_the_secondaries_it_executes() {
        let mut r = Recorder::default();
        // Secondary 9 is recorded first, continuing a render pass: its draws
        // are outside any pass of its own.
        for _ in 0..4 {
            r.draw(9, DrawKind::Indexed);
        }
        r.begin_pass(rp_pass(1, 0xa));
        r.execute(1, vec![9, 10]);
        r.end_pass(1);
        let Event::Pass(p) = &r.events[0] else { panic!() };
        assert_eq!(p.executed, vec![9, 10]);
        assert_eq!(r.pass_draws(p).indexed, 4);
        assert!(matches!(&r.events[1], Event::Execute { cmd: 1, secondaries } if secondaries == &[9, 10]));
    }

    #[test]
    fn the_report_names_attachments_subpasses_and_submits() {
        use registry::*;
        let mut reg = Registry::default();
        let depth = Attachment { format: 126, samples: 4, load: 1, store: 1, stencil_load: 2, stencil_store: 2, initial_layout: 0, final_layout: 3 };
        let color = Attachment { format: 44, samples: 1, load: 2, store: 0, initial_layout: 0, final_layout: 1_000_001_002, ..depth };
        reg.render_passes.insert(
            0x50,
            RenderPass {
                device: 1,
                version: 1,
                attachments: vec![color, depth],
                subpasses: vec![Subpass { color: vec![Ref { attachment: 0, layout: 2 }], depth: Some(Ref { attachment: 1, layout: 3 }), ..Subpass::default() }],
                dependencies: vec![],
            },
        );
        reg.framebuffers.insert(0x60, Framebuffer { device: 1, render_pass: 0x50, width: 8, height: 8, layers: 1, views: vec![0x70, 0x71], imageless: false });
        let img = |format, samples, swapchain| Image { device: 1, flags: 0, format, extent: [8, 8, 1], mips: 1, layers: 1, samples, usage: 0x20, swapchain };
        reg.images.insert(0x80, img(44, 1, true));
        reg.images.insert(0x81, img(126, 4, false));
        let view = |image| View { device: 1, image, view_type: 1, format: 0, aspect: 0, base_mip: 0, levels: 1, base_layer: 0, layers: 1 };
        reg.views.insert(0x70, view(0x80));
        reg.views.insert(0x71, view(0x81));

        let mut r = Recorder::default();
        let mut p = Pass::new(0x1, Target::RenderPass { render_pass: 0x50, framebuffer: 0x60, views: vec![], contents: 0, version: 1 }, (0, 0, 8, 8), vec![[0; 4], [0; 4]]);
        p.subpasses[0].indexed = 2;
        r.begin_pass(p);
        r.end_pass(0x1);
        r.events.push(Event::Submit { queue: 0x9, cmds: vec![0x1] });
        let lines = report(&r, &reg, 601);
        assert!(lines[0].starts_with("frame after present 600: 1 pass(es), 2 draw(s) in passes"), "{}", lines[0]);
        assert!(lines[1].starts_with("pass 0: cmd 0x1 (submit 0), area 8x8+0+0, 2 draw(s)"), "{}", lines[1]);
        assert!(lines[2].contains("att 0: B8G8R8A8_UNORM(44) 8x8 x1 usage 0x20 image 0x80 SWAPCHAIN"), "{}", lines[2]);
        assert!(lines[2].contains("load DONT_CARE store STORE, UNDEFINED -> PRESENT_SRC"), "{}", lines[2]);
        assert!(lines[3].contains("att 1: D32_SFLOAT(126) 8x8 x4") && lines[3].contains("clear depth 0 stencil 0"), "{}", lines[3]);
        assert!(lines[4].contains("subpass 0: color [0] resolve [] depth 1 (DEPTH_STENCIL_ATTACHMENT) input [], 2 draw(s)"), "{}", lines[4]);
        assert_eq!(lines[5], "submit to queue 0x9: cmd(s) 0x1");
        assert!(lines.iter().any(|l| l == "2 image(s) that can be drawn into:"));
    }

    #[test]
    fn nothing_is_hooked_for_a_name_that_is_not_traced_or_missing() {
        let p = 0x1234 as *mut c_void;
        assert_eq!(hook(b"vkCmdSetViewport", p), p);
        assert!(ours(b"vkCmdSetViewport").is_none() && ours(b"vkCmdBindVertexBuffers").is_none());
        assert!(ours(b"vkCmdBindPipeline").is_some() && ours(b"vkBeginCommandBuffer").is_some(), "for the reflections");
        assert!(hook(b"vkCmdDraw", std::ptr::null_mut()).is_null());
        assert!(ours(b"vkCmdDraw").is_some() && ours(b"vkCmdBeginRenderPass2KHR").is_some());
        assert!(ours(b"vkCreateImage").is_none(), "images are vulkan_etc's single hook");
    }
}
