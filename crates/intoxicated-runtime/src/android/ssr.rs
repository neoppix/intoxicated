//! Screen-space reflections: `INTOXICATED_SSR=1`, and devctl `ssr` to turn
//! them on and off, change how they look and read their GPU cost while the
//! game runs. The same point of the frame also takes the depth of field
//! (`INTOXICATED_DOF=1`, devctl `dof`), the bloom (`INTOXICATED_BLOOM=1`,
//! devctl `bloom`), the lens effects (`INTOXICATED_LENS=1`, devctl `lens`)
//! and the sharpening (`INTOXICATED_SHARPEN=1`, devctl `sharpen`) of
//! [`post`], each switched on its own, recorded in the order of
//! [`EFFECTS`]: reflections, depth of field, bloom, lens, sharpen.
//! Everything below about the frame, the match, what is put back and our
//! objects holds for all of them; a frame that does not match gets none of
//! them.
//!
//! The engine's frame is not changed in any other way than this: a few
//! commands of ours are recorded into its own command buffer, through the
//! Vulkan API, at one point of the frame. Nothing of the engine's memory,
//! scripts or game state is read or written.
//!
//! # Where they go
//!
//! Roblox's Android engine draws its 3D scene into a multisampled colour and
//! depth pair (an opaque pass that clears the depth, then a transparent pass
//! that loads it), resolves the colour into a single-sample image, and then
//! begins one render pass on the swapchain image that draws that image to the
//! screen with a single full-screen draw before it draws the HUD on top
//! (measured on Natural Disaster Survival; see the commit that added this
//! module). The reflections change that resolved image after the resolve and
//! before the swapchain pass begins, so the HUD is drawn over them and never
//! reflected.
//!
//! # How the frame is matched
//!
//! Per command buffer, from its `vkBeginCommandBuffer`, by what the passes
//! draw into ([`History`]), never by a pass index, a handle kept from an
//! earlier frame or an engine address:
//!
//! * a pass with a depth attachment the size of a live swapchain image that
//!   clears it starts the scene; a later pass that loads the same depth
//!   continues it;
//! * a resolve from a colour attachment of those passes into a single-sample
//!   image of the same size is the scene colour (a single-sample scene's own
//!   colour attachment is that image, with nothing to resolve);
//! * the first pass after both that has no depth attachment and draws into
//!   a swapchain image is where the work is recorded, inside the hook of its
//!   `vkCmdBeginRenderPass` (or `2`, or `vkCmdBeginRendering`), before the
//!   call is forwarded.
//!
//! A command buffer that does not have this shape gets nothing, and the
//! reason is printed once.
//!
//! # What is recorded
//!
//! A barrier that waits for the scene; one compute dispatch (`ssr/depth.comp`)
//! that copies sample 0 of the scene depth into a single-sample R32F image of
//! ours, which the march reads far faster than the multisampled original;
//! a second (`ssr/ssr.comp`, the trace) that marches every wet pixel's
//! reflected ray through that depth and writes the colour it found and how
//! strongly it shows into an RGBA16F image of ours; a third (the same source
//! with `COMPOSE`) that blurs that over the pixel's own surface, blends it
//! into the scene colour and writes every pixel, with a reflection blended in
//! or unchanged, packed in the engine's colour format into a 32-bit image of
//! ours; then the depth of field's, the bloom's, the lens's and the
//! sharpen's dispatches when they are on ([`post`]), each reading the scene
//! from that image (or from the engine's when nothing ran before it) and
//! writing it back there; one copy of that image over the engine's; and a
//! barrier that puts the engine's image back in the layout it was in. The
//! depth copy is made when the reflections, the depth of field or a
//! depth-aware sharpen need it. Reading needs `SAMPLED` and the copy
//! `TRANSFER_DST` on the engine's image; a resolve target has both (it was
//! seen with `0x17`), and with SSR on, a candidate image created without them
//! gets them added at `vkCreateImage` when its format supports them
//! ([`widen_usage`]). Eight timestamps (before, after the depth copy and
//! after each effect, after the copy back) feed each effect's `stats`, and
//! the whole for all of them.
//!
//! # What is put back
//!
//! Our dispatch changes the command buffer's compute state: the bound compute
//! pipeline, the compute descriptor sets and the compute push constants. The
//! engine's own compute binds in that command buffer are recorded as they
//! pass ([`Compute`]) and replayed after the dispatch, so a dispatch of the
//! engine's later in the command buffer finds what it bound. Graphics state
//! is not touched. A command buffer whose compute state cannot be replayed
//! (push descriptors, descriptor buffers, shader objects, or a push constant
//! call shared with graphics stages) is left alone. On Natural Disaster
//! Survival the engine dispatches once, at the start of the frame, and never
//! after our work, so that a later dispatch finds its state is INFERRED from
//! Vulkan's rules, not seen.
//!
//! Also not seen in a game, and so INFERRED: dynamic rendering (the engine
//! uses render passes; the matcher reads `vkCmdBeginRendering` the same way)
//! and [`widen_usage`] (the scene colour had `SAMPLED` and `TRANSFER_DST` at
//! MSAA 4 and at MSAA 1 alike, so nothing was widened).
//!
//! # How they look
//!
//! Every look is a runtime parameter ([`KNOBS`]): intensity, roughness (a
//! blur of the reflected colour that grows with the ray's length), Fresnel
//! strength, up-facing surfaces only or all and the threshold for "up", the
//! ray's maximum length, the fade at the screen edges, how much wet surfaces
//! are darkened and tinted and how much of their own colour the reflection
//! takes; also the march's steps and thickness and the camera's field of
//! view. Each has an `INTOXICATED_SSR_*` variable and is changed live with
//! devctl `ssr set <name> <value>`. They reach the shader as push constants
//! ([`Params`]), recorded with each frame. [`LOOKS`] names whole sets of
//! them (`INTOXICATED_SSR_LOOK`, devctl `ssr look <name>`).
//!
//! Whatever the knobs, a reflection is never sharper than the compose's
//! blur, and a hit fades rather than cuts out where it is doubtful: behind
//! the surface it hit, on an edge of it, at the end of a finite ray, near the
//! screen edge, or on a surface seen nearly edge on.
//!
//! # All off and back
//!
//! The effects hotkey (`crate::effects_hotkey`, F8 with
//! `INTOXICATED_EFFECTS_HOTKEY=1`) and devctl `effects toggle|on|off|get`
//! turn every effect off at once and back as configured ([`AllOff`]): off
//! saves each switch and clears it, so the hooks stop watching the frame and
//! nothing of ours is recorded into it; back on puts the saved switches back,
//! the knobs and looks never having been touched. Our objects stay made
//! meanwhile, costing memory but no GPU time. That in game the hooks then
//! record nothing and the effects come back as they were is INFERRED from
//! this code and the unit tests: it was not run in a game when it was
//! added.
//!
//! # Our objects
//!
//! Made on the render thread at the first matched frame of a device and kept
//! until `vkDestroyDevice`: four pipelines (the depth copy for a
//! multisampled and a single-sample depth, the trace and the compose), their
//! layout, two samplers, a descriptor pool, a query pool, the depth copy, the
//! trace and the 32-bit image. Views of the engine's images and the descriptor sets
//! that name them are made when first needed and dropped when the engine
//! destroys the image. When the scene changes size (a swapchain resize) our
//! three images are made again and the old ones destroyed some presents
//! later, when no frame in flight can still use them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use super::depth_capture::{
    barrier_aspects, sampleable as depth_sampleable, ComputePipelineCreateInfo, CreateFn, DescriptorImageInfo,
    DescriptorPoolCreateInfo, DescriptorPoolSize, DescriptorSetAllocateInfo, DescriptorSetLayoutBinding,
    DescriptorSetLayoutCreateInfo, DestroyFn, ImageViewCreateInfo, PipelineLayoutCreateInfo,
    PipelineShaderStageCreateInfo, PushConstantRange, SamplerCreateInfo, ShaderModuleCreateInfo, WriteDescriptorSet,
};
use super::frame_capture::{
    self, device_fn, Dev, ImageMemoryBarrier, ImageSubresourceRange, MemoryAllocateInfo, MemoryBarrier,
    MemoryRequirements, PhysicalDeviceMemoryProperties,
};
use super::vk_layer::passes::{self, Pass, Target, WATCH, WATCH_POST};

pub(crate) mod post;
use super::vk_layer::registry::{self, Registry, UNUSED};

// ------------------------------------------------------------- switches

/// Whether the effects are built in (the `shaders` feature): here they are.
/// `android::no_effects` stands in without them.
pub const AVAILABLE: bool = true;

/// The switch: `1` draws reflections from the start.
pub const ENV: &str = "INTOXICATED_SSR";

/// What kind of value a knob takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    Number { min: f32, max: f32 },
    Whole { min: u32, max: u32 },
    Switch,
    /// Three numbers `r,g,b`, or one for all three.
    Rgb { min: f32, max: f32 },
}

/// One look of an effect, changed at run time.
pub(crate) struct Knob {
    /// Its name in devctl `<effect> set`.
    pub name: &'static str,
    pub env: &'static str,
    pub kind: Kind,
    /// Only the first value counts but for [`Kind::Rgb`].
    pub default: [f32; 3],
    /// Where its value is kept in its effect's values: one slot, three for a
    /// colour.
    pub slot: usize,
}

pub(crate) const fn knob(name: &'static str, env: &'static str, kind: Kind, default: [f32; 3], slot: usize) -> Knob {
    Knob { name, env, kind, default, slot }
}

/// Every look, in the order `ssr get` lists them. The defaults are the
/// look measured on Timebomb Duels (place 11379739543), a competitive game:
/// a faint, soft reflection on floors only that adds depth without drawing
/// the eye, never a sharp mirror. [`LOOKS`] has the others.
pub(crate) const KNOBS: [Knob; 13] = [
    // How much of the reflection is blended in.
    knob("intensity", "INTOXICATED_SSR_INTENSITY", Kind::Number { min: 0.0, max: 1.0 }, [INTENSITY; 3], 0),
    // Blur of the reflected colour, more the further the ray went, and a
    // wider spacing of the compose's blur.
    knob("roughness", "INTOXICATED_SSR_ROUGHNESS", Kind::Number { min: 0.0, max: 1.0 }, [ROUGHNESS; 3], 1),
    // 0: the same at every angle, like a mirror; 1: strong only at grazing
    // angles.
    knob("fresnel", "INTOXICATED_SSR_FRESNEL", Kind::Number { min: 0.0, max: 1.0 }, [FRESNEL; 3], 2),
    // 1: only surfaces that face up are wet; 0: every surface.
    knob("up_only", "INTOXICATED_SSR_UP_ONLY", Kind::Switch, [1.0; 3], 3),
    // The cosine of the angle from straight up at which a surface counts as
    // facing up (faded over 0.15 either side), in view space.
    knob("up_threshold", "INTOXICATED_SSR_UP_THRESHOLD", Kind::Number { min: 0.0, max: 1.0 }, [UP_THRESHOLD; 3], 4),
    // The longest ray, in multiples of the surface's distance from the
    // camera; 0 for no limit (and the sky).
    knob("max_distance", "INTOXICATED_SSR_MAX_DISTANCE", Kind::Number { min: 0.0, max: 1000.0 }, [0.0; 3], 5),
    // Fade towards the screen edges, as a fraction of the smaller side.
    knob("edge_fade", "INTOXICATED_SSR_EDGE_FADE", Kind::Number { min: 0.0, max: 0.5 }, [EDGE_FADE; 3], 6),
    // How much darker wet surfaces are.
    knob("darken", "INTOXICATED_SSR_DARKEN", Kind::Number { min: 0.0, max: 1.0 }, [0.0; 3], 7),
    // The colour wet surfaces are multiplied by.
    knob("tint", "INTOXICATED_SSR_TINT", Kind::Rgb { min: 0.0, max: 2.0 }, [1.0; 3], 8),
    // Steps along a ray (denser near its start) before the binary search:
    // quality for GPU time.
    knob("steps", "INTOXICATED_SSR_STEPS", Kind::Whole { min: 4, max: 64 }, [32.0; 3], 11),
    // How far behind a drawn surface a ray may be and still hit it, as a
    // fraction of the surface's distance; hits fade out over the second
    // half of it.
    knob("thickness", "INTOXICATED_SSR_THICKNESS", Kind::Number { min: 0.005, max: 1.0 }, [0.06; 3], 12),
    // The camera's vertical field of view in degrees; Roblox's default
    // `Camera.FieldOfView` is 70.
    knob("fov", "INTOXICATED_SSR_FOV", Kind::Number { min: 10.0, max: 150.0 }, [70.0; 3], 13),
    // 0: the reflection has the colours of what it reflects; 1: their
    // brightness in the hue of the surface it lies on, so water stays blue
    // (INFERRED: Timebomb Duels has no water to try it on).
    knob("keep_colour", "INTOXICATED_SSR_KEEP_COLOUR", Kind::Number { min: 0.0, max: 1.0 }, [KEEP_COLOUR; 3], 14),
];

// The competitive defaults, shared with the look of that name.
const INTENSITY: f32 = 0.4;
const ROUGHNESS: f32 = 0.4;
const FRESNEL: f32 = 0.9;
const UP_THRESHOLD: f32 = 0.6;
const EDGE_FADE: f32 = 0.1;
const KEEP_COLOUR: f32 = 0.5;

const SLOTS: usize = 15;

static ON: AtomicBool = AtomicBool::new(false);

/// The knobs' values as `f32` bits, by slot.
static VALUES: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];

#[cfg(test)]
pub(crate) fn find(name: &str) -> Option<&'static Knob> {
    SSR.find(name)
}

/// The variable that starts from a look; the knobs' own variables then
/// change it.
pub const LOOK_ENV: &str = "INTOXICATED_SSR_LOOK";

/// A named set of knob values, for the settings app's Shaders page, an
/// effect's look variable and devctl `<effect> look <name>`. A look sets
/// every knob it names to its value here and every other one to its
/// default, but for the knobs in its effect's `not_a_look`, which stay as
/// they are.
pub(crate) struct Look {
    pub name: &'static str,
    /// One line for the Shaders page, under the look's button.
    pub description: &'static str,
    pub values: &'static [(&'static str, [f32; 3])],
}

/// Quality for GPU time and the camera, not how the reflections look.
const NOT_A_LOOK: [&str; 3] = ["steps", "thickness", "fov"];

/// The looks, the default first. Only the default was tuned in game, on
/// Timebomb Duels; the others are starting points whose look is INFERRED
/// (`ssr look wet_road` was applied live there, not judged).
pub(crate) const LOOKS: [Look; 5] = [
    Look {
        name: "competitive",
        description: "Faint and soft, on floors only: depth without drawing the eye. Tuned for competitive play.",
        values: &[],
    },
    Look {
        name: "wet_road",
        description: "Darker, slightly blue wet tarmac with a clear, fairly strong reflection.",
        values: &[
            ("intensity", [0.55; 3]),
            ("roughness", [0.25; 3]),
            ("fresnel", [0.8; 3]),
            ("darken", [0.3; 3]),
            ("tint", [0.92, 0.95, 1.0]),
            ("keep_colour", [0.3; 3]),
        ],
    },
    Look {
        name: "clean_mirror",
        description: "As sharp as the reflections get, and strong at every angle.",
        values: &[("intensity", [0.9; 3]), ("roughness", [0.0; 3]), ("fresnel", [0.3; 3]), ("keep_colour", [0.0; 3])],
    },
    Look {
        name: "puddles",
        description: "Only the flattest floors, darkened, with a strong and fairly clear reflection.",
        values: &[
            ("intensity", [0.7; 3]),
            ("roughness", [0.15; 3]),
            ("fresnel", [0.6; 3]),
            ("up_threshold", [0.85; 3]),
            ("darken", [0.15; 3]),
            ("keep_colour", [0.2; 3]),
        ],
    },
    Look {
        name: "glossy",
        description: "Every surface a little glossy, walls too, each in its own colour.",
        values: &[("intensity", [0.4; 3]), ("roughness", [0.2; 3]), ("fresnel", [0.9; 3]), ("up_only", [0.0; 3]), ("keep_colour", [0.7; 3])],
    },
];

pub(crate) const USAGE: &str = "ssr on|off|stats|get|reset|look <name>|intensity <0..1>|set <name> <value>; names: \
     intensity roughness fresnel up_only up_threshold max_distance edge_fade darken tint steps thickness fov keep_colour; \
     looks: competitive wet_road clean_mirror puddles glossy";

/// One of the effects recorded before the swapchain pass: its switch, its
/// knobs and looks and where their values live. [`SSR`] here,
/// [`post::DOF`] and [`post::BLOOM`] beside it, in the order they are
/// recorded ([`EFFECTS`]).
pub(crate) struct Effect {
    /// Its devctl verb and the name its log lines carry.
    pub name: &'static str,
    /// What the Shaders page calls it, and one line on what it does.
    pub title: &'static str,
    pub summary: &'static str,
    /// Its place in [`EFFECTS`], the order of recording.
    pub index: usize,
    /// The switch and the look variable.
    pub env: &'static str,
    pub look_env: &'static str,
    pub knobs: &'static [Knob],
    /// The default first.
    pub looks: &'static [Look],
    /// Knobs a look leaves as they are.
    pub not_a_look: &'static [&'static str],
    /// The knobs' values as `f32` bits, by slot.
    pub values: &'static [AtomicU32],
    pub on: &'static AtomicBool,
    pub usage: &'static str,
}

pub(crate) static SSR: Effect = Effect {
    name: "ssr",
    title: "Reflections (SSR)",
    summary: "Real reflections from the game's own depth, for about 0.5 ms of GPU time a frame. Only what is on \
              screen can be reflected.",
    index: 0,
    env: ENV,
    look_env: LOOK_ENV,
    knobs: &KNOBS,
    looks: &LOOKS,
    not_a_look: &NOT_A_LOOK,
    values: &VALUES,
    on: &ON,
    usage: USAGE,
};

/// Every effect, in the order their work is recorded: the reflections
/// change the scene first, the depth of field then blurs it, reflections
/// included, and the bloom glows over the result, so a light blurred out of
/// focus glows as the soft disc it has become, not as the sharp point it
/// was. The lens comes after them as a camera's lens would, over the whole
/// picture, glow included, and its fringe reads the bloom's chain. The
/// sharpen comes last, so the texture detail it raises is the final
/// picture's and nothing after it softens it again; it holds back where
/// contrast is already high (the bloom's and the fringe's smooth gradients
/// have little to raise), so it does not sharpen the glow into rings.
pub(crate) static EFFECTS: [&Effect; 5] = [&SSR, &post::DOF, &post::BLOOM, &post::LENS, &post::SHARPEN];

/// The effect a devctl verb names.
pub(crate) fn effect(name: &str) -> Option<&'static Effect> {
    EFFECTS.iter().copied().find(|fx| fx.name == name)
}

/// The variable that picks a whole-page mode ([`MODES`]).
pub const MODE_ENV: &str = "INTOXICATED_SHADERS_MODE";

/// Every effect's switch and look at once. A mode only fills in what is not
/// set: an effect's own switch or look variable, when it is set, wins over
/// the mode, and the knobs' variables then change the look as always.
pub(crate) struct Mode {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Per effect, in the order of [`EFFECTS`]: on, and the look it starts
    /// from.
    pub effects: [(bool, &'static str); N],
}

/// The modes, for `INTOXICATED_SHADERS_MODE`, devctl `shaders mode` and the
/// top of the Shaders page.
pub(crate) const MODES: [Mode; 3] = [
    Mode {
        name: "off",
        title: "Off",
        description: "Every effect off: the game as the engine draws it.",
        effects: [(false, "competitive"), (false, "playable"), (false, "soft"), (false, "subtle"), (false, "light")],
    },
    Mode {
        name: "competitive",
        title: "Competitive",
        description: "Prettier but playable: faint, soft reflections on floors, only the far background softened, a \
                      subtle glow and light sharpening. No lens effects, nothing that hides players or blinds.",
        effects: [(true, "competitive"), (true, "playable"), (true, "soft"), (false, "subtle"), (true, "light")],
    },
    Mode {
        name: "cinematic",
        title: "Cinematic",
        description: "For clips: clear reflections, your character in focus against a soft background, strong \
                      glow, a colour split and rainbow halo like a lens, crunchy sharpening.",
        effects: [(true, "clean_mirror"), (true, "cinematic"), (true, "neon"), (true, "prism"), (true, "crunchy")],
    },
];

pub(crate) fn mode(name: &str) -> Option<&'static Mode> {
    MODES.iter().find(|m| m.name == name)
}

pub(crate) const SHADERS_USAGE: &str = "shaders [get]|mode <name>; modes: off competitive cinematic";

/// `var` with the mode `var` names (by [`MODE_ENV`]) filled in for `fx`:
/// its switch and look variables, where they are unset or blank, read as
/// the mode sets them. A warning when the mode names no mode.
fn with_mode<'a>(
    fx: &'a Effect,
    var: impl Fn(&str) -> Option<String> + 'a,
    warnings: &mut Vec<String>,
) -> impl Fn(&str) -> Option<String> + 'a {
    let fill = match var(MODE_ENV).as_deref().map(str::trim) {
        None | Some("") => None,
        Some(name) => match mode(name) {
            Some(m) => Some(m.effects[fx.index]),
            None => {
                warnings.push(format!(
                    "[android] vulkan: {}: {MODE_ENV}={name:?} is not a mode ({}); using each effect's own settings",
                    fx.name,
                    MODES.iter().map(|m| m.name).collect::<Vec<_>>().join(" ")
                ));
                None
            }
        },
    };
    move |name: &str| {
        let own = var(name);
        let unset = own.as_deref().is_none_or(|v| v.trim().is_empty());
        match fill {
            Some((on, _)) if unset && name == fx.env => Some(if on { "1" } else { "0" }.to_string()),
            Some((_, look)) if unset && name == fx.look_env => Some(look.to_string()),
            _ => own,
        }
    }
}

/// What the environment asks for: on or off, every knob's value, and a line
/// for each value that was not understood (and left at its default).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub on: bool,
    pub values: Vec<[f32; 3]>,
    pub warnings: Vec<String>,
}

/// One devctl command for an effect.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    On,
    Off,
    /// A knob and its new value.
    Set(&'static str, [f32; 3]),
    /// Every knob back to its built-in default.
    Reset,
    /// A look, by name.
    Look(&'static str),
    /// The knobs' values.
    Get,
    Stats,
}

impl Effect {
    pub(crate) fn find(&self, name: &str) -> Option<&'static Knob> {
        self.knobs.iter().find(|k| k.name == name)
    }

    pub(crate) fn is_on(&self) -> bool {
        self.on.load(Relaxed)
    }

    pub(crate) fn get(&self, k: &Knob) -> [f32; 3] {
        let mut v = [0.0; 3];
        for (i, slot) in v.iter_mut().enumerate().take(k.width()) {
            *slot = f32::from_bits(self.values[k.slot + i].load(Relaxed));
        }
        v
    }

    fn set(&self, k: &Knob, v: [f32; 3]) {
        for (i, value) in v.iter().enumerate().take(k.width()) {
            self.values[k.slot + i].store(value.to_bits(), Relaxed);
        }
    }

    /// Every knob's value now, as [`Effect::knobs`] orders them.
    pub(crate) fn current(&self) -> Vec<[f32; 3]> {
        self.knobs.iter().map(|k| self.get(k)).collect()
    }

    /// What look `name` does to each knob, in knob order: its new value, or
    /// `None` to leave it.
    pub(crate) fn look_values(&self, name: &str) -> Option<Vec<Option<[f32; 3]>>> {
        let look = self.looks.iter().find(|l| l.name == name)?;
        Some(
            self.knobs
                .iter()
                .map(|k| {
                    let named = look.values.iter().find(|(n, _)| *n == k.name).map(|&(_, v)| v);
                    if self.not_a_look.contains(&k.name) {
                        named
                    } else {
                        Some(named.unwrap_or(k.default))
                    }
                })
                .collect(),
        )
    }

    /// Every knob's value at start from look `name` (in knob order), before
    /// the knobs' own variables: what the look sets, the default for the
    /// rest.
    fn start_values(&self, name: &str) -> Option<Vec<[f32; 3]>> {
        let look = self.look_values(name)?;
        Some(self.knobs.iter().zip(look).map(|(k, v)| v.unwrap_or(k.default)).collect())
    }

    /// The look whose values every knob has now, if any.
    fn look_now(&self) -> Option<&'static str> {
        let now = self.current();
        // Only a colour's three values count; another knob keeps one.
        let same = |k: &Knob, a: [f32; 3], b: [f32; 3]| a[..k.width()] == b[..k.width()];
        self.looks.iter().map(|l| l.name).find(|name| {
            self.look_values(name).is_some_and(|look| {
                self.knobs.iter().zip(look).zip(&now).all(|((k, want), have)| want.is_none_or(|w| same(k, w, *have)))
            })
        })
    }

    fn look_names(&self) -> String {
        self.looks.iter().map(|l| l.name).collect::<Vec<_>>().join(" ")
    }

    /// The effect as one JSON object: the switch and look variables, every
    /// knob (its name, variable, kind, range and default) and every look
    /// with the value it starts each knob at, the default look named.
    pub(crate) fn json(&self) -> serde_json::Value {
        use serde_json::{json, Value};
        let knobs: Vec<Value> = self
            .knobs
            .iter()
            .map(|k| {
                let (kind, min, max) = match k.kind {
                    Kind::Number { min, max } => ("number", json_number(min), json_number(max)),
                    Kind::Whole { min, max } => ("whole", json!(min), json!(max)),
                    Kind::Switch => ("switch", json!(0), json!(1)),
                    Kind::Rgb { min, max } => ("rgb", json_number(min), json_number(max)),
                };
                json!({
                    "name": k.name,
                    "env": k.env,
                    "kind": kind,
                    "min": min,
                    "max": max,
                    "default": json_value(k, k.default),
                })
            })
            .collect();
        let looks: Vec<Value> = self
            .looks
            .iter()
            .map(|l| {
                let start = self.start_values(l.name).expect("a look of the table");
                let values: serde_json::Map<String, Value> =
                    self.knobs.iter().zip(start).map(|(k, v)| (k.name.to_string(), json_value(k, v))).collect();
                json!({ "name": l.name, "description": l.description, "values": values })
            })
            .collect();
        json!({
            "switch": self.env,
            "look_env": self.look_env,
            "default_look": self.looks[0].name,
            "knobs": knobs,
            "looks": looks,
        })
    }

    /// The effect's settings from `var`, with the mode [`MODE_ENV`] names
    /// filled in.
    pub(crate) fn parse_settings(&self, var: impl Fn(&str) -> Option<String>) -> Settings {
        let mut warnings = Vec::new();
        let var = with_mode(self, var, &mut warnings);
        let mut s = self.parse_own_settings(var);
        warnings.append(&mut s.warnings);
        s.warnings = warnings;
        s
    }

    fn parse_own_settings(&self, var: impl Fn(&str) -> Option<String>) -> Settings {
        let (name, env, look_env) = (self.name, self.env, self.look_env);
        let mut warnings = Vec::new();
        let on = match var(env).as_deref().map(str::trim) {
            None | Some("" | "0") => false,
            Some("1") => true,
            Some(v) => {
                warnings.push(format!("[android] vulkan: {name}: {env}={v:?} is not understood; leaving it off"));
                false
            }
        };
        let defaults = || self.knobs.iter().map(|k| k.default).collect();
        let start: Vec<[f32; 3]> = match var(look_env).as_deref().map(str::trim) {
            None | Some("") => defaults(),
            Some(look) => match self.start_values(look) {
                Some(values) => values,
                None => {
                    warnings.push(format!(
                        "[android] vulkan: {name}: {look_env}={look:?} is not a look ({}); using the defaults",
                        self.look_names()
                    ));
                    defaults()
                }
            },
        };
        let values = self
            .knobs
            .iter()
            .zip(start)
            .map(|(k, start)| match var(k.env).filter(|v| !v.trim().is_empty()) {
                None => start,
                Some(v) => k.parse(&v).unwrap_or_else(|why| {
                    warnings.push(format!("[android] vulkan: {name}: {}: {why}; using {}", k.env, k.show(start)));
                    start
                }),
            })
            .collect();
        Settings { on, values, warnings }
    }

    /// Set the switch and the knobs from the environment and say what was
    /// asked for; whether the effect is on.
    fn start_from(&self, var: impl Fn(&str) -> Option<String>) -> bool {
        let s = self.parse_settings(var);
        for w in &s.warnings {
            // The same warning about the mode comes for every effect.
            if self.index == 0 || !w.contains(MODE_ENV) {
                println!("{w}");
            }
        }
        for (k, v) in self.knobs.iter().zip(&s.values) {
            self.set(k, *v);
        }
        if s.on {
            println!("[android] vulkan: {}: on ({}=1): {}", self.name, self.env, self.values_line());
        }
        self.on.store(s.on, Relaxed);
        s.on
    }

    /// Every knob as `name=value`.
    fn values_line(&self) -> String {
        self.knobs.iter().map(|k| format!("{}={}", k.name, k.show(self.get(k)))).collect::<Vec<_>>().join(" ")
    }

    pub(crate) fn parse_command(&self, args: &[&str]) -> Result<Command, String> {
        let set = |name: &str, v: &str| {
            let k = self.find(name).ok_or_else(|| format!("no knob {name:?}; {}", self.usage))?;
            Ok(Command::Set(k.name, k.parse(v)?))
        };
        match args {
            ["on"] => Ok(Command::On),
            ["off"] => Ok(Command::Off),
            [] | ["stats"] => Ok(Command::Stats),
            ["get"] => Ok(Command::Get),
            ["reset"] => Ok(Command::Reset),
            ["look", name] => match self.looks.iter().find(|l| l.name == *name) {
                Some(l) => Ok(Command::Look(l.name)),
                None => Err(format!("no look {name:?}; looks: {}", self.look_names())),
            },
            ["intensity", v] if self.find("intensity").is_some() => set("intensity", v),
            ["set", name, v] => set(name, v),
            _ => Err(self.usage.into()),
        }
    }

    /// devctl `<effect> ...`: the answer is the state after the command.
    pub(crate) fn control(&self, args: &[&str]) -> Result<String, String> {
        requested_at_start();
        match self.parse_command(args)? {
            Command::On => {
                if !passes::wanted() {
                    return Err(format!("the pass hooks are not handed out in this run (set {}=1 or dev control)", self.env));
                }
                set_on(self, true);
            }
            Command::Off => set_on(self, false),
            Command::Set(name, v) => self.set(self.find(name).expect("parsed from the table"), v),
            Command::Reset => self.knobs.iter().for_each(|k| self.set(k, k.default)),
            Command::Look(name) => {
                let look = self.look_values(name).expect("parsed from the table");
                for (k, v) in self.knobs.iter().zip(look) {
                    if let Some(v) = v {
                        self.set(k, v);
                    }
                }
            }
            Command::Get => return Ok(format!("on={} {}", u8::from(self.is_on()), self.values_line())),
            Command::Stats => {}
        }
        let st = state();
        Ok(format!(
            "on={} {} {}; all effects: {}",
            u8::from(self.is_on()),
            self.values_line(),
            st.stats[self.index].summary(),
            st.chain.summary()
        ))
    }
}

/// What look `name` does to each reflection knob; see
/// [`Effect::look_values`].
#[cfg(test)]
pub(crate) fn look_values(name: &str) -> Option<Vec<Option<[f32; 3]>>> {
    SSR.look_values(name)
}

/// A knob's value as JSON: a number (whole for a whole number or a switch),
/// or `[r, g, b]` for a colour. Each number is the shortest text that reads
/// back as the same `f32`, as `ssr get` writes it, not the `f64` the `f32`
/// widens to (0.4, not 0.4000000059604645).
fn json_value(k: &Knob, v: [f32; 3]) -> serde_json::Value {
    match k.kind {
        Kind::Rgb { .. } => serde_json::Value::Array(v.iter().map(|&f| json_number(f)).collect()),
        Kind::Whole { .. } | Kind::Switch => serde_json::Value::from(v[0] as u32),
        Kind::Number { .. } => json_number(v[0]),
    }
}

fn json_number(f: f32) -> serde_json::Value {
    let shortest: f64 = format!("{f}").parse().expect("an f32 prints as a number");
    serde_json::Number::from_f64(shortest).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// What `intoxicated-run --print-ssr-looks` prints: the reflections alone,
/// as [`Effect::json`] writes them. One JSON object, so nothing outside
/// keeps a copy of [`KNOBS`] or [`LOOKS`].
pub fn looks_json() -> String {
    serde_json::to_string_pretty(&SSR.json()).expect("a JSON value always serialises")
}

/// What `intoxicated-run --print-shader-looks` prints, for Intoxicated
/// Settings' Shaders page: `{"available": true, "effects": [...],
/// "mode_env": ..., "modes": [...]}` (a client built without the effects
/// prints `{"available": false, ...}` instead), every effect in the order it
/// is recorded, each as
/// [`Effect::json`] writes it plus its devctl name, its title and a
/// one-line summary, and every mode ([`MODES`]) with its title, a
/// description and each effect's switch and look.
pub fn shader_looks_json() -> String {
    let modes: Vec<serde_json::Value> = MODES
        .iter()
        .map(|m| {
            let effects: serde_json::Map<String, serde_json::Value> = EFFECTS
                .iter()
                .zip(m.effects)
                .map(|(fx, (on, look))| (fx.name.to_string(), serde_json::json!({ "on": on, "look": look })))
                .collect();
            serde_json::json!({ "name": m.name, "title": m.title, "description": m.description, "effects": effects })
        })
        .collect();
    let effects: Vec<serde_json::Value> = EFFECTS
        .iter()
        .map(|fx| {
            let mut v = fx.json();
            let o = v.as_object_mut().expect("an effect is an object");
            o.insert("name".into(), fx.name.into());
            o.insert("title".into(), fx.title.into());
            o.insert("summary".into(), fx.summary.into());
            v
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "available": true, "effects": effects, "mode_env": MODE_ENV, "modes": modes }))
        .expect("a JSON value always serialises")
}

impl Knob {
    fn width(&self) -> usize {
        if matches!(self.kind, Kind::Rgb { .. }) {
            3
        } else {
            1
        }
    }

    /// A value as text says it, or what is wrong with it.
    pub(crate) fn parse(&self, text: &str) -> Result<[f32; 3], String> {
        let text = text.trim();
        let number = |v: &str, min: f32, max: f32| match v.trim().parse::<f32>() {
            Ok(f) if (min..=max).contains(&f) => Ok(f),
            _ => Err(format!("{} takes a number from {min} to {max}, not {v:?}", self.name)),
        };
        match self.kind {
            Kind::Number { min, max } => number(text, min, max).map(|f| [f; 3]),
            Kind::Whole { min, max } => match text.parse::<u32>() {
                Ok(n) if (min..=max).contains(&n) => Ok([n as f32; 3]),
                _ => Err(format!("{} takes a whole number from {min} to {max}, not {text:?}", self.name)),
            },
            Kind::Switch => match text {
                "1" => Ok([1.0; 3]),
                "0" => Ok([0.0; 3]),
                _ => Err(format!("{} takes 0 or 1, not {text:?}", self.name)),
            },
            Kind::Rgb { min, max } => {
                let parts: Vec<&str> = text.split(',').collect();
                match parts.as_slice() {
                    [one] => number(one, min, max).map(|f| [f; 3]),
                    [r, g, b] => Ok([number(r, min, max)?, number(g, min, max)?, number(b, min, max)?]),
                    _ => Err(format!("{} takes r,g,b (each {min} to {max}) or one number, not {text:?}", self.name)),
                }
            }
        }
    }

    /// The value as `<effect> get` and the variables write it.
    pub(crate) fn show(&self, v: [f32; 3]) -> String {
        match self.kind {
            Kind::Rgb { .. } => format!("{},{},{}", v[0], v[1], v[2]),
            _ => format!("{}", v[0]),
        }
    }
}

/// The reflections' settings from the environment; see
/// [`Effect::parse_settings`].
#[cfg(test)]
pub(crate) fn parse_settings(var: impl Fn(&str) -> Option<String>) -> Settings {
    SSR.parse_settings(var)
}

/// Read the environment once, set every effect's switch and knobs and say
/// what was asked for. Whether any effect is on from the start.
pub(crate) fn requested_at_start() -> bool {
    static AT_START: OnceLock<bool> = OnceLock::new();
    *AT_START.get_or_init(|| {
        let mut any = false;
        for fx in EFFECTS {
            any |= fx.start_from(|name| std::env::var(name).ok());
        }
        any
    })
}

/// Turn an effect on or off for the frames recorded from now on; the
/// recording hooks watch while any is on. While everything is off by the
/// hotkey ([`AllOff`]), this is a choice of the user's that the saved
/// switches follow.
fn set_on(fx: &Effect, on: bool) {
    all_off().switch_changed(fx.index, on);
    fx.on.store(on, Relaxed);
    watch_follows();
}

/// Watch the frame for the effects while any is on, and not at all while
/// none is: then the hooks record nothing and call nothing of ours.
fn watch_follows() {
    if EFFECTS.iter().any(|e| e.is_on()) {
        WATCH.fetch_or(WATCH_POST, Relaxed);
    } else {
        WATCH.fetch_and(!WATCH_POST, Relaxed);
    }
}

// ------------------------------------------------------------- all off

/// A switch that goes off with the rest when everything is turned off, and
/// comes back as it was.
#[derive(Clone, Copy)]
pub(crate) struct Switched {
    pub name: &'static str,
    pub on: &'static AtomicBool,
}

/// Switches outside [`EFFECTS`] that go off and come back with them; none
/// yet. This is where ray tracing joins the hotkey: an RT effect that is one
/// of [`EFFECTS`] joins it by itself, with nothing to add here; one kept
/// apart, with a switch and a point in the frame of its own, is listed here
/// (`Switched { name: "rt", on: &rt::ON }`) and has [`watch_follows`] watch
/// the frame for it while its switch is on, as it does for the effects.
pub(crate) static ALSO_SWITCHED: [Switched; 0] = [];

/// Every switch the hotkey turns off: the effects' in the order of
/// [`EFFECTS`], then [`ALSO_SWITCHED`].
fn switched() -> Vec<Switched> {
    EFFECTS.iter().map(|fx| Switched { name: fx.name, on: fx.on }).chain(ALSO_SWITCHED).collect()
}

/// Everything off at once, by the effects hotkey (`crate::effects_hotkey`)
/// or devctl `effects off`, and back as it was. Off saves every switch and
/// clears it; nothing else is touched, so every look and knob stays as it
/// is, and back on puts the saved switches back. The environment is never
/// read again.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct AllOff {
    /// The switches as they were, by [`switched`]'s order; `None` while the
    /// effects are as configured.
    saved: Option<Vec<bool>>,
}

impl AllOff {
    const fn new() -> Self {
        AllOff { saved: None }
    }

    pub(crate) fn is_off(&self) -> bool {
        self.saved.is_some()
    }

    /// Save the switches and clear them; false when they are off already.
    pub(crate) fn turn_off(&mut self, switches: &[Switched]) -> bool {
        if self.saved.is_some() {
            return false;
        }
        self.saved = Some(switches.iter().map(|s| s.on.load(Relaxed)).collect());
        for s in switches {
            s.on.store(false, Relaxed);
        }
        true
    }

    /// Put the saved switches back; false when nothing was turned off.
    pub(crate) fn turn_on(&mut self, switches: &[Switched]) -> bool {
        let Some(saved) = self.saved.take() else { return false };
        for (s, on) in switches.iter().zip(saved) {
            s.on.store(on, Relaxed);
        }
        true
    }

    /// devctl switched switch `index` while everything was off. On ends the
    /// all off with the switches as they are now, as the user has chosen
    /// again; off is kept, so that one stays off when the rest come back.
    pub(crate) fn switch_changed(&mut self, index: usize, on: bool) {
        match &mut self.saved {
            Some(_) if on => self.saved = None,
            Some(saved) => {
                if let Some(s) = saved.get_mut(index) {
                    *s = false;
                }
            }
            None => {}
        }
    }

    /// The names of the saved switches that were on.
    fn saved_on(&self, switches: &[Switched]) -> Vec<&'static str> {
        let saved = self.saved.as_deref().unwrap_or(&[]);
        switches.iter().zip(saved).filter(|(_, &on)| on).map(|(s, _)| s.name).collect()
    }
}

static ALL_OFF: Mutex<AllOff> = Mutex::new(AllOff::new());

fn all_off() -> MutexGuard<'static, AllOff> {
    ALL_OFF.lock().unwrap_or_else(|e| e.into_inner())
}

fn names_or_none(names: &[&str]) -> String {
    if names.is_empty() {
        "none was on".into()
    } else {
        names.join(" ")
    }
}

/// Everything off (`to` false), back as configured (true) or the other way
/// from now (`None`), for the frames recorded from now on; the log line
/// that says what happened, `by` naming the key or `devctl`.
fn switch_all(to: Option<bool>, by: &str) -> String {
    requested_at_start();
    let switches = switched();
    let mut all = all_off();
    let off = to.map_or(!all.is_off(), |on| !on);
    let line = if off {
        if all.turn_off(&switches) {
            format!("all off ({by}); back on with it: {}", names_or_none(&all.saved_on(&switches)))
        } else {
            format!("all off already ({by})")
        }
    } else {
        let was_on = all.saved_on(&switches);
        if all.turn_on(&switches) {
            format!("back as configured ({by}): {}", names_or_none(&was_on))
        } else {
            format!("as configured already ({by})")
        }
    };
    drop(all);
    watch_follows();
    format!("[android] vulkan: effects: {line}")
}

/// The effects hotkey was pressed (`crate::effects_hotkey`): everything off,
/// or back as configured. One line in the log.
pub(crate) fn toggle_all(by: &str) {
    println!("{}", switch_all(None, by));
}

pub(crate) const EFFECTS_USAGE: &str = "effects [get]|toggle|on|off; off turns every effect off, on puts them back";

/// devctl `effects toggle|on|off|get`: what the hotkey does, without a key.
/// The answer is the state after the command.
fn effects_control(args: &[&str]) -> Result<String, String> {
    let to = match args {
        [] | ["get"] => None,
        ["toggle"] => Some(None),
        ["on"] => Some(Some(true)),
        ["off"] => Some(Some(false)),
        _ => return Err(EFFECTS_USAGE.into()),
    };
    if let Some(to) = to {
        println!("{}", switch_all(to, "devctl"));
    }
    requested_at_start();
    let switches = switched();
    let all = all_off();
    let now: Vec<String> = switches.iter().map(|s| format!("{}={}", s.name, u8::from(s.on.load(Relaxed)))).collect();
    let mut answer = format!(
        "effects={} hotkey={} {}",
        if all.is_off() { "off" } else { "configured" },
        crate::effects_hotkey::active_key().unwrap_or_else(|| "off".into()),
        now.join(" ")
    );
    if all.is_off() {
        answer += &format!("; back on: {}", names_or_none(&all.saved_on(&switches)));
    }
    Ok(answer)
}

// ------------------------------------------------------------- devctl

#[cfg(test)]
pub(crate) fn parse_command(args: &[&str]) -> Result<Command, String> {
    SSR.parse_command(args)
}

/// devctl `ssr ...`: the answer is the state after the command.
pub fn control(args: &[&str]) -> Result<String, String> {
    SSR.control(args)
}

/// devctl `ssr ...`, `dof ...`, `bloom ...`, `lens ...`, `sharpen ...`,
/// `shaders ...` or `effects ...`; `None` for another verb.
pub fn control_verb(verb: &str, args: &[&str]) -> Option<Result<String, String>> {
    if verb == "shaders" {
        return Some(shaders_control(args));
    }
    if verb == "effects" {
        return Some(effects_control(args));
    }
    effect(verb).map(|fx| fx.control(args))
}

/// devctl `shaders mode <name>`: every effect's switch and look as the mode
/// sets them; `shaders` or `shaders get`: each effect's switch and the look
/// its knobs are at (`custom` when they are at none).
fn shaders_control(args: &[&str]) -> Result<String, String> {
    requested_at_start();
    match args {
        [] | ["get"] => {}
        ["mode", name] => {
            let m = mode(name).ok_or_else(|| format!("no mode {name:?}; {SHADERS_USAGE}"))?;
            if m.effects.iter().any(|(on, _)| *on) && !passes::wanted() {
                return Err(format!("the pass hooks are not handed out in this run (set {MODE_ENV} or dev control)"));
            }
            for (fx, (on, look)) in EFFECTS.iter().zip(m.effects) {
                for (k, v) in fx.knobs.iter().zip(fx.look_values(look).expect("a mode names looks of the table")) {
                    if let Some(v) = v {
                        fx.set(k, v);
                    }
                }
                set_on(fx, on);
            }
        }
        _ => return Err(SHADERS_USAGE.into()),
    }
    let mode_now = MODES.iter().find(|m| {
        EFFECTS.iter().zip(m.effects).all(|(fx, (on, look))| fx.is_on() == on && (!on || fx.look_now() == Some(look)))
    });
    let effects: Vec<String> = EFFECTS
        .iter()
        .map(|fx| format!("{}={}:{}", fx.name, u8::from(fx.is_on()), fx.look_now().unwrap_or("custom")))
        .collect();
    Ok(format!("mode={} {}", mode_now.map_or("custom", |m| m.name), effects.join(" ")))
}

// ------------------------------------------------------------- the matcher

const LOAD_OP_LOAD: u32 = 0;
const LOAD_OP_CLEAR: u32 = 1;

/// One attachment of a pass, or the destination of a resolve, as the
/// registry knows its image.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Att {
    pub image: u64,
    pub device: usize,
    pub format: u32,
    pub samples: u32,
    pub extent: [u32; 2],
    pub usage: u32,
    pub swapchain: bool,
    pub load: u32,
    /// The layout the pass leaves it in.
    pub layout: u32,
}

/// What the matcher needs of one pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PassDesc {
    /// Whether the pass has a depth attachment, known or not.
    pub has_depth: bool,
    pub depth: Option<Att>,
    pub colors: Vec<Att>,
    /// The single-sample attachments the colours are resolved into.
    pub resolves: Vec<Att>,
}

fn att_of(image: u64, info: &registry::Image, load: u32, layout: u32) -> Att {
    Att {
        image,
        device: info.device,
        format: info.format,
        samples: info.samples,
        extent: [info.extent[0], info.extent[1]],
        usage: info.usage,
        swapchain: info.swapchain,
        load,
        layout,
    }
}

/// A pass as the matcher sees it, through the registry. Attachments whose
/// image the registry does not know are left out.
pub(crate) fn describe(pass: &Pass, reg: &Registry) -> PassDesc {
    let mut d = PassDesc::default();
    match &pass.target {
        Target::RenderPass { render_pass, framebuffer, views, .. } => {
            let atts = reg.attachments(*render_pass, *framebuffer, views);
            let Some(rp) = reg.render_passes.get(render_pass) else { return d };
            let known = |i: u32| {
                let a = atts.get(i as usize)?;
                Some(att_of(a.image, a.info.as_ref()?, a.desc.load, a.desc.final_layout))
            };
            let mut seen = HashSet::new();
            for s in &rp.subpasses {
                if !d.has_depth {
                    if let Some(r) = s.depth.filter(|r| r.attachment != UNUSED) {
                        d.has_depth = true;
                        d.depth = known(r.attachment);
                    }
                }
                for r in s.color.iter().filter(|r| r.attachment != UNUSED) {
                    if seen.insert(r.attachment) {
                        d.colors.extend(known(r.attachment));
                    }
                }
                for r in s.resolve.iter().filter(|r| r.attachment != UNUSED) {
                    if seen.insert(r.attachment) {
                        d.resolves.extend(known(r.attachment));
                    }
                }
            }
        }
        Target::Dynamic { colors, depth, .. } => {
            let known = |view: u64, load: u32, layout: u32| {
                let (image, info) = reg.image_of_view(view);
                Some(att_of(image, info?, load, layout))
            };
            for c in colors {
                d.colors.extend(known(c.view, c.load, c.layout));
                if c.resolve_view != 0 {
                    d.resolves.extend(known(c.resolve_view, LOAD_OP_LOAD, c.resolve_layout));
                }
            }
            if let Some(z) = depth.filter(|z| z.view != 0) {
                d.has_depth = true;
                d.depth = known(z.view, z.load, z.layout);
            }
        }
    }
    d
}

/// An image the matcher follows, with the layout it is in now.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tracked {
    pub att: Att,
    pub layout: u32,
}

impl Tracked {
    fn new(att: &Att) -> Tracked {
        Tracked { layout: att.layout, att: att.clone() }
    }
}

/// What the dispatch works on, with the layouts the images are in at that
/// point of the command buffer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Found {
    pub depth: Tracked,
    pub color: Tracked,
    /// The colour was resolved from a multisampled one (rather than being
    /// the single-sample scene's own attachment).
    pub resolved: bool,
}

/// One `vkCmdBindDescriptorSets` of the engine's for compute.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetBind {
    pub layout: u64,
    pub first: u32,
    pub sets: Vec<u64>,
    pub dynamic: Vec<u32>,
}

/// One `vkCmdPushConstants` of the engine's for compute.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Push {
    pub layout: u64,
    pub stages: u32,
    pub offset: u32,
    pub bytes: Vec<u8>,
}

/// The engine's compute state in one command buffer, as far as it has to be
/// put back after our dispatch.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Compute {
    pub pipeline: u64,
    pub sets: Vec<SetBind>,
    pub pushes: Vec<Push>,
    /// Why it cannot be put back, when it cannot.
    pub unrestorable: Option<&'static str>,
}

/// How many separate binds or pushes are kept before giving up; a call that
/// covers an earlier one replaces it.
const MAX_REPLAY: usize = 16;

impl Compute {
    pub(crate) fn bind_pipeline(&mut self, pipeline: u64) {
        self.pipeline = pipeline;
    }

    pub(crate) fn bind_sets(&mut self, b: SetBind) {
        let (lo, hi) = (b.first, b.first + b.sets.len() as u32);
        self.sets.retain(|o| !(o.first >= lo && o.first + o.sets.len() as u32 <= hi));
        if self.sets.len() >= MAX_REPLAY {
            self.unrestorable = Some("the engine bound more compute descriptor sets than are kept to bind again");
            return;
        }
        self.sets.push(b);
    }

    pub(crate) fn push(&mut self, p: Push) {
        if p.stages & !SHADER_STAGE_COMPUTE != 0 {
            self.unrestorable = Some("the engine pushed constants for compute and graphics stages in one call");
            return;
        }
        let (lo, hi) = (p.offset, p.offset + p.bytes.len() as u32);
        self.pushes.retain(|o| !(o.layout == p.layout && o.offset >= lo && o.offset + o.bytes.len() as u32 <= hi));
        if self.pushes.len() >= MAX_REPLAY {
            self.unrestorable = Some("the engine pushed more compute constants than are kept to push again");
            return;
        }
        self.pushes.push(p);
    }
}

/// One command buffer's recording so far. See the module doc for the rules.
#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    depth: Option<Tracked>,
    loaded: bool,
    /// The colour attachments of the scene passes.
    scene_colors: Vec<Att>,
    /// The first of them, followed for a single-sample scene.
    scene_color: Option<Tracked>,
    resolved: Option<Tracked>,
    broken: Option<String>,
    /// The work has been recorded into this command buffer.
    pub done: bool,
    pub compute: Compute,
    /// The present count when the recording began.
    pub begun: u64,
}

impl History {
    pub(crate) fn new(begun: u64) -> History {
        History { begun, ..History::default() }
    }

    /// A pass begins. `Some` when it is a swapchain pass without depth that
    /// is where the work goes: the images to work on, or why not.
    pub(crate) fn pass(&mut self, p: &PassDesc, full: &[[u32; 2]]) -> Option<Result<Found, String>> {
        let scene_depth = p.depth.as_ref().filter(|d| registry::is_depth_format(d.format) && full.contains(&d.extent));
        if let Some(d) = scene_depth {
            let same = self.depth.as_ref().is_some_and(|s| s.att.image == d.image);
            if d.load == LOAD_OP_CLEAR {
                // A new scene, whatever came before it.
                self.depth = Some(Tracked::new(d));
                self.loaded = false;
                self.broken = None;
            } else if same {
                if d.load == LOAD_OP_LOAD {
                    self.loaded = true;
                }
                if let Some(s) = self.depth.as_mut() {
                    s.layout = d.layout;
                }
            } else {
                // Another full-size depth this command buffer never cleared.
                return None;
            }
            // Anything resolved before this pass is not the finished scene.
            self.resolved = None;
            self.scene_colors = p.colors.iter().filter(|c| c.extent == d.extent).cloned().collect();
            self.scene_color = self.scene_colors.first().map(Tracked::new);
            if let Some(r) = p.resolves.iter().find(|r| r.samples == 1 && r.extent == d.extent) {
                self.resolved = Some(Tracked::new(r));
            }
            return None;
        }
        if p.has_depth || !p.colors.iter().any(|c| c.swapchain) || self.done {
            return None;
        }
        let found = self.found();
        if found.is_ok() {
            self.done = true;
        }
        Some(found)
    }

    fn found(&self) -> Result<Found, String> {
        if let Some(b) = &self.broken {
            return Err(b.clone());
        }
        let Some(depth) = &self.depth else {
            return Err("no pass cleared a depth attachment of the swapchain's size before the swapchain pass".into());
        };
        if !self.loaded {
            return Err("the scene depth was cleared but no later pass loaded it again (no transparent pass)".into());
        }
        let (color, resolved) = match (&self.resolved, &self.scene_color) {
            (Some(r), _) => (r.clone(), true),
            (None, Some(c)) if c.att.samples == 1 && depth.att.samples == 1 => (c.clone(), false),
            _ => return Err("the scene colour was not resolved into a single-sample image before the swapchain pass".into()),
        };
        if let Some(why) = self.compute.unrestorable {
            return Err(format!("{why}, so the compute state could not be put back"));
        }
        Ok(Found { depth: depth.clone(), color, resolved })
    }

    /// A `vkCmdResolveImage` from `src` into `dst` (described, when known).
    pub(crate) fn resolve(&mut self, src: u64, dst: Option<Att>) {
        let Some(depth) = &self.depth else { return };
        let Some(dst) = dst else { return };
        if self.scene_colors.iter().any(|c| c.image == src) && dst.samples == 1 && dst.extent == depth.att.extent {
            self.resolved = Some(Tracked::new(&dst));
        }
    }

    /// An image barrier moved `image` to `layout`.
    pub(crate) fn barrier(&mut self, image: u64, layout: u32) {
        for t in [&mut self.depth, &mut self.resolved, &mut self.scene_color].into_iter().flatten() {
            if t.att.image == image {
                t.layout = layout;
            }
        }
    }

    /// A copy, blit, resolve or clear wrote `image`.
    pub(crate) fn written(&mut self, image: u64) {
        if self.broken.is_none() && self.depth.as_ref().is_some_and(|d| d.att.image == image) {
            self.broken = Some("the scene depth was overwritten by a copy or clear after the scene".into());
        }
    }
}

// ------------------------------------------------------------- Vulkan ABI

const VK_SUCCESS: i32 = 0;

const ST_MEMORY_ALLOCATE_INFO: i32 = 5;
const ST_QUERY_POOL_CREATE_INFO: i32 = 11;
const ST_IMAGE_CREATE_INFO: i32 = 14;
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
const ST_IMAGE_MEMORY_BARRIER: i32 = 45;
const ST_MEMORY_BARRIER: i32 = 46;

const FORMAT_R8G8B8A8_UNORM: u32 = 37;
const FORMAT_B8G8R8A8_UNORM: u32 = 44;
const FORMAT_A2R10G10B10_UNORM: u32 = 58;
const FORMAT_A2B10G10R10_UNORM: u32 = 64;
const FORMAT_R32_UINT: u32 = 98;
const FORMAT_R32_SFLOAT: u32 = 100;
const FORMAT_R16G16B16A16_SFLOAT: u32 = 97;

const LAYOUT_UNDEFINED: u32 = 0;
const LAYOUT_GENERAL: u32 = 1;
const LAYOUT_SHADER_READ_ONLY: u32 = 5;
const LAYOUT_TRANSFER_SRC: u32 = 6;
const LAYOUT_TRANSFER_DST: u32 = 7;
const LAYOUT_PREINITIALIZED: u32 = 8;
const LAYOUT_READ_ONLY: u32 = 1_000_314_000;
const LAYOUT_PRESENT_SRC: u32 = 1_000_001_002;

const STAGE_COMPUTE_SHADER: u32 = 0x800;
const STAGE_TRANSFER: u32 = 0x1000;
const STAGE_BOTTOM_OF_PIPE: u32 = 0x2000;
const STAGE_ALL_COMMANDS: u32 = 0x1_0000;
const ACCESS_SHADER_READ: u32 = 0x20;
const ACCESS_SHADER_WRITE: u32 = 0x40;
const ACCESS_TRANSFER_READ: u32 = 0x800;
const ACCESS_TRANSFER_WRITE: u32 = 0x1000;
const ACCESS_MEMORY_READ: u32 = 0x8000;
const ACCESS_MEMORY_WRITE: u32 = 0x1_0000;

const ASPECT_COLOR: u32 = 0x1;
const ASPECT_DEPTH: u32 = 0x2;
pub(crate) const SHADER_STAGE_COMPUTE: u32 = 0x20;
pub(crate) const BIND_POINT_COMPUTE: u32 = 1;
const DESCRIPTOR_COMBINED_IMAGE_SAMPLER: u32 = 1;
const DESCRIPTOR_STORAGE_IMAGE: u32 = 3;
const DESCRIPTOR_POOL_FREE_SET: u32 = 0x1;
const QUEUE_FAMILY_IGNORED: u32 = !0;
const VIEW_TYPE_2D: u32 = 1;
const IMAGE_TYPE_2D: u32 = 1;
const TILING_OPTIMAL: u32 = 0;

const USAGE_TRANSFER_SRC: u32 = 0x1;
const USAGE_TRANSFER_DST: u32 = 0x2;
const USAGE_SAMPLED: u32 = 0x4;
const USAGE_STORAGE: u32 = 0x8;
const USAGE_COLOR_ATTACHMENT: u32 = 0x10;
const FEATURE_SAMPLED_IMAGE: u32 = 0x1;
const FEATURE_SAMPLED_LINEAR: u32 = 0x1000;
const FEATURE_STORAGE_IMAGE: u32 = 0x2;
const FEATURE_TRANSFER_SRC: u32 = 0x4000;
const FEATURE_TRANSFER_DST: u32 = 0x8000;
const MEMORY_DEVICE_LOCAL: u32 = 0x1;
const QUEUE_COMPUTE: u32 = 0x2;
const QUERY_TYPE_TIMESTAMP: u32 = 2;
const QUERY_RESULT_64: u32 = 0x1;
const QUERY_RESULT_WITH_AVAILABILITY: u32 = 0x4;

#[repr(C)]
struct ImageCreateInfo {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    image_type: u32,
    format: u32,
    extent: [u32; 3],
    mip_levels: u32,
    array_layers: u32,
    samples: u32,
    tiling: u32,
    usage: u32,
    sharing_mode: u32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
    initial_layout: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ImageSubresourceLayers {
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
struct ImageCopy {
    src_subresource: ImageSubresourceLayers,
    src_offset: [i32; 3],
    dst_subresource: ImageSubresourceLayers,
    dst_offset: [i32; 3],
    extent: [u32; 3],
}

#[repr(C)]
struct QueryPoolCreateInfo {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    query_type: u32,
    query_count: u32,
    pipeline_statistics: u32,
}

/// The push constants of `ssr/ssr.comp` (std430, which for these fields is
/// the C layout); `ssr/depth.comp` reads only `size`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Params {
    pub size: [u32; 2],
    pub tan_half: [f32; 2],
    /// rgb, and a fourth float that only aligns it.
    pub tint: [f32; 4],
    pub intensity: f32,
    pub flags: u32,
    pub steps: u32,
    pub thickness: f32,
    pub roughness: f32,
    pub fresnel: f32,
    pub up_threshold: f32,
    pub max_distance: f32,
    pub edge_fade: f32,
    pub darken: f32,
    pub keep_colour: f32,
}

/// `flags` bit 0.
pub(crate) const PARAM_UP_ONLY: u32 = 0x1;

/// How `ssr.comp`'s `pack` writes the engine's colour format, or `None` for
/// a format it cannot write.
pub(crate) fn pack_mode(format: u32) -> Option<u32> {
    match format {
        FORMAT_A2B10G10R10_UNORM => Some(0),
        FORMAT_A2R10G10B10_UNORM => Some(1),
        FORMAT_R8G8B8A8_UNORM => Some(2),
        FORMAT_B8G8R8A8_UNORM => Some(3),
        _ => None,
    }
}

/// The push constants for one frame of `extent` from the knobs' values (as
/// [`KNOBS`] orders them), for a colour format packed as `mode`.
pub(crate) fn params(extent: [u32; 2], values: &[[f32; 3]], mode: u32) -> Params {
    let v = |name: &str| KNOBS.iter().position(|k| k.name == name).map_or([0.0; 3], |i| values[i]);
    let tan_y = (v("fov")[0].to_radians() * 0.5).tan();
    let tan_x = tan_y * extent[0] as f32 / extent[1].max(1) as f32;
    let tint = v("tint");
    Params {
        size: extent,
        tan_half: [tan_x, tan_y],
        tint: [tint[0], tint[1], tint[2], 1.0],
        intensity: v("intensity")[0],
        flags: u32::from(v("up_only")[0] != 0.0) * PARAM_UP_ONLY | (mode << 8),
        steps: v("steps")[0] as u32,
        thickness: v("thickness")[0],
        roughness: v("roughness")[0],
        fresnel: v("fresnel")[0],
        up_threshold: v("up_threshold")[0],
        max_distance: v("max_distance")[0],
        edge_fade: v("edge_fade")[0],
        darken: v("darken")[0],
        keep_colour: v("keep_colour")[0],
    }
}

/// Whether knobs with these values (as [`KNOBS`] orders them) leave every
/// pixel as it was, so nothing needs recording.
pub(crate) fn changes_nothing(values: &[[f32; 3]]) -> bool {
    let p = params([1, 1], values, 0);
    p.intensity <= 0.0 && p.darken <= 0.0 && p.tint[..3] == [1.0; 3]
}

/// `ssr/depth.comp` for a multisampled scene depth, as build.rs made it.
const SPIRV_DEPTH_MS: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_depth_ms.spv.rs"));
/// The same for a single-sample one.
const SPIRV_DEPTH_1X: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_depth_1x.spv.rs"));
/// `ssr/ssr.comp`: the trace.
const SPIRV_SSR: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_ssr.spv.rs"));
/// `ssr/ssr.comp` with `COMPOSE`.
const SPIRV_COMPOSE: &[u32] = include!(concat!(env!("OUT_DIR"), "/ssr_compose.spv.rs"));

/// The device-level commands used here, straight from the driver, so none of
/// them passes through the layer's own hooks or shows in a trace.
struct Fns {
    create_image: CreateFn<ImageCreateInfo>,
    destroy_image: DestroyFn,
    image_requirements: extern "C" fn(Dev, u64, *mut MemoryRequirements),
    allocate_memory: CreateFn<MemoryAllocateInfo>,
    free_memory: DestroyFn,
    bind_image_memory: extern "C" fn(Dev, u64, u64, u64) -> i32,
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
    free_descriptor_sets: extern "C" fn(Dev, u64, u32, *const u64) -> i32,
    update_descriptor_sets: extern "C" fn(Dev, u32, *const WriteDescriptorSet, u32, *const c_void),
    create_query_pool: CreateFn<QueryPoolCreateInfo>,
    destroy_query_pool: DestroyFn,
    #[allow(clippy::type_complexity)]
    get_query_pool_results: extern "C" fn(Dev, u64, u32, u32, usize, *mut c_void, u64, u32) -> i32,
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
    cmd_bind_pipeline: extern "C" fn(*mut c_void, u32, u64),
    #[allow(clippy::type_complexity)]
    cmd_bind_descriptor_sets: extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u64, u32, *const u32),
    cmd_push_constants: extern "C" fn(*mut c_void, u64, u32, u32, u32, *const c_void),
    cmd_dispatch: extern "C" fn(*mut c_void, u32, u32, u32),
    cmd_copy_image: extern "C" fn(*mut c_void, u64, i32, u64, i32, u32, *const ImageCopy),
    cmd_reset_query_pool: extern "C" fn(*mut c_void, u64, u32, u32),
    cmd_write_timestamp: extern "C" fn(*mut c_void, u32, u64, u32),
}

impl Fns {
    fn resolve(device: Dev) -> Result<Fns, String> {
        Ok(Fns {
            create_image: device_fn(device, c"vkCreateImage")?,
            destroy_image: device_fn(device, c"vkDestroyImage")?,
            image_requirements: device_fn(device, c"vkGetImageMemoryRequirements")?,
            allocate_memory: device_fn(device, c"vkAllocateMemory")?,
            free_memory: device_fn(device, c"vkFreeMemory")?,
            bind_image_memory: device_fn(device, c"vkBindImageMemory")?,
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
            free_descriptor_sets: device_fn(device, c"vkFreeDescriptorSets")?,
            update_descriptor_sets: device_fn(device, c"vkUpdateDescriptorSets")?,
            create_query_pool: device_fn(device, c"vkCreateQueryPool")?,
            destroy_query_pool: device_fn(device, c"vkDestroyQueryPool")?,
            get_query_pool_results: device_fn(device, c"vkGetQueryPoolResults")?,
            cmd_pipeline_barrier: device_fn(device, c"vkCmdPipelineBarrier")?,
            cmd_bind_pipeline: device_fn(device, c"vkCmdBindPipeline")?,
            cmd_bind_descriptor_sets: device_fn(device, c"vkCmdBindDescriptorSets")?,
            cmd_push_constants: device_fn(device, c"vkCmdPushConstants")?,
            cmd_dispatch: device_fn(device, c"vkCmdDispatch")?,
            cmd_copy_image: device_fn(device, c"vkCmdCopyImage")?,
            cmd_reset_query_pool: device_fn(device, c"vkCmdResetQueryPool")?,
            cmd_write_timestamp: device_fn(device, c"vkCmdWriteTimestamp")?,
        })
    }
}

/// `vkGetPhysicalDeviceFormatProperties`: `optimalTilingFeatures`, or
/// `None` when it cannot be asked.
fn optimal_features(physical: usize, format: u32) -> Option<u32> {
    let f = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceFormatProperties");
    if f.is_null() || physical == 0 {
        return None;
    }
    // SAFETY: resolved by the host loader for exactly this name.
    let f: extern "C" fn(*mut c_void, u32, *mut [u32; 3]) = unsafe { std::mem::transmute(f) };
    let mut props = [0u32; 3];
    f(physical as *mut c_void, format, &mut props);
    Some(props[1])
}

/// What the timestamps need from the physical device and the queue family:
/// nanoseconds per tick, and the mask of valid bits (0: no timestamps).
fn timestamp_info(physical: usize, family: u32) -> (f64, u64) {
    let props = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceProperties");
    let families = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceQueueFamilyProperties");
    if props.is_null() || families.is_null() || physical == 0 {
        return (0.0, 0);
    }
    // SAFETY: resolved by the host loader for exactly these names.
    let props: extern "C" fn(*mut c_void, *mut u8) = unsafe { std::mem::transmute(props) };
    let families: extern "C" fn(*mut c_void, *mut u32, *mut [u32; 6]) = unsafe { std::mem::transmute(families) };
    // `VkPhysicalDeviceProperties` is 824 bytes; `limits.timestampPeriod` is
    // a float 720 bytes in (printed with the system compiler).
    let mut buffer = [0u64; 128];
    props(physical as *mut c_void, buffer.as_mut_ptr() as *mut u8);
    // SAFETY: the buffer is 1024 initialised bytes.
    let period = unsafe { std::ptr::read_unaligned((buffer.as_ptr() as *const u8).add(720) as *const f32) };
    let mut n = 0u32;
    families(physical as *mut c_void, &mut n, std::ptr::null_mut());
    let mut list = vec![[0u32; 6]; n as usize];
    families(physical as *mut c_void, &mut n, list.as_mut_ptr());
    // `VkQueueFamilyProperties`: queueFlags 0, timestampValidBits 8.
    let bits = list.get(family as usize).map_or(0, |f| f[2]);
    let mask = match bits {
        0 => 0,
        64.. => u64::MAX,
        b => (1u64 << b) - 1,
    };
    (f64::from(period), mask)
}

/// Whether the queue family the engine's frames go to can run compute.
fn family_has_compute(physical: usize, family: u32) -> bool {
    let families = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceQueueFamilyProperties");
    if families.is_null() || physical == 0 {
        return false;
    }
    // SAFETY: resolved by the host loader for exactly this name.
    let families: extern "C" fn(*mut c_void, *mut u32, *mut [u32; 6]) = unsafe { std::mem::transmute(families) };
    let mut n = 0u32;
    families(physical as *mut c_void, &mut n, std::ptr::null_mut());
    let mut list = vec![[0u32; 6]; n as usize];
    families(physical as *mut c_void, &mut n, list.as_mut_ptr());
    list.get(family as usize).is_some_and(|f| f[0] & QUEUE_COMPUTE != 0)
}

/// Milliseconds between two timestamps of `mask` valid bits.
pub(crate) fn ticks_to_ms(start: u64, end: u64, mask: u64, period_ns: f64) -> f64 {
    (end.wrapping_sub(start) & mask) as f64 * period_ns / 1e6
}

// ------------------------------------------------------------- our objects

/// How many frames' timestamps can be in flight.
const RING: usize = 16;
/// Timestamps per frame: before everything, after the depth copy, after
/// each effect of [`EFFECTS`], and after the copy back.
const TIMESTAMPS: usize = 8;

/// How many effects there are.
const N: usize = 5;

/// GPU milliseconds of one frame: each effect's (0 for one that did not
/// run), and everything, the depth copy and the copy back included.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stages {
    pub effects: [f64; N],
    pub total: f64,
}
/// Presents after which a replaced object can no longer be in use by any
/// frame in flight.
const RETIRE_AFTER: u64 = 16;
/// Descriptor sets the pool holds: one per combination of the engine's
/// images and layouts, dropped with the image.
const MAX_SETS: u32 = 64;

/// One image of ours, with its memory and view.
#[derive(Default)]
struct Owned {
    image: u64,
    memory: u64,
    view: u64,
}

/// Our images of the scene's size: the depth copy `depth.comp` writes
/// (R32F), the trace (RGBA16F, written by the trace and sampled by the
/// compose), the result the composes write (R32_UINT, copied to the
/// engine's image), and the post effects' when any of them is on.
struct Output {
    depth: Owned,
    trace: Owned,
    result: Owned,
    post: Option<post::PostImages>,
    extent: [u32; 2],
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct SetKey {
    color: u64,
    color_layout: u32,
    depth: u64,
    depth_layout: u32,
    output: u64,
}

/// What was replaced, kept until no frame in flight can use it.
struct Retired {
    at: u64,
    output: Option<Output>,
    sets: Vec<u64>,
}

/// Everything made on one device.
struct Gpu {
    fns: Fns,
    device: usize,
    memory_types: Vec<u32>,
    period_ns: f64,
    timestamp_mask: u64,
    sampler_linear: u64,
    sampler_nearest: u64,
    set_layout: u64,
    pipeline_layout: u64,
    /// The depth copy of a multisampled depth, of a single-sample one, the
    /// trace and the compose.
    shaders: [u64; 4],
    pipelines: [u64; 4],
    descriptor_pool: u64,
    query_pool: u64,
    /// The depth of field's, the bloom's, the lens's and the sharpen's.
    post: post::PostGpu,
    /// Which effects ran in the frame each timestamp slot holds, while its
    /// timestamps are still to be read.
    pending: [Option<Run>; RING],
    next_slot: usize,
    output: Option<Output>,
    /// The engine's image to our view of it.
    views: HashMap<u64, u64>,
    sets: HashMap<SetKey, u64>,
    retired: Vec<Retired>,
}

impl Gpu {
    fn dev(&self) -> Dev {
        self.device as Dev
    }

    fn destroy_owned(&self, o: &Owned) {
        let (f, d, none) = (&self.fns, self.dev(), std::ptr::null());
        for (what, h) in [(f.destroy_image_view, o.view), (f.destroy_image, o.image), (f.free_memory, o.memory)] {
            if h != 0 {
                what(d, h, none);
            }
        }
    }

    fn destroy_output(&self, o: &Output) {
        self.destroy_owned(&o.depth);
        self.destroy_owned(&o.trace);
        self.destroy_owned(&o.result);
        if let Some(p) = &o.post {
            self.destroy_post_images(p);
        }
    }

    fn free_sets(&self, sets: &[u64]) {
        if !sets.is_empty() {
            (self.fns.free_descriptor_sets)(self.dev(), self.descriptor_pool, sets.len() as u32, sets.as_ptr());
        }
    }

    /// Everything, at device destruction, when the engine has let the
    /// device go idle.
    fn destroy(self) {
        let (f, d, none) = (&self.fns, self.dev(), std::ptr::null());
        for r in &self.retired {
            if let Some(o) = &r.output {
                self.destroy_output(o);
            }
        }
        if let Some(o) = &self.output {
            self.destroy_output(o);
        }
        for &v in self.views.values() {
            (f.destroy_image_view)(d, v, none);
        }
        let destroy = |what: DestroyFn, h: u64| {
            if h != 0 {
                what(d, h, none);
            }
        };
        // Frees every set with it.
        destroy(f.destroy_descriptor_pool, self.descriptor_pool);
        self.destroy_post();
        for p in self.pipelines {
            destroy(f.destroy_pipeline, p);
        }
        for s in self.shaders {
            destroy(f.destroy_shader_module, s);
        }
        destroy(f.destroy_pipeline_layout, self.pipeline_layout);
        destroy(f.destroy_set_layout, self.set_layout);
        destroy(f.destroy_sampler, self.sampler_linear);
        destroy(f.destroy_sampler, self.sampler_nearest);
        destroy(f.destroy_query_pool, self.query_pool);
    }

    /// Our view of an engine image, made the first time.
    fn view_of(&mut self, image: u64, format: u32, aspect: u32) -> Result<u64, String> {
        if let Some(&v) = self.views.get(&image) {
            return Ok(v);
        }
        let info = ImageViewCreateInfo {
            s_type: ST_IMAGE_VIEW_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            image,
            view_type: VIEW_TYPE_2D,
            format,
            components: [0; 4],
            range: range(aspect),
        };
        let mut v = 0;
        let rc = (self.fns.create_image_view)(self.dev(), &info, std::ptr::null(), &mut v);
        if rc != VK_SUCCESS {
            return Err(format!("vkCreateImageView of an engine image failed ({rc})"));
        }
        self.views.insert(image, v);
        Ok(v)
    }

    /// Our images for `extent`, with the post effects' when `post` asks for
    /// them, made again when the size changed or they
    /// are newly asked for. The view of the result, which names them in a
    /// [`SetKey`].
    fn output_for(&mut self, extent: [u32; 2], post: bool, presents: u64) -> Result<u64, String> {
        if let Some(o) = &self.output {
            if o.extent == extent && (o.post.is_some() || !post) {
                return Ok(o.result.view);
            }
        }
        if let Some(old) = self.output.take() {
            let view = old.result.view;
            let sets: Vec<u64> = self.sets.iter().filter(|(k, _)| k.output == view).map(|(_, &s)| s).collect();
            self.sets.retain(|k, _| k.output != view);
            self.retired.push(Retired { at: presents, output: Some(old), sets });
        }
        let mut o =
            Output { depth: Owned::default(), trace: Owned::default(), result: Owned::default(), post: None, extent };
        let made = self
            .make_image(&mut o.depth, extent, FORMAT_R32_SFLOAT, USAGE_STORAGE)
            .and_then(|()| self.make_image(&mut o.trace, extent, FORMAT_R16G16B16A16_SFLOAT, USAGE_STORAGE | USAGE_SAMPLED))
            .and_then(|()| self.make_image(&mut o.result, extent, FORMAT_R32_UINT, USAGE_STORAGE | USAGE_TRANSFER_SRC))
            .and_then(|()| {
                if post {
                    o.post = Some(self.make_post_images(extent)?);
                }
                Ok(())
            });
        if let Err(e) = made {
            self.destroy_output(&o);
            return Err(e);
        }
        let view = o.result.view;
        self.output = Some(o);
        Ok(view)
    }

    /// One single-sample 2D image in device-local memory, and its view, into
    /// `o`; whatever was made stays there for the caller to destroy.
    fn make_image(&self, o: &mut Owned, extent: [u32; 2], format: u32, usage: u32) -> Result<(), String> {
        let (f, d, none) = (&self.fns, self.dev(), std::ptr::null());
        let info = ImageCreateInfo {
            s_type: ST_IMAGE_CREATE_INFO,
            p_next: none,
            flags: 0,
            image_type: IMAGE_TYPE_2D,
            format,
            extent: [extent[0], extent[1], 1],
            mip_levels: 1,
            array_layers: 1,
            samples: 1,
            tiling: TILING_OPTIMAL,
            usage,
            sharing_mode: 0,
            queue_family_index_count: 0,
            p_queue_family_indices: none.cast(),
            initial_layout: LAYOUT_UNDEFINED,
        };
        check((f.create_image)(d, &info, none, &mut o.image), "vkCreateImage")?;
        let mut req = MemoryRequirements::default();
        (f.image_requirements)(d, o.image, &mut req);
        let index = (0..self.memory_types.len())
            .find(|&i| req.memory_type_bits & (1 << i) != 0 && self.memory_types[i] & MEMORY_DEVICE_LOCAL != 0)
            .ok_or("no device-local memory type fits the image")?;
        let alloc = MemoryAllocateInfo {
            s_type: ST_MEMORY_ALLOCATE_INFO,
            p_next: none,
            allocation_size: req.size,
            memory_type_index: index as u32,
        };
        check((f.allocate_memory)(d, &alloc, none, &mut o.memory), "vkAllocateMemory")?;
        check((f.bind_image_memory)(d, o.image, o.memory, 0), "vkBindImageMemory")?;
        let view = ImageViewCreateInfo {
            s_type: ST_IMAGE_VIEW_CREATE_INFO,
            p_next: none,
            flags: 0,
            image: o.image,
            view_type: VIEW_TYPE_2D,
            format,
            components: [0; 4],
            range: range(ASPECT_COLOR),
        };
        check((f.create_image_view)(d, &view, none, &mut o.view), "vkCreateImageView")
    }

    /// The descriptor set naming these images in these layouts, written
    /// once: a set a frame in flight may use is never written again.
    fn set_for(&mut self, key: SetKey) -> Result<u64, String> {
        if let Some(&s) = self.sets.get(&key) {
            return Ok(s);
        }
        let (depth_copy, trace) = match &self.output {
            Some(o) if o.result.view == key.output => (o.depth.view, o.trace.view),
            _ => return Err("the descriptor set names images of ours that are gone".into()),
        };
        let none = std::ptr::null();
        let alloc = DescriptorSetAllocateInfo {
            s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
            p_next: none,
            pool: self.descriptor_pool,
            count: 1,
            layouts: &self.set_layout,
        };
        let mut set = 0;
        let rc = (self.fns.allocate_descriptor_sets)(self.dev(), &alloc, &mut set);
        if rc != VK_SUCCESS {
            return Err(format!("vkAllocateDescriptorSets failed ({rc}); {} sets are in use", self.sets.len()));
        }
        // 0 the engine's colour, 1 its depth, 2 our depth copy, 3 our result,
        // 4 the trace written and 5 the same read filtered.
        let images = [
            DescriptorImageInfo { sampler: self.sampler_linear, view: self.views[&key.color], layout: key.color_layout as i32 },
            DescriptorImageInfo { sampler: self.sampler_nearest, view: self.views[&key.depth], layout: key.depth_layout as i32 },
            DescriptorImageInfo { sampler: 0, view: depth_copy, layout: LAYOUT_GENERAL as i32 },
            DescriptorImageInfo { sampler: 0, view: key.output, layout: LAYOUT_GENERAL as i32 },
            DescriptorImageInfo { sampler: 0, view: trace, layout: LAYOUT_GENERAL as i32 },
            DescriptorImageInfo { sampler: self.sampler_linear, view: trace, layout: LAYOUT_GENERAL as i32 },
        ];
        let write = |binding: u32, kind: u32| WriteDescriptorSet {
            s_type: ST_WRITE_DESCRIPTOR_SET,
            p_next: none,
            set,
            binding,
            element: 0,
            count: 1,
            kind,
            image: &images[binding as usize],
            buffer: none.cast(),
            texel_views: none.cast(),
        };
        let writes = [
            write(0, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
            write(1, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
            write(2, DESCRIPTOR_STORAGE_IMAGE),
            write(3, DESCRIPTOR_STORAGE_IMAGE),
            write(4, DESCRIPTOR_STORAGE_IMAGE),
            write(5, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
        ];
        (self.fns.update_descriptor_sets)(self.dev(), writes.len() as u32, writes.as_ptr(), 0, none);
        self.sets.insert(key, set);
        Ok(set)
    }

    /// The engine destroyed `image`: every command buffer that used it has
    /// completed, ours inside them too, so our view of it and the sets that
    /// name the view can go now.
    fn forget_image(&mut self, image: u64) {
        let Some(view) = self.views.remove(&image) else { return };
        let gone: Vec<u64> =
            self.sets.iter().filter(|(k, _)| k.color == image || k.depth == image).map(|(_, &s)| s).collect();
        self.sets.retain(|k, _| k.color != image && k.depth != image);
        self.free_sets(&gone);
        (self.fns.destroy_image_view)(self.dev(), view, std::ptr::null());
    }

    /// Destroy what was replaced long enough ago.
    fn reap(&mut self, presents: u64) {
        let (old, keep): (Vec<Retired>, Vec<Retired>) =
            std::mem::take(&mut self.retired).into_iter().partition(|r| presents >= r.at + RETIRE_AFTER);
        self.retired = keep;
        for r in old {
            self.free_sets(&r.sets);
            if let Some(o) = &r.output {
                self.destroy_output(o);
            }
        }
    }

    /// The previous use of timestamp slot `slot`, if its frame has finished:
    /// which effects ran in it and the GPU milliseconds of each stage.
    fn collect(&mut self, slot: usize) -> Option<(Run, Stages)> {
        let ran = self.pending[slot].take()?;
        // Each query's value followed by its availability.
        let mut t = [0u64; 2 * TIMESTAMPS];
        let rc = (self.fns.get_query_pool_results)(
            self.dev(),
            self.query_pool,
            (TIMESTAMPS * slot) as u32,
            TIMESTAMPS as u32,
            std::mem::size_of_val(&t),
            t.as_mut_ptr() as *mut c_void,
            16,
            QUERY_RESULT_64 | QUERY_RESULT_WITH_AVAILABILITY,
        );
        if rc != VK_SUCCESS || (0..TIMESTAMPS).any(|i| t[2 * i + 1] == 0) {
            return None;
        }
        let ms = |a: usize, b: usize| ticks_to_ms(t[2 * a], t[2 * b], self.timestamp_mask, self.period_ns);
        Some((ran, Stages { effects: [ms(1, 2), ms(2, 3), ms(3, 4), ms(4, 5), ms(5, 6)], total: ms(0, 7) }))
    }
}

fn check(rc: i32, what: &str) -> Result<(), String> {
    if rc == VK_SUCCESS {
        Ok(())
    } else {
        Err(format!("{what} failed ({rc})"))
    }
}

fn range(aspect: u32) -> ImageSubresourceRange {
    ImageSubresourceRange { aspect_mask: aspect, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 }
}

fn layers(aspect: u32) -> ImageSubresourceLayers {
    ImageSubresourceLayers { aspect_mask: aspect, mip_level: 0, base_array_layer: 0, layer_count: 1 }
}

/// Make everything for `device`, on the render thread at its first matched
/// frame.
fn create_gpu(device: usize) -> Result<Gpu, String> {
    let (current, physical) = frame_capture::current_device().ok_or("no Vulkan device is known")?;
    if current != device {
        return Err("the frame is on another device than the one the client follows".into());
    }
    let family = frame_capture::likely_queue_family().unwrap_or(0);
    if !family_has_compute(physical, family) {
        return Err(format!("queue family {family} cannot run compute"));
    }
    match optimal_features(physical, FORMAT_R32_UINT) {
        Some(f) if f & FEATURE_STORAGE_IMAGE != 0 && f & FEATURE_TRANSFER_SRC != 0 => {}
        _ => return Err("R32_UINT cannot be a storage image and a copy source on this device".into()),
    }
    match optimal_features(physical, FORMAT_R32_SFLOAT) {
        Some(f) if f & FEATURE_STORAGE_IMAGE != 0 => {}
        _ => return Err("R32_SFLOAT cannot be a storage image on this device".into()),
    }
    match optimal_features(physical, FORMAT_R16G16B16A16_SFLOAT) {
        Some(f) if f & FEATURE_STORAGE_IMAGE != 0 && f & FEATURE_SAMPLED_IMAGE != 0 && f & FEATURE_SAMPLED_LINEAR != 0 => {}
        _ => return Err("R16G16B16A16_SFLOAT cannot be a storage and a linearly filtered image on this device".into()),
    }
    let fns = Fns::resolve(device as Dev)?;
    let mp = super::vk_layer::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties");
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
    let (period_ns, timestamp_mask) = timestamp_info(physical, family);
    let mut g = Gpu {
        fns,
        device,
        memory_types: props.memory_types[..(props.memory_type_count as usize).min(32)].iter().map(|t| t[0]).collect(),
        period_ns,
        timestamp_mask,
        sampler_linear: 0,
        sampler_nearest: 0,
        set_layout: 0,
        pipeline_layout: 0,
        shaders: [0; 4],
        pipelines: [0; 4],
        descriptor_pool: 0,
        query_pool: 0,
        post: post::PostGpu::default(),
        pending: [None; RING],
        next_slot: 0,
        output: None,
        views: HashMap::new(),
        sets: HashMap::new(),
        retired: Vec::new(),
    };
    match build(&mut g).and_then(|()| g.build_post()) {
        Ok(()) => Ok(g),
        Err(e) => {
            g.destroy();
            Err(e)
        }
    }
}

fn build(g: &mut Gpu) -> Result<(), String> {
    let f = &g.fns;
    let d = g.device as Dev;
    let none = std::ptr::null();
    for (filter, out) in [(1u32, &mut g.sampler_linear), (0, &mut g.sampler_nearest)] {
        let sampler = SamplerCreateInfo {
            s_type: ST_SAMPLER_CREATE_INFO,
            p_next: none,
            flags: 0,
            mag_filter: filter,
            min_filter: filter,
            mipmap_mode: 0,
            // CLAMP_TO_EDGE.
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
        check((f.create_sampler)(d, &sampler, none, out), "vkCreateSampler")?;
    }
    let binding = |binding: u32, kind: u32| DescriptorSetLayoutBinding {
        binding,
        kind,
        count: 1,
        stages: SHADER_STAGE_COMPUTE,
        immutable_samplers: none.cast(),
    };
    // One layout for all four pipelines; each shader uses the bindings it
    // declares (see `set_for`).
    let bindings = [
        binding(0, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
        binding(1, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
        binding(2, DESCRIPTOR_STORAGE_IMAGE),
        binding(3, DESCRIPTOR_STORAGE_IMAGE),
        binding(4, DESCRIPTOR_STORAGE_IMAGE),
        binding(5, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
    ];
    let layout = DescriptorSetLayoutCreateInfo {
        s_type: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        count: bindings.len() as u32,
        bindings: bindings.as_ptr(),
    };
    check((f.create_set_layout)(d, &layout, none, &mut g.set_layout), "vkCreateDescriptorSetLayout")?;
    let push = PushConstantRange { stages: SHADER_STAGE_COMPUTE, offset: 0, size: std::mem::size_of::<Params>() as u32 };
    let pipeline_layout = PipelineLayoutCreateInfo {
        s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
        p_next: none,
        flags: 0,
        set_layout_count: 1,
        set_layouts: &g.set_layout,
        push_constant_count: 1,
        push_constants: &push,
    };
    check((f.create_pipeline_layout)(d, &pipeline_layout, none, &mut g.pipeline_layout), "vkCreatePipelineLayout")?;
    for (i, code) in [SPIRV_DEPTH_MS, SPIRV_DEPTH_1X, SPIRV_SSR, SPIRV_COMPOSE].into_iter().enumerate() {
        let module = ShaderModuleCreateInfo {
            s_type: ST_SHADER_MODULE_CREATE_INFO,
            p_next: none,
            flags: 0,
            code_size: std::mem::size_of_val(code),
            code: code.as_ptr(),
        };
        check((f.create_shader_module)(d, &module, none, &mut g.shaders[i]), "vkCreateShaderModule")?;
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
            layout: g.pipeline_layout,
            base_pipeline: 0,
            base_pipeline_index: -1,
        };
        check((f.create_compute_pipelines)(d, 0, 1, &pipeline, none, &mut g.pipelines[i]), "vkCreateComputePipelines")?;
    }
    let sizes = [
        DescriptorPoolSize { kind: DESCRIPTOR_COMBINED_IMAGE_SAMPLER, count: 3 * MAX_SETS },
        DescriptorPoolSize { kind: DESCRIPTOR_STORAGE_IMAGE, count: 3 * MAX_SETS },
    ];
    let pool = DescriptorPoolCreateInfo {
        s_type: ST_DESCRIPTOR_POOL_CREATE_INFO,
        p_next: none,
        flags: DESCRIPTOR_POOL_FREE_SET,
        max_sets: MAX_SETS,
        count: 2,
        sizes: sizes.as_ptr(),
    };
    check((f.create_descriptor_pool)(d, &pool, none, &mut g.descriptor_pool), "vkCreateDescriptorPool")?;
    if g.timestamp_mask != 0 {
        let queries = QueryPoolCreateInfo {
            s_type: ST_QUERY_POOL_CREATE_INFO,
            p_next: none,
            flags: 0,
            query_type: QUERY_TYPE_TIMESTAMP,
            query_count: (TIMESTAMPS * RING) as u32,
            pipeline_statistics: 0,
        };
        check((f.create_query_pool)(d, &queries, none, &mut g.query_pool), "vkCreateQueryPool")?;
    }
    Ok(())
}

/// A colour layout the shader may sample in, or the one to move it to.
fn color_read_layout(layout: u32) -> Option<u32> {
    match layout {
        LAYOUT_UNDEFINED | LAYOUT_PREINITIALIZED | LAYOUT_PRESENT_SRC => None,
        LAYOUT_SHADER_READ_ONLY | LAYOUT_GENERAL | LAYOUT_READ_ONLY => Some(layout),
        _ => Some(LAYOUT_SHADER_READ_ONLY),
    }
}

fn depth_read_layout(layout: u32) -> Option<u32> {
    match layout {
        LAYOUT_UNDEFINED | LAYOUT_PREINITIALIZED => None,
        l if depth_sampleable(l) => Some(l),
        _ => Some(LAYOUT_SHADER_READ_ONLY),
    }
}

/// Whether the images can be worked on, before anything is made or
/// recorded; the reason when not.
pub(crate) fn usable(f: &Found) -> Result<u32, String> {
    let (c, z) = (&f.color.att, &f.depth.att);
    let mode = pack_mode(c.format)
        .ok_or_else(|| format!("the scene colour is {}, which the shader cannot write", registry::format_name(c.format)))?;
    if c.usage & (USAGE_SAMPLED | USAGE_TRANSFER_DST) != USAGE_SAMPLED | USAGE_TRANSFER_DST {
        return Err(format!("the scene colour image was created without SAMPLED or TRANSFER_DST (usage 0x{:x})", c.usage));
    }
    if z.usage & USAGE_SAMPLED == 0 {
        return Err(format!("the scene depth image was created without SAMPLED (usage 0x{:x})", z.usage));
    }
    if c.extent != z.extent {
        return Err("the scene colour and depth differ in size".into());
    }
    if c.device != z.device {
        return Err("the scene colour and depth belong to different devices".into());
    }
    if color_read_layout(f.color.layout).is_none() {
        return Err(format!("the scene colour is in layout {}", registry::layout_name(f.color.layout)));
    }
    if depth_read_layout(f.depth.layout).is_none() {
        return Err(format!("the scene depth is in layout {}", registry::layout_name(f.depth.layout)));
    }
    Ok(mode)
}

/// A compute-to-compute barrier: what one dispatch wrote, the next reads.
fn compute_barrier(fns: &Fns, cmd: *mut c_void) {
    let m = MemoryBarrier {
        s_type: ST_MEMORY_BARRIER,
        p_next: std::ptr::null(),
        src_access_mask: ACCESS_SHADER_WRITE,
        dst_access_mask: ACCESS_SHADER_READ | ACCESS_SHADER_WRITE,
    };
    (fns.cmd_pipeline_barrier)(cmd, STAGE_COMPUTE_SHADER, STAGE_COMPUTE_SHADER, 0, 1, &m, 0, std::ptr::null(), 0, std::ptr::null());
}

/// Which effects a frame gets, in the order of [`EFFECTS`].
pub(crate) type Run = [bool; N];

/// Record the work into `cmd`, the engine's command buffer, outside any
/// render pass (see the module doc), with timestamps in `slot`: the depth
/// copy when the reflections, the depth of field or a depth-aware sharpen
/// need it, then each effect of `run` in turn, each reading the scene as the
/// one before left it, and one copy of the result over the engine's image.
/// `chain`: the lens reads the bloom chain, which it records itself when
/// the bloom does not run; `depth_for_sharpen`: the sharpen reads the depth
/// copy.
#[allow(clippy::too_many_arguments)]
fn record(
    g: &mut Gpu,
    cmd: *mut c_void,
    slot: usize,
    f: &Found,
    compute: &Compute,
    run: Run,
    p: &Params,
    post_p: &post::PostParams,
    chain: bool,
    depth_for_sharpen: bool,
    presents: u64,
) -> Result<(), String> {
    let [ssr, dof, bloom, lens, sharpen] = run;
    let (color, depth) = (&f.color, &f.depth);
    let color_read = color_read_layout(color.layout).ok_or("unusable colour layout")?;
    let depth_read = depth_read_layout(depth.layout).ok_or("unusable depth layout")?;
    let output = g.output_for(color.att.extent, dof || bloom || lens || sharpen, presents)?;
    g.view_of(color.att.image, color.att.format, ASPECT_COLOR)?;
    g.view_of(depth.att.image, depth.att.format, ASPECT_DEPTH)?;
    let set = g.set_for(SetKey {
        color: color.att.image,
        color_layout: color_read,
        depth: depth.att.image,
        depth_layout: depth_read,
        output,
    })?;
    let fresh_focus = if dof { post::focus_layout(g) } else { None };
    let o = g.output.as_ref().ok_or("no output image")?;
    let (out_image, copy_image, trace_image) = (o.result.image, o.depth.image, o.trace.image);
    let post_images: Vec<u64> = o.post.iter().flat_map(|p| p.images()).collect();
    let fns = &g.fns;
    let none = std::ptr::null();
    let image_barrier = |image: u64, old: u32, new: u32, src: u32, dst: u32, aspect: u32| ImageMemoryBarrier {
        s_type: ST_IMAGE_MEMORY_BARRIER,
        p_next: none,
        src_access_mask: src,
        dst_access_mask: dst,
        old_layout: old as i32,
        new_layout: new as i32,
        src_queue_family_index: QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: QUEUE_FAMILY_IGNORED,
        image,
        subresource_range: range(aspect),
    };
    let depth_aspects = barrier_aspects(depth.att.format);
    let timed = g.query_pool != 0;
    let stamp = |i: usize| {
        if timed {
            (fns.cmd_write_timestamp)(cmd, STAGE_BOTTOM_OF_PIPE, g.query_pool, (TIMESTAMPS * slot + i) as u32);
        }
    };
    if timed {
        (fns.cmd_reset_query_pool)(cmd, g.query_pool, (TIMESTAMPS * slot) as u32, TIMESTAMPS as u32);
    }
    stamp(0);

    // Everything recorded before (the scene, the resolve and the engine's
    // own barriers) is done writing; our images may be discarded, but for
    // the focus, which is kept from frame to frame.
    let mut before = vec![
        image_barrier(out_image, LAYOUT_UNDEFINED, LAYOUT_GENERAL, 0, ACCESS_SHADER_WRITE, ASPECT_COLOR),
        image_barrier(copy_image, LAYOUT_UNDEFINED, LAYOUT_GENERAL, 0, ACCESS_SHADER_WRITE, ASPECT_COLOR),
        image_barrier(trace_image, LAYOUT_UNDEFINED, LAYOUT_GENERAL, 0, ACCESS_SHADER_WRITE, ASPECT_COLOR),
    ];
    for &image in &post_images {
        before.push(image_barrier(image, LAYOUT_UNDEFINED, LAYOUT_GENERAL, 0, ACCESS_SHADER_WRITE, ASPECT_COLOR));
    }
    if let Some((focus, old)) = fresh_focus {
        before.push(image_barrier(focus, old, LAYOUT_GENERAL, 0, ACCESS_SHADER_READ | ACCESS_SHADER_WRITE, ASPECT_COLOR));
    }
    if color_read != color.layout {
        before.push(image_barrier(color.att.image, color.layout, color_read, ACCESS_MEMORY_WRITE, ACCESS_SHADER_READ, ASPECT_COLOR));
    }
    if depth_read != depth.layout {
        before.push(image_barrier(depth.att.image, depth.layout, depth_read, ACCESS_MEMORY_WRITE, ACCESS_SHADER_READ, depth_aspects));
    }
    let memory = MemoryBarrier {
        s_type: ST_MEMORY_BARRIER,
        p_next: none,
        src_access_mask: ACCESS_MEMORY_WRITE,
        dst_access_mask: ACCESS_SHADER_READ | ACCESS_SHADER_WRITE,
    };
    (fns.cmd_pipeline_barrier)(cmd, STAGE_ALL_COMMANDS, STAGE_COMPUTE_SHADER, 0, 1, &memory, 0, none, before.len() as u32, before.as_ptr());

    // The reflections' set and push constants serve their pipelines, which
    // share the layout.
    let [w, h] = color.att.extent;
    let bind_ssr = |pipeline: u64| {
        (fns.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, pipeline);
        (fns.cmd_bind_descriptor_sets)(cmd, BIND_POINT_COMPUTE, g.pipeline_layout, 0, 1, &set, 0, none.cast());
        (fns.cmd_push_constants)(
            cmd,
            g.pipeline_layout,
            SHADER_STAGE_COMPUTE,
            0,
            std::mem::size_of::<Params>() as u32,
            (p as *const Params).cast(),
        );
    };
    if ssr || dof || (sharpen && depth_for_sharpen) {
        bind_ssr(if depth.att.samples > 1 { g.pipelines[0] } else { g.pipelines[1] });
        (fns.cmd_dispatch)(cmd, w.div_ceil(8), h.div_ceil(8), 1);
        compute_barrier(fns, cmd);
    }
    stamp(1);
    if ssr {
        bind_ssr(g.pipelines[2]);
        (fns.cmd_dispatch)(cmd, w.div_ceil(8), h.div_ceil(8), 1);
        // The trace written, then read by the compose.
        compute_barrier(fns, cmd);
        (fns.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, g.pipelines[3]);
        (fns.cmd_dispatch)(cmd, w.div_ceil(8), h.div_ceil(8), 1);
        compute_barrier(fns, cmd);
    }
    stamp(2);
    let post_images = g.output.as_ref().and_then(|o| o.post.as_ref());
    if dof {
        let mut pp = *post_p;
        if ssr {
            pp.flags |= post::SRC_RESULT;
        }
        if fresh_focus.is_some() {
            pp.flags |= post::FOCUS_RESET;
        }
        post::record_dof(g, cmd, set, post_images.ok_or("no depth of field images")?, &pp);
    }
    stamp(3);
    if bloom {
        let mut pp = *post_p;
        if ssr || dof {
            pp.flags |= post::SRC_RESULT;
        }
        post::record_bloom(g, cmd, set, post_images.ok_or("no bloom images")?, &pp);
    }
    stamp(4);
    if lens {
        let mut pp = *post_p;
        if ssr || dof || bloom {
            pp.flags |= post::SRC_RESULT;
        }
        let images = post_images.ok_or("no lens images")?;
        // The fringe reads the chain; the bloom made it already when it ran.
        if chain && !bloom {
            post::record_bloom_chain(g, cmd, set, images, &pp);
        }
        post::record_lens(g, cmd, set, images, &pp, !sharpen);
    }
    stamp(5);
    if sharpen {
        let mut pp = *post_p;
        if ssr || dof || bloom || lens {
            pp.flags |= post::SRC_RESULT;
        }
        if !depth_for_sharpen {
            pp.flags &= !post::DEPTH_AWARE;
        }
        post::record_sharpen(g, cmd, set, post_images.ok_or("no sharpen images")?, &pp, lens);
    }
    stamp(6);

    // Our image to a copy source, the engine's to a copy destination, the
    // depth back; everything after (the copy, the swapchain pass, the next
    // frame's clear of the depth) waits for the dispatches.
    let mut middle = vec![
        image_barrier(out_image, LAYOUT_GENERAL, LAYOUT_TRANSFER_SRC, ACCESS_SHADER_WRITE, ACCESS_TRANSFER_READ, ASPECT_COLOR),
        image_barrier(color.att.image, color_read, LAYOUT_TRANSFER_DST, 0, ACCESS_TRANSFER_WRITE, ASPECT_COLOR),
    ];
    if depth_read != depth.layout {
        middle.push(image_barrier(depth.att.image, depth_read, depth.layout, 0, 0, depth_aspects));
    }
    (fns.cmd_pipeline_barrier)(cmd, STAGE_COMPUTE_SHADER, STAGE_ALL_COMMANDS, 0, 0, none.cast(), 0, none, middle.len() as u32, middle.as_ptr());
    let region = ImageCopy {
        src_subresource: layers(ASPECT_COLOR),
        src_offset: [0; 3],
        dst_subresource: layers(ASPECT_COLOR),
        dst_offset: [0; 3],
        extent: [w, h, 1],
    };
    (fns.cmd_copy_image)(cmd, out_image, LAYOUT_TRANSFER_SRC as i32, color.att.image, LAYOUT_TRANSFER_DST as i32, 1, &region);
    // The engine's image back in its layout, its new content visible to
    // whatever reads it next.
    let after = [image_barrier(
        color.att.image,
        LAYOUT_TRANSFER_DST,
        color.layout,
        ACCESS_TRANSFER_WRITE,
        ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE,
        ASPECT_COLOR,
    )];
    (fns.cmd_pipeline_barrier)(cmd, STAGE_TRANSFER, STAGE_ALL_COMMANDS, 0, 0, none.cast(), 0, none, 1, after.as_ptr());
    stamp(7);

    // The engine's compute state, as it was.
    if compute.pipeline != 0 {
        (fns.cmd_bind_pipeline)(cmd, BIND_POINT_COMPUTE, compute.pipeline);
    }
    for b in &compute.sets {
        (fns.cmd_bind_descriptor_sets)(
            cmd,
            BIND_POINT_COMPUTE,
            b.layout,
            b.first,
            b.sets.len() as u32,
            b.sets.as_ptr(),
            b.dynamic.len() as u32,
            b.dynamic.as_ptr(),
        );
    }
    for p in &compute.pushes {
        (fns.cmd_push_constants)(cmd, p.layout, p.stages, p.offset, p.bytes.len() as u32, p.bytes.as_ptr().cast());
    }
    if timed {
        g.pending[slot] = Some(run);
    }
    Ok(())
}

// ------------------------------------------------------------- the numbers

/// What `<effect> stats` reports, for one effect or for all of them.
#[derive(Default)]
pub(crate) struct Stats {
    pub applied: u64,
    pub skipped: u64,
    pub last_skip: Option<String>,
    /// GPU milliseconds of the last frames, newest last.
    pub gpu: VecDeque<f64>,
    pub gpu_total: f64,
    pub gpu_count: u64,
    /// Render-thread microseconds spent recording, summed.
    pub cpu_us_total: f64,
}

/// GPU samples kept for the window figures.
const WINDOW: usize = 1024;

impl Stats {
    pub(crate) fn gpu_sample(&mut self, ms: f64) {
        if self.gpu.len() == WINDOW {
            self.gpu.pop_front();
        }
        self.gpu.push_back(ms);
        self.gpu_total += ms;
        self.gpu_count += 1;
    }

    pub(crate) fn summary(&self) -> String {
        let mut sorted: Vec<f64> = self.gpu.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted.get(((sorted.len().max(1) - 1) as f64 * q).round() as usize).copied().unwrap_or(0.0);
        let avg = if sorted.is_empty() { 0.0 } else { sorted.iter().sum::<f64>() / sorted.len() as f64 };
        format!(
            "applied={} skipped={} gpu_ms avg={avg:.3} p50={:.3} p95={:.3} max={:.3} over {} frames, avg_all={:.3} over {}; \
             cpu_us_per_frame={:.1}{}",
            self.applied,
            self.skipped,
            at(0.5),
            at(0.95),
            at(1.0),
            sorted.len(),
            if self.gpu_count == 0 { 0.0 } else { self.gpu_total / self.gpu_count as f64 },
            self.gpu_count,
            if self.applied == 0 { 0.0 } else { self.cpu_us_total / self.applied as f64 },
            self.last_skip.as_ref().map_or(String::new(), |s| format!("; last skipped: {s}")),
        )
    }
}

// ------------------------------------------------------------- the state

#[derive(Default)]
struct State {
    /// Command buffer to its recording so far.
    cmds: HashMap<usize, History>,
    gpus: Vec<Gpu>,
    /// Devices whose objects could not be made, and why.
    failed: Vec<(usize, String)>,
    /// What has been printed once.
    reported: HashSet<String>,
    /// Each effect's, in the order of [`EFFECTS`]; its GPU time is its own
    /// dispatches'.
    stats: [Stats; N],
    /// Every frame that got any effect; its GPU time is everything recorded,
    /// the depth copy and the copy back included.
    chain: Stats,
    presents: u64,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Whether there is anything to look after at presents (retired objects,
/// histories) even with every effect off.
static ACTIVE: AtomicBool = AtomicBool::new(false);

fn state() -> MutexGuard<'static, State> {
    STATE.get_or_init(|| Mutex::new(State::default())).lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    /// Print `line` the first time it comes.
    fn once(&mut self, line: String) {
        if self.reported.insert(line.clone()) {
            println!("[android] vulkan: effects: {line}");
        }
    }

    /// A frame the effects in `on` were asked for was left alone.
    fn skipped(&mut self, on: Run, why: String) {
        for (s, _) in self.stats.iter_mut().zip(on).filter(|(_, on)| *on) {
            s.skipped += 1;
            s.last_skip = Some(why.clone());
        }
        self.chain.skipped += 1;
        self.once(format!("a frame was left alone: {why} (said once)"));
        self.chain.last_skip = Some(why);
    }
}

// ------------------------------------------------------------- hooks

/// `vkBeginCommandBuffer`: a recording starts from nothing.
pub(crate) fn cmd_begun(cmd: *mut c_void) {
    let mut st = state();
    let presents = st.presents;
    st.cmds.insert(cmd as usize, History::new(presents));
    ACTIVE.store(true, Relaxed);
}

/// A pass begins in `pass.cmd`, before the begin is forwarded: the work is
/// recorded here when this is the pass it goes in front of.
pub(crate) fn pass_begun(pass: &Pass) {
    let (desc, full) = {
        let reg = passes::registry();
        (describe(pass, &reg), reg.swapchain_extents())
    };
    let mut st = state();
    let Some(h) = st.cmds.get_mut(&pass.cmd) else { return };
    let Some(outcome) = h.pass(&desc, &full) else { return };
    let compute = h.compute.clone();
    let on: Run = EFFECTS.map(|fx| fx.is_on());
    let found = match outcome {
        Ok(f) => f,
        Err(why) => return st.skipped(on, why),
    };
    let values = SSR.current();
    let post_values = post::PostValues::current();
    let run: Run = [
        on[0] && !changes_nothing(&values),
        on[1] && !post::dof_changes_nothing(&post_values.dof),
        on[2] && !post::bloom_changes_nothing(&post_values.bloom),
        on[3] && !post::lens_changes_nothing(&post_values.lens),
        on[4] && !post::sharpen_changes_nothing(&post_values.sharpen),
    ];
    if run == [false; N] {
        return st.skipped(
            on,
            "every effect that is on changes nothing (reflections: intensity 0 and wet surfaces neither darkened nor \
             tinted; depth of field: strength 0; bloom: intensity 0 or a black tint; lens: no aberration, vignette \
             or halo; sharpen: strength or limit 0)"
                .into(),
        );
    }
    let mode = match usable(&found) {
        Ok(m) => m,
        Err(why) => return st.skipped(on, why),
    };
    let started = Instant::now();
    let device = found.color.att.device;
    let failed = st.failed.iter().find(|(d, _)| *d == device).map(|(_, why)| why.clone());
    if let Some(why) = failed {
        return st.skipped(on, why);
    }
    if !st.gpus.iter().any(|g| g.device == device) {
        match create_gpu(device) {
            Ok(g) => {
                st.once(format!(
                    "pipelines made on device 0x{device:x}{}",
                    if g.timestamp_mask == 0 { "; the queue has no timestamps, so stats have no GPU time" } else { "" }
                ));
                st.gpus.push(g);
            }
            Err(why) => {
                let why = format!("our Vulkan objects could not be made: {why}");
                st.failed.push((device, why.clone()));
                return st.skipped(on, why);
            }
        }
    }
    let presents = st.presents;
    let extent = found.color.att.extent;
    let p = params(extent, &values, mode);
    let g = st.gpus.iter_mut().find(|g| g.device == device).expect("made above");
    let dt = if run[1] {
        let now = Instant::now();
        let dt = g.post.focus_at.map_or(f32::INFINITY, |t| now.duration_since(t).as_secs_f32());
        g.post.focus_at = Some(now);
        dt
    } else {
        0.0
    };
    let post_p = post::post_params(extent, &post_values, mode, dt);
    let chain = post::lens_wants_chain(&post_values.lens);
    let depth_for_sharpen = post::sharpen_wants_depth(&post_values.sharpen);
    // The timestamps this slot held last time round, then the slot again.
    let slot = g.next_slot;
    g.next_slot = (slot + 1) % RING;
    let sample = g.collect(slot);
    let recorded =
        record(g, pass.cmd as *mut c_void, slot, &found, &compute, run, &p, &post_p, chain, depth_for_sharpen, presents);
    if let Some((ran, ms)) = sample {
        for ((s, ran), ms) in st.stats.iter_mut().zip(ran).zip(ms.effects) {
            if ran {
                s.gpu_sample(ms);
            }
        }
        st.chain.gpu_sample(ms.total);
    }
    match recorded {
        Ok(()) => {
            let cpu = started.elapsed().as_secs_f64() * 1e6;
            for (s, _) in st.stats.iter_mut().zip(run).filter(|(_, ran)| *ran) {
                s.applied += 1;
                s.cpu_us_total += cpu;
            }
            st.chain.applied += 1;
            st.chain.cpu_us_total += cpu;
            // An effect that is on but changes nothing was skipped.
            for (s, _) in st.stats.iter_mut().zip(on.iter().zip(run)).filter(|(_, (on, ran))| **on && !ran) {
                s.skipped += 1;
                s.last_skip = Some("it is on but changes nothing".into());
            }
            let (c, z) = (&found.color, &found.depth);
            let names: Vec<&str> = EFFECTS.iter().zip(run).filter(|(_, ran)| *ran).map(|(fx, _)| fx.name).collect();
            st.once(format!(
                "{} recorded before the swapchain pass: scene depth {} x{} {}x{} in {}, colour {} {}x{} in {}{}",
                names.join(" "),
                registry::format_name(z.att.format),
                z.att.samples,
                z.att.extent[0],
                z.att.extent[1],
                registry::layout_name(z.layout),
                registry::format_name(c.att.format),
                c.att.extent[0],
                c.att.extent[1],
                registry::layout_name(c.layout),
                if found.resolved { ", resolved from the multisampled scene" } else { ", the single-sample scene's own" },
            ));
        }
        Err(why) => st.skipped(on, format!("recording failed: {why}")),
    }
}

/// A resolve from `src` into `dst` (in `dst_layout`) was recorded in `cmd`.
pub(crate) fn resolved(cmd: *mut c_void, src: u64, dst: u64, dst_layout: u32) {
    let described = passes::registry().images.get(&dst).map(|i| att_of(dst, i, LOAD_OP_LOAD, dst_layout));
    if let Some(h) = state().cmds.get_mut(&(cmd as usize)) {
        h.resolve(src, described);
    }
}

/// Image barriers recorded in `cmd`: (image, new layout).
pub(crate) fn barriers(cmd: *mut c_void, list: impl Iterator<Item = (u64, u32)>) {
    if let Some(h) = state().cmds.get_mut(&(cmd as usize)) {
        for (image, layout) in list {
            h.barrier(image, layout);
        }
    }
}

/// A copy, blit or clear wrote `image` in `cmd`.
pub(crate) fn written(cmd: *mut c_void, image: u64) {
    if let Some(h) = state().cmds.get_mut(&(cmd as usize)) {
        h.written(image);
    }
}

/// The engine's compute state in `cmd` changed.
pub(crate) fn compute(cmd: *mut c_void, change: impl FnOnce(&mut Compute)) {
    if let Some(h) = state().cmds.get_mut(&(cmd as usize)) {
        change(&mut h.compute);
    }
}

/// From `vkQueuePresentKHR`: replaced objects whose frames are done go, and
/// recordings nobody continued for a long time are forgotten.
pub(crate) fn frame_end(presents: u64) {
    if !ACTIVE.load(Relaxed) {
        return;
    }
    let mut st = state();
    st.presents = presents;
    for g in st.gpus.iter_mut() {
        if !g.retired.is_empty() {
            g.reap(presents);
        }
    }
    if presents % 256 == 0 {
        st.cmds.retain(|_, h| presents.saturating_sub(h.begun) < 1024);
    }
}

/// The engine destroys `image`.
pub(crate) fn image_destroyed(image: u64) {
    if !ACTIVE.load(Relaxed) {
        return;
    }
    let mut st = state();
    for g in st.gpus.iter_mut() {
        g.forget_image(image);
    }
}

/// Before `vkDestroyDevice` is forwarded: our objects on it go first.
pub(crate) fn device_destroying(device: *mut c_void) {
    if !ACTIVE.load(Relaxed) {
        return;
    }
    let mut st = state();
    let (gone, keep): (Vec<Gpu>, Vec<Gpu>) = std::mem::take(&mut st.gpus).into_iter().partition(|g| g.device == device as usize);
    st.gpus = keep;
    for g in gone {
        g.destroy();
    }
    st.failed.retain(|(d, _)| *d != device as usize);
    st.cmds.clear();
}

// ------------------------------------------------------------- usage

/// How many images have had usage added, for the log.
static WIDENED: AtomicU32 = AtomicU32::new(0);

/// The usage a candidate for the scene colour needs and lacks, given its
/// create info's fields, or 0. A candidate is a single-sample 2D colour
/// attachment with one mip and one layer, of the swapchain's size, in a
/// format the shader can write.
#[allow(clippy::too_many_arguments)]
pub(crate) fn missing_usage(image_type: u32, format: u32, extent: [u32; 3], mips: u32, layers: u32, samples: u32, usage: u32, full: &[[u32; 2]]) -> u32 {
    let candidate = image_type == IMAGE_TYPE_2D
        && samples == 1
        && mips == 1
        && layers == 1
        && extent[2] == 1
        && usage & USAGE_COLOR_ATTACHMENT != 0
        && pack_mode(format).is_some()
        && full.contains(&[extent[0], extent[1]]);
    if candidate {
        (USAGE_SAMPLED | USAGE_TRANSFER_DST) & !usage
    } else {
        0
    }
}

/// From `vkCreateImage`, with SSR on: the engine's create info with
/// `SAMPLED` and `TRANSFER_DST` added when it is a candidate for the scene
/// colour that lacks them and its format supports them, or `None` to pass
/// the engine's own through unchanged.
pub(crate) fn widen_usage(device: *mut c_void, info: *const c_void) -> Option<[u64; 11]> {
    if !EFFECTS.iter().any(|fx| fx.is_on()) || info.is_null() {
        return None;
    }
    // SAFETY: the engine's 88-byte `VkImageCreateInfo`, read during its own
    // call: imageType 20, format 24, extent 28, mipLevels 40, arrayLayers 44,
    // samples 48, tiling 52, usage 56.
    let (image_type, format, extent, mips, layers, samples, tiling, usage): (u32, u32, [u32; 3], u32, u32, u32, u32, u32) = unsafe {
        use registry::get;
        (get(info, 20), get(info, 24), get(info, 28), get(info, 40), get(info, 44), get(info, 48), get(info, 52), get(info, 56))
    };
    if tiling != TILING_OPTIMAL {
        return None;
    }
    let full = passes::registry().swapchain_extents();
    let missing = missing_usage(image_type, format, extent, mips, layers, samples, usage, &full);
    if missing == 0 {
        return None;
    }
    let (current, physical) = frame_capture::current_device()?;
    if current != device as usize {
        return None;
    }
    let features = optimal_features(physical, format)?;
    let needs = (if missing & USAGE_SAMPLED != 0 { FEATURE_SAMPLED_IMAGE } else { 0 })
        | (if missing & USAGE_TRANSFER_DST != 0 { FEATURE_TRANSFER_DST } else { 0 });
    if features & needs != needs {
        return None;
    }
    // SAFETY: as above; the whole structure is copied and only `usage`
    // changes, so every pointer in it stays the engine's.
    let mut copy: [u64; 11] = unsafe { registry::get(info, 0) };
    // SAFETY: offset 56 of the 88-byte copy.
    unsafe { std::ptr::write_unaligned((copy.as_mut_ptr() as *mut u8).add(56) as *mut u32, usage | missing) };
    if WIDENED.fetch_add(1, Relaxed) < 8 {
        println!(
            "[android] vulkan: ssr: image {} {}x{} created with usage 0x{usage:x} | 0x{missing:x}, so the reflections \
             can read and write it",
            registry::format_name(format),
            extent[0],
            extent[1]
        );
    }
    Some(copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry::{Attachment, Framebuffer, RenderPass, Subpass, View};

    const FULL: [u32; 2] = [1667, 1349];
    const SCENE_DEPTH: u64 = 0xd0;
    const SCENE_COLOR: u64 = 0xc4;
    const RESOLVED: u64 = 0xc1;
    const SWAPCHAIN: u64 = 0x5c;
    const SRO: u32 = 5;

    fn att(image: u64, format: u32, samples: u32, extent: [u32; 2], load: u32, layout: u32) -> Att {
        Att { image, device: 1, format, samples, extent, usage: 0x17, swapchain: image == SWAPCHAIN, load, layout }
    }

    fn scene(load: u32, samples: u32) -> PassDesc {
        PassDesc {
            has_depth: true,
            depth: Some(Att { usage: 0x27, ..att(SCENE_DEPTH, 126, samples, FULL, load, SRO) }),
            colors: vec![att(SCENE_COLOR, 64, samples, FULL, load, 2)],
            resolves: vec![],
        }
    }

    fn color_pass(image: u64, extent: [u32; 2]) -> PassDesc {
        PassDesc { has_depth: false, depth: None, colors: vec![att(image, 77, 1, extent, 2, SRO)], resolves: vec![] }
    }

    fn swapchain_pass() -> PassDesc {
        PassDesc { has_depth: false, depth: None, colors: vec![att(SWAPCHAIN, 44, 1, FULL, 2, 2)], resolves: vec![] }
    }

    fn resolved_att() -> Option<Att> {
        Some(att(RESOLVED, 64, 1, FULL, LOAD_OP_LOAD, 7))
    }

    /// The frame traced on Natural Disaster Survival at quality 21, MSAA 4.
    fn traced(h: &mut History) -> Option<Result<Found, String>> {
        let small = |load| PassDesc {
            has_depth: true,
            depth: Some(att(0x24, 129, 1, [256, 256], load, 3)),
            colors: vec![att(0x16, 16, 1, [256, 256], load, SRO)],
            resolves: vec![],
        };
        for p in [small(1), small(0), small(0)] {
            assert!(h.pass(&p, &[FULL]).is_none());
        }
        assert!(h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]).is_none());
        for _ in 0..5 {
            assert!(h.pass(&color_pass(0x77, [834, 675]), &[FULL]).is_none());
        }
        assert!(h.pass(&scene(LOAD_OP_LOAD, 4), &[FULL]).is_none());
        h.barrier(SCENE_COLOR, 6);
        h.barrier(RESOLVED, 7);
        h.resolve(SCENE_COLOR, resolved_att());
        h.barrier(SCENE_COLOR, 2);
        h.barrier(RESOLVED, SRO);
        h.pass(&swapchain_pass(), &[FULL])
    }

    #[test]
    fn the_traced_frame_matches_before_its_swapchain_pass() {
        let mut h = History::new(0);
        let f = traced(&mut h).expect("the swapchain pass is evaluated").expect("and matches");
        assert_eq!((f.depth.att.image, f.depth.att.samples, f.depth.layout), (SCENE_DEPTH, 4, SRO));
        assert_eq!((f.color.att.image, f.color.att.samples, f.color.layout, f.resolved), (RESOLVED, 1, SRO, true));
        assert!(h.done);
        assert!(h.pass(&swapchain_pass(), &[FULL]).is_none(), "once per recording");
        assert_eq!(usable(&f), Ok(0));
    }

    #[test]
    fn a_single_sample_scene_uses_its_own_colour() {
        let mut h = History::new(0);
        h.pass(&scene(LOAD_OP_CLEAR, 1), &[FULL]);
        h.pass(&color_pass(0x77, FULL), &[FULL]);
        h.pass(&scene(LOAD_OP_LOAD, 1), &[FULL]);
        h.barrier(SCENE_COLOR, SRO);
        let f = h.pass(&swapchain_pass(), &[FULL]).unwrap().unwrap();
        assert_eq!((f.color.att.image, f.color.layout, f.resolved), (SCENE_COLOR, SRO, false));
    }

    #[test]
    fn frames_without_the_shape_are_left_alone_with_a_reason() {
        let why = |steps: &dyn Fn(&mut History)| {
            let mut h = History::new(0);
            steps(&mut h);
            let r = h.pass(&swapchain_pass(), &[FULL]).expect("a swapchain pass is evaluated");
            assert!(!h.done);
            r.expect_err("no match")
        };
        // A loading screen: nothing but the swapchain.
        assert!(why(&|_| {}).starts_with("no pass cleared a depth attachment"));
        // The scene at another size than the swapchain.
        assert!(why(&|h| {
            let mut s = scene(LOAD_OP_CLEAR, 4);
            s.depth.as_mut().unwrap().extent = [833, 674];
            h.pass(&s, &[FULL]);
        })
        .starts_with("no pass cleared"));
        // No transparent pass.
        assert!(why(&|h| {
            h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
            h.resolve(SCENE_COLOR, resolved_att());
        })
        .contains("no later pass loaded it"));
        // Multisampled and never resolved.
        assert!(why(&|h| {
            h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
            h.pass(&scene(LOAD_OP_LOAD, 4), &[FULL]);
        })
        .contains("not resolved"));
        // A resolve of something else, or into an image of another size.
        assert!(why(&|h| {
            h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
            h.pass(&scene(LOAD_OP_LOAD, 4), &[FULL]);
            h.resolve(0x999, resolved_att());
            h.resolve(SCENE_COLOR, Some(att(RESOLVED, 64, 1, [833, 674], 0, 7)));
        })
        .contains("not resolved"));
        // Resolved, then the scene drawn again: the resolve is stale.
        assert!(why(&|h| {
            h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
            h.resolve(SCENE_COLOR, resolved_att());
            h.pass(&scene(LOAD_OP_LOAD, 4), &[FULL]);
        })
        .contains("not resolved"));
        // The depth overwritten after the scene.
        assert!(why(&|h| {
            h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
            h.pass(&scene(LOAD_OP_LOAD, 4), &[FULL]);
            h.resolve(SCENE_COLOR, resolved_att());
            h.written(SCENE_DEPTH);
        })
        .contains("overwritten"));
        // Compute state that cannot be put back.
        assert!(why(&|h| {
            h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
            h.pass(&scene(LOAD_OP_LOAD, 4), &[FULL]);
            h.resolve(SCENE_COLOR, resolved_att());
            h.compute.push(Push { layout: 1, stages: SHADER_STAGE_COMPUTE | 0x1, offset: 0, bytes: vec![0; 4] });
        })
        .contains("compute state could not be put back"));
    }

    #[test]
    fn a_swapchain_pass_before_the_scene_does_not_end_the_match() {
        let mut h = History::new(0);
        assert!(h.pass(&swapchain_pass(), &[FULL]).unwrap().is_err());
        assert!(traced(&mut h).unwrap().is_ok());
        // A pass with a depth attachment that draws to the swapchain is not
        // the composite.
        let mut h = History::new(0);
        h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
        h.pass(&scene(LOAD_OP_LOAD, 4), &[FULL]);
        h.resolve(SCENE_COLOR, resolved_att());
        let mut p = swapchain_pass();
        p.has_depth = true;
        assert!(h.pass(&p, &[FULL]).is_none());
    }

    #[test]
    fn a_render_pass_resolve_attachment_is_the_resolved_colour() {
        let mut h = History::new(0);
        h.pass(&scene(LOAD_OP_CLEAR, 4), &[FULL]);
        let mut s = scene(LOAD_OP_LOAD, 4);
        s.resolves.push(att(RESOLVED, 64, 1, FULL, 2, SRO));
        h.pass(&s, &[FULL]);
        let f = h.pass(&swapchain_pass(), &[FULL]).unwrap().unwrap();
        assert_eq!((f.color.att.image, f.color.layout), (RESOLVED, SRO));
    }

    #[test]
    fn compute_binds_are_kept_to_be_put_back() {
        let mut c = Compute::default();
        c.bind_pipeline(7);
        c.bind_sets(SetBind { layout: 1, first: 0, sets: vec![10, 11], dynamic: vec![] });
        c.bind_sets(SetBind { layout: 1, first: 1, sets: vec![12], dynamic: vec![64] });
        c.bind_sets(SetBind { layout: 2, first: 0, sets: vec![13, 14, 15], dynamic: vec![] });
        assert_eq!(c.sets.len(), 1, "a bind of sets 0..3 covers the earlier ones");
        c.push(Push { layout: 2, stages: SHADER_STAGE_COMPUTE, offset: 0, bytes: vec![1; 16] });
        c.push(Push { layout: 2, stages: SHADER_STAGE_COMPUTE, offset: 0, bytes: vec![2; 8] });
        c.push(Push { layout: 2, stages: SHADER_STAGE_COMPUTE, offset: 0, bytes: vec![3; 16] });
        assert_eq!(c.pushes.len(), 1);
        assert_eq!(c.pushes[0].bytes, vec![3; 16]);
        assert_eq!((c.pipeline, c.unrestorable), (7, None));
        for i in 0..MAX_REPLAY as u32 + 1 {
            c.bind_sets(SetBind { layout: 3, first: 10 + i, sets: vec![1], dynamic: vec![] });
        }
        assert!(c.unrestorable.is_some());
    }

    #[test]
    fn passes_are_described_through_the_registry() {
        let mut reg = Registry::default();
        let a = |format, samples, load, final_layout| Attachment {
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
                attachments: vec![a(64, 4, 1, 2), a(126, 4, 1, SRO), a(64, 1, 2, SRO)],
                subpasses: vec![Subpass {
                    color: vec![registry::Ref { attachment: 0, layout: 2 }],
                    resolve: vec![registry::Ref { attachment: 2, layout: 2 }],
                    depth: Some(registry::Ref { attachment: 1, layout: 3 }),
                    ..Subpass::default()
                }],
                dependencies: vec![],
            },
        );
        reg.framebuffers.insert(
            0x60,
            Framebuffer { device: 1, render_pass: 0x50, width: 8, height: 8, layers: 1, views: vec![0x70, 0x71, 0x72], imageless: false },
        );
        let img = |format, samples, usage| registry::Image {
            device: 1,
            flags: 0,
            format,
            extent: [8, 8, 1],
            mips: 1,
            layers: 1,
            samples,
            usage,
            swapchain: false,
        };
        reg.images.insert(0x80, img(64, 4, 0x13));
        reg.images.insert(0x81, img(126, 4, 0x27));
        reg.images.insert(0x82, img(64, 1, 0x17));
        let view = |image| View { device: 1, image, view_type: 1, format: 0, aspect: 0, base_mip: 0, levels: 1, base_layer: 0, layers: 1 };
        reg.views.insert(0x70, view(0x80));
        reg.views.insert(0x71, view(0x81));
        reg.views.insert(0x72, view(0x82));
        let pass = Pass::new(9, Target::RenderPass { render_pass: 0x50, framebuffer: 0x60, views: vec![], contents: 0, version: 1 }, (0, 0, 8, 8), vec![]);
        let d = describe(&pass, &reg);
        assert!(d.has_depth);
        let z = d.depth.unwrap();
        assert_eq!((z.image, z.load, z.layout, z.usage, z.samples), (0x81, 1, SRO, 0x27, 4));
        assert_eq!(d.colors.iter().map(|c| (c.image, c.samples)).collect::<Vec<_>>(), vec![(0x80, 4)]);
        assert_eq!(d.resolves.iter().map(|c| (c.image, c.layout)).collect::<Vec<_>>(), vec![(0x82, SRO)]);

        let dynamic = Pass::new(
            9,
            Target::Dynamic {
                flags: 0,
                layers: 1,
                colors: vec![passes::DynAttachment { view: 0x70, layout: 2, resolve_view: 0x72, resolve_layout: 2, ..Default::default() }],
                depth: Some(passes::DynAttachment { view: 0x71, layout: 3, load: 0, ..Default::default() }),
                stencil: None,
            },
            (0, 0, 8, 8),
            vec![],
        );
        let d = describe(&dynamic, &reg);
        assert_eq!(d.depth.map(|z| (z.image, z.load, z.layout)), Some((0x81, 0, 3)));
        assert_eq!((d.colors[0].image, d.resolves[0].image, d.resolves[0].layout), (0x80, 0x82, 2));
    }

    #[test]
    fn settings_are_read_with_defaults_and_warnings() {
        let defaults: Vec<[f32; 3]> = KNOBS.iter().map(|k| k.default).collect();
        let s = parse_settings(|_| None);
        assert_eq!((s.on, &s.values, s.warnings.len()), (false, &defaults, 0));

        let env = |name: &str| {
            Some(match name {
                "INTOXICATED_SSR" => "1",
                "INTOXICATED_SSR_FOV" => "90",
                "INTOXICATED_SSR_UP_ONLY" => "0",
                "INTOXICATED_SSR_INTENSITY" => "0.25",
                "INTOXICATED_SSR_TINT" => "0.9, 0.95,1",
                "INTOXICATED_SSR_STEPS" => "16",
                _ => return None,
            }
            .to_string())
        };
        let s = parse_settings(env);
        assert!(s.on && s.warnings.is_empty(), "{:?}", s.warnings);
        let p = params(FULL, &s.values, 0);
        assert!((p.tan_half[1] - (45f32).to_radians().tan()).abs() < 1e-6);
        assert_eq!((p.flags, p.intensity, p.steps, p.tint), (0, 0.25, 16, [0.9, 0.95, 1.0, 1.0]));

        let bad = |name: &str| {
            Some(match name {
                "INTOXICATED_SSR" => "yes",
                "INTOXICATED_SSR_FOV" => "400",
                "INTOXICATED_SSR_UP_ONLY" => "maybe",
                "INTOXICATED_SSR_INTENSITY" => "2",
                "INTOXICATED_SSR_TINT" => "1,2",
                "INTOXICATED_SSR_STEPS" => "7.5",
                _ => return None,
            }
            .to_string())
        };
        let s = parse_settings(bad);
        assert_eq!((s.on, &s.values), (false, &defaults));
        assert_eq!(s.warnings.len(), 6, "{:?}", s.warnings);
        assert!(s.warnings[0].contains("INTOXICATED_SSR=\"yes\""));
    }

    #[test]
    fn every_knob_has_its_own_slots_and_variable() {
        let mut used = [false; SLOTS];
        for k in &KNOBS {
            for slot in &mut used[k.slot..k.slot + k.width()] {
                assert!(!*slot, "{} shares a slot", k.name);
                *slot = true;
            }
            assert_eq!(k.env, format!("INTOXICATED_SSR_{}", k.name.to_uppercase()));
            assert_eq!(k.parse(&k.show(k.default)), Ok(k.default), "{} reads its own default back", k.name);
        }
        assert!(used.iter().all(|&u| u), "no slot left over");
        for name in
            ["intensity", "roughness", "fresnel", "up_only", "up_threshold", "max_distance", "edge_fade", "darken", "tint", "keep_colour"]
        {
            assert!(find(name).is_some(), "{name}");
            assert!(USAGE.contains(name), "{name}");
        }
    }

    #[test]
    fn devctl_commands_are_parsed() {
        assert_eq!(parse_command(&["on"]), Ok(Command::On));
        assert_eq!(parse_command(&["off"]), Ok(Command::Off));
        assert_eq!(parse_command(&["intensity", "0.3"]), Ok(Command::Set("intensity", [0.3; 3])));
        assert!(parse_command(&["intensity", "1.5"]).is_err());
        assert!(parse_command(&["intensity"]).is_err());
        assert_eq!(parse_command(&["set", "fov", "80"]), Ok(Command::Set("fov", [80.0; 3])));
        assert_eq!(parse_command(&["set", "up_only", "0"]), Ok(Command::Set("up_only", [0.0; 3])));
        assert_eq!(parse_command(&["set", "roughness", "0.3"]), Ok(Command::Set("roughness", [0.3; 3])));
        assert_eq!(parse_command(&["set", "tint", "0.5"]), Ok(Command::Set("tint", [0.5; 3])));
        assert_eq!(parse_command(&["set", "tint", "1,0.9,0.8"]), Ok(Command::Set("tint", [1.0, 0.9, 0.8])));
        assert_eq!(parse_command(&["set", "steps", "24"]), Ok(Command::Set("steps", [24.0; 3])));
        assert!(parse_command(&["set", "steps", "100"]).unwrap_err().contains("whole number from 4 to 64"));
        assert!(parse_command(&["set", "up_only", "2"]).is_err());
        assert!(parse_command(&["set", "shininess", "1"]).unwrap_err().contains("no knob \"shininess\""));
        assert_eq!(parse_command(&[]), Ok(Command::Stats));
        assert_eq!(parse_command(&["stats"]), Ok(Command::Stats));
        assert_eq!(parse_command(&["get"]), Ok(Command::Get));
        assert_eq!(parse_command(&["reset"]), Ok(Command::Reset));
        assert_eq!(parse_command(&["look", "wet_road"]), Ok(Command::Look("wet_road")));
        assert!(parse_command(&["look", "shiny"]).unwrap_err().contains("no look \"shiny\"; looks: competitive wet_road"));
        assert!(parse_command(&["look"]).is_err());
        assert_eq!(parse_command(&["bogus"]), Err(USAGE.to_string()));
    }

    #[test]
    fn looks_set_every_look_knob_and_leave_the_rest() {
        let at = |name: &str| KNOBS.iter().position(|k| k.name == name).unwrap();
        for look in &LOOKS {
            assert!(USAGE.contains(look.name), "{}", look.name);
            let values = look_values(look.name).unwrap();
            for (name, v) in look.values {
                let k = find(name).unwrap_or_else(|| panic!("{} names no knob {name}", look.name));
                assert_eq!(k.parse(&k.show(*v)), Ok(*v), "{} sets {name} out of range", look.name);
                assert_eq!(values[at(name)], Some(*v));
            }
            for k in &KNOBS {
                let named = look.values.iter().any(|(n, _)| *n == k.name);
                match values[at(k.name)] {
                    None => assert!(NOT_A_LOOK.contains(&k.name) && !named, "{} {}", look.name, k.name),
                    Some(v) if !named => assert_eq!(v, k.default, "{} {}", look.name, k.name),
                    Some(_) => {}
                }
            }
        }
        // The default is the first look, and the competitive one.
        assert_eq!(LOOKS[0].name, "competitive");
        let defaults: Vec<Option<[f32; 3]>> =
            KNOBS.iter().map(|k| (!NOT_A_LOOK.contains(&k.name)).then_some(k.default)).collect();
        assert_eq!(look_values("competitive").unwrap(), defaults);
        assert!(look_values("shiny").is_none());

        // From the environment: the look, then the knobs' own variables.
        let env = |name: &str| {
            Some(match name {
                "INTOXICATED_SSR_LOOK" => "clean_mirror",
                "INTOXICATED_SSR_FRESNEL" => "0.5",
                "INTOXICATED_SSR_STEPS" => "20",
                _ => return None,
            }
            .to_string())
        };
        let s = parse_settings(env);
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let p = params(FULL, &s.values, 0);
        assert_eq!((p.intensity, p.roughness, p.fresnel, p.steps, p.keep_colour), (0.9, 0.0, 0.5, 20, 0.0));
        assert_eq!((p.edge_fade, p.thickness), (EDGE_FADE, 0.06));
        let s = parse_settings(|name| (name == LOOK_ENV).then(|| "shiny".to_string()));
        assert_eq!(s.values, KNOBS.iter().map(|k| k.default).collect::<Vec<_>>());
        assert!(s.warnings[0].contains("is not a look (competitive wet_road clean_mirror puddles glossy)"), "{:?}", s.warnings);
    }

    #[test]
    fn the_looks_json_is_the_tables_and_what_the_client_starts_with() {
        let text = looks_json();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["switch"], ENV);
        assert_eq!(doc["look_env"], LOOK_ENV);
        assert_eq!(doc["default_look"], LOOKS[0].name);

        let knobs = doc["knobs"].as_array().unwrap();
        assert_eq!(knobs.len(), KNOBS.len());
        // A JSON value read back as the text a variable would hold.
        let as_text = |v: &serde_json::Value| match v {
            serde_json::Value::Array(parts) => parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(","),
            other => other.to_string(),
        };
        for (k, j) in KNOBS.iter().zip(knobs) {
            assert_eq!(j["name"], k.name);
            assert_eq!(j["env"], k.env);
            let (kind, min, max) = match k.kind {
                Kind::Number { min, max } => ("number", min, max),
                Kind::Whole { min, max } => ("whole", min as f32, max as f32),
                Kind::Switch => ("switch", 0.0, 1.0),
                Kind::Rgb { min, max } => ("rgb", min, max),
            };
            assert_eq!(j["kind"], kind, "{}", k.name);
            assert_eq!(j["min"].as_f64().unwrap() as f32, min, "{}", k.name);
            assert_eq!(j["max"].as_f64().unwrap() as f32, max, "{}", k.name);
            assert_eq!(k.parse(&as_text(&j["default"])), Ok(k.default), "{}", k.name);
        }

        let looks = doc["looks"].as_array().unwrap();
        assert_eq!(looks.len(), LOOKS.len());
        for (l, j) in LOOKS.iter().zip(looks) {
            assert_eq!(j["name"], l.name);
            assert!(!l.description.is_empty() && j["description"] == l.description, "{}", l.name);
            assert!(l.description.is_ascii(), "{}: no dashes or symbols outside ASCII", l.name);
            // Every knob, at the value the client starts it with for this look.
            let started = parse_settings(|name| (name == LOOK_ENV).then(|| l.name.to_string()));
            assert!(started.warnings.is_empty(), "{:?}", started.warnings);
            let values = j["values"].as_object().unwrap();
            assert_eq!(values.len(), KNOBS.len(), "{}", l.name);
            for (k, v) in KNOBS.iter().zip(&started.values) {
                assert_eq!(k.parse(&as_text(&values[k.name])), Ok(*v), "{} {}", l.name, k.name);
            }
        }
        // The default look starts every knob at its default.
        let first = looks[0]["values"].as_object().unwrap();
        for k in &KNOBS {
            assert_eq!(k.parse(&as_text(&first[k.name])), Ok(k.default), "{}", k.name);
        }

        // Numbers as short as the client writes them, not widened f32s.
        assert!(text.contains("\"intensity\": 0.4,"), "{text}");
        assert!(text.contains("0.92,") && !text.contains("0.9200000"), "{text}");
        assert!(text.contains("\"steps\": 32"), "{text}");
        assert!(text.is_ascii());
    }

    #[test]
    fn every_effect_has_its_own_variables_slots_looks_and_verbs() {
        let names: Vec<&str> = EFFECTS.iter().map(|fx| fx.name).collect();
        assert_eq!(names, ["ssr", "dof", "bloom", "lens", "sharpen"], "recorded in this order");
        for (i, fx) in EFFECTS.iter().enumerate() {
            assert_eq!(fx.index, i, "{}", fx.name);
            assert_eq!(fx.env, format!("INTOXICATED_{}", fx.name.to_uppercase()));
            assert_eq!(fx.look_env, format!("{}_LOOK", fx.env));
            assert!(fx.usage.starts_with(&format!("{} on|off|stats|get|reset|look <name>|", fx.name)), "{}", fx.usage);
            assert!(fx.title.is_ascii() && fx.summary.is_ascii() && !fx.summary.is_empty(), "{}", fx.name);
            assert!(std::ptr::eq(effect(fx.name).unwrap(), *fx));
            let mut used = vec![false; fx.values.len()];
            for k in fx.knobs {
                for slot in &mut used[k.slot..k.slot + k.width()] {
                    assert!(!*slot, "{} {} shares a slot", fx.name, k.name);
                    *slot = true;
                }
                assert_eq!(k.env, format!("{}_{}", fx.env, k.name.to_uppercase()));
                assert_eq!(k.parse(&k.show(k.default)), Ok(k.default), "{} {} reads its own default back", fx.name, k.name);
                assert!(fx.usage.contains(k.name), "{} {}", fx.name, k.name);
            }
            assert!(used.iter().all(|&u| u), "{}: no slot left over", fx.name);
            for name in fx.not_a_look {
                assert!(fx.find(name).is_some(), "{} {name}", fx.name);
            }
            for look in fx.looks {
                assert!(fx.usage.contains(look.name), "{} {}", fx.name, look.name);
                assert!(look.description.is_ascii() && !look.description.is_empty());
                for (name, v) in look.values {
                    let k = fx.find(name).unwrap_or_else(|| panic!("{} names no knob {name}", look.name));
                    assert!(!fx.not_a_look.contains(name), "{} sets {name}, which no look sets", look.name);
                    assert_eq!(k.parse(&k.show(*v)), Ok(*v), "{} sets {name} out of range", look.name);
                }
            }
            // The default look is the defaults.
            let first = fx.look_values(fx.looks[0].name).unwrap();
            for (k, v) in fx.knobs.iter().zip(first) {
                assert_eq!(v, (!fx.not_a_look.contains(&k.name)).then_some(k.default), "{} {}", fx.name, k.name);
            }
            // The same commands for each.
            assert_eq!(fx.parse_command(&["on"]), Ok(Command::On));
            assert_eq!(fx.parse_command(&["stats"]), Ok(Command::Stats));
            assert_eq!(fx.parse_command(&["look", fx.looks[1].name]), Ok(Command::Look(fx.looks[1].name)));
            assert_eq!(fx.parse_command(&["bogus"]), Err(fx.usage.to_string()));
            let k = &fx.knobs[0];
            assert_eq!(fx.parse_command(&["set", k.name, &k.show(k.default)]), Ok(Command::Set(k.name, k.default)));
        }
        assert!(effect("fxaa").is_none());
        assert!(control_verb("fxaa", &[]).is_none());

        // The depth of field and the bloom from the environment.
        let env = |name: &str| {
            Some(match name {
                "INTOXICATED_DOF" => "1",
                "INTOXICATED_DOF_LOOK" => "portrait",
                "INTOXICATED_DOF_STRENGTH" => "10",
                "INTOXICATED_DOF_SAMPLES" => "500",
                "INTOXICATED_BLOOM_TINT" => "1,0.9,0.8",
                _ => return None,
            }
            .to_string())
        };
        let dof = post::DOF.parse_settings(env);
        assert!(dof.on);
        assert_eq!(dof.warnings.len(), 1, "{:?}", dof.warnings);
        assert!(dof.warnings[0].starts_with("[android] vulkan: dof: INTOXICATED_DOF_SAMPLES:"), "{:?}", dof.warnings);
        let at = |name: &str| post::DOF_KNOBS.iter().position(|k| k.name == name).unwrap();
        assert_eq!((dof.values[at("strength")], dof.values[at("near_blur")]), ([10.0; 3], [1.0; 3]));
        assert_eq!(dof.values[at("focus_range")], [0.15; 3], "from the look");
        assert_eq!(dof.values[at("samples")], [48.0; 3], "the default for a value out of range");
        let bloom = post::BLOOM.parse_settings(env);
        assert!(!bloom.on && bloom.warnings.is_empty());
        assert_eq!(bloom.values[post::BLOOM_KNOBS.iter().position(|k| k.name == "tint").unwrap()], [1.0, 0.9, 0.8]);
        assert_eq!(post::BLOOM.parse_command(&["intensity", "0.5"]), Ok(Command::Set("intensity", [0.5; 3])));
        assert_eq!(post::DOF.parse_command(&["intensity", "0.5"]), Err(post::DOF_USAGE.to_string()), "dof has no intensity");

        // The lens and the sharpen likewise.
        let env = |name: &str| {
            Some(match name {
                "INTOXICATED_LENS" => "1",
                "INTOXICATED_LENS_LOOK" => "prism",
                "INTOXICATED_LENS_VIGNETTE" => "0",
                "INTOXICATED_SHARPEN" => "1",
                "INTOXICATED_SHARPEN_LOOK" => "crunchy",
                "INTOXICATED_SHARPEN_DEPTH_AWARE" => "2",
                _ => return None,
            }
            .to_string())
        };
        let lens = post::LENS.parse_settings(env);
        assert!(lens.on && lens.warnings.is_empty(), "{:?}", lens.warnings);
        assert_eq!(lens.values, [[5.0; 3], [0.0; 3], [1.0; 3]], "the look, then the knob");
        let sharpen = post::SHARPEN.parse_settings(env);
        assert!(sharpen.on);
        assert_eq!(sharpen.warnings.len(), 1, "{:?}", sharpen.warnings);
        assert_eq!(sharpen.values, [[2.0; 3], [0.28; 3], [1.0; 3]], "the default for a value it does not take");
        assert_eq!(post::LENS.parse_command(&["set", "halo", "0.3"]), Ok(Command::Set("halo", [0.3; 3])));
        assert!(post::SHARPEN.parse_command(&["set", "strength", "4"]).is_err());
    }

    #[test]
    fn modes_name_every_effect_and_fill_in_only_what_is_unset() {
        assert_eq!(MODES.map(|m| m.name), ["off", "competitive", "cinematic"]);
        for m in &MODES {
            assert!(m.title.is_ascii() && m.description.is_ascii() && !m.description.is_empty());
            assert!(SHADERS_USAGE.contains(m.name));
            for (fx, (_, look)) in EFFECTS.iter().zip(m.effects) {
                assert!(fx.looks.iter().any(|l| l.name == look), "{} names no look {look} of {}", m.name, fx.name);
            }
        }
        assert!(MODES[0].effects.iter().all(|(on, _)| !on), "off is off");
        // Competitive: no lens; the default looks, which are the playable ones.
        let competitive = mode("competitive").unwrap();
        for (fx, (on, look)) in EFFECTS.iter().zip(competitive.effects) {
            assert_eq!(on, fx.name != "lens", "{}", fx.name);
            assert_eq!(look, fx.looks[0].name, "{}", fx.name);
        }
        assert!(mode("cinematic").unwrap().effects.iter().all(|(on, _)| *on));
        assert!(mode("fancy").is_none());

        // From the environment: the mode, overridden by an effect's own
        // switch and look, and the knobs as always.
        let env = |name: &str| {
            Some(match name {
                "INTOXICATED_SHADERS_MODE" => "cinematic",
                "INTOXICATED_SSR" => "0",
                "INTOXICATED_DOF_LOOK" => "portrait",
                "INTOXICATED_BLOOM" => " ",
                "INTOXICATED_SHARPEN_LIMIT" => "0.2",
                _ => return None,
            }
            .to_string())
        };
        let ssr = SSR.parse_settings(env);
        assert!(!ssr.on && ssr.warnings.is_empty(), "its own switch wins");
        let dof = post::DOF.parse_settings(env);
        assert!(dof.on);
        assert_eq!(dof.values[0], [14.0; 3], "its own look wins");
        let bloom = post::BLOOM.parse_settings(env);
        assert!(bloom.on, "a blank switch is unset");
        assert_eq!(bloom.values[0], [0.7; 3], "neon");
        let sharpen = post::SHARPEN.parse_settings(env);
        assert!(sharpen.on);
        assert_eq!(sharpen.values, [[2.0; 3], [0.2; 3], [1.0; 3]], "crunchy, the limit its own");
        let lens = post::LENS.parse_settings(env);
        assert!(lens.on && lens.values[2] == [1.0; 3], "prism");

        let off = |name: &str| (name == MODE_ENV).then(|| "off".to_string());
        assert!(EFFECTS.iter().all(|fx| !fx.parse_settings(off).on));
        let competitive = |name: &str| (name == MODE_ENV).then(|| "competitive".to_string());
        let on: Vec<bool> = EFFECTS.iter().map(|fx| fx.parse_settings(competitive).on).collect();
        assert_eq!(on, [true, true, true, false, true]);
        // The look the knobs are at, as devctl shaders reports it (seen as
        // "custom" for every look in game before a knob's unused slots were
        // left out of the comparison). Only this test touches the lens's
        // live values.
        let lens = &post::LENS;
        for look in ["prism", "subtle"] {
            for (k, v) in lens.knobs.iter().zip(lens.look_values(look).unwrap()) {
                lens.set(k, v.unwrap());
            }
            assert_eq!(lens.look_now(), Some(look));
        }
        lens.set(&lens.knobs[0], [3.0; 3]);
        assert_eq!(lens.look_now(), None, "tuned");
        lens.knobs.iter().for_each(|k| lens.set(k, k.default));

        let bad = SSR.parse_settings(|name: &str| (name == MODE_ENV).then(|| "fancy".to_string()));
        assert!(!bad.on && bad.warnings.len() == 1 && bad.warnings[0].contains("is not a mode"), "{:?}", bad.warnings);
    }

    #[test]
    fn the_shader_looks_json_lists_every_effect_as_its_own_table() {
        let text = shader_looks_json();
        assert!(text.is_ascii());
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let effects = doc["effects"].as_array().unwrap();
        assert_eq!(effects.len(), EFFECTS.len());
        for (fx, j) in EFFECTS.iter().zip(effects) {
            assert_eq!((j["name"].as_str(), j["title"].as_str(), j["summary"].as_str()), (Some(fx.name), Some(fx.title), Some(fx.summary)));
            let mut table = j.clone();
            for extra in ["name", "title", "summary"] {
                table.as_object_mut().unwrap().remove(extra);
            }
            assert_eq!(table, fx.json(), "{}", fx.name);
            assert_eq!(j["looks"].as_array().unwrap().len(), fx.looks.len());
            assert_eq!(j["default_look"], fx.looks[0].name);
        }
        assert_eq!(doc["mode_env"], MODE_ENV);
        let modes = doc["modes"].as_array().unwrap();
        assert_eq!(modes.len(), MODES.len());
        for (m, j) in MODES.iter().zip(modes) {
            assert_eq!((j["name"].as_str(), j["title"].as_str()), (Some(m.name), Some(m.title)));
            assert_eq!(j["description"], m.description);
            for (fx, (on, look)) in EFFECTS.iter().zip(m.effects) {
                assert_eq!(j["effects"][fx.name], serde_json::json!({ "on": on, "look": look }), "{} {}", m.name, fx.name);
            }
        }
        // The reflections' own table is what --print-ssr-looks prints.
        let ssr: serde_json::Value = serde_json::from_str(&looks_json()).unwrap();
        assert_eq!(ssr, SSR.json());
    }

    #[test]
    fn parameters_and_packing() {
        let mut values: Vec<[f32; 3]> = KNOBS.iter().map(|k| k.default).collect();
        let p = params(FULL, &values, 0);
        assert!((p.tan_half[1] - 0.700_207_5).abs() < 1e-6);
        assert!((p.tan_half[0] - 0.700_207_5 * 1667.0 / 1349.0).abs() < 1e-6);
        assert_eq!((p.size, p.flags, p.steps, p.intensity, p.fresnel), (FULL, 1, 32, INTENSITY, FRESNEL));
        assert_eq!((p.up_threshold, p.max_distance, p.edge_fade, p.darken, p.roughness), (UP_THRESHOLD, 0.0, EDGE_FADE, 0.0, ROUGHNESS));
        assert_eq!(p.keep_colour, KEEP_COLOUR);
        assert!(!changes_nothing(&values));
        let at = |name: &str| KNOBS.iter().position(|k| k.name == name).unwrap();
        values[at("up_only")] = [0.0; 3];
        assert_eq!(params(FULL, &values, 3).flags, 3 << 8);
        values[at("intensity")] = [0.0; 3];
        assert!(changes_nothing(&values), "nothing to blend and nothing to darken");
        values[at("darken")] = [0.2; 3];
        assert!(!changes_nothing(&values));
        assert_eq!(
            [64, 58, 37, 44, 122, 97, 50].map(pack_mode),
            [Some(0), Some(1), Some(2), Some(3), None, None, None]
        );
        let mut f = Found {
            depth: Tracked { att: Att { usage: 0x27, ..att(SCENE_DEPTH, 126, 4, FULL, 1, SRO) }, layout: SRO },
            color: Tracked { att: att(RESOLVED, 122, 1, FULL, 0, SRO), layout: SRO },
            resolved: true,
        };
        assert!(usable(&f).unwrap_err().contains("cannot write"));
        f.color.att.format = 64;
        f.color.att.usage = 0x13;
        assert!(usable(&f).unwrap_err().contains("without SAMPLED or TRANSFER_DST"));
        f.color.att.usage = 0x17;
        f.color.layout = LAYOUT_UNDEFINED;
        assert!(usable(&f).unwrap_err().contains("layout UNDEFINED"));
        f.color.layout = 7;
        assert_eq!(usable(&f), Ok(0));
        assert_eq!(color_read_layout(7), Some(SRO));
        assert_eq!(depth_read_layout(3), Some(SRO));
        assert_eq!(depth_read_layout(4), Some(4));
    }

    #[test]
    fn candidates_for_the_scene_colour_get_the_usage_they_lack() {
        let full = [FULL];
        // The MSAA 1 scene colour without SAMPLED or TRANSFER_DST.
        assert_eq!(missing_usage(1, 64, [1667, 1349, 1], 1, 1, 1, 0x10, &full), 0x6);
        assert_eq!(missing_usage(1, 64, [1667, 1349, 1], 1, 1, 1, 0x17, &full), 0);
        // Not candidates: multisampled, another size, not a colour
        // attachment, a format the shader cannot write, no swapchain yet.
        assert_eq!(missing_usage(1, 64, [1667, 1349, 1], 1, 1, 4, 0x10, &full), 0);
        assert_eq!(missing_usage(1, 64, [833, 674, 1], 1, 1, 1, 0x10, &full), 0);
        assert_eq!(missing_usage(1, 64, [1667, 1349, 1], 1, 1, 1, 0x4, &full), 0);
        assert_eq!(missing_usage(1, 122, [1667, 1349, 1], 1, 1, 1, 0x10, &full), 0);
        assert_eq!(missing_usage(1, 64, [1667, 1349, 1], 1, 1, 1, 0x10, &[]), 0);
    }

    #[test]
    fn timestamps_and_stats() {
        assert!((ticks_to_ms(1_000, 501_000, u64::MAX, 1.0) - 0.5).abs() < 1e-12);
        // A counter of 36 valid bits that wrapped.
        let mask = (1u64 << 36) - 1;
        assert!((ticks_to_ms(mask - 99, 900, mask, 1.0) - 0.001).abs() < 1e-12);
        let mut s = Stats::default();
        for i in 0..WINDOW + 10 {
            s.gpu_sample(if i % 2 == 0 { 0.25 } else { 0.75 });
        }
        assert_eq!(s.gpu.len(), WINDOW);
        s.applied = 3;
        let line = s.summary();
        assert!(line.contains("gpu_ms avg=0.500"), "{line}");
        assert!(line.contains(&format!("over {WINDOW} frames")), "{line}");
    }

    #[test]
    fn the_structs_have_the_c_layout() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<ImageCreateInfo>(), 88);
        assert_eq!(offset_of!(ImageCreateInfo, usage), 56);
        assert_eq!(offset_of!(ImageCreateInfo, p_queue_family_indices), 72);
        assert_eq!(offset_of!(ImageCreateInfo, initial_layout), 80);
        assert_eq!(size_of::<ImageCopy>(), 68);
        assert_eq!(offset_of!(ImageCopy, dst_subresource), 28);
        assert_eq!(offset_of!(ImageCopy, extent), 56);
        assert_eq!(size_of::<QueryPoolCreateInfo>(), 32);
        assert_eq!(offset_of!(QueryPoolCreateInfo, query_count), 24);
        // The std430 offsets spirv-dis prints for ssr.comp's block.
        assert_eq!(size_of::<Params>(), 76, "the push constant block of ssr.comp");
        assert_eq!(offset_of!(Params, tint), 16);
        assert_eq!(offset_of!(Params, intensity), 32);
        assert_eq!(offset_of!(Params, steps), 40);
        assert_eq!(offset_of!(Params, roughness), 48);
        assert_eq!(offset_of!(Params, darken), 68);
        assert_eq!(offset_of!(Params, keep_colour), 72);
    }

    /// Every shader build.rs knows, compiled again by glslc and compared
    /// with the committed SPIR-V, then validated by spirv-val. Skipped (and
    /// said so) where glslc is not installed.
    #[test]
    fn the_committed_spirv_is_what_glslc_makes_of_the_source() {
        const GLSLC: &str = "/usr/local/bin/glslc";
        const SPIRV_VAL: &str = "/usr/local/bin/spirv-val";
        if !std::path::Path::new(GLSLC).is_file() {
            eprintln!("{GLSLC} is not installed; the committed SPIR-V is not checked");
            return;
        }
        let android = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/android");
        let words = |text: &str| -> Vec<u32> {
            text.split(|c: char| c == ',' || c.is_whitespace() || c == '[' || c == ']' || c == '&')
                .filter(|w| !w.is_empty())
                .map(|w| u32::from_str_radix(w.trim_start_matches("0x"), 16).expect("a hex word"))
                .collect()
        };
        // The same list as build.rs's `SHADERS`.
        let shaders: [(&str, &str, &[&str]); 18] = [
            ("depth_capture/resolve.comp", "depth_capture/resolve_ms", &[]),
            ("depth_capture/resolve.comp", "depth_capture/resolve_1x", &["-DSINGLE_SAMPLE"]),
            ("ssr/depth.comp", "ssr/depth_ms", &[]),
            ("ssr/depth.comp", "ssr/depth_1x", &["-DSINGLE_SAMPLE"]),
            ("ssr/ssr.comp", "ssr/ssr", &[]),
            ("ssr/ssr.comp", "ssr/compose", &["-DCOMPOSE"]),
            ("ssr/post.comp", "ssr/post_focus", &["-DFOCUS"]),
            ("ssr/post.comp", "ssr/post_dof_prep", &["-DDOF_PREP"]),
            ("ssr/post.comp", "ssr/post_dof_gather", &["-DDOF_GATHER"]),
            ("ssr/post.comp", "ssr/post_dof_compose", &["-DDOF_COMPOSE"]),
            ("ssr/post.comp", "ssr/post_bloom_prefilter", &["-DBLOOM_PREFILTER"]),
            ("ssr/post.comp", "ssr/post_bloom_down", &["-DBLOOM_DOWN"]),
            ("ssr/post.comp", "ssr/post_bloom_up", &["-DBLOOM_UP"]),
            ("ssr/post.comp", "ssr/post_bloom_compose", &["-DBLOOM_COMPOSE"]),
            ("ssr/post.comp", "ssr/post_lens", &["-DLENS"]),
            ("ssr/post.comp", "ssr/post_stage_copy", &["-DSTAGE_COPY"]),
            ("ssr/post.comp", "ssr/post_stage_store", &["-DSTAGE_STORE"]),
            ("ssr/post.comp", "ssr/post_sharpen", &["-DSHARPEN"]),
        ];
        for (source, stem, defines) in shaders {
            let run = std::process::Command::new(GLSLC)
                .args(["-O", "--target-env=vulkan1.0", "-mfmt=num"])
                .args(defines)
                .arg(android.join(source))
                .args(["-o", "-"])
                .output()
                .expect("glslc runs");
            assert!(run.status.success(), "{source}: {}", String::from_utf8_lossy(&run.stderr));
            let fresh = words(&String::from_utf8(run.stdout).unwrap());
            let committed = words(&std::fs::read_to_string(android.join(format!("{stem}.spv.rs"))).unwrap());
            assert_eq!(fresh, committed, "{stem}.spv.rs is not what glslc makes of {source} {defines:?}");
            assert_eq!(fresh[0], 0x0723_0203, "SPIR-V magic");
            if std::path::Path::new(SPIRV_VAL).is_file() {
                let path = std::env::temp_dir().join(format!("intoxicated-test-{}-{}.spv", std::process::id(), stem.replace('/', "_")));
                std::fs::write(&path, fresh.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
                let val = std::process::Command::new(SPIRV_VAL).args(["--target-env", "vulkan1.0"]).arg(&path).output().expect("spirv-val runs");
                let _ = std::fs::remove_file(&path);
                assert!(val.status.success(), "{stem}: {}", String::from_utf8_lossy(&val.stderr));
            }
        }
        // What the build included is the same.
        for (built, stem) in
            [(SPIRV_DEPTH_MS, "depth_ms"), (SPIRV_DEPTH_1X, "depth_1x"), (SPIRV_SSR, "ssr"), (SPIRV_COMPOSE, "compose")]
        {
            assert_eq!(built, &words(&std::fs::read_to_string(android.join(format!("ssr/{stem}.spv.rs"))).unwrap())[..]);
        }
        for (built, variant) in post::SPIRV.iter().zip(post::VARIANTS) {
            let stem = format!("ssr/post_{}", variant.to_lowercase());
            assert!(shaders.iter().any(|(_, s, d)| *s == stem && d == &[format!("-D{variant}").as_str()]), "{stem} is listed");
            assert_eq!(*built, &words(&std::fs::read_to_string(android.join(format!("{stem}.spv.rs"))).unwrap())[..]);
        }
    }

    // The all off of the effects hotkey, on switches and an effect of the
    // test's own, so the client's are never touched.

    static T_ON: [AtomicBool; 3] = [const { AtomicBool::new(false) }; 3];

    fn test_switches() -> Vec<Switched> {
        ["a", "b", "c"].iter().zip(&T_ON).map(|(&name, on)| Switched { name, on }).collect()
    }

    fn now(switches: &[Switched]) -> Vec<bool> {
        switches.iter().map(|s| s.on.load(Relaxed)).collect()
    }

    #[test]
    fn all_off_clears_every_switch_and_puts_back_exactly_the_saved_ones() {
        let sw = test_switches();
        for (s, on) in sw.iter().zip([true, false, true]) {
            s.on.store(on, Relaxed);
        }
        let mut all = AllOff::new();
        assert!(!all.is_off());
        assert!(!all.turn_on(&sw), "nothing to put back yet");
        assert!(all.turn_off(&sw));
        assert!(all.is_off());
        assert_eq!(now(&sw), [false, false, false]);
        assert_eq!(all.saved_on(&sw), ["a", "c"]);
        // A second off keeps what was saved the first time.
        assert!(!all.turn_off(&sw));
        assert_eq!(all.saved_on(&sw), ["a", "c"]);
        assert!(all.turn_on(&sw));
        assert!(!all.is_off());
        assert_eq!(now(&sw), [true, false, true]);
        // And again, the same.
        assert!(all.turn_off(&sw) && all.turn_on(&sw));
        assert_eq!(now(&sw), [true, false, true]);
    }

    #[test]
    fn a_switch_changed_while_all_off_is_followed() {
        let sw = test_switches();
        let mut all = AllOff::new();
        for s in &sw {
            s.on.store(true, Relaxed);
        }
        all.turn_off(&sw);
        // devctl turns b off: it stays off when the rest come back.
        all.switch_changed(1, false);
        assert_eq!(all.saved_on(&sw), ["a", "c"]);
        all.turn_on(&sw);
        assert_eq!(now(&sw), [true, false, true]);
        // devctl turns one on while all are off: that ends the all off, with
        // the switches as they are.
        all.turn_off(&sw);
        all.switch_changed(2, true);
        assert!(!all.is_off());
        assert!(!all.turn_on(&sw));
        assert_eq!(now(&sw), [false, false, false]);
        // Out of range or with nothing off, nothing happens.
        all.switch_changed(9, false);
        assert_eq!(all, AllOff::new());
    }

    static T_FX_ON: AtomicBool = AtomicBool::new(false);
    static T_FX_VALUES: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
    /// The reflections' knobs and looks, with values and a switch of its own.
    static T_FX: Effect = Effect {
        name: "test",
        title: "Test",
        summary: "",
        index: 0,
        env: "INTOXICATED_TEST_EFFECT",
        look_env: "INTOXICATED_TEST_EFFECT_LOOK",
        knobs: &KNOBS,
        looks: &LOOKS,
        not_a_look: &NOT_A_LOOK,
        values: &T_FX_VALUES,
        on: &T_FX_ON,
        usage: USAGE,
    };

    #[test]
    fn back_on_an_effect_has_the_look_it_had() {
        let fx = &T_FX;
        let look = fx.start_values("clean_mirror").expect("a look");
        for (k, v) in fx.knobs.iter().zip(&look) {
            fx.set(k, *v);
        }
        let intensity = fx.find("intensity").unwrap();
        fx.set(intensity, [0.77; 3]);
        fx.on.store(true, Relaxed);
        let before = fx.current();
        let sw = [Switched { name: fx.name, on: fx.on }];
        let mut all = AllOff::new();
        all.turn_off(&sw);
        assert!(!fx.is_on());
        assert_eq!(fx.current(), before, "off leaves the knobs alone");
        all.turn_on(&sw);
        assert!(fx.is_on());
        assert_eq!(fx.current(), before);
        assert_eq!(fx.get(intensity)[0], 0.77);
    }

    #[test]
    fn every_effect_is_switched_and_the_verb_is_known() {
        let names: Vec<&str> = switched().iter().map(|s| s.name).collect();
        assert_eq!(names, ["ssr", "dof", "bloom", "lens", "sharpen"]);
        assert_eq!(control_verb("effects", &["sideways"]), Some(Err(EFFECTS_USAGE.to_string())));
        assert_eq!(names_or_none(&[]), "none was on");
    }
}
