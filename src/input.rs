// SPDX-License-Identifier: GPL-3.0-or-later
use crate::desktop::Desktop;
use anyhow::{Context, Result};
use openxr as xr;

const SOURCES: [&str; 2] = ["controller_ray:left", "controller_ray:right"];

#[derive(Clone, Copy)]
struct Ray {
    origin: [f32; 3],
    direction: [f32; 3],
    pressed: bool,
}

pub struct Input {
    set: xr::ActionSet,
    aim: xr::Action<xr::Posef>,
    select: xr::Action<bool>,
    trigger: xr::Action<f32>,
    spaces: [xr::Space; 2],
    hands: [xr::Path; 2],
    pressed: [bool; 2],
    require_release: [bool; 2],
    last_hand: Option<usize>,
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
        ] {
            eprintln!("XR_INIT_STAGE=input-profile STATUS=begin PROFILE={profile}");
            let mut bindings = Vec::new();
            for hand in ["left", "right"] {
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
                if float {
                    bindings.push(xr::Binding::new(&trigger, path));
                } else {
                    bindings.push(xr::Binding::new(&select, path));
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
        Ok(Self {
            set,
            aim,
            select,
            trigger,
            spaces,
            hands,
            pressed: [false; 2],
            require_release: [false; 2],
            last_hand: None,
        })
    }

    fn cancel_hand(&mut self, hand: usize, desktop: &mut Desktop, reason: &str) {
        desktop.cancel_pointer(SOURCES[hand], reason);
        self.pressed[hand] = false;
        // A held trigger after tracking/focus loss must not become a new press.
        self.require_release[hand] = true;
    }

    pub fn update(
        &mut self,
        session: &xr::Session<xr::Vulkan>,
        base: &xr::Space,
        time: xr::Time,
        desktop: &mut Desktop,
    ) -> Result<()> {
        if !desktop.focused {
            for hand in 0..2 {
                self.cancel_hand(hand, desktop, "XR focus lost");
            }
            self.last_hand = None;
            return Ok(());
        }
        if let Err(error) = session.sync_actions(&[(&self.set).into()]) {
            for hand in 0..2 {
                self.cancel_hand(hand, desktop, "controller action sync failed");
            }
            return Err(error.into());
        }

        let previous = self.pressed;
        let mut rays = [None; 2];
        let mut new_press = [false; 2];
        // Read BOTH subaction paths even during a capture. Ignored presses from
        // the other hand must not be replayed when the current owner releases.
        for hand in 0..2 {
            if !self.aim.is_active(session, self.hands[hand])? {
                self.cancel_hand(hand, desktop, "controller aim inactive");
                continue;
            }
            let location = self.spaces[hand].locate(base, time)?;
            if !location.location_flags.contains(
                xr::SpaceLocationFlags::ORIENTATION_VALID | xr::SpaceLocationFlags::POSITION_VALID,
            ) {
                self.cancel_hand(hand, desktop, "controller pose invalid");
                continue;
            }
            let select = self.select.state(session, self.hands[hand])?;
            let trigger = self.trigger.state(session, self.hands[hand])?;
            let pressed = (select.is_active && select.current_state)
                || (trigger.is_active && trigger.current_state > 0.5);
            self.pressed[hand] = pressed;
            if self.require_release[hand] {
                if pressed {
                    continue;
                }
                self.require_release[hand] = false;
            }
            new_press[hand] = pressed && !previous[hand];
            let q = location.pose.orientation;
            let p = location.pose.position;
            rays[hand] = Some(Ray {
                origin: [p.x, p.y, p.z],
                direction: [
                    -2.0 * (q.x * q.z + q.w * q.y),
                    -2.0 * (q.y * q.z - q.w * q.x),
                    -1.0 + 2.0 * (q.x * q.x + q.y * q.y),
                ],
                pressed,
            });
        }

        // Desktop remains the capture authority. Its owner always receives
        // drag/release before another hand may issue a new interaction.
        let owner = (0..2).find(|&hand| desktop.has_pointer_capture(SOURCES[hand]));
        let preferred = self.last_hand.unwrap_or(1);
        let order = [preferred, 1 - preferred];
        let hand = owner
            .or_else(|| order.into_iter().find(|&hand| new_press[hand]))
            .or_else(|| {
                order
                    .into_iter()
                    .find(|&hand| rays[hand].is_some_and(|ray| !ray.pressed))
            });
        let Some(hand) = hand else {
            return Ok(());
        };
        let Some(ray) = rays[hand] else {
            return Ok(());
        };
        let phase = if new_press[hand] {
            "press"
        } else if !ray.pressed && previous[hand] {
            "release"
        } else if ray.pressed {
            "drag"
        } else {
            "hover"
        };
        self.last_hand = Some(hand);
        if let Err(error) = desktop.ray(ray.origin, ray.direction, phase, SOURCES[hand]) {
            eprintln!("XR_INPUT_ROUTE={} {error:#}", SOURCES[hand]);
        }
        Ok(())
    }
}
