#version 450

// epaint vertices: position in pixels from the top-left, color as premultiplied sRGB bytes
layout(location = 0) in vec2 pos;
layout(location = 1) in vec2 uv;
layout(location = 2) in vec4 color;

layout(push_constant) uniform Push {
    vec2 screen_size;
    uint srgb_target;
} push;

layout(location = 0) out vec4 out_color;

vec3 linear_from_srgb(vec3 srgb) {
    bvec3 low = lessThan(srgb, vec3(0.04045));
    return mix(pow((srgb + 0.055) / 1.055, vec3(2.4)), srgb / 12.92, low);
}

void main() {
    gl_Position = vec4(2.0 * pos / push.screen_size - 1.0, 0.0, 1.0);
    out_color = push.srgb_target != 0 ? vec4(linear_from_srgb(color.rgb), color.a) : color;
}
