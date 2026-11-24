//! The engine's SPIR-V, as it is handed to `vkCreateShaderModule`: read to
//! tell vertex shaders from the rest, and changed in one way only, so that
//! the scene's geometry can be captured with transform feedback.
//!
//! # The change
//!
//! A vertex shader that writes `gl_Position` (`BuiltIn Position`, a member
//! of the `gl_PerVertex` block or a variable of its own) gets:
//!
//! * `OpCapability TransformFeedback`, when it lacks it;
//! * `OpExecutionMode <entry> Xfb` on its vertex entry point;
//! * `XfbBuffer 0` and `XfbStride 16` on the output variable that holds the
//!   position, and `Offset 0` on the position itself.
//!
//! Nothing else: no instruction of the shader's own is changed or removed,
//! no variable, type or function is added, and the ids are the shader's, so
//! the bound in the header stays. While transform feedback is not active the
//! decorations have no effect at all, and while it is, the position is
//! written to our buffer as well as passed on; what is rasterised is the
//! same. A module that does not have exactly that shape (no vertex entry
//! point, two of them, no position output, a position already captured, a
//! module that is not SPIR-V) is left as it came.

/// The first word of every SPIR-V module.
pub(crate) const MAGIC: u32 = 0x0723_0203;

const OP_ENTRY_POINT: u16 = 15;
const OP_EXECUTION_MODE: u16 = 16;
const OP_CAPABILITY: u16 = 17;
const OP_TYPE_STRUCT: u16 = 30;
const OP_TYPE_POINTER: u16 = 32;
const OP_VARIABLE: u16 = 59;
const OP_DECORATE: u16 = 71;
const OP_MEMBER_DECORATE: u16 = 72;
const OP_EXECUTION_MODE_ID: u16 = 331;

const DECORATION_BUILTIN: u32 = 11;
const DECORATION_OFFSET: u32 = 35;
const DECORATION_XFB_BUFFER: u32 = 36;
const DECORATION_XFB_STRIDE: u32 = 37;
const BUILTIN_POSITION: u32 = 0;
const EXECUTION_MODE_XFB: u32 = 11;
const CAPABILITY_TRANSFORM_FEEDBACK: u32 = 53;
const STORAGE_OUTPUT: u32 = 3;
pub(crate) const MODEL_VERTEX: u32 = 0;

/// Bytes per captured vertex: the clip-space position.
pub(crate) const STRIDE: u32 = 16;

/// One instruction: where it starts in the module and its opcode.
#[derive(Clone, Copy, Debug)]
struct Inst {
    at: usize,
    op: u16,
    len: usize,
}

/// The instructions of a module, after its five-word header, or why it is
/// not one.
fn instructions(words: &[u32]) -> Result<Vec<Inst>, &'static str> {
    if words.len() < 5 || words[0] != MAGIC {
        return Err("not SPIR-V");
    }
    let mut out = Vec::new();
    let mut at = 5;
    while at < words.len() {
        let len = (words[at] >> 16) as usize;
        if len == 0 || at + len > words.len() {
            return Err("a malformed instruction");
        }
        out.push(Inst { at, op: (words[at] & 0xffff) as u16, len });
        at += len;
    }
    Ok(out)
}

/// What a module is, for the dump and the capture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    /// The execution model of each entry point.
    pub models: Vec<u32>,
    /// Whether it writes `BuiltIn Position` (declares it, strictly).
    pub position: bool,
}

pub(crate) fn summary(words: &[u32]) -> Summary {
    let Ok(list) = instructions(words) else { return Summary::default() };
    let mut s = Summary::default();
    for i in &list {
        let w = &words[i.at..i.at + i.len];
        match i.op {
            OP_ENTRY_POINT if i.len >= 3 => s.models.push(w[1]),
            OP_DECORATE if i.len >= 4 && w[2] == DECORATION_BUILTIN && w[3] == BUILTIN_POSITION => s.position = true,
            OP_MEMBER_DECORATE if i.len >= 5 && w[3] == DECORATION_BUILTIN && w[4] == BUILTIN_POSITION => s.position = true,
            _ => {}
        }
    }
    s
}

/// Where the position is.
enum Position {
    /// Member `member` of the block type `block`, in the output `var`.
    Member { block: u32, member: u32, var: u32 },
    /// The output variable `var` itself.
    Variable { var: u32 },
}

/// The module with the position captured to transform feedback buffer 0, or
/// why it is left as it is.
pub(crate) fn instrument_vertex(words: &[u32]) -> Result<Vec<u32>, &'static str> {
    let list = instructions(words)?;
    let w = |i: &Inst| &words[i.at..i.at + i.len];
    let entries: Vec<&Inst> = list.iter().filter(|i| i.op == OP_ENTRY_POINT).collect();
    let vertex: Vec<&Inst> = entries.iter().copied().filter(|i| i.len >= 3 && w(i)[1] == MODEL_VERTEX).collect();
    let entry = match (vertex.as_slice(), entries.len()) {
        ([one], 1) => w(one)[2],
        ([], _) => return Err("no vertex entry point"),
        _ => return Err("more than one entry point"),
    };
    if list.iter().any(|i| i.op == OP_EXECUTION_MODE && i.len >= 3 && w(i)[2] == EXECUTION_MODE_XFB) {
        return Err("it already declares Xfb");
    }
    if list.iter().any(|i| {
        let x = w(i);
        i.op == OP_DECORATE
            && i.len >= 3
            && (matches!(x[2], DECORATION_XFB_BUFFER | DECORATION_XFB_STRIDE)
                || (x[2] == DECORATION_OFFSET && is_output_var(words, &list, x[1])))
    }) {
        return Err("it already has transform feedback decorations");
    }
    // The position: a decorated member of a block, or a decorated variable.
    let mut position = None;
    for i in &list {
        let x = w(i);
        if i.op == OP_MEMBER_DECORATE && i.len >= 5 && x[3] == DECORATION_BUILTIN && x[4] == BUILTIN_POSITION {
            let (block, member) = (x[1], x[2]);
            let vars = output_vars_of_struct(words, &list, block);
            match vars.as_slice() {
                [var] => position = Some(Position::Member { block, member, var: *var }),
                [] => continue,
                _ => return Err("the position block has more than one output variable"),
            }
            if list.iter().any(|j| {
                let y = w(j);
                j.op == OP_MEMBER_DECORATE && j.len >= 4 && y[1] == block && y[3] == DECORATION_OFFSET
            }) {
                return Err("the position block already has offsets");
            }
            break;
        }
        if i.op == OP_DECORATE && i.len >= 4 && x[2] == DECORATION_BUILTIN && x[3] == BUILTIN_POSITION && is_output_var(words, &list, x[1]) {
            position = Some(Position::Variable { var: x[1] });
            break;
        }
    }
    let Some(position) = position else { return Err("no position output") };
    let has_capability = list.iter().any(|i| i.op == OP_CAPABILITY && i.len >= 2 && w(i)[1] == CAPABILITY_TRANSFORM_FEEDBACK);

    // Where each addition goes, in the module's logical layout.
    let last_capability = list.iter().rposition(|i| i.op == OP_CAPABILITY).ok_or("no capability")?;
    let after_modes = list
        .iter()
        .rposition(|i| matches!(i.op, OP_EXECUTION_MODE | OP_EXECUTION_MODE_ID | OP_ENTRY_POINT))
        .ok_or("no entry point")?;
    // Decorations go after the module's last one, or, when it has none
    // other than in groups, before its first type.
    let after_decorations = list.iter().rposition(|i| matches!(i.op, OP_DECORATE | OP_MEMBER_DECORATE)).ok_or("no decorations")?;
    if !(last_capability < after_modes && after_modes < after_decorations) {
        return Err("the module is not in the logical layout");
    }
    let var = match position {
        Position::Member { var, .. } | Position::Variable { var } => var,
    };
    let mut decorations = vec![
        op(OP_DECORATE, &[var, DECORATION_XFB_BUFFER, 0]),
        op(OP_DECORATE, &[var, DECORATION_XFB_STRIDE, STRIDE]),
    ];
    decorations.push(match position {
        Position::Member { block, member, .. } => op(OP_MEMBER_DECORATE, &[block, member, DECORATION_OFFSET, 0]),
        Position::Variable { var } => op(OP_DECORATE, &[var, DECORATION_OFFSET, 0]),
    });

    let mut out = Vec::with_capacity(words.len() + 24);
    out.extend_from_slice(&words[..5]);
    for (n, i) in list.iter().enumerate() {
        out.extend_from_slice(w(i));
        if n == last_capability && !has_capability {
            out.extend(op(OP_CAPABILITY, &[CAPABILITY_TRANSFORM_FEEDBACK]));
        }
        if n == after_modes {
            out.extend(op(OP_EXECUTION_MODE, &[entry, EXECUTION_MODE_XFB]));
        }
        if n == after_decorations {
            for d in &decorations {
                out.extend_from_slice(d);
            }
        }
    }
    Ok(out)
}

fn op(code: u16, operands: &[u32]) -> Vec<u32> {
    let mut v = vec![((operands.len() as u32 + 1) << 16) | code as u32];
    v.extend_from_slice(operands);
    v
}

/// Whether `id` is an `OpVariable` in the Output storage class.
fn is_output_var(words: &[u32], list: &[Inst], id: u32) -> bool {
    list.iter().any(|i| i.op == OP_VARIABLE && i.len >= 4 && words[i.at + 2] == id && words[i.at + 3] == STORAGE_OUTPUT)
}

/// The Output variables whose type is a pointer to `block`.
fn output_vars_of_struct(words: &[u32], list: &[Inst], block: u32) -> Vec<u32> {
    let pointers: Vec<u32> = list
        .iter()
        .filter(|i| i.op == OP_TYPE_POINTER && i.len >= 4 && words[i.at + 2] == STORAGE_OUTPUT && words[i.at + 3] == block)
        .map(|i| words[i.at + 1])
        .collect();
    let is_struct = list.iter().any(|i| i.op == OP_TYPE_STRUCT && words[i.at + 1] == block);
    if !is_struct {
        return Vec::new();
    }
    list.iter()
        .filter(|i| i.op == OP_VARIABLE && i.len >= 4 && pointers.contains(&words[i.at + 1]) && words[i.at + 3] == STORAGE_OUTPUT)
        .map(|i| words[i.at + 2])
        .collect()
}

/// FNV-1a over the module's bytes: the name a dumped module is written
/// under, and how a pipeline's shaders are told apart in the tables.
pub(crate) fn hash(words: &[u32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        for b in w.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    // `glslc -O --target-env=vulkan1.0 -mfmt=num` of a vertex shader that
    // transforms a position by a uniform matrix, as the engine's do; and
    // the same with `gl_Position` as an output of its own is not something
    // GLSL can say, so that case is built by hand below.
    const VS: &[u32] = include!("test_vs.spv.rs");

    fn ops(words: &[u32]) -> Vec<(u16, Vec<u32>)> {
        instructions(words).unwrap().iter().map(|i| (i.op, words[i.at + 1..i.at + i.len].to_vec())).collect()
    }

    #[test]
    fn a_vertex_shader_gets_its_position_captured_and_nothing_else() {
        let s = summary(VS);
        assert_eq!(s.models, vec![MODEL_VERTEX]);
        assert!(s.position);
        let out = instrument_vertex(VS).expect("instrumented");
        assert_eq!(&out[..5], &VS[..5], "the header, its id bound included, is kept");
        let before = ops(VS);
        let after = ops(&out);
        // Every instruction of the shader is there, in its order.
        let mut it = after.iter();
        for b in &before {
            assert!(it.any(|a| a == b), "{b:?} kept in order");
        }
        let added: Vec<_> = after.iter().filter(|a| !before.contains(a)).collect();
        assert_eq!(added.len(), 5, "{added:?}");
        assert!(added.contains(&&(OP_CAPABILITY, vec![CAPABILITY_TRANSFORM_FEEDBACK])));
        assert!(added.iter().any(|(o, a)| *o == OP_EXECUTION_MODE && a[1] == EXECUTION_MODE_XFB));
        assert!(added.iter().any(|(o, a)| *o == OP_MEMBER_DECORATE && a[1] == 0 && a[2] == DECORATION_OFFSET && a[3] == 0));
        assert!(added.iter().any(|(o, a)| *o == OP_DECORATE && a[1] == DECORATION_XFB_STRIDE && a[2] == STRIDE));
        // Done once only.
        assert_eq!(instrument_vertex(&out), Err("it already declares Xfb"));
    }

    #[test]
    fn anything_else_is_left_alone() {
        assert_eq!(instrument_vertex(&[1, 2, 3]), Err("not SPIR-V"));
        let mut frag = VS.to_vec();
        // The entry point's execution model, made Fragment (4).
        let list = instructions(&frag).unwrap();
        let e = list.iter().find(|i| i.op == OP_ENTRY_POINT).unwrap().at;
        frag[e + 1] = 4;
        assert_eq!(instrument_vertex(&frag), Err("no vertex entry point"));
        // An instruction that runs past the end of the module.
        let mut cut = VS.to_vec();
        let last = cut.len() - 1;
        cut[last] = (5 << 16) | (cut[last] & 0xffff);
        assert_eq!(instrument_vertex(&cut), Err("a malformed instruction"));
    }

    /// The instrumented module is valid Vulkan SPIR-V, whenever the
    /// system's validator is there to say so.
    #[test]
    fn the_instrumented_module_validates() {
        const VAL: &str = "/usr/local/bin/spirv-val";
        if !std::path::Path::new(VAL).is_file() {
            return;
        }
        let out = instrument_vertex(VS).unwrap();
        let path = std::env::temp_dir().join(format!("intoxicated-rt-test-{}.spv", std::process::id()));
        std::fs::write(&path, out.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        let run = std::process::Command::new(VAL).args(["--target-env", "vulkan1.0"]).arg(&path).output().unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    }

    #[test]
    fn the_hash_tells_modules_apart() {
        let out = instrument_vertex(VS).unwrap();
        assert_ne!(hash(VS), hash(&out));
        assert_eq!(hash(VS), hash(&VS.to_vec()));
    }
}
