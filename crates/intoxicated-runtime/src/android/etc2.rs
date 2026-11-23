//! A CPU decoder for the ETC2 and EAC block formats, for drivers that cannot
//! sample them.
//!
//! NVIDIA's desktop driver reports `textureCompressionETC2` false. The engine
//! reads that bit, logs `ETC1 0 ETC2 0`, and then has nothing to put in the
//! built-in textures it ships only as ETC1 -- the plastic detail texture is the
//! one somebody noticed missing. `vulkan_etc` answers the engine as though the
//! feature were there and hands the driver an uncompressed image instead; this
//! file is what turns the engine's blocks into that image's texels.
//!
//! This decoder is written from the Khronos Data Format Specification's "ETC2 Compressed Texture Image Formats" and "EAC"
//! sections; the tables below were typed from it, and the tests at the bottom
//! are blocks assembled bit by bit and worked through by hand from the same
//! text, not output captured from another decoder.
//!
//! Hand vectors and decoder share one reader of the specification, so they
//! were also checked against a decoder nobody here wrote: on 2026-11-21,
//! NVIDIA's own GLES driver (580.142, RTX 4070 Ti, FreeBSD -- the same driver
//! whose Vulkan side lacks the feature) sampled 4096 random blocks of each of
//! the seven layouts through a headless EGL context, and every texel matched
//! this file's output.
//!
//! Every function here is pure and safe, so all of it is tested without a GPU.

/// One of the seven block layouts behind the ten ETC2/EAC `VkFormat`s. The
/// sRGB and UNORM variants of each share a layout; only the substitute format
/// they are uploaded into differs, and that is `vulkan_etc`'s business.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// ETC2 RGB, opaque.
    Rgb8,
    /// ETC2 RGB with one bit of "punch-through" alpha.
    Rgb8A1,
    /// An EAC alpha block followed by an ETC2 RGB block.
    Rgba8,
    /// One EAC 11-bit channel.
    R11,
    R11Signed,
    /// Two EAC 11-bit channels, red block first.
    Rg11,
    Rg11Signed,
}

impl Layout {
    /// Bytes per 4x4 block.
    pub const fn block_bytes(self) -> usize {
        match self {
            Layout::Rgb8 | Layout::Rgb8A1 | Layout::R11 | Layout::R11Signed => 8,
            Layout::Rgba8 | Layout::Rg11 | Layout::Rg11Signed => 16,
        }
    }

    /// Bytes per decoded texel: RGBA8 for the colour formats, one or two
    /// 16-bit channels for EAC. Sixteen bits, not eight, because EAC carries
    /// eleven and the substitute should not throw three of them away.
    pub const fn texel_bytes(self) -> usize {
        match self {
            Layout::Rgb8 | Layout::Rgb8A1 | Layout::Rgba8 => 4,
            Layout::R11 | Layout::R11Signed => 2,
            Layout::Rg11 | Layout::Rg11Signed => 4,
        }
    }
}

/// ETC1/ETC2 intensity modifiers, the "small" and "large" value of each of the
/// eight tables; the sign comes from the pixel index.
const INTENSITY: [[i32; 2]; 8] =
    [[2, 8], [5, 17], [9, 29], [13, 42], [18, 60], [24, 80], [33, 106], [47, 183]];

/// The distance table T and H modes share.
const DISTANCE: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

/// EAC modifier tables, indexed by the block's table index then the 3-bit
/// pixel index.
const EAC: [[i32; 8]; 16] = [
    [-3, -6, -9, -15, 2, 5, 8, 14],
    [-3, -7, -10, -13, 2, 6, 9, 12],
    [-2, -5, -8, -13, 1, 4, 7, 12],
    [-2, -4, -6, -13, 1, 3, 5, 12],
    [-3, -6, -8, -12, 2, 5, 7, 11],
    [-3, -7, -9, -11, 2, 6, 8, 10],
    [-4, -7, -8, -11, 3, 6, 7, 10],
    [-3, -5, -8, -11, 2, 4, 7, 10],
    [-2, -6, -8, -10, 1, 5, 7, 9],
    [-2, -5, -8, -10, 1, 4, 7, 9],
    [-2, -4, -8, -10, 1, 3, 7, 9],
    [-2, -5, -7, -10, 1, 4, 6, 9],
    [-3, -4, -7, -10, 2, 3, 6, 9],
    [-1, -2, -3, -10, 0, 1, 2, 9],
    [-4, -6, -8, -9, 3, 5, 7, 8],
    [-3, -5, -7, -9, 2, 4, 6, 8],
];

/// A 64-bit block, which the specification numbers from bit 63 in the first
/// byte down to bit 0 in the eighth: big-endian, whatever the host is.
#[derive(Clone, Copy)]
struct Bits(u64);

impl Bits {
    fn of(bytes: &[u8]) -> Bits {
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[..8]);
        Bits(u64::from_be_bytes(b))
    }

    /// Bits `hi` down to `lo` inclusive, as an unsigned field.
    fn field(self, hi: u32, lo: u32) -> i32 {
        ((self.0 >> lo) & ((1u64 << (hi - lo + 1)) - 1)) as i32
    }

    fn bit(self, at: u32) -> i32 {
        self.field(at, at)
    }

    /// The two index bits of the pixel at column `x`, row `y`, as `msb*2+lsb`.
    /// Pixels are numbered down the columns, so pixel `x*4+y`; the low bits
    /// sit in bits 15..0 and the high bits in 31..16.
    fn index(self, x: usize, y: usize) -> usize {
        let i = (x * 4 + y) as u32;
        ((self.bit(16 + i) << 1) | self.bit(i)) as usize
    }
}

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn extend4(v: i32) -> i32 {
    (v << 4) | v
}

fn extend5(v: i32) -> i32 {
    (v << 3) | (v >> 2)
}

fn extend6(v: i32) -> i32 {
    (v << 2) | (v >> 4)
}

fn extend7(v: i32) -> i32 {
    (v << 1) | (v >> 6)
}

/// A 3-bit two's-complement delta.
fn delta3(v: i32) -> i32 {
    if v >= 4 {
        v - 8
    } else {
        v
    }
}

/// Sixteen RGBA texels, row-major (`y*4+x`), which is the order the region
/// decoder below writes them out in.
type Texels = [[u8; 4]; 16];

const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

/// Decode one ETC2 colour block.
///
/// `punch_through` selects the RGB8A1 reading, where bit 33 is not the
/// differential flag but an "opaque" flag, the block is always read as
/// differential (individual mode does not exist in that format), and with the
/// flag clear one index means a fully transparent black texel.
fn etc2_colour(b: Bits, punch_through: bool) -> Texels {
    let flag33 = b.bit(33) != 0;
    let (differential, opaque) = if punch_through { (true, flag33) } else { (flag33, true) };

    let mut out = [[0u8, 0, 0, 255]; 16];

    let (c1, c2) = if !differential {
        (
            [extend4(b.field(63, 60)), extend4(b.field(55, 52)), extend4(b.field(47, 44))],
            [extend4(b.field(59, 56)), extend4(b.field(51, 48)), extend4(b.field(43, 40))],
        )
    } else {
        let r = b.field(63, 59);
        let g = b.field(55, 51);
        let bl = b.field(47, 43);
        let r2 = r + delta3(b.field(58, 56));
        let g2 = g + delta3(b.field(50, 48));
        let b2 = bl + delta3(b.field(42, 40));
        // An overflowing sum is not an error: it is how ETC2 fits three more
        // modes into blocks that ETC1 decoders would read as differential.
        // The order of the checks is the specification's.
        if !(0..=31).contains(&r2) {
            return t_mode(b, opaque);
        }
        if !(0..=31).contains(&g2) {
            return h_mode(b, opaque);
        }
        if !(0..=31).contains(&b2) {
            return planar(b);
        }
        ([extend5(r), extend5(g), extend5(bl)], [extend5(r2), extend5(g2), extend5(b2)])
    };

    // Each half has four possible colours, one per index, so build both
    // palettes once (eight colours) and look texels up by index, rather than
    // matching and clamping per texel. The per-texel form decoded ~46 MB/s of
    // ETC2 RGB, the rate a submit-time burst was bound by.
    let flip = b.bit(32) != 0;
    let mut palettes = [[[0u8, 0, 0, 255]; 4]; 2];
    for (half, (base, table)) in [(c1, b.field(39, 37)), (c2, b.field(36, 34))].into_iter().enumerate() {
        let [small, large] = INTENSITY[table as usize];
        let shade = |m: i32| [clamp8(base[0] + m), clamp8(base[1] + m), clamp8(base[2] + m), 255];
        palettes[half] = if opaque {
            [shade(small), shade(large), shade(-small), shade(-large)]
        } else {
            // Punch-through with the opaque flag clear: the small modifiers
            // are dropped to zero and index 2 is transparent.
            [shade(0), shade(large), TRANSPARENT, shade(-large)]
        };
    }
    for y in 0..4 {
        for x in 0..4 {
            // Flip clear: two 2x4 halves side by side. Set: two 4x2 halves
            // stacked.
            let second = if flip { y >= 2 } else { x >= 2 };
            out[y * 4 + x] = palettes[second as usize][b.index(x, y)];
        }
    }
    out
}

/// Paint four colours by index, honouring punch-through transparency on
/// index 2, which T and H modes share.
fn paint(b: Bits, colours: [[i32; 3]; 4], opaque: bool) -> Texels {
    let mut palette = colours.map(|c| [clamp8(c[0]), clamp8(c[1]), clamp8(c[2]), 255]);
    if !opaque {
        palette[2] = TRANSPARENT;
    }
    let mut out = [[0u8; 4]; 16];
    for y in 0..4 {
        for x in 0..4 {
            out[y * 4 + x] = palette[b.index(x, y)];
        }
    }
    out
}

fn add(c: [i32; 3], d: i32) -> [i32; 3] {
    [c[0] + d, c[1] + d, c[2] + d]
}

fn t_mode(b: Bits, opaque: bool) -> Texels {
    let c1 = [
        extend4((b.field(60, 59) << 2) | b.field(57, 56)),
        extend4(b.field(55, 52)),
        extend4(b.field(51, 48)),
    ];
    let c2 = [extend4(b.field(47, 44)), extend4(b.field(43, 40)), extend4(b.field(39, 36))];
    let d = DISTANCE[((b.field(35, 34) << 1) | b.bit(32)) as usize];
    paint(b, [c1, add(c2, d), c2, add(c2, -d)], opaque)
}

fn h_mode(b: Bits, opaque: bool) -> Texels {
    let r1 = b.field(62, 59);
    let g1 = (b.field(58, 56) << 1) | b.bit(52);
    let b1 = (b.bit(51) << 3) | b.field(49, 47);
    let r2 = b.field(46, 43);
    let g2 = b.field(42, 39);
    let b2 = b.field(38, 35);
    // The lowest bit of the distance index is not stored; it is which of the
    // two base colours is numerically larger, read as 12-bit RGB444.
    let order = i32::from((r1 << 8) | (g1 << 4) | b1 >= (r2 << 8) | (g2 << 4) | b2);
    let d = DISTANCE[((b.bit(34) << 2) | (b.bit(32) << 1) | order) as usize];
    let c1 = [extend4(r1), extend4(g1), extend4(b1)];
    let c2 = [extend4(r2), extend4(g2), extend4(b2)];
    paint(b, [add(c1, d), add(c1, -d), add(c2, d), add(c2, -d)], opaque)
}

fn planar(b: Bits) -> Texels {
    let o = [
        extend6(b.field(62, 57)),
        extend7((b.bit(56) << 6) | b.field(54, 49)),
        extend6((b.bit(48) << 5) | (b.field(44, 43) << 3) | b.field(41, 39)),
    ];
    let h = [extend6((b.field(38, 34) << 1) | b.bit(32)), extend7(b.field(31, 25)), extend6(b.field(24, 19))];
    let v = [extend6(b.field(18, 13)), extend7(b.field(12, 6)), extend6(b.field(5, 0))];
    let mut out = [[0u8; 4]; 16];
    for y in 0..4i32 {
        for x in 0..4i32 {
            let ch = |c: usize| clamp8((x * (h[c] - o[c]) + y * (v[c] - o[c]) + 4 * o[c] + 2) >> 2);
            out[(y * 4 + x) as usize] = [ch(0), ch(1), ch(2), 255];
        }
    }
    // Planar ignores punch-through: every texel is opaque.
    out
}

/// The raw parts of an EAC block: base codeword, multiplier, and each pixel's
/// modifier, row-major.
fn eac_parts(b: Bits) -> (i32, i32, [i32; 16]) {
    let table = &EAC[b.field(51, 48) as usize];
    let mut modifiers = [0i32; 16];
    for (i, m) in (0..16u32).map(|i| (i, table[b.field(47 - 3 * i, 45 - 3 * i) as usize])) {
        // Pixel `i` in the block's own column-major numbering.
        let (x, y) = ((i / 4) as usize, (i % 4) as usize);
        modifiers[y * 4 + x] = m;
    }
    (b.field(63, 56), b.field(55, 52), modifiers)
}

/// An EAC block read as the 8-bit alpha of `ETC2_R8G8B8A8`.
fn eac_alpha(b: Bits) -> [u8; 16] {
    let (base, mul, m) = eac_parts(b);
    m.map(|m| clamp8(base + m * mul))
}

/// An EAC block read as an 11-bit channel, returned already widened to the
/// 16-bit UNORM or SNORM value the substitute format stores.
fn eac_11(b: Bits, signed: bool) -> [u16; 16] {
    let (raw_base, mul, m) = eac_parts(b);
    m.map(|m| {
        // A zero multiplier is not "no modulation": the specification reads it
        // as an eighth, so the modifier lands on the 11-bit value directly.
        let step = if mul == 0 { m } else { m * mul * 8 };
        if signed {
            // The base is two's complement, and -128 is read as -127 so the
            // range is symmetric; values clamp to +-1023, not +-1024.
            let base = (raw_base as u8 as i8).max(-127) as i32;
            widen_snorm((base * 8 + step).clamp(-1023, 1023))
        } else {
            widen_unorm((raw_base * 8 + 4 + step).clamp(0, 2047))
        }
    })
}

/// 11-bit UNORM to 16-bit by bit replication, so 0 and 2047 land exactly on
/// 0 and 65535.
fn widen_unorm(v: i32) -> u16 {
    ((v << 5) | (v >> 6)) as u16
}

/// 11-bit SNORM (+-1023) to 16-bit (+-32767), by replication of the
/// magnitude, so the end points stay exact and zero stays zero.
fn widen_snorm(v: i32) -> u16 {
    let m = v.abs();
    let wide = (m << 5) | (m >> 5);
    (if v < 0 { -wide } else { wide }) as i16 as u16
}

/// Decode one block into sixteen texels of `layout.texel_bytes()` each,
/// row-major, written to `out`.
pub fn decode_block(layout: Layout, block: &[u8], out: &mut [u8; 64]) {
    let colour = |texels: Texels, out: &mut [u8; 64]| {
        for (i, t) in texels.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(t);
        }
    };
    let channels = |planes: &[[u16; 16]], out: &mut [u8; 64]| {
        let stride = planes.len() * 2;
        for (c, plane) in planes.iter().enumerate() {
            for (i, v) in plane.iter().enumerate() {
                out[i * stride + c * 2..i * stride + c * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
    };
    match layout {
        Layout::Rgb8 => colour(etc2_colour(Bits::of(block), false), out),
        Layout::Rgb8A1 => colour(etc2_colour(Bits::of(block), true), out),
        Layout::Rgba8 => {
            let mut texels = etc2_colour(Bits::of(&block[8..]), false);
            for (t, a) in texels.iter_mut().zip(eac_alpha(Bits::of(block))) {
                t[3] = a;
            }
            colour(texels, out);
        }
        Layout::R11 | Layout::R11Signed => {
            channels(&[eac_11(Bits::of(block), layout == Layout::R11Signed)], out)
        }
        Layout::Rg11 | Layout::Rg11Signed => {
            let signed = layout == Layout::Rg11Signed;
            channels(&[eac_11(Bits::of(block), signed), eac_11(Bits::of(&block[8..]), signed)], out)
        }
    }
}

/// Decode a `width` x `height` texel rectangle whose blocks start at `src`,
/// `row_blocks` blocks to a row, into `dst` packed tightly at
/// `width * layout.texel_bytes()` bytes a row.
///
/// The rectangle need not be a multiple of four: an image whose mip is 2x1 is
/// still stored as one whole block, and only the texels inside the rectangle
/// are written. Errors are a source or destination too short for the
/// rectangle, which is a caller's arithmetic going wrong and is reported
/// rather than read or written out of bounds.
pub fn decode_rect(
    layout: Layout,
    src: &[u8],
    row_blocks: usize,
    width: usize,
    height: usize,
    dst: &mut [u8],
) -> Result<(), String> {
    let bb = layout.block_bytes();
    let tb = layout.texel_bytes();
    let (wide, high) = (width.div_ceil(4), height.div_ceil(4));
    if wide > row_blocks {
        return Err(format!("{width} texels need {wide} blocks a row, the source has {row_blocks}"));
    }
    if high > 0 {
        let need = ((high - 1) * row_blocks + wide) * bb;
        if src.len() < need {
            return Err(format!("source is {} bytes, the rectangle needs {need}", src.len()));
        }
    }
    if dst.len() < width * height * tb {
        return Err(format!(
            "destination is {} bytes, the rectangle needs {}",
            dst.len(),
            width * height * tb
        ));
    }
    let mut texels = [0u8; 64];
    for by in 0..high {
        for bx in 0..wide {
            let at = (by * row_blocks + bx) * bb;
            decode_block(layout, &src[at..at + bb], &mut texels);
            for y in 0..4.min(height - by * 4) {
                let cols = 4.min(width - bx * 4);
                let from = y * 4 * tb;
                let to = ((by * 4 + y) * width + bx * 4) * tb;
                dst[to..to + cols * tb].copy_from_slice(&texels[from..from + cols * tb]);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(layout: Layout, block: &[u8]) -> [[u8; 4]; 16] {
        let mut out = [0u8; 64];
        decode_block(layout, block, &mut out);
        std::array::from_fn(|i| [out[i * 4], out[i * 4 + 1], out[i * 4 + 2], out[i * 4 + 3]])
    }

    fn at(t: &[[u8; 4]; 16], x: usize, y: usize) -> [u8; 4] {
        t[y * 4 + x]
    }

    /// Individual mode, flip clear. Bases R 0xA/0x5, G 0x0/0xF, B 0x3/0xC,
    /// extended to (170,0,51) and (85,255,204); tables 0 {2,8} and 7 {47,183}.
    /// Pixel (0,0) has index 3 (-b), (2,1) index 2 (-a), (3,3) index 1 (+b);
    /// every other pixel index 0 (+a).
    #[test]
    fn individual_mode() {
        let t = rgba(Layout::Rgb8, &[0xA5, 0x0F, 0x3C, 0x1C, 0x02, 0x01, 0x80, 0x01]);
        assert_eq!(at(&t, 0, 0), [162, 0, 43, 255]);
        assert_eq!(at(&t, 1, 0), [172, 2, 53, 255]);
        assert_eq!(at(&t, 1, 3), [172, 2, 53, 255]);
        assert_eq!(at(&t, 2, 0), [132, 255, 251, 255]);
        assert_eq!(at(&t, 2, 1), [38, 208, 157, 255]);
        assert_eq!(at(&t, 3, 3), [255, 255, 255, 255]);
    }

    /// Differential mode, flip set. R 16+3, G 31+0, B 8-1, extended to
    /// (132,255,66) above and (156,255,57) below; tables 1 {5,17}, 2 {9,29}.
    /// Pixel (1,3) has index 3, so the lower half's -29.
    #[test]
    fn differential_mode_with_flip() {
        let t = rgba(Layout::Rgb8, &[0x83, 0xF8, 0x47, 0x2B, 0x00, 0x80, 0x00, 0x80]);
        assert_eq!(at(&t, 0, 0), [137, 255, 71, 255]);
        assert_eq!(at(&t, 3, 1), [137, 255, 71, 255]);
        assert_eq!(at(&t, 0, 2), [165, 255, 66, 255]);
        assert_eq!(at(&t, 1, 3), [127, 226, 28, 255]);
    }

    /// Red overflows (31+1), so T mode. c1 = (221,34,255), c2 = (136,68,0),
    /// distance index 0b101 = 32. Pixels (0..3,0) carry indices 0..3.
    #[test]
    fn t_mode_block() {
        let t = rgba(Layout::Rgb8, &[0xF9, 0x2F, 0x84, 0x0B, 0x11, 0x00, 0x10, 0x10]);
        assert_eq!(at(&t, 0, 0), [221, 34, 255, 255]);
        assert_eq!(at(&t, 1, 0), [168, 100, 32, 255]);
        assert_eq!(at(&t, 2, 0), [136, 68, 0, 255]);
        assert_eq!(at(&t, 3, 0), [104, 36, 0, 255]);
        assert_eq!(at(&t, 2, 2), [221, 34, 255, 255]);
    }

    /// Green overflows (31+2) with red in range, so H mode. c1 = (170,119,221),
    /// c2 = (51,136,102); stored distance bits 1 and 0, and c1 > c2, so index
    /// 0b101 = 32.
    #[test]
    fn h_mode_block() {
        let t = rgba(Layout::Rgb8, &[0x53, 0xFA, 0x9C, 0x36, 0x11, 0x00, 0x10, 0x10]);
        assert_eq!(at(&t, 0, 0), [202, 151, 253, 255]);
        assert_eq!(at(&t, 1, 0), [138, 87, 189, 255]);
        assert_eq!(at(&t, 2, 0), [83, 168, 134, 255]);
        assert_eq!(at(&t, 3, 0), [19, 104, 70, 255]);
    }

    /// Blue overflows (31+2) with red and green in range, so planar.
    /// O = (130,129,113), H = (255,0,0), V = (0,255,255).
    #[test]
    fn planar_mode_block() {
        let t = rgba(Layout::Rgb8, &[0x41, 0x00, 0xFA, 0x7F, 0x00, 0x00, 0x1F, 0xFF]);
        assert_eq!(at(&t, 0, 0), [130, 129, 113, 255]);
        assert_eq!(at(&t, 3, 0), [224, 32, 28, 255]);
        assert_eq!(at(&t, 0, 3), [33, 224, 220, 255]);
        assert_eq!(at(&t, 3, 3), [126, 127, 135, 255]);
    }

    /// The differential block above with bit 33 clear, read as RGB8A1: the
    /// small modifier becomes zero, index 2 is transparent black, and the
    /// large modifiers stand.
    #[test]
    fn punch_through_alpha() {
        let t = rgba(Layout::Rgb8A1, &[0x83, 0xF8, 0x47, 0x29, 0x80, 0x10, 0x81, 0x00]);
        assert_eq!(at(&t, 0, 0), [132, 255, 66, 255]);
        assert_eq!(at(&t, 1, 0), [0, 0, 0, 0]);
        assert_eq!(at(&t, 2, 0), [149, 255, 83, 255]);
        assert_eq!(at(&t, 0, 3), [156, 255, 57, 255]);
        assert_eq!(at(&t, 3, 3), [127, 226, 28, 255]);
        // With the opaque flag set it is the ordinary differential block.
        let opaque = rgba(Layout::Rgb8A1, &[0x83, 0xF8, 0x47, 0x2B, 0x00, 0x80, 0x00, 0x80]);
        assert_eq!(opaque, rgba(Layout::Rgb8, &[0x83, 0xF8, 0x47, 0x2B, 0x00, 0x80, 0x00, 0x80]));
    }

    /// Base 128, multiplier 3, table 13 {-1 -2 -3 -10 0 1 2 9}. Pixel 0 (0,0)
    /// index 3, pixel 1 (0,1) index 7, pixel 15 (3,3) index 0, the rest 4.
    const EAC_BLOCK: [u8; 8] = [0x80, 0x3D, 0x7E, 0x49, 0x24, 0x92, 0x49, 0x20];

    #[test]
    fn eac_alpha_rides_on_the_colour_block() {
        let mut block = EAC_BLOCK.to_vec();
        block.extend_from_slice(&[0xA5, 0x0F, 0x3C, 0x1C, 0x02, 0x01, 0x80, 0x01]);
        let t = rgba(Layout::Rgba8, &block);
        assert_eq!(at(&t, 0, 0), [162, 0, 43, 98]);
        assert_eq!(at(&t, 0, 1), [172, 2, 53, 155]);
        assert_eq!(at(&t, 1, 0), [172, 2, 53, 128]);
        assert_eq!(at(&t, 3, 3), [255, 255, 255, 125]);
    }

    fn r16(layout: Layout, block: &[u8]) -> [u16; 16] {
        let mut out = [0u8; 64];
        decode_block(layout, block, &mut out);
        std::array::from_fn(|i| u16::from_le_bytes([out[i * 2], out[i * 2 + 1]]))
    }

    /// 11-bit values 128*8+4+24m, widened by replication: 788, 1244, 1004 and
    /// 1028 for the four modifiers.
    #[test]
    fn r11_unsigned() {
        let v = r16(Layout::R11, &EAC_BLOCK);
        assert_eq!((v[0], v[4], v[15], v[1]), (25228, 39827, 32143, 32912));
        // A zero multiplier adds the modifier to the 11-bit value directly:
        // 1018 and 1037.
        let mut zero = EAC_BLOCK;
        zero[1] = 0x0D;
        let v = r16(Layout::R11, &zero);
        assert_eq!((v[0], v[4]), (widen_unorm(1018), widen_unorm(1037)));
        assert_eq!((widen_unorm(0), widen_unorm(2047)), (0, 65535));
    }

    /// Base 0xF0 is -16, so -128 + 24m: -368, 88, -152, -128.
    #[test]
    fn r11_signed() {
        let mut block = EAC_BLOCK;
        block[0] = 0xF0;
        let v = r16(Layout::R11Signed, &block).map(|v| v as i16);
        assert_eq!((v[0], v[4], v[15], v[1]), (-11787, 2818, -4868, -4100));
        // -128 reads as -127: -1016, and -1016 - 240 clamps to -1023.
        let v = r16(Layout::R11Signed, &EAC_BLOCK).map(|v| v as i16);
        assert_eq!((v[0], v[1]), (-32767, widen_snorm(-1016) as i16));
        assert_eq!((widen_snorm(1023) as i16, widen_snorm(0)), (32767, 0));
    }

    #[test]
    fn rg11_puts_red_then_green_in_each_texel() {
        let mut block = EAC_BLOCK.to_vec();
        block.extend_from_slice(&[0x00, 0x0D, 0, 0, 0, 0, 0, 0]);
        let mut out = [0u8; 64];
        decode_block(Layout::Rg11, &block, &mut out);
        // Green: base 0, table 13 index 0 (-1) with multiplier 0 is 0*8+4-1.
        assert_eq!(u16::from_le_bytes([out[0], out[1]]), 25228);
        assert_eq!(u16::from_le_bytes([out[2], out[3]]), widen_unorm(3));
    }

    /// A 6x3 rectangle out of a source two blocks wide with a third block of
    /// padding per row: the partial right-hand block and the partial bottom
    /// row are clipped, and the padding is never read.
    #[test]
    fn a_rectangle_that_is_not_whole_blocks() {
        let a = [0xA5, 0x0F, 0x3C, 0x1C, 0x02, 0x01, 0x80, 0x01];
        let b = [0x41, 0x00, 0xFA, 0x7F, 0x00, 0x00, 0x1F, 0xFF];
        let mut src = Vec::new();
        src.extend_from_slice(&a);
        src.extend_from_slice(&b);
        src.extend_from_slice(&[0xEE; 8]);
        let mut dst = vec![0u8; 6 * 3 * 4];
        decode_rect(Layout::Rgb8, &src, 3, 6, 3, &mut dst).unwrap();
        let texel = |x: usize, y: usize| <[u8; 4]>::try_from(&dst[(y * 6 + x) * 4..][..4]).unwrap();
        let (ta, tb) = (rgba(Layout::Rgb8, &a), rgba(Layout::Rgb8, &b));
        for y in 0..3 {
            for x in 0..6 {
                let want = if x < 4 { at(&ta, x, y) } else { at(&tb, x - 4, y) };
                assert_eq!(texel(x, y), want, "texel ({x},{y})");
            }
        }
        assert!(decode_rect(Layout::Rgb8, &src[..8], 3, 6, 3, &mut dst).is_err());
        assert!(decode_rect(Layout::Rgb8, &src, 1, 6, 3, &mut dst).is_err());
        assert!(decode_rect(Layout::Rgb8, &src, 3, 6, 3, &mut dst[..10]).is_err());
    }
    /// Throughput of the decoder alone, over 5 MB of arbitrary blocks: the size
    /// of the burst a home-page load submits at once. Ignored because it is a
    /// measurement, not a check; run it with `--ignored --nocapture` in release.
    #[test]
    #[ignore]
    fn decode_throughput() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let src: Vec<u8> = (0..5 << 20)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        for layout in [Layout::Rgb8, Layout::Rgba8, Layout::Rg11] {
            let bb = layout.block_bytes();
            // Blocks to a side, so the rectangle uses the whole 5 MB.
            let blocks = ((src.len() / bb) as f64).sqrt() as usize;
            let mut dst = vec![1u8; blocks * 4 * blocks * 4 * layout.texel_bytes()];
            let mut best = f64::MAX;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                decode_rect(layout, &src, blocks, blocks * 4, blocks * 4, &mut dst).unwrap();
                best = best.min(t.elapsed().as_secs_f64());
            }
            let mb = (blocks * blocks * bb) as f64 / 1_048_576.0;
            println!("{layout:?}: {mb:.1} MB of blocks in {:.1} ms = {:.0} MB/s", best * 1e3, mb / best);
        }
    }

}
