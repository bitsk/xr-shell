// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded application observations. GPU completion wait is not GPU execution
//! time; xrEndFrame return is not scanout. No physical MTP is inferred here.
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
pub fn now() -> i64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t);
    }
    t.tv_sec as i64 * 1_000_000_000 + t.tv_nsec as i64
}
#[derive(Serialize)]
pub struct Frame {
    pub number: u64,
    pub mono_ns: [i64; 9],
    pub predicted_display_xr_ns: i64,
    pub runtime_now_xr_ns: Option<i64>,
    pub runtime_now_mono_bracket_ns: [i64; 2],
    pub layers: u32,
    pub capture: bool,
}
#[derive(Default, Serialize)]
pub struct Timeline {
    pub observed: u64,
    pub frames: VecDeque<Frame>,
}
impl Timeline {
    pub fn push(&mut self, f: Frame) {
        self.observed += 1;
        if self.frames.len() == 4096 {
            self.frames.pop_front();
        }
        self.frames.push_back(f);
    }
    pub fn summary(&self) -> Value {
        let labels = [
            "desktop_before_wait",
            "xrWaitFrame_and_begin",
            "locate_views",
            "input_and_atlas_prepare",
            "swapchain_acquire_wait",
            "record_submit_completion_wait",
            "swapchain_release",
            "xrEndFrame",
        ];
        let mut stages = serde_json::Map::new();
        // Capture frames include readback, and allocation frames remain visible.
        for (i, label) in labels.iter().enumerate() {
            let mut v: Vec<f64> = self
                .frames
                .iter()
                .filter(|f| !f.capture)
                .map(|f| (f.mono_ns[i + 1] - f.mono_ns[i]) as f64 / 1e6)
                .collect();
            v.sort_by(f64::total_cmp);
            if !v.is_empty() {
                let n = v.len() - 1;
                stages.insert(label.to_string(),json!({"samples":v.len(),"p50_ms":v[n/2],"p95_ms":v[n*95/100],"p99_ms":v[n*99/100],"max_ms":v[n]}));
            }
        }
        json!({"observed":self.observed,"retained":self.frames.len(),"clock":"CLOCK_MONOTONIC",
            "xr_clock":"runtime XrTime; paired now only with XR_KHR_convert_timespec_time",
            "stage_boundaries":labels,"stages":stages,
            "scope":"application CPU wall time, not compositor timing, GPU execution time, scanout or physical MTP"})
    }
}
