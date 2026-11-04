// SPDX-License-Identifier: GPL-3.0-or-later
use openxr as xr;

/// A Shell-local horizontal origin, always sampled in the original reference
/// space. Repeated recenters never compound offsets or reset the tracker.
pub fn pose(flags: xr::ViewStateFlags, views: &[xr::View]) -> Option<xr::Posef> {
    if views.len() != 2
        || !flags
            .contains(xr::ViewStateFlags::ORIENTATION_VALID | xr::ViewStateFlags::POSITION_VALID)
    {
        return None;
    }
    if !views.iter().all(|v| {
        let p = v.pose.position;
        let q = v.pose.orientation;
        let norm = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
        [p.x, p.y, p.z, q.x, q.y, q.z, q.w]
            .iter()
            .all(|v| v.is_finite())
            && (norm - 1.).abs() < 0.01
    }) {
        return None;
    }
    let q = views[0].pose.orientation;
    // Project the actual forward vector onto the floor; vertical gaze has no yaw.
    let x = 2. * (q.x * q.z + q.w * q.y);
    let z = 1. - 2. * (q.x * q.x + q.y * q.y);
    if x * x + z * z < 0.0001 {
        return None;
    }
    let yaw = x.atan2(z);
    let (sin, cos) = (yaw * 0.5).sin_cos();
    Some(xr::Posef {
        orientation: xr::Quaternionf {
            x: 0.,
            y: sin,
            z: 0.,
            w: cos,
        },
        position: xr::Vector3f {
            x: (views[0].pose.position.x + views[1].pose.position.x) * 0.5,
            y: 0.,
            z: (views[0].pose.position.z + views[1].pose.position.z) * 0.5,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn horizontal_origin_keeps_floor_and_rejects_invalid_views() {
        let yaw = std::f32::consts::FRAC_PI_2;
        let q = xr::Quaternionf {
            x: 0.,
            y: (yaw * 0.5).sin(),
            z: 0.,
            w: (yaw * 0.5).cos(),
        };
        let views = [-0.03, 0.03].map(|dx| xr::View {
            pose: xr::Posef {
                orientation: q,
                position: xr::Vector3f {
                    x: 2. + dx,
                    y: 1.7,
                    z: 3.,
                },
            },
            fov: xr::Fovf::default(),
        });
        let flags = xr::ViewStateFlags::ORIENTATION_VALID | xr::ViewStateFlags::POSITION_VALID;
        let p = pose(flags, &views).unwrap();
        assert!((p.position.x - 2.).abs() < 0.00001);
        assert_eq!(p.position.y, 0.);
        assert_eq!(p.position.z, 3.);
        assert!((p.orientation.y - q.y).abs() < 0.00001);
        assert!(pose(xr::ViewStateFlags::ORIENTATION_VALID, &views).is_none());
        let mut bad = views;
        bad[0].pose.orientation.w = f32::NAN;
        assert!(pose(flags, &bad).is_none());
        assert!(pose(flags, &views[..1]).is_none());
    }
}
