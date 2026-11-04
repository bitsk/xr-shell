// SPDX-License-Identifier: GPL-3.0-or-later
//! Regressions for observed taskbar anchoring and hover stalls. No XR pose injection.
use super::*;
use crate::spatial::{add, dot, scale, sub};
use xr_spatial_kit::{Scene, Widget, taskbar};

fn peer(d: &Desktop, id: u64, scene: Scene) -> Peer {
    Peer {
        socket: SeqPacket::connect(&d.path).unwrap(),
        id,
        generation: "1".into(),
        role: if id == 1 { "dock" } else { "system_ui" }.into(),
        capabilities: CAPABILITIES.iter().map(|s| s.to_string()).collect(),
        revision: 1,
        nodes: scene
            .widgets
            .iter()
            .map(|w| {
                let n: Node = serde_json::from_value(w.upsert()).unwrap();
                (n.node_id.clone(), n)
            })
            .collect(),
        root: Root {
            logical: scene.logical_size,
            size: scene.size_m,
            position: scene.position_m,
            policy: scene.policy,
            rotation: scene.rotation_degrees,
        },
        used_buffers: BTreeSet::new(),
    }
}
fn close(a: [f32; 3], b: [f32; 3]) {
    assert!(
        sub(a, b).iter().all(|d| d.abs() < 0.00001),
        "{a:?} != {b:?}"
    );
}
#[test]
fn hover_reuses_other_rasters_without_freezing_text_or_window_updates() {
    let dir = std::env::temp_dir().join(format!(
        "xr-hover-regression-{}-{}",
        std::process::id(),
        monotonic()
    ));
    let mut d = Desktop::new(&dir.join("shell.sock"), true).unwrap();
    let mut scene = Scene::new([320., 100.], [0.32, 0.1], [0., 1.6, -1.7], "world_locked");
    scene.push(Widget::card("bg", [0., 0., 320., 100.], "panel.background"));
    for (id, x) in [("a", 10.), ("b", 160.)] {
        scene.push(Widget::button(
            id,
            [x, 20., 140., 60.],
            id,
            "ui:test",
            "button.secondary",
        ));
    }
    d.peers.insert(1, peer(&d, 1, scene));
    d.windows.insert(
        "test:1".into(),
        Window {
            owner: 1,
            state: json!({"role":"toplevel","workspace":0}),
            width: 32,
            height: 32,
            pixels: vec![255; 32 * 32 * 4],
            position: [0., 1.5, -1.5],
            size: [0.32, 0.32],
            frame: 1,
            minimized: false,
            curved: false,
        },
    );
    let rasters = |d: &mut Desktop| -> BTreeMap<String, Arc<[u8]>> {
        d.dirty = true;
        d.panels()
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|p| {
                assert_eq!(p.rgba.len(), (p.width * p.height * 4) as usize);
                (p.id, p.rgba)
            })
            .collect()
    };
    let normal = rasters(&mut d);
    d.hover = Some((1, "a".into()));
    let hover = rasters(&mut d);
    for (id, pixels) in &normal {
        assert_eq!(Arc::ptr_eq(pixels, &hover[id]), id != "node:1:a", "{id}");
    }
    assert_ne!(normal["node:1:a"].as_ref(), hover["node:1:a"].as_ref());
    d.hover = None;
    let restored = rasters(&mut d);
    assert!(Arc::ptr_eq(&normal["node:1:a"], &restored["node:1:a"]));
    d.capture = Some(Capture {
        target: Target::Node(1, "a".into()),
        surface: None,
        resize_frame: None,
        size: [0.; 2],
        logical: [0.; 2],
        id: "1".into(),
        interaction: "1".into(),
        source: "regression".into(),
        start: [0.; 2],
        position: [0.; 3],
    });
    let pressed = rasters(&mut d);
    assert_ne!(pressed["node:1:a"].as_ref(), normal["node:1:a"].as_ref());
    assert!(Arc::ptr_eq(&pressed["node:1:b"], &normal["node:1:b"]));
    d.capture = None;
    d.peers
        .get_mut(&1)
        .unwrap()
        .nodes
        .get_mut("b")
        .unwrap()
        .text = "updated".into();
    d.windows.get_mut("test:1").unwrap().frame += 1;
    d.windows.get_mut("test:1").unwrap().pixels.fill(128);
    let updated = rasters(&mut d);
    assert!(!Arc::ptr_eq(&updated["node:1:b"], &normal["node:1:b"]));
    assert!(!Arc::ptr_eq(
        &updated["window:test:1"],
        &normal["window:test:1"]
    ));
    assert!(Arc::ptr_eq(&updated["node:1:a"], &normal["node:1:a"]));
    assert!(Arc::ptr_eq(&updated["node:1:bg"], &normal["node:1:bg"]));
    drop(d);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn islands_share_motion_capture_and_scene_refresh_without_moving_world_panels() {
    let dir = std::env::temp_dir().join(format!(
        "xr-taskbar-regression-{}-{}",
        std::process::id(),
        monotonic()
    ));
    let mut d = Desktop::new(&dir.join("shell.sock"), true).unwrap();
    let mut dock = Scene::new(
        taskbar::CENTER_LOGICAL,
        taskbar::CENTER_SIZE,
        taskbar::CENTER_POSITION,
        taskbar::POLICY,
    )
    .with_rotation(taskbar::ROTATION);
    dock.push(Widget::card(
        "center",
        [0., 0., 620., 72.],
        "dock.background",
    ));
    dock.push(
        Widget::spatial_group(
            "left",
            taskbar::SIDE_LOGICAL,
            taskbar::SIDE_SIZE,
            taskbar::LEFT_POSITION,
        )
        .with_rotation(taskbar::ROTATION)
        .with_spatial_policy(taskbar::POLICY),
    );
    dock.push(Widget::card("left-bg", [0., 0., 440., 72.], "dock.background").parent("left"));
    let mut system = Scene::new([600., 700.], [0.6, 0.7], [1.18, 1.6, -1.74], "world_locked")
        .with_rotation([0., -18., 0.]);
    system.push(
        Widget::spatial_group(
            "right",
            taskbar::SIDE_LOGICAL,
            taskbar::SIDE_SIZE,
            taskbar::RIGHT_POSITION,
        )
        .with_rotation(taskbar::ROTATION)
        .with_spatial_policy(taskbar::POLICY),
    );
    system.push(Widget::card("right-bg", [0., 0., 440., 72.], "dock.background").parent("right"));
    system.push(
        Widget::button(
            "status",
            [26., 10., 52., 52.],
            "",
            "ui:control",
            "dock.status-icon",
        )
        .parent("right-bg"),
    );
    system.push(Widget::card(
        "control",
        [0., 0., 600., 700.],
        "panel.background",
    ));
    d.peers.insert(1, peer(&d, 1, dock));
    d.peers.insert(2, peer(&d, 2, system));
    let frame = |d: &Desktop, id, name: &str| {
        let p = &d.peers[&id];
        d.node_frame(p, &p.nodes[name]).unwrap()
    };
    let world = frame(&d, 2, "control");
    d.update_head([0., 1.6, 0.], [0., 0., 0., 1.], 1. / 72.);
    assert_eq!(d.taskbar_follow.mode(), "fixed");
    let fixed_center = frame(&d, 1, "center");
    d.update_head([0.3, 1.7, 0.2], [0., 0.3826834, 0., 0.9238795], 1.);
    close(frame(&d, 1, "center").position, fixed_center.position);
    d.taskbar_follow.set_mode(true);
    let yaw = 45f32.to_radians();
    let q = [0., (yaw / 2.).sin(), 0., (yaw / 2.).cos()];
    for _ in 0..120 {
        d.update_head([0.3, 1.7, 0.2], q, 1. / 72.);
    }
    let center = frame(&d, 1, "center");
    let left = frame(&d, 1, "left-bg");
    let right = frame(&d, 2, "right-bg");
    assert!(
        sub(center.position, taskbar::CENTER_POSITION)
            .iter()
            .any(|d| d.abs() > 0.2)
    );
    assert!(center.right[2].abs() > 0.5);
    close(
        sub(left.position, center.position),
        scale(center.right, -0.9632),
    );
    close(
        sub(right.position, center.position),
        scale(center.right, 0.9632),
    );
    close(left.right, center.right);
    close(right.down, center.down);
    close(frame(&d, 2, "control").position, world.position);
    close(frame(&d, 2, "control").normal(), world.normal());
    // The same resolved frame drives rendering and ray projection after motion.
    let button = frame(&d, 2, "status");
    let (xy, _) = button
        .project(
            add(button.position, scale(button.normal(), 0.3)),
            scale(button.normal(), -1.),
        )
        .unwrap();
    assert!((xy[0] - 26.).abs() < 0.001 && (xy[1] - 26.).abs() < 0.001);
    assert!(dot(button.normal(), center.normal()) > 0.9999);
    d.capture = Some(Capture {
        target: Target::Node(2, "status".into()),
        surface: None,
        resize_frame: None,
        size: [0.; 2],
        logical: [0.; 2],
        id: "1".into(),
        interaction: "1".into(),
        source: "regression".into(),
        start: [0.; 2],
        position: [0.; 3],
    });
    assert!(!d.update_head([1., 1.8, 1.], [0., 0., 0., 1.], 1.));
    close(frame(&d, 1, "center").position, center.position);
    close(frame(&d, 1, "left-bg").position, left.position);
    close(frame(&d, 2, "right-bg").position, right.position);
    d.capture = None;
    // Periodic system-state transactions or a reconnect do not restart the anchor.
    let node = d.peers[&2].nodes["right"].clone();
    d.peers
        .get_mut(&2)
        .unwrap()
        .nodes
        .insert("right".into(), node);
    close(frame(&d, 2, "right-bg").position, right.position);
    d.update_head([0.5, 1.7, 0.4], q, 1. / 72.);
    assert_ne!(frame(&d, 1, "center").position, center.position);
    close(
        sub(
            frame(&d, 2, "right-bg").position,
            frame(&d, 1, "center").position,
        ),
        scale(center.right, 0.9632),
    );
    let current = frame(&d, 1, "center");
    d.taskbar_follow.set_mode(false);
    d.update_head([1., 2., 1.], [0., 0., 0., 1.], 1.);
    close(frame(&d, 1, "center").position, current.position);
    close(frame(&d, 1, "center").normal(), current.normal());
    d.taskbar_follow.set_mode(true);
    close(frame(&d, 1, "center").position, current.position);
    close(frame(&d, 1, "center").normal(), current.normal());
    drop(d);
    std::fs::remove_dir_all(dir).unwrap();
}
