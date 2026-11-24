// A fragment shader of the engine's shape, for the tests of `spirv.rs`
// only: the base colour is a texture at the texture coordinates times the
// vertex colour (with DETAIL, a grey detail texture's one channel twice
// over; with SCALED, the coordinates are scaled first, which the analysis
// does not follow).
//
//     glslc -O --target-env=vulkan1.0 -mfmt=num test_fs.frag -o -
//         -> test_fs.spv.rs
//     (and -DDETAIL -> test_fs_detail.spv.rs, -DSCALED -> test_fs_scaled.spv.rs)

#version 450

layout(set = 0, binding = 0) uniform sampler2D environment;
layout(set = 0, binding = 3) uniform sampler2D base;
layout(set = 0, binding = 4) uniform Material { vec2 scale; } material;

layout(location = 0) in vec2 uv;
layout(location = 1) in vec4 colour;
layout(location = 2) in vec3 normal;

layout(location = 0) out vec4 result;

void main() {
#if defined(DETAIL)
    vec3 albedo = colour.rgb * (texture(base, uv).x * 2.0);
#elif defined(SCALED)
    vec3 albedo = (texture(base, uv * material.scale) * colour).rgb;
#else
    vec3 albedo = (texture(base, uv) * colour).rgb;
#endif
    vec3 light = texture(environment, normal.xy).rgb;
    result = vec4(sqrt(albedo * albedo * light), colour.a);
}
