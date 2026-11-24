// A vertex shader of the engine's shape, for the tests of `spirv.rs` only:
// a position from a vertex buffer, a world matrix per instance, a view and
// projection from a uniform block, and varyings for the fragment shader.
//
//     glslc -O --target-env=vulkan1.0 -mfmt=num test_vs.vert -o -
//         -> test_vs.spv.rs

#version 450

layout(location = 0) in vec4 position;
layout(location = 1) in vec4 normal;
layout(location = 2) in vec4 world_x;
layout(location = 3) in vec4 world_y;
layout(location = 4) in vec4 world_z;

layout(set = 0, binding = 0) uniform Frame {
    mat4 view_projection;
    vec4 camera;
} frame;

layout(location = 0) out vec3 world_normal;
layout(location = 1) out vec4 color;

void main() {
    vec3 world = vec3(dot(world_x, position), dot(world_y, position), dot(world_z, position));
    world_normal = vec3(dot(world_x.xyz, normal.xyz), dot(world_y.xyz, normal.xyz), dot(world_z.xyz, normal.xyz));
    color = vec4(length(world - frame.camera.xyz) * 0.01);
    gl_Position = frame.view_projection * vec4(world, 1.0);
}
