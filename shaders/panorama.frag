// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 colour;
layout(set = 0, binding = 0) uniform sampler2D panorama;
layout(push_constant) uniform View {
    vec4 fov;         // tangents: left, right, up, down
    vec4 orientation; // world-space quaternion xyzw
    vec4 environment; // cosine/sine of panorama yaw
} view;
const float PI = 3.141592653589793;
void main() {
    vec3 d = normalize(vec3(mix(view.fov.x, view.fov.y, uv.x),
                            mix(view.fov.z, view.fov.w, uv.y), -1.0));
    vec4 q = view.orientation;
    d = d + 2.0 * cross(q.xyz, cross(q.xyz, d) + q.w * d);
    d = normalize(vec3(view.environment.x*d.x - view.environment.y*d.z, d.y,
                       view.environment.y*d.x + view.environment.x*d.z));
    vec2 equirect = vec2(atan(d.x, -d.z) / (2.0 * PI) + 0.5,
                         0.5 - asin(clamp(d.y, -1.0, 1.0)) / PI);
    colour = vec4(texture(panorama, equirect).rgb, 1.0);
}
