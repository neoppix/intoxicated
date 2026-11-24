// A vertex shader of the shape of the engine's terrain, for the tests of
// `spirv.rs` only: quantized integer positions, a packed normal, material
// indices, no colour; with NOCAMERA, no camera vector either.
//
//     glslc -O --target-env=vulkan1.0 -mfmt=num test_terrain.vert -o -
//         -> test_terrain.spv.rs
//     (and -DNOCAMERA -> test_terrain_nocamera.spv.rs)

#version 450

layout(set = 0, binding = 0) uniform Frame { mat4 view_projection; vec3 camera; vec3 sun; } frame;
layout(set = 0, binding = 1) uniform Chunk { vec3 offset; vec3 scale; } chunk;
layout(set = 0, binding = 2) uniform Materials { vec4 tint[32]; } materials;

layout(location = 0) in ivec4 position;
layout(location = 1) in uvec4 normal;
layout(location = 4) in uvec4 material;

layout(location = 0) out vec4 weights;
layout(location = 1) out vec3 light;
layout(location = 2) out vec4 tint;
#ifdef NOCAMERA
layout(location = 3) out vec4 world_and_distance;
#else
layout(location = 3) out vec3 to_camera;
#endif

void main() {
    vec3 world = vec3(position.xyz) * chunk.scale + chunk.offset;
    vec3 n = vec3(normal.xyz) * (1.0 / 127.0) - 1.0;
    gl_Position = vec4(world, 1.0) * frame.view_projection;
    weights = vec4(equal(ivec3(abs(position.www) % 4), ivec3(1, 2, 3)), 1.0);
    light = frame.sun * max(dot(n, frame.sun), 0.0);
    tint = materials.tint[material.x];
#ifdef NOCAMERA
    world_and_distance = vec4(world, length(frame.camera - world));
#else
    to_camera = frame.camera - world;
#endif
}
