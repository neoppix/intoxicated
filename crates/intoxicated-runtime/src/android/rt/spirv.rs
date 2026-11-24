//! The engine's SPIR-V, as it is handed to `vkCreateShaderModule`: read to
//! tell vertex shaders from the rest, and changed in one way only, so that
//! the scene's geometry, its base colour and its place in the world can be
//! captured with transform feedback.
//!
//! # The change
//!
//! A vertex shader that writes `gl_Position` (`BuiltIn Position`, a member
//! of the `gl_PerVertex` block or a variable of its own) gets:
//!
//! * `OpCapability TransformFeedback`, when it lacks it;
//! * `OpExecutionMode <entry> Xfb` on its vertex entry point;
//! * `XfbBuffer 0` and `XfbStride 64` on the output variable that holds the
//!   position, and `Offset 0` on the position itself;
//! * `XfbBuffer 0`, `XfbStride 64` and `Offset 16` on its colour output,
//!   and `Offset 32` on its camera vector output;
//! * an output of ours, `Offset 48`, at a location the shader does not use,
//!   holding the vertex's world position (w: 1 for a static vertex, 0 for
//!   one placed by bones), stored right after the shader computes its
//!   camera vector from it: one constant, one variable and one vector are
//!   added, and the entry point's interface names the variable. The fragment
//!   shader never reads it.
//!
//! The colour output is the `vec4` output, of the lowest location, whose
//! value comes from the vertex input at location 2: that is where the
//! engine's shaders take their vertex colour, and what they pass on as the
//! colour (times the colour of the instance or the bone, for the shaders
//! that have one). The camera vector is the output that holds the camera's
//! position (from a uniform block) minus the vertex's position in the world
//! (from the position input): the engine's lit shaders pass it on for their
//! lighting. With the world position it gives the camera's position, and it
//! ties the clip-space positions to a space with true angles, from which the
//! real projection is worked out (see `build.comp`). A shader without both
//! is taken not to be lit world geometry and is left as it came, so it is
//! never captured: in Timebomb Duels' lobby those are the particle and
//! billboard shaders (camera-facing quads), the sky and a shader that
//! outputs only a position.
//!
//! No instruction of the shader's own is changed or removed. While transform
//! feedback is not active the decorations have no effect at all, and while
//! it is, the outputs are written to our buffer as well as passed on; what
//! is rasterised is the same. A module of any other shape (no vertex entry
//! point, two of them, no position output, a position already captured, no
//! free location, a module that is not SPIR-V) is left as it came too.

use std::collections::{HashMap, HashSet};

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
const OP_FUNCTION: u16 = 54;
const OP_FUNCTION_END: u16 = 56;
const OP_VARIABLE: u16 = 59;
const OP_LOAD: u16 = 61;
const OP_STORE: u16 = 62;
const OP_ACCESS_CHAIN: u16 = 65;
const OP_IN_BOUNDS_ACCESS_CHAIN: u16 = 66;
const OP_PTR_ACCESS_CHAIN: u16 = 67;
const OP_VECTOR_SHUFFLE: u16 = 79;
const OP_COMPOSITE_CONSTRUCT: u16 = 80;
const OP_COMPOSITE_EXTRACT: u16 = 81;
const OP_COMPOSITE_INSERT: u16 = 82;
const OP_F_SUB: u16 = 131;
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

/// Bytes per captured vertex: the clip-space position, the colour, the
/// camera vector, the world position.
pub(crate) const STRIDE: u32 = 64;
/// Where the colour, the camera vector and the world position are in a
/// captured vertex.
pub(crate) const COLOUR_OFFSET: u32 = 16;
pub(crate) const CAMERA_OFFSET: u32 = 32;
pub(crate) const WORLD_OFFSET: u32 = 48;
/// Output locations a vertex shader can use on the GPUs this runs on
/// (`maxVertexOutputComponents` 128 on NVIDIA).
const OUTPUT_LOCATIONS: u32 = 32;
/// The vertex input the engine's shaders take their vertex colour from.
const COLOUR_INPUT_LOCATION: u32 = 2;

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

    // The colour and the camera vector, which lit world geometry has, and
    // an output of ours for the world position.
    let flow = Flow::new(words, &list);
    let colour = colour_output(words, &list, &flow).ok_or("no colour output")?;
    let camera = camera_vector_output(words, &list, &flow).ok_or("no camera vector output (not lit world geometry)")?;
    let (float, _, vec4) = float_types(words, &list).ok_or("no float type")?;
    let vec4 = vec4.ok_or("no vec4 type")?;
    let location = free_output_location(words, &list, &flow).ok_or("no free output location for the world position")?;
    let mut bound = words[3];
    let mut new_id = || {
        bound += 1;
        bound - 1
    };
    let mut globals: Vec<Vec<u32>> = Vec::new();
    let pointer = match list.iter().find(|i| i.op == OP_TYPE_POINTER && i.len >= 4 && w(i)[2] == STORAGE_OUTPUT && w(i)[3] == vec4) {
        Some(i) => w(i)[1],
        None => {
            let id = new_id();
            globals.push(op(OP_TYPE_POINTER, &[id, STORAGE_OUTPUT, vec4]));
            id
        }
    };
    let flag = new_id();
    globals.push(op(OP_CONSTANT, &[float, flag, if camera.fixed { 1.0f32 } else { 0.0f32 }.to_bits()]));
    let world_out = new_id();
    globals.push(op(OP_VARIABLE, &[pointer, world_out, STORAGE_OUTPUT]));
    let world4 = new_id();
    decorations.push(op(OP_DECORATE, &[world_out, DECORATION_LOCATION, location]));
    for (v, offset) in [(colour, COLOUR_OFFSET), (camera.output, CAMERA_OFFSET), (world_out, WORLD_OFFSET)] {
        decorations.push(op(OP_DECORATE, &[v, DECORATION_XFB_BUFFER, 0]));
        decorations.push(op(OP_DECORATE, &[v, DECORATION_XFB_STRIDE, STRIDE]));
        decorations.push(op(OP_DECORATE, &[v, DECORATION_OFFSET, offset]));
    }
    let first_function = list.iter().position(|i| i.op == OP_FUNCTION).ok_or("no function")?;
    if first_function <= after_decorations {
        return Err("the module is not in the logical layout");
    }

    let mut out = Vec::with_capacity(words.len() + 64);
    out.extend_from_slice(&words[..5]);
    out[3] = bound;
    for (n, i) in list.iter().enumerate() {
        if n == first_function {
            for g in &globals {
                out.extend_from_slice(g);
            }
        }
        if i.op == OP_ENTRY_POINT && w(i)[2] == entry {
            // Our output joins the entry point's interface.
            let mut e = w(i).to_vec();
            e.push(world_out);
            e[0] = ((e.len() as u32) << 16) | OP_ENTRY_POINT as u32;
            out.extend(e);
        } else {
            out.extend_from_slice(w(i));
        }
        if i.op == OP_F_SUB && w(i)[2] == camera.difference {
            // Right after the camera vector is computed, in the same block:
            // the world position it was computed from, and whether static.
            out.extend(op(OP_COMPOSITE_CONSTRUCT, &[vec4, world4, camera.world, flag]));
            out.extend(op(OP_STORE, &[world_out, world4]));
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

/// The ids of the 32-bit float type and of its 3- and 4-vectors.
fn float_types(words: &[u32], list: &[Inst]) -> Option<(u32, Option<u32>, Option<u32>)> {
    let float = list.iter().find(|i| i.op == OP_TYPE_FLOAT && i.len >= 3 && words[i.at + 2] == 32).map(|i| words[i.at + 1])?;
    let vec = |n: u32| {
        list.iter()
            .find(|i| i.op == OP_TYPE_VECTOR && i.len >= 4 && words[i.at + 2] == float && words[i.at + 3] == n)
            .map(|i| words[i.at + 1])
    };
    Some((float, vec(3), vec(4)))
}

/// The location of each decorated variable.
fn locations(words: &[u32], list: &[Inst]) -> HashMap<u32, u32> {
    list.iter()
        .filter(|i| i.op == OP_DECORATE && i.len >= 4 && words[i.at + 2] == DECORATION_LOCATION)
        .map(|i| (words[i.at + 1], words[i.at + 3]))
        .collect()
}

/// The Output and Input variables: (variable, pointee type, storage class).
fn interface_vars(words: &[u32], list: &[Inst]) -> Vec<(u32, u32, u32)> {
    let pointee: HashMap<u32, u32> =
        list.iter().filter(|i| i.op == OP_TYPE_POINTER && i.len >= 4).map(|i| (words[i.at + 1], words[i.at + 3])).collect();
    list.iter()
        .filter(|i| i.op == OP_VARIABLE && i.len >= 4 && matches!(words[i.at + 3], STORAGE_INPUT | STORAGE_OUTPUT))
        .filter_map(|i| Some((words[i.at + 2], *pointee.get(&words[i.at + 1])?, words[i.at + 3])))
        .collect()
}

/// A value an instruction depends on: another result, or what was stored
/// in a variable.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Source {
    Value(u32),
    Memory(u32),
}

/// How values flow through a module's functions, over-approximated where an
/// instruction's operands are not known (an unknown literal taken for an id
/// at worst adds a dependency).
struct Flow {
    /// Each result: its opcode and its operands after the result id.
    defs: HashMap<u32, (u16, Vec<u32>)>,
    deps: HashMap<u32, Vec<Source>>,
    /// The values stored into each variable, through any access chain.
    stores: HashMap<u32, Vec<u32>>,
    /// Interface variables by (location, storage class), and their types.
    vars: Vec<(u32, u32, u32)>,
    loc: HashMap<u32, u32>,
    builtins: HashSet<u32>,
}

impl Flow {
    fn new(words: &[u32], list: &[Inst]) -> Flow {
        let w = |i: &Inst| &words[i.at..i.at + i.len];
        let mut f = Flow {
            defs: HashMap::new(),
            deps: HashMap::new(),
            stores: HashMap::new(),
            vars: interface_vars(words, list),
            loc: locations(words, list),
            builtins: list.iter().filter(|i| i.op == OP_DECORATE && i.len >= 3 && w(i)[2] == DECORATION_BUILTIN).map(|i| w(i)[1]).collect(),
        };
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
                    f.stores.entry(r).or_default().push(x[2]);
                    continue;
                }
                OP_LOAD if i.len >= 4 => {
                    // What was stored there, and the pointer itself: an
                    // access chain's indices (a bone index picking a
                    // matrix) are what the loaded value depends on too.
                    let r = *root.get(&x[3]).unwrap_or(&x[3]);
                    f.deps.insert(x[2], vec![Source::Memory(r), Source::Value(x[3])]);
                    f.defs.insert(x[2], (i.op, x[3..].to_vec()));
                    continue;
                }
                OP_ACCESS_CHAIN | OP_IN_BOUNDS_ACCESS_CHAIN | OP_PTR_ACCESS_CHAIN if i.len >= 4 => {
                    let r = *root.get(&x[3]).unwrap_or(&x[3]);
                    root.insert(x[2], r);
                    f.deps.insert(x[2], x[4..].iter().map(|&v| Source::Value(v)).collect());
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
            f.deps.insert(x[2], ids.iter().map(|&v| Source::Value(v)).collect());
            f.defs.insert(x[2], (i.op, operands.to_vec()));
        }
        f
    }

    /// The variables whose stored values `start` depends on.
    fn memories(&self, start: u32) -> HashSet<u32> {
        let mut seen = HashSet::new();
        let mut out = HashSet::new();
        let mut todo = vec![Source::Value(start)];
        while let Some(s) = todo.pop() {
            if !seen.insert(s) {
                continue;
            }
            match s {
                Source::Memory(v) => {
                    out.insert(v);
                    todo.extend(self.stores.get(&v).into_iter().flatten().map(|&x| Source::Value(x)));
                }
                Source::Value(v) => todo.extend(self.deps.get(&v).into_iter().flatten().copied()),
            }
        }
        out
    }

    fn inputs_at(&self, location: u32) -> HashSet<u32> {
        self.vars
            .iter()
            .filter(|&&(v, _, class)| class == STORAGE_INPUT && self.loc.get(&v) == Some(&location))
            .map(|&(v, _, _)| v)
            .collect()
    }

    /// The Output variables of type `ty` (not built-ins), by location.
    fn outputs_of(&self, ty: Option<u32>) -> Vec<(u32, u32)> {
        let mut o: Vec<(u32, u32)> = self
            .vars
            .iter()
            .filter(|&&(v, t, class)| class == STORAGE_OUTPUT && Some(t) == ty && !self.builtins.contains(&v))
            .filter_map(|&(v, _, _)| Some((*self.loc.get(&v)?, v)))
            .collect();
        o.sort_unstable();
        o
    }
}

/// The shader's colour output: the `vec4` float Output variable (not a
/// built-in, not in a block) of the lowest location whose stored value
/// depends on the vertex input at [`COLOUR_INPUT_LOCATION`].
fn colour_output(words: &[u32], list: &[Inst], flow: &Flow) -> Option<u32> {
    let (_, _, vec4) = float_types(words, list)?;
    let inputs = flow.inputs_at(COLOUR_INPUT_LOCATION);
    if inputs.is_empty() {
        return None;
    }
    flow.outputs_of(vec4)
        .into_iter()
        .find(|&(_, v)| flow.stores.get(&v).is_some_and(|s| s.iter().any(|&x| !flow.memories(x).is_disjoint(&inputs))))
        .map(|(_, v)| v)
}

/// The shader's camera vector output: a `vec3` output that is, or a `vec4`
/// output whose x, y and z are, `a - b` where `a` depends on no vertex
/// input (the camera's position from a uniform block) and `b` on the
/// position input at location 0 (the vertex in the world). The engine's
/// lit shaders pass it on for their lighting; particles, billboards, the
/// sky and the interface have none. Of the lowest location.
fn camera_vector_output(words: &[u32], list: &[Inst], flow: &Flow) -> Option<CameraVector> {
    let (_, vec3, vec4) = float_types(words, list)?;
    let vec3 = vec3?;
    let position = flow.inputs_at(0);
    let any_input: HashSet<u32> = flow.vars.iter().filter(|v| v.2 == STORAGE_INPUT).map(|v| v.0).collect();
    if position.is_empty() {
        return None;
    }
    let is_difference = |id: u32| {
        let Some((op, x)) = flow.defs.get(&id) else { return false };
        // OpFSub: result type was dropped with the result id; the operands.
        *op == OP_F_SUB
            && x.len() == 2
            && list.iter().any(|i| i.op == OP_F_SUB && words[i.at + 2] == id && words[i.at + 1] == vec3)
            && flow.memories(x[0]).is_disjoint(&any_input)
            && !flow.memories(x[1]).is_disjoint(&position)
    };
    // The difference a stored value is, or is made of.
    let difference_of = |stored: u32, four: bool| -> Option<u32> {
        if !four {
            return is_difference(stored).then_some(stored);
        }
        let Some((OP_COMPOSITE_CONSTRUCT, parts)) = flow.defs.get(&stored) else { return None };
        if parts.len() == 2 {
            return is_difference(parts[0]).then_some(parts[0]);
        }
        // Its first three parts the components 0, 1 and 2 of one difference.
        if parts.len() != 4 {
            return None;
        }
        let of = |k: usize| match flow.defs.get(&parts[k]) {
            Some((OP_COMPOSITE_EXTRACT, e)) if e.len() == 2 && e[1] == k as u32 => Some(e[0]),
            _ => None,
        };
        match (of(0), of(1), of(2)) {
            (Some(a), Some(b), Some(c)) if a == b && b == c && is_difference(a) => Some(a),
            _ => None,
        }
    };
    let mut candidates: Vec<(u32, u32, bool)> = flow.outputs_of(Some(vec3)).into_iter().map(|(l, v)| (l, v, false)).collect();
    candidates.extend(flow.outputs_of(vec4).into_iter().map(|(l, v)| (l, v, true)));
    candidates.sort_unstable();
    candidates.into_iter().find_map(|(_, v, four)| {
        let stored = flow.stores.get(&v)?;
        let diffs: Vec<u32> = stored.iter().map(|&x| difference_of(x, four)).collect::<Option<Vec<u32>>>()?;
        let &first = diffs.first()?;
        if diffs.iter().any(|&d| d != first) {
            return None;
        }
        let world = flow.defs.get(&first)?.1[1];
        // Static: the world position depends on no vertex input but the
        // position and built-ins such as the instance index (so not on
        // bone indices or weights).
        let fixed = flow.memories(world).iter().all(|m| !any_input.contains(m) || position.contains(m) || flow.builtins.contains(m));
        Some(CameraVector { output: v, difference: first, world, fixed })
    })
}

/// The camera vector output and what it is made of.
struct CameraVector {
    output: u32,
    /// The `OpFSub` (camera minus world) and its second operand.
    difference: u32,
    world: u32,
    /// The world position depends on the position input alone.
    fixed: bool,
}

/// A location after every one the shader's outputs use.
fn free_output_location(words: &[u32], list: &[Inst], flow: &Flow) -> Option<u32> {
    let slots = |ty: u32| -> u32 {
        let Some(i) = list.iter().find(|i| matches!(i.op, OP_TYPE_ARRAY | OP_TYPE_MATRIX) && words[i.at + 1] == ty) else { return 1 };
        if i.op == OP_TYPE_MATRIX {
            return words[i.at + 3];
        }
        // The array's length, a constant; an unknown one counts as many.
        let len = words[i.at + 3];
        list.iter().find(|c| c.op == OP_CONSTANT && c.len >= 4 && words[c.at + 2] == len).map_or(OUTPUT_LOCATIONS, |c| words[c.at + 3])
    };
    let end = flow
        .vars
        .iter()
        .filter(|v| v.2 == STORAGE_OUTPUT)
        .filter_map(|&(v, ty, _)| Some(flow.loc.get(&v)? + slots(ty)))
        .max()
        .unwrap_or(0);
    (end < OUTPUT_LOCATIONS).then_some(end)
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
    fn a_vertex_shader_gets_its_outputs_captured_and_nothing_else_changes() {
        let s = summary(VS);
        assert_eq!(s.models, vec![MODEL_VERTEX]);
        assert!(s.position);
        let out = instrument_vertex(VS).expect("instrumented");
        assert_eq!(&out[..3], &VS[..3]);
        assert_eq!(out[3], VS[3] + 3, "three ids added: a constant, a variable, a vector");
        let before = ops(VS);
        let after = ops(&out);
        // Every instruction of the shader but its entry point (whose
        // interface gains our output) is there, in its order.
        let mut it = after.iter();
        for b in before.iter().filter(|(o, _)| *o != OP_ENTRY_POINT) {
            assert!(it.any(|a| a == b), "{b:?} kept in order");
        }
        let added: Vec<_> = after.iter().filter(|a| !before.contains(a)).collect();
        // The colour is `color`, at offset 16; the camera vector
        // `to_camera`, at 32; ours, the world position, at 48 and location 3.
        let color = output_at(&out, 1);
        assert!(added.contains(&&(OP_DECORATE, vec![color, DECORATION_OFFSET, COLOUR_OFFSET])));
        let to_camera = output_at(&out, 2);
        assert!(added.contains(&&(OP_DECORATE, vec![to_camera, DECORATION_OFFSET, CAMERA_OFFSET])));
        let world = output_at(&out, 3);
        assert!(added.contains(&&(OP_DECORATE, vec![world, DECORATION_OFFSET, WORLD_OFFSET])));
        assert!(after.iter().any(|(o, a)| *o == OP_ENTRY_POINT && a.last() == Some(&world)));
        assert!(after.iter().any(|(o, a)| *o == OP_STORE && a[0] == world));
        // The test shader places its vertex by its instance's matrix:
        // static, flag 1; skinned, by a bone a vertex input picks: flag 0.
        assert_eq!(world_flag(&out), 1.0);
        assert_eq!(world_flag(&instrument_vertex(SKINNED).unwrap()), 0.0);
        validate(&instrument_vertex(SKINNED).unwrap());
        assert!(added.contains(&&(OP_CAPABILITY, vec![CAPABILITY_TRANSFORM_FEEDBACK])));
        assert!(added.iter().any(|(o, a)| *o == OP_EXECUTION_MODE && a[1] == EXECUTION_MODE_XFB));
        assert!(added.iter().any(|(o, a)| *o == OP_MEMBER_DECORATE && a[1] == 0 && a[2] == DECORATION_OFFSET && a[3] == 0));
        assert!(added.iter().any(|(o, a)| *o == OP_DECORATE && a[1] == DECORATION_XFB_STRIDE && a[2] == STRIDE));
        // Done once only.
        assert_eq!(instrument_vertex(&out), Err("it already declares Xfb"));
    }

    const SKINNED: &[u32] = include!("test_vs_skinned.spv.rs");

    /// The static flag stored with the world position: the constant in the
    /// vector made right before the store to our output.
    fn world_flag(words: &[u32]) -> f32 {
        let after = ops(words);
        let world = output_at(words, 3);
        let at = after.iter().position(|(o, a)| *o == OP_STORE && a[0] == world).unwrap();
        let (_, c) = &after[at - 1];
        let constant = c[3];
        let (_, k) = after.iter().find(|(o, a)| *o == OP_CONSTANT && a[1] == constant).unwrap();
        f32::from_bits(k[2])
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
    fn a_shader_without_a_colour_or_a_camera_vector_is_left_alone() {
        let words = without_colour();
        assert_eq!(instrument_vertex(&words), Err("no colour output"));
        // The position input moved: nothing depends on it any more.
        let mut moved = VS.to_vec();
        let list = instructions(&moved).unwrap();
        let inputs: Vec<u32> = interface_vars(&moved, &list).into_iter().filter(|v| v.2 == STORAGE_INPUT).map(|v| v.0).collect();
        for i in &list {
            let at = i.at;
            if i.op == OP_DECORATE && moved[at + 2] == DECORATION_LOCATION && moved[at + 3] == 0 && inputs.contains(&moved[at + 1]) {
                moved[at + 3] = 9;
            }
        }
        assert_eq!(instrument_vertex(&moved), Err("no camera vector output (not lit world geometry)"));
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
