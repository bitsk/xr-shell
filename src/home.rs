// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only A081 controller tap: the runtime reserves the Home/system button.
//! Wire v2 is owned by qiyu-controller-tracking (a081_controller_pose_wire.h).
use std::{
    env, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::{Duration, Instant},
};

const MAX_AGE_NS: u64 = 100_000_000;
const HOLD_NS: u64 = 1_000_000_000;

#[derive(Default)]
struct Hold {
    session: u64,
    sequence: u64,
    last_send: u64,
    since: Option<u64>,
    released: bool,
    fired: bool,
    down: bool,
}
impl Hold {
    fn cancel(&mut self) {
        self.since = None;
        self.released = false;
        self.fired = false;
    }
    fn sample(&mut self, session: u64, sequence: u64, send: u64, down: bool, focused: bool) {
        if session != self.session || send.saturating_sub(self.last_send) >= MAX_AGE_NS {
            self.cancel();
        } else if sequence <= self.sequence || send < self.last_send {
            return;
        }
        self.session = session;
        self.sequence = sequence;
        self.last_send = send;
        self.down = down;
        if !focused {
            self.cancel();
        } else if !down {
            self.since = None;
            self.released = true;
            self.fired = false;
        } else if self.released && self.since.is_none() {
            self.since = Some(send);
        }
    }
    fn poll(&mut self, now: u64, focused: bool) -> bool {
        if !focused || now < self.last_send || now - self.last_send >= MAX_AGE_NS {
            self.cancel();
            return false;
        }
        // Count only the interval covered by received, fresh held samples.
        if !self.fired
            && self
                .since
                .is_some_and(|start| self.last_send - start >= HOLD_NS)
        {
            self.fired = true;
            return true;
        }
        false
    }
}

pub struct Home {
    socket: Option<OwnedFd>,
    retry: Instant,
    hands: [Hold; 2],
}
impl Home {
    pub fn new() -> Self {
        Self {
            socket: None,
            retry: Instant::now(),
            hands: std::array::from_fn(|_| Hold::default()),
        }
    }
    pub fn cancel(&mut self) {
        for hand in &mut self.hands {
            hand.cancel();
        }
    }
    fn disconnect(&mut self) {
        self.socket = None;
        for hand in &mut self.hands {
            hand.cancel();
        }
        self.retry = Instant::now() + Duration::from_secs(1);
    }
    fn connect(&mut self) {
        if self.socket.is_some() || Instant::now() < self.retry {
            return;
        }
        self.retry = Instant::now() + Duration::from_secs(1);
        let Some(path) = env::var_os("A081_CONTROLLER_POSE_SOCKET") else {
            return;
        };
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_bytes();
        let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
        if bytes.is_empty() || bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
            return;
        }
        addr.sun_family = libc::AF_UNIX as _;
        for (out, &byte) in addr.sun_path.iter_mut().zip(bytes) {
            *out = byte as _;
        }
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                &addr as *const _ as _,
                mem::size_of_val(&addr) as _,
            )
        } == 0
        {
            self.socket = Some(fd);
            eprintln!("XR_HOME_TAP=connected HOLD_MS=1000");
        }
    }
    pub fn poll(&mut self, focused: bool) -> bool {
        self.connect();
        let mut clock: libc::timespec = unsafe { mem::zeroed() };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) } != 0 {
            self.disconnect();
            return false;
        }
        let now = clock.tv_sec as u64 * 1_000_000_000 + clock.tv_nsec as u64;
        let Some(socket) = &self.socket else {
            return false;
        };
        let mut exhausted = true;
        for _ in 0..256 {
            let mut packet = [0u8; 96];
            let n = unsafe {
                libc::recv(
                    socket.as_raw_fd(),
                    packet.as_mut_ptr() as _,
                    packet.len(),
                    libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                )
            };
            if n < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    exhausted = false;
                    break;
                }
                self.disconnect();
                return false;
            }
            if n == 0 {
                self.disconnect();
                return false;
            }
            if n != 96 {
                continue;
            }
            let u32_at = |i| u32::from_le_bytes(packet[i..i + 4].try_into().unwrap());
            let u64_at = |i| u64::from_le_bytes(packet[i..i + 8].try_into().unwrap());
            let flags = u32_at(12);
            let send = u64_at(40);
            let hand = u32_at(48) as usize;
            if u32_at(0) != 0x43503841
                || u32_at(4) != 2
                || u32_at(8) != 96
                || flags & !127 != 0
                || hand > 1
            {
                continue;
            }
            if send > now || now - send >= MAX_AGE_NS {
                continue;
            }
            let controls = u32_at(60);
            if flags & 65 != 65 || (controls >> 24) & 15 != 15 {
                self.hands[hand].cancel();
                continue;
            }
            let down = controls & 0x40 != 0;
            if down != self.hands[hand].down {
                eprintln!("XR_HOME_BUTTON HAND={hand} DOWN={down}");
            }
            self.hands[hand].sample(u64_at(16), u64_at(24), send, down, focused);
        }
        if exhausted {
            for hand in &mut self.hands {
                hand.cancel();
            }
            return false;
        }
        let mut requested = false;
        for (hand, hold) in self.hands.iter_mut().enumerate() {
            if hold.poll(now, focused) {
                eprintln!("XR_HOME_LONG_PRESS HAND={hand} HOLD_MS=1000");
                requested = true;
            }
        }
        requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn feed(h: &mut Hold, seq: u64, down: bool, focused: bool) -> bool {
        let time = 1_000_000_000 + seq * 20_000_000;
        h.sample(7, seq, time, down, focused);
        h.poll(time, focused)
    }
    #[test]
    fn short_press_and_held_once_until_release() {
        let mut h = Hold::default();
        assert!(!feed(&mut h, 1, false, true));
        for i in 2..20 {
            assert!(!feed(&mut h, i, true, true));
        }
        assert!(!feed(&mut h, 20, false, true));
        for i in 21..71 {
            assert!(!feed(&mut h, i, true, true));
        }
        assert!(feed(&mut h, 71, true, true));
        for i in 72..180 {
            assert!(!feed(&mut h, i, true, true));
        }
        assert!(!feed(&mut h, 180, false, true));
        for i in 181..231 {
            assert!(!feed(&mut h, i, true, true));
        }
        assert!(feed(&mut h, 231, true, true));
    }
    #[test]
    fn focus_stale_session_and_startup_require_release() {
        let mut h = Hold::default();
        for i in 1..80 {
            assert!(!feed(&mut h, i, true, true));
        }
        feed(&mut h, 80, false, true);
        for i in 81..110 {
            assert!(!feed(&mut h, i, true, true));
        }
        assert!(!feed(&mut h, 110, true, false));
        for i in 111..180 {
            assert!(!feed(&mut h, i, true, true));
        }
        feed(&mut h, 180, false, true);
        for i in 181..210 {
            assert!(!feed(&mut h, i, true, true));
        }
        assert!(!h.poll(1_000_000_000 + 220 * 20_000_000, true));
        for i in 220..280 {
            assert!(!feed(&mut h, i, true, true));
        }
        feed(&mut h, 280, false, true);
        h.sample(8, 1, 7_000_000_000, true, true);
        assert!(!h.poll(7_000_000_000, true));
        assert!(h.since.is_none());
    }
}
