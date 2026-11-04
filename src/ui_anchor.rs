// SPDX-License-Identifier: GPL-3.0-or-later
//! A frozen UI-layout translation, independent of the runtime's tracked poses.
use anyhow::{Result, bail};
use openxr as xr;
use serde::Serialize;

#[derive(Clone, Copy, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Stage,
    Head,
}
impl Mode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "stage" => Ok(Self::Stage),
            "head" => Ok(Self::Head),
            _ => bail!("--ui-anchor requires stage or head"),
        }
    }
}

#[derive(Serialize)]
struct EyePose {
    position_m: [f32; 3],
    orientation_xyzw: [f32; 4],
}

#[derive(Serialize)]
pub struct UiAnchor {
    mode: Mode,
    ready: bool,
    offset_m: Option<[f32; 3]>,
    design_eye_height_m: f32,
    source_mean_eye_position_m: Option<[f32; 3]>,
    source_eye_poses: Option<[EyePose; 2]>,
    source_view_state_flags: Option<crate::ViewFlags>,
    source_predicted_display_time_ns: Option<i64>,
}
impl Default for UiAnchor {
    fn default() -> Self {
        Self::new(Mode::Stage)
    }
}
impl UiAnchor {
    pub fn new(mode: Mode) -> Self {
        let ready = matches!(mode, Mode::Stage);
        Self {
            mode,
            ready,
            offset_m: ready.then_some([0.; 3]),
            design_eye_height_m: 1.6,
            source_mean_eye_position_m: None,
            source_eye_poses: None,
            source_view_state_flags: None,
            source_predicted_display_time_ns: None,
        }
    }

    pub fn offset(&self) -> Option<[f32; 3]> {
        self.offset_m
    }

    /// Freeze once from finite, runtime-valid views. Never alter those views.
    pub fn observe(
        &mut self,
        flags: xr::ViewStateFlags,
        views: &[xr::View],
        time: xr::Time,
    ) -> bool {
        if self.ready
            || views.len() != 2
            || !flags.contains(
                xr::ViewStateFlags::ORIENTATION_VALID | xr::ViewStateFlags::POSITION_VALID,
            )
        {
            return false;
        }
        let eye = |view: &xr::View| {
            let p = view.pose.position;
            let q = view.pose.orientation;
            EyePose {
                position_m: [p.x, p.y, p.z],
                orientation_xyzw: [q.x, q.y, q.z, q.w],
            }
        };
        let poses = [eye(&views[0]), eye(&views[1])];
        if !poses.iter().all(|p| {
            p.position_m
                .iter()
                .chain(p.orientation_xyzw.iter())
                .all(|v| v.is_finite())
        }) {
            return false;
        }
        let mean = std::array::from_fn(|i| (poses[0].position_m[i] + poses[1].position_m[i]) / 2.);
        let offset = [mean[0], mean[1] - self.design_eye_height_m, mean[2]];
        if !mean.iter().chain(offset.iter()).all(|v| v.is_finite()) {
            return false;
        }
        self.ready = true;
        self.offset_m = Some(offset);
        self.source_mean_eye_position_m = Some(mean);
        self.source_eye_poses = Some(poses);
        self.source_view_state_flags = Some(flags.into());
        self.source_predicted_display_time_ns = Some(time.as_nanos());
        true
    }
}
