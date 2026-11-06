// SPDX-License-Identifier: GPL-3.0-or-later
// Included in renderer: layer ownership and Vulkan targets stay with Renderer.
#[derive(Clone, Copy)]
enum DrawPass {
    All,
    Background,
    Foreground,
    Quad,
}

pub struct FocusQuad {
    pub id: String,
    pub frame: crate::spatial::Frame,
}
impl FocusQuad {
    pub fn pose(&self) -> xr::Posef {
        let n = self.frame.normal();
        let (sy, cy) = (n[0].atan2(n[2]) * 0.5).sin_cos();
        let (sp, cp) = (n[1].clamp(-1., 1.).asin() * 0.5).sin_cos();
        xr::Posef {
            orientation: xr::Quaternionf {
                x: -sp * cy,
                y: cp * sy,
                z: sp * sy,
                w: cp * cy,
            },
            position: xr::Vector3f {
                x: self.frame.position[0],
                y: self.frame.position[1],
                z: self.frame.position[2],
            },
        }
    }
    // Front-on sampling at the window's pixel resolution. Existing shader keeps
    // rounded alpha and converts Cairo sRGB premultiplication to linear light.
    // This is a texture bake, never a predicted eye pose or tracking observation.
    fn view(&self) -> xr::View {
        let mut pose = self.pose();
        let n = self.frame.normal();
        pose.position.x += n[0];
        pose.position.y += n[1];
        pose.position.z += n[2];
        let x = (self.frame.size[0] * 0.5).atan();
        let y = (self.frame.size[1] * 0.5).atan();
        xr::View {
            pose,
            fov: xr::Fovf {
                angle_left: -x,
                angle_right: x,
                angle_up: y,
                angle_down: -y,
            },
        }
    }
}
impl Renderer {
    pub fn quad_status(&self) -> serde_json::Value {
        if let Some(q) = &self.focus_quad {
            let p = q.pose();
            serde_json::json!({"active":true,"layer_count":3,"order":["background-projection","focus-quad","foreground-projection"],
                "foreground_crop_enabled":self.quad_crop_enabled,
                "foreground_regions":self.foreground_regions.iter().map(|(r,f)| serde_json::json!({"rect":[r.offset.x,r.offset.y,r.extent.width,r.extent.height],"fov_radians":[f.angle_left,f.angle_right,f.angle_up,f.angle_down]})).collect::<Vec<_>>(),
                "window":q.id,"position_m":q.frame.position,"size_m":q.frame.size,
                "orientation_xyzw":[p.orientation.x,p.orientation.y,p.orientation.z,p.orientation.w],
                "texture_size":[self.quad.as_ref().unwrap().extent.width,self.quad.as_ref().unwrap().extent.height],
                "alpha":"linear-light premultiplied; source-alpha layer flag", "capture_scope":"separate submitted layers, not compositor output"})
        } else {
            serde_json::json!({"active":false,"layer_count":1,"fallback_reason":self.quad_reason})
        }
    }
    pub fn targets_mut(&mut self) -> Vec<&mut Eye> {
        let mut out: Vec<_> = self.eyes.iter_mut().collect();
        if self.focus_quad.is_some() {
            out.extend(self.foreground.iter_mut());
            out.push(self.quad.as_mut().unwrap());
        }
        out
    }
    pub fn prepare_focus_quad(
        &mut self,
        session: &xr::Session<xr::Vulkan>,
        focus: Option<String>,
        views: &[xr::View],
        enabled: bool,
        limits: [u32; 3],
    ) {
        self.focus_quad = None;
        let reason = self.select_focus_quad(focus, views, enabled, limits);
        if let Err(reason) = reason {
            self.quad_reason = reason;
            return;
        }
        let q = self.focus_quad.as_ref().unwrap();
        let entry = &self.atlas_entries[&q.id];
        let extent = vk::Extent2D {
            width: entry.size.0,
            height: entry.size.1,
        };
        let allocate = (|| -> Result<()> {
            if self.foreground.is_empty() {
                let a = self.create_target(session, self.eyes[0].extent)?;
                let b = self.create_target(session, self.eyes[1].extent)?;
                self.foreground = vec![a, b];
            }
            if self.quad.as_ref().is_none_or(|q| q.extent != extent) {
                let next = self.create_target(session, extent)?;
                self.quad = Some(next);
            }
            Ok(())
        })();
        if let Err(e) = allocate {
            // Do not retry a failing allocation every frame. Original projection
            // and its existing targets remain available for this session.
            self.focus_quad = None;
            self.quad_disabled = true;
            self.quad_reason = format!("swapchain-unavailable: {e:#}");
            eprintln!("XR_FOCUS_QUAD_FALLBACK={}", self.quad_reason);
        } else {
            self.foreground_regions = views
                .iter()
                .enumerate()
                .map(|(i, v)| self.foreground_region(v, self.eyes[i].extent))
                .collect();
            self.quad_reason = "active".into();
        }
    }
    // Crop the foreground projection's submitted sub-image and FOV together.
    // Geometry, pixel density, eye pose and depth ordering remain unchanged.
    // Flat panels (including their extrusion) are bounded before perspective
    // division. A near-plane crossing or non-finite value retains the full eye.
    fn foreground_region(&self, view: &xr::View, extent: vk::Extent2D) -> (vk::Rect2D, xr::Fovf) {
        use crate::spatial::{add, cross, scale, sub};
        let full = (
            vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent,
            },
            view.fov,
        );
        if !self.quad_crop_enabled {
            return full;
        }
        let q = view.pose.orientation;
        let inverse = [-q.x, -q.y, -q.z];
        let eye = view.pose.position;
        let origin = [eye.x, eye.y, eye.z];
        let left = view.fov.angle_left.tan();
        let right = view.fov.angle_right.tan();
        let up = view.fov.angle_up.tan();
        let down = view.fov.angle_down.tan();
        let width = extent.width as f32;
        let height = extent.height as f32;
        let mut lo = [width, height];
        let mut hi = [0f32, 0f32];
        for (g, part) in self.geometry.iter().zip(&self.partition) {
            if *part != 1 {
                continue;
            }
            let f = g.frame;
            let thickness = match g.material as u32 {
                1 => 0.006,
                2 => 0.003,
                3 => 0.002,
                _ => 0.,
            };
            for x in [0., f.logical[0]] {
                for y in [0., f.logical[1]] {
                    for z in [-thickness, 0.] {
                        let p = sub(add(f.point([x, y], z), self.ui_offset), origin);
                        let t = scale(cross(inverse, p), 2.);
                        let v = add(p, add(scale(t, q.w), cross(inverse, t)));
                        if v.iter().any(|c| !c.is_finite()) || -v[2] <= 0.05 {
                            return full;
                        }
                        let pixel = [
                            (v[0] / -v[2] - left) / (right - left) * width,
                            (up - v[1] / -v[2]) / (up - down) * height,
                        ];
                        if pixel.iter().any(|c| !c.is_finite()) {
                            return full;
                        }
                        for i in 0..2 {
                            lo[i] = lo[i].min(pixel[i]);
                            hi[i] = hi[i].max(pixel[i]);
                        }
                    }
                }
            }
        }
        // Two native pixels retain raster/filter coverage at the rectangle edge.
        let x = (lo[0] - 2.).floor().clamp(0., width - 1.) as i32;
        let y = (lo[1] - 2.).floor().clamp(0., height - 1.) as i32;
        let end_x = (hi[0] + 2.).ceil().clamp(x as f32 + 1., width);
        let end_y = (hi[1] + 2.).ceil().clamp(y as f32 + 1., height);
        let rect = vk::Rect2D {
            offset: vk::Offset2D { x, y },
            extent: vk::Extent2D {
                width: end_x as u32 - x as u32,
                height: end_y as u32 - y as u32,
            },
        };
        let fov = xr::Fovf {
            angle_left: (left + (right - left) * x as f32 / width).atan(),
            angle_right: (left + (right - left) * end_x / width).atan(),
            angle_up: (up - (up - down) * y as f32 / height).atan(),
            angle_down: (up - (up - down) * end_y / height).atan(),
        };
        (rect, fov)
    }
    fn select_focus_quad(
        &mut self,
        focus: Option<String>,
        views: &[xr::View],
        enabled: bool,
        limits: [u32; 3],
    ) -> std::result::Result<(), String> {
        use crate::spatial::{add, dot, sub};
        if !enabled {
            return Err("disabled".into());
        }
        if self.quad_disabled {
            return Err("swapchain-disabled-after-error".into());
        }
        if limits[0] < 3 {
            return Err("runtime-layer-limit".into());
        }
        if !self.ui_anchor_ready {
            return Err("anchor-unavailable".into());
        }
        let id = focus.ok_or("no-acknowledged-flat-focus")?;
        let selected = self
            .geometry
            .iter()
            .find(|p| p.id == id)
            .ok_or("focus-not-drawn")?;
        let mut frame = selected.frame;
        if frame.curve != 0. || frame.right[1].abs() > 1e-5 {
            return Err("curved-or-rolled-focus".into());
        }
        let entry = self
            .atlas_entries
            .get(&id)
            .ok_or("focus-texture-unavailable")?;
        if entry.size.0 > limits[1] || entry.size.1 > limits[2] {
            return Err("runtime-texture-limit".into());
        }
        frame.position = add(frame.position, self.ui_offset);
        let normal = frame.normal();
        for view in views {
            let p = view.pose.position;
            if dot(sub([p.x, p.y, p.z], frame.position), normal) <= 0.05 {
                return Err("eye-behind-focus".into());
            }
        }
        // Until model/ray volumes have a verified cross-layer clipping path,
        // preserve the established single-pass depth relationship for controllers.
        if self
            .controller_states
            .iter()
            .any(|s| s.grip.is_some() || s.aim.is_some())
        {
            return Err("controller-depth-requires-projection".into());
        }
        self.partition.clear();
        for g in &self.geometry {
            if g.id == id {
                self.partition.push(0);
                continue;
            }
            let f = g.frame;
            if f.curve != 0. {
                return Err(format!("curved-occluder:{}", g.id));
            }
            let thickness = match g.material as u32 {
                1 => 0.006,
                2 => 0.003,
                3 => 0.002,
                _ => 0.,
            };
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for x in [0., f.logical[0]] {
                for y in [0., f.logical[1]] {
                    for z in [-thickness, 0.] {
                        let d = dot(
                            sub(add(f.point([x, y], z), self.ui_offset), frame.position),
                            normal,
                        );
                        lo = lo.min(d);
                        hi = hi.max(d);
                    }
                }
            }
            if hi < -1e-5 {
                self.partition.push(-1);
            } else if lo > 1e-5 {
                self.partition.push(1);
            } else {
                return Err(format!("intersecting-or-coplanar:{}", g.id));
            }
        }
        self.focus_quad = Some(FocusQuad { id, frame });
        Ok(())
    }
}
