// A vertex shader of the engine's shape, for the tests of `spirv.rs` only:
// a position from a vertex buffer, a world matrix per instance from a
// uniform array (or, with SKINNED, per bone, picked by a vertex input), a
// view and projection from a uniform block, a vertex colour, and varyings
// for the fragment shader, among them the vector to the camera.
//
//     glslc -O --target-env=vulkan1.0 -mfmt=num test_vs.vert -o -
//         -> test_vs.spv.rs
//     glslc -O --target-env=vulkan1.0 -mfmt=num -DSKINNED test_vs.vert -o -
//         -> test_vs_skinned.spv.rs

#version 450

layout(location = 0) in vec4 position;
layout(location = 1) in vec4 normal;
layout(location = 2) in vec4 vertex_color;
layout(location = 3) in uvec4 bone;

layout(set = 0, binding = 0) uniform Frame {
    mat4 view_projection;
    vec4 camera;
    vec4 rows[3 * 16];
} frame;

layout(location = 0) out vec3 world_normal;
layout(location = 1) out vec4 color;
layout(location = 2) out vec3 to_camera;

void main() {
#ifdef SKINNED
    uint m = 3u * bone.x;
#else
    uint m = 3u * uint(gl_InstanceIndex % 16);
#endif
    vec4 x = frame.rows[m], y = frame.rows[m + 1u], z = frame.rows[m + 2u];
    vec3 world = vec3(dot(x, position), dot(y, position), dot(z, position));
    world_normal = vec3(dot(x.xyz, normal.xyz), dot(y.xyz, normal.xyz), dot(z.xyz, normal.xyz));
    color = vertex_color * 0.5;
    to_camera = frame.camera.xyz - world;
    gl_Position = frame.view_projection * vec4(world, 1.0);
}
