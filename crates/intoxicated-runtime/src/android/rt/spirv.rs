//! The engine's SPIR-V, as it is handed to `vkCreateShaderModule`: read to
//! tell vertex shaders from the rest, and changed in one way only, so that
//! the scene's geometry and its base colour can be captured with transform
//! feedback.
//!
//! # The change
//!
//! A vertex shader that writes `gl_Position` (`BuiltIn Position`, a member
//! of the `gl_PerVertex` block or a variable of its own) gets:
//!
//! * `OpCapability TransformFeedback`, when it lacks it;
//! * `OpExecutionMode <entry> Xfb` on its vertex entry point;
//! * `XfbBuffer 0` and `XfbStride 32` on the output variable that holds the
//!   position, and `Offset 0` on the position itself;
//! * `XfbBuffer 0`, `XfbStride 32` and `Offset 16` on its colour output.
//!
//! The colour output is the `vec4` output, of the lowest location, whose
//! value comes from the vertex input at location 2: that is where the
//! engine's shaders take their vertex colour, and what they pass on as the
//! colour (times the colour of the instance or the bone, for the shaders
//! that have one). A shader without such an output gets one of ours: a
//! `vec4` output at a location it does not use, set to (-1, -1, -1, -1)
//! before each return of its entry point, so that what is captured for it
//! says "no colour"; the fragment shader never reads it. That is the only
//! case in which ids, a variable and instructions are added; nothing of the
//! shader's own is changed or removed in any case. While transform feedback
//! is not active the decorations have no effect at all, and while it is,
//! the position and colour are written to our buffer as well as passed on;
//! what is rasterised is the same. A module that does not have exactly that
//! shape (no vertex entry point, two of them, no position output, a
//! position already captured, no room for our output, a module that is not
//! SPIR-V) is left as it came.

/// The first word of every SPIR-V module.
pub(crate) const MAGIC: u32 = 0x0723_0203;

const OP_EXT_INST: u16 = 12;
const OP_ENTRY_POINT: u16 = 15;
const OP_EXECUTION_MODE: u16 = 16;
const OP_CAPABILITY: u16 = 17;
const OP_TYPE_FLOAT: u16 = 22;
const OP_TYPE_VECTOR: u16 = 23;
const OP_TYPE_MATRIX: u16 = 24;
const OP_TYPE_ARRAY: u16 = 28;
const OP_TYPE_STRUCT: u16 = 30;
const OP_TYPE_POINTER: u16 = 32;
const OP_CONSTANT: u16 = 43;
const OP_CONSTANT_COMPOSITE: u16 = 44;
const OP_FUNCTION: u16 = 54;
const OP_FUNCTION_END: u16 = 56;
const OP_VARIABLE: u16 = 59;
const OP_LOAD: u16 = 61;
const OP_STORE: u16 = 62;
const OP_ACCESS_CHAIN: u16 = 65;
const OP_IN_BOUNDS_ACCESS_CHAIN: u16 = 66;
const OP_PTR_ACCESS_CHAIN: u16 = 67;
const OP_VECTOR_SHUFFLE: u16 = 79;
const OP_COMPOSITE_EXTRACT: u16 = 81;
const OP_COMPOSITE_INSERT: u16 = 82;
const OP_RETURN: u16 = 253;
const OP_DECORATE: u16 = 71;
const OP_MEMBER_DECORATE: u16 = 72;
const OP_EXECUTION_MODE_ID: u16 = 331;

const DECORATION_BUILTIN: u32 = 11;
const DECORATION_LOCATION: u32 = 30;
const DECORATION_OFFSET: u32 = 35;
const DECORATION_XFB_BUFFER: u32 = 36;
const DECORATION_XFB_STRIDE: u32 = 37;
const BUILTIN_POSITION: u32 = 0;
const EXECUTION_MODE_XFB: u32 = 11;
const CAPABILITY_TRANSFORM_FEEDBACK: u32 = 53;
const STORAGE_INPUT: u32 = 1;
const STORAGE_OUTPUT: u32 = 3;
pub(crate) const MODEL_VERTEX: u32 = 0;

/// Bytes per captured vertex: the clip-space position, then the colour.
pub(crate) const STRIDE: u32 = 32;
/// Where the colour is in a captured vertex.
pub(crate) const COLOUR_OFFSET: u32 = 16;
/// The vertex input the engine's shaders take their vertex colour from.
const COLOUR_INPUT_LOCATION: u32 = 2;
/// Output locations a vertex shader can use on the GPUs this runs on
/// (`maxVertexOutputComponents` 128 on NVIDIA).
const OUTPUT_LOCATIONS: u32 = 32;

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

    // The colour: the shader's own, or ours.
    let mut bound = words[3];
    let mut globals: Vec<Vec<u32>> = Vec::new();
    let mut ours = None;
    let colour = match colour_output(words, &list) {
        Some(c) => c,
        None => {
            let (float, vec4) = vec4_type(words, &list).ok_or("no vec4 type")?;
            let location = free_output_location(words, &list).ok_or("no free output location for the colour")?;
            let mut new_id = || {
                bound += 1;
                bound - 1
            };
            let pointer = match list.iter().find(|i| {
                let x = w(i);
                i.op == OP_TYPE_POINTER && i.len >= 4 && x[2] == STORAGE_OUTPUT && x[3] == vec4
            }) {
                Some(i) => w(i)[1],
                None => {
                    let id = new_id();
                    globals.push(op(OP_TYPE_POINTER, &[id, STORAGE_OUTPUT, vec4]));
                    id
                }
            };
            let minus_one = new_id();
            globals.push(op(OP_CONSTANT, &[float, minus_one, (-1.0f32).to_bits()]));
            let none = new_id();
            globals.push(op(OP_CONSTANT_COMPOSITE, &[vec4, none, minus_one, minus_one, minus_one, minus_one]));
            let var = new_id();
            globals.push(op(OP_VARIABLE, &[pointer, var, STORAGE_OUTPUT]));
            decorations.push(op(OP_DECORATE, &[var, DECORATION_LOCATION, location]));
            ours = Some((var, none));
            var
        }
    };
    decorations.push(op(OP_DECORATE, &[colour, DECORATION_XFB_BUFFER, 0]));
    decorations.push(op(OP_DECORATE, &[colour, DECORATION_XFB_STRIDE, STRIDE]));
    decorations.push(op(OP_DECORATE, &[colour, DECORATION_OFFSET, COLOUR_OFFSET]));
    let first_function = list.iter().position(|i| i.op == OP_FUNCTION).ok_or("no function")?;
    if first_function <= after_decorations {
        return Err("the module is not in the logical layout");
    }

    let mut out = Vec::with_capacity(words.len() + 64);
    out.extend_from_slice(&words[..5]);
    out[3] = bound;
    let mut in_entry = false;
    for (n, i) in list.iter().enumerate() {
        if n == first_function {
            for g in &globals {
                out.extend_from_slice(g);
            }
        }
        match (i.op, ours) {
            (OP_ENTRY_POINT, Some((var, _))) if w(i)[2] == entry => {
                // Our output joins the entry point's interface.
                let mut e = w(i).to_vec();
                e.push(var);
                e[0] = ((e.len() as u32) << 16) | OP_ENTRY_POINT as u32;
                out.extend(e);
            }
            (OP_RETURN, Some((var, none))) if in_entry => {
                out.extend(op(OP_STORE, &[var, none]));
                out.extend_from_slice(w(i));
            }
            _ => out.extend_from_slice(w(i)),
        }
        if i.op == OP_FUNCTION {
            in_entry = i.len >= 3 && w(i)[2] == entry;
        } else if i.op == OP_FUNCTION_END {
            in_entry = false;
        }
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

/// Whether the module has a colour output of its own to capture (see the
/// module doc); without one, an instrumented module captures "no colour".
pub(crate) fn has_colour(words: &[u32]) -> bool {
    instructions(words).is_ok_and(|list| colour_output(words, &list).is_some())
}

/// The ids of the 32-bit float type and of its 4-vector.
fn vec4_type(words: &[u32], list: &[Inst]) -> Option<(u32, u32)> {
    let float = list.iter().find(|i| i.op == OP_TYPE_FLOAT && i.len >= 3 && words[i.at + 2] == 32).map(|i| words[i.at + 1])?;
    let vec4 = list
        .iter()
        .find(|i| i.op == OP_TYPE_VECTOR && i.len >= 4 && words[i.at + 2] == float && words[i.at + 3] == 4)
        .map(|i| words[i.at + 1])?;
    Some((float, vec4))
}

/// The location of each decorated variable.
fn locations(words: &[u32], list: &[Inst]) -> std::collections::HashMap<u32, u32> {
    list.iter()
        .filter(|i| i.op == OP_DECORATE && i.len >= 4 && words[i.at + 2] == DECORATION_LOCATION)
        .map(|i| (words[i.at + 1], words[i.at + 3]))
        .collect()
}

/// The Output and Input variables: (variable, pointee type, storage class).
fn interface_vars(words: &[u32], list: &[Inst]) -> Vec<(u32, u32, u32)> {
    let pointee: std::collections::HashMap<u32, u32> = list
        .iter()
        .filter(|i| i.op == OP_TYPE_POINTER && i.len >= 4)
        .map(|i| (words[i.at + 1], words[i.at + 3]))
        .collect();
    list.iter()
        .filter(|i| i.op == OP_VARIABLE && i.len >= 4 && matches!(words[i.at + 3], STORAGE_INPUT | STORAGE_OUTPUT))
        .filter_map(|i| Some((words[i.at + 2], *pointee.get(&words[i.at + 1])?, words[i.at + 3])))
        .collect()
}

/// A location after every one the shader's outputs use.
fn free_output_location(words: &[u32], list: &[Inst]) -> Option<u32> {
    let loc = locations(words, list);
    let slots = |ty: u32| -> u32 {
        let Some(i) = list.iter().find(|i| matches!(i.op, OP_TYPE_ARRAY | OP_TYPE_MATRIX) && words[i.at + 1] == ty) else { return 1 };
        if i.op == OP_TYPE_MATRIX {
            return words[i.at + 3];
        }
        // The array's length, a constant; an unknown one counts as many.
        let len = words[i.at + 3];
        list.iter()
            .find(|c| c.op == OP_CONSTANT && c.len >= 4 && words[c.at + 2] == len)
            .map_or(OUTPUT_LOCATIONS, |c| words[c.at + 3])
    };
    let end = interface_vars(words, list)
        .into_iter()
        .filter(|&(_, _, class)| class == STORAGE_OUTPUT)
        .filter_map(|(var, ty, _)| Some(loc.get(&var)? + slots(ty)))
        .max()
        .unwrap_or(0);
    (end < OUTPUT_LOCATIONS).then_some(end)
}

/// A value an instruction depends on: another result, or what was stored
/// in a variable.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Source {
    Value(u32),
    Memory(u32),
}

/// The shader's colour output: the `vec4` float Output variable (not a
/// built-in, not in a block) of the lowest location whose stored value
/// depends on the vertex input at [`COLOUR_INPUT_LOCATION`]. The
/// dependencies are followed through every function's instructions,
/// through variables by what was stored in them, over-approximating where
/// an instruction's operands are not known (an unknown literal taken for an
/// id at worst adds a dependency).
fn colour_output(words: &[u32], list: &[Inst]) -> Option<u32> {
    use std::collections::{HashMap, HashSet};
    let w = |i: &Inst| &words[i.at..i.at + i.len];
    let (_, vec4) = vec4_type(words, list)?;
    let loc = locations(words, list);
    let builtins: HashSet<u32> =
        list.iter().filter(|i| i.op == OP_DECORATE && i.len >= 3 && w(i)[2] == DECORATION_BUILTIN).map(|i| w(i)[1]).collect();
    let vars = interface_vars(words, list);
    let inputs: HashSet<u32> = vars
        .iter()
        .filter(|&&(v, _, class)| class == STORAGE_INPUT && loc.get(&v) == Some(&COLOUR_INPUT_LOCATION))
        .map(|&(v, _, _)| v)
        .collect();
    if inputs.is_empty() {
        return None;
    }
    let mut outputs: Vec<(u32, u32)> = vars
        .iter()
        .filter(|&&(v, ty, class)| class == STORAGE_OUTPUT && ty == vec4 && !builtins.contains(&v))
        .filter_map(|&(v, _, _)| Some((*loc.get(&v)?, v)))
        .collect();
    outputs.sort_unstable();
    let mut deps: HashMap<u32, Vec<Source>> = HashMap::new();
    let mut stores: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut root: HashMap<u32, u32> = HashMap::new();
    let mut in_function = false;
    for i in list {
        let x = w(i);
        match i.op {
            OP_FUNCTION => {
                in_function = true;
                continue;
            }
            OP_FUNCTION_END => {
                in_function = false;
                continue;
            }
            _ if !in_function => continue,
            OP_STORE if i.len >= 3 => {
                let r = *root.get(&x[1]).unwrap_or(&x[1]);
                stores.entry(r).or_default().push(x[2]);
                continue;
            }
            OP_LOAD if i.len >= 4 => {
                let r = *root.get(&x[3]).unwrap_or(&x[3]);
                deps.insert(x[2], vec![Source::Memory(r)]);
                continue;
            }
            OP_ACCESS_CHAIN | OP_IN_BOUNDS_ACCESS_CHAIN | OP_PTR_ACCESS_CHAIN if i.len >= 4 => {
                let r = *root.get(&x[3]).unwrap_or(&x[3]);
                root.insert(x[2], r);
                deps.insert(x[2], x[4..].iter().map(|&v| Source::Value(v)).collect());
                continue;
            }
            // No result: stores, copies, branches, merges, returns, kills,
            // barriers, image and atomic writes, line markers; and labels.
            62..=64 | 245..=255 | 8 | 317 | 224 | 225 | 99 | 228 | 55 => continue,
            _ => {}
        }
        if i.len < 3 {
            continue;
        }
        let operands = &x[3..];
        let ids: &[u32] = match i.op {
            OP_COMPOSITE_EXTRACT => &operands[..operands.len().min(1)],
            OP_COMPOSITE_INSERT | OP_VECTOR_SHUFFLE => &operands[..operands.len().min(2)],
            OP_EXT_INST => operands.get(2..).unwrap_or(&[]),
            _ => operands,
        };
        deps.insert(x[2], ids.iter().map(|&v| Source::Value(v)).collect());
    }
    // Whether a stored value reaches one of the inputs.
    let reaches = |start: u32| {
        let mut seen = HashSet::new();
        let mut todo = vec![Source::Value(start)];
        while let Some(s) = todo.pop() {
            if !seen.insert(s) {
                continue;
            }
            match s {
                Source::Memory(v) if inputs.contains(&v) => return true,
                Source::Memory(v) => todo.extend(stores.get(&v).into_iter().flatten().map(|&x| Source::Value(x))),
                Source::Value(v) => todo.extend(deps.get(&v).into_iter().flatten().copied()),
            }
        }
        false
    };
    outputs.into_iter().find(|&(_, v)| stores.get(&v).is_some_and(|s| s.iter().any(|&x| reaches(x)))).map(|(_, v)| v)
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
        assert_eq!(added.len(), 8, "{added:?}");
        // The colour is the shader's own `color`, at offset 16.
        let color = output_at(&out, 1);
        assert!(added.contains(&&(OP_DECORATE, vec![color, DECORATION_OFFSET, COLOUR_OFFSET])));
        assert!(added.contains(&&(OP_CAPABILITY, vec![CAPABILITY_TRANSFORM_FEEDBACK])));
        assert!(added.iter().any(|(o, a)| *o == OP_EXECUTION_MODE && a[1] == EXECUTION_MODE_XFB));
        assert!(added.iter().any(|(o, a)| *o == OP_MEMBER_DECORATE && a[1] == 0 && a[2] == DECORATION_OFFSET && a[3] == 0));
        assert!(added.iter().any(|(o, a)| *o == OP_DECORATE && a[1] == DECORATION_XFB_STRIDE && a[2] == STRIDE));
        // Done once only.
        assert_eq!(instrument_vertex(&out), Err("it already declares Xfb"));
    }

    /// The Output variable at `location`.
    fn output_at(words: &[u32], location: u32) -> u32 {
        let list = instructions(words).unwrap();
        let loc = locations(words, &list);
        interface_vars(words, &list)
            .into_iter()
            .find(|&(v, _, class)| class == STORAGE_OUTPUT && loc.get(&v) == Some(&location))
            .unwrap()
            .0
    }

    /// The test shader with its input at location 2 moved to 7, so nothing
    /// is its colour.
    fn without_colour() -> Vec<u32> {
        let mut words = VS.to_vec();
        let list = instructions(&words).unwrap();
        let inputs: Vec<u32> = interface_vars(&words, &list).into_iter().filter(|v| v.2 == STORAGE_INPUT).map(|v| v.0).collect();
        for i in &list {
            let at = i.at;
            if i.op == OP_DECORATE && words[at + 2] == DECORATION_LOCATION && words[at + 3] == 2 && inputs.contains(&words[at + 1]) {
                words[at + 3] = 7;
            }
        }
        words
    }

    #[test]
    fn a_shader_without_a_colour_gets_an_output_that_says_so() {
        let words = without_colour();
        let list = instructions(&words).unwrap();
        assert_eq!(colour_output(&words, &list), None);
        let out = instrument_vertex(&words).expect("instrumented");
        assert!(out[3] > words[3], "ids were added");
        // Our output, after the shader's own (locations 0 and 1).
        let ours = output_at(&out, 2);
        let after = ops(&out);
        assert!(after.contains(&(OP_DECORATE, vec![ours, DECORATION_OFFSET, COLOUR_OFFSET])));
        assert!(after.iter().any(|(o, a)| *o == OP_ENTRY_POINT && a.last() == Some(&ours)));
        assert!(after.iter().any(|(o, a)| *o == OP_STORE && a[0] == ours));
        let before = ops(&words);
        let mut it = after.iter();
        for b in before.iter().filter(|(o, _)| *o != OP_ENTRY_POINT) {
            assert!(it.any(|a| a == b), "{b:?} kept in order");
        }
        validate(&out);
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
        validate(&instrument_vertex(VS).unwrap());
    }

    fn validate(out: &[u32]) {
        const VAL: &str = "/usr/local/bin/spirv-val";
        if !std::path::Path::new(VAL).is_file() {
            return;
        }
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("intoxicated-rt-test-{}-{n}.spv", std::process::id()));
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
