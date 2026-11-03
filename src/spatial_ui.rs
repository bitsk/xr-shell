// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::spatial::{Frame, add, sub, dot};
#[derive(Clone)]
enum Source {
    Node(u64, String),
    Window(String),
    Grab(String),
    Orb(String),
    Close(String),
    Minimize(String),
    Curve(String),
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
}
impl Desktop {
    fn node_base(p: &Peer, node: &Node) -> (Frame, String) {
        let mut parent = node.parent_id.as_str();
        for _ in 0..512 {
            if parent == "root" { break; }
            let Some(n) = p.nodes.get(parent) else { break; };
            if n.spatial_panel {
                return (Frame::new(n.position_m, n.size_m, n.logical_size, n.rotation_degrees), format!("component:{}:{}", p.id, n.node_id));
            }
            parent = &n.parent_id;
        }
        (Self::root_frame(p), format!("component:{}:root", p.id))
    }
    fn window_group(&self, key: &str) -> (String, [f32; 3], f32) {
        let mut root = key.to_owned();
        let mut depth = 0;
        for _ in 0..16 {
            let w = &self.windows[&root];
            if w.state["role"].as_str().is_none_or(|r| r == "toplevel") { break; }
            let Some(parent) = w.state["parent_surface_id"].as_str() else { break; };
            let key = format!("{}:{parent}", w.state["window_server_generation"].as_str().unwrap_or(""));
            if !self.windows.contains_key(&key) { break; }
            root = key;
            depth += 1;
        }
        (format!("window:{root}"), self.window_frame(&root, 0).map(|f| f.position).unwrap_or([0.; 3]), depth as f32 * 0.004)
    }
    fn root_frame(p: &Peer) -> Frame {
        let mut rotation = p.root.rotation;
        rotation[1] += p.root.yaw.to_degrees();
        Frame::new(p.root.position, p.root.size, p.root.logical, rotation)
    }
    fn node_frame(&self, p: &Peer, node: &Node) -> Option<Frame> {
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
        let mut frame = Self::root_frame(p);
        for _ in 0..512 {
            if parent == "root" {
                break;
            }
            let n = p.nodes.get(parent)?;
            if !n.visible {
                return None;
            }
            if n.spatial_panel {
                frame = Frame::new(n.position_m, n.size_m, n.logical_size, n.rotation_degrees);
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
    fn window_frame(&self, k: &str, level: u32) -> Option<Frame> {
        let w = self.windows.get(k)?;
        let logical = Self::logical(w);
        let mut frame = Frame::new(w.position, w.size, logical, [0.; 3]);
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
            && w.state["mapped"] != false
            && w.state["workspace"].as_u64().unwrap_or(0) == self.workspace as u64
            && !w.pixels.is_empty()
    }
    fn companion_frame(&self, k: &str, kind: &str) -> Option<Frame> {
        let w = self.windows.get(k)?;
        if !self.visible_window(w) || w.state["role"].as_str().is_some_and(|r| r != "toplevel") {
            return None;
        }
        let frame = self.window_frame(k, 0)?;
        let unit = frame.size[0] / frame.logical[0];
        let grabbed = self.companion_hover.as_deref() == Some(&format!("grab:{k}"));
        let height = if grabbed { 12. } else { 8. };
        let bar_width = frame.logical[0] * 0.35;
        let bar_x = (frame.logical[0] - bar_width) / 2.;
        // Each companion is an independent world-space surface, 4cm below content.
        let y = frame.logical[1] + 0.04 / (frame.size[1] / frame.logical[1]) + 4.;
        let rect = match kind {
            "grab" => [bar_x, y - height / 2., bar_width, height],
            "orb" => [bar_x - 12. - 28., y - 14., 28., 28.],
            "close" => [bar_x - 12. - 28. - 40., y - 14., 28., 28.],
            "minimize" => [bar_x - 12. - 28. - 80., y - 14., 28., 28.],
            "curve" => [bar_x - 12. - 28. - 120., y - 14., 28., 28.],
            _ => return None,
        };
        let mut plane = frame;
        plane.curve = 0.;
        let mut out = plane.rect(rect, 0.005);
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
                let glass = node.kind == "Card"
                    && ["dock.background", "panel.background"].contains(&node.style_id.as_str());
                let radius = if node.kind == "Text" || node.kind == "Icon" || node.kind == "Slider"
                {
                    0.
                } else if node.style_id == "dock.background" {
                    32.
                } else if node.style_id.starts_with("dock.") {
                    node.rect[3] / 2.
                } else if node.style_id == "keyboard.key" {
                    8.
                } else {
                    16.
                };
                out.push(Draw {
                    id: format!("node:{}:{}", p.id, node.node_id),
                    frame,
                    source: Source::Node(p.id, node.node_id.clone()),
                    radius,
                    rim: if button || glass { 1. } else { 0. },
                    border: if button {
                        0.18
                    } else if glass {
                        0.22
                    } else {
                        0.
                    },
                    material: if glass { 1. } else { 0. },
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
                border: 0.35,
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
                            14.
                        },
                        rim: if kind == "grab" { 0. } else { 1. },
                        border: 0.22,
                        material: if kind == "grab" { 0. } else { 1. },
                    });
                }
            }
            if self.expanded_orb.as_deref() == Some(k) {
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
                            radius: 14.,
                            rim: 1.,
                            border: 0.22,
                            material: 1.,
                        });
                    }
                }
            }
        }
        out
    }
    pub fn panels(&mut self) -> Result<Option<Vec<Panel>>> {
        if !self.dirty {
            return Ok(None);
        }
        self.dirty = false;
        let theme = Theme::default();
        let mut panels = Vec::new();
        for draw in self.draws() {
            let width = draw.frame.logical[0].round().max(1.) as u32;
            let height = draw.frame.logical[1].round().max(1.) as u32;
            let canvas = Canvas::new(width, height)?;
            match &draw.source {
                Source::Node(id, node) => {
                    let p = &self.peers[id];
                    let n = &p.nodes[node];
                    let pressed = self.capture.as_ref().is_some_and(
                        |c| matches!(&c.target,Target::Node(owner,key)if owner==id&&key==node),
                    );
                    let hovered = self
                        .hover
                        .as_ref()
                        .is_some_and(|(owner, key)| owner == id && key == node);
                    if n.kind == "SurfaceReference" {
                        if let Some(w) = self
                            .windows
                            .get(&format!("{}:{}", n.window_server_generation, n.surface_id))
                        {
                            canvas.image(
                                [0., 0., width as f32, height as f32],
                                w.width,
                                w.height,
                                &w.pixels,
                            );
                        }
                    } else {
                        paint_node(
                            &canvas,
                            n,
                            [0., 0., width as f32, height as f32],
                            &theme,
                            pressed,
                            hovered,
                            true,
                        );
                    }
                }
                Source::Window(k) => {
                    let w = &self.windows[k];
                    canvas.image(
                        [0., 0., width as f32, height as f32],
                        w.width,
                        w.height,
                        &w.pixels,
                    );
                }
                Source::Grab(k) => {
                    let hover = self.companion_hover.as_deref() == Some(&format!("grab:{k}"));
                    canvas.rounded(
                        [0., 0., width as f32, height as f32],
                        [1., 1., 1., if hover { 1. } else { 0.65 }],
                        height as f32 / 2.,
                    );
                }
                source => {
                    let id = match source {
                        Source::Orb(k) => format!("orb:{k}"),
                        Source::Close(k) => format!("close:{k}"),
                        Source::Minimize(k) => format!("minimize:{k}"),
                        Source::Curve(k) => format!("curve:{k}"),
                        _ => unreachable!(),
                    };
                    let hover = self.companion_hover.as_deref() == Some(&id);
                    canvas.rounded(
                        [0., 0., 28., 28.],
                        if hover {
                            [0.216, 0.565, 0.98, 1.]
                        } else {
                            [22. / 255., 26. / 255., 35. / 255., 0.78]
                        },
                        14.,
                    );
                    let text = match source {
                        Source::Orb(_) => "···",
                        Source::Close(_) => "×",
                        Source::Minimize(_) => "−",
                        Source::Curve(_) => "⌒",
                        _ => "",
                    };
                    canvas.text([3., 1., 24., 27.], text, [1.; 4], 20.);
                }
            }
            panels.push(Panel {
                id: draw.id,
                frame: draw.frame,
                width,
                height,
                rgba: canvas.rgba(),
                radius: draw.radius,
                rim: draw.rim,
                border: draw.border,
                material: draw.material,
            });
        }
        ensure!(panels.len() <= 2048, "draw limit exceeded");
        Ok(Some(panels))
    }
    pub fn panel_poses(&self)->Vec<Geometry>{self.draws().into_iter().map(|d|{
        let(anchor,group,layer)=match &d.source {
            Source::Node(id,node)=>{let p=&self.peers[id];let(base,group)=Self::node_base(p,&p.nodes[node]);(base.position,group,dot(sub(d.frame.position,base.position),base.normal()))},
            Source::Window(k)=>{let(group,anchor,layer)=self.window_group(k);(anchor,group,layer)},
            Source::Grab(k)|Source::Orb(k)|Source::Close(k)|Source::Minimize(k)|Source::Curve(k)=>{let(group,anchor,layer)=self.window_group(k);(anchor,group,layer+0.005)}
        };
        Geometry{id:d.id,frame:d.frame,radius:d.radius,rim:d.rim,border:d.border,material:d.material,anchor,group,layer}
    }).collect()}
    pub fn animate(&mut self, dt: f32) -> bool {
        let mut changed = false;
        for p in self.peers.values() {
            for n in p
                .nodes
                .values()
                .filter(|n| ["Button", "Slider", "SurfaceReference"].contains(&n.kind.as_str()))
            {
                let id = (p.id, n.node_id.clone());
                let hovered = self.hover.as_ref() == Some(&id);
                let value = self.lifts.entry(id).or_insert(0.);
                let target = if hovered { 0.012 } else { 0. };
                let step = (target - *value) * (1. - (-dt / 0.075).exp());
                if step.abs() > 0.000001 {
                    *value += step;
                    changed = true;
                } else {
                    *value = target;
                }
            }
        }
        changed
    }
    pub fn layout(&self, offset: usize, limit: usize) -> Value {
        let draws = self.draws();
        let items:Vec<_>=draws.iter().skip(offset).take(limit.min(32)).map(|d|json!({"id":d.id,"position_m":d.frame.position,"right":d.frame.right,"down":d.frame.down,"normal":d.frame.normal(),"size_m":d.frame.size,"logical_size":d.frame.logical,"curve_radius_m":d.frame.curve,"corner_radius_px":d.radius,"rim_width_px":d.rim,"rim_alpha":d.border})).collect();
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
            Target::Move(k) => {
                let mut f = self.companion_frame(k, "grab")?;
                let w = self.windows.get(k)?;
                f.position = add(f.position, sub(c.position, w.position));
                Some(f)
            }
            Target::Resize(k) => self.window_frame(k, 0),
        }
    }
    pub fn ray(
        &mut self,
        origin: [f32; 3],
        direction: [f32; 3],
        phase: &str,
        source: &str,
    ) -> Result<()> {
        if phase == "hover" && self.capture.as_ref().is_some_and(|c| c.source != source) {
            return Ok(());
        }
        if let Some(c) = self.capture.clone() {
            if c.source == source && phase != "press" && phase != "hover" {
                if let Some(frame) = self.capture_frame(&c) {
                    if let Some((xy, _)) = frame.project(origin, direction) {
                        return self.pointer(Some((c.target, xy)), phase, source);
                    }
                }
            }
        }
        let mut hit = None;
        let mut nearest = f32::MAX;
        let mut hover_companion = None;
        for draw in self.draws() {
            let target = match &draw.source {
                Source::Node(id, node) => {
                    let n = &self.peers[id].nodes[node];
                    if !n.enabled
                        || !["Button", "Slider", "SurfaceReference"].contains(&n.kind.as_str())
                    {
                        continue;
                    }
                    Self::target(&draw)
                }
                _ => Self::target(&draw),
            };
            if let Some((xy, t)) = draw.frame.project(origin, direction) {
                if t < nearest && draw.frame.inside(xy) {
                    if draw.radius >= draw.frame.logical[1] / 2. - 0.1
                        && draw.frame.logical[0] <= draw.frame.logical[1] + 0.1
                    {
                        let dx = xy[0] - draw.frame.logical[0] / 2.;
                        let dy = xy[1] - draw.frame.logical[1] / 2.;
                        if dx * dx + dy * dy > (draw.frame.logical[0] / 2.).powi(2) {
                            continue;
                        }
                    }
                    nearest = t;
                    hit = target.map(|t| (t, xy));
                    if !matches!(draw.source, Source::Node(_, _) | Source::Window(_)) {
                        hover_companion = Some(draw.id.clone());
                    }
                }
            }
        }
        if phase == "hover" {
            if self.companion_hover != hover_companion {
                self.companion_hover = hover_companion;
                self.dirty = true;
            }
            if !matches!(&hit, Some((Target::Node(_, _), _))) && self.hover.take().is_some() {
                self.dirty = true;
            }
        }
        self.pointer(hit, phase, source)
    }
}
