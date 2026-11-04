// SPDX-License-Identifier: GPL-3.0-or-later
//! One rigid, lazy-follow anchor for every taskbar island, across components.
use crate::spatial::{Frame, add, sub};
use serde::Serialize;
use xr_spatial_kit::taskbar;

#[derive(Debug, Serialize)]
pub struct TaskbarFollow {
    position_m: [f32; 3],
    yaw_radians: f32,
    initialized: bool,
    following: bool,
    head_height: Option<f32>,
    follow_enabled: bool,
}
impl Default for TaskbarFollow {
    fn default() -> Self {
        Self {
            position_m: taskbar::CENTER_POSITION,
            yaw_radians: 0.,
            initialized: false,
            following: false,
            head_height: None,
            follow_enabled: false,
        }
    }
}
impl TaskbarFollow {
    fn rotate(&self, v: [f32; 3]) -> [f32; 3] {
        let (sin, cos) = self.yaw_radians.sin_cos();
        [cos * v[0] + sin * v[2], v[1], -sin * v[0] + cos * v[2]]
    }
    pub fn frame(&self, mut frame: Frame) -> Frame {
        frame.position = add(
            self.position_m,
            self.rotate(sub(frame.position, taskbar::CENTER_POSITION)),
        );
        frame.right = self.rotate(frame.right);
        frame.down = self.rotate(frame.down);
        frame
    }
    pub fn mode(&self) -> &'static str {
        if self.follow_enabled {
            "follow"
        } else {
            "fixed"
        }
    }
    pub fn set_mode(&mut self, follow: bool) {
        self.follow_enabled = follow;
        self.following = follow;
    }
    pub fn update(&mut self, position: [f32; 3], q: [f32; 4], dt: f32) -> bool {
        let height = *self.head_height.get_or_insert(position[1]);
        self.initialized = true;
        if !self.follow_enabled {
            return false;
        }
        let yaw = (2. * (q[0] * q[2] + q[3] * q[1])).atan2(1. - 2. * (q[0] * q[0] + q[1] * q[1]));
        let delta = (yaw - self.yaw_radians + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        let mut changed = false;
        if delta.abs() > 20f32.to_radians() {
            self.following = true;
        }
        if self.following {
            let step = delta.clamp(-3. * dt, 3. * dt);
            self.yaw_radians += step;
            if delta.abs() < 2f32.to_radians() {
                self.following = false;
            }
            changed |= step.abs() > 0.00001;
        }
        let rotated = self.rotate(taskbar::CENTER_POSITION);
        let target = [
            position[0] + rotated[0],
            position[1] + rotated[1] - height,
            position[2] + rotated[2],
        ];
        let alpha = (dt * 8.).clamp(0., 1.);
        for (current, target) in self.position_m.iter_mut().zip(target) {
            let step = (target - *current) * alpha;
            // Update all axes together so small deltas cannot shear the islands.
            *current += step;
            changed |= step.abs() > 0.00001;
        }
        changed
    }
}
