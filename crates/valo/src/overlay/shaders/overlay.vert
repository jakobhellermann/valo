#version 450
// TODO(ai-review): generated, not yet audited

// epaint vertices: position in pixels from the top-left, color as premultiplied sRGB bytes
layout(location = 0) in vec2 pos;
layout(location = 1) in vec2 uv;
layout(location = 2) in vec4 color;

layout(push_constant) uniform Push {
    vec2 screen_size;
    uint srgb_target;
} push;

layout(location = 0) out vec2 out_uv;
layout(location = 1) out vec4 out_color;

void main() {
    gl_Position = vec4(2.0 * pos / push.screen_size - 1.0, 0.0, 1.0);
    out_uv = uv;
    out_color = color;
}
