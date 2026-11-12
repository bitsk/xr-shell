// SPDX-License-Identifier: GPL-3.0-or-later
use openxr as xr;

pub struct Tracking {
    trackers: Option<[xr::HandTracker; 2]>,
    frames: u64,
    previous: [bool; 2],
}
impl Tracking {
    pub fn new(instance: &xr::Instance, session: &xr::Session<xr::Vulkan>) -> Self {
        let trackers = if instance.exts().ext_hand_tracking.is_some() {
            match (
                session.create_hand_tracker(xr::Hand::LEFT),
                session.create_hand_tracker(xr::Hand::RIGHT),
            ) {
                (Ok(l), Ok(r)) => Some([l, r]),
                (l, r) => {
                    eprintln!(
                        "XR_HAND_TRACKING_INIT left={:?} right={:?}",
                        l.err(),
                        r.err()
                    );
                    None
                }
            }
        } else {
            None
        };
        eprintln!(
            "XR_HAND_TRACKING extension=XR_EXT_hand_tracking trackers={} joints=26",
            trackers.is_some()
        );
        Self {
            trackers,
            frames: 0,
            previous: [false; 2],
        }
    }
    pub fn sample(
        &mut self,
        base: &xr::Space,
        time: xr::Time,
    ) -> [Option<xr::HandJointLocations>; 2] {
        self.frames += 1;
        let samples = std::array::from_fn(|hand| {
            self.trackers
                .as_ref()
                .and_then(|t| match base.locate_hand_joints(&t[hand], time) {
                    Ok(s) => s,
                    Err(e) => {
                        if self.frames % 240 == 1 {
                            eprintln!("XR_HAND_TRACKING_ERROR hand={hand} result={e:?}");
                        }
                        None
                    }
                })
        });
        let active = std::array::from_fn(|h| samples[h].is_some());
        if active != self.previous || self.frames % 240 == 1 {
            eprintln!(
                "XR_HAND_JOINTS left={} right={} count=26 time={}",
                active[0],
                active[1],
                time.as_nanos()
            );
            self.previous = active;
        }
        samples
    }
}

/// Capture-only diagnostic: recorded geometry cannot attach desktop actions or
/// submit projection layers. The normal live hand path never reads this file.
pub struct Reference([xr::HandJointLocations; 2]);
impl Reference {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let v: serde_json::Value = serde_json::from_reader(std::fs::File::open(path)?)?;
        anyhow::ensure!(
            v["source"] == "recorded-capture-only",
            "reference must be explicitly labelled recorded-capture-only"
        );
        let hands = v["hands"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing recorded hands"))?;
        anyhow::ensure!(hands.len() == 2, "need two hand slots");
        let mut out: [[xr::HandJointLocation; 26]; 2] = [[xr::HandJointLocation {
            location_flags: xr::SpaceLocationFlags::from_raw(0),
            pose: xr::Posef::IDENTITY,
            radius: 0.,
        }; 26]; 2];
        for h in 0..2 {
            let joints = hands[h]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid joint array"))?;
            anyhow::ensure!(joints.len() == 26, "need 26 joints");
            for j in 0..26 {
                let a: Vec<f32> = serde_json::from_value(joints[j].clone())?;
                anyhow::ensure!(
                    a.len() == 8 && a.iter().all(|v| v.is_finite()),
                    "invalid joint values"
                );
                anyhow::ensure!((0.002..0.04).contains(&a[7]), "invalid joint radius");
                let norm = a[3..7].iter().map(|v| v * v).sum::<f32>();
                anyhow::ensure!((norm - 1.).abs() < 0.01, "invalid orientation");
                out[h][j] = xr::HandJointLocation {
                    location_flags: xr::SpaceLocationFlags::POSITION_VALID
                        | xr::SpaceLocationFlags::ORIENTATION_VALID,
                    pose: xr::Posef {
                        position: xr::Vector3f {
                            x: a[0],
                            y: a[1],
                            z: a[2],
                        },
                        orientation: xr::Quaternionf {
                            x: a[3],
                            y: a[4],
                            z: a[5],
                            w: a[6],
                        },
                    },
                    radius: a[7],
                };
            }
        }
        Ok(Self(out))
    }
    pub fn at(&self, head: xr::Posef) -> [Option<xr::HandJointLocations>; 2] {
        let q = head.orientation;
        let rotate = |p: xr::Vector3f| {
            let u = [q.x, q.y, q.z];
            let cross = |a: [f32; 3], b: [f32; 3]| {
                [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ]
            };
            let a = cross(u, [p.x, p.y, p.z]);
            let b = cross(u, a);
            xr::Vector3f {
                x: p.x + 2. * (q.w * a[0] + b[0]),
                y: p.y + 2. * (q.w * a[1] + b[1]),
                z: p.z + 2. * (q.w * a[2] + b[2]),
            }
        };
        std::array::from_fn(|h| {
            let mut js = self.0[h];
            for j in &mut js {
                let p = rotate(j.pose.position);
                j.pose.position = xr::Vector3f {
                    x: head.position.x + p.x,
                    y: head.position.y + p.y,
                    z: head.position.z + p.z,
                };
                let b = j.pose.orientation;
                j.pose.orientation = xr::Quaternionf {
                    x: q.w * b.x + q.x * b.w + q.y * b.z - q.z * b.y,
                    y: q.w * b.y - q.x * b.z + q.y * b.w + q.z * b.x,
                    z: q.w * b.z + q.x * b.y - q.y * b.x + q.z * b.w,
                    w: q.w * b.w - q.x * b.x - q.y * b.y - q.z * b.z,
                };
            }
            Some(js)
        })
    }
}
