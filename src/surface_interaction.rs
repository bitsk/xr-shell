// SPDX-License-Identifier: GPL-3.0-or-later
//! Surface button identity and spatial click stabilization. No pose filtering.
use super::*;

#[derive(Clone)]
pub(super) struct SurfaceClick {
    key: String,
    button: u32,
    xy: [f32; 2],
    direction: Option<[f32; 3]>,
    pub(super) at: Instant,
    size: [u32; 2],
}
#[derive(Clone)]
pub(super) struct SurfacePress {
    click: SurfaceClick,
    raw_start: [f32; 2],
    drift_squared: f32,
    snapped: bool,
}
fn distance_squared(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}
impl Desktop {
    pub(super) fn target_surface(&self, target: &Target) -> Option<String> {
        match target {
            Target::Window(k) => Some(k.clone()),
            Target::Node(owner, node) => self
                .peers
                .get(owner)?
                .nodes
                .get(node)
                .filter(|n| n.kind == "SurfaceReference")
                .map(|n| format!("{}:{}", n.window_server_generation, n.surface_id)),
            _ => None,
        }
    }
    pub(super) fn grabbable_window(&self, target: &Target) -> Option<String> {
        let key = match target {
            Target::Move(k) | Target::Orb(k) => Some(k.clone()),
            _ => self.target_surface(target),
        }?;
        self.windows
            .get(&key)
            .filter(|w| self.visible_window(w) && w.has_window_controls())?;
        Some(key)
    }
    pub(super) fn flush_pending_surface_input(&mut self) -> Result<()> {
        if !self.focus_applied {
            self.pending_pointer = None;
            self.pending_scroll = None;
            return Ok(());
        }
        if let Some(press) = self.pending_pointer.take() {
            let source = press["source"].as_str().unwrap_or("");
            let button = press["button"].as_u64().unwrap_or(272) as u32;
            if self
                .captures
                .get(source)
                .is_some_and(|c| c.surface == self.focus && c.button == button)
            {
                self.surface_input(json!({"kind":"pointer_motion","x":press["x"],"y":press["y"]}))?;
                self.surface_input(
                    json!({"kind":"pointer_button","button":button,"pressed":true}),
                )?;
                self.captures.get_mut(source).unwrap().button_sent = true;
            }
        }
        if let Some(scroll) = self.pending_scroll.take() {
            if self.focus.as_deref() == scroll["target"].as_str()
                && !self.captures.values().any(|c| c.surface.is_some())
            {
                if let (Some(x), Some(y)) = (scroll.get("x"), scroll.get("y")) {
                    self.surface_input(json!({"kind":"pointer_motion","x":x,"y":y}))?;
                }
                self.surface_input(json!({"kind":"scroll","dx":scroll["dx"],"dy":scroll["dy"]}))?;
            }
        }
        Ok(())
    }
    pub(super) fn pointer_surface(
        &mut self,
        key: &str,
        mut local: [f32; 2],
        phase: &str,
        source: &str,
        button: u32,
    ) -> Result<()> {
        if phase == "press" {
            self.set_focus(Some(key.to_owned()))?;
            // Ignore a refused capture (single legacy seat arbitration).
            if !self.captures.contains_key(source) {
                return Ok(());
            }
            let w = &self.windows[key];
            let now = Instant::now();
            let spatial = source.starts_with("controller_ray:") || source.starts_with("debug_ray:");
            let raw_start = local;
            let previous = self.last_clicks.remove(source);
            let snapped = spatial
                && previous.as_ref().is_some_and(|c| {
                    let elapsed = now.duration_since(c.at);
                    let angular = self.pointer_ray.zip(c.direction).is_some_and(|(a, b)| {
                        let dot = a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
                        dot >= 1.5f32.to_radians().cos()
                    });
                    c.key == key
                        && c.button == button
                        && c.size == [w.width, w.height]
                        && (Duration::from_millis(40)..=Duration::from_millis(420))
                            .contains(&elapsed)
                        && (angular || distance_squared(local, c.xy) <= 24. * 24.)
                });
            if snapped {
                local = previous.as_ref().unwrap().xy;
            }
            self.captures.get_mut(source).unwrap().click = Some(SurfacePress {
                click: SurfaceClick {
                    key: key.into(),
                    button,
                    xy: local,
                    direction: self.pointer_ray,
                    at: now,
                    size: [w.width, w.height],
                },
                raw_start,
                drift_squared: 0.,
                snapped,
            });
            if !self.focus_applied {
                self.pending_pointer =
                    Some(json!({"x":local[0],"y":local[1],"button":button,"source":source}));
            }
        } else if let Some(press) = self.captures.get_mut(source).and_then(|c| c.click.as_mut()) {
            press.drift_squared = press
                .drift_squared
                .max(distance_squared(local, press.raw_start));
            if press.snapped && press.drift_squared <= 24. * 24. {
                local = press.click.xy;
            }
        }
        if self.focus.as_deref() == Some(key) && self.focus_applied {
            // A short click released before FocusApplied must not create an orphan release.
            let sent = self.captures.get(source).is_some_and(|c| c.button_sent);
            if phase != "release" || sent {
                self.surface_input(json!({"kind":"pointer_motion","x":local[0],"y":local[1]}))?;
            }
            if phase == "press" || (phase == "release" && sent) {
                self.surface_input(
                    json!({"kind":"pointer_button","button":button,"pressed":phase=="press"}),
                )?;
                if let Some(c) = self.captures.get_mut(source) {
                    c.button_sent = phase == "press";
                }
                if phase == "release" {
                    if let Some(p) = self.captures.get(source).and_then(|c| c.click.as_ref()) {
                        // A drag or an already paired click cannot seed a new double-click.
                        if !p.snapped && p.drift_squared <= 24. * 24. {
                            self.last_clicks.insert(source.into(), p.click.clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
