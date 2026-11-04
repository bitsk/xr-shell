// SPDX-License-Identifier: GPL-3.0-or-later
mod art;
mod desktop;
mod environment;
mod gpu;
mod input;
mod raster;
mod renderer;
mod spatial;

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
}
impl Options {
    fn parse() -> Result<Option<Self>> {
        let mut o = Self {
            environment: "/usr/share/xr/environments/qingkong/environment.json".into(),
            seconds: 0,
            capture_dir: None,
            validation: false,
            scale: 1.0,
            components: false,
            debug_input: false,
            socket: None,
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
                "--view-scale" => {
                    o.scale = args
                        .next()
                        .context("--view-scale requires a number")?
                        .parse()?
                }
                "--help" | "-h" => {
                    println!(
                        "xr-shell [--environment FILE] [--seconds N] [--capture-dir DIR] [--validation] [--view-scale 0.1..1]\n\
                              --seconds 0 runs until SIGINT/SIGTERM or runtime session shutdown.\n\
                              Write an ASCII label to DIR/capture.request to save that frame's two eyes and metadata."
                    );
                    return Ok(None);
                }
                _ => bail!("unknown argument: {arg}"),
            }
        }
        ensure!(
            o.scale.is_finite() && (0.1..=1.0).contains(&o.scale),
            "view scale must be in 0.1..1"
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
    asset_sha256: Option<String>,
    validation_enabled: bool,
    validation_errors: u64,
    validation_warnings: u64,
    frames: u64,
    waited_frames: u64,
    eye_submissions: [u64; 2],
    view_state: ViewStateObservations,
    captures: Vec<String>,
    session_states: Vec<String>,
    elapsed_seconds: f64,
    reference_space: Option<String>,
    cleanup: Cleanup,
    outcome: String,
    error: Option<String>,
    desktop: Value,
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
    println!(
        "XR_ENVIRONMENT={} SHA256={}",
        environment.id, environment.sha256
    );
    summary.environment = Some(environment.id.clone());
    summary.asset_sha256 = Some(environment.sha256.clone());
    let quit = Arc::new(AtomicBool::new(false));
    let signal_quit = quit.clone();
    ctrlc::set_handler(move || {
        signal_quit.store(true, Ordering::Relaxed);
    })?;
    let entry = unsafe { xr::Entry::load(&())? };
    let available = entry.enumerate_extensions()?;
    ensure!(
        available.khr_vulkan_enable2,
        "runtime requires XR_KHR_vulkan_enable2"
    );
    let mut extensions = xr::ExtensionSet::default();
    extensions.khr_vulkan_enable2 = true;
    let instance = entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "UKUI XR Shell",
            application_version: 1,
            engine_name: "xr-shell",
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
    let config = instance.enumerate_view_configuration_views(system, VIEW_TYPE)?;
    ensure!(config.len() == 2, "PRIMARY_STEREO must expose two views");
    let blends = instance.enumerate_environment_blend_modes(system, VIEW_TYPE)?;
    ensure!(
        blends.contains(&xr::EnvironmentBlendMode::OPAQUE),
        "opaque panorama mode unavailable"
    );
    let gpu = Gpu::new(&instance, system, options.validation)?;
    summary.gpu = Some(gpu.name.clone());
    let validation_stats = gpu.instance.validation.clone();
    let (session, mut waiter, mut stream) = gpu.session(&instance, system)?;
    let spaces = session.enumerate_reference_spaces()?;
    let space_type = if spaces.contains(&xr::ReferenceSpaceType::STAGE) {
        xr::ReferenceSpaceType::STAGE
    } else {
        xr::ReferenceSpaceType::LOCAL
    };
    summary.reference_space = Some(format!("{space_type:?}"));
    let space = session.create_reference_space(space_type, xr::Posef::IDENTITY)?;
    let mut input = if options.components {
        Some(input::Input::new(&instance, &session)?)
    } else {
        None
    };
    let mut renderer = Renderer::new(gpu.clone(), &session, &config, &environment, options.scale)?;
    let mut desktop = if options.components {
        let path = options.socket.clone().unwrap_or_else(|| {
            PathBuf::from(env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into()))
                .join("xr-desktop/shell.sock")
        });
        Some(desktop::Desktop::new(&path, options.debug_input)?)
    } else {
        None
    };
    let started = Instant::now();
    let result = frame_loop(
        options,
        summary,
        &instance,
        &session,
        &mut waiter,
        &mut stream,
        &space,
        &mut renderer,
        &quit,
        &mut desktop,
        &mut input,
    );
    if let Some(d) = &desktop {
        summary.desktop = d.snapshot();
    }
    drop(desktop);
    summary.elapsed_seconds = started.elapsed().as_secs_f64();
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
    result?;
    idle?;
    ensure!(
        summary.validation_errors == 0,
        "Vulkan validation reported {} errors",
        summary.validation_errors
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn frame_loop(
    options: &Options,
    summary: &mut Summary,
    instance: &xr::Instance,
    session: &xr::Session<xr::Vulkan>,
    waiter: &mut xr::FrameWaiter,
    stream: &mut xr::FrameStream<xr::Vulkan>,
    space: &xr::Space,
    renderer: &mut Renderer,
    quit: &AtomicBool,
    desktop: &mut Option<desktop::Desktop>,
    input: &mut Option<input::Input>,
) -> Result<()> {
    let mut events = xr::EventDataBuffer::new();
    let mut running = false;
    let mut started = None::<Instant>;
    loop {
        while let Some(event) = instance.poll_event(&mut events)? {
            match event {
                xr::Event::SessionStateChanged(e) => {
                    let state = e.state();
                    println!("XR_SESSION_STATE={state:?}");
                    summary.session_states.push(format!("{state:?}"));
                    if let Some(d) = desktop {
                        d.set_xr_focus(state == xr::SessionState::FOCUSED);
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
                            return Ok(());
                        }
                        xr::SessionState::EXITING => return Ok(()),
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
        if (quit.load(Ordering::Relaxed) || timed_out) && !summary.cleanup.exit_requested {
            if !running {
                return Ok(());
            }
            session.request_exit()?;
            summary.cleanup.exit_requested = true;
            println!("XR_EXIT_REQUESTED=1");
        }
        if !running {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        if let Some(d) = desktop {
            d.tick()?;
            if let Some(panels) = d.panels()? {
                renderer.update_panels(&panels)?;
            }
        }
        let state = waiter.wait()?;
        stream.begin()?;
        summary.waited_frames += 1;
        if !state.should_render {
            stream.end(
                state.predicted_display_time,
                xr::EnvironmentBlendMode::OPAQUE,
                &[],
            )?;
            continue;
        }
        let (flags, views) =
            session.locate_views(VIEW_TYPE, state.predicted_display_time, space)?;
        summary
            .view_state
            .observe(flags, &views, state.predicted_display_time);
        if !flags.contains(xr::ViewStateFlags::ORIENTATION_VALID) {
            stream.end(
                state.predicted_display_time,
                xr::EnvironmentBlendMode::OPAQUE,
                &[],
            )?;
            continue;
        }
        ensure!(views.len() == 2, "runtime changed stereo view count");
        let head_position = [
            (views[0].pose.position.x + views[1].pose.position.x) / 2.,
            (views[0].pose.position.y + views[1].pose.position.y) / 2.,
            (views[0].pose.position.z + views[1].pose.position.z) / 2.,
        ];
        let dt = state.predicted_display_period.as_nanos() as f32 / 1e9;
        if let Some(d) = desktop.as_mut() {
            let q = views[0].pose.orientation;
            d.update_head(head_position, [q.x, q.y, q.z, q.w], dt);
        }
        if let (Some(i), Some(d)) = (input.as_mut(), desktop.as_mut()) {
            i.update(session, space, state.predicted_display_time, d)?;
        }
        if let Some(d) = desktop.as_mut() {
            d.animate(dt);
            if let Some(panels) = d.panels()? {
                renderer.update_panels(&panels)?;
            }
            renderer.update_panel_poses(&d.panel_poses(), head_position)?;
        }

        let capture = capture_request(options.capture_dir.as_deref())?;
        let mut indices = [0u32; 2];
        let mut pixels = Vec::with_capacity(2);
        for eye in 0..2 {
            indices[eye] = renderer.eyes[eye].swapchain.acquire_image()?;
            renderer.eyes[eye]
                .swapchain
                .wait_image(xr::Duration::INFINITE)?;
            let result =
                renderer.render(eye, indices[eye] as usize, &views[eye], capture.is_some());
            let release = renderer.eyes[eye].swapchain.release_image();
            pixels.push(result?);
            release?;
        }
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
        stream.end(
            state.predicted_display_time,
            xr::EnvironmentBlendMode::OPAQUE,
            &[&xr::CompositionLayerProjection::new()
                .space(space)
                .views(&layers)],
        )?;
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
                indices,
                pixels,
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
    indices: [u32; 2],
    pixels: Vec<Option<Vec<u8>>>,
) -> Result<()> {
    let mut eyes = Vec::new();
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
    atomic_json(
        &dir.join(format!("{tag}.json")),
        &json!({"schema":1,"tag":tag,"frame":frame,
        "predicted_display_time_ns":time,"projection_submitted":true,"view_count":2,
        "view_state_flags": ViewFlags::from(flags),
        "pixels":"actual GPU swapchain readback after rendering","panorama_translation":"ignored", "eyes":eyes}),
    )
}
fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}
