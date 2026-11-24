//! Depth of field (`INTOXICATED_DOF=1`, devctl `dof`), bloom
//! (`INTOXICATED_BLOOM=1`, devctl `bloom`), lens effects
//! (`INTOXICATED_LENS=1`, devctl `lens`) and sharpening
//! (`INTOXICATED_SHARPEN=1`, devctl `sharpen`), recorded by the parent
//! module at the same point of the frame as the reflections and after them,
//! in that order: before the swapchain pass that draws the scene and then
//! the HUD, so the HUD is drawn over all of them and is never blurred,
//! glowing, split into colours or sharpened. The frame is matched exactly as
//! for the reflections, and a frame that does not match gets none of them.
//!
//! None of them reveals anything that is not drawn: the depth of field only
//! blurs the engine's own picture, the bloom and the lens's fringe only add
//! light around what is already bright in it, screened so they can only
//! brighten, the aberration moves drawn colours by a few pixels, the
//! vignette only darkens, and the sharpen only raises the contrast of
//! detail that is there, by at most `limit`. Fog, darkness and whatever
//! hides something stay as the engine drew them.
//!
//! # Depth of field
//!
//! Four dispatches of `post.comp`, the last three at half resolution but for
//! the compose:
//!
//! 1. the focus, by `focus_mode`: 1, the nearest depth in a small square
//!    around the screen centre (the character in third person); 2, past
//!    it: the mean log distance of what lies more than 1.5 times as far as
//!    that nearest depth in a larger square (the play area behind the
//!    character), or the sky when only the subject and the sky are there;
//!    0, the fixed `focus_distance`. Eased towards over `focus_time`
//!    seconds in log distance so the focus does not pump, and kept in a 1x1
//!    image of ours from frame to frame. Only what is behind the focus by
//!    more than `far_range` (and, with `near_blur`, in front of it by more
//!    than `focus_range`) blurs.
//! 2. the preparation: the colour 2x2 averaged, with the signed circle of
//!    confusion of the nearest of the four (positive behind the sharp band,
//!    negative in front of it, which only blurs with `near_blur`). The
//!    circle grows with the difference of inverse distances, as a lens's
//!    does, to `strength` pixels at 1080p (scaled with the height); the sky
//!    is as far as anything.
//! 3. the gather: `samples` taps on a golden-angle disc (a round bokeh). A
//!    tap counts where its own circle covers this texel; a tap behind this
//!    texel counts only as far as this texel's own circle reaches, so a
//!    blurred background never bleeds over a sharp edge in front of it.
//! 4. the compose: each full-resolution pixel's own circle mixes the sharp
//!    scene with the blur, upsampled from the four nearest texels weighted
//!    by how alike their circles are to the pixel's (no halo of the edge
//!    on the background), and a blurred thing in front shows over the sharp
//!    pixels behind it.
//!
//! # Bloom
//!
//! A dual-filter chain of `post.comp`: a half-resolution prefilter that
//! keeps what is brighter than `threshold` with a soft knee (`knee`), four
//! levels down, four up, each blending the coarser level into its own by
//! `radius`, and a compose that screens the glow, times `intensity` and
//! `tint`, over the scene. The scene's colour is the engine's 10-bit UNORM
//! image, not HDR, so the threshold is a brightness from 0 to 1.
//!
//! # Lens
//!
//! One full-resolution dispatch into a staging image of ours: red sampled
//! further out from the centre and blue further in, by `aberration` pixels
//! at the corners (at 1080p, scaled with the height) and by the square of
//! the distance from the centre in between, so the middle of the screen,
//! where one aims, stays clean; a rainbow fringe (`halo`) where the bloom
//! chain's wide glow is stronger than its tight one, that is just outside
//! the brightest areas, red at the inner edge to violet at the outer,
//! screened over the scene; and a `vignette` that darkens the corners. The
//! fringe reads the bloom chain, which the lens runs itself (with the
//! bloom's threshold, knee and radius) when the bloom is off.
//!
//! # Sharpen
//!
//! Contrast adaptive sharpening, written here from the idea (AMD's
//! FidelityFX CAS is the best known form of it): the pixel's difference
//! from the average of its four sides is added back, times `strength` and
//! times how much room the 3x3 neighbourhood leaves before black or white
//! against how bright it gets, so flat mid-tone texture is sharpened most
//! and an edge already at full contrast barely at all; the change is
//! clamped to `limit`. With `depth_aware` a neighbour more than 5% (fully
//! from 10%) nearer or further than the pixel counts as the pixel itself,
//! so the outline of a character against the distance is not drawn as a
//! bright or dark halo. Recorded last, after the lens, reading the staging
//! image the lens wrote, or a copy of the scene there when something before
//! wrote our result image (it reads its neighbours, so it never reads the
//! image it writes).
//!
//! # Units
//!
//! Distances are in studs: a depth `d` is `NEAR_STUDS / d` studs away. The
//! near plane's 0.1 studs is assumed, not measured (INFERRED); only
//! `focus_distance` uses it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32};

use super::{
    check, knob, ComputePipelineCreateInfo, DescriptorImageInfo, DescriptorPoolCreateInfo,
    DescriptorPoolSize, DescriptorSetAllocateInfo, DescriptorSetLayoutBinding, DescriptorSetLayoutCreateInfo, Effect,
    Gpu, Kind, Knob, Look, Owned, PipelineLayoutCreateInfo, PipelineShaderStageCreateInfo, PushConstantRange,
    ShaderModuleCreateInfo, WriteDescriptorSet, BIND_POINT_COMPUTE, DESCRIPTOR_COMBINED_IMAGE_SAMPLER,
    DESCRIPTOR_POOL_FREE_SET, DESCRIPTOR_STORAGE_IMAGE, FORMAT_R16G16B16A16_SFLOAT, FORMAT_R32_SFLOAT, LAYOUT_GENERAL,
    SHADER_STAGE_COMPUTE, ST_COMPUTE_PIPELINE_CREATE_INFO, ST_DESCRIPTOR_POOL_CREATE_INFO,
    ST_DESCRIPTOR_SET_ALLOCATE_INFO, ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, ST_PIPELINE_LAYOUT_CREATE_INFO,
    ST_PIPELINE_SHADER_STAGE_CREATE_INFO, ST_SHADER_MODULE_CREATE_INFO, ST_WRITE_DESCRIPTOR_SET, USAGE_SAMPLED,
    USAGE_STORAGE,
};

// ------------------------------------------------------------- depth of field

pub const DOF_ENV: &str = "INTOXICATED_DOF";
pub const DOF_LOOK_ENV: &str = "INTOXICATED_DOF_LOOK";

/// The default look's numbers, shared with the look of that name: a light
/// blur of the far background only, the play area sharp (tuned on Timebomb
/// Duels after the owner found the subject focus too blurry to play with).
const STRENGTH: f32 = 5.0;
const FOCUS_RANGE: f32 = 0.3;
const FAR_RANGE: f32 = 1.5;
const FOCUS_MODE: f32 = 2.0;

/// Every knob of the depth of field, in the order `dof get` lists them.
pub(crate) const DOF_KNOBS: [Knob; 8] = [
    // The largest blur, as a radius in pixels at 1080p (scaled with the
    // height of the scene).
    knob("strength", "INTOXICATED_DOF_STRENGTH", Kind::Number { min: 0.0, max: 32.0 }, [STRENGTH; 3], 0),
    // 0: at focus_distance; 1: on the nearest thing at the screen centre
    // (the character); 2: past it, on what lies behind it.
    knob("focus_mode", "INTOXICATED_DOF_FOCUS_MODE", Kind::Whole { min: 0, max: 2 }, [FOCUS_MODE; 3], 1),
    // The focus in focus_mode 0, in studs.
    knob("focus_distance", "INTOXICATED_DOF_FOCUS_DISTANCE", Kind::Number { min: 0.5, max: 500.0 }, [20.0; 3], 2),
    // How far in front of the focus stays sharp (with near_blur), as a
    // fraction of its distance.
    knob("focus_range", "INTOXICATED_DOF_FOCUS_RANGE", Kind::Number { min: 0.0, max: 4.0 }, [FOCUS_RANGE; 3], 3),
    // How far behind the focus stays sharp, as a fraction of its distance.
    knob("far_range", "INTOXICATED_DOF_FAR_RANGE", Kind::Number { min: 0.0, max: 10.0 }, [FAR_RANGE; 3], 7),
    // 1: what is nearer than the focus blurs too.
    knob("near_blur", "INTOXICATED_DOF_NEAR_BLUR", Kind::Switch, [0.0; 3], 4),
    // Seconds the autofocus takes to settle on a new distance (to 63%).
    knob("focus_time", "INTOXICATED_DOF_FOCUS_TIME", Kind::Number { min: 0.0, max: 3.0 }, [0.3; 3], 5),
    // Taps of the gather: quality for GPU time.
    knob("samples", "INTOXICATED_DOF_SAMPLES", Kind::Whole { min: 8, max: 128 }, [48.0; 3], 6),
];

static DOF_ON: AtomicBool = AtomicBool::new(false);
static DOF_VALUES: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];

/// The looks, the default first; the others are starting points.
pub(crate) const DOF_LOOKS: [Look; 3] = [
    Look {
        name: "playable",
        description: "Focus past your character: the play area stays sharp, only the far background and the sky soften.",
        values: &[],
    },
    Look {
        name: "cinematic",
        description: "For clips: focus on your character, the background soft like a camera lens.",
        values: &[("strength", [8.0; 3]), ("focus_mode", [1.0; 3]), ("far_range", [0.3; 3])],
    },
    Look {
        name: "portrait",
        description: "For clips: your character alone sharp, a strong blur in front and behind.",
        values: &[
            ("strength", [14.0; 3]),
            ("focus_mode", [1.0; 3]),
            ("focus_range", [0.15; 3]),
            ("far_range", [0.15; 3]),
            ("near_blur", [1.0; 3]),
        ],
    },
];

pub(crate) const DOF_USAGE: &str = "dof on|off|stats|get|reset|look <name>|set <name> <value>; names: strength \
     focus_mode focus_distance focus_range far_range near_blur focus_time samples; looks: playable cinematic portrait";

pub(crate) static DOF: Effect = Effect {
    name: "dof",
    title: "Depth of field",
    summary: "Blurs what is far from the focus like a camera lens: by default only the far background, or \
              everything but your character for clips. The HUD stays sharp.",
    index: 1,
    env: DOF_ENV,
    look_env: DOF_LOOK_ENV,
    knobs: &DOF_KNOBS,
    looks: &DOF_LOOKS,
    not_a_look: &["focus_distance", "focus_time", "samples"],
    values: &DOF_VALUES,
    on: &DOF_ON,
    usage: DOF_USAGE,
};

// ------------------------------------------------------------- bloom

pub const BLOOM_ENV: &str = "INTOXICATED_BLOOM";
pub const BLOOM_LOOK_ENV: &str = "INTOXICATED_BLOOM_LOOK";

const BLOOM_INTENSITY: f32 = 0.35;
const THRESHOLD: f32 = 0.75;
const KNEE: f32 = 0.5;
const RADIUS: f32 = 0.7;

/// Every knob of the bloom, in the order `bloom get` lists them.
pub(crate) const BLOOM_KNOBS: [Knob; 5] = [
    // How strong the glow is.
    knob("intensity", "INTOXICATED_BLOOM_INTENSITY", Kind::Number { min: 0.0, max: 2.0 }, [BLOOM_INTENSITY; 3], 0),
    // The brightness (the largest of red, green and blue, 0 to 1) a pixel
    // starts to glow at.
    knob("threshold", "INTOXICATED_BLOOM_THRESHOLD", Kind::Number { min: 0.0, max: 1.0 }, [THRESHOLD; 3], 1),
    // How softly the glow starts around the threshold, as a fraction of it.
    knob("knee", "INTOXICATED_BLOOM_KNEE", Kind::Number { min: 0.0, max: 1.0 }, [KNEE; 3], 2),
    // 0: a tight glow; 1: a wide one.
    knob("radius", "INTOXICATED_BLOOM_RADIUS", Kind::Number { min: 0.0, max: 1.0 }, [RADIUS; 3], 3),
    // The colour the glow is multiplied by.
    knob("tint", "INTOXICATED_BLOOM_TINT", Kind::Rgb { min: 0.0, max: 2.0 }, [1.0; 3], 4),
];

static BLOOM_ON: AtomicBool = AtomicBool::new(false);
static BLOOM_VALUES: [AtomicU32; 7] = [const { AtomicU32::new(0) }; 7];

pub(crate) const BLOOM_LOOKS: [Look; 3] = [
    Look { name: "soft", description: "A soft glow around lights, neon and the brightest sky.", values: &[] },
    Look {
        name: "neon",
        description: "Strong glow from more of the bright colours, for neon and night maps.",
        values: &[("intensity", [0.7; 3]), ("threshold", [0.6; 3]), ("radius", [0.75; 3])],
    },
    Look {
        name: "dreamy",
        description: "A wide, hazy glow from everything bright, slightly warm.",
        values: &[
            ("intensity", [0.5; 3]),
            ("threshold", [0.5; 3]),
            ("knee", [0.8; 3]),
            ("radius", [0.9; 3]),
            ("tint", [1.0, 0.95, 0.85]),
        ],
    },
];

pub(crate) const BLOOM_USAGE: &str = "bloom on|off|stats|get|reset|look <name>|intensity <0..2>|set <name> <value>; \
     names: intensity threshold knee radius tint; looks: soft neon dreamy";

pub(crate) static BLOOM: Effect = Effect {
    name: "bloom",
    title: "Bloom",
    summary: "A soft glow around bright lights and neon. The HUD never glows.",
    index: 2,
    env: BLOOM_ENV,
    look_env: BLOOM_LOOK_ENV,
    knobs: &BLOOM_KNOBS,
    looks: &BLOOM_LOOKS,
    not_a_look: &[],
    values: &BLOOM_VALUES,
    on: &BLOOM_ON,
    usage: BLOOM_USAGE,
};

// ------------------------------------------------------------- lens

pub const LENS_ENV: &str = "INTOXICATED_LENS";
pub const LENS_LOOK_ENV: &str = "INTOXICATED_LENS_LOOK";

const ABERRATION: f32 = 1.5;
const VIGNETTE: f32 = 0.15;
const HALO: f32 = 0.3;

/// Every knob of the lens, in the order `lens get` lists them.
pub(crate) const LENS_KNOBS: [Knob; 3] = [
    // How far red and blue are pulled apart in the corners, in pixels at
    // 1080p; less towards the centre, nothing in it.
    knob("aberration", "INTOXICATED_LENS_ABERRATION", Kind::Number { min: 0.0, max: 16.0 }, [ABERRATION; 3], 0),
    // How much darker the corners are.
    knob("vignette", "INTOXICATED_LENS_VIGNETTE", Kind::Number { min: 0.0, max: 1.0 }, [VIGNETTE; 3], 1),
    // How strong the rainbow fringe round the brightest areas is.
    knob("halo", "INTOXICATED_LENS_HALO", Kind::Number { min: 0.0, max: 2.0 }, [HALO; 3], 2),
];

static LENS_ON: AtomicBool = AtomicBool::new(false);
static LENS_VALUES: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

pub(crate) const LENS_LOOKS: [Look; 3] = [
    Look {
        name: "subtle",
        description: "A faint colour split towards the screen edges, slightly darker corners, a hint of rainbow round \
                      bright lights.",
        values: &[],
    },
    Look {
        name: "prism",
        description: "For clips: a clear colour split at the edges and a rainbow halo round the brightest areas.",
        values: &[("aberration", [5.0; 3]), ("vignette", [0.3; 3]), ("halo", [1.0; 3])],
    },
    Look {
        name: "film",
        description: "Darker corners and a slight colour split, no halo, like an old camera lens.",
        values: &[("aberration", [2.5; 3]), ("vignette", [0.45; 3]), ("halo", [0.0; 3])],
    },
];

pub(crate) const LENS_USAGE: &str = "lens on|off|stats|get|reset|look <name>|set <name> <value>; names: aberration \
     vignette halo; looks: subtle prism film";

pub(crate) static LENS: Effect = Effect {
    name: "lens",
    title: "Lens",
    summary: "Chromatic aberration towards the screen edges, a rainbow halo round the brightest lights and a \
              vignette, like a camera lens. The HUD stays clean.",
    index: 3,
    env: LENS_ENV,
    look_env: LENS_LOOK_ENV,
    knobs: &LENS_KNOBS,
    looks: &LENS_LOOKS,
    not_a_look: &[],
    values: &LENS_VALUES,
    on: &LENS_ON,
    usage: LENS_USAGE,
};

// ------------------------------------------------------------- sharpen

pub const SHARPEN_ENV: &str = "INTOXICATED_SHARPEN";
pub const SHARPEN_LOOK_ENV: &str = "INTOXICATED_SHARPEN_LOOK";

const SHARPEN_STRENGTH: f32 = 0.5;
const SHARPEN_LIMIT: f32 = 0.1;

/// Every knob of the sharpen, in the order `sharpen get` lists them.
pub(crate) const SHARPEN_KNOBS: [Knob; 3] = [
    // How strongly detail is raised; the adaptive part holds it back where
    // contrast is already high.
    knob("strength", "INTOXICATED_SHARPEN_STRENGTH", Kind::Number { min: 0.0, max: 3.0 }, [SHARPEN_STRENGTH; 3], 0),
    // The most a pixel's red, green or blue changes, 0 to 1.
    knob("limit", "INTOXICATED_SHARPEN_LIMIT", Kind::Number { min: 0.0, max: 1.0 }, [SHARPEN_LIMIT; 3], 1),
    // 1: not across depth edges, so outlines get no halo.
    knob("depth_aware", "INTOXICATED_SHARPEN_DEPTH_AWARE", Kind::Switch, [1.0; 3], 2),
];

static SHARPEN_ON: AtomicBool = AtomicBool::new(false);
static SHARPEN_VALUES: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

pub(crate) const SHARPEN_LOOKS: [Look; 3] = [
    Look { name: "light", description: "Crisper textures without halos round edges.", values: &[] },
    Look {
        name: "crisp",
        description: "Clearly sharper textures and text on signs.",
        values: &[("strength", [1.0; 3]), ("limit", [0.16; 3])],
    },
    Look {
        name: "crunchy",
        description: "For clips: strongly sharpened, crunchy texture detail.",
        values: &[("strength", [2.0; 3]), ("limit", [0.28; 3])],
    },
];

pub(crate) const SHARPEN_USAGE: &str = "sharpen on|off|stats|get|reset|look <name>|set <name> <value>; names: \
     strength limit depth_aware; looks: light crisp crunchy";

pub(crate) static SHARPEN: Effect = Effect {
    name: "sharpen",
    title: "Sharpen",
    summary: "Contrast adaptive sharpening of the 3D scene: crisper texture detail without halos round edges. \
              The HUD is not sharpened.",
    index: 4,
    env: SHARPEN_ENV,
    look_env: SHARPEN_LOOK_ENV,
    knobs: &SHARPEN_KNOBS,
    looks: &SHARPEN_LOOKS,
    not_a_look: &[],
    values: &SHARPEN_VALUES,
    on: &SHARPEN_ON,
    usage: SHARPEN_USAGE,
};

// ------------------------------------------------------------- parameters

/// Studs per near-plane unit: a depth `d` is `NEAR_STUDS / d` studs away
/// (INFERRED, see the module doc).
pub(crate) const NEAR_STUDS: f32 = 0.1;

/// The push constants of `post.comp` (std430, which for these fields is the
/// C layout).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PostParams {
    pub size: [u32; 2],
    pub dst_size: [u32; 2],
    pub tint: [f32; 4],
    pub flags: u32,
    pub coc_scale: f32,
    pub focus_fixed: f32,
    pub focus_range: f32,
    pub focus_blend: f32,
    pub threshold: f32,
    pub knee: f32,
    pub intensity: f32,
    pub radius: f32,
    pub samples: u32,
    pub far_range: f32,
    pub aberration: f32,
    pub vignette: f32,
    pub halo: f32,
    pub sharpen: f32,
    pub sharpen_limit: f32,
}

/// `flags` bits.
pub(crate) const SRC_RESULT: u32 = 0x1;
pub(crate) const NEAR_BLUR: u32 = 0x2;
pub(crate) const AUTOFOCUS: u32 = 0x4;
pub(crate) const FOCUS_RESET: u32 = 0x8;
pub(crate) const FOCUS_PAST: u32 = 0x10;
pub(crate) const SRC_STAGE: u32 = 0x20;
pub(crate) const DEPTH_AWARE: u32 = 0x40;
/// The reflections' trace image holds the ray-traced glow above white
/// (`ssr.comp` GIAPPLY), which the bloom adds.
pub(crate) const GLOW: u32 = 0x80;

fn value(fx: &Effect, values: &[[f32; 3]], name: &str) -> [f32; 3] {
    fx.knobs.iter().position(|k| k.name == name).map_or([0.0; 3], |i| values[i])
}

/// Every post effect's knob values, each in its knob order.
#[derive(Clone)]
pub(crate) struct PostValues {
    pub dof: Vec<[f32; 3]>,
    pub bloom: Vec<[f32; 3]>,
    pub lens: Vec<[f32; 3]>,
    pub sharpen: Vec<[f32; 3]>,
}

impl PostValues {
    pub(crate) fn current() -> PostValues {
        PostValues { dof: DOF.current(), bloom: BLOOM.current(), lens: LENS.current(), sharpen: SHARPEN.current() }
    }

    #[cfg(test)]
    pub(crate) fn defaults() -> PostValues {
        let d = |fx: &Effect| fx.knobs.iter().map(|k| k.default).collect();
        PostValues { dof: d(&DOF), bloom: d(&BLOOM), lens: d(&LENS), sharpen: d(&SHARPEN) }
    }
}

/// The push constants for a scene of `extent` packed as `mode`, from the
/// post effects' knob values, `dt` seconds after the last frame with depth
/// of field.
pub(crate) fn post_params(extent: [u32; 2], v: &PostValues, mode: u32, dt: f32) -> PostParams {
    let d = |name| value(&DOF, &v.dof, name)[0];
    let b = |name| value(&BLOOM, &v.bloom, name);
    let l = |name| value(&LENS, &v.lens, name)[0];
    let s = |name| value(&SHARPEN, &v.sharpen, name)[0];
    let focus_time = d("focus_time");
    let tint = b("tint");
    PostParams {
        size: extent,
        dst_size: half(extent),
        tint: [tint[0], tint[1], tint[2], 1.0],
        flags: (mode << 8)
            | if d("near_blur") != 0.0 { NEAR_BLUR } else { 0 }
            | match d("focus_mode") as u32 {
                0 => 0,
                1 => AUTOFOCUS,
                _ => AUTOFOCUS | FOCUS_PAST,
            }
            | if s("depth_aware") != 0.0 { DEPTH_AWARE } else { 0 },
        coc_scale: d("strength") * extent[1] as f32 / 1080.0 * 0.5,
        focus_fixed: d("focus_distance") / NEAR_STUDS,
        focus_range: d("focus_range"),
        focus_blend: if focus_time <= 0.0 { 1.0 } else { 1.0 - (-dt.max(0.0) / focus_time).exp() },
        threshold: b("threshold")[0],
        knee: b("knee")[0],
        intensity: b("intensity")[0],
        radius: b("radius")[0],
        samples: d("samples") as u32,
        far_range: d("far_range"),
        aberration: l("aberration") * extent[1] as f32 / 1080.0,
        vignette: l("vignette"),
        halo: l("halo"),
        sharpen: s("strength"),
        sharpen_limit: s("limit"),
    }
}

/// Whether depth of field with these values leaves every pixel as it was.
pub(crate) fn dof_changes_nothing(values: &[[f32; 3]]) -> bool {
    value(&DOF, values, "strength")[0] <= 0.0
}

/// Whether bloom with these values leaves every pixel as it was.
pub(crate) fn bloom_changes_nothing(values: &[[f32; 3]]) -> bool {
    value(&BLOOM, values, "intensity")[0] <= 0.0 || value(&BLOOM, values, "tint") == [0.0; 3]
}

/// Whether the lens with these values leaves every pixel as it was.
pub(crate) fn lens_changes_nothing(values: &[[f32; 3]]) -> bool {
    ["aberration", "vignette", "halo"].iter().all(|name| value(&LENS, values, name)[0] <= 0.0)
}

/// Whether the lens with these values reads the bloom chain.
pub(crate) fn lens_wants_chain(values: &[[f32; 3]]) -> bool {
    value(&LENS, values, "halo")[0] > 0.0
}

/// Whether the sharpen with these values leaves every pixel as it was.
pub(crate) fn sharpen_changes_nothing(values: &[[f32; 3]]) -> bool {
    value(&SHARPEN, values, "strength")[0] <= 0.0 || value(&SHARPEN, values, "limit")[0] <= 0.0
}

/// Whether the sharpen with these values reads the depth copy.
pub(crate) fn sharpen_wants_depth(values: &[[f32; 3]]) -> bool {
    value(&SHARPEN, values, "depth_aware")[0] != 0.0
}

/// Half of `extent`, rounded up.
pub(crate) fn half(extent: [u32; 2]) -> [u32; 2] {
    [extent[0].div_ceil(2).max(1), extent[1].div_ceil(2).max(1)]
}

/// Levels of the bloom chain, the half-resolution one first.
pub(crate) const LEVELS: usize = 5;

/// The size of every level of the bloom chain for a scene of `extent`.
pub(crate) fn level_sizes(extent: [u32; 2]) -> [[u32; 2]; LEVELS] {
    let mut sizes = [half(extent); LEVELS];
    for i in 1..LEVELS {
        sizes[i] = half(sizes[i - 1]);
    }
    sizes
}

// ------------------------------------------------------------- shaders

/// The variants of `post.comp`, in pipeline order, with the define of each.
#[cfg(test)]
pub(crate) const VARIANTS: [&str; 12] = [
    "FOCUS",
    "DOF_PREP",
    "DOF_GATHER",
    "DOF_COMPOSE",
    "BLOOM_PREFILTER",
    "BLOOM_DOWN",
    "BLOOM_UP",
    "BLOOM_COMPOSE",
    "LENS",
    "STAGE_COPY",
    "STAGE_STORE",
    "SHARPEN",
];
const FOCUS: usize = 0;
const DOF_PREP: usize = 1;
const DOF_GATHER: usize = 2;
const DOF_COMPOSE: usize = 3;
const BLOOM_PREFILTER: usize = 4;
const BLOOM_DOWN: usize = 5;
const BLOOM_UP: usize = 6;
const BLOOM_COMPOSE: usize = 7;
const LENS_PASS: usize = 8;
const STAGE_COPY: usize = 9;
const STAGE_STORE: usize = 10;
const SHARPEN_PASS: usize = 11;

/// `post.comp` with each define, as build.rs made them.
pub(crate) const SPIRV: [&[u32]; 12] = [
    include!(concat!(env!("OUT_DIR"), "/ssr_post_focus.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_dof_prep.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_dof_gather.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_dof_compose.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_bloom_prefilter.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_bloom_down.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_bloom_up.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_bloom_compose.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_lens.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_stage_copy.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_stage_store.spv.rs")),
    include!(concat!(env!("OUT_DIR"), "/ssr_post_sharpen.spv.rs")),
];

// ------------------------------------------------------------- our objects

/// The per-dispatch descriptor sets (set 1 of `post.comp`) of one set of
/// images: what each reads, writes and reads besides.
const SET_PREP: usize = 0;
const SET_GATHER: usize = 1;
const SET_DOF_COMPOSE: usize = 2;
const SET_BLOOM_PREFILTER: usize = 3;
/// Four: level i into level i + 1.
const SET_DOWN: usize = 4;
/// Four: level i + 1 into level i.
const SET_UP: usize = 8;
const SET_BLOOM_COMPOSE: usize = 12;
/// The lens: the bloom chain's second and fourth levels in, the staging
/// image out.
const SET_LENS: usize = 13;
/// The staging image read (the sharpen after the lens, the store).
const SET_STAGE_READ: usize = 14;
/// The staging image written (the copy before a sharpen).
const SET_STAGE_WRITE: usize = 15;
pub(crate) const PASS_SETS: usize = 16;

/// Sets of images the post pool holds the pass sets of: the current one and
/// those retired after a resize that frames in flight may still use.
const MAX_IMAGE_SETS: u32 = 24;

/// The images of one scene size: the depth of field's half-resolution
/// preparation and blur, the bloom chain, the full-resolution staging image
/// of the lens and the sharpen, and their pass sets.
pub(crate) struct PostImages {
    pub(super) prep: Owned,
    pub(super) gather: Owned,
    pub(super) stage: Owned,
    pub(super) levels: [Owned; LEVELS],
    pub(super) sizes: [[u32; 2]; LEVELS],
    pub(super) sets: [u64; PASS_SETS],
}

impl PostImages {
    pub(crate) fn images(&self) -> impl Iterator<Item = u64> + '_ {
        [self.prep.image, self.gather.image, self.stage.image].into_iter().chain(self.levels.iter().map(|l| l.image))
    }
}

/// What is made once per device for the depth of field and the bloom.
#[derive(Default)]
pub(crate) struct PostGpu {
    pub(super) set_layout: u64,
    pub(super) pipeline_layout: u64,
    pub(super) shaders: [u64; 12],
    pub(super) pipelines: [u64; 12],
    pub(super) pool: u64,
    /// The focus distance, kept from frame to frame (1x1 R32F).
    pub(super) focus: Owned,
    /// The focus image has never been written: it is moved out of
    /// `UNDEFINED` and the focus starts over.
    pub(super) focus_fresh: bool,
    /// When depth of field was last recorded, for the focus's easing.
    pub(super) focus_at: Option<std::time::Instant>,
}

impl Gpu {
    /// The depth of field's and the bloom's layouts, pipelines, pool and
    /// focus image; on failure the caller destroys whatever was made.
    pub(super) fn build_post(&mut self) -> Result<(), String> {
        let d = self.dev();
        let none: *const c_void = std::ptr::null();
        let binding = |binding: u32, kind: u32| DescriptorSetLayoutBinding {
            binding,
            kind,
            count: 1,
            stages: SHADER_STAGE_COMPUTE,
            immutable_samplers: none.cast(),
        };
        let bindings = [
            binding(0, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
            binding(1, DESCRIPTOR_STORAGE_IMAGE),
            binding(2, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
            binding(3, DESCRIPTOR_STORAGE_IMAGE),
        ];
        let layout = DescriptorSetLayoutCreateInfo {
            s_type: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            p_next: none,
            flags: 0,
            count: bindings.len() as u32,
            bindings: bindings.as_ptr(),
        };
        check((self.fns.create_set_layout)(d, &layout, none, &mut self.post.set_layout), "vkCreateDescriptorSetLayout")?;
        // Set 0 is the reflections' own layout, so their set serves here too.
        let set_layouts = [self.set_layout, self.post.set_layout];
        let push = PushConstantRange { stages: SHADER_STAGE_COMPUTE, offset: 0, size: std::mem::size_of::<PostParams>() as u32 };
        let pipeline_layout = PipelineLayoutCreateInfo {
            s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
            p_next: none,
            flags: 0,
            set_layout_count: 2,
            set_layouts: set_layouts.as_ptr(),
            push_constant_count: 1,
            push_constants: &push,
        };
        check((self.fns.create_pipeline_layout)(d, &pipeline_layout, none, &mut self.post.pipeline_layout), "vkCreatePipelineLayout")?;
        for (i, code) in SPIRV.into_iter().enumerate() {
            let module = ShaderModuleCreateInfo {
                s_type: ST_SHADER_MODULE_CREATE_INFO,
                p_next: none,
                flags: 0,
                code_size: std::mem::size_of_val(code),
                code: code.as_ptr(),
            };
            check((self.fns.create_shader_module)(d, &module, none, &mut self.post.shaders[i]), "vkCreateShaderModule")?;
            let pipeline = ComputePipelineCreateInfo {
                s_type: ST_COMPUTE_PIPELINE_CREATE_INFO,
                p_next: none,
                flags: 0,
                stage: PipelineShaderStageCreateInfo {
                    s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                    p_next: none,
                    flags: 0,
                    stage: SHADER_STAGE_COMPUTE,
                    module: self.post.shaders[i],
                    name: c"main".as_ptr(),
                    specialization: none,
                },
                layout: self.post.pipeline_layout,
                base_pipeline: 0,
                base_pipeline_index: -1,
            };
            check(
                (self.fns.create_compute_pipelines)(d, 0, 1, &pipeline, none, &mut self.post.pipelines[i]),
                "vkCreateComputePipelines",
            )?;
        }
        let sets = MAX_IMAGE_SETS * PASS_SETS as u32;
        let sizes = [
            DescriptorPoolSize { kind: DESCRIPTOR_COMBINED_IMAGE_SAMPLER, count: 2 * sets },
            DescriptorPoolSize { kind: DESCRIPTOR_STORAGE_IMAGE, count: 2 * sets },
        ];
        let pool = DescriptorPoolCreateInfo {
            s_type: ST_DESCRIPTOR_POOL_CREATE_INFO,
            p_next: none,
            flags: DESCRIPTOR_POOL_FREE_SET,
            max_sets: sets,
            count: 2,
            sizes: sizes.as_ptr(),
        };
        check((self.fns.create_descriptor_pool)(d, &pool, none, &mut self.post.pool), "vkCreateDescriptorPool")?;
        let mut focus = Owned::default();
        let made = self.make_image(&mut focus, [1, 1], FORMAT_R32_SFLOAT, USAGE_STORAGE);
        self.post.focus = focus;
        self.post.focus_fresh = true;
        made
    }

    pub(super) fn destroy_post(&self) {
        let (f, d, none) = (&self.fns, self.dev(), std::ptr::null());
        let destroy = |what: super::DestroyFn, h: u64| {
            if h != 0 {
                what(d, h, none);
            }
        };
        self.destroy_owned(&self.post.focus);
        destroy(f.destroy_descriptor_pool, self.post.pool);
        for p in self.post.pipelines {
            destroy(f.destroy_pipeline, p);
        }
        for s in self.post.shaders {
            destroy(f.destroy_shader_module, s);
        }
        destroy(f.destroy_pipeline_layout, self.post.pipeline_layout);
        destroy(f.destroy_set_layout, self.post.set_layout);
    }

    pub(super) fn destroy_post_images(&self, p: &PostImages) {
        let made: Vec<u64> = p.sets.iter().copied().filter(|&s| s != 0).collect();
        if !made.is_empty() {
            (self.fns.free_descriptor_sets)(self.dev(), self.post.pool, made.len() as u32, made.as_ptr());
        }
        self.destroy_owned(&p.prep);
        self.destroy_owned(&p.gather);
        self.destroy_owned(&p.stage);
        for l in &p.levels {
            self.destroy_owned(l);
        }
    }

    /// The images and pass sets for a scene of `extent`; on failure
    /// whatever was made is destroyed.
    pub(super) fn make_post_images(&self, extent: [u32; 2]) -> Result<PostImages, String> {
        let mut p = PostImages {
            prep: Owned::default(),
            gather: Owned::default(),
            stage: Owned::default(),
            levels: Default::default(),
            sizes: level_sizes(extent),
            sets: [0; PASS_SETS],
        };
        let made = self.fill_post_images(&mut p, extent);
        match made {
            Ok(()) => Ok(p),
            Err(e) => {
                self.destroy_post_images(&p);
                Err(e)
            }
        }
    }

    fn fill_post_images(&self, p: &mut PostImages, extent: [u32; 2]) -> Result<(), String> {
        let usage = USAGE_STORAGE | USAGE_SAMPLED;
        self.make_image(&mut p.prep, half(extent), FORMAT_R16G16B16A16_SFLOAT, usage)?;
        self.make_image(&mut p.gather, half(extent), FORMAT_R16G16B16A16_SFLOAT, usage)?;
        self.make_image(&mut p.stage, extent, FORMAT_R16G16B16A16_SFLOAT, usage)?;
        for i in 0..LEVELS {
            let size = p.sizes[i];
            self.make_image(&mut p.levels[i], size, FORMAT_R16G16B16A16_SFLOAT, usage)?;
        }
        let (prep, gather, stage) = (p.prep.view, p.gather.view, p.stage.view);
        let level = |i: usize| p.levels[i].view;
        // What each set reads (binding 0), writes (1) and reads besides (2).
        // Every binding is written with something valid, the ones a variant
        // does not use with the preparation image.
        let mut wanted = [(prep, prep, prep); PASS_SETS];
        wanted[SET_PREP] = (gather, prep, gather);
        wanted[SET_GATHER] = (prep, gather, prep);
        wanted[SET_DOF_COMPOSE] = (gather, prep, prep);
        wanted[SET_BLOOM_PREFILTER] = (prep, level(0), prep);
        for i in 0..LEVELS - 1 {
            wanted[SET_DOWN + i] = (level(i), level(i + 1), prep);
            wanted[SET_UP + i] = (level(i + 1), level(i), prep);
        }
        wanted[SET_BLOOM_COMPOSE] = (level(0), prep, prep);
        wanted[SET_LENS] = (level(1), stage, level(3));
        wanted[SET_STAGE_READ] = (stage, prep, prep);
        wanted[SET_STAGE_WRITE] = (prep, stage, prep);
        for (i, &(input, output, aux)) in wanted.iter().enumerate() {
            p.sets[i] = self.pass_set(input, output, aux)?;
        }
        Ok(())
    }

    fn pass_set(&self, input: u64, output: u64, aux: u64) -> Result<u64, String> {
        let none = std::ptr::null();
        let alloc = DescriptorSetAllocateInfo {
            s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
            p_next: none,
            pool: self.post.pool,
            count: 1,
            layouts: &self.post.set_layout,
        };
        let mut set = 0;
        check((self.fns.allocate_descriptor_sets)(self.dev(), &alloc, &mut set), "vkAllocateDescriptorSets (post)")?;
        let general = LAYOUT_GENERAL as i32;
        let images = [
            DescriptorImageInfo { sampler: self.sampler_linear, view: input, layout: general },
            DescriptorImageInfo { sampler: 0, view: output, layout: general },
            DescriptorImageInfo { sampler: self.sampler_linear, view: aux, layout: general },
            DescriptorImageInfo { sampler: 0, view: self.post.focus.view, layout: general },
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
            write(1, DESCRIPTOR_STORAGE_IMAGE),
            write(2, DESCRIPTOR_COMBINED_IMAGE_SAMPLER),
            write(3, DESCRIPTOR_STORAGE_IMAGE),
        ];
        (self.fns.update_descriptor_sets)(self.dev(), writes.len() as u32, writes.as_ptr(), 0, none);
        Ok(set)
    }
}

// ------------------------------------------------------------- recording

/// Binds, pushes and dispatches of the post pipelines in one command buffer.
struct Rec<'a> {
    g: &'a Gpu,
    cmd: *mut c_void,
    /// The reflections' set for this frame (set 0).
    frame_set: u64,
    images: &'a PostImages,
}

impl Rec<'_> {
    fn dispatch(&self, pipeline: usize, set: usize, p: &PostParams, groups: [u32; 2]) {
        let f = &self.g.fns;
        let sets = [self.frame_set, self.images.sets[set]];
        (f.cmd_bind_pipeline)(self.cmd, BIND_POINT_COMPUTE, self.g.post.pipelines[pipeline]);
        (f.cmd_bind_descriptor_sets)(self.cmd, BIND_POINT_COMPUTE, self.g.post.pipeline_layout, 0, 2, sets.as_ptr(), 0, std::ptr::null());
        (f.cmd_push_constants)(
            self.cmd,
            self.g.post.pipeline_layout,
            SHADER_STAGE_COMPUTE,
            0,
            std::mem::size_of::<PostParams>() as u32,
            (p as *const PostParams).cast(),
        );
        (f.cmd_dispatch)(self.cmd, groups[0], groups[1], 1);
        super::compute_barrier(f, self.cmd);
    }
}

fn groups(size: [u32; 2]) -> [u32; 2] {
    [size[0].div_ceil(8), size[1].div_ceil(8)]
}

/// The depth of field, into our result image, after a barrier that made
/// the depth copy (and the reflections' result, when `p.flags` says the
/// scene is there) visible.
pub(super) fn record_dof(g: &Gpu, cmd: *mut c_void, frame_set: u64, images: &PostImages, p: &PostParams) {
    let r = Rec { g, cmd, frame_set, images };
    let mut p = *p;
    let full = p.size;
    let half = half(full);
    p.dst_size = [1, 1];
    r.dispatch(FOCUS, SET_PREP, &p, [1, 1]);
    p.dst_size = half;
    r.dispatch(DOF_PREP, SET_PREP, &p, groups(half));
    r.dispatch(DOF_GATHER, SET_GATHER, &p, groups(half));
    r.dispatch(DOF_COMPOSE, SET_DOF_COMPOSE, &p, groups(full));
}

/// The bloom, into our result image.
pub(super) fn record_bloom(g: &Gpu, cmd: *mut c_void, frame_set: u64, images: &PostImages, p: &PostParams) {
    record_bloom_chain(g, cmd, frame_set, images, p);
    let r = Rec { g, cmd, frame_set, images };
    let mut p = *p;
    p.dst_size = images.sizes[0];
    r.dispatch(BLOOM_COMPOSE, SET_BLOOM_COMPOSE, &p, groups(p.size));
}

/// The bloom chain alone, every level merged with the coarser ones: the
/// bloom's, and the lens's fringe's when the bloom is off.
pub(super) fn record_bloom_chain(g: &Gpu, cmd: *mut c_void, frame_set: u64, images: &PostImages, p: &PostParams) {
    let r = Rec { g, cmd, frame_set, images };
    let mut p = *p;
    let sizes = images.sizes;
    p.dst_size = sizes[0];
    r.dispatch(BLOOM_PREFILTER, SET_BLOOM_PREFILTER, &p, groups(sizes[0]));
    for i in 0..LEVELS - 1 {
        p.dst_size = sizes[i + 1];
        r.dispatch(BLOOM_DOWN, SET_DOWN + i, &p, groups(sizes[i + 1]));
    }
    for i in (0..LEVELS - 1).rev() {
        p.dst_size = sizes[i];
        r.dispatch(BLOOM_UP, SET_UP + i, &p, groups(sizes[i]));
    }
}

/// The lens, from the scene (and the bloom chain, which must have been
/// recorded this frame when `p.halo` is above 0) into the staging image;
/// with `store`, then into our result image, for when no sharpen follows.
pub(super) fn record_lens(g: &Gpu, cmd: *mut c_void, frame_set: u64, images: &PostImages, p: &PostParams, store: bool) {
    let r = Rec { g, cmd, frame_set, images };
    r.dispatch(LENS_PASS, SET_LENS, p, groups(p.size));
    if store {
        r.dispatch(STAGE_STORE, SET_STAGE_READ, p, groups(p.size));
    }
}

/// The sharpen, into our result image. `staged`: the scene is already in
/// the staging image (the lens wrote it). Otherwise, when `p.flags` says
/// the scene is in our result image, it is copied to the staging image
/// first, as the sharpen reads neighbours of the pixel it writes.
pub(super) fn record_sharpen(g: &Gpu, cmd: *mut c_void, frame_set: u64, images: &PostImages, p: &PostParams, staged: bool) {
    let r = Rec { g, cmd, frame_set, images };
    let mut p = *p;
    if staged {
        p.flags |= SRC_STAGE;
    } else if p.flags & SRC_RESULT != 0 {
        r.dispatch(STAGE_COPY, SET_STAGE_WRITE, &p, groups(p.size));
        p.flags |= SRC_STAGE;
    }
    r.dispatch(SHARPEN_PASS, SET_STAGE_READ, &p, groups(p.size));
}

/// The barrier that moves a fresh focus image out of `UNDEFINED`, once.
pub(super) fn focus_layout(g: &mut Gpu) -> Option<(u64, u32)> {
    if std::mem::take(&mut g.post.focus_fresh) {
        Some((g.post.focus.image, super::LAYOUT_UNDEFINED))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_push_constants_have_the_std430_layout() {
        use std::mem::{offset_of, size_of};
        // As spirv-dis prints post.comp's block.
        assert_eq!(size_of::<PostParams>(), 96);
        assert_eq!(offset_of!(PostParams, far_range), 72);
        assert_eq!(offset_of!(PostParams, aberration), 76);
        assert_eq!(offset_of!(PostParams, halo), 84);
        assert_eq!(offset_of!(PostParams, sharpen_limit), 92);
        assert_eq!(offset_of!(PostParams, dst_size), 8);
        assert_eq!(offset_of!(PostParams, tint), 16);
        assert_eq!(offset_of!(PostParams, flags), 32);
        assert_eq!(offset_of!(PostParams, focus_blend), 48);
        assert_eq!(offset_of!(PostParams, radius), 64);
        assert_eq!(offset_of!(PostParams, samples), 68);
    }

    #[test]
    fn parameters_from_the_knobs() {
        let all = PostValues::defaults();
        let (dof, bloom) = (all.dof.clone(), all.bloom.clone());
        let p = post_params([1920, 1080], &all, 1, 0.0);
        assert_eq!((p.size, p.dst_size, p.flags), ([1920, 1080], [960, 540], (1 << 8) | AUTOFOCUS | FOCUS_PAST | DEPTH_AWARE));
        assert_eq!((p.aberration, p.vignette, p.halo), (ABERRATION, VIGNETTE, HALO));
        assert_eq!((p.sharpen, p.sharpen_limit), (SHARPEN_STRENGTH, SHARPEN_LIMIT));
        assert_eq!(p.far_range, FAR_RANGE);
        assert!((p.coc_scale - STRENGTH * 0.5).abs() < 1e-6, "strength is a radius at 1080p, in half pixels");
        assert_eq!((p.focus_blend, p.samples, p.focus_range), (0.0, 48, FOCUS_RANGE));
        assert!((p.focus_fixed - 200.0).abs() < 1e-3, "20 studs over a near plane of 0.1");
        assert_eq!((p.threshold, p.knee, p.intensity, p.radius, p.tint), (THRESHOLD, KNEE, BLOOM_INTENSITY, RADIUS, [1.0; 4]));
        let p = post_params([1667, 1349], &all, 0, 0.3);
        assert!((p.aberration - ABERRATION * 1349.0 / 1080.0).abs() < 1e-5, "the aberration is in pixels at 1080p");
        assert!((p.coc_scale - STRENGTH * 1349.0 / 1080.0 * 0.5).abs() < 1e-5, "scaled with the height");
        assert!((p.focus_blend - (1.0 - (-1.0f32).exp())).abs() < 1e-6, "63% of the way after focus_time");
        assert_eq!(p.dst_size, [834, 675]);

        let mut dof2 = dof.clone();
        dof2[DOF_KNOBS.iter().position(|k| k.name == "near_blur").unwrap()] = [1.0; 3];
        dof2[DOF_KNOBS.iter().position(|k| k.name == "focus_mode").unwrap()] = [0.0; 3];
        dof2[DOF_KNOBS.iter().position(|k| k.name == "focus_time").unwrap()] = [0.0; 3];
        let mut other = all.clone();
        other.dof = dof2.clone();
        other.sharpen[SHARPEN_KNOBS.iter().position(|k| k.name == "depth_aware").unwrap()] = [0.0; 3];
        let p = post_params([8, 8], &other, 2, 0.0);
        assert_eq!((p.flags, p.focus_blend), ((2 << 8) | NEAR_BLUR, 1.0));

        assert!(!dof_changes_nothing(&dof) && !bloom_changes_nothing(&bloom));
        dof2[0] = [0.0; 3];
        assert!(dof_changes_nothing(&dof2));
        let mut bloom2 = bloom.clone();
        bloom2[0] = [0.0; 3];
        assert!(bloom_changes_nothing(&bloom2));
        let mut bloom3 = bloom.clone();
        bloom3[BLOOM_KNOBS.iter().position(|k| k.name == "tint").unwrap()] = [0.0; 3];
        assert!(bloom_changes_nothing(&bloom3));

        assert!(!lens_changes_nothing(&all.lens) && lens_wants_chain(&all.lens));
        assert!(!sharpen_changes_nothing(&all.sharpen) && sharpen_wants_depth(&all.sharpen));
        let mut lens = all.lens.clone();
        lens[2] = [0.0; 3];
        assert!(!lens_changes_nothing(&lens) && !lens_wants_chain(&lens), "no halo, no chain");
        lens[0] = [0.0; 3];
        lens[1] = [0.0; 3];
        assert!(lens_changes_nothing(&lens));
        for i in 0..2 {
            let mut sharpen = all.sharpen.clone();
            sharpen[i] = [0.0; 3];
            assert!(sharpen_changes_nothing(&sharpen), "{}", SHARPEN_KNOBS[i].name);
        }
    }

    #[test]
    fn the_bloom_chain_halves_down_to_one_texel() {
        assert_eq!(level_sizes([1667, 1349]), [[834, 675], [417, 338], [209, 169], [105, 85], [53, 43]]);
        assert_eq!(level_sizes([3, 1]), [[2, 1], [1, 1], [1, 1], [1, 1], [1, 1]]);
        assert_eq!(VARIANTS.len(), SPIRV.len());
    }
}
