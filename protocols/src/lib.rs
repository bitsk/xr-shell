// SPDX-License-Identifier: GPL-3.0-or-later
//! Local, same-user, bounded JSON datagrams with SCM_RIGHTS snapshot transport.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::CString,
    io, mem,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

/// Shell-owned actions on buttons inside a horizon-spawned spatial panel.
/// These gestures change the whole panel and never emit client InputEvents.
pub const PANEL_MOVE_ACTION: &str = "shell:panel-move";
pub const PANEL_SCALE_ACTION: &str = "shell:panel-scale";

pub const MAX_METADATA: usize = 65536;
pub const VERSION: &str = "xr-desktop/1.0";
pub const CAPABILITIES: &[&str] = &[
    "core.v1",
    "scene.v1",
    "scene.spatial-policy.v1",
    "scene.horizon-spawn.v1",
    "scene.spherical-placement.v1",
    "taskbar.v1",
    "snapshot.v1",
    "input.v1",
    "shm.v1",
    "focus.v1",
    "window.v1",
    "workspace.v1",
    "system.v1",
    "clipboard.v1",
];

pub struct Packet {
    pub message: Value,
    pub fds: Vec<OwnedFd>,
}
pub struct SeqPacket {
    fd: OwnedFd,
}
impl SeqPacket {
    fn address(path: &Path) -> Result<(libc::sockaddr_un, libc::socklen_t)> {
        let name = path.as_os_str().as_bytes();
        let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
        ensure!(
            !name.is_empty() && name.len() < addr.sun_path.len() && !name.contains(&0),
            "invalid socket path"
        );
        addr.sun_family = libc::AF_UNIX as _;
        for (out, b) in addr.sun_path.iter_mut().zip(name) {
            *out = *b as _;
        }
        Ok((
            addr,
            (mem::size_of::<libc::sa_family_t>() + name.len() + 1) as _,
        ))
    }
    fn new() -> Result<Self> {
        let fd =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }
    pub fn connect(path: &Path) -> Result<Self> {
        let socket = Self::new()?;
        let (addr, len) = Self::address(path)?;
        if unsafe { libc::connect(socket.fd.as_raw_fd(), &addr as *const _ as *const _, len) } < 0 {
            return Err(io::Error::last_os_error()).context("connect component socket");
        }
        ensure!(
            socket.peer_uid()? == unsafe { libc::geteuid() },
            "component peer has different uid"
        );
        Ok(socket)
    }
    pub fn listen(path: &Path) -> Result<Self> {
        let socket = Self::new()?;
        let (addr, len) = Self::address(path)?;
        if unsafe { libc::bind(socket.fd.as_raw_fd(), &addr as *const _ as *const _, len) } < 0 {
            return Err(io::Error::last_os_error())
                .context("bind component socket (existing sockets are never removed)");
        }
        if unsafe { libc::listen(socket.fd.as_raw_fd(), 16) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(socket)
    }
    pub fn accept(&self) -> Result<Option<Self>> {
        if !self.ready(0)? {
            return Ok(None);
        }
        let fd = unsafe {
            libc::accept4(
                self.fd.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let socket = Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        };
        ensure!(
            socket.peer_uid()? == unsafe { libc::geteuid() },
            "component uid mismatch"
        );
        Ok(Some(socket))
    }
    pub fn peer_uid(&self) -> Result<u32> {
        let mut cred: libc::ucred = unsafe { mem::zeroed() };
        let mut len = mem::size_of_val(&cred) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut _,
                &mut len,
            )
        } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(cred.uid)
    }
    pub fn ready(&self, timeout_ms: i32) -> Result<bool> {
        let mut fd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(e.into());
        }
        Ok(n > 0)
    }
    pub fn send(&self, message: &Value, fds: &[BorrowedFd<'_>]) -> Result<()> {
        let data = serde_json::to_vec(message)?;
        ensure!(
            data.len() <= MAX_METADATA && fds.len() <= 1,
            "packet exceeds limits"
        );
        let mut iov = libc::iovec {
            iov_base: data.as_ptr() as *mut _,
            iov_len: data.len(),
        };
        let mut hdr: libc::msghdr = unsafe { mem::zeroed() };
        hdr.msg_iov = &mut iov;
        hdr.msg_iovlen = 1;
        let mut control = [0usize; 8];
        if !fds.is_empty() {
            unsafe {
                hdr.msg_control = control.as_mut_ptr() as *mut _;
                hdr.msg_controllen = libc::CMSG_SPACE(mem::size_of::<i32>() as _) as _;
                let c = libc::CMSG_FIRSTHDR(&hdr);
                (*c).cmsg_level = libc::SOL_SOCKET;
                (*c).cmsg_type = libc::SCM_RIGHTS;
                (*c).cmsg_len = libc::CMSG_LEN(mem::size_of::<i32>() as _) as _;
                std::ptr::write_unaligned(libc::CMSG_DATA(c) as *mut i32, fds[0].as_raw_fd());
            }
        }
        // Bound a stalled peer without making the render loop wait indefinitely.
        let timeout = libc::timeval {
            tv_sec: 0,
            tv_usec: 20000,
        };
        unsafe {
            libc::setsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDTIMEO,
                &timeout as *const _ as *const _,
                mem::size_of_val(&timeout) as _,
            );
        }
        let n = unsafe { libc::sendmsg(self.fd.as_raw_fd(), &hdr, libc::MSG_NOSIGNAL) };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        ensure!(n as usize == data.len(), "short datagram");
        Ok(())
    }
    pub fn recv(&self, timeout_ms: i32) -> Result<Option<Packet>> {
        if !self.ready(timeout_ms)? {
            return Ok(None);
        }
        let mut data = vec![0u8; MAX_METADATA];
        let mut control = [0usize; 16];
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr() as *mut _,
            iov_len: data.len(),
        };
        let mut hdr: libc::msghdr = unsafe { mem::zeroed() };
        hdr.msg_iov = &mut iov;
        hdr.msg_iovlen = 1;
        hdr.msg_control = control.as_mut_ptr() as *mut _;
        hdr.msg_controllen = mem::size_of_val(&control);
        let n = unsafe { libc::recvmsg(self.fd.as_raw_fd(), &mut hdr, libc::MSG_CMSG_CLOEXEC) };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if n == 0 {
            bail!("component disconnected");
        }
        let mut fds = Vec::new();
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&hdr);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let bytes = (*c).cmsg_len.saturating_sub(libc::CMSG_LEN(0) as usize);
                    for i in 0..bytes / mem::size_of::<i32>() {
                        let fd =
                            std::ptr::read_unaligned((libc::CMSG_DATA(c) as *const i32).add(i));
                        fds.push(OwnedFd::from_raw_fd(fd));
                    }
                }
                c = libc::CMSG_NXTHDR(&hdr, c);
            }
        }
        ensure!(
            hdr.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) == 0 && fds.len() <= 1,
            "truncated packet or excessive FDs"
        );
        Ok(Some(Packet {
            message: serde_json::from_slice(&data[..n as usize])?,
            fds,
        }))
    }
}
pub struct Client {
    socket: SeqPacket,
    pub welcome: Value,
    next_request: u64,
}
impl Client {
    pub fn connect(path: &Path, role: &str, required: &[&str]) -> Result<Self> {
        let socket = SeqPacket::connect(path)?;
        socket.send(&json!({"type":"Hello","request_id":"1","payload":{"protocol_major":1,"protocol_minor":0,"role":role,"required_capabilities":required,"optional_capabilities":CAPABILITIES}}),&[])?;
        let packet = socket
            .recv(5000)?
            .context("component handshake timed out")?;
        ensure!(
            packet.fds.is_empty() && packet.message["type"] == "Welcome",
            "handshake rejected: {}",
            packet.message
        );
        Ok(Self {
            socket,
            welcome: packet.message["payload"].clone(),
            next_request: 2,
        })
    }
    pub fn send(&mut self, kind: &str, payload: Value, fds: &[BorrowedFd<'_>]) -> Result<String> {
        let id = self.next_request.to_string();
        self.next_request += 1;
        self.socket.send(&json!({"type":kind,"request_id":id,"session_generation":self.welcome["session_generation"],"component_id":self.welcome["component_id"],"component_generation":self.welcome["component_generation"],"payload":payload}),fds)?;
        Ok(id)
    }
    pub fn recv(&self, timeout_ms: i32) -> Result<Option<Packet>> {
        self.socket.recv(timeout_ms)
    }
}

/// All rectangles are component-local logical pixels, +X right and +Y down.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub node_id: String,
    #[serde(default = "root_id")]
    pub parent_id: String,
    pub kind: String,
    #[serde(default)]
    pub spatial_panel: bool,
    /// Independent panels are world locked unless explicitly opted into the shared taskbar anchor.
    #[serde(default = "world_locked")]
    pub spatial_policy: String,
    #[serde(default)]
    pub logical_size: [f32; 2],
    #[serde(default)]
    pub position_m: [f32; 3],
    #[serde(default)]
    pub size_m: [f32; 2],
    #[serde(default)]
    pub rotation_degrees: [f32; 3],
    #[serde(default)]
    pub yaw_deg: f32,
    #[serde(default)]
    pub pitch_deg: f32,
    #[serde(default)]
    pub distance_m: Option<f32>,
    #[serde(default)]
    pub user_facing_billboard: bool,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default)]
    pub rect: [f32; 4],
    #[serde(default)]
    pub surface_id: String,
    #[serde(default)]
    pub window_server_generation: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub icon_id: String,
    #[serde(default)]
    pub action_id: String,
    #[serde(default = "default_style")]
    pub style_id: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub value: f32,
    #[serde(default)]
    pub min: f32,
    #[serde(default = "one")]
    pub max: f32,
}
fn world_locked() -> String {
    "world_locked".into()
}
fn root_id() -> String {
    "root".into()
}
fn default_style() -> String {
    "ukui.default".into()
}
fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}

pub fn sealed_snapshot(bytes: &[u8]) -> Result<OwnedFd> {
    let name = CString::new("xr-shm-snapshot")?;
    let fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut offset = 0;
    while offset < bytes.len() {
        let n = unsafe {
            libc::write(
                fd.as_raw_fd(),
                bytes[offset..].as_ptr() as *const _,
                bytes.len() - offset,
            )
        };
        if n <= 0 {
            return Err(io::Error::last_os_error().into());
        }
        offset += n as usize;
    }
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(fd)
}
