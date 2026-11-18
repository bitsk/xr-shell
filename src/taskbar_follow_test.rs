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
        spawn_frames: BTreeMap::new(),
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
            rotation: [0.; 3],
            placed: true,
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
    d.hovers.insert("regression".into(), (1, "a".into()));
    let hover = rasters(&mut d);
    for (id, pixels) in &normal {
        assert!(Arc::ptr_eq(pixels, &hover[id]), "hover repainted {id}");
    }
    assert_eq!(normal["node:1:a"].as_ref(), hover["node:1:a"].as_ref());
    d.hovers.clear();
    let restored = rasters(&mut d);
    assert!(Arc::ptr_eq(&normal["node:1:a"], &restored["node:1:a"]));
    d.captures.insert(
        "regression".into(),
        Capture {
            panel: None,
            button: 272,
            button_sent: false,
            click: None,
            target: Target::Node(1, "a".into()),
            surface: None,
            resize_frame: None,
            resize_rotation: [0.; 3],
            size: [0.; 2],
            logical: [0.; 2],
            id: "1".into(),
            interaction: "1".into(),
            grab: None,
            start: [0.; 2],
            position: [0.; 3],
        },
    );
    let pressed = rasters(&mut d);
    assert!(Arc::ptr_eq(&pressed["node:1:a"], &normal["node:1:a"]));
    assert!(Arc::ptr_eq(&pressed["node:1:b"], &normal["node:1:b"]));
    d.captures.clear();
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
    // The audited adjacent-button case: slop from the earlier draw must
    // not steal a real face hit or a nearer slop edge from the next button.
    d.windows.clear();
    for (id, x) in [("a", 10.), ("b", 32.)] {
        d.peers.get_mut(&1).unwrap().nodes.get_mut(id).unwrap().rect = [x, 20., 20., 20.];
    }
    d.horizon_head = Some(([0., 1.6, 0.], [0., 0., 0., 1.]));
    for x in [42., 31.8] {
        let origin = [(x - 160.) * 0.001, 1.6 - (30. - 50.) * 0.001, 0.];
        assert_eq!(
            d.pointer_target("audit", origin, [0., 0., -1.]).as_deref(),
            Some("node:1:b")
        );
    }
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
    dock.push(Widget::card("left-bg", [0., 0., 340., 72.], "dock.background").parent("left"));
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
    system.push(Widget::card("right-bg", [0., 0., 340., 72.], "dock.background").parent("right"));
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
    let horizontal_normal = [-center.right[2], 0., center.right[0]];
    for (island, design) in [
        (left, taskbar::LEFT_POSITION),
        (right, taskbar::RIGHT_POSITION),
    ] {
        close(
            sub(island.position, center.position),
            add(
                scale(center.right, design[0]),
                scale(horizontal_normal, design[2] - taskbar::CENTER_POSITION[2]),
            ),
        );
        assert!((design[0].hypot(design[2]) - taskbar::RADIUS_M).abs() < 0.00001);
        assert_eq!(island.curve, taskbar::RADIUS_M);
        assert!((island.normal()[1] - center.normal()[1]).abs() < 0.00001);
        // Curved face pixels must round-trip through the same ray intersection.
        for xy in [[8., 36.], [170., 36.], [332., 36.]] {
            let point = island.point(xy, 0.);
            let normal = island.normal_at(xy);
            let (hit, _) = island
                .project(add(point, scale(normal, 0.3)), scale(normal, -1.))
                .unwrap();
            assert!((hit[0] - xy[0]).abs() < 0.001 && (hit[1] - xy[1]).abs() < 0.001);
        }
    }
    let arc_gap = taskbar::RADIUS_M * taskbar::RIGHT_POSITION[0].atan2(-taskbar::RIGHT_POSITION[2])
        - taskbar::CENTER_SIZE[0] * 0.5
        - taskbar::SIDE_SIZE[0] * 0.5;
    assert!((arc_gap - taskbar::GAP_PX * taskbar::SCALE_M).abs() < 0.00001);
    assert!(dot(left.normal(), center.normal()) < 0.99);
    assert!(dot(right.normal(), center.normal()) < 0.99);
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
    assert!(dot(button.normal(), right.normal_at([52., 36.])) > 0.9999);
    d.captures.insert(
        "regression".into(),
        Capture {
            panel: None,
            button: 272,
            button_sent: false,
            click: None,
            target: Target::Node(2, "status".into()),
            surface: None,
            resize_frame: None,
            resize_rotation: [0.; 3],
            size: [0.; 2],
            logical: [0.; 2],
            id: "1".into(),
            interaction: "1".into(),
            grab: None,
            start: [0.; 2],
            position: [0.; 3],
        },
    );
    assert!(!d.update_head([1., 1.8, 1.], [0., 0., 0., 1.], 1.));
    close(frame(&d, 1, "center").position, center.position);
    close(frame(&d, 1, "left-bg").position, left.position);
    close(frame(&d, 2, "right-bg").position, right.position);
    d.captures.clear();
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
        add(
            scale(center.right, taskbar::RIGHT_POSITION[0]),
            scale(
                horizontal_normal,
                taskbar::RIGHT_POSITION[2] - taskbar::CENTER_POSITION[2],
            ),
        ),
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

fn interaction_desktop() -> Desktop {
    let dir = std::env::temp_dir().join(format!(
        "xr-input-regression-{}-{}",
        std::process::id(),
        monotonic()
    ));
    let mut d = Desktop::new(&dir.join("shell.sock"), true).unwrap();
    d.ui_anchor_ready = true;
    d.focused = true;
    d.horizon_head = Some(([0., 1.5, 0.], [0., 0., 0., 1.]));
    d.active_peer = Some((
        9,
        "window_server".into(),
        Value::Null,
        vec!["focus.v1".into(), "window.v1".into()],
    ));
    for (key, x, id) in [("test:1", 0., "1"), ("test:2", 1.2, "2")] {
        d.windows.insert(key.into(),Window {owner:9,state:json!({"role":"toplevel","workspace":0,"width":800,"height":600,"surface_id":id,"window_server_generation":"test"}),width:800,height:600,pixels:vec![255;800*600*4],position:[x,1.5,-1.5],rotation:[0.;3],placed:true,size:[0.8,0.6],frame:1,minimized:false,curved:false});
    }
    d.focus = Some("test:1".into());
    d.focus_applied = true;
    d
}
fn inputs(d: &Desktop) -> Vec<Value> {
    d.outbox
        .borrow()
        .iter()
        .filter(|(_, kind, _)| kind == "SurfaceInput")
        .map(|(_, _, p)| p.clone())
        .collect()
}
#[test]
fn popup_removal_reconciles_only_its_focused_ancestor() {
    let mut d = interaction_desktop();
    let mut popup = d.windows["test:1"].clone();
    popup.state = json!({"role":"popup","surface_id":"3","parent_surface_id":"1","window_server_generation":"test"});
    d.windows.insert("test:3".into(), popup.clone());
    let epoch = d.focus_epoch;
    d.remove_window("test:3");
    assert_eq!(d.focus.as_deref(), Some("test:1"));
    assert!(d.focus_applied);
    assert_eq!(d.focus_epoch, epoch);
    assert!(
        d.outbox
            .borrow()
            .iter()
            .any(|(_, kind, p)| kind == "FocusChanged" && p["surface_id"] == "1")
    );
    d.focus = Some("test:2".into());
    d.focus_applied = true;
    d.outbox.borrow_mut().clear();
    d.windows.insert("test:3".into(), popup);
    d.remove_window("test:3");
    assert_eq!(d.focus_epoch, epoch);
    assert!(d.focus_applied);
    assert!(d.outbox.borrow().is_empty());
}
#[test]
fn surface_buttons_pending_focus_cancel_and_double_click_coordinates() {
    let mut d = interaction_desktop();
    let source = "debug_ray:left";
    let hit = |xy| Some((Target::Window("test:1".into()), xy));
    for button in [272, 273, 274] {
        d.outbox.borrow_mut().clear();
        d.pointer_button(hit([110., 120.]), "press", source, button)
            .unwrap();
        d.cancel_pointer(source, "tracking lost");
        let events = inputs(&d);
        let buttons: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == "pointer_button")
            .map(|e| {
                (
                    e["button"].as_u64().unwrap(),
                    e["pressed"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(buttons, vec![(button as u64, true), (button as u64, false)]);
    }
    // A pending right press retains its identity after asynchronous FocusApplied.
    d.focus_applied = false;
    d.pointer_button(hit([130., 140.]), "press", source, 273)
        .unwrap();
    d.outbox.borrow_mut().clear();
    d.focus_applied = true;
    d.flush_pending_surface_input().unwrap();
    assert!(
        inputs(&d)
            .iter()
            .any(|p| p["button"] == 273 && p["pressed"] == true)
    );
    d.cancel_pointer(source, "cancel");
    // Release before focus ack must produce neither delayed press nor orphan release.
    d.focus_applied = false;
    d.pointer_button(hit([150., 160.]), "press", source, 274)
        .unwrap();
    d.pointer_button(hit([150., 160.]), "release", source, 274)
        .unwrap();
    d.outbox.borrow_mut().clear();
    d.focus_applied = true;
    d.flush_pending_surface_input().unwrap();
    assert!(inputs(&d).is_empty());
    d.pointer_button(hit([200., 220.]), "press", source, 272)
        .unwrap();
    d.pointer_button(hit([201., 220.]), "release", source, 272)
        .unwrap();
    d.last_clicks.get_mut(source).unwrap().at = Instant::now() - Duration::from_millis(220);
    d.outbox.borrow_mut().clear();
    d.pointer_button(hit([214., 220.]), "press", source, 272)
        .unwrap();
    d.pointer_button(hit([216., 221.]), "release", source, 272)
        .unwrap();
    let motions: Vec<_> = inputs(&d)
        .into_iter()
        .filter(|p| p["kind"] == "pointer_motion")
        .collect();
    assert_eq!(motions.len(), 2);
    assert!(motions.iter().all(|p| p["x"] == 200. && p["y"] == 220.));
    // A drag must not seed a stabilized click, and cancellation clears history.
    d.pointer_button(hit([300., 220.]), "press", source, 272)
        .unwrap();
    d.pointer_button(hit([370., 220.]), "drag", source, 272)
        .unwrap();
    d.pointer_button(hit([370., 220.]), "release", source, 272)
        .unwrap();
    assert!(!d.last_clicks.contains_key(source));
    d.pointer_button(hit([200., 220.]), "press", source, 272)
        .unwrap();
    d.pointer_button(hit([200., 220.]), "release", source, 272)
        .unwrap();
    d.set_xr_focus(false);
    assert!(d.last_clicks.is_empty());
}
#[test]
fn squeeze_moves_and_scales_without_client_click_and_hover_scroll_owns_target() {
    let mut d = interaction_desktop();
    let ray = [0., 0., -1.];
    let origin = [-0.12, 1.5, 0.];
    let initial = d.windows["test:1"].position;
    let size = d.windows["test:1"].size;
    d.ray_squeeze(origin, ray, "press", "debug_ray:left")
        .unwrap();
    assert!(matches!(
        d.captures["debug_ray:left"].target,
        Target::Move(_)
    ));
    d.ray_squeeze([0.12, 1.5, 0.], ray, "press", "debug_ray:right")
        .unwrap();
    assert!(d.bimanual.contains_key("test:1"));
    d.ray_squeeze([0.25, 1.5, 0.], ray, "drag", "debug_ray:right")
        .unwrap();
    assert!(d.windows["test:1"].size[0] > size[0]);
    assert!(inputs(&d).is_empty());
    d.cancel("back");
    assert_eq!(d.windows["test:1"].position, initial);
    assert_eq!(d.windows["test:1"].size, size);
    d.outbox.borrow_mut().clear();
    d.controller_scroll_at("debug_ray:left", [1.2, 1.5, 0.], ray, [0., 12.])
        .unwrap();
    assert_eq!(d.focus.as_deref(), Some("test:2"));
    assert!(inputs(&d).is_empty());
    d.focus_applied = true;
    d.flush_pending_surface_input().unwrap();
    let events = inputs(&d);
    assert!(
        events
            .iter()
            .any(|p| p["kind"] == "scroll" && p["surface_id"] == "2" && p["dy"] == 12.)
    );
    // Other hand scrolling cannot steal a live legacy seat capture.
    d.ray_button([1.2, 1.5, 0.], ray, "press", "debug_ray:right", 273)
        .unwrap();
    d.outbox.borrow_mut().clear();
    d.controller_scroll_at("debug_ray:left", [0., 1.5, 0.], ray, [0., 12.])
        .unwrap();
    assert_eq!(d.focus.as_deref(), Some("test:2"));
    assert!(inputs(&d).is_empty());
    d.cancel("test complete");
}

#[test]
fn component_scroll_requires_hovered_panel_threshold_and_cooldown() {
    let mut d = interaction_desktop();
    d.windows.clear();
    let mut scene = Scene::new([320., 100.], [0.32, 0.1], [0., 1.6, -1.7], "world_locked");
    scene.push(Widget::card(
        "panel",
        [0., 0., 320., 100.],
        "panel.background",
    ));
    scene.push(
        Widget::button(
            "page-next",
            [200., 20., 80., 50.],
            "next",
            "page-next",
            "button.secondary",
        )
        .parent("panel"),
    );
    d.peers.insert(1, peer(&d, 1, scene));
    let count = |d: &Desktop| d.stats["input_events"].as_u64().unwrap_or(0);
    assert!(
        d.controller_page_at(
            "controller_ray:left",
            [0., 1.6, 0.],
            [0., 0., -1.],
            [0.64, 0.]
        )
        .unwrap()
    );
    assert_eq!(count(&d), 0);
    assert!(
        d.controller_page_at(
            "controller_ray:left",
            [0., 1.6, 0.],
            [0., 0., -1.],
            [0.8, 0.]
        )
        .unwrap()
    );
    assert_eq!(count(&d), 2);
    d.controller_page_at(
        "controller_ray:left",
        [0., 1.6, 0.],
        [0., 0., -1.],
        [0.8, 0.],
    )
    .unwrap();
    assert_eq!(count(&d), 2);
    assert!(
        !d.controller_page_at(
            "controller_ray:right",
            [2., 1.6, 0.],
            [0., 0., -1.],
            [0.8, 0.]
        )
        .unwrap()
    );
    assert_eq!(count(&d), 2);
}

#[test]
fn panel_handles_move_scale_and_cancel_without_client_input() {
    let mut d = interaction_desktop();
    d.windows.clear();
    let mut scene = Scene::new([1000., 334.], [1., 0.334], [0., 1.5, -1.6], "world_locked");
    scene.push(Widget::horizon_panel(
        "keyboard",
        [1000., 334.],
        [1., 0.334],
        0.,
        -10.,
        1.6,
    ));
    for (id, rect, action) in [
        (
            "move",
            [340., 278., 320., 48.],
            xr_shell_protocols::PANEL_MOVE_ACTION,
        ),
        (
            "scale",
            [832., 278., 144., 48.],
            xr_shell_protocols::PANEL_SCALE_ACTION,
        ),
    ] {
        scene.push(Widget::button(id, rect, id, action, "button.secondary").parent("keyboard"));
    }
    let mut p = peer(&d, 2, scene);
    Desktop::freeze_spawn_frames(&mut p, d.horizon_head.unwrap());
    let original = p.spawn_frames["keyboard"];
    d.peers.insert(2, p);
    let source = "debug_ray:left";
    let origin = [0., 1.5, 0.];
    let direction = |point| {
        let delta = sub(point, origin);
        scale(delta, 1. / dot(delta, delta).sqrt())
    };
    let handle =
        |d: &Desktop, id: &str| d.node_frame(&d.peers[&2], &d.peers[&2].nodes[id]).unwrap();
    let start = handle(&d, "move").position;
    d.ray(origin, direction(start), "press", source).unwrap();
    assert!(d.captures[source].panel.is_some());
    let end = add(start, [0.2, 0.1, 0.]);
    d.ray(origin, direction(end), "drag", source).unwrap();
    assert!(d.peers[&2].spawn_frames["keyboard"].position[0] > 0.1);
    d.ray(origin, direction(end), "release", source).unwrap();
    assert!(!d.captures.contains_key(source));
    let moved = d.peers[&2].spawn_frames["keyboard"];
    assert_eq!(moved.size, original.size);
    let start = handle(&d, "scale").position;
    d.ray(origin, direction(start), "press", source).unwrap();
    // A second pointer cannot mutate the same panel during the gesture.
    let other = handle(&d, "move").position;
    d.ray(origin, direction(other), "press", "debug_ray:right")
        .unwrap();
    assert!(!d.captures.contains_key("debug_ray:right"));
    let end = add(start, scale(moved.right, 0.25));
    d.ray(origin, direction(end), "drag", source).unwrap();
    let scaled = d.peers[&2].spawn_frames["keyboard"];
    assert!(scaled.size[0] > moved.size[0]);
    assert!((scaled.size[0] / scaled.size[1] - moved.size[0] / moved.size[1]).abs() < 0.00001);
    close(scaled.position, moved.position);
    d.cancel_pointer(source, "tracking lost");
    assert_eq!(d.peers[&2].spawn_frames["keyboard"].size, moved.size);
    close(
        d.peers[&2].spawn_frames["keyboard"].position,
        moved.position,
    );
    assert_eq!(d.stats["input_events"].as_u64().unwrap_or(0), 0);
    // Re-publishing a keyboard page preserves the Shell-owned frame.
    Desktop::freeze_spawn_frames(d.peers.get_mut(&2).unwrap(), d.horizon_head.unwrap());
    close(
        d.peers[&2].spawn_frames["keyboard"].position,
        moved.position,
    );
}
