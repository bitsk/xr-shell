// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded application observations. GPU completion wait is not GPU execution
//! time; xrEndFrame return is not scanout. No physical MTP is inferred here.
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
pub fn now() -> i64 {
    clock(libc::CLOCK_MONOTONIC)
}
pub fn thread_cpu() -> i64 {
    clock(libc::CLOCK_THREAD_CPUTIME_ID)
}
fn clock(clock: libc::clockid_t) -> i64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(clock, &mut t);
    }
    t.tv_sec as i64 * 1_000_000_000 + t.tv_nsec as i64
}
#[derive(Clone, Serialize)]
pub struct DrawSpan {
    pub eye: usize,
    pub pass: &'static str,
    pub stage: &'static str,
    pub mono_ns: [i64; 2],
}
#[derive(Serialize)]
pub struct Frame {
    pub number: u64,
    pub wait_number: u64,
    pub thread_cpu_ns: [i64; 2],
    pub cpu_core: [i32; 2],
    pub action_sync_span: Option<[i64; 2]>,
    pub controller_read_spans: [Option<[i64; 2]>; 2],
    pub controller_flags: [[u64; 2]; 2],
    pub hand_joint_count: [usize; 2],
    pub predicted_display_period_ns: i64,
    pub pre_input_mono_ns: [i64; 4],
    pub render_mono_ns: [i64; 11],
    pub gpu: crate::gpu_timing::Sample,
    pub draw_spans: Vec<DrawSpan>,
    pub model_vertices: [[u32; 2]; 2],
    pub panel_instances: u32,
    pub mono_ns: [i64; 9],
    pub predicted_display_xr_ns: i64,
    pub runtime_now_xr_ns: Option<i64>,
    pub runtime_now_mono_bracket_ns: [i64; 2],
    pub layers: u32,
    pub capture: bool,
    pub input_mono_ns: [i64; 5],
    pub head_pose: [f32; 7],
    pub head_flags: u64,
    pub controller_grip: [Option<[f32; 7]>; 2],
    pub controller_aim: [Option<[f32; 7]>; 2],
    pub controller_draw_mask: u8,
    pub spatial_fast_path: bool,
    // regions, source bytes, full replacement; sampled before GPU submission.
    pub atlas_update: (usize, u64, bool),
}
#[derive(Serialize)]
pub struct Timeline {
    pub boot_id: Option<String>,
    pub observed: u64,
    pub frames: VecDeque<Frame>,
    pub skipped: VecDeque<Value>,
}
impl Default for Timeline {
    fn default() -> Self {
        Self {
            boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .ok()
                .map(|v| v.trim().to_owned()),
            observed: 0,
            frames: VecDeque::with_capacity(4096),
            skipped: VecDeque::new(),
        }
    }
}
impl Timeline {
    pub fn skip(
        &mut self,
        wait_number: u64,
        mono_ns: [i64; 3],
        reason: &str,
        head_flags: Option<u64>,
    ) {
        if self.skipped.len() == 4096 {
            self.skipped.pop_front();
        }
        self.skipped.push_back(json!({"wait_number":wait_number,"mono_ns":mono_ns,"reason":reason,"head_flags":head_flags}));
    }
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
            "schema":2,"boot_id":self.boot_id,"shell_version":env!("CARGO_PKG_VERSION"),"skipped":self.skipped,"retained_limit":4096,"process_id":std::process::id(),
            "pre_input_stage_boundaries":["head_controls","locate_hand_joints","hand_mesh_update"],
            "render_stage_boundaries":["reuse_fence_and_begin","passthrough_prepare_pose","record_atlas_upload","record_left_eye","record_right_eye","record_extra_layers","end_command_buffer","queue_submit","completion_fence_wait","query_readback_and_retire"],
            "gpu_stage_boundaries":["uploads","left_eye","right_eye","extra_layers"],
            "gpu_scope":"optional device timestamp intervals; pipeline overlap and query overhead possible; not CPU-clock aligned",
            "input_stage_boundaries":["actions_and_routing","animate_and_raster","geometry_upload","ray_feedback"],
            "stage_boundaries":labels,"stages":stages,
            "scope":"application CPU wall time, not compositor timing, GPU execution time, scanout or physical MTP"})
    }
}

pub fn pose(p: openxr::Posef) -> [f32; 7] {
    [
        p.position.x,
        p.position.y,
        p.position.z,
        p.orientation.x,
        p.orientation.y,
        p.orientation.z,
        p.orientation.w,
    ]
}
