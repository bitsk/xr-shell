// SPDX-License-Identifier: GPL-3.0-or-later
//! Rounded articulated surfaces, generated from standard OpenXR joints.
//! No vendor artwork or model weights are embedded here.
use openxr as xr;
type V = [f32; 3];
fn add(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] - b[i])
}
fn scale(a: V, s: f32) -> V {
    a.map(|x| x * s)
}
fn dot(a: V, b: V) -> f32 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(v: V) -> V {
    let n = dot(v, v).sqrt();
    if n > 1e-6 {
        scale(v, n.recip())
    } else {
        [0., 1., 0.]
    }
}
fn vertex(p: V, n: V) -> [f32; 8] {
    [p[0], p[1], p[2], n[0], n[1], n[2], 0.5, 0.5]
}
fn tri(v: &mut Vec<[f32; 8]>, a: V, b: V, c: V) {
    let n = unit(cross(sub(b, a), sub(c, a)));
    v.extend([vertex(a, n), vertex(b, n), vertex(c, n)]);
}
pub fn vertices(joints: &xr::HandJointLocations) -> Vec<[f32; 8]> {
    let valid = xr::SpaceLocationFlags::POSITION_VALID;
    if joints.iter().any(|j| {
        !j.location_flags.contains(valid)
            || !j.radius.is_finite()
            || !j.radius.is_sign_positive()
            || ![j.pose.position.x, j.pose.position.y, j.pose.position.z]
                .iter()
                .all(|v| v.is_finite())
    }) {
        return Vec::new();
    }
    let p: [V; 26] = std::array::from_fn(|i| {
        let p = joints[i].pose.position;
        [p.x, p.y, p.z]
    });
    let r: [f32; 26] = std::array::from_fn(|i| joints[i].radius.clamp(0.003, 0.026));
    let mut out = Vec::with_capacity(7500);
    // Rounded joints and tapered phalanges form an opaque, lit hand surface.
    for j in 2..26 {
        for lat in 0..4 {
            for lon in 0..8 {
                let point = |a: usize, b: usize| {
                    let t = std::f32::consts::PI * a as f32 / 4.;
                    let f = std::f32::consts::TAU * b as f32 / 8.;
                    let n = [t.sin() * f.cos(), t.cos(), t.sin() * f.sin()];
                    vertex(add(p[j], scale(n, r[j])), n)
                };
                let a = point(lat, lon);
                let b = point(lat + 1, lon);
                let c = point(lat + 1, lon + 1);
                let d = point(lat, lon + 1);
                out.extend([a, b, c, a, c, d]);
            }
        }
        let parent = match j {
            2 | 6 | 11 | 16 | 21 => 1,
            _ => j - 1,
        };
        if [2, 6, 11, 16, 21].contains(&j) {
            continue;
        }
        let axis = unit(sub(p[j], p[parent]));
        let x = unit(cross(
            axis,
            if axis[1].abs() < 0.9 {
                [0., 1., 0.]
            } else {
                [1., 0., 0.]
            },
        ));
        let y = cross(axis, x);
        for i in 0..8 {
            let ring = |end: usize, k: usize| {
                let f = std::f32::consts::TAU * k as f32 / 8.;
                let n = add(scale(x, f.cos()), scale(y, f.sin()));
                vertex(add(p[end], scale(n, r[end])), n)
            };
            let a = ring(parent, i);
            let b = ring(j, i);
            let c = ring(j, i + 1);
            let d = ring(parent, i + 1);
            out.extend([a, b, c, a, c, d]);
        }
    }
    // Closed palm volume, including a thumb web. Normal follows joint Y.
    let q = joints[0].pose.orientation;
    let normal = [
        2. * (q.x * q.y - q.w * q.z),
        1. - 2. * (q.x * q.x + q.z * q.z),
        2. * (q.y * q.z + q.w * q.x),
    ];
    let n = scale(unit(normal), r[0] * 0.65);
    let border = [1, 2, 3, 7, 12, 17, 22, 21];
    for k in 0..border.len() {
        let a = p[border[k]];
        let b = p[border[(k + 1) % border.len()]];
        tri(&mut out, add(p[0], n), add(a, n), add(b, n));
        tri(&mut out, sub(p[0], n), sub(b, n), sub(a, n));
        tri(&mut out, add(a, n), sub(a, n), sub(b, n));
        tri(&mut out, add(a, n), sub(b, n), add(b, n));
    }
    out
}
