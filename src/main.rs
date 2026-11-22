// SPDX-License-Identifier: GPL-3.0-or-later
mod art;
mod controller_model;
mod controls;
mod desktop;
mod environment;
mod gpu;
mod gpu_timing;
mod hand_model;
mod hand_tracking;
mod input;
mod raster;
mod recenter;
mod renderer;
mod spatial;
mod taskbar_follow;
mod timeline;
mod ui_anchor;

use anyhow::{Context, Result, bail, ensure};
use environment::Environment;
use gpu::Gpu;
use openxr as xr;
use renderer::Renderer;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    env, fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const VIEW_TYPE: xr::ViewConfigurationType = xr::ViewConfigurationType::PRIMARY_STEREO;

struct Options {
    environment: PathBuf,
    seconds: u64,
    capture_dir: Option<PathBuf>,
    validation: bool,
    scale: f32,
    components: bool,
    debug_input: bool,
    socket: Option<PathBuf>,
    ui_anchor: ui_anchor::Mode,
    focus_quad: bool,
    passthrough: bool,
    hand_reference_capture: Option<PathBuf>,
}
impl Options {
    fn parse() -> Result<Option<Self>> {
        let mut o = Self {
            environment: "/usr/share/xr/environments/qingkong/environment.json".into(),
            seconds: 0,
            passthrough: match env::var("XR_ENVIRONMENT_MODE").as_deref() {
                Ok("passthrough") => true,
                Err(env::VarError::NotPresent) | Ok("opaque") => false,
                _ => bail!("XR_ENVIRONMENT_MODE must be opaque or passthrough"),
            },
            capture_dir: None,
            validation: false,
            scale: 1.0,
            components: false,
            debug_input: false,
            socket: None,
            ui_anchor: ui_anchor::Mode::Stage,
            hand_reference_capture: None,
            focus_quad: env::var("XR_FOCUS_QUAD").is_ok_and(|v| v == "auto"),
        };
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--environment" => {
                    o.environment = args.next().context("--environment requires a path")?.into()
                }
                "--seconds" => {
                    o.seconds = args
                        .next()
                        .context("--seconds requires an integer")?
                        .parse()?
                }
                "--capture-dir" => {
                    o.capture_dir = Some(
                        args.next()
                            .context("--capture-dir requires a directory")?
                            .into(),
                    )
                }
                "--hand-reference-capture" => {
                    o.hand_reference_capture = Some(
                        args.next()
                            .context("--hand-reference-capture requires a recorded file")?
                            .into(),
                    )
                }
                "--focus-quad=auto" => o.focus_quad = true,
                "--focus-quad=off" => o.focus_quad = false,
                "--components" => o.components = true,
                "--debug-input" => {
                    o.components = true;
                    o.debug_input = true;
                }
                "--component-socket" => {
                    o.components = true;
                    o.socket = Some(
                        args.next()
                            .context("--component-socket requires path")?
                            .into(),
                    );
                }
                "--validation" => o.validation = true,
                "--ui-anchor" => {
                    o.ui_anchor = ui_anchor::Mode::parse(
                        &args.next().context("--ui-anchor requires stage or head")?,
                    )?
                }
                "--view-scale" => {
                    o.scale = args
                        .next()
                        .context("--view-scale requires a number")?
                        .parse()?
                }
                "--help" | "-h" => {
                    println!(
                        "xr-shell [--environment FILE] [--seconds N] [--capture-dir DIR] [--validation] [--view-scale 0.1..1] [--ui-anchor=stage|head] [--focus-quad=auto|off]\n\
                              --seconds 0 runs until SIGINT/SIGTERM or runtime session shutdown.\n\
                              Write an ASCII label to DIR/capture.request to save that frame's two eyes and metadata."
                    );
                    return Ok(None);
                }
                _ if arg.starts_with("--ui-anchor=") => {
                    o.ui_anchor = ui_anchor::Mode::parse(&arg[12..])?
                }
                _ => bail!("unknown argument: {arg}"),
            }
        }
        ensure!(
            o.scale.is_finite() && (0.1..=1.0).contains(&o.scale),
            "view scale must be in 0.1..1"
        );
        ensure!(
            o.hand_reference_capture.is_none()
                || (!o.components && o.capture_dir.is_some() && (1..=30).contains(&o.seconds)),
            "recorded hand capture requires no components, a capture directory and 1..30 seconds"
        );
        Ok(Some(o))
    }
    fn summary_dir(&self) -> PathBuf {
        self.capture_dir.clone().unwrap_or_else(|| {
            env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into()))
                        .join(".local/state")
                })
                .join("xr-shell")
        })
    }
}

#[derive(Default, Serialize)]
struct Cleanup {
    exit_requested: bool,
    session_begun: bool,
    session_ended: bool,
    resources_destroyed: bool,
}
#[derive(Clone, Copy, Serialize)]
struct ViewFlags {
    raw_bits: u64,
    orientation_valid: bool,
    position_valid: bool,
    orientation_tracked: bool,
    position_tracked: bool,
}
impl From<xr::ViewStateFlags> for ViewFlags {
    fn from(flags: xr::ViewStateFlags) -> Self {
        Self {
            raw_bits: flags.into_raw(),
            orientation_valid: flags.contains(xr::ViewStateFlags::ORIENTATION_VALID),
            position_valid: flags.contains(xr::ViewStateFlags::POSITION_VALID),
            orientation_tracked: flags.contains(xr::ViewStateFlags::ORIENTATION_TRACKED),
            position_tracked: flags.contains(xr::ViewStateFlags::POSITION_TRACKED),
        }
    }
}
#[derive(Default, Serialize)]
struct ViewStateObservations {
    located_frames: u64,
    pose_valid_frames: u64,
    pose_invalid_frames: u64,
    orientation_valid_frames: u64,
    orientation_invalid_frames: u64,
    position_valid_frames: u64,
    position_invalid_frames: u64,
    orientation_tracked_frames: u64,
    orientation_untracked_frames: u64,
    position_tracked_frames: u64,
    position_untracked_frames: u64,
    finite_position_samples: u64,
    position_non_finite_frames: u64,
    first_valid_mean_eye_position_m: Option<[f32; 3]>,
    last_valid_mean_eye_position_m: Option<[f32; 3]>,
    first_valid_position_time_ns: Option<i64>,
    last_valid_position_time_ns: Option<i64>,
    last_flags: Option<ViewFlags>,
}
impl ViewStateObservations {
    fn observe(&mut self, flags: xr::ViewStateFlags, views: &[xr::View], time: xr::Time) {
        let flags = ViewFlags::from(flags);
        self.located_frames += 1;
        let valid = flags.orientation_valid && flags.position_valid;
        self.pose_valid_frames += u64::from(valid);
        self.pose_invalid_frames += u64::from(!valid);
        self.orientation_valid_frames += u64::from(flags.orientation_valid);
        self.orientation_invalid_frames += u64::from(!flags.orientation_valid);
        self.position_valid_frames += u64::from(flags.position_valid);
        self.position_invalid_frames += u64::from(!flags.position_valid);
        self.orientation_tracked_frames += u64::from(flags.orientation_tracked);
        self.orientation_untracked_frames += u64::from(!flags.orientation_tracked);
        self.position_tracked_frames += u64::from(flags.position_tracked);
        self.position_untracked_frames += u64::from(!flags.position_tracked);
        self.last_flags = Some(flags);
        // Record returned positions only when the runtime marks them valid.
        // This observation never changes rendering, input or tracking gates.
        if flags.position_valid && views.len() == 2 {
            let a = views[0].pose.position;
            let b = views[1].pose.position;
            let mean = [(a.x + b.x) / 2., (a.y + b.y) / 2., (a.z + b.z) / 2.];
            if [a.x, a.y, a.z, b.x, b.y, b.z].iter().all(|v| v.is_finite())
                && mean.iter().all(|v| v.is_finite())
            {
                self.finite_position_samples += 1;
                self.first_valid_mean_eye_position_m.get_or_insert(mean);
                self.first_valid_position_time_ns
                    .get_or_insert(time.as_nanos());
                self.last_valid_mean_eye_position_m = Some(mean);
                self.last_valid_position_time_ns = Some(time.as_nanos());
            } else {
                self.position_non_finite_frames += 1;
            }
        }
    }
}
#[derive(Default, Serialize)]
struct Summary {
    schema: u32,
    application: &'static str,
    version: &'static str,
    runtime: Option<String>,
    gpu: Option<String>,
    environment: Option<String>,
    passthrough: Option<serde_json::Value>,
    runtime_alpha_supported: bool,
    runtime_recenter_count: u64,
    home_recenter_count: u64,
    validation_enabled: bool,
    validation_errors: u64,
    validation_warnings: u64,
    frames: u64,
    waited_frames: u64,
    eye_submissions: [u64; 2],
    view_state: ViewStateObservations,
    render_view_state: ViewStateObservations,
    captures: Vec<String>,
    session_states: Vec<String>,
    elapsed_seconds: f64,
    reference_space: Option<String>,
    initialization_stage: Option<&'static str>,
    ui_anchor: ui_anchor::UiAnchor,
    cleanup: Cleanup,
    outcome: String,
    error: Option<String>,
    desktop: Value,
    // The exit summary keeps stage statistics only. Full frames are written on
    // request; the session wrapper copies this file twice more.
    #[serde(serialize_with = "timeline::serialize_summary")]
    timeline: timeline::Timeline,
    focus_quad: Value,
    native_quad_frames: u64,
    layer_limits: [u32; 3],
}

fn main() {
    if let Err(e) = entry() {
        eprintln!("XR_SHELL_ERROR={e:#}");
        std::process::exit(1);
    }
}
fn entry() -> Result<()> {
    let Some(options) = Options::parse()? else {
        return Ok(());
    };
    let summary_dir = options.summary_dir();
    fs::create_dir_all(&summary_dir)?;
    let mut summary = Summary {
        schema: 1,
        application: "xr-shell",
        version: env!("CARGO_PKG_VERSION"),
        validation_enabled: options.validation,
        ui_anchor: ui_anchor::UiAnchor::new(options.ui_anchor),
        outcome: "initializing".into(),
        ..Default::default()
    };
    let result = run(&options, &mut summary);
    summary.outcome = if result.is_ok() { "success" } else { "error" }.into();
    summary.error = result.as_ref().err().map(|e| format!("{e:#}"));
    atomic_json(
        &summary_dir.join("summary.json"),
        &serde_json::to_value(&summary)?,
    )?;
    println!(
        "XR_SHELL_SUMMARY={}",
        summary_dir.join("summary.json").display()
    );
    result
}

fn run(options: &Options, summary: &mut Summary) -> Result<()> {
    let environment = Environment::load(&options.environment)?;
    println!("XR_ENVIRONMENT={}", environment.id);
    summary.environment = Some(environment.id.clone());
    let quit = Arc::new(AtomicBool::new(false));
    let signal_quit = quit.clone();
    ctrlc::set_handler(move || {
        signal_quit.store(true, Ordering::Relaxed);
    })?;
    let foreground = env::var_os("XR_SHELL_FOREGROUND_DIR")
        .filter(|_| options.components)
        .map(PathBuf::from);
    let mut desktop = None;
    let mut desktop_origin = None;
    let started = Instant::now();
    loop {
        let yielded = render_session(
            options,
            summary,
            &environment,
            &quit,
            &mut desktop,
            foreground.as_deref(),
            &mut desktop_origin,
        )?;
        if !yielded || quit.load(Ordering::Relaxed) {
            break;
        }
        let directory = foreground
            .as_ref()
            .context("missing foreground directory")?;
        let released = directory.join("released");
        fs::write(&released, b"")?;
        println!("XR_FOREGROUND_RELEASED=1");
        while directory.join("yield").exists() && !quit.load(Ordering::Relaxed) {
            if let Some(d) = desktop.as_mut() {
                d.tick()?;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        fs::remove_file(released)?;
        if quit.load(Ordering::Relaxed) {
            break;
        }
        println!("XR_FOREGROUND_RESUMING=1");
    }
    if let Some(d) = &desktop {
        summary.desktop = d.snapshot();
    }
    summary.elapsed_seconds = started.elapsed().as_secs_f64();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_session(
    options: &Options,
    summary: &mut Summary,
    environment: &Environment,
    quit: &AtomicBool,
    desktop: &mut Option<desktop::Desktop>,
    foreground: Option<&Path>,
    desktop_origin: &mut Option<xr::Posef>,
) -> Result<bool> {
    summary.cleanup = Cleanup::default();
    let entry = unsafe { xr::Entry::load(&())? };
    let available = entry.enumerate_extensions()?;
    ensure!(
        available.khr_vulkan_enable2,
        "runtime requires XR_KHR_vulkan_enable2"
    );
    let mut extensions = xr::ExtensionSet::default();
    extensions.khr_vulkan_enable2 = true;
    extensions.ext_hand_tracking = available.ext_hand_tracking;
    extensions.ext_hand_interaction = available.ext_hand_interaction;
    extensions.khr_convert_timespec_time = available.khr_convert_timespec_time;
    let instance = entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "HyperSpace",
            application_version: 1,
            engine_name: "Vultra",
            engine_version: 1,
            api_version: xr::Version::new(1, 0, 0),
        },
        &extensions,
        &[],
        &(),
    )?;
    let properties = instance.properties()?;
    summary.runtime = Some(format!(
        "{} {}",
        properties.runtime_name, properties.runtime_version
    ));
    println!("XR_RUNTIME={}", summary.runtime.as_ref().unwrap());
    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let modes = instance.enumerate_environment_blend_modes(system, VIEW_TYPE)?;
    summary.runtime_alpha_supported = modes.contains(&xr::EnvironmentBlendMode::ALPHA_BLEND);
    ensure!(
        modes.contains(&if options.passthrough {
            xr::EnvironmentBlendMode::ALPHA_BLEND
        } else {
            xr::EnvironmentBlendMode::OPAQUE
        }),
        "runtime does not support the requested environment mode"
    );
    let graphics = instance.system_properties(system)?.graphics_properties;
    summary.layer_limits = [
        graphics.max_layer_count,
        graphics.max_swapchain_image_width,
        graphics.max_swapchain_image_height,
    ];
    let config = instance
        .enumerate_view_configuration_views(system, VIEW_TYPE)
        .context("xrEnumerateViewConfigurationViews(PRIMARY_STEREO)")?;
    ensure!(config.len() == 2, "PRIMARY_STEREO must expose two views");
    for (eye, view) in config.iter().enumerate() {
        eprintln!(
            "XR_VIEW_CONFIG EYE={eye} RECOMMENDED={}x{} MAX={}x{} RECOMMENDED_SAMPLES={} MAX_SAMPLES={}",
            view.recommended_image_rect_width,
            view.recommended_image_rect_height,
            view.max_image_rect_width,
            view.max_image_rect_height,
            view.recommended_swapchain_sample_count,
            view.max_swapchain_sample_count
        );
    }
    let gpu = Gpu::new(&instance, system, options.validation)?;
    summary.gpu = Some(gpu.name.clone());
    let validation_stats = gpu.instance.validation.clone();
    let (session, mut waiter, mut stream) = gpu.session(&instance, system)?;
    summary.initialization_stage = Some("enumerate-reference-spaces");
    eprintln!("XR_INIT_STAGE=enumerate-reference-spaces STATUS=begin");
    let spaces = session
        .enumerate_reference_spaces()
        .context("xrEnumerateReferenceSpaces")?;
    eprintln!("XR_INIT_STAGE=enumerate-reference-spaces STATUS=complete SPACES={spaces:?}");
    let space_type = if spaces.contains(&xr::ReferenceSpaceType::STAGE) {
        xr::ReferenceSpaceType::STAGE
    } else {
        xr::ReferenceSpaceType::LOCAL
    };
    summary.reference_space = Some(format!("{space_type:?}"));
    summary.initialization_stage = Some("create-reference-space");
    eprintln!("XR_INIT_STAGE=create-reference-space STATUS=begin TYPE={space_type:?}");
    let space = session
        .create_reference_space(space_type, xr::Posef::IDENTITY)
        .with_context(|| format!("xrCreateReferenceSpace({space_type:?}, identity)"))?;
    eprintln!("XR_INIT_STAGE=create-reference-space STATUS=complete TYPE={space_type:?}");
    let mut input = if options.components {
        summary.initialization_stage = Some("input");
        eprintln!("XR_INIT_STAGE=input STATUS=begin");
        let input = input::Input::new(&instance, &session)
            .context("initialize OpenXR controller actions")?;
        eprintln!("XR_INIT_STAGE=input STATUS=complete");
        Some(input)
    } else {
        None
    };
    summary.initialization_stage = Some("renderer");
    eprintln!("XR_INIT_STAGE=renderer STATUS=begin");
    let mut renderer = Renderer::new(gpu.clone(), &session, &config, environment, options.scale)
        .context("initialize Vulkan renderer and OpenXR eye swapchains")?;
    eprintln!("XR_INIT_STAGE=renderer STATUS=complete");
    renderer.set_ui_anchor(summary.ui_anchor.offset());
    summary.initialization_stage = Some("desktop");
    if options.components && desktop.is_none() {
        let path = options.socket.clone().unwrap_or_else(|| {
            PathBuf::from(env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into()))
                .join("xr-desktop/shell.sock")
        });
        let mut new_desktop = desktop::Desktop::new(&path, options.debug_input)
            .context("create desktop component socket")?;
        new_desktop.set_ui_anchor(summary.ui_anchor.offset());
        *desktop = Some(new_desktop);
    }
    if let Some(d) = desktop.as_mut() {
        d.refresh_render_buffers();
    }
    summary.initialization_stage = Some("complete");
    eprintln!("XR_INIT_STAGE=complete STATUS=complete");
    let result = frame_loop(
        options,
        summary,
        &instance,
        &session,
        &mut waiter,
        &mut stream,
        &space,
        &mut renderer,
        quit,
        desktop,
        &mut input,
        foreground,
        desktop_origin,
    );
    if let Some(d) = desktop.as_mut() {
        d.set_xr_focus(false);
    }
    let idle = unsafe { gpu.device.device_wait_idle() }.context("wait for final GPU work");
    // Framebuffers/views and XR swapchains are destroyed before session/device.
    drop(input);
    drop(renderer);
    drop(space);
    drop(stream);
    drop(waiter);
    drop(session);
    drop(gpu);
    drop(instance);
    summary.validation_errors = validation_stats.errors.load(Ordering::Relaxed);
    summary.validation_warnings = validation_stats.warnings.load(Ordering::Relaxed);
    summary.cleanup.resources_destroyed = true;
    println!(
        "XR_SHELL_CLEANUP=1 FRAMES={} EYES={:?} VALIDATION_ERRORS={}",
        summary.frames, summary.eye_submissions, summary.validation_errors
    );
    let yielded = result?;
    idle?;
    ensure!(
        summary.validation_errors == 0,
        "Vulkan validation reported {} errors",
        summary.validation_errors
    );
    Ok(yielded)
}

#[allow(clippy::too_many_arguments)]
fn frame_loop(
    options: &Options,
    summary: &mut Summary,
    instance: &xr::Instance,
    session: &xr::Session<xr::Vulkan>,
    waiter: &mut xr::FrameWaiter,
    stream: &mut xr::FrameStream<xr::Vulkan>,
    root_space: &xr::Space,
    renderer: &mut Renderer,
    quit: &AtomicBool,
    desktop: &mut Option<desktop::Desktop>,
    input: &mut Option<input::Input>,
    foreground: Option<&Path>,
    desktop_origin: &mut Option<xr::Posef>,
) -> Result<bool> {
    let mut controls = controls::Controls::new();
    renderer.runtime_alpha_supported = summary.runtime_alpha_supported;
    let mut hands = hand_tracking::Tracking::new(instance, session);
    let reference = options
        .hand_reference_capture
        .as_ref()
        .map(|p| hand_tracking::Reference::load(p))
        .transpose()?;
    renderer.hand_reference_capture = reference.is_some();
    if reference.is_some() {
        eprintln!(
            "XR_HAND_REFERENCE source=recorded capture_only=true action_routes=none projection_layers=0"
        );
    }
    let mut pending_recenter = None;
    let space_kind = if summary.reference_space.as_deref() == Some("STAGE") {
        xr::ReferenceSpaceType::STAGE
    } else {
        xr::ReferenceSpaceType::LOCAL
    };
    let mut recentered_space = desktop_origin
        .map(|pose| session.create_reference_space(space_kind, pose))
        .transpose()?;
    let mut home_recenter_pending = false;
    let mut events = xr::EventDataBuffer::new();
    let yield_request = foreground.map(|dir| dir.join("yield"));
    let mut yielding = false;
    let mut running = false;
    let mut started = None::<Instant>;
    loop {
        let loop_start = timeline::now();
        while let Some(event) = instance.poll_event(&mut events)? {
            match event {
                xr::Event::ReferenceSpaceChangePending(e) => {
                    let selected = if summary.reference_space.as_deref() == Some("STAGE") {
                        xr::ReferenceSpaceType::STAGE
                    } else {
                        xr::ReferenceSpaceType::LOCAL
                    };
                    if e.reference_space_type() == selected {
                        pending_recenter = Some(e.change_time().as_nanos());
                    }
                }
                xr::Event::SessionStateChanged(e) => {
                    let state = e.state();
                    println!("XR_SESSION_STATE={state:?}");
                    summary.session_states.push(format!("{state:?}"));
                    if let Some(d) = desktop {
                        d.set_xr_focus(!yielding && state == xr::SessionState::FOCUSED);
                    }
                    match state {
                        xr::SessionState::READY => {
                            session.begin(VIEW_TYPE)?;
                            running = true;
                            summary.cleanup.session_begun = true;
                            started = Some(Instant::now());
                        }
                        xr::SessionState::STOPPING => {
                            session.end()?;
                            summary.cleanup.session_ended = true;
                            return Ok(yielding);
                        }
                        xr::SessionState::EXITING => return Ok(yielding),
                        xr::SessionState::LOSS_PENDING => bail!("OpenXR session loss pending"),
                        _ => {}
                    }
                }
                xr::Event::InstanceLossPending(_) => bail!("OpenXR instance loss pending"),
                xr::Event::EventsLost(e) => eprintln!("XR_EVENTS_LOST={}", e.lost_event_count()),
                _ => {}
            }
        }
        let timed_out = options.seconds != 0
            && started.is_some_and(|t| t.elapsed() >= Duration::from_secs(options.seconds));
        let stopping = quit.load(Ordering::Relaxed) || timed_out;
        if !summary.cleanup.exit_requested {
            yielding = !stopping
                && yield_request
                    .as_ref()
                    .is_some_and(|request| request.is_file());
        }
        if (stopping || yielding) && !summary.cleanup.exit_requested {
            if !running {
                return Ok(yielding);
            }
            if let Some(d) = desktop.as_mut() {
                d.set_xr_focus(false);
            }
            session.request_exit()?;
            summary.cleanup.exit_requested = true;
            println!("XR_EXIT_REQUESTED=1");
        }
        if !running {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let events_done = timeline::now();
        if summary.waited_frames % 30 == 0 {
            let request = options.summary_dir().join("timeline-request");
            if request.is_file() {
                fs::remove_file(request)?;
                atomic_json(
                    &options.summary_dir().join("timeline.json"),
                    &json!({"summary":summary.timeline.summary(),"samples":summary.timeline.frames}),
                )?;
            }
        }
        let mut times = [0i64; 9];
        times[0] = timeline::now();
        let cpu_start = timeline::thread_cpu();
        let core_start = unsafe { libc::sched_getcpu() };
        if let Some(d) = desktop {
            d.tick()?;
            // Collect peer updates now, then prepare pixels once after input.
            // Preparing here and again after a hover change forced a complete
            // replacement upload for the still-pending atlas batch.
            if !d.spatial_fast_path {
                if let Some(panels) = d.panels()? {
                    renderer.update_panels(&panels)?;
                }
            }
        }
        times[1] = timeline::now();
        controls.poll();
        home_recenter_pending |= controls.take_home_recenter();
        renderer.set_passthrough_visible(controls.passthrough(options.passthrough));
        let blend_mode = if renderer.runtime_alpha_active() {
            xr::EnvironmentBlendMode::ALPHA_BLEND
        } else {
            xr::EnvironmentBlendMode::OPAQUE
        };
        let state = waiter.wait()?;
        stream.begin()?;
        if pending_recenter.is_some_and(|time| state.predicted_display_time.as_nanos() >= time) {
            pending_recenter = None;
            recentered_space = None;
            *desktop_origin = None;
            summary.runtime_recenter_count += 1;
            summary.ui_anchor = ui_anchor::UiAnchor::new(options.ui_anchor);
            renderer.set_ui_anchor(summary.ui_anchor.offset());
            if let Some(d) = desktop.as_mut() {
                d.recenter_taskbar();
                d.set_ui_anchor(summary.ui_anchor.offset());
                if let Some(i) = input.as_mut() {
                    i.cancel_for_recenter(d);
                }
            }
        }

        if home_recenter_pending && state.should_render {
            let (flags, views) =
                session.locate_views(VIEW_TYPE, state.predicted_display_time, root_space)?;
            if let Some(origin) = recenter::pose(flags, &views) {
                recentered_space = Some(session.create_reference_space(space_kind, origin)?);
                *desktop_origin = Some(origin);
                home_recenter_pending = false;
                summary.home_recenter_count += 1;
                summary.ui_anchor = ui_anchor::UiAnchor::new(options.ui_anchor);
                renderer.set_ui_anchor(summary.ui_anchor.offset());
                if let Some(d) = desktop.as_mut() {
                    d.recenter_taskbar();
                    d.set_ui_anchor(summary.ui_anchor.offset());
                    if let Some(i) = input.as_mut() {
                        i.cancel_for_recenter(d);
                    }
                }
                eprintln!(
                    "XR_HOME_RECENTER count={} scope=shell_xz_yaw floor=preserved",
                    summary.home_recenter_count
                );
            }
        }
        let space = recentered_space.as_ref().unwrap_or(root_space);
        times[2] = timeline::now();
        let runtime_now_xr_ns = if instance.exts().khr_convert_timespec_time.is_some() {
            instance.now().ok().map(|t| t.as_nanos())
        } else {
            None
        };
        let clock_bracket = [times[2], timeline::now()];
        summary.waited_frames += 1;
        // Clear visibility even while the runtime suspends scene rendering.
        renderer.update_controllers(&[input::HandState::default(); 2]);
        renderer.clear_hands();
        renderer.update_ray_feedback([None; 2]);
        if yielding || !state.should_render {
            stream.end(state.predicted_display_time, blend_mode, &[])?;
            summary.timeline.skip(
                summary.waited_frames,
                [times[0], times[2], timeline::now()],
                "runtime_should_not_render",
                None,
            );
            continue;
        }
        let input_locate_start = timeline::now();
        let (flags, views) =
            session.locate_views(VIEW_TYPE, state.predicted_display_time, space)?;
        let input_view_locate_mono_ns = [input_locate_start, timeline::now()];
        summary
            .view_state
            .observe(flags, &views, state.predicted_display_time);
        if summary
            .ui_anchor
            .observe(flags, &views, state.predicted_display_time)
        {
            renderer.set_ui_anchor(summary.ui_anchor.offset());
            if let Some(d) = desktop.as_mut() {
                d.set_ui_anchor(summary.ui_anchor.offset());
            }
            eprintln!(
                "XR_UI_ANCHOR_LOCKED={}",
                serde_json::to_string(&summary.ui_anchor)?
            );
        }
        if !flags.contains(xr::ViewStateFlags::ORIENTATION_VALID) {
            if let Some(d) = desktop.as_mut() {
                d.invalidate_horizon_head();
            }
            stream.end(state.predicted_display_time, blend_mode, &[])?;
            summary.timeline.skip(
                summary.waited_frames,
                [times[0], times[2], timeline::now()],
                "head_orientation_invalid",
                Some(flags.into_raw()),
            );
            continue;
        }
        ensure!(views.len() == 2, "runtime changed stereo view count");
        times[3] = timeline::now();
        let head_position = [
            (views[0].pose.position.x + views[1].pose.position.x) / 2.,
            (views[0].pose.position.y + views[1].pose.position.y) / 2.,
            (views[0].pose.position.z + views[1].pose.position.z) / 2.,
        ];
        let input_head_pose = {
            let mut p = views[0].pose;
            p.position = xr::Vector3f {
                x: head_position[0],
                y: head_position[1],
                z: head_position[2],
            };
            timeline::pose(p)
        };
        let input_head_flags = flags.into_raw();
        let dt = state.predicted_display_period.as_nanos() as f32 / 1e9;
        if let Some(d) = desktop.as_mut() {
            if let Some(centre) = recenter::center_eye(flags, &views) {
                let q = centre.orientation;
                d.observe_horizon_head(head_position, [q.x, q.y, q.z, q.w]);
                d.update_head(head_position, [q.x, q.y, q.z, q.w], dt);
            } else {
                d.invalidate_horizon_head();
            }
        }
        if controls.take_source_change() {
            if let (Some(i), Some(d)) = (input.as_mut(), desktop.as_mut()) {
                i.cancel_for_source_change(d);
            }
        }
        let head_controls_done = timeline::now();
        let hand_joints = hands.sample(space, state.predicted_display_time);
        let hand_joint_count = hand_joints
            .each_ref()
            .map(|j| j.as_ref().map_or(0, |v| v.len()));
        let hands_located = timeline::now();
        let mut input_times = [timeline::now(); 5];
        let mut optical_hands = [false; 2];
        if let (Some(i), Some(d)) = (input.as_mut(), desktop.as_mut()) {
            if controls.source() == "none" {
                i.cancel_for_source_change(d);
            } else {
                i.update(
                    session,
                    space,
                    state.predicted_display_time,
                    d,
                    controls.source(),
                )?;
                renderer.update_controllers(i.controllers());
                optical_hands = i.optical_hands();
            }
        }
        renderer.optical_hands = optical_hands;
        renderer.update_hands(std::array::from_fn(|hand| {
            optical_hands[hand].then(|| hand_joints[hand]).flatten()
        }))?;
        if let Some(reference) = reference.as_ref() {
            let mut centre = views[0].pose;
            centre.position = xr::Vector3f {
                x: head_position[0],
                y: head_position[1],
                z: head_position[2],
            };
            renderer.update_hands(reference.at(centre))?;
        }
        input_times[1] = timeline::now();
        if let Some(d) = desktop.as_mut() {
            d.animate(dt);
            if let Some(panels) = d.panels()? {
                renderer.update_panels(&panels)?;
            }
            input_times[2] = timeline::now();
            renderer.update_panel_poses(&d.panel_poses(), head_position)?;
            input_times[3] = timeline::now();
            // Optical hands use measured grip/aim and the existing 6DoF routes.
            // Do not draw a physical controller body at an optical palm pose.
            renderer.controller_visuals = match controls.source() {
                "hands" | "controllers" | "runtime" => d.controller_visuals,
                _ => 0,
            };
            if let Some(i) = input.as_ref() {
                renderer.update_ray_feedback(std::array::from_fn(|hand| {
                    let (origin, direction) = i.controllers()[hand].aim_ray()?;
                    d.ray_feedback(
                        origin,
                        direction,
                        if hand == 0 {
                            "controller_ray:left"
                        } else {
                            "controller_ray:right"
                        },
                    )
                }));
            }
        }

        input_times[4] = timeline::now();
        if desktop.is_none() {
            input_times[2] = input_times[1];
            input_times[3] = input_times[1];
        }
        renderer.prepare_focus_quad(
            session,
            desktop.as_ref().and_then(|d| d.focus_quad_window()),
            &views,
            options.focus_quad,
            summary.layer_limits,
        );
        summary.focus_quad = renderer.quad_status();
        let atlas_update = renderer.pending_atlas_stats();
        let capture = capture_request(options.capture_dir.as_deref())?;
        times[4] = timeline::now();
        let mut indices = Vec::new();
        for target in renderer.targets_mut() {
            indices.push(target.swapchain.acquire_image()?);
            target.swapchain.wait_image(xr::Duration::INFINITE)?;
        }
        times[5] = timeline::now();
        // Keep input and scene preparation on their earlier observation, then
        // ask the runtime again after CPU work and swapchain waits. The target
        // remains xrWaitFrame's display time: shortening it here would render
        // for the wrong scanout, and adding local extrapolation would duplicate
        // runtime prediction. Render and projection metadata use this same pair.
        let render_clock_start = timeline::now();
        let render_runtime_now_xr_ns = if instance.exts().khr_convert_timespec_time.is_some() {
            instance.now().ok().map(|t| t.as_nanos())
        } else {
            None
        };
        let render_runtime_now_mono_bracket_ns = [render_clock_start, timeline::now()];
        let render_locate_start = timeline::now();
        let render_views = session.locate_views(VIEW_TYPE, state.predicted_display_time, space);
        let render_view_locate_mono_ns = [render_locate_start, timeline::now()];
        let render_views_valid = render_views.as_ref().is_ok_and(|(flags, views)| {
            flags.contains(xr::ViewStateFlags::ORIENTATION_VALID) && views.len() == 2
        });
        let focus_views_valid = render_views_valid
            && renderer.refresh_focus_quad_views(&render_views.as_ref().unwrap().1);
        if !focus_views_valid {
            // Acquisition preceded late location. Release every acquired target
            // before skipping or propagating a runtime error.
            let releases: Vec<_> = renderer
                .targets_mut()
                .into_iter()
                .map(|eye| eye.swapchain.release_image())
                .collect();
            for release in releases {
                release?;
            }
            let (flags, views) = render_views?;
            summary
                .render_view_state
                .observe(flags, &views, state.predicted_display_time);
            ensure!(views.len() == 2, "runtime changed stereo view count");
            if !render_views_valid {
                if let Some(d) = desktop.as_mut() {
                    d.invalidate_horizon_head();
                }
            }
            stream.end(state.predicted_display_time, blend_mode, &[])?;
            summary.timeline.skip(
                summary.waited_frames,
                [times[0], times[2], timeline::now()],
                if render_views_valid {
                    "late_head_behind_focus_quad"
                } else {
                    "late_head_orientation_invalid"
                },
                Some(flags.into_raw()),
            );
            continue;
        }
        let (flags, views) = render_views?;
        summary
            .render_view_state
            .observe(flags, &views, state.predicted_display_time);
        summary.focus_quad = renderer.quad_status();
        let result = renderer.enqueue_stereo(&indices, &views, capture.is_some());
        times[6] = timeline::now();
        let releases: Vec<_> = renderer
            .targets_mut()
            .into_iter()
            .map(|eye| eye.swapchain.release_image())
            .collect();
        result?;
        for release in releases {
            release?;
        }
        times[7] = timeline::now();
        let layers: Vec<_> = renderer
            .eyes
            .iter()
            .zip(&views)
            .map(|(eye, view)| {
                xr::CompositionLayerProjectionView::new()
                    .pose(view.pose)
                    .fov(view.fov)
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&eye.swapchain)
                            .image_array_index(0)
                            .image_rect(xr::Rect2Di {
                                offset: xr::Offset2Di { x: 0, y: 0 },
                                extent: xr::Extent2Di {
                                    width: eye.extent.width as i32,
                                    height: eye.extent.height as i32,
                                },
                            }),
                    )
            })
            .collect();
        let background = xr::CompositionLayerProjection::new()
            .space(space)
            .layer_flags(if renderer.runtime_alpha_active() {
                xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA
            } else {
                xr::CompositionLayerFlags::EMPTY
            })
            .views(&layers);
        if reference.is_some() {
            stream.end(state.predicted_display_time, blend_mode, &[])?;
        } else if let Some(focus) = &renderer.focus_quad {
            let target = renderer.quad.as_ref().unwrap();
            let quad = xr::CompositionLayerQuad::new()
                .space(space)
                .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
                .eye_visibility(xr::EyeVisibility::BOTH)
                .pose(focus.pose())
                .size(xr::Extent2Df {
                    width: focus.frame.size[0],
                    height: focus.frame.size[1],
                })
                .sub_image(
                    xr::SwapchainSubImage::new()
                        .swapchain(&target.swapchain)
                        .image_array_index(0)
                        .image_rect(xr::Rect2Di {
                            offset: xr::Offset2Di { x: 0, y: 0 },
                            extent: xr::Extent2Di {
                                width: target.extent.width as i32,
                                height: target.extent.height as i32,
                            },
                        }),
                );
            let front_views: Vec<_> = renderer
                .foreground
                .iter()
                .zip(&views)
                .enumerate()
                .map(|(index, (eye, view))| {
                    let (region, fov) = renderer.foreground_regions[index];
                    xr::CompositionLayerProjectionView::new()
                        .pose(view.pose)
                        .fov(fov)
                        .sub_image(
                            xr::SwapchainSubImage::new()
                                .swapchain(&eye.swapchain)
                                .image_array_index(0)
                                .image_rect(xr::Rect2Di {
                                    offset: xr::Offset2Di {
                                        x: region.offset.x,
                                        y: region.offset.y,
                                    },
                                    extent: xr::Extent2Di {
                                        width: region.extent.width as i32,
                                        height: region.extent.height as i32,
                                    },
                                }),
                        )
                })
                .collect();
            let foreground = xr::CompositionLayerProjection::new()
                .space(space)
                .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
                .views(&front_views);
            stream.end(
                state.predicted_display_time,
                blend_mode,
                &[&background, &quad, &foreground],
            )?;
            summary.native_quad_frames += 1;
        } else {
            stream.end(state.predicted_display_time, blend_mode, &[&background])?;
        }
        let end_frame_done = timeline::now();
        // Runtime submission does not require a CPU completion wait for work
        // queued on the bound Vulkan queue. Retire now, before another loop can
        // overwrite uniforms/atlas/hand buffers or reuse the command/query pool.
        // Errors above leave pending buffers owned by Renderer until its idle
        // cleanup in run(), rather than dropping them while the GPU uses them.
        let pixels = renderer.retire_stereo()?;
        summary.passthrough = renderer.passthrough_status();
        controls.report(summary.passthrough.clone());
        times[8] = timeline::now();
        summary.timeline.push(timeline::Frame {
            number: summary.frames + 1,
            mono_ns: times,
            loop_mono_ns: [loop_start, events_done, times[0]],
            end_frame_mono_ns: [times[7], end_frame_done, times[8]],
            post_frame_mono_ns: [times[8]; 2],
            post_frame_complete: false,
            wait_number: summary.waited_frames,
            thread_cpu_ns: [cpu_start, timeline::thread_cpu()],
            cpu_core: [core_start, unsafe { libc::sched_getcpu() }],
            action_sync_span: input.as_ref().and_then(|i| i.sync_span),
            controller_read_spans: input.as_ref().map_or([None; 2], |i| i.read_spans),
            controller_flags: std::array::from_fn(|h| {
                input.as_ref().map_or([0; 2], |i| {
                    [i.controllers()[h].grip_flags, i.controllers()[h].aim_flags]
                })
            }),
            hand_joint_count,
            predicted_display_period_ns: state.predicted_display_period.as_nanos(),
            pre_input_mono_ns: [times[3], head_controls_done, hands_located, input_times[0]],
            render_mono_ns: renderer.render_mono_ns,
            retire_mono_ns: renderer.retire_mono_ns,
            gpu: renderer.timing.sample.clone(),
            draw_spans: renderer.draw_spans.borrow().clone(),
            model_vertices: renderer.model_vertices(),
            panel_instances: renderer.panel_instances(),
            predicted_display_xr_ns: state.predicted_display_time.as_nanos(),
            runtime_now_xr_ns,
            runtime_now_mono_bracket_ns: clock_bracket,
            input_view_locate_mono_ns,
            render_view_locate_mono_ns,
            render_runtime_now_xr_ns,
            render_runtime_now_mono_bracket_ns,
            input_head_pose,
            input_head_flags,
            layers: if reference.is_some() {
                0
            } else if renderer.focus_quad.is_some() {
                3
            } else {
                1
            },
            capture: capture.is_some(),
            input_mono_ns: input_times,
            head_pose: {
                let mut p = views[0].pose;
                p.position = xr::Vector3f {
                    x: (views[0].pose.position.x + views[1].pose.position.x) * 0.5,
                    y: (views[0].pose.position.y + views[1].pose.position.y) * 0.5,
                    z: (views[0].pose.position.z + views[1].pose.position.z) * 0.5,
                };
                timeline::pose(p)
            },
            head_flags: flags.into_raw(),
            controller_grip: std::array::from_fn(|h| {
                input
                    .as_ref()
                    .and_then(|i| i.controllers()[h].grip)
                    .map(timeline::pose)
            }),
            controller_aim: std::array::from_fn(|h| {
                input
                    .as_ref()
                    .and_then(|i| i.controllers()[h].aim)
                    .map(timeline::pose)
            }),
            controller_draw_mask: renderer.controller_visuals,
            spatial_fast_path: desktop.as_ref().is_some_and(|d| d.spatial_fast_path),
            atlas_update,
        });
        summary.frames += 1;
        for n in &mut summary.eye_submissions {
            *n += 1;
        }
        if let Some(tag) = capture {
            write_capture(
                options.capture_dir.as_deref().unwrap(),
                &tag,
                summary.frames,
                state.predicted_display_time.as_nanos(),
                renderer,
                &views,
                flags,
                &summary.ui_anchor,
                &indices,
                pixels,
            )?;
            atomic_json(
                &options.summary_dir().join("timeline.json"),
                &json!({"summary":summary.timeline.summary(),"samples":summary.timeline.frames,"focus_quad":summary.focus_quad,"native_quad_frames":summary.native_quad_frames}),
            )?;
            summary.captures.push(tag.clone());
            println!("XR_CAPTURE={tag} FRAME={} VIEWS=2", summary.frames);
        }
        if summary.frames == 1 || summary.frames % 300 == 0 {
            println!(
                "XR_FRAME={} VIEWS=2 TIME={}",
                summary.frames,
                state.predicted_display_time.as_nanos()
            );
        }
        if let Some(frame) = summary.timeline.frames.back_mut() {
            frame.post_frame_mono_ns[1] = timeline::now();
            frame.post_frame_complete = true;
        }
    }
}

fn capture_request(dir: Option<&Path>) -> Result<Option<String>> {
    let Some(dir) = dir else {
        return Ok(None);
    };
    let file = dir.join("capture.request");
    let text = match fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let tag = text.trim();
    ensure!(
        !tag.is_empty()
            && tag.len() <= 64
            && tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "capture label must contain 1..64 ASCII letters, digits, '-' or '_'"
    );
    fs::remove_file(file)?;
    Ok(Some(tag.into()))
}
fn write_capture(
    dir: &Path,
    tag: &str,
    frame: u64,
    time: i64,
    renderer: &Renderer,
    views: &[xr::View],
    flags: xr::ViewStateFlags,
    ui_anchor: &ui_anchor::UiAnchor,
    indices: &[u32],
    mut pixels: Vec<Option<Vec<u8>>>,
) -> Result<()> {
    let mut eyes = Vec::new();
    let extra = if pixels.len() > 2 {
        pixels.split_off(2)
    } else {
        Vec::new()
    };
    for (eye, (view, bytes)) in views.iter().zip(pixels).enumerate() {
        let target = &renderer.eyes[eye];
        let name = format!("{tag}-{}.png", if eye == 0 { "left" } else { "right" });
        let mut encoder = png::Encoder::new(
            BufWriter::new(fs::File::create(dir.join(&name))?),
            target.extent.width,
            target.extent.height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&bytes.context("missing GPU readback")?)?;
        writer.finish()?;
        let p = view.pose.position;
        let q = view.pose.orientation;
        let f = view.fov;
        eyes.push(json!({"eye": eye, "png": name, "width": target.extent.width, "height": target.extent.height,
            "swapchain": format!("{:?}", target.swapchain.as_raw()), "image_index": indices[eye], "array_index": 0,
            "swapchain_format": format!("{:?}", renderer.format),
            "position_m": [p.x,p.y,p.z], "orientation_xyzw": [q.x,q.y,q.z,q.w],
            "fov_radians": {"left":f.angle_left,"right":f.angle_right,"up":f.angle_up,"down":f.angle_down}}));
    }
    let mut layer_images = Vec::new();
    for (n, bytes) in extra.into_iter().enumerate() {
        let target = if n < 2 {
            &renderer.foreground[n]
        } else {
            renderer.quad.as_ref().unwrap()
        };
        let suffix = ["foreground-left", "foreground-right", "focus-quad"][n];
        let name = format!("{tag}-{suffix}.png");
        let mut encoder = png::Encoder::new(
            BufWriter::new(fs::File::create(dir.join(&name))?),
            target.extent.width,
            target.extent.height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&bytes.context("missing layer readback")?)?;
        writer.finish()?;
        layer_images.push(json!({"role":suffix,"png":name,"width":target.extent.width,"height":target.extent.height,"image_index":indices[n+2]}));
    }
    atomic_json(
        &dir.join(format!("{tag}.json")),
        &json!({"schema":1,"tag":tag,"frame":frame,
        "predicted_display_time_ns":time,"projection_submitted":true,"view_count":2,
        "view_state_flags": ViewFlags::from(flags),
        "ui_anchor": ui_anchor,
        "controllers": renderer.controller_status(),
        "passthrough": renderer.passthrough_status(),
        "focus_quad":renderer.quad_status(),"extra_layer_images":layer_images,
        "pixels":"actual GPU swapchain readback; when Quad is active eyes are background-only and extra_layer_images contain foreground and Quad; not final compositor output","panorama_translation":"ignored", "eyes":eyes}),
    )
}
fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn ui_profile() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("XR_UI_PROFILE").is_some_and(|v| v == "1"))
}
