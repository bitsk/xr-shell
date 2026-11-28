// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Default)]
struct Shared {
    snapshot: Option<(Option<Value>, Option<Value>)>,
    report: Option<Value>,
}
pub struct Controls {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    root: Option<Value>,
    input: Option<Value>,
    managed: bool,
    generation: Option<u64>,
    changed: bool,
    last_write: Instant,
    home: [HomeHold; 2],
}
impl Controls {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let work = shared.clone();
        let done = stop.clone();
        let root_path = std::env::var_os("XR_SESSION_CONTROLS_FILE").map(PathBuf::from);
        let input_path = std::env::var_os("XR_INPUT_STATUS_FILE").map(PathBuf::from);
        let report_path = std::env::var_os("XR_SCENE_CONTROLS_STATUS_FILE").map(PathBuf::from);
        let managed = root_path.is_some() || input_path.is_some();
        let worker = (managed || report_path.is_some()).then(|| {
            thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let root = root_path.as_ref().and_then(|p| Self::read(p, true));
                    let input = input_path
                        .as_ref()
                        .and_then(|p| Self::read(p, false))
                        .filter(|v| {
                            v["observed_monotonic_ns"].as_u64().is_some_and(|n| {
                                let now = crate::timeline::now() as u64;
                                now >= n && now - n < 1_000_000_000
                            })
                        });
                    let report = if let Ok(mut state) = work.lock() {
                        state.snapshot = Some((root, input));
                        state.report.take()
                    } else {
                        None
                    };
                    if let (Some(p), Some(v)) = (report_path.as_ref(), report) {
                        let tmp = p.with_extension("tmp");
                        if let Ok(data) = serde_json::to_vec(&v) {
                            if fs::write(&tmp, data).is_ok() {
                                let _ = fs::rename(tmp, p);
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            })
        });
        Self {
            shared,
            stop,
            worker,
            managed,
            root: None,
            input: None,
            generation: None,
            changed: false,
            last_write: Instant::now(),
            home: [HomeHold::default(); 2],
        }
    }
    fn read(path: &PathBuf, root: bool) -> Option<Value> {
        let m = fs::symlink_metadata(path).ok()?;
        if !m.is_file() || m.len() > 16384 || (root && (m.uid() != 0 || m.mode() & 0o022 != 0)) {
            return None;
        }
        let v: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        (v["schema"] == 1).then_some(v)
    }
    pub fn poll(&mut self) {
        if let Ok(mut s) = self.shared.try_lock() {
            if let Some((root, input)) = s.snapshot.take() {
                self.root = root;
                self.input = input;
            }
        }
        let generation = self.input.as_ref().and_then(|v| v["generation"].as_u64());
        if generation != self.generation {
            self.changed = true;
            self.generation = generation;
        }
    }
    /// Runtime-owned system buttons are independent of optical ray validity.
    /// Use the producer observation epoch so a stale held value cannot fire.
    pub fn take_home_recenter(&mut self) -> bool {
        let now = crate::timeline::now() as u64;
        let mut fire = false;
        for hand in 0..2 {
            let sample = self.input.as_ref().and_then(|v| {
                let observed = v["observed_monotonic_ns"].as_u64()?;
                (self
                    .root
                    .as_ref()
                    .is_some_and(|r| r["session_active"] == true)
                    && now >= observed
                    && now - observed < 250_000_000
                    && v["controls_valid"][hand] == true)
                    .then_some((observed, v["system_buttons"][hand] == true))
            });
            fire |= self.home[hand].sample(sample);
        }
        fire
    }
    pub fn take_source_change(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
    pub fn source(&self) -> &str {
        if let Some(root) = self.root.as_ref() {
            if root["session_active"] != true {
                return "none";
            }
        }
        if let Some(v) = self.input.as_ref() {
            return v["active_source"].as_str().unwrap_or("none");
        }
        if self
            .root
            .as_ref()
            .is_some_and(|v| v["capabilities"]["hand_tracking"] == false)
        {
            "controllers"
        } else if self.managed {
            "none"
        } else {
            "runtime"
        }
    }
    pub fn passthrough(&self, default: bool) -> bool {
        self.root.as_ref().map_or(default, |v| {
            v["session_active"] == true && v["preferences"]["passthrough_enabled"] == true
        })
    }
    pub fn report(&mut self, passthrough: impl FnOnce() -> Option<Value>) {
        if self.last_write.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_write = Instant::now();
        let passthrough = passthrough();
        let v = json!({"schema":1,"observed_monotonic_ns":crate::timeline::now(),"input_source":self.source(),"passthrough_requested":passthrough.as_ref().and_then(|v|v["requested"].as_bool()).unwrap_or(false),"passthrough":passthrough});
        if let Ok(mut s) = self.shared.try_lock() {
            s.report = Some(v);
        }
    }
}
impl Drop for Controls {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[derive(Clone, Copy, Default)]
struct HomeHold {
    armed: bool,
    since: Option<u64>,
    fired: bool,
}
impl HomeHold {
    fn sample(&mut self, sample: Option<(u64, bool)>) -> bool {
        let Some((time, down)) = sample else {
            *self = Self::default();
            return false;
        };
        if !down {
            self.armed = true;
            self.since = None;
            self.fired = false;
            return false;
        }
        if !self.armed || self.fired {
            return false;
        }
        let start = *self.since.get_or_insert(time);
        if time < start {
            *self = Self::default();
            return false;
        }
        if time - start < 1_000_000_000 {
            return false;
        }
        self.fired = true;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn home_requires_fresh_release_and_fires_once_per_hold() {
        let mut h = HomeHold::default();
        assert!(!h.sample(Some((1, true))));
        assert!(!h.sample(Some((2_000_000_000, true))));
        assert!(!h.sample(Some((3_000_000_000, false))));
        assert!(!h.sample(Some((4_000_000_000, true))));
        assert!(!h.sample(Some((4_999_999_999, true))));
        assert!(h.sample(Some((5_000_000_000, true))));
        assert!(!h.sample(Some((8_000_000_000, true))));
        assert!(!h.sample(None));
        assert!(!h.sample(Some((10_000_000_000, true))));
        assert!(!h.sample(Some((11_000_000_000, false))));
        assert!(!h.sample(Some((12_000_000_000, true))));
        assert!(!h.sample(Some((12_000_000_000, true))));
        assert!(h.sample(Some((13_000_000_000, true))));
    }
}
