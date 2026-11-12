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
    root: Option<Value>,
    input: Option<Value>,
    report: Option<Value>,
}
pub struct Controls {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    root: Option<Value>,
    input: Option<Value>,
    generation: Option<u64>,
    changed: bool,
    last_write: Instant,
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
        let worker = thread::spawn(move || {
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
                    state.root = root;
                    state.input = input;
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
        });
        Self {
            shared,
            stop,
            worker: Some(worker),
            root: None,
            input: None,
            generation: None,
            changed: false,
            last_write: Instant::now(),
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
        if let Ok(s) = self.shared.try_lock() {
            self.root = s.root.clone();
            self.input = s.input.clone();
        }
        let generation = self.input.as_ref().and_then(|v| v["generation"].as_u64());
        if generation != self.generation {
            self.changed = true;
            self.generation = generation;
        }
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
        if self.root.as_ref().is_some_and(|v|v["capabilities"]["hand_tracking"]==false){
            "controllers"
        } else if self.root.is_some() {
            "none"
        } else if std::env::var("XR_SHELL_INPUT_MODE").as_deref() == Ok("hands") {
            "hands"
        } else {
            "controllers"
        }
    }
    pub fn passthrough(&self) -> bool {
        self.root.as_ref().map_or(true, |v| {
            v["session_active"] == true && v["preferences"]["passthrough_enabled"] == true
        })
    }
    pub fn report(&mut self, passthrough: Option<Value>) {
        if self.last_write.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_write = Instant::now();
        let v = json!({"schema":1,"observed_monotonic_ns":crate::timeline::now(),"input_source":self.source(),"passthrough_requested":self.passthrough(),"passthrough":passthrough});
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
