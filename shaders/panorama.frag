// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
layout(location = 0) in vec3 panorama_ray;
layout(location = 0) out vec4 colour;
layout(set = 0, binding = 0) uniform sampler2D panorama;
const float PI = 3.141592653589793;

void main() {
    vec3 d = normalize(panorama_ray);
    vec2 uv = vec2(atan(d.x, -d.z) / (2.0 * PI) + 0.5,
                   0.5 - asin(clamp(d.y, -1.0, 1.0)) / PI);
    colour = vec4(texture(panorama, uv).rgb, 1.0);
}
