#version 450
// TODO(ai-review): generated, not yet audited

// epaint's font atlas: premultiplied sRGB bytes, sampled without decoding
layout(set = 0, binding = 0) uniform sampler2D atlas;

layout(push_constant) uniform Push {
    vec2 screen_size;
    uint srgb_target;
} push;

layout(location = 0) in vec2 uv;
layout(location = 1) in vec4 color;
layout(location = 0) out vec4 out_color;

vec3 linear_from_srgb(vec3 srgb) {
    bvec3 low = lessThan(srgb, vec3(0.04045));
    return mix(pow((srgb + 0.055) / 1.055, vec3(2.4)), srgb / 12.92, low);
}

void main() {
    vec4 c = color * texture(atlas, uv);
    out_color = push.srgb_target != 0 ? vec4(linear_from_srgb(c.rgb), c.a) : c;
}
