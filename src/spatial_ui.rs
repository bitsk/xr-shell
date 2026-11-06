// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::spatial::{Frame, add, dot, scale, sub};
#[derive(Clone)]
enum Source {
    Node(u64, String),
    Window(String),
    Grab(String),
    Orb(String),
    Close(String),
    Minimize(String),
    Curve(String),
    Resize(String, bool),
    Shadow(Box<Source>),
}
struct RayPick {
    target: Option<Target>,
    xy: [f32; 2],
    distance: f32,
    normal: [f32; 3],
    companion: Option<String>,
}
struct Draw {
    id: String,
    frame: Frame,
    source: Source,
    radius: f32,
    rim: f32,
    border: f32,
    material: f32,
}
#[derive(Clone)]
pub struct Geometry {
    pub id: String,
    pub frame: Frame,
    pub radius: f32,
    pub rim: f32,
    pub border: f32,
    pub material: f32,
    pub anchor: [f32; 3],
    pub group: String,
    pub layer: f32,
    pub tint: [f32; 4],
    pub controls: [f32; 4],
}
fn raster_size(draw: &Draw) -> (u32, u32) {
    if matches!(draw.source, Source::Shadow(_)) {
        return (1, 1);
    }
    let w = draw.frame.logical[0].round().max(1.) as u32;
    let h = draw.frame.logical[1].round().max(1.) as u32;
    if matches!(draw.source, Source::Window(_)) {
        return (w, h);
    }
    let scale = (2048. / w.max(h) as f64).min(2.);
    (
        ((w as f64 * scale).round().max(1.)) as u32,
        ((h as f64 * scale).round().max(1.)) as u32,
    )
}
impl Desktop {
    pub(super) fn node_policy<'a>(p: &'a Peer, node: &'a Node) -> &'a str {
        let mut parent = node.parent_id.as_str();
        for _ in 0..512 {
            if parent == "root" {
                break;
            }
            let Some(n) = p.nodes.get(parent) else {
                break;
            };
            if n.spatial_panel {
                return &n.spatial_policy;
            }
            parent = &n.parent_id;
        }
        &p.root.policy
    }
    fn panel_frame(&self, p: &Peer, n: &Node) -> Frame {
        let frame = Frame::new(n.position_m, n.size_m, n.logical_size, n.rotation_degrees);
        if n.spatial_policy == "head_horizon_spawn" {
            p.spawn_frames.get(&n.node_id).copied().unwrap_or(frame)
        } else if n.spatial_policy == "dock_lazy_follow" {
            self.taskbar_follow.frame(frame)
        } else {
            frame
        }
    }
    fn node_base(&self, p: &Peer, node: &Node) -> (Frame, String) {
        let mut parent = node.parent_id.as_str();
        for _ in 0..512 {
            if parent == "root" {
                break;
            }
            let Some(n) = p.nodes.get(parent) else {
                break;
            };
            if n.spatial_panel {
                return (
                    self.panel_frame(p, n),
                    format!("component:{}:{}", p.id, n.node_id),
                );
            }
            parent = &n.parent_id;
        }
        (self.root_frame(p), format!("component:{}:root", p.id))
    }
    fn window_group(&self, key: &str) -> (String, [f32; 3], f32) {
        let mut root = key.to_owned();
        let mut depth = 0;
        for _ in 0..16 {
            let w = &self.windows[&root];
            if w.state["role"].as_str().is_none_or(|r| r == "toplevel") {
                break;
            }
            let Some(parent) = w.state["parent_surface_id"].as_str() else {
                break;
            };
            let key = format!(
                "{}:{parent}",
                w.state["window_server_generation"].as_str().unwrap_or("")
            );
            if !self.windows.contains_key(&key) {
                break;
            }
            root = key;
            depth += 1;
        }
        (
            format!("window:{root}"),
            self.window_frame(&root, 0)
                .map(|f| f.position)
                .unwrap_or([0.; 3]),
            depth as f32 * 0.004,
        )
    }
    pub(super) fn root_frame(&self, p: &Peer) -> Frame {
        let frame = Frame::new(
            p.root.position,
            p.root.size,
            p.root.logical,
            p.root.rotation,
        );
        if p.root.policy == "dock_lazy_follow" {
            self.taskbar_follow.frame(frame)
        } else {
            frame
        }
    }
    pub(super) fn node_frame(&self, p: &Peer, node: &Node) -> Option<Frame> {
        if !node.visible || node.kind == "Group" {
            return None;
        }
        let mut rect = node.rect;
        if rect[2] <= 0. || rect[3] <= 0. {
            return None;
        }
        let interactive = ["Button", "Slider", "SurfaceReference"].contains(&node.kind.as_str());
        let mut depth = if interactive {
            0.008
        } else if ["Text", "Icon"].contains(&node.kind.as_str()) {
            0.001
        } else {
            0.
        };
        depth += self
            .lifts
            .get(&(p.id, node.node_id.clone()))
            .copied()
            .unwrap_or(0.);
        let mut parent = node.parent_id.as_str();
        let mut frame = self.root_frame(p);
        for _ in 0..512 {
            if parent == "root" {
                break;
            }
            let n = p.nodes.get(parent)?;
            if !n.visible {
                return None;
            }
            if n.spatial_panel {
                if n.spatial_policy == "head_horizon_spawn"
                    && !p.spawn_frames.contains_key(&n.node_id)
                {
                    return None;
                }
                frame = self.panel_frame(p, n);
                break;
            }
            rect[0] += n.rect[0];
            rect[1] += n.rect[1];
            if ["Button", "Slider"].contains(&n.kind.as_str()) {
                depth += 0.008
                    + self
                        .lifts
                        .get(&(p.id, n.node_id.clone()))
                        .copied()
                        .unwrap_or(0.);
            }
            parent = &n.parent_id;
        }
        Some(frame.rect(rect, depth))
    }
    /// Only acknowledged, visible, flat top-level focus can become a native layer.
    pub fn focus_quad_window(&self) -> Option<String> {
        if !self.focus_applied {
            return None;
        }
        let key = self.focus.as_ref()?;
        let w = self.windows.get(key)?;
        (self.visible_window(w) && w.has_window_controls() && !w.curved)
            .then(|| format!("window:{key}"))
    }

    fn window_frame(&self, k: &str, level: u32) -> Option<Frame> {
        let w = self.windows.get(k)?;
        let logical = Self::logical(w);
        if !w.placed {
            return None;
        }
        let mut frame = Frame::new(w.position, w.size, logical, w.rotation);
        frame.curve = if w.curved {
            w.size[0] / 30f32.to_radians()
        } else {
            0.
        };
        if level < 16 && w.state["role"].as_str().is_some_and(|r| r != "toplevel") {
            if let Some(parent) = w.state["parent_surface_id"].as_str() {
                let pk = format!("{}:{parent}", w.state["window_server_generation"].as_str()?);
                if let Some(parent) = self.window_frame(&pk, level + 1) {
                    let x = w.state["popup_x"].as_f64().unwrap_or(0.) as f32;
                    let y = w.state["popup_y"].as_f64().unwrap_or(0.) as f32;
                    frame = parent.rect([x, y, logical[0], logical[1]], 0.004);
                }
            }
        }
        Some(frame)
    }
    fn visible_window(&self, w: &Window) -> bool {
        !w.minimized
            && w.placed
            && w.state["mapped"] != false
            && w.state["workspace"].as_u64().unwrap_or(0) == self.workspace as u64
            && !w.pixels.is_empty()
    }
    pub(super) fn companion_frame(&self, k: &str, kind: &str) -> Option<Frame> {
        let w = self.windows.get(k)?;
        if !self.visible_window(w) || !w.has_window_controls() {
            return None;
        }
        let frame = self.window_frame(k, 0)?;
        let unit = frame.size[0] / frame.logical[0];
        let height = 24.;
        let bar_width = (frame.logical[0] * 0.24).clamp(144., 220.);
        // Centre the complete companion group instead of the grab capsule alone.
        let bar_x = (frame.logical[0] - bar_width - 168.) / 2. + 168.;
        let y = frame.logical[1] + 32.;
        let rect = match kind {
            "grab" => [bar_x, y - height / 2., bar_width, height],
            "orb" => [bar_x - 42., y - 16., 32., 32.],
            "curve" => [bar_x - 84., y - 16., 32., 32.],
            "minimize" => [bar_x - 126., y - 16., 32., 32.],
            "close" => [bar_x - 168., y - 16., 32., 32.],
            "resize-left" => [-30., frame.logical[1] - 12., 24., 24.],
            "resize-right" => [frame.logical[0] + 6., frame.logical[1] - 12., 24., 24.],
            _ => return None,
        };
        if kind.starts_with("resize-") {
            return Some(frame.rect(rect, 0.006));
        }
        let mut plane = frame;
        plane.curve = 0.;
        let lift = self
            .companion_lifts
            .get(&format!("{kind}:{k}"))
            .copied()
            .unwrap_or(0.);
        let mut out = plane.rect(rect, 0.008 + lift);
        out.size[1] = rect[3] * unit;
        Some(out)
    }
    fn draws(&self) -> Vec<Draw> {
        let mut out = Vec::new();
        for p in self.peers.values() {
            for node in Self::ordered_nodes(p) {
                let Some(frame) = self.node_frame(p, node) else {
                    continue;
                };
                if node.kind == "SurfaceReference"
                    && !self.windows.contains_key(&format!(
                        "{}:{}",
                        node.window_server_generation, node.surface_id
                    ))
                {
                    continue;
                }
                let button = node.kind == "Button";
                let icon_button = button
                    && ["dock.app-icon", "dock.active-icon", "dock.status-icon"]
                        .contains(&node.style_id.as_str());
                let glass = node.kind == "Card"
                    && ["dock.background", "panel.background"].contains(&node.style_id.as_str());
                let radius = if node.kind == "Text" || node.kind == "Icon" {
                    0.
                } else if node.kind == "Slider" {
                    11.
                } else if node.style_id == "dock.background" {
                    node.rect[3] / 2.
                } else if icon_button {
                    12.
                } else if node.style_id.starts_with("dock.") {
                    node.rect[3] / 2.
                } else if node.kind == "SurfaceReference" {
                    16.
                } else if glass {
                    20.
                } else {
                    12.
                };
                out.push(Draw {
                    id: format!("node:{}:{}", p.id, node.node_id),
                    frame,
                    source: Source::Node(p.id, node.node_id.clone()),
                    radius,
                    rim: if icon_button {
                        0.
                    } else if node.kind == "SurfaceReference" {
                        2.
                    } else if button || glass {
                        1.
                    } else {
                        0.
                    },
                    border: if icon_button {
                        0.
                    } else if node.style_id == "keyboard.key" {
                        0.25
                    } else if node.kind == "SurfaceReference" {
                        0.5
                    } else if button {
                        0.18
                    } else if glass {
                        0.22
                    } else {
                        0.
                    },
                    material: if glass {
                        1.
                    } else if node.kind == "SurfaceReference" {
                        2.
                    } else if node.kind == "Slider" {
                        5.
                    } else if button || node.kind == "Card" || node.kind == "Slider" {
                        3.
                    } else {
                        0.
                    },
                });
            }
        }
        for (k, _w) in self.windows.iter().filter(|(_, w)| self.visible_window(w)) {
            let Some(frame) = self.window_frame(k, 0) else {
                continue;
            };
            out.push(Draw {
                id: format!("window:{k}"),
                frame,
                source: Source::Window(k.clone()),
                radius: 16.,
                rim: 1.5,
                border: 0.5,
                material: 2.,
            });
            for (kind, source) in [
                ("grab", Source::Grab(k.clone())),
                ("orb", Source::Orb(k.clone())),
            ] {
                if let Some(frame) = self.companion_frame(k, kind) {
                    out.push(Draw {
                        id: format!("{kind}:{k}"),
                        frame,
                        source,
                        radius: if kind == "grab" {
                            frame.logical[1] / 2.
                        } else {
                            16.
                        },
                        rim: 0.75,
                        border: 0.22,
                        material: 1.,
                    });
                }
            }
            for left in [true, false] {
                let kind = if left { "resize-left" } else { "resize-right" };
                if let Some(frame) = self.companion_frame(k, kind) {
                    out.push(Draw {
                        id: format!("{kind}:{k}"),
                        frame,
                        source: Source::Resize(k.clone(), left),
                        radius: 0.,
                        rim: 0.,
                        border: 0.,
                        material: 0.,
                    });
                }
            }
            if self.focus.as_deref() == Some(k) || self.expanded_orb.as_deref() == Some(k) {
                for (kind, source) in [
                    ("close", Source::Close(k.clone())),
                    ("minimize", Source::Minimize(k.clone())),
                    ("curve", Source::Curve(k.clone())),
                ] {
                    if let Some(frame) = self.companion_frame(k, kind) {
                        out.push(Draw {
                            id: format!("{kind}:{k}"),
                            frame,
                            source,
                            radius: 16.,
                            rim: 0.75,
                            border: 0.22,
                            material: 1.,
                        });
                    }
                }
            }
        }
        let mut decorated = Vec::with_capacity(out.len() * 2);
        for d in out {
            if [1., 2., 3., 5.].contains(&d.material) {
                let w = d.frame.logical[0];
                let h = d.frame.logical[1];
                let elevation = match &d.source {
                    Source::Node(id, key) if [3., 5.].contains(&d.material) => {
                        0.008 + self.lifts.get(&(*id, key.clone())).copied().unwrap_or(0.)
                    }
                    _ => 0.,
                };
                decorated.push(Draw {
                    id: format!("shadow:{}", d.id),
                    frame: Frame {
                        position: add(
                            sub(d.frame.position, scale(d.frame.normal(), elevation + 0.001)),
                            scale(d.frame.down, elevation * 0.6),
                        ),
                        size: [
                            d.frame.size[0] * (w + 48.) / w,
                            d.frame.size[1] * (h + 48.) / h,
                        ],
                        logical: [w + 48., h + 48.],
                        ..d.frame
                    },
                    source: Source::Shadow(Box::new(d.source.clone())),
                    radius: d.radius,
                    rim: 0.,
                    border: 0.,
                    material: 4.,
                });
            }
            decorated.push(d);
        }
        decorated
    }
    fn raster_key(&self, draw: &Draw) -> Value {
        let state = match &draw.source {
            Source::Node(id, node) => {
                let n = &self.peers[id].nodes[node];
                let window = self
                    .windows
                    .get(&format!("{}:{}", n.window_server_generation, n.surface_id));
                // Geometry, interaction state and slider value live in the SSBO.
                // Cache keys contain only glyph/icon content and source pixels.
                json!([
                    n.kind,
                    n.text,
                    n.icon_id,
                    n.style_id,
                    n.enabled,
                    window.map(|w| (w.frame, w.width, w.height))
                ])
            }
            Source::Window(k) => {
                let w = &self.windows[k];
                json!([w.frame, w.width, w.height])
            }
            Source::Grab(_) => Value::Null,
            Source::Orb(_) => json!("more"),
            Source::Close(_) => json!("close"),
            Source::Minimize(_) => json!("minimize"),
            Source::Curve(_) => json!("curve"),
            Source::Resize(_, left) => json!(left),
            Source::Shadow(_) => Value::Null,
        };
        json!([draw.frame.logical, state])
    }
    pub fn panels(&mut self) -> Result<Option<Vec<Panel>>> {
        if !self.dirty {
            return Ok(None);
        }
        self.dirty = false;
        let started = std::time::Instant::now();
        let theme = Theme::default();
        let mut panels = Vec::new();
        let draws = self.draws();
        let live: BTreeSet<_> = draws.iter().map(|d| d.id.clone()).collect();
        self.raster_cache.retain(|id, _| live.contains(id));
        let mut hits = 0usize;
        let mut misses = 0usize;
        for draw in draws {
            let key = self.raster_key(&draw);
            if let Some(pixels) = self
                .raster_cache
                .get(&draw.id)
                .and_then(|entries| entries.iter().find(|(k, _)| k == &key))
                .map(|(_, pixels)| pixels.clone())
            {
                let (width, height) = raster_size(&draw);
                panels.push(Panel {
                    id: draw.id,
                    frame: draw.frame,
                    width,
                    height,
                    rgba: pixels,
                    radius: draw.radius,
                    rim: draw.rim,
                    border: draw.border,
                    material: draw.material,
                });
                hits += 1;
                continue;
            }
            misses += 1;
            let width = draw.frame.logical[0].round().max(1.) as u32;
            let height = draw.frame.logical[1].round().max(1.) as u32;
            let canvas = if matches!(draw.source, Source::Shadow(_)) {
                Canvas::new(1, 1)?
            } else if matches!(draw.source, Source::Window(_)) {
                // Application buffers retain native resolution; Shell vector
                // controls and text use the separate supersampled path.
                Canvas::new(width, height)?
            } else {
                Canvas::for_logical(width, height)?
            };
            match &draw.source {
                Source::Node(id, node) => {
                    let p = &self.peers[id];
                    let n = &p.nodes[node];
                    if n.kind == "SurfaceReference" {
                        if let Some(w) = self
                            .windows
                            .get(&format!("{}:{}", n.window_server_generation, n.surface_id))
                        {
                            canvas.rounded_image(
                                [0., 0., width as f32, height as f32],
                                w.width,
                                w.height,
                                &w.pixels,
                                16.,
                            );
                        }
                    } else {
                        paint_node(
                            &canvas,
                            n,
                            [0., 0., width as f32, height as f32],
                            &theme,
                            false,
                            false,
                            true,
                        );
                    }
                }
                Source::Window(k) => {
                    let w = &self.windows[k];
                    canvas.rounded_image(
                        [0., 0., width as f32, height as f32],
                        w.width,
                        w.height,
                        &w.pixels,
                        16.,
                    );
                }
                Source::Grab(_) => { /* Capsule and grip mark are GPU geometry. */ }
                Source::Resize(_k, left) => {
                    canvas.resize_handle(*left, false);
                }
                Source::Shadow(_) => {}
                source => {
                    let icon = match source {
                        Source::Orb(_) => "ukui-xr-more",
                        Source::Close(_) => "ukui-xr-close",
                        Source::Minimize(_) => "ukui-xr-minimize",
                        Source::Curve(_) => "ukui-xr-curve",
                        _ => "",
                    };
                    canvas.symbolic([5., 5., 22., 22.], icon, theme.colour("foreground"));
                }
            }
            let pixels: Arc<[u8]> = canvas.rgba().into();
            let variants = if matches!(&draw.source, Source::Node(id,node) if ["Button","Slider"].contains(&self.peers[id].nodes[node].kind.as_str()))
            {
                3
            } else {
                1
            };
            let entries = self.raster_cache.entry(draw.id.clone()).or_default();
            while entries.len() >= variants {
                entries.pop_front();
            }
            entries.push_back((key, pixels.clone()));
            panels.push(Panel {
                id: draw.id,
                frame: draw.frame,
                width: canvas.width,
                height: canvas.height,
                rgba: pixels,
                radius: draw.radius,
                rim: draw.rim,
                border: draw.border,
                material: draw.material,
            });
        }
        let elapsed = started.elapsed();
        if crate::ui_profile() || elapsed.as_millis() >= 8 {
            eprintln!(
                "XR_UI_RASTER panels={} hits={hits} misses={misses} elapsed_us={}",
                panels.len(),
                elapsed.as_micros()
            );
        }
        ensure!(panels.len() <= 2048, "draw limit exceeded");
        Ok(Some(panels))
    }
    pub fn panel_poses(&self) -> Vec<Geometry> {
        self.draws()
            .into_iter()
            .map(|d| {
                let original = if let Source::Shadow(base) = &d.source {
                    base.as_ref()
                } else {
                    &d.source
                };
                let (anchor, group, mut layer) = match original {
                    Source::Node(id, node) => {
                        let p = &self.peers[id];
                        let (base, group) = self.node_base(p, &p.nodes[node]);
                        (
                            base.position,
                            group,
                            dot(sub(d.frame.position, base.position), base.normal()),
                        )
                    }
                    Source::Window(k) => {
                        let (group, anchor, layer) = self.window_group(k);
                        (anchor, group, layer)
                    }
                    Source::Grab(k)
                    | Source::Orb(k)
                    | Source::Close(k)
                    | Source::Minimize(k)
                    | Source::Curve(k)
                    | Source::Resize(k, _) => {
                        let (group, anchor, layer) = self.window_group(k);
                        (anchor, group, layer + 0.008 + self.companion_lifts.get(d.id.strip_prefix("shadow:").unwrap_or(&d.id)).copied().unwrap_or(0.))
                    }
                    Source::Shadow(_) => unreachable!(),
                };
                if matches!(d.source, Source::Shadow(_)) && matches!(original, Source::Window(_)) {
                    layer -= 0.01;
                }
                let theme = Theme::default();
                let (tint, controls) = match original {
                    Source::Node(id,key) => {
                        let n = &self.peers[id].nodes[key];
                        let hovered = self.hovers.values().any(|v| v == &(*id,key.clone()));
                        let pressed = self.captures.values().any(|c| matches!(&c.target,Target::Node(owner,node) if owner==id && node==key));
                        let tint = theme.colour(&n.style_id);
                        (tint, [if hovered {1.} else {0.}, if pressed {1.} else {0.}, ((n.value-n.min)/(n.max-n.min).max(0.0001)).clamp(0.,1.), 0.])
                    }
                    _ => {
                        let hovered = self.companion_hovers.values().any(|id| id == &d.id);
                        let pressed = self.captures.values().any(|c| match (&c.target,original) {
                            (Target::Move(a),Source::Grab(b)) | (Target::Orb(a),Source::Orb(b)) |
                            (Target::Close(a),Source::Close(b)) | (Target::Minimize(a),Source::Minimize(b)) |
                            (Target::Curve(a),Source::Curve(b)) => a==b,
                            _ => false,
                        });
                        (theme.colour("card"), [if hovered {1.} else {0.},if pressed {1.} else {0.},0.,if matches!(original,Source::Grab(_)){1.}else{0.}])
                    }
                };
                Geometry {
                    tint, controls,
                    id: d.id,
                    frame: d.frame,
                    radius: d.radius,
                    rim: d.rim,
                    border: d.border,
                    material: d.material,
                    anchor,
                    group,
                    layer,
                }
            })
            .collect()
    }
    pub fn animate(&mut self, dt: f32) -> bool {
        let mut changed = false;
        for p in self.peers.values() {
            for n in p
                .nodes
                .values()
                .filter(|n| ["Button", "Slider", "SurfaceReference"].contains(&n.kind.as_str()))
            {
                let id = (p.id, n.node_id.clone());
                let hovered = self.hovers.values().any(|v| v == &id);
                let pressed = self.captures.values().any(|c| matches!(&c.target, Target::Node(owner,key) if (*owner,key)==(p.id,&n.node_id)));
                let value = self.lifts.entry(id).or_insert(0.);
                let target = if pressed {
                    -0.006
                } else if hovered {
                    0.010
                } else {
                    0.
                };
                let step = (target - *value) * (1. - (-dt / 0.075).exp());
                if step.abs() > 0.000001 {
                    *value += step;
                    changed = true;
                } else {
                    *value = target;
                }
            }
        }
        let controls: Vec<_> = self
            .panel_poses()
            .into_iter()
            .filter(|g| {
                ["grab:", "orb:", "close:", "minimize:", "curve:"]
                    .iter()
                    .any(|p| g.id.starts_with(p))
            })
            .collect();
        self.companion_lifts
            .retain(|id, _| controls.iter().any(|g| &g.id == id));
        for g in controls {
            let target = if g.controls[1] > 0. {
                -0.006
            } else if g.controls[0] > 0. {
                0.010
            } else {
                0.
            };
            let value = self.companion_lifts.entry(g.id).or_insert(0.);
            let next = *value + (target - *value) * (1. - (-dt / 0.075).exp());
            let next = if (target - next).abs() < 0.000001 {
                target
            } else {
                next
            };
            changed |= next != *value;
            *value = next;
        }
        changed
    }
    pub fn layout(&self, offset: usize, limit: usize) -> Value {
        let draws = self.draws();
        let items:Vec<_>=draws.iter().skip(offset).take(limit.min(32)).map(|d|json!({"id":d.id,"position_m":d.frame.position,"right":d.frame.right,"down":d.frame.down,"normal":d.frame.normal(),"size_m":d.frame.size,"logical_size":d.frame.logical,"curve_radius_m":d.frame.curve,"corner_radius_px":d.radius,"rim_width_px":d.rim,"rim_alpha":d.border,"material":d.material,"rim_alpha_top_left":if d.material==1. {0.50}else{d.border},"rim_alpha_bottom_right":if d.material==1. {0.16}else{d.border},"extrusion_depth_m":match d.material as u32 {1=>0.006,2=>0.003,3=>0.002,_=>0.},"shadow_spread_px":if d.material==4. {24}else{0}})).collect();
        json!({"total":draws.len(),"offset":offset,"items":items})
    }
    fn target(draw: &Draw) -> Option<Target> {
        match &draw.source {
            Source::Node(id, node) => Some(Target::Node(*id, node.clone())),
            Source::Window(k) => Some(Target::Window(k.clone())),
            Source::Grab(k) => Some(Target::Move(k.clone())),
            Source::Orb(k) => Some(Target::Orb(k.clone())),
            Source::Close(k) => Some(Target::Close(k.clone())),
            Source::Minimize(k) => Some(Target::Minimize(k.clone())),
            Source::Curve(k) => Some(Target::Curve(k.clone())),
            Source::Resize(k, left) => Some(Target::Resize(k.clone(), *left)),
            Source::Shadow(_) => None,
        }
    }
    fn capture_frame(&self, c: &Capture) -> Option<Frame> {
        match &c.target {
            Target::Node(id, node) => self
                .peers
                .get(id)
                .and_then(|p| p.nodes.get(node).and_then(|n| self.node_frame(p, n))),
            Target::Window(k) => self.window_frame(k, 0),
            Target::Close(k) => self.companion_frame(k, "close"),
            Target::Minimize(k) => self.companion_frame(k, "minimize"),
            Target::Curve(k) => self.companion_frame(k, "curve"),
            Target::Orb(k) => self.companion_frame(k, "orb"),
            Target::Move(k) => self.companion_frame(k, "grab"),
            Target::Resize(_, _) => c.resize_frame,
        }
    }
    fn hit_local(
        &self,
        frame: Frame,
        xy: [f32; 2],
        distance: f32,
        radius: f32,
        interactive: bool,
    ) -> Option<[f32; 2]> {
        if frame.inside_rounded(xy, radius) {
            return Some(xy);
        }
        if !interactive {
            return None;
        }
        let toward_eye = self
            .horizon_head
            .map(|(eye, _)| sub(eye, frame.point(xy, 0.)));
        let distance = toward_eye.map_or(distance, |v| dot(v, v).sqrt());
        let minimum = xr_spatial_kit::min_ray_target_m(distance);
        let slop = std::array::from_fn::<_, 2, _>(|i| {
            let axis = if i == 0 { frame.right } else { frame.down };
            let projected = toward_eye.map_or(1., |v| {
                (1. - (dot(axis, v) / distance.max(0.001)).powi(2))
                    .max(0.01)
                    .sqrt()
            });
            ((minimum / projected - frame.size[i]).max(0.) * 0.5) * frame.logical[i] / frame.size[i]
        });
        if xy[0] >= -slop[0]
            && xy[1] >= -slop[1]
            && xy[0] <= frame.logical[0] + slop[0]
            && xy[1] <= frame.logical[1] + slop[1]
        {
            Some([
                xy[0].clamp(0.001, frame.logical[0] - 0.001),
                xy[1].clamp(0.001, frame.logical[1] - 0.001),
            ])
        } else {
            None
        }
    }
    // One occlusion/rounding/curve query serves input and visible feedback.
    fn nearest_ray_pick(&self, origin: [f32; 3], direction: [f32; 3]) -> Option<RayPick> {
        let mut pick = None;
        let mut nearest = f32::MAX;
        let mut nearest_direct = false;
        let mut nearest_edge = f32::MAX;
        for draw in self.draws() {
            if matches!(draw.source, Source::Shadow(_)) {
                continue;
            }
            let target = match &draw.source {
                Source::Node(id, node) => {
                    let n = &self.peers[id].nodes[node];
                    if ["Text", "Icon"].contains(&n.kind.as_str()) {
                        continue;
                    }
                    if n.enabled
                        && ["Button", "Slider", "SurfaceReference"].contains(&n.kind.as_str())
                    {
                        Self::target(&draw)
                    } else {
                        // Glass cards and disabled controls occlude pointer input too.
                        None
                    }
                }
                _ => Self::target(&draw),
            };
            let hit_frame = if let Source::Node(id, node) = &draw.source {
                let p = &self.peers[id];
                if p.nodes[node].kind == "Slider" {
                    self.node_frame(p, &p.nodes[node]).unwrap_or(draw.frame)
                } else {
                    draw.frame
                }
            } else {
                draw.frame
            };
            if let Some((xy, t)) = hit_frame.project(origin, direction) {
                let effective = self.hit_local(hit_frame, xy, t, draw.radius, target.is_some());
                let direct = hit_frame.inside_rounded(xy, draw.radius);
                let edge = hit_frame.rounded_distance_m(xy, draw.radius);
                let coplanar = (t - nearest).abs() < 0.0001;
                let better = if coplanar {
                    (direct && !nearest_direct)
                        || (direct == nearest_direct && !direct && edge < nearest_edge)
                } else {
                    t < nearest
                };
                if better && effective.is_some() {
                    let xy = effective.unwrap();
                    nearest = t;
                    nearest_direct = direct;
                    nearest_edge = edge;
                    let companion =
                        (!matches!(draw.source, Source::Node(_, _) | Source::Window(_)))
                            .then(|| draw.id.clone());
                    pick = Some(RayPick {
                        target,
                        xy,
                        distance: t,
                        normal: hit_frame.normal_at(xy),
                        companion,
                    });
                }
            }
        }
        pick
    }

    pub fn pointer_target(
        &self,
        source: &str,
        origin: [f32; 3],
        direction: [f32; 3],
    ) -> Option<String> {
        if self.captures.contains_key(source) {
            return Some(format!("capture:{source}"));
        }
        let pick = self.nearest_ray_pick(sub(origin, self.ui_offset), direction)?;
        match pick.target? {
            Target::Node(owner, id) => Some(format!("node:{owner}:{id}")),
            Target::Window(id) => Some(format!("window:{id}")),
            _ => pick.companion,
        }
    }
    pub fn ray_feedback(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        source: &str,
    ) -> Option<crate::spatial::RayFeedback> {
        if !self.ui_anchor_ready {
            return None;
        }
        let local_origin = sub(origin, self.ui_offset);
        let (distance, normal, interactive, captured) = if let Some(c) = self.captures.get(source) {
            let frame = self.capture_frame(c)?;
            if let Some(g) = &c.grab {
                (g.distance, frame.normal(), true, true)
            } else {
                let (xy, distance) = frame.project(local_origin, direction)?;
                (distance, frame.normal_at(xy), true, true)
            }
        } else {
            let hit = self.nearest_ray_pick(local_origin, direction)?;
            (hit.distance, hit.normal, hit.target.is_some(), false)
        };
        if !distance.is_finite() || distance <= 0. {
            return None;
        }
        Some(crate::spatial::RayFeedback {
            distance_m: distance,
            point_m: add(origin, scale(direction, distance)),
            normal,
            interactive,
            captured,
        })
    }

    pub fn ray(
        &mut self,
        origin: [f32; 3],
        direction: [f32; 3],
        phase: &str,
        source: &str,
    ) -> Result<()> {
        ensure!(
            ["hover", "press", "drag", "release"].contains(&phase),
            "unknown pointer phase"
        );
        if !self.ui_anchor_ready {
            return Ok(());
        }
        let origin = sub(origin, self.ui_offset);
        if let Some(c) = self.captures.get(source).cloned() {
            if phase != "press" && phase != "hover" {
                if let Target::Move(ref key) = c.target {
                    self.update_grab(source, key, origin, direction);
                    return self.pointer(Some((c.target, c.start)), phase, source);
                }
                if let Some(frame) = self.capture_frame(&c) {
                    if let Some((xy, distance)) = frame.project(origin, direction) {
                        let xy = self.hit_local(frame, xy, distance, 0., true).unwrap_or(xy);
                        return self.pointer(Some((c.target, xy)), phase, source);
                    }
                }
                // Even if the ray no longer intersects, release the actual
                // capture owner; never redirect a release to a different node.
                return self.pointer(Some((c.target, [-1., -1.])), phase, source);
            }
        }
        let pick = self.nearest_ray_pick(origin, direction);
        let grab_point = pick
            .as_ref()
            .map(|p| (p.distance, add(origin, scale(direction, p.distance))));
        let (hit, hover_companion) = pick
            .map(|p| (p.target.map(|t| (t, p.xy)), p.companion))
            .unwrap_or((None, None));
        if phase == "hover" {
            if let Some(k) = self.expanded_orb.clone() {
                let within = self.companion_frame(&k, "orb").is_some_and(|f| {
                    let region = f.rect([-124., -6., 156., 40.], 0.);
                    region
                        .project(origin, direction)
                        .is_some_and(|(xy, _)| region.inside(xy))
                });
                if !within {
                    self.expanded_orb = None;
                    self.dirty = true;
                }
            }
            if self.companion_hovers.get(source) != hover_companion.as_ref() {
                if let Some(v) = hover_companion {
                    self.companion_hovers.insert(source.into(), v);
                } else {
                    self.companion_hovers.remove(source);
                }
                self.dirty = true;
            }
            if !matches!(&hit, Some((Target::Node(_, _), _)))
                && self.hovers.remove(source).is_some()
            {
                self.dirty = true;
            }
        }
        self.pointer(hit, phase, source)?;
        if phase == "press" {
            if let Some((distance, point)) = grab_point {
                let key = self.captures.get(source).and_then(|c| {
                    if let Target::Move(k) = &c.target {
                        Some(k.clone())
                    } else {
                        None
                    }
                });
                if let Some(key) = key {
                    if let Some(f) = self.window_frame(&key, 0) {
                        let delta = sub(point, f.position);
                        let local_anchor = [
                            dot(delta, f.right),
                            dot(delta, f.down),
                            dot(delta, f.normal()),
                        ];
                        self.captures.get_mut(source).unwrap().grab = Some(Grab {
                            distance,
                            point,
                            local_anchor,
                        });
                        let grabs: Vec<_> = self
                            .captures
                            .values()
                            .filter(|c| matches!(&c.target,Target::Move(k) if k==&key))
                            .filter_map(|c| c.grab.as_ref())
                            .collect();
                        if grabs.len() == 2 {
                            let span = dot(
                                sub(grabs[0].point, grabs[1].point),
                                sub(grabs[0].point, grabs[1].point),
                            )
                            .sqrt();
                            if span > 0.02 {
                                self.bimanual.insert(
                                    key.clone(),
                                    Bimanual {
                                        span,
                                        size: self.windows[&key].size,
                                        logical: Self::logical(&self.windows[&key]),
                                        anchor: scale(
                                            add(grabs[0].local_anchor, grabs[1].local_anchor),
                                            0.5,
                                        ),
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish_grab(&mut self, key: &str, commit: bool) -> Result<()> {
        if let Some(b) = self.bimanual.remove(key) {
            if commit {
                if let Some(w) = self.windows.get(key) {
                    let logical: [u32; 2] = std::array::from_fn(|i| {
                        (b.logical[i] * w.size[i] / b.size[i]).round() as u32
                    });
                    self.window_intent(
                        json!({"intent":"resize", "surface_id":w.state["surface_id"],
                        "window_server_generation":w.state["window_server_generation"],
                        "width":logical[0], "height":logical[1]}),
                    )?;
                    self.changed();
                }
            } else if let Some(w) = self.windows.get_mut(key) {
                w.size = b.size;
            }
        }
        let Some(f) = self.window_frame(key, 0) else {
            return Ok(());
        };
        for c in self
            .captures
            .values_mut()
            .filter(|c| matches!(&c.target,Target::Move(k) if k==key))
        {
            if let Some(g) = c.grab.as_mut() {
                let delta = sub(g.point, f.position);
                g.local_anchor = [
                    dot(delta, f.right),
                    dot(delta, f.down),
                    dot(delta, f.normal()),
                ];
            }
        }
        Ok(())
    }
    /// Joystick Y changes reach while physical controller translation naturally
    /// pushes/pulls the grab point. Bounds refer to distance from the eyes.
    pub fn grab_depth(&mut self, source: &str, delta: f32) -> bool {
        if let Some(g) = self.captures.get_mut(source).and_then(|c| c.grab.as_mut()) {
            g.distance = (g.distance + delta).clamp(0.1, 5.);
            true
        } else {
            false
        }
    }
    fn update_grab(&mut self, source: &str, key: &str, origin: [f32; 3], direction: [f32; 3]) {
        let Some((eye, _)) = self.horizon_head else {
            return;
        };
        let Some(g) = self.captures.get_mut(source).and_then(|c| c.grab.as_mut()) else {
            return;
        };
        g.point = add(origin, scale(direction, g.distance));
        let mut point = g.point;
        let mut anchor = g.local_anchor;
        let mut size = self.windows[key].size;
        if let Some(b) = self.bimanual.get(key) {
            let grabs: Vec<_> = self
                .captures
                .values()
                .filter(|c| matches!(&c.target,Target::Move(k) if k==key))
                .filter_map(|c| c.grab.as_ref())
                .collect();
            if grabs.len() == 2 {
                let d = sub(grabs[0].point, grabs[1].point);
                let ratio = (dot(d, d).sqrt() / b.span).clamp(
                    (0.3 / b.size[0])
                        .max(0.2 / b.size[1])
                        .max(160. / b.logical[0])
                        .max(120. / b.logical[1]),
                    (3. / b.size[0])
                        .min(2.5 / b.size[1])
                        .min(2048. / b.logical[0])
                        .min(1536. / b.logical[1]),
                );
                point = scale(add(grabs[0].point, grabs[1].point), 0.5);
                anchor = scale(b.anchor, ratio);
                size = b.size.map(|v| v * ratio);
            }
        }
        let w = self.windows.get_mut(key).unwrap();
        let mut position = w.position;
        for _ in 0..3 {
            let rotation = crate::spatial::user_facing(position, eye);
            let f = Frame::new(position, size, [1.; 2], rotation);
            position = sub(
                point,
                add(
                    scale(f.right, anchor[0]),
                    add(scale(f.down, anchor[1]), scale(f.normal(), anchor[2])),
                ),
            );
        }
        let delta = sub(position, eye);
        let distance = dot(delta, delta).sqrt();
        if distance < 0.0001 {
            return;
        }
        w.position = add(eye, scale(delta, distance.clamp(0.6, 4.) / distance));
        w.rotation = crate::spatial::user_facing(w.position, eye);
        w.size = size;
        self.dirty = true;
    }
}
