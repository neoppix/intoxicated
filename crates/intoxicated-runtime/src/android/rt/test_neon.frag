// A fragment shader of the shape of the engine's Neon material, for the
// tests of `spirv.rs` only: unlit, its colour a power of the vertex
// colour, faded into the sky by distance and through the exposure.
//
//     glslc -O --target-env=vulkan1.0 -mfmt=num test_neon.frag -o -
//         -> test_neon.spv.rs

#version 450

layout(set = 0, binding = 0) uniform Frame { vec4 fog; float exposure; } frame;
layout(set = 1, binding = 15) uniform samplerCube sky;

layout(location = 0) in vec2 uv;
layout(location = 1) in vec4 colour;
layout(location = 3) in vec4 camera;

layout(location = 0) out vec4 result;

void main() {
    vec4 c = colour;
    c.rgb = pow(c.rgb * 1.35, vec3(4.0)) * 4.0;
    float d = length(camera.xyz);
    float f = clamp(exp2(frame.fog.z * d + frame.fog.x) - frame.fog.w, 0.0, 1.0);
    c.rgb = mix(textureLod(sky, -camera.xyz / d, 0.0).rgb, c.rgb, f);
    result = vec4(sqrt(clamp(c.rgb * frame.exposure, 0.0, 1.0)), c.a);
}
