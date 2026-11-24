//! For the effects drawn before the HUD (`android::ssr`), and built only
//! with them (the `shaders` feature): the start of a recording, and the
//! engine's compute state, which their dispatch disturbs and puts back.
//! Graphics binds are forwarded after one comparison. Without the feature
//! none of these commands is hooked.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use super::{next, VK_ERROR_INITIALIZATION_FAILED, VK_SUCCESS, WATCH, WATCH_POST};

slots!(
    BEGIN_COMMAND_BUFFER,
    BIND_PIPELINE,
    BIND_DESCRIPTOR_SETS,
    PUSH_CONSTANTS,
    PUSH_DESCRIPTOR_SET,
    PUSH_DESCRIPTOR_SET_WITH_TEMPLATE,
    BIND_DESCRIPTOR_BUFFERS,
    BIND_SHADERS,
);

/// Our entry point for `name`, if it is one of the commands followed here.
pub(super) fn ours(name: &[u8]) -> Option<(&'static AtomicUsize, *const ())> {
    Some(match name {
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
    rc
}

extern "C" fn cmd_bind_pipeline(cmd: *mut c_void, bind_point: u32, pipeline: u64) {
    if bind_point == crate::android::ssr::BIND_POINT_COMPUTE && ssr_watching() {
        crate::android::ssr::compute(cmd, |c| c.bind_pipeline(pipeline));
    }
    if let Some(f) = next::<extern "C" fn(*mut c_void, u32, u64)>(&BIND_PIPELINE) {
        f(cmd, bind_point, pipeline);
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
