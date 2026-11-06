// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared geometry for rendering and hit testing. Local UI +Y points down;
//! positive UI Pitch tilts the face upward. STAGE remains +Y up, -Z forward.
pub const IDLE_RAY_LENGTH_M: f32 = 3.;

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct RayFeedback {
    pub distance_m: f32,
    pub point_m: [f32; 3],
    pub normal: [f32; 3],
    pub interactive: bool,
    pub captured: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub position: [f32; 3],
    pub right: [f32; 3],
    pub down: [f32; 3],
    pub size: [f32; 2],
    pub logical: [f32; 2],
    pub curve: f32,
}
pub fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
/// Roll-free orientation: local +Z points from the panel to the user's eyes.
pub fn user_facing(position: [f32; 3], eye: [f32; 3]) -> [f32; 3] {
    let d = sub(eye, position);
    [
        d[1].atan2(d[0].hypot(d[2])).to_degrees(),
        d[0].atan2(d[2]).to_degrees(),
        0.,
    ]
}

/// Angles describe placement in the current horizontal head frame, not eye cant.
pub fn horizon_spawn(
    eye: [f32; 3],
    q: [f32; 4],
    yaw_deg: f32,
    pitch_deg: f32,
    distance: f32,
) -> [f32; 3] {
    let heading = (2. * (q[0] * q[2] + q[3] * q[1])).atan2(1. - 2. * (q[0] * q[0] + q[1] * q[1]));
    let yaw = heading + yaw_deg.to_radians();
    let pitch = pitch_deg.to_radians();
    add(
        eye,
        [
            -yaw.sin() * pitch.cos() * distance,
            pitch.sin() * distance,
            -yaw.cos() * pitch.cos() * distance,
        ],
    )
}

impl Frame {
    pub fn new(position: [f32; 3], size: [f32; 2], logical: [f32; 2], degrees: [f32; 3]) -> Self {
        let (p, y, r) = (
            degrees[0].to_radians(),
            degrees[1].to_radians(),
            degrees[2].to_radians(),
        );
        let (sy, cy) = y.sin_cos();
        let (sp, cp) = p.sin_cos();
        let (sr, cr) = r.sin_cos();
        let right = [cy, 0., -sy];
        let down = [sy * sp, -cp, cy * sp];
        Self {
            position,
            right: add(scale(right, cr), scale(down, -sr)),
            down: add(scale(right, sr), scale(down, cr)),
            size,
            logical,
            curve: 0.,
        }
    }
    pub fn normal(&self) -> [f32; 3] {
        cross(self.down, self.right)
    }
    pub fn normal_at(&self, xy: [f32; 2]) -> [f32; 3] {
        let angle = if self.curve > 0. {
            (xy[0] / self.logical[0] - 0.5) * self.size[0] / self.curve
        } else {
            0.
        };
        sub(
            scale(self.normal(), angle.cos()),
            scale(self.right, angle.sin()),
        )
    }
    pub fn point(&self, xy: [f32; 2], depth: f32) -> [f32; 3] {
        let mut x = (xy[0] / self.logical[0] - 0.5) * self.size[0];
        let y = (xy[1] / self.logical[1] - 0.5) * self.size[1];
        let mut z = depth;
        if self.curve > 0. {
            let theta = x / self.curve;
            z += self.curve * (1. - theta.cos());
            x = self.curve * theta.sin();
        }
        add(
            self.position,
            add(
                scale(self.right, x),
                add(scale(self.down, y), scale(self.normal(), z)),
            ),
        )
    }
    pub fn rect(&self, rect: [f32; 4], depth: f32) -> Self {
        let xy = [rect[0] + rect[2] / 2., rect[1] + rect[3] / 2.];
        let mut out = *self;
        out.position = self.point(xy, depth);
        out.size = [
            rect[2] / self.logical[0] * self.size[0],
            rect[3] / self.logical[1] * self.size[1],
        ];
        out.logical = [rect[2], rect[3]];
        if self.curve > 0. {
            let theta = (xy[0] / self.logical[0] - 0.5) * self.size[0] / self.curve;
            let n = self.normal();
            out.right = add(scale(self.right, theta.cos()), scale(n, theta.sin()));
            out.down = self.down;
            out.curve = 0.;
        }
        out
    }
    pub fn project(&self, origin: [f32; 3], direction: [f32; 3]) -> Option<([f32; 2], f32)> {
        let rel = sub(origin, self.position);
        let n = self.normal();
        let ox = dot(rel, self.right);
        let oy = dot(rel, self.down);
        let oz = dot(rel, n);
        let dx = dot(direction, self.right);
        let dy = dot(direction, self.down);
        let dz = dot(direction, n);
        if self.curve > 0. {
            let r = self.curve;
            let a = dx * dx + dz * dz;
            if a < 1e-8 {
                return None;
            }
            let b = 2. * (ox * dx + (oz - r) * dz);
            let c = ox * ox + (oz - r) * (oz - r) - r * r;
            let disc = b * b - 4. * a * c;
            if disc < 0. {
                return None;
            }
            let mut best = None;
            for t in [(-b - disc.sqrt()) / (2. * a), (-b + disc.sqrt()) / (2. * a)] {
                if t <= 0. {
                    continue;
                }
                let x = ox + t * dx;
                let z = oz + t * dz;
                let theta = x.atan2(r - z);
                if z > r || theta.abs() > self.size[0] / r / 2. + 0.001 {
                    continue;
                }
                let xy = [
                    (theta * r / self.size[0] + 0.5) * self.logical[0],
                    ((oy + t * dy) / self.size[1] + 0.5) * self.logical[1],
                ];
                if best.is_none_or(|(_, old)| t < old) {
                    best = Some((xy, t));
                }
            }
            best
        } else {
            if dz.abs() < 1e-6 {
                return None;
            }
            let t = -oz / dz;
            if t <= 0. {
                return None;
            }
            Some((
                [
                    ((ox + t * dx) / self.size[0] + 0.5) * self.logical[0],
                    ((oy + t * dy) / self.size[1] + 0.5) * self.logical[1],
                ],
                t,
            ))
        }
    }
    pub fn inside(&self, xy: [f32; 2]) -> bool {
        xy[0] >= 0. && xy[1] >= 0. && xy[0] <= self.logical[0] && xy[1] <= self.logical[1]
    }
    /// Distance to the rounded face, in metres (not clamped hit coordinates).
    pub fn rounded_distance_m(&self, xy: [f32; 2], radius: f32) -> f32 {
        if self.inside_rounded(xy, radius) {
            return 0.;
        }
        let r = radius
            .max(0.)
            .min(self.logical[0].min(self.logical[1]) * 0.5);
        let c = [
            xy[0].clamp(r, self.logical[0] - r),
            xy[1].clamp(r, self.logical[1] - r),
        ];
        let delta = [xy[0] - c[0], xy[1] - c[1]];
        let length = delta[0].hypot(delta[1]);
        let edge = [c[0] + delta[0] * r / length, c[1] + delta[1] * r / length];
        let d = sub(self.point(xy, 0.), self.point(edge, 0.));
        dot(d, d).sqrt()
    }
    pub fn inside_rounded(&self, xy: [f32; 2], radius: f32) -> bool {
        if !self.inside(xy) {
            return false;
        }
        let radius = radius
            .max(0.)
            .min(self.logical[0].min(self.logical[1]) / 2.);
        let dx = (xy[0] - self.logical[0] / 2.).abs() - (self.logical[0] / 2. - radius);
        let dy = (xy[1] - self.logical[1] / 2.).abs() - (self.logical[1] / 2. - radius);
        dx.max(0.).hypot(dy.max(0.)) + dx.max(dy).min(0.) <= radius
    }
}
