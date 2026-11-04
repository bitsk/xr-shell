// SPDX-License-Identifier: GPL-3.0-or-later
use crate::raster::{Canvas, paint_node};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    os::{
        fd::AsRawFd,
        unix::{
            fs::PermissionsExt,
            fs::{DirBuilderExt, MetadataExt},
        },
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use xr_shell_protocols::{CAPABILITIES, Node, Packet, SeqPacket};
use xr_spatial_kit::Theme;

#[derive(Clone)]
struct Root {
    logical: [f32; 2],
    size: [f32; 2],
    position: [f32; 3],
    policy: String,
    rotation: [f32; 3],
}
impl Default for Root {
    fn default() -> Self {
        Self {
            logical: [800., 500.],
            size: [0.8, 0.5],
            position: [0., 1.5, -1.5],
            policy: "world_locked".into(),
            rotation: [0.; 3],
        }
    }
}
struct Peer {
    socket: SeqPacket,
    id: u64,
    generation: String,
    role: String,
    capabilities: Vec<String>,
    revision: u64,
    nodes: BTreeMap<String, Node>,
    root: Root,
    used_buffers: BTreeSet<String>,
}
fn message_capability(kind: &str) -> &'static str {
    match kind {
        "GetSnapshot" | "Snapshot" | "StateChanged" | "WindowState" | "WindowRemoved"
        | "WindowAccepted" => "snapshot.v1",
        "GetScene" | "ScenePage" | "GetLayout" | "LayoutPage" | "DebugInput" => "debug.v1",
        "SceneTransaction" | "SceneApplied" | "SceneRejected" => "scene.v1",
        "InputEvent" => "input.v1",
        "ShmBufferCommit" | "BufferRelease" => "shm.v1",
        "FocusChanged" | "FocusApplied" | "SurfaceInput" => "focus.v1",
        "WindowIntent" | "RequestResult" => "window.v1",
        "ComponentIntent" | "ComponentCommand" | "SystemState" | "TextCommit" | "KeyIntent" => {
            "system.v1"
        }
        "WorkspaceIntent" | "WorkspaceApplied" => "workspace.v1",
        "ClipboardSet" | "ClipboardChanged" | "GetClipboard" => "clipboard.v1",
        // Error, SessionEnding and Cancel remain core control messages. In
        // particular, withdrawing a capture must not depend on optional input.
        _ => "core.v1",
    }
}
fn accepts_event(capabilities: &[String], kind: &str) -> bool {
    let required = message_capability(kind);
    required == "core.v1" || capabilities.iter().any(|cap| cap == required)
}
impl Peer {
    // Replies retain their request ID, including handshake and capability errors.
    fn send(&self, session: &str, kind: &str, request: &str, payload: Value) -> Result<()> {
        self.socket.send(&json!({"type":kind,"request_id":request,"session_generation":session,"component_id":self.id.to_string(),"component_generation":self.generation,"payload":payload}),&[])
    }
    fn send_event(&self, session: &str, kind: &str, payload: Value) -> Result<bool> {
        if !accepts_event(&self.capabilities, kind) {
            return Ok(false);
        }
        self.send(session, kind, "0", payload)?;
        Ok(true)
    }
    fn error(&self, session: &str, request: &str, code: &str, message: &str) -> Result<()> {
        self.send(
            session,
            "Error",
            request,
            json!({"code":code,"message":message}),
        )
    }
}
#[derive(Clone)]
struct Window {
    owner: u64,
    state: Value,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    position: [f32; 3],
    size: [f32; 2],
    frame: u64,
    minimized: bool,
    curved: bool,
}
#[derive(Clone)]
enum Target {
    Node(u64, String),
    Window(String),
    Close(String),
    Move(String),
    Resize(String, bool),
    Orb(String),
    Minimize(String),
    Curve(String),
}
#[derive(Clone)]
struct Capture {
    target: Target,
    /// Real application surface bound at press, even when a UI node embeds it.
    surface: Option<String>,
    resize_frame: Option<crate::spatial::Frame>,
    size: [f32; 2],
    logical: [f32; 2],
    id: String,
    interaction: String,
    source: String,
    start: [f32; 2],
    position: [f32; 3],
}
#[derive(Clone)]
pub struct Panel {
    pub id: String,
    pub frame: crate::spatial::Frame,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub radius: f32,
    pub rim: f32,
    pub border: f32,
    pub material: f32,
}
pub struct Desktop {
    listener: SeqPacket,
    active_peer: Option<(u64, String, Value, Vec<String>)>,
    outbox: RefCell<Vec<(u64, String, Value)>>,
    pending_requests: BTreeMap<(u64, String), String>,
    path: PathBuf,
    inode: u64,
    session: String,
    next_peer: u64,
    peers: BTreeMap<u64, Peer>,
    windows: BTreeMap<String, Window>,
    revision: u64,
    focus_epoch: u64,
    focus: Option<String>,
    focus_applied: bool,
    workspace: u32,
    system: Value,
    system_owner: u64,
    clipboard_owner: u64,
    clipboard: Value,
    capture: Option<Capture>,
    hover: Option<(u64, String)>,
    event: u64,
    debug: bool,
    dirty: bool,
    state_dirty: bool,
    pending_pointer: Option<Value>,
    pub focused: bool,
    taskbar_follow: crate::taskbar_follow::TaskbarFollow,
    ui_offset: [f32; 3],
    ui_anchor_ready: bool,
    lifts: BTreeMap<(u64, String), f32>,
    expanded_orb: Option<String>,
    companion_hover: Option<String>,
    stats: Value,
}
fn number(v: &Value, k: &str) -> Result<u64> {
    v[k].as_str()
        .context(format!("{k} must be a decimal string"))?
        .parse()
        .map_err(Into::into)
}
fn key(v: &Value) -> Result<String> {
    Ok(format!(
        "{}:{}",
        v["window_server_generation"]
            .as_str()
            .context("window generation missing")?,
        v["surface_id"].as_str().context("surface missing")?
    ))
}
fn vector<const N: usize>(v: &Value) -> Result<[f32; N]> {
    let a = v.as_array().context("expected vector")?;
    ensure!(a.len() == N, "wrong vector length");
    let mut out = [0.; N];
    for i in 0..N {
        out[i] = a[i].as_f64().context("expected finite number")? as f32;
        ensure!(out[i].is_finite(), "non-finite vector");
    }
    Ok(out)
}
fn node_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn monotonic() -> u64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}
impl Desktop {
    pub fn new(path: &Path, debug: bool) -> Result<Self> {
        let dir = path.parent().context("component socket needs parent")?;
        if !dir.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let meta = fs::symlink_metadata(dir)?;
        ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
            "component directory must be owned by caller and mode0700"
        );
        let listener = SeqPacket::listen(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let inode = fs::metadata(path)?.ino();
        let session = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .to_string();
        println!(
            "XR_COMPONENT_SOCKET={} GENERATION={session}",
            path.display()
        );
        Ok(Self {
            listener,
            active_peer: None,
            outbox: RefCell::new(Vec::new()),
            pending_requests: BTreeMap::new(),
            path: path.into(),
            inode,
            session,
            next_peer: 1,
            peers: BTreeMap::new(),
            windows: BTreeMap::new(),
            revision: 0,
            focus_epoch: 0,
            focus: None,
            focus_applied: false,
            workspace: 0,
            system: json!({}),
            system_owner: 0,
            clipboard_owner: 0,
            clipboard: Value::Null,
            capture: None,
            hover: None,
            event: 0,
            debug,
            dirty: true,
            state_dirty: false,
            pending_pointer: None,
            focused: false,
            taskbar_follow: Default::default(),
            ui_offset: [0.; 3],
            ui_anchor_ready: true,
            lifts: BTreeMap::new(),
            expanded_orb: None,
            companion_hover: None,
            stats: json!({"connections":0,"disconnects":0,"scene_transactions":0,"shm_commits":0,"buffer_releases":0,"input_events":0,"cancellations":0,"protocol_errors":0,"focus_changes":0}),
        })
    }
    fn count(&mut self, k: &str) {
        self.stats[k] = json!(self.stats[k].as_u64().unwrap_or(0) + 1);
    }
    fn changed(&mut self) {
        self.state_dirty = true;
        self.dirty = true;
    }
    pub fn summary(&self) -> Value {
        json!({"stats":self.stats,"state_revision":self.revision.to_string(),"window_count":self.windows.len(),"component_count":self.peers.len(),"workspace":self.workspace,"focus_epoch":self.focus_epoch.to_string(),"focus_applied":self.focus_applied,"capture_active":self.capture.is_some()})
    }
    pub fn snapshot(&self) -> Value {
        let windows: Vec<_> = self
            .windows
            .iter()
            .map(|(k, w)| {
                let mut v = w.state.clone();
                for field in ["title", "app_id"] {
                    if let Some(text) = v[field].as_str() {
                        v[field] = json!(text.chars().take(256).collect::<String>());
                    }
                }
                v["minimized"] = json!(w.minimized);
                v["curved"] = json!(w.curved);
                v["focused"] = json!(self.focus.as_ref() == Some(k));
                v["position_m"] = json!(w.position);
                v["size_m"] = json!(w.size);
                v["last_frame_id"] = json!(w.frame.to_string());
                v
            })
            .collect();
        let mut components:Vec<_>=self.peers.values().filter(|p|!p.role.is_empty()).map(|p|json!({"component_id":p.id.to_string(),"component_generation":p.generation,"role":p.role,"scene_revision":p.revision.to_string(),"node_count":p.nodes.len(),"root":{"logical_size":p.root.logical,"size_m":p.root.size,"position_m":self.root_frame(p).position,"policy":p.root.policy}})).collect();
        if let Some((_, role, component, _)) = &self.active_peer {
            if !role.is_empty() {
                components.push(component.clone());
            }
        }
        json!({"state_revision":self.revision.to_string(),"windows":windows,"components":components,"active_workspace":self.workspace,"focus_epoch":self.focus_epoch.to_string(),"focus":self.focus,"focus_applied":self.focus_applied,"system":self.system,"clipboard":self.clipboard_summary(),"stats":self.stats,"taskbar_follow":self.taskbar_follow,"taskbar_capture_frozen":self.taskbar_captured()})
    }
    fn clipboard_summary(&self) -> Value {
        if self.clipboard.is_null() {
            return Value::Null;
        }
        let text = self.clipboard["text"].as_str().unwrap_or("");
        json!({"mime_type":self.clipboard["mime_type"],"preview":text.chars().take(128).collect::<String>(),"bytes":text.len()})
    }
    fn forward_request(
        &mut self,
        origin: u64,
        request: &str,
        kind: &str,
        mut payload: Value,
    ) -> Result<()> {
        ensure!(self.pending_requests.len() < 256, "pending request limit");
        payload["origin_component_id"] = json!(origin.to_string());
        payload["origin_request_id"] = json!(request);
        self.to_role("window_server", kind, payload)?;
        self.pending_requests.insert(
            (origin, request.into()),
            if kind == "SurfaceInput" {
                "InputAccepted".into()
            } else {
                "IntentAccepted".into()
            },
        );
        Ok(())
    }
    fn surface_input_for(&mut self, origin: u64, request: &str, mut payload: Value) -> Result<()> {
        ensure!(
            self.focus_applied,
            "stale_focus_epoch: focus not acknowledged"
        );
        let k = self.focus.as_ref().context("no focused surface")?;
        let w = &self.windows[k];
        payload["focus_epoch"] = json!(self.focus_epoch.to_string());
        payload["surface_id"] = w.state["surface_id"].clone();
        payload["window_server_generation"] = w.state["window_server_generation"].clone();
        self.forward_request(origin, request, "SurfaceInput", payload)
    }
    fn broadcast(&self, kind: &str, payload: Value) {
        if let Some((id, role, _, capabilities)) = &self.active_peer {
            if !role.is_empty() && accepts_event(capabilities, kind) {
                self.outbox
                    .borrow_mut()
                    .push((*id, kind.into(), payload.clone()));
            }
        }
        for p in self.peers.values().filter(|p| !p.role.is_empty()) {
            let _ = p.send_event(&self.session, kind, payload.clone());
        }
    }
    fn to_role(&self, role: &str, kind: &str, payload: Value) -> Result<()> {
        let mut found = false;
        let mut delivered = false;
        for p in self.peers.values().filter(|p| p.role == role) {
            found = true;
            delivered |= p.send_event(&self.session, kind, payload.clone())?;
        }
        if let Some((id, active_role, _, capabilities)) = &self.active_peer {
            if role == active_role {
                found = true;
                if accepts_event(capabilities, kind) {
                    self.outbox.borrow_mut().push((*id, kind.into(), payload));
                    delivered = true;
                }
            }
        }
        ensure!(found, "{role} service unavailable");
        ensure!(
            delivered,
            "missing_capability: {role} has no receiver for {}",
            message_capability(kind)
        );
        Ok(())
    }
    pub fn tick(&mut self) -> Result<()> {
        while let Some(socket) = self.listener.accept()? {
            let id = self.next_peer;
            self.next_peer += 1;
            self.peers.insert(
                id,
                Peer {
                    socket,
                    id,
                    generation: id.to_string(),
                    role: String::new(),
                    capabilities: Vec::new(),
                    revision: 0,
                    nodes: BTreeMap::new(),
                    root: Root::default(),
                    used_buffers: BTreeSet::new(),
                },
            );
            self.count("connections");
        }

        let ids: Vec<_> = self.peers.keys().copied().collect();
        for id in ids {
            let Some(mut peer) = self.peers.remove(&id) else {
                continue;
            };
            let mut live = true;
            for _ in 0..32 {
                match peer.socket.recv(0) {
                    Ok(Some(packet)) => {
                        let request = packet.message["request_id"]
                            .as_str()
                            .unwrap_or("0")
                            .to_owned();
                        let transaction = packet.message["type"] == "SceneTransaction";
                        self.active_peer = Some((
                            peer.id,
                            peer.role.clone(),
                            json!({"component_id":peer.id.to_string(),"component_generation":peer.generation,"role":peer.role,"scene_revision":peer.revision.to_string(),"node_count":peer.nodes.len(),"root":{"logical_size":peer.root.logical,"size_m":peer.root.size,"position_m":self.root_frame(&peer).position,"policy":peer.root.policy}}),
                            peer.capabilities.clone(),
                        ));
                        let handled = self.handle(&mut peer, packet);
                        self.active_peer = None;
                        let queued = std::mem::take(&mut *self.outbox.borrow_mut());
                        for (target, kind, payload) in queued {
                            if target == peer.id {
                                if peer.send_event(&self.session, &kind, payload).is_err() {
                                    live = false;
                                }
                            }
                        }
                        if let Err(error) = handled {
                            self.count("protocol_errors");
                            eprintln!("XR_PROTOCOL_ERROR peer={id} {error:#}");
                            let description = error.to_string();
                            let code = [
                                "unsupported_version",
                                "missing_capability",
                                "stale_generation",
                                "stale_revision",
                                "wrong_role",
                                "limit_exceeded",
                                "unknown_surface",
                                "stale_focus_epoch",
                                "invalid_buffer",
                            ]
                            .into_iter()
                            .find(|c| description.contains(c))
                            .unwrap_or("invalid_message");
                            let response = if transaction {
                                peer.send(&self.session,"SceneRejected",&request,json!({"code":code,"revision":peer.revision.to_string(),"message":description}))
                            } else {
                                peer.error(&self.session, &request, code, &description)
                            };
                            if response.is_err() {
                                live = false;
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        live = false;
                        break;
                    }
                }
            }
            if live {
                self.peers.insert(id, peer);
            } else {
                self.disconnect(&peer);
            }
        }
        if self.state_dirty {
            self.state_dirty = false;
            self.revision += 1;
            self.broadcast("StateChanged", self.snapshot());
        }
        Ok(())
    }
    fn disconnect(&mut self, p: &Peer) {
        self.count("disconnects");
        if self.system_owner == p.id {
            self.system = json!({});
            self.system_owner = 0;
        }
        if self.clipboard_owner == p.id {
            self.clipboard = Value::Null;
            self.clipboard_owner = 0;
        }
        if p.role == "window_server" {
            let pending = std::mem::take(&mut self.pending_requests);
            for ((origin, request), _) in pending {
                if let Some(client) = self.peers.get(&origin) {
                    let _ = client.error(
                        &self.session,
                        &request,
                        "service_unavailable",
                        "window service disconnected",
                    );
                }
            }
        }
        self.pending_requests
            .retain(|(origin, _), _| *origin != p.id);
        self.cancel_if(
            |t| matches!(t,Target::Node(id,_) if *id==p.id),
            "peer disconnected",
        );
        let removed: Vec<_> = self
            .windows
            .iter()
            .filter(|(_, w)| w.owner == p.id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in removed {
            self.remove_window(&k);
        }
        self.changed();
        println!("XR_COMPONENT_DISCONNECT id={} role={}", p.id, p.role);
    }
    fn handle(&mut self, p: &mut Peer, packet: Packet) -> Result<()> {
        let v = packet.message;
        let kind = v["type"].as_str().context("missing message type")?;
        let request = v["request_id"]
            .as_str()
            .context("request_id must be string")?;
        let payload = &v["payload"];
        if p.role.is_empty() {
            ensure!(
                kind == "Hello" && packet.fds.is_empty(),
                "first packet must be Hello without FDs"
            );
            if payload["protocol_major"] != 1 {
                p.error(
                    &self.session,
                    request,
                    "unsupported_version",
                    "major must be1",
                )?;
                bail!("unsupported_version");
            }
            let role = payload["role"].as_str().context("missing role")?;
            ensure!(
                ["dock", "system_ui", "window_server"].contains(&role),
                "wrong_role"
            );
            ensure!(
                role != "window_server" || !self.peers.values().any(|x| x.role == role),
                "only one window server allowed"
            );
            let supported: Vec<String> = CAPABILITIES
                .iter()
                .map(|s| s.to_string())
                .chain(self.debug.then_some("debug.v1".into()))
                .collect();
            let required = payload["required_capabilities"]
                .as_array()
                .context("missing required capabilities")?;
            ensure!(
                required
                    .iter()
                    .all(|c| c.as_str().is_some_and(|s| supported.iter().any(|x| x == s))),
                "missing_capability"
            );
            p.role = role.into();
            ensure!(
                required.iter().any(|c| c == "core.v1"),
                "missing_capability: core.v1 required"
            );
            let optional = payload["optional_capabilities"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            p.capabilities = supported
                .into_iter()
                .filter(|cap| {
                    required
                        .iter()
                        .chain(optional.iter())
                        .any(|v| v.as_str() == Some(cap.as_str()))
                })
                .collect();
            let mut root = Root::default();
            if role == "dock" {
                root.position = [0., 1.05, -1.5];
            } else if role == "system_ui" {
                root.position = [0.9, 1.6, -1.8];
            }
            p.root = root;
            p.send(&self.session,"Welcome",request,json!({"protocol_major":1,"protocol_minor":0,"capabilities":p.capabilities,"session_generation":self.session,"component_id":p.id.to_string(),"component_generation":p.generation,"root_id":"root","scene_revision":"0","limits":{"metadata_bytes":65536,"nodes_per_component":512,"transaction_operations":128,"shm_max_dimension_px":8192,"shm_max_bytes":134217728}}))?;
            println!("XR_COMPONENT_CONNECTED id={} role={}", p.id, p.role);
            self.changed();
            return Ok(());
        }
        ensure!(
            v["session_generation"] == self.session
                && v["component_id"] == p.id.to_string()
                && v["component_generation"] == p.generation,
            "stale_generation: connection identity changed"
        );
        ensure!(
            kind == "ShmBufferCommit" || packet.fds.is_empty(),
            "non-shm message carries FDs"
        );
        let capability = message_capability(kind);
        ensure!(
            p.capabilities.iter().any(|c| c == capability),
            "missing_capability: {capability}"
        );
        match kind {
            "GetSnapshot" => p.send(&self.session, "Snapshot", request, self.snapshot())?,
            "GetLayout" => {
                ensure!(self.debug, "missing_capability: debug disabled");
                p.send(
                    &self.session,
                    "LayoutPage",
                    request,
                    self.layout(
                        payload["offset"].as_u64().unwrap_or(0) as usize,
                        payload["limit"].as_u64().unwrap_or(32) as usize,
                    ),
                )?;
            }
            "GetScene" => {
                ensure!(self.debug, "missing_capability: debug input disabled");
                let id = number(payload, "component_id")?;
                let owner = self.peers.get(&id).context("component not found")?;
                let offset = payload["offset"].as_u64().unwrap_or(0) as usize;
                let limit = payload["limit"].as_u64().unwrap_or(16).min(16) as usize;
                let nodes: Vec<_> = owner.nodes.values().skip(offset).take(limit).collect();
                p.send(&self.session,"ScenePage",request,json!({"component_id":id.to_string(),"scene_revision":owner.revision.to_string(),"offset":offset,"total":owner.nodes.len(),"nodes":nodes}))?;
            }
            "RequestResult" => {
                ensure!(p.role == "window_server", "wrong_role");
                let origin = number(payload, "origin_component_id")?;
                let original = payload["origin_request_id"]
                    .as_str()
                    .context("origin request missing")?
                    .to_owned();
                if let Some(kind) = self.pending_requests.remove(&(origin, original.clone())) {
                    if let Some(client) = self.peers.get(&origin) {
                        if payload["success"] == true {
                            client.send(&self.session, &kind, &original, payload.clone())?;
                        } else {
                            client.error(
                                &self.session,
                                &original,
                                "invalid_request",
                                payload["message"]
                                    .as_str()
                                    .unwrap_or("window service rejected operation"),
                            )?;
                        }
                    }
                }
            }
            "Error" => {
                eprintln!("XR_COMPONENT_REPORTED_ERROR role={} {}", p.role, payload);
                self.count("protocol_errors");
            }
            "SceneTransaction" => {
                ensure!(
                    ["dock", "system_ui"].contains(&p.role.as_str()),
                    "wrong_role"
                );
                let base = number(payload, "base_revision")?;
                let rev = number(payload, "revision")?;
                if base != p.revision || rev != base + 1 {
                    p.send(
                        &self.session,
                        "SceneRejected",
                        request,
                        json!({"code":"stale_revision","revision":p.revision.to_string()}),
                    )?;
                    return Ok(());
                }
                let ops = payload["operations"]
                    .as_array()
                    .context("operations missing")?;
                ensure!(ops.len() <= 128, "limit_exceeded");
                let mut nodes = p.nodes.clone();
                let mut root = p.root.clone();
                for op in ops {
                    match op["op"].as_str().context("missing operation")? {
                        "upsert_node" => {
                            let node: Node = serde_json::from_value(op.clone())?;
                            ensure!(
                                node_id(&node.node_id)
                                    && node.node_id != "root"
                                    && node_id(&node.parent_id),
                                "invalid node id"
                            );
                            ensure!(
                                [
                                    "Group",
                                    "Card",
                                    "Text",
                                    "Icon",
                                    "Button",
                                    "Slider",
                                    "SurfaceReference"
                                ]
                                .contains(&node.kind.as_str()),
                                "unknown node kind"
                            );
                            ensure!(
                                node.rect.iter().all(|n| n.is_finite() && n.abs() <= 8192.)
                                    && node.rect[2] >= 0.
                                    && node.rect[3] >= 0.
                                    && node.text.len() <= 2048
                                    && node.value.is_finite()
                                    && node.min.is_finite()
                                    && node.max.is_finite(),
                                "invalid primitive fields"
                            );
                            ensure!(
                                Theme::default().styles.contains_key(&node.style_id)
                                    || node.style_id == "ukui.default"
                                    || (node.kind == "Group" && node.style_id.is_empty()),
                                "unregistered style id"
                            );
                            ensure!(
                                ["world_locked", "dock_lazy_follow"]
                                    .contains(&node.spatial_policy.as_str())
                                    && (node.spatial_policy == "world_locked"
                                        || (node.spatial_panel
                                            && p.capabilities
                                                .iter()
                                                .any(|c| c == "scene.spatial-policy.v1"))),
                                "invalid or unnegotiated spatial policy"
                            );
                            if node.spatial_panel {
                                ensure!(
                                    node.kind == "Group"
                                        && node
                                            .logical_size
                                            .iter()
                                            .all(|v| v.is_finite() && *v >= 1. && *v <= 2048.)
                                        && node
                                            .size_m
                                            .iter()
                                            .all(|v| v.is_finite() && *v > 0. && *v <= 4.)
                                        && node
                                            .position_m
                                            .iter()
                                            .all(|v| v.is_finite() && v.abs() <= 10.)
                                        && node
                                            .rotation_degrees
                                            .iter()
                                            .all(|v| v.is_finite() && v.abs() <= 360.),
                                    "invalid spatial group geometry"
                                );
                            }
                            if node.kind == "Slider" {
                                ensure!(node.max > node.min, "invalid slider range");
                            }
                            if node.kind == "SurfaceReference" {
                                ensure!(
                                    self.windows.contains_key(&format!(
                                        "{}:{}",
                                        node.window_server_generation, node.surface_id
                                    )),
                                    "unknown_surface"
                                );
                            }
                            nodes.insert(node.node_id.clone(), node);
                        }
                        "remove_node" => {
                            let id = op["node_id"].as_str().context("node id missing")?;
                            ensure!(id != "root", "cannot remove root");
                            nodes.remove(id);
                        }
                        "set_root_policy" => {
                            root.policy =
                                op["policy"].as_str().context("root policy missing")?.into();
                            ensure!(
                                ["world_locked", "dock_lazy_follow"]
                                    .contains(&root.policy.as_str()),
                                "unknown root policy"
                            );
                            root.logical = vector(&op["logical_size"])?;
                            root.size = vector(&op["size_m"])?;
                            root.position = vector(&op["position_m"])?;
                            if op.get("rotation_degrees").is_some() {
                                root.rotation = vector(&op["rotation_degrees"])?;
                            }
                            ensure!(
                                root.logical.iter().all(|v| *v >= 1. && *v <= 2048.)
                                    && root.size.iter().all(|v| *v > 0. && *v <= 4.)
                                    && root.position.iter().all(|v| v.abs() <= 10.),
                                "invalid root geometry"
                            );
                        }
                        _ => bail!("unknown scene operation"),
                    }
                }
                ensure!(nodes.len() <= 512, "limit_exceeded");
                for node in nodes.values() {
                    let mut parent = node.parent_id.as_str();
                    let mut seen = BTreeSet::new();
                    seen.insert(node.node_id.as_str());
                    while parent != "root" {
                        ensure!(seen.insert(parent), "scene graph cycle");
                        parent = &nodes.get(parent).context("parent missing")?.parent_id;
                    }
                }
                let cancelled = self.capture.as_ref().is_some_and(|c| {
                    let Target::Node(id, n) = &c.target else {
                        return false;
                    };
                    if *id != p.id {
                        return false;
                    }
                    let Some(node) = nodes.get(n) else {
                        return true;
                    };
                    let reference = (node.kind == "SurfaceReference")
                        .then(|| format!("{}:{}", node.window_server_generation, node.surface_id));
                    if reference != c.surface {
                        return true;
                    }
                    if reference.is_some() {
                        let mut current = node;
                        loop {
                            if !current.visible || !current.enabled {
                                return true;
                            }
                            if current.parent_id == "root" {
                                break;
                            }
                            current = &nodes[&current.parent_id];
                        }
                        if node.rect[2] <= 0. || node.rect[3] <= 0. {
                            return true;
                        }
                    }
                    false
                });
                if cancelled {
                    self.pending_pointer = None;
                    if let Some(c) = self.capture.take() {
                        self.release_capture_surface(&c);
                        p.send(&self.session,"Cancel","0",json!({"capture_id":c.id,"interaction_id":c.interaction,"reason":"target removed"}))?;
                        self.count("cancellations");
                    }
                }
                p.nodes = nodes;
                p.root = root;
                p.revision = rev;
                self.changed();
                self.count("scene_transactions");
                p.send(
                    &self.session,
                    "SceneApplied",
                    request,
                    json!({"revision":rev.to_string()}),
                )?;
            }
            "WindowState" => {
                ensure!(p.role == "window_server", "wrong_role");
                let id = payload["surface_id"]
                    .as_str()
                    .context("surface id missing")?;
                ensure!(node_id(id), "invalid surface id");
                let mut state = payload.clone();
                state["window_server_generation"] = json!(p.generation);
                state["workspace"] = json!(
                    payload["workspace"]
                        .as_u64()
                        .or_else(|| self
                            .windows
                            .get(&format!("{}:{id}", p.generation))
                            .and_then(|w| w.state["workspace"].as_u64()))
                        .unwrap_or(self.workspace as u64)
                        .min(3)
                );
                let k = format!("{}:{id}", p.generation);
                let was_mapped = self
                    .windows
                    .get(&k)
                    .is_some_and(|w| w.state["mapped"] == true);
                let count = self.windows.len();
                ensure!(
                    count < 32 || self.windows.contains_key(&k),
                    "window limit exceeded"
                );
                if let Some(w) = self.windows.get_mut(&k) {
                    w.state = state;
                } else {
                    self.windows.insert(
                        k.clone(),
                        Window {
                            owner: p.id,
                            state,
                            width: 0,
                            height: 0,
                            pixels: Vec::new(),
                            position: [
                                (count % 3) as f32 * 0.15,
                                1.6,
                                -1.8 - (count % 3) as f32 * 0.1,
                            ],
                            size: [1.59, 1.1],
                            frame: 0,
                            minimized: false,
                            curved: false,
                        },
                    );
                }
                if !was_mapped
                    && self.windows[&k].state["mapped"] == true
                    && self.windows[&k].state["role"]
                        .as_str()
                        .is_none_or(|r| r == "toplevel")
                    && self.windows[&k].state["workspace"] == self.workspace
                {
                    self.set_focus(Some(k.clone()))?;
                }
                self.changed();
                p.send(
                    &self.session,
                    "WindowAccepted",
                    request,
                    json!({"surface_id":id}),
                )?;
            }
            "WindowRemoved" => {
                ensure!(p.role == "window_server", "wrong_role");
                self.remove_window(&format!(
                    "{}:{}",
                    p.generation,
                    payload["surface_id"].as_str().context("surface missing")?
                ));
            }
            "ShmBufferCommit" => {
                ensure!(
                    p.role == "window_server" && packet.fds.len() == 1,
                    "wrong_role or invalid FD count"
                );
                let k = key(payload)?;
                ensure!(
                    payload["window_server_generation"] == p.generation,
                    "stale_generation: invalid buffer generation"
                );
                let w = self.windows.get_mut(&k).context("unknown_surface")?;
                ensure!(w.owner == p.id, "wrong_role");
                let buffer = payload["buffer_id"]
                    .as_str()
                    .context("buffer id missing")?
                    .to_owned();
                let frame = number(payload, "frame_id")?;
                ensure!(
                    !p.used_buffers.contains(&buffer),
                    "invalid_buffer: reused snapshot"
                );
                if frame <= w.frame {
                    p.used_buffers.insert(buffer.clone());
                    p.send(&self.session,"BufferRelease",request,json!({"window_server_generation":p.generation,"surface_id":payload["surface_id"],"buffer_id":buffer,"frame_id":frame.to_string(),"status":"rejected"}))?;
                    self.count("buffer_releases");
                    bail!("invalid_buffer: non-monotonic frame");
                }
                let result = (|| -> Result<(u32, u32, Vec<u8>)> {
                    let width = payload["width_px"].as_u64().context("width missing")?;
                    let height = payload["height_px"].as_u64().context("height missing")?;
                    let stride = payload["stride_bytes"].as_u64().context("stride missing")?;
                    let offset = payload["offset_bytes"].as_u64().context("offset missing")?;
                    let length = payload["length_bytes"].as_u64().context("length missing")?;
                    ensure!(
                        width > 0
                            && height > 0
                            && width <= 8192
                            && height <= 8192
                            && stride >= width * 4
                            && length <= 134217728
                            && payload["fd_index"] == 0,
                        "invalid_buffer dimensions"
                    );
                    ensure!(
                        payload["colour_space"] == "srgb"
                            && ((payload["format"] == "argb8888"
                                && payload["alpha_mode"] == "premultiplied")
                                || (payload["format"] == "xrgb8888"
                                    && payload["alpha_mode"] == "opaque")),
                        "invalid_buffer format"
                    );
                    let end = offset
                        .checked_add(stride.checked_mul(height - 1).context("stride overflow")?)
                        .and_then(|n| n.checked_add(width * 4))
                        .context("buffer overflow")?;
                    let fd = packet.fds[0].as_raw_fd();
                    let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
                    let required = libc::F_SEAL_WRITE
                        | libc::F_SEAL_GROW
                        | libc::F_SEAL_SHRINK
                        | libc::F_SEAL_SEAL;
                    ensure!(
                        seals >= 0 && seals & required == required,
                        "invalid_buffer seals"
                    );
                    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                    ensure!(
                        unsafe { libc::fstat(fd, &mut stat) } == 0
                            && end <= length
                            && length <= stat.st_size as u64,
                        "invalid_buffer extent"
                    );
                    let damage = payload["damage_rects"]
                        .as_array()
                        .context("missing damage rectangles")?;
                    ensure!(damage.len() <= 256, "damage limit");
                    for r in damage {
                        let a = vector::<4>(r)?;
                        ensure!(
                            a[0] >= 0.
                                && a[1] >= 0.
                                && a[2] >= 0.
                                && a[3] >= 0.
                                && a[0] + a[2] <= width as f32
                                && a[1] + a[3] <= height as f32,
                            "invalid damage rect"
                        );
                    }
                    let mut bytes = vec![0; width as usize * height as usize * 4];
                    for y in 0..height as usize {
                        let dst = &mut bytes[y * width as usize * 4..(y + 1) * width as usize * 4];
                        let n = unsafe {
                            libc::pread(
                                fd,
                                dst.as_mut_ptr() as *mut _,
                                dst.len(),
                                (offset + y as u64 * stride) as _,
                            )
                        };
                        ensure!(n == dst.len() as isize, "short snapshot read");
                    }
                    if payload["format"] == "xrgb8888" {
                        for px in bytes.chunks_exact_mut(4) {
                            px[3] = 255;
                        }
                    }
                    Ok((width as u32, height as u32, bytes))
                })();
                p.used_buffers.insert(buffer.clone());

                let release = if result.is_ok() {
                    "consumed"
                } else {
                    "rejected"
                };
                p.send(&self.session,"BufferRelease",request,json!({"window_server_generation":p.generation,"surface_id":payload["surface_id"],"buffer_id":buffer,"frame_id":frame.to_string(),"status":release}))?;
                self.count("buffer_releases");
                let (width, height, bytes) = result?;
                let w = self.windows.get_mut(&k).unwrap();
                w.width = width;
                w.height = height;
                w.pixels = bytes;
                w.frame = frame;
                let logical_width = w.state["width"].as_f64().unwrap_or(width as f64) as f32;
                let logical_height = w.state["height"].as_f64().unwrap_or(height as f64) as f32;
                if !self
                    .capture
                    .as_ref()
                    .is_some_and(|c| matches!(&c.target,Target::Resize(key,_) if key==&k))
                {
                    w.size[1] = w.size[0] * logical_height / logical_width.max(1.);
                }
                self.dirty = true;
                self.count("shm_commits");
            }
            "WindowIntent" => {
                ensure!(
                    ["dock", "system_ui"].contains(&p.role.as_str()),
                    "wrong_role"
                );
                if ["launch", "close", "resize"].contains(&payload["intent"].as_str().unwrap_or(""))
                {
                    if payload["intent"] != "launch" {
                        ensure!(self.windows.contains_key(&key(payload)?), "unknown_surface");
                    }
                    self.forward_request(p.id, request, "WindowIntent", payload.clone())?;
                } else {
                    self.window_intent(payload.clone())?;
                    p.send(
                        &self.session,
                        "IntentAccepted",
                        request,
                        json!({"intent":payload["intent"]}),
                    )?;
                }
            }
            "FocusApplied" => {
                ensure!(
                    p.role == "window_server"
                        && number(payload, "focus_epoch")? == self.focus_epoch,
                    "stale_focus_epoch: incorrect focus acknowledgement"
                );
                ensure!(
                    self.focus
                        .as_ref()
                        .is_none_or(|k| self.windows[k].owner == p.id),
                    "wrong focus owner"
                );
                self.focus_applied = payload["success"].as_bool().unwrap_or(false);
                if self.focus_applied {
                    if let Some(press) = self.pending_pointer.take() {
                        self.surface_input(
                            json!({"kind":"pointer_motion","x":press["x"],"y":press["y"]}),
                        )?;
                        self.surface_input(
                            json!({"kind":"pointer_button","button":272,"pressed":true}),
                        )?;
                    }
                }
                self.changed();
            }
            "ComponentIntent" => {
                ensure!(
                    ["dock", "system_ui"].contains(&p.role.as_str()),
                    "wrong_role"
                );
                let role = payload["role"].as_str().context("missing component role")?;
                ensure!(["dock", "system_ui"].contains(&role), "wrong_role");
                ensure!(
                    ["show", "hide", "toggle"].contains(&payload["intent"].as_str().unwrap_or("")),
                    "invalid component intent"
                );
                self.to_role(role, "ComponentCommand", payload.clone())?;
                p.send(&self.session, "IntentAccepted", request, payload.clone())?;
            }
            "SystemState" => {
                ensure!(p.role == "system_ui", "wrong_role");
                self.system = payload.clone();
                self.system_owner = p.id;
                self.changed();
            }
            "WorkspaceIntent" => {
                let workspace = payload["workspace"].as_u64().context("workspace missing")?;
                ensure!(workspace < 4, "invalid workspace");
                self.cancel("source switched");
                self.workspace = workspace as u32;
                self.set_focus(None)?;
                self.changed();
                p.send(
                    &self.session,
                    "WorkspaceApplied",
                    request,
                    json!({"workspace":workspace}),
                )?;
            }
            "GetClipboard" => {
                p.send(
                    &self.session,
                    "ClipboardChanged",
                    request,
                    self.clipboard.clone(),
                )?;
            }
            "ClipboardSet" => {
                ensure!(p.role == "system_ui", "wrong_role");
                ensure!(
                    payload["text"].as_str().is_some_and(|t| t.len() <= 60000),
                    "clipboard text limit"
                );
                self.to_role("window_server", "ClipboardSet", payload.clone())?;
                p.send(
                    &self.session,
                    "IntentAccepted",
                    request,
                    json!({"intent":"clipboard"}),
                )?;
            }
            "ClipboardChanged" => {
                ensure!(p.role == "window_server", "wrong_role");
                ensure!(
                    payload["text"].as_str().is_none_or(|s| s.len() <= 60000),
                    "clipboard text limit"
                );
                self.clipboard = payload.clone();
                self.clipboard_owner = p.id;
                self.changed();
                self.broadcast("ClipboardChanged", payload.clone());
            }
            "TextCommit" | "KeyIntent" => {
                ensure!(p.role == "system_ui", "wrong_role");
                let mut event = payload.clone();
                event["kind"] = json!(if kind == "TextCommit" { "text" } else { "key" });
                self.surface_input_for(p.id, request, event)?;
            }
            "DebugInput" => {
                ensure!(
                    self.debug && p.capabilities.iter().any(|c| c == "debug.v1"),
                    "missing_capability: debug input disabled"
                );
                self.debug_input(payload)?;
                p.send(&self.session, "InputAccepted", request, self.snapshot())?;
            }
            _ => bail!("unknown message: {kind}"),
        }
        Ok(())
    }
    fn remove_window(&mut self, k: &str) {
        if self
            .capture
            .as_ref()
            .is_some_and(|c| c.surface.as_deref() == Some(k))
        {
            self.cancel("target removed");
        }
        self.cancel_if(|t|matches!(t,Target::Window(key)|Target::Close(key)|Target::Move(key)|Target::Resize(key,_)|Target::Orb(key)|Target::Minimize(key)|Target::Curve(key) if key==k),"target removed");
        if self.focus.as_deref() == Some(k) {
            let _ = self.set_focus(None);
        }
        if self.windows.remove(k).is_some() {
            self.changed();
        }
    }
    fn set_focus(&mut self, k: Option<String>) -> Result<()> {
        if self.focus == k && self.focus_applied {
            return Ok(());
        }
        if let Some(key) = &k {
            if let Some(w) = self.windows.get_mut(key) {
                w.minimized = false;
            }
        }
        self.focus = k;
        self.focus_applied = false;
        self.pending_pointer = None;
        self.focus_epoch += 1;
        self.count("focus_changes");
        let payload = if let Some(k) = &self.focus {
            let w = &self.windows[k];
            json!({"focus_epoch":self.focus_epoch.to_string(),"surface_id":w.state["surface_id"],"window_server_generation":w.state["window_server_generation"]})
        } else {
            json!({"focus_epoch":self.focus_epoch.to_string(),"surface_id":null,"window_server_generation":null})
        };
        self.changed();
        if self.focus.is_none()
            && !self.peers.values().any(|p| p.role == "window_server")
            && self
                .active_peer
                .as_ref()
                .is_none_or(|(_, r, _, _)| r != "window_server")
        {
            self.focus_applied = true;
            Ok(())
        } else {
            self.to_role("window_server", "FocusChanged", payload)
        }
    }
    fn surface_input(&self, mut payload: Value) -> Result<()> {
        ensure!(
            self.focus_applied,
            "stale_focus_epoch: focus not acknowledged"
        );
        let k = self.focus.as_ref().context("no focused surface")?;
        let w = &self.windows[k];
        payload["focus_epoch"] = json!(self.focus_epoch.to_string());
        payload["surface_id"] = w.state["surface_id"].clone();
        payload["window_server_generation"] = w.state["window_server_generation"].clone();
        self.to_role("window_server", "SurfaceInput", payload)
    }
    fn window_intent(&mut self, payload: Value) -> Result<()> {
        let intent = payload["intent"].as_str().context("intent missing")?;
        if intent == "launch" {
            ensure!(
                payload["app_id"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() < 256),
                "invalid app id"
            );
            return self.to_role("window_server", "WindowIntent", payload);
        }
        let k = key(&payload)?;
        ensure!(self.windows.contains_key(&k), "unknown_surface");
        match intent {
            "focus" => self.set_focus(Some(k)),
            "move" => {
                let pos = vector::<3>(&payload["position_m"])?;
                ensure!(
                    pos.iter().all(|n| n.abs() <= 10.),
                    "invalid window position"
                );
                self.windows.get_mut(&k).unwrap().position = pos;
                self.changed();
                Ok(())
            }
            "minimize" => {
                self.windows.get_mut(&k).unwrap().minimized = true;
                self.set_focus(None)?;
                self.changed();
                Ok(())
            }
            "curve" => {
                let w = self.windows.get_mut(&k).unwrap();
                w.curved = !w.curved;
                self.changed();
                Ok(())
            }
            "resize" | "close" => self.to_role("window_server", "WindowIntent", payload),
            _ => bail!("unknown window intent"),
        }
    }
    pub fn has_pointer_capture(&self, source: &str) -> bool {
        self.capture.as_ref().is_some_and(|c| c.source == source)
    }
    pub fn cancel_pointer(&mut self, source: &str, reason: &str) {
        if self.has_pointer_capture(source) {
            self.cancel(reason);
        }
    }
    pub fn set_xr_focus(&mut self, focused: bool) {
        if self.focused && !focused {
            self.cancel("XR focus lost");
            let _ = self.set_focus(None);
        }
        self.focused = focused;
    }
    fn cancel_if(&mut self, predicate: impl Fn(&Target) -> bool, reason: &str) {
        if self.capture.as_ref().is_some_and(|c| predicate(&c.target)) {
            self.cancel(reason);
        }
    }
    fn cancel(&mut self, reason: &str) {
        self.pending_pointer = None;
        if let Some(c) = self.capture.take() {
            if let Target::Resize(key, _) = &c.target {
                if let Some(w) = self.windows.get_mut(key) {
                    w.size = c.size;
                    w.position = c.position;
                }
            }
            self.release_capture_surface(&c);
            if let Target::Node(id, node) = c.target {
                if let Some(p) = self.peers.get(&id) {
                    let _=p.send(&self.session,"Cancel","0",json!({"target_node_id":node,"capture_id":c.id,"interaction_id":c.interaction,"reason":reason}));
                }
            } else if c.surface.is_none() {
                let _ = self
                    .surface_input(json!({"kind":"pointer_button","button":272,"pressed":false}));
            }
            self.count("cancellations");
            self.dirty = true;
        }
    }
    fn release_capture_surface(&self, capture: &Capture) {
        if capture.surface.is_some() && capture.surface == self.focus && self.focus_applied {
            let _ =
                self.surface_input(json!({"kind":"pointer_button","button":272,"pressed":false}));
        }
    }
    fn node_rect(p: &Peer, node: &Node) -> [f32; 4] {
        let mut rect = node.rect;
        let mut parent = node.parent_id.as_str();
        for _ in 0..512 {
            if parent == "root" {
                break;
            }
            if let Some(n) = p.nodes.get(parent) {
                rect[0] += n.rect[0];
                rect[1] += n.rect[1];
                parent = &n.parent_id;
            } else {
                break;
            }
        }
        rect
    }
    fn hit_node(p: &Peer, x: f32, y: f32) -> Option<(String, [f32; 2])> {
        if x < 0. || y < 0. || x > p.root.logical[0] || y > p.root.logical[1] {
            return None;
        }
        let mut hit = None;
        for n in Self::ordered_nodes(p) {
            let r = Self::node_rect(p, n);
            if n.enabled
                && ["Button", "Slider", "SurfaceReference"].contains(&n.kind.as_str())
                && x >= r[0]
                && y >= r[1]
                && x <= r[0] + r[2]
                && y <= r[1] + r[3]
            {
                hit = Some((n.node_id.clone(), [x - r[0], y - r[1]]));
            }
        }
        hit
    }
    fn ordered_nodes(p: &Peer) -> Vec<&Node> {
        fn walk<'a>(p: &'a Peer, parent: &str, out: &mut Vec<&'a Node>) {
            for node in p.nodes.values().filter(|n| n.parent_id == parent) {
                out.push(node);
                walk(p, &node.node_id, out);
            }
        }
        let mut out = Vec::new();
        walk(p, "root", &mut out);
        out
    }
    fn emit_node(
        &mut self,
        id: u64,
        node: &str,
        phase: &str,
        local: [f32; 2],
        source: &str,
    ) -> Result<()> {
        let p = self.peers.get(&id).context("component gone")?;
        let n = p.nodes.get(node).context("target gone")?;
        self.event += 1;
        let (capture, interaction) = self
            .capture
            .as_ref()
            .map(|c| (c.id.clone(), c.interaction.clone()))
            .unwrap_or_else(|| ("0".into(), self.event.to_string()));
        let value = n.min + (n.max - n.min) * (local[0] / n.rect[2].max(1.)).clamp(0., 1.);
        let pointer = if source.ends_with(":left") {
            "2"
        } else if source.ends_with(":right") {
            "3"
        } else {
            "1"
        };
        let wire_source = source.split(':').next().unwrap_or(source);
        if p.send_event(&self.session,"InputEvent",json!({"event_id":self.event.to_string(),"pointer_id":pointer,"interaction_id":interaction,"capture_id":capture,"source":wire_source,"phase":phase,"target_node_id":node,"action_id":n.action_id,"local_x":local[0],"local_y":local[1],"value":value,"event_time_ns":monotonic().to_string(),"focus_epoch":self.focus_epoch.to_string()}))? {
            self.count("input_events");
        }
        Ok(())
    }
    fn debug_input(&mut self, payload: &Value) -> Result<()> {
        match payload["kind"].as_str().unwrap_or("pointer") {
            "cancel" => {
                self.cancel(payload["reason"].as_str().unwrap_or("source switched"));
                Ok(())
            }
            "key" | "text" | "scroll" | "pointer_motion" | "pointer_button" => {
                self.surface_input(payload.clone())
            }
            "ray" => {
                let origin = vector::<3>(&payload["origin"])?;
                let mut direction = vector::<3>(&payload["direction"])?;
                let length = direction.iter().map(|v| v * v).sum::<f32>().sqrt();
                ensure!(length > 0.0001, "invalid ray");
                for v in &mut direction {
                    *v /= length;
                }
                self.ray(
                    origin,
                    direction,
                    payload["phase"].as_str().context("phase missing")?,
                    "mouse",
                )
            }
            "focus_lost" => {
                self.set_xr_focus(false);
                Ok(())
            }
            "pointer" => {
                let id = number(payload, "component_id")?;
                let peer = self.peers.get(&id).context("component not found")?;
                let phase = payload["phase"].as_str().context("phase missing")?;
                let (x, y) = if let Some(node) = payload["node_id"].as_str() {
                    let n = peer.nodes.get(node).context("node missing")?;
                    let r = Self::node_rect(peer, n);
                    (
                        r[0] + payload["local_x"].as_f64().unwrap_or(r[2] as f64 / 2.) as f32,
                        r[1] + payload["local_y"].as_f64().unwrap_or(r[3] as f64 / 2.) as f32,
                    )
                } else {
                    (
                        payload["x"].as_f64().context("x missing")? as f32,
                        payload["y"].as_f64().context("y missing")? as f32,
                    )
                };
                ensure!(x.is_finite() && y.is_finite(), "invalid pointer coordinate");
                let hit = if let Some(c) = &self.capture {
                    if let Target::Node(owner, node) = &c.target {
                        if *owner == id {
                            let r = Self::node_rect(
                                peer,
                                peer.nodes.get(node).context("capture target removed")?,
                            );
                            Some((node.clone(), [x - r[0], y - r[1]]))
                        } else {
                            Self::hit_node(peer, x, y)
                        }
                    } else {
                        Self::hit_node(peer, x, y)
                    }
                } else {
                    Self::hit_node(peer, x, y)
                };
                self.pointer(hit.map(|(n, xy)| (Target::Node(id, n), xy)), phase, "mouse")
            }
            _ => bail!("unknown debug input kind"),
        }
    }
    fn pointer(
        &mut self,
        hit: Option<(Target, [f32; 2])>,
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
        if self.capture.as_ref().is_some_and(|c| c.source != source) {
            if phase == "hover" {
                return Ok(());
            }
            self.cancel("source switched");
        }
        if phase == "release" && self.capture.is_none() {
            return Ok(());
        }
        if phase == "release" && !self.focus_applied {
            self.pending_pointer = None;
        }
        if phase == "press" {
            ensure!(self.capture.is_none(), "pointer already captured");
            if let Some((target, xy)) = &hit {
                self.event += 1;
                let pos = match target {
                    Target::Move(k) | Target::Resize(k, _) => self.windows[k].position,
                    _ => [0.; 3],
                };
                let surface = match target {
                    Target::Window(k) => Some(k.clone()),
                    Target::Node(id, n) => self
                        .peers
                        .get(id)
                        .and_then(|p| p.nodes.get(n))
                        .filter(|n| n.kind == "SurfaceReference")
                        .map(|n| format!("{}:{}", n.window_server_generation, n.surface_id)),
                    _ => None,
                };
                self.capture = Some(Capture {
                    target: target.clone(),
                    resize_frame: match target {
                        Target::Resize(k, left) => self
                            .companion_frame(k, if *left { "resize-left" } else { "resize-right" }),
                        _ => None,
                    },
                    size: match target {
                        Target::Resize(k, _) => self.windows[k].size,
                        _ => [0.; 2],
                    },
                    logical: match target {
                        Target::Resize(k, _) => Self::logical(&self.windows[k]),
                        _ => [0.; 2],
                    },
                    surface,
                    id: self.event.to_string(),
                    interaction: self.event.to_string(),
                    source: source.into(),
                    start: *xy,
                    position: pos,
                });
            }
        }
        let target = if let Some(c) = &self.capture {
            let xy = hit.as_ref().map(|(_, xy)| *xy).unwrap_or([-1., -1.]);
            Some((c.target.clone(), xy))
        } else {
            hit
        };
        if let Some((target, xy)) = target {
            match target {
                Target::Node(id, n) => {
                    if phase == "hover" {
                        let hover = Some((id, n.clone()));
                        if self.hover != hover {
                            self.hover = hover;
                            self.dirty = true;
                        }
                    }
                    let node = self
                        .peers
                        .get(&id)
                        .and_then(|p| p.nodes.get(&n))
                        .context("target gone")?;
                    if node.kind == "SurfaceReference" {
                        let key = format!("{}:{}", node.window_server_generation, node.surface_id);
                        let w = self.windows.get(&key).context("referenced window gone")?;
                        let logical = Self::logical(w);
                        let local = [
                            xy[0] * logical[0] / node.rect[2],
                            xy[1] * logical[1] / node.rect[3],
                        ];
                        self.pointer_surface(&key, local, phase)?;
                    } else {
                        self.emit_node(id, &n, phase, xy, source)?;
                    }
                }
                Target::Window(k) => {
                    self.pointer_surface(&k, xy, phase)?;
                }
                Target::Orb(k) => {
                    if (phase == "hover" || phase == "press")
                        && self.expanded_orb.as_ref() != Some(&k)
                    {
                        self.expanded_orb = Some(k);
                        self.dirty = true;
                    }
                }
                Target::Minimize(k) => {
                    if phase == "release" {
                        self.windows
                            .get_mut(&k)
                            .context("window disappeared")?
                            .minimized = true;
                        self.set_focus(None)?;
                        self.changed();
                    }
                }
                Target::Curve(k) => {
                    if phase == "release" {
                        let w = self.windows.get_mut(&k).context("window disappeared")?;
                        w.curved = !w.curved;
                        self.changed();
                    }
                }
                Target::Close(k) => {
                    if phase == "release" {
                        let w = &self.windows[&k];
                        self.window_intent(json!({"intent":"close","surface_id":w.state["surface_id"],"window_server_generation":w.state["window_server_generation"]}))?;
                    }
                }
                Target::Move(k) => {
                    if phase == "drag" {
                        if let Some(c) = &self.capture {
                            let w = self.windows.get_mut(&k).unwrap();
                            w.position[0] = c.position[0]
                                + (xy[0] - c.start[0]) * w.size[0] / Self::logical(w)[0];
                            w.position[1] = c.position[1]
                                - (xy[1] - c.start[1]) * w.size[1] / Self::logical(w)[1].max(1.);
                            self.dirty = true;
                        }
                    }
                }
                Target::Resize(k, left) => {
                    if phase == "drag" || phase == "release" {
                        if let Some(c) = self.capture.clone() {
                            let width = (c.logical[0]
                                + (xy[0] - c.start[0]) * if left { -1. } else { 1. })
                            .clamp(160., 2048.)
                            .round();
                            let height = (c.logical[1] + xy[1] - c.start[1])
                                .clamp(120., 1536.)
                                .round();
                            let size = [
                                c.size[0] * width / c.logical[0],
                                c.size[1] * height / c.logical[1],
                            ];
                            let w = self.windows.get_mut(&k).context("window disappeared")?;
                            w.size = size;
                            let half_arc = 15f32.to_radians();
                            let arc_x = if w.curved {
                                half_arc.sin() / half_arc
                            } else {
                                1.
                            };
                            let arc_z = if w.curved {
                                (1. - half_arc.cos()) / (2. * half_arc)
                            } else {
                                0.
                            };
                            w.position = [
                                c.position[0]
                                    + (c.size[0] - size[0]) * if left { 0.5 } else { -0.5 } * arc_x,
                                c.position[1] + (c.size[1] - size[1]) * 0.5,
                                c.position[2] + (c.size[0] - size[0]) * arc_z,
                            ];
                            self.dirty = true;
                            if phase == "release" {
                                self.window_intent(json!({"intent":"resize","surface_id":self.windows[&k].state["surface_id"],"window_server_generation":self.windows[&k].state["window_server_generation"],"width":width as u32,"height":height as u32}))?;
                                self.changed();
                            }
                        }
                    }
                }
            }
        }
        if phase == "release" {
            self.capture = None;
        }
        if phase != "hover" {
            self.dirty = true;
        }
        Ok(())
    }
    fn pointer_surface(&mut self, key: &str, local: [f32; 2], phase: &str) -> Result<()> {
        if phase == "press" {
            self.set_focus(Some(key.to_owned()))?;
            if !self.focus_applied {
                self.pending_pointer = Some(json!({"x":local[0],"y":local[1]}));
            }
        }
        if self.focus.as_deref() == Some(key) && self.focus_applied {
            self.surface_input(json!({"kind":"pointer_motion","x":local[0],"y":local[1]}))?;
            if phase == "press" || phase == "release" {
                self.surface_input(
                    json!({"kind":"pointer_button","button":272,"pressed":phase=="press"}),
                )?;
            }
        }
        Ok(())
    }
    pub fn set_ui_anchor(&mut self, offset: Option<[f32; 3]>) {
        self.ui_anchor_ready = offset.is_some();
        self.ui_offset = offset.unwrap_or([0.; 3]);
    }

    /// Every opted-in root/panel uses one anchor; capturing any island freezes all.
    /// Actual application windows and unrelated System UI panels remain world locked.
    pub fn update_head(&mut self, position: [f32; 3], q: [f32; 4], dt: f32) -> bool {
        if !self.ui_anchor_ready || self.taskbar_captured() {
            return false;
        }
        if !self.peers.values().any(|p| {
            p.root.policy == "dock_lazy_follow"
                || p.nodes
                    .values()
                    .any(|n| n.spatial_panel && n.spatial_policy == "dock_lazy_follow")
        }) {
            return false;
        }
        self.taskbar_follow
            .update(crate::spatial::sub(position, self.ui_offset), q, dt)
    }
    fn taskbar_captured(&self) -> bool {
        self.capture.as_ref().is_some_and(|c| match &c.target {
            Target::Node(id, node) => self.peers.get(id).is_some_and(|p| {
                p.nodes
                    .get(node)
                    .is_some_and(|n| Self::node_policy(p, n) == "dock_lazy_follow")
            }),
            _ => false,
        })
    }
    fn logical(w: &Window) -> [f32; 2] {
        [
            w.state["width"].as_f64().unwrap_or(w.width as f64).max(1.) as f32,
            w.state["height"]
                .as_f64()
                .unwrap_or(w.height as f64)
                .max(1.) as f32,
        ]
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.cancel("peer disconnected");
        self.broadcast("SessionEnding", json!({"reason":"shell exit"}));
        if fs::metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
        }
        println!("XR_DESKTOP_CLEANUP=1 {}", self.summary());
    }
}

#[path = "spatial_ui.rs"]
pub(crate) mod spatial_ui;

#[cfg(test)]
#[path = "taskbar_follow_test.rs"]
mod taskbar_follow_test;
