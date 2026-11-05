// SPDX-License-Identifier: GPL-3.0-or-later
use openxr as xr;

/// A Shell-local horizontal origin, always sampled in the original reference
/// space. Repeated recenters never compound offsets or reset the tracker.
pub fn pose(flags: xr::ViewStateFlags, views: &[xr::View]) -> Option<xr::Posef> {
    let centre = center_eye(flags, views)?;
    let q = centre.orientation;
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
            x: centre.position.x,
            y: 0.,
            z: centre.position.z,
        },
    })
}

/// Shortest-arc quaternion midpoint treats both views symmetrically, including
/// canted eye rotations and the equivalent q/-q representation.
pub fn center_eye(flags: xr::ViewStateFlags, views: &[xr::View]) -> Option<xr::Posef> {
    if views.len() != 2
        || !flags
            .contains(xr::ViewStateFlags::ORIENTATION_VALID | xr::ViewStateFlags::POSITION_VALID)
    {
        return None;
    }
    let mut qs = [[0.; 4]; 2];
    for (i, view) in views.iter().enumerate() {
        let p = view.pose.position;
        let q = view.pose.orientation;
        let v = [q.x, q.y, q.z, q.w];
        let norm: f32 = v.iter().map(|n| n * n).sum();
        if ![p.x, p.y, p.z]
            .iter()
            .chain(v.iter())
            .all(|n| n.is_finite())
            || (norm - 1.).abs() > 0.01
        {
            return None;
        }
        qs[i] = v.map(|n| n / norm.sqrt());
    }
    let sign = if qs[0].iter().zip(qs[1]).map(|(a, b)| a * b).sum::<f32>() < 0. {
        -1.
    } else {
        1.
    };
    let mid: [f32; 4] = std::array::from_fn(|i| qs[0][i] + sign * qs[1][i]);
    let norm = mid.iter().map(|n| n * n).sum::<f32>().sqrt();
    let q = mid.map(|n| n / norm);
    Some(xr::Posef {
        orientation: xr::Quaternionf {
            x: q[0],
            y: q[1],
            z: q[2],
            w: q[3],
        },
        position: xr::Vector3f {
            x: (views[0].pose.position.x + views[1].pose.position.x) * 0.5,
            y: (views[0].pose.position.y + views[1].pose.position.y) * 0.5,
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
        let mut canted = views;
        for (i, angle) in [yaw + 0.2, yaw - 0.2].into_iter().enumerate() {
            let sign = if i == 0 { 1. } else { -1. };
            canted[i].pose.orientation = xr::Quaternionf {
                x: 0.,
                y: sign * (angle * 0.5).sin(),
                z: 0.,
                w: sign * (angle * 0.5).cos(),
            };
        }
        let midpoint = center_eye(flags, &canted).unwrap();
        assert!((midpoint.orientation.y - q.y).abs() < 0.00001);
        assert!((midpoint.orientation.w - q.w).abs() < 0.00001);
    }
}
