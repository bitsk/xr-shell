// SPDX-License-Identifier: GPL-3.0-or-later
#version 450
layout(location = 0) out vec3 panorama_ray;
layout(push_constant) uniform View {
    vec4 fov;
    vec4 orientation;
    vec4 environment;
    vec4 origin;
} view;

void main() {
    vec2 p = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    vec3 ray = vec3(mix(view.fov.x, view.fov.y, p.x),
                    mix(view.fov.z, view.fov.w, p.y), -1.0);
    vec4 q = view.orientation;
    ray += 2.0 * cross(q.xyz, cross(q.xyz, ray) + q.w * ray);
    // Rotation is linear and clip W is constant. Interpolate the world ray
    // across the triangle before normalizing and applying the spherical map.
    panorama_ray = vec3(view.environment.x * ray.x - view.environment.y * ray.z,
                        ray.y,
                        view.environment.y * ray.x + view.environment.x * ray.z);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
