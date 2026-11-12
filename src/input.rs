// SPDX-License-Identifier: GPL-3.0-or-later
use crate::desktop::Desktop;
use anyhow::{Context, Result};
use openxr as xr;
use std::time::Instant;

const SOURCES: [&str; 2] = ["controller_ray:left", "controller_ray:right"];

/// One xrSyncActions sample. Poses remain absent when the runtime marks them
/// invalid; controls are independent of positional tracking.
#[derive(Clone, Copy, Default)]
pub struct HandState {
    pub controls_active: bool,
    pub channel_active: [bool; 4],
    pub grip: Option<xr::Posef>,
    pub aim: Option<xr::Posef>,
    pub select: bool,
    pub trigger: f32,
    pub squeeze: f32,
    pub menu: bool,
    pub secondary: bool,
    pub thumb_click: bool,
    pub stick: [f32; 2],
}

#[derive(Default)]
struct Observation {
    buttons: [bool; 6],
    state: Option<HandState>,
    logged: Option<Instant>,
}

#[derive(Clone, Copy)]
struct Ray {
    origin: [f32; 3],
    direction: [f32; 3],
    channel: usize,
    phase: &'static str,
}

impl HandState {
    pub fn aim_ray(&self) -> Option<([f32; 3], [f32; 3])> {
        let pose = self.aim?;
        let q = pose.orientation;
        let p = pose.position;
        let mut direction = [
            -2. * (q.x * q.z + q.w * q.y),
            -2. * (q.y * q.z - q.w * q.x),
            -1. + 2. * (q.x * q.x + q.y * q.y),
        ];
        let length = direction.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !length.is_finite() || length < 0.0001 {
            return None;
        }
        for v in &mut direction {
            *v /= length;
        }
        Some(([p.x, p.y, p.z], direction))
    }
}

pub struct Input {
    set: xr::ActionSet,
    aim: xr::Action<xr::Posef>,
    grip: xr::Action<xr::Posef>,
    select: xr::Action<bool>,
    trigger: xr::Action<f32>,
    squeeze: xr::Action<f32>,
    menu: xr::Action<bool>,
    back: xr::Action<bool>,
    secondary: xr::Action<bool>,
    thumb_click: xr::Action<bool>,
    trigger_click: xr::Action<bool>,
    squeeze_click: xr::Action<bool>,
    stick: xr::Action<xr::Vector2f>,
    spaces: [xr::Space; 2],
    grip_spaces: [xr::Space; 2],
    hands: [xr::Path; 2],
    pressed: [bool; 2],
    gates: [ButtonGate; 2],
    haptic: xr::Action<xr::Haptic>,
    last_hover: [Option<String>; 2],
    controllers: [HandState; 2],
    menu_pressed: [bool; 2],
    require_menu_release: [bool; 2],
    updated: Instant,
    observations: [Observation; 2],
    routes: [&'static str; 2],
    route_ok: [bool; 2],
    cancelled: [bool; 2],
    back_routed: [bool; 2],
    scroll_delta: [[f32; 2]; 2],
    scroll_ok: [bool; 2],
}
impl Input {
    pub fn new(instance: &xr::Instance, session: &xr::Session<xr::Vulkan>) -> Result<Self> {
        eprintln!("XR_INIT_STAGE=input-hand-paths STATUS=begin");
        let hands = [
            instance
                .string_to_path("/user/hand/left")
                .context("xrStringToPath(/user/hand/left)")?,
            instance
                .string_to_path("/user/hand/right")
                .context("xrStringToPath(/user/hand/right)")?,
        ];
        eprintln!("XR_INIT_STAGE=input-hand-paths STATUS=complete");
        eprintln!("XR_INIT_STAGE=input-action-set STATUS=begin");
        let set = instance
            .create_action_set("desktop", "Desktop interaction", 0)
            .context("xrCreateActionSet(desktop)")?;
        eprintln!("XR_INIT_STAGE=input-action-set STATUS=complete");
        eprintln!("XR_INIT_STAGE=input-action-aim STATUS=begin");
        let aim = set
            .create_action::<xr::Posef>("aim", "Aim ray", &hands)
            .context("xrCreateAction(aim, pose)")?;
        eprintln!("XR_INIT_STAGE=input-action-aim STATUS=complete");
        let grip = set.create_action::<xr::Posef>("grip", "Controller grip", &hands)?;
        let squeeze = set.create_action::<f32>("squeeze", "Grip pressure", &hands)?;
        let menu = set.create_action::<bool>("menu", "Application menu", &hands)?;
        let haptic = set.create_action::<xr::Haptic>("feedback", "Spatial feedback", &hands)?;
        let back = set.create_action::<bool>("back", "Back", &hands)?;
        let secondary = set.create_action::<bool>("secondary", "Secondary click", &hands)?;
        let thumb_click = set.create_action::<bool>("thumb_click", "Middle click", &hands)?;
        let trigger_click =
            set.create_action::<bool>("trigger_click", "Digital trigger", &hands)?;
        let squeeze_click = set.create_action::<bool>("squeeze_click", "Digital grip", &hands)?;
        let stick = set.create_action::<xr::Vector2f>("scroll", "Scroll", &hands)?;
        eprintln!("XR_INIT_STAGE=input-action-select STATUS=begin");
        let select = set
            .create_action::<bool>("select", "Select", &hands)
            .context("xrCreateAction(select, boolean)")?;
        eprintln!("XR_INIT_STAGE=input-action-select STATUS=complete");
        eprintln!("XR_INIT_STAGE=input-action-trigger STATUS=begin");
        let trigger = set
            .create_action::<f32>("trigger", "Trigger", &hands)
            .context("xrCreateAction(trigger, float)")?;
        eprintln!("XR_INIT_STAGE=input-action-trigger STATUS=complete");
        for (profile, button, float) in [
            (
                "/interaction_profiles/khr/simple_controller",
                "select/click",
                false,
            ),
            (
                "/interaction_profiles/htc/vive_controller",
                "trigger/click",
                false,
            ),
            (
                "/interaction_profiles/oculus/touch_controller",
                "trigger/value",
                true,
            ),
            (
                "/interaction_profiles/valve/index_controller",
                "trigger/value",
                true,
            ),
        ] {
            eprintln!("XR_INIT_STAGE=input-profile STATUS=begin PROFILE={profile}");
            let mut bindings = Vec::new();
            for hand in ["left", "right"] {
                bindings.push(xr::Binding::new(
                    &haptic,
                    instance.string_to_path(&format!("/user/hand/{hand}/output/haptic"))?,
                ));
                let grip_path = format!("/user/hand/{hand}/input/grip/pose");
                bindings.push(xr::Binding::new(
                    &grip,
                    instance.string_to_path(&grip_path)?,
                ));
                let aim_path = format!("/user/hand/{hand}/input/aim/pose");
                bindings.push(xr::Binding::new(
                    &aim,
                    instance
                        .string_to_path(&aim_path)
                        .with_context(|| format!("xrStringToPath({aim_path})"))?,
                ));
                let button_path = format!("/user/hand/{hand}/input/{button}");
                let path = instance
                    .string_to_path(&button_path)
                    .with_context(|| format!("xrStringToPath({button_path})"))?;
                if profile == "/interaction_profiles/khr/simple_controller"
                    || profile == "/interaction_profiles/htc/vive_controller"
                    || (profile == "/interaction_profiles/oculus/touch_controller"
                        && hand == "left")
                {
                    let path = format!("/user/hand/{hand}/input/menu/click");
                    bindings.push(xr::Binding::new(&menu, instance.string_to_path(&path)?));
                }
                if [
                    "/interaction_profiles/oculus/touch_controller",
                    "/interaction_profiles/valve/index_controller",
                ]
                .contains(&profile)
                {
                    let select_path = format!(
                        "/user/hand/{hand}/input/{}/click",
                        if hand == "left"
                            && profile == "/interaction_profiles/oculus/touch_controller"
                        {
                            "x"
                        } else {
                            "a"
                        }
                    );
                    bindings.push(xr::Binding::new(
                        &secondary,
                        instance.string_to_path(&select_path)?,
                    ));
                    let squeeze_path = format!("/user/hand/{hand}/input/squeeze/value");
                    bindings.push(xr::Binding::new(
                        &squeeze,
                        instance.string_to_path(&squeeze_path)?,
                    ));
                    bindings.push(xr::Binding::new(
                        &thumb_click,
                        instance
                            .string_to_path(&format!("/user/hand/{hand}/input/thumbstick/click"))?,
                    ));
                    let stick_path = format!("/user/hand/{hand}/input/thumbstick");
                    bindings.push(xr::Binding::new(
                        &stick,
                        instance.string_to_path(&stick_path)?,
                    ));
                    let back_path = format!(
                        "/user/hand/{hand}/input/{}/click",
                        if hand == "left"
                            && profile == "/interaction_profiles/oculus/touch_controller"
                        {
                            "y"
                        } else {
                            "b"
                        }
                    );
                    bindings.push(xr::Binding::new(
                        &back,
                        instance.string_to_path(&back_path)?,
                    ));
                } else if profile == "/interaction_profiles/htc/vive_controller" {
                    bindings.push(xr::Binding::new(
                        &secondary,
                        instance
                            .string_to_path(&format!("/user/hand/{hand}/input/trackpad/click"))?,
                    ));
                    bindings.push(xr::Binding::new(
                        &squeeze_click,
                        instance
                            .string_to_path(&format!("/user/hand/{hand}/input/squeeze/click"))?,
                    ));
                    let stick_path = format!("/user/hand/{hand}/input/trackpad");
                    bindings.push(xr::Binding::new(
                        &stick,
                        instance.string_to_path(&stick_path)?,
                    ));
                }
                if float {
                    bindings.push(xr::Binding::new(&trigger, path));
                } else if profile == "/interaction_profiles/khr/simple_controller" {
                    bindings.push(xr::Binding::new(&select, path));
                } else {
                    bindings.push(xr::Binding::new(&trigger_click, path));
                }
            }
            let profile_path = instance
                .string_to_path(profile)
                .with_context(|| format!("xrStringToPath({profile})"))?;
            if let Err(error) =
                instance.suggest_interaction_profile_bindings(profile_path, &bindings)
            {
                eprintln!("XR_INPUT_PROFILE_UNAVAILABLE={profile} {error:?}");
            } else {
                eprintln!("XR_INIT_STAGE=input-profile STATUS=complete PROFILE={profile}");
            }
        }
        eprintln!("XR_INIT_STAGE=input-attach-action-sets STATUS=begin");
        session
            .attach_action_sets(&[&set])
            .context("xrAttachSessionActionSets(desktop)")?;
        eprintln!("XR_INIT_STAGE=input-attach-action-sets STATUS=complete");
        eprintln!("XR_INIT_STAGE=input-action-space STATUS=begin HAND=left");
        let left_space = aim
            .create_space(session, hands[0], xr::Posef::IDENTITY)
            .context("xrCreateActionSpace(aim, left, identity)")?;
        eprintln!("XR_INIT_STAGE=input-action-space STATUS=complete HAND=left");
        eprintln!("XR_INIT_STAGE=input-action-space STATUS=begin HAND=right");
        let right_space = aim
            .create_space(session, hands[1], xr::Posef::IDENTITY)
            .context("xrCreateActionSpace(aim, right, identity)")?;
        eprintln!("XR_INIT_STAGE=input-action-space STATUS=complete HAND=right");
        let spaces = [left_space, right_space];
        let grip_spaces = [
            grip.create_space(session, hands[0], xr::Posef::IDENTITY)?,
            grip.create_space(session, hands[1], xr::Posef::IDENTITY)?,
        ];
        Ok(Self {
            set,
            aim,
            grip,
            select,
            trigger,
            squeeze,
            menu,
            back,
            secondary,
            thumb_click,
            trigger_click,
            squeeze_click,
            stick,
            spaces,
            grip_spaces,
            hands,
            pressed: [false; 2],
            gates: std::array::from_fn(|_| ButtonGate::default()),
            haptic,
            last_hover: [None, None],
            controllers: [HandState::default(); 2],
            menu_pressed: [false; 2],
            require_menu_release: [false; 2],
            updated: Instant::now(),
            observations: std::array::from_fn(|_| Observation::default()),
            routes: ["none"; 2],
            route_ok: [false; 2],
            cancelled: [false; 2],
            back_routed: [false; 2],
            scroll_delta: [[0.; 2]; 2],
            scroll_ok: [false; 2],
        })
    }

    pub fn controllers(&self) -> &[HandState; 2] {
        &self.controllers
    }

    fn pose(
        action: &xr::Action<xr::Posef>,
        space: &xr::Space,
        hand: xr::Path,
        session: &xr::Session<xr::Vulkan>,
        base: &xr::Space,
        time: xr::Time,
    ) -> Result<Option<xr::Posef>> {
        if !action.is_active(session, hand)? {
            return Ok(None);
        }
        let location = space.locate(base, time)?;
        let pose = location.pose;
        let q = pose.orientation;
        let p = pose.position;
        let norm = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
        Ok((location.location_flags.contains(
            xr::SpaceLocationFlags::ORIENTATION_VALID | xr::SpaceLocationFlags::POSITION_VALID,
        ) && [p.x, p.y, p.z, q.x, q.y, q.z, q.w]
            .iter()
            .all(|v| v.is_finite())
            && (0.9..=1.1).contains(&norm))
        .then_some(pose))
    }

    fn read_hand(
        &self,
        hand: usize,
        session: &xr::Session<xr::Vulkan>,
        base: &xr::Space,
        time: xr::Time,
    ) -> Result<HandState> {
        let path = self.hands[hand];
        let select = self.select.state(session, path)?;
        let trigger = self.trigger.state(session, path)?;
        let squeeze = self.squeeze.state(session, path)?;
        let menu = self.menu.state(session, path)?;
        let back = self.back.state(session, path)?;
        let stick = self.stick.state(session, path)?;
        let secondary = self.secondary.state(session, path)?;
        let thumb_click = self.thumb_click.state(session, path)?;
        let trigger_click = self.trigger_click.state(session, path)?;
        let squeeze_click = self.squeeze_click.state(session, path)?;
        let scalar = |active: bool, value: f32| {
            if active && value.is_finite() {
                value.clamp(0., 1.)
            } else {
                0.
            }
        };
        Ok(HandState {
            channel_active: [
                select.is_active || trigger.is_active || trigger_click.is_active,
                squeeze.is_active || squeeze_click.is_active,
                secondary.is_active,
                thumb_click.is_active,
            ],
            controls_active: select.is_active
                || trigger.is_active
                || squeeze.is_active
                || menu.is_active
                || back.is_active
                || stick.is_active
                || secondary.is_active
                || thumb_click.is_active
                || trigger_click.is_active
                || squeeze_click.is_active,
            grip: Self::pose(
                &self.grip,
                &self.grip_spaces[hand],
                path,
                session,
                base,
                time,
            )?,
            aim: Self::pose(&self.aim, &self.spaces[hand], path, session, base, time)?,
            select: select.is_active && select.current_state,
            trigger: if trigger_click.is_active && trigger_click.current_state {
                1.
            } else {
                scalar(trigger.is_active, trigger.current_state)
            },
            squeeze: if squeeze_click.is_active && squeeze_click.current_state {
                1.
            } else {
                scalar(squeeze.is_active, squeeze.current_state)
            },
            secondary: secondary.is_active && secondary.current_state,
            thumb_click: thumb_click.is_active && thumb_click.current_state,
            menu: (menu.is_active && menu.current_state) || (back.is_active && back.current_state),
            stick: if stick.is_active
                && stick.current_state.x.is_finite()
                && stick.current_state.y.is_finite()
            {
                [
                    stick.current_state.x.clamp(-1., 1.),
                    stick.current_state.y.clamp(-1., 1.),
                ]
            } else {
                [0.; 2]
            },
        })
    }

    fn cancel_hand(&mut self, hand: usize, desktop: &mut Desktop, reason: &str) {
        self.cancelled[hand] |= desktop.has_pointer_capture(SOURCES[hand]);
        desktop.cancel_pointer(SOURCES[hand], reason);
        self.pressed[hand] = false;
        self.menu_pressed[hand] = false;
        self.require_menu_release[hand] = true;
        // A held trigger after tracking/focus loss must not become a new press.
        self.gates[hand].cancel();
    }

    pub fn cancel_for_recenter(&mut self, desktop: &mut Desktop) {
        for hand in 0..2 {
            self.cancel_hand(hand, desktop, "desktop recentered");
        }
        self.last_hover = [None, None];
    }

    pub fn update(
        &mut self,
        session: &xr::Session<xr::Vulkan>,
        base: &xr::Space,
        time: xr::Time,
        desktop: &mut Desktop,
    ) -> Result<()> {
        self.controllers = [HandState::default(); 2];
        self.routes = ["none"; 2];
        self.route_ok = [false; 2];
        self.cancelled = [false; 2];
        self.back_routed = [false; 2];
        self.scroll_delta = [[0.; 2]; 2];
        self.scroll_ok = [false; 2];
        let result = self.update_routes(session, base, time, desktop);
        self.observe(time, desktop);
        result
    }

    // Record actual action samples and routing outcomes. Analog changes and a
    // held nonzero stick are limited to 10 Hz; sampled button edges and actual
    // press/release/cancel routes are never discarded by the throttle.
    fn observe(&mut self, time: xr::Time, desktop: &Desktop) {
        let now = Instant::now();
        for hand in 0..2 {
            let state = self.controllers[hand];
            let buttons = [
                state.select,
                self.gates[hand].latched[0],
                self.gates[hand].latched[1],
                state.menu,
                state.secondary,
                state.thumb_click,
            ];
            let observation = &mut self.observations[hand];
            let edge = observation.buttons != buttons;
            observation.buttons = buttons;
            let changed = observation.state.is_none_or(|last| {
                last.controls_active != state.controls_active
                    || last.grip.is_some() != state.grip.is_some()
                    || last.aim.is_some() != state.aim.is_some()
                    || (last.trigger - state.trigger).abs() >= 0.02
                    || (last.squeeze - state.squeeze).abs() >= 0.02
                    || last
                        .stick
                        .iter()
                        .zip(state.stick)
                        .any(|(a, b)| (a - b).abs() >= 0.02)
            });
            let due = observation
                .logged
                .is_none_or(|last| now.duration_since(last).as_millis() >= 100);
            let route_edge = ["press", "release"].contains(&self.routes[hand])
                || self.cancelled[hand]
                || self.back_routed[hand];
            let active_stick = state.stick.iter().any(|v| v.abs() > 0.18);
            if edge || route_edge || (due && (changed || active_stick)) {
                eprintln!(
                    "XR_CONTROLLER_INPUT={}",
                    serde_json::json!({
                        "hand": if hand == 0 { "left" } else { "right" },
                        "source": SOURCES[hand], "focused": desktop.focused,
                        "predicted_display_time_ns": time.as_nanos(),
                        "controls_active": state.controls_active,
                        "grip_valid": state.grip.is_some(), "aim_valid": state.aim.is_some(),
                        "select": state.select, "trigger": state.trigger, "squeeze": state.squeeze,
                        "secondary": state.secondary, "thumb_click": state.thumb_click,
                        "menu": state.menu, "stick": state.stick,
                        "route_phase": self.routes[hand], "route_completed": self.route_ok[hand],
                        "back_routed": self.back_routed[hand], "capture_cancelled": self.cancelled[hand],
                        "scroll_delta": self.scroll_delta[hand], "scroll_completed": self.scroll_ok[hand],
                        "captured": desktop.has_pointer_capture(SOURCES[hand]),
                        "require_release": self.gates[hand].blocked
                    })
                );
                observation.state = Some(state);
                observation.logged = Some(now);
            }
        }
    }

    fn update_routes(
        &mut self,
        session: &xr::Session<xr::Vulkan>,
        base: &xr::Space,
        time: xr::Time,
        desktop: &mut Desktop,
    ) -> Result<()> {
        let now = Instant::now();
        let dt = now.duration_since(self.updated).as_secs_f32().min(0.1);
        self.updated = now;
        if !desktop.focused {
            for hand in 0..2 {
                self.cancel_hand(hand, desktop, "XR focus lost");
            }
            self.last_hover = [None, None];
            return Ok(());
        }
        if let Err(error) = session.sync_actions(&[(&self.set).into()]) {
            for hand in 0..2 {
                self.cancel_hand(hand, desktop, "controller action sync failed");
            }
            return Err(error.into());
        }

        let mut menu_cancelled = false;
        let mut rays = [None; 2];
        // Read BOTH subaction paths even during a capture. Ignored presses from
        // the other hand must not be replayed when the current owner releases.
        for hand in 0..2 {
            let state = match self.read_hand(hand, session, base, time) {
                Ok(state) => state,
                Err(error) => {
                    self.cancel_hand(hand, desktop, "controller state unavailable");
                    eprintln!("XR_INPUT_STATE={} {error:#}", SOURCES[hand]);
                    continue;
                }
            };
            self.controllers[hand] = state;
            let Some((origin, direction)) = state.aim_ray() else {
                self.cancel_hand(hand, desktop, "controller pose invalid");
                continue;
            };
            let menu = state.menu;
            if !menu {
                self.require_menu_release[hand] = false;
            }
            if menu && !self.menu_pressed[hand] && !self.require_menu_release[hand] {
                for other in 0..2 {
                    self.cancelled[other] |= desktop.has_pointer_capture(SOURCES[other]);
                }
                let result = desktop.controller_back();
                self.back_routed[hand] = result.is_ok();
                self.routes[hand] = "back";
                self.route_ok[hand] = result.is_ok();
                if let Err(error) = result {
                    eprintln!("XR_INPUT_BACK={} {error:#}", SOURCES[hand]);
                }
                menu_cancelled = true;
            }
            self.menu_pressed[hand] = menu;
            if !state.controls_active {
                self.cancel_hand(hand, desktop, "controller controls inactive");
                continue;
            }
            if self.gates[hand]
                .owner
                .is_some_and(|i| !state.channel_active[i])
            {
                self.cancel_hand(hand, desktop, "captured action inactive");
                continue;
            }
            let values = [
                if state.select { 1. } else { state.trigger },
                state.squeeze,
                if state.secondary { 1. } else { 0. },
                if state.thumb_click { 1. } else { 0. },
            ];
            let (channel, phase) = self.gates[hand].sample(std::array::from_fn(|i| {
                if state.channel_active[i] {
                    values[i]
                } else {
                    f32::NAN
                }
            }));
            self.pressed[hand] = self.gates[hand].latched[0];
            rays[hand] = Some(Ray {
                origin,
                direction,
                channel,
                phase,
            });
        }

        if menu_cancelled {
            // Back may arrive after the other hand was sampled. Discard every
            // pending ray in this frame and clear accepted press history, so a
            // cancelled drag cannot emit a synthetic release or resume held.
            self.pressed = [false; 2];
            for gate in &mut self.gates {
                gate.cancel();
            }
            return Ok(());
        }

        for hand in 0..2 {
            let Some(ray) = rays[hand] else {
                continue;
            };
            let phase = ray.phase;
            let stick = stick_curve(self.controllers[hand].stick);
            // Depth takes priority only for this hand's actual grab.
            let grabbing = desktop.grab_depth(SOURCES[hand], -stick[1] * 1.2 * dt);
            self.routes[hand] = phase;
            let had_capture = desktop.has_pointer_capture(SOURCES[hand]);
            let result = if ray.channel == 1 {
                desktop.ray_squeeze(ray.origin, ray.direction, phase, SOURCES[hand])
            } else {
                let button = match ray.channel {
                    2 => 273,
                    3 => 274,
                    _ => 272,
                };
                if button == 272 {
                    desktop.ray(ray.origin, ray.direction, phase, SOURCES[hand])
                } else {
                    desktop.ray_button(ray.origin, ray.direction, phase, SOURCES[hand], button)
                }
            };
            self.route_ok[hand] = result.is_ok();
            if let Err(error) = result {
                eprintln!("XR_INPUT_ROUTE={} {error:#}", SOURCES[hand]);
            }
            let hover = desktop.pointer_target(SOURCES[hand], ray.origin, ray.direction);
            let accepted = desktop.has_pointer_capture(SOURCES[hand]);
            let pulse = if phase == "press" && accepted {
                Some(if ray.channel == 1 {
                    (0.55, 22)
                } else if ray.channel == 2 {
                    (0.45, 18)
                } else {
                    (0.35, 12)
                })
            } else if phase == "release" && had_capture {
                Some((0.25, 10))
            } else if hover.is_some() && hover != self.last_hover[hand] {
                Some((0.12, 7))
            } else {
                None
            };
            if let Some((amplitude, milliseconds)) = pulse.filter(|_| std::env::var("XR_SHELL_INPUT_MODE").as_deref() != Ok("hands")) {
                let vibration = xr::HapticVibration::new()
                    .amplitude(amplitude)
                    .frequency(xr::FREQUENCY_UNSPECIFIED)
                    .duration(xr::Duration::from_nanos(milliseconds * 1_000_000));
                if let Err(error) =
                    self.haptic
                        .apply_feedback(session, self.hands[hand], &vibration)
                {
                    eprintln!(
                        "XR_HAPTIC_UNAVAILABLE source={} error={error:?}",
                        SOURCES[hand]
                    );
                }
            }
            self.last_hover[hand] = hover;
            let scroll = [stick[0] * 600. * dt, -stick[1] * 600. * dt];
            if !grabbing && scroll != [0., 0.] {
                self.scroll_delta[hand] = scroll;
                let result = desktop
                    .controller_page_at(
                        SOURCES[hand],
                        ray.origin,
                        ray.direction,
                        self.controllers[hand].stick,
                    )
                    .and_then(|handled| {
                        if handled {
                            Ok(())
                        } else {
                            desktop.controller_scroll_at(
                                SOURCES[hand],
                                ray.origin,
                                ray.direction,
                                scroll,
                            )
                        }
                    });
                self.scroll_ok[hand] = result.is_ok();
                if let Err(error) = result {
                    eprintln!("XR_INPUT_SCROLL={} {error:#}", SOURCES[hand]);
                }
            }
        }
        Ok(())
    }
}

/// Each hand owns one interaction at a time. Consume other button edges even
/// when refused, so releasing the owner never replays a held secondary press.
struct ButtonGate {
    latched: [bool; 4],
    blocked: [bool; 4],
    owner: Option<usize>,
}
impl Default for ButtonGate {
    fn default() -> Self {
        Self {
            latched: [false; 4],
            blocked: [true; 4],
            owner: None,
        }
    }
}
impl ButtonGate {
    fn cancel(&mut self) {
        self.latched = [false; 4];
        self.blocked = [true; 4];
        self.owner = None;
    }
    fn sample(&mut self, values: [f32; 4]) -> (usize, &'static str) {
        let previous = self.latched;
        for (i, value) in values.into_iter().enumerate() {
            if !value.is_finite() {
                self.latched[i] = false;
                self.blocked[i] = true;
                continue;
            }
            if self.blocked[i] {
                if value.is_finite() && value <= 0.35 {
                    self.blocked[i] = false;
                }
                self.latched[i] = false;
            } else {
                let press = [0.65, 0.60, 0.5, 0.5][i];
                if !value.is_finite() || value <= 0.35 {
                    self.latched[i] = false;
                } else if value >= press {
                    self.latched[i] = true;
                }
            }
        }
        if let Some(i) = self.owner {
            if !self.latched[i] {
                self.owner = None;
                return (i, "release");
            }
            return (i, "drag");
        }
        for i in [1, 0, 2, 3] {
            if self.latched[i] && !previous[i] {
                self.owner = Some(i);
                return (i, "press");
            }
        }
        (0, "hover")
    }
}
fn stick_curve(v: [f32; 2]) -> [f32; 2] {
    if !v.iter().all(|v| v.is_finite()) {
        return [0.; 2];
    }
    let radius = v[0].hypot(v[1]);
    if radius <= 0.18 {
        return [0.; 2];
    }
    let n = (radius.min(1.) - 0.18) / 0.82;
    let gain = (0.35 * n + 0.65 * n * n) / radius;
    v.map(|x| x * gain)
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    #[test]
    fn chatter_squeeze_priority_and_held_buttons_never_replay() {
        let mut gate = ButtonGate::default();
        assert_eq!(gate.sample([1., 0., 0., 0.]).1, "hover"); // startup held
        gate.sample([0.; 4]);
        for v in [0.45, 0.55, 0.64, 0.50] {
            assert_eq!(gate.sample([v, 0., 0., 0.]).1, "hover");
        }
        assert_eq!(gate.sample([0.65, 0., 0., 0.]), (0, "press"));
        for v in [0.64, 0.50, 0.36] {
            assert_eq!(gate.sample([v, 0., 0., 0.]), (0, "drag"));
        }
        assert_eq!(gate.sample([0.35, 0., 1., 0.]), (0, "release"));
        assert_eq!(gate.sample([0., 0., 1., 0.]).1, "hover"); // refused secondary not replayed
        gate.sample([0.; 4]);
        assert_eq!(gate.sample([0.8, 0.6, 0., 0.]), (1, "press"));
        assert_eq!(gate.sample([0.8, 0.35, 0., 0.]), (1, "release"));
        assert_eq!(gate.sample([0.8, 0., 0., 0.]).1, "hover");
        gate.cancel();
        assert_eq!(gate.sample([0.5, 0., 1., 1.]).1, "hover");
        gate.sample([0.; 4]);
        assert_eq!(gate.sample([0., 0., 0., 1.]), (3, "press"));
        gate.cancel();
        assert_eq!(gate.sample([0., 0., 0., 1.]).1, "hover");
    }
    #[test]
    fn radial_stick_deadzone_finite_and_precision() {
        assert_eq!(stick_curve([0.1, 0.1]), [0.; 2]);
        assert_eq!(stick_curve([f32::NAN, 0.]), [0.; 2]);
        let diagonal = stick_curve([1., 1.]);
        assert!((diagonal[0].hypot(diagonal[1]) - 1.).abs() < 1e-6);
        assert!(stick_curve([0.5, 0.])[0] < 0.4);
        assert_eq!(stick_curve([1., 0.]), [1., 0.]);
    }
}
