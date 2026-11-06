# xr-shell

Rust OpenXR panorama Shell for UKUI XR. The first implementation loads a
versioned environment, renders each PRIMARY_STEREO view into its own runtime
swapchain, and submits one projection layer containing both views. It uses
`openxr` and `ash` directly; environment artwork is supplied by the independent
`xr-environments` package.

Version 0.2 adds the component protocol and spatial window composition.
Dock, spatial widgets, Linux window serving and system controls are independent
components. Builds, historical QEMU runs, native ARM64 development runs and
physical headset acceptance are recorded independently by the integration project.

## Build

Use Rust 2024, Cargo, glslangValidator and the checked-in Cargo.lock. Shader
sources are compiled into SPIR-V at build time; a deployed executable does not
need the shader compiler, Cargo or a network connection.

The native build also links Cairo, Pango/PangoCairo and librsvg. Install their
development packages and check out the matching xr-spatial-kit sibling repository.

```sh
cargo fetch --locked
# After dependencies have been cached:
cargo build --release --locked --offline
```

`CARGO_HOME` and `CARGO_TARGET_DIR` can select an isolated dependency cache and
build directory. `GLSLANG_VALIDATOR` selects a private shader compiler when it
is not in PATH. Preserve the toolchain versions, source commit and artifact
hash when handing a build to a target system. Runtime selection is external:
use the chosen OpenXR loader/runtime and Vulkan ICD rather than changing a
system-wide default from this program.

## Run

```sh
xr-shell --environment /usr/share/xr/environments/qingkong/environment.json \
  --seconds 30 --capture-dir /tmp/xr-shell-capture --validation
```

Options:

- `--environment FILE`: manifest path; defaults to the Qingkong path above.
- `--seconds N`: running-session duration, with `0` for interactive lifetime.
- `--capture-dir DIR`: optional on-demand GPU capture and final summary output.
- `--validation`: explicitly require and enable `VK_LAYER_KHRONOS_validation`;
  startup fails if unavailable. Debug-utils messages and error/warning counts
  are collected. Target-specific layer search paths remain profile settings.
- `--view-scale 0.1..1`: per-eye fraction of runtime-recommended dimensions;
  defaults to `1`.

SIGINT and SIGTERM request a normal OpenXR exit. The program processes the
runtime's STOPPING state, ends the session and destroys its swapchains and
Vulkan resources before writing the final summary. It does not own or stop a
Monado service, simulator GUI, desktop session or unrelated application; the
launching session manager/development profile owns those processes.

## Environment rendering

The schema-v1 environment is verified before GPU initialization: texture hash,
PNG dimensions, equirectangular projection, sRGB colour and coordinate mapping.
The GPU samples the image in linear colour space and writes to a supported
sRGB swapchain format. Camera orientation and each eye's FOV drive sampling;
view translation is intentionally ignored for this infinite background.

World coordinates are +Y up, -Z front and +X right. The panorama's front is
`u=0.5`, right `u=0.75`, with U repeat and V clamp. The environment keeps world
orientation as the user turns. It is a distant panorama, so the artwork's
painted ground does not provide near geometry, collision or positional
parallax. Matching left/right pixels can be correct when both eye directions
and FOVs match; independent view/swapchain metadata establishes stereo output.

## On-demand capture and summary

Normal rendering performs no pixel readback. To capture a frame, atomically
publish a short ASCII label while the program runs:

```sh
printf 'front\n' > /tmp/xr-shell-capture/capture.request.tmp
mv /tmp/xr-shell-capture/capture.request.tmp \
  /tmp/xr-shell-capture/capture.request
```

Labels contain 1–64 ASCII letters, digits, `-` or `_`. The request is consumed
once. For `front`, the actual rendered GPU images are saved as
`front-left.png` and `front-right.png`; `front.json` is written after both
images are complete and the corresponding two-view projection was submitted.
It contains the frame/time, per-eye pose, FOV, swapchain, acquired image index,
format and dimensions. Readback and PNG encoding are diagnostic work, so
capture intervals must be excluded from steady-state performance measurements.

`view_state_flags` preserves the actual `locate_views` raw bits plus separate
orientation/position valid and tracked booleans. `summary.json` also contains
`view_state`: valid/invalid and tracked/untracked counts cover every successful
view location, including orientation-invalid frames skipped by the existing
renderer. First/last mean eye positions and their predicted display times are
recorded only from two finite returned eye positions with `POSITION_VALID`;
they remain null when no such sample exists. These diagnostics neither adjust
poses or UI anchors nor change rendering/input validity gates. A tracked flag
is separate from a valid flag, and frame submission is separate from physical
tracking and wearer acceptance.

On normal completion, `summary.json` records runtime, GPU, asset hash, frames,
per-eye submission counts, state transitions, validation counts and cleanup
status. It is placed in the capture directory when supplied, otherwise in
`$XDG_STATE_HOME/xr-shell/` or `$HOME/.local/state/xr-shell/`. Program errors
return nonzero and record the available failure state; successful compilation
alone is not runtime acceptance.

Initialization writes `XR_INIT_STAGE` begin/complete records for reference
spaces, controller actions and each eye swapchain; errors retain the exact
OpenXR/Vulkan operation in their context chain. `initialization_stage` in the
summary identifies the last major stage entered. `XR_VIEW_CONFIG` and swapchain
records show runtime recommended/max samples and the unchanged requested
sample count, format, size, array size and usage.

`--ui-anchor=stage` (default) retains the existing UI layout coordinates.
`--ui-anchor=head` waits for finite stereo poses marked orientation/position
valid, then freezes the UI translation at their actual mean minus `[0,1.6,0]`.
Until that sample arrives, the panorama and component processing continue but
spatial UI is neither displayed nor hit. UI geometry and transparency sorting
use the same fixed translation; mouse/controller rays and the existing lazy
Dock layout use its inverse. Local pixel input stays in local coordinates.
The runtime reference space, tracked views, projection layers, panorama and
captured eye poses are unchanged. `ui_anchor` in summary and capture metadata
records the source eye poses, flags, time and fixed offset. This mode does not
continuously recenter the desktop or synthesize a tracked eye height.

S1 deliberately waits for each eye's GPU work before releasing its XR image.
This is a clear correctness baseline, not a claim of an optimized multi-frame
pipeline or zero-copy display transport. Observation through a browser may
have separate CPU-copy and video-encoding costs.

## Component desktop candidate

Version 0.2 supplies the Shell-owned local protocol SDK, independent scene
clients, immutable SHM window snapshots and a spatial Vulkan sprite renderer.
Use `--components` to expose the local same-user scene socket; `--debug-input`
adds the explicit developer input channel used by `xr-shellctl`. Native ARM64
and historical QEMU development profiles are maintained by xr-image-tools; installing Shell does not
launch services. Runtime acceptance remains in the integration records.

World coordinates remain +Y up and -Z forward. UI-local +Y is down. Positive
UI Pitch tilts a panel face upward; right-side UI uses Yaw -18 degrees. The
same `spatial::Frame` maps actual world geometry and input rays. Components
can publish independent spatial Group subtrees. Interactive primitives have
8mm local relief and smoothly add 12mm on hover; they are independent Vulkan
instances, not a single flattened screenshot.

Client content is an unframed, rounded surface with a thin rim. Shell adds no
upper titlebar. A separate lower grab capsule and control orb own spatial
manipulation, close, minimise and a real tessellated curved-surface toggle.
Wayland decoration negotiation belongs to xr-wayland; custom application
header widgets are application content and are not cropped by Shell.

Spatial UI uses the Kit smoke-glass palette independently of application
light/dark settings. Text is rasterised through Pango and installed fonts;
registered freedesktop icon IDs resolve installed UKUI theme SVG/PNG assets.
Environment-only frost sampling is a bounded visual baseline; general scene
backdrop filtering, retained rendering, DMA-BUF and target build closure remain
separate work. Normal frame rendering does not read the eye buffers back.

Application snapshots larger than the raster budget are scaled in full to a
maximum texture edge of 2048 pixels. Their logical dimensions and ray/input
coordinates stay unchanged; no client content is cropped. This CPU-copy path
does not establish a DMA-BUF or zero-copy implementation.


## Optical glass candidate

Version 0.2.1 adds rounded 6 mm panel sidewalls (3 mm client edges and 2 mm
control relief), back-face rejection, a diagonal 14%–2% display-value highlight,
cool environment transmission and a directional 45%/12% rim. Soft SDF shadows
extend 24 logical pixels at a 10 mm rear offset, retaining curved-window geometry.
These are deliberately bounded UI materials; the frost samples the environment,
not a general blur of arbitrary overlapping scene content.

Client alpha is clipped twice: the complete surface is rounded in its Cairo
snapshot and the Vulkan fragment shader applies the same 16 px SDF boundary.
A 2 px rim, independent bright grab capsule and two lower resize handles complete
the content plane. Resize capture uses the frozen handle frame, keeps the opposite
upper corner anchored and sends a real client configure on release. The expanded
control-orb strip collapses when the ray leaves its complete menu region.

The system volume rail is a 22 px blue gradient capsule inside a larger scene
hit region. Buttons use semibold labels at 115% of the prior size; keycaps have
centred, slightly raised labels and a 1 px quarter-opacity white rim.

These changes must be built and captured on the selected native target before
new visual or interaction acceptance is recorded. Earlier 0.2.0 captures describe
that earlier candidate only.

The Qingkong component art is drawn before implementation in the Spatial Kit
`design/qingkong-controls/` reference library. The style package provides original
RGBA atlases and verified crop/inset metadata in `/usr/share/xr/art/qingkong`.
Shell separates crops into premultiplied images with transparent sampling gutters,
uses nine-slice scaling for optical edges, and keeps glyphs and live state separate.
`XR_SPATIAL_ART_DIR` can select a development art directory; the executable prefix
is checked before the system path. `XR_ART_LIBRARY` and `XR_ART_RESOURCE` report
loaded art; explicit fallback logs identify missing or invalid resources.

## Shared taskbar follow anchor

All roots and independent groups opting into `dock_lazy_follow` use one
Shell-owned rigid transform. It preserves island gaps, normals, shadows and
ray coordinates across Dock/System UI scene refreshes. Yaw keeps the 20-degree
activation and 2-degree settling thresholds; translation is smoothed. Capturing
any followed control freezes the entire taskbar until release or Cancel.
Unrelated world-locked panels and application windows retain their positions.
The negotiated `scene.spatial-policy.v1` extension selects group policy explicitly;
missing group policy remains world locked. Snapshots expose `taskbar_follow`
and `taskbar_capture_frozen` for diagnostics without injecting XR poses.

The taskbar defaults to `fixed`. The control centre can select fixed/follow;
fixing preserves the current pose and enabling follow starts smooth motion.
System UI persists only acknowledged choices in its per-user preferences.
`taskbar.v1` provides `TaskbarIntent{mode:fixed|follow}` / `TaskbarApplied{mode}`.
Snapshots expose `taskbar_mode`; window/world-locked panel policies are unchanged.


## Controller input

Shell reads both hands through OpenXR actions, using the runtime's current
interaction profile. KHR simple and Vive select buttons, Touch A/X clicks and trigger
values route the same ray press/drag/release path. Grip and aim are separate pose
actions at the predicted display time; models use the grip pose, while pointer
hit testing uses aim. Invalid, non-finite or inactive poses are omitted every
frame. Pose freshness and the original hardware packet validity gates remain
the runtime's responsibility.

Touch thumbsticks (or Vive trackpads) scroll the focused application with a
0.18 dead zone and elapsed-time scaling. Left menu and left Y/right B send a
back action: release capture, then an Escape press/release to the acknowledged
focused application. SteamVR may reserve its system key, so the application
back binding does not depend on that key. Grip pressure is exposed in the
controller state without assigning it an arbitrary desktop action.

Input is routed only while the XR session is FOCUSED. Focus loss, invalid aim
or a failed action query cancels the affected pointer; a held trigger/menu must
be released before it can issue a new press after recovery. Both hands are
sampled even when one owns a drag. The active capture remains authoritative;
other-hand presses are not replayed when it releases. Physical controller
motion, button mappings and grip/aim alignment require device acceptance.

## Original controller models

Separate device assets provide `left`/`right` `mesh.json` and `texture.png`
under `/usr/share/xr/devices/a081/controllers` (or `XR_CONTROLLER_MODEL_DIR`).
Shell renders these models at the runtime grip action and routes rays through
the independent aim action. A device package may provide its native
`model_binding.rotation_xyzw` in `controller-anchors.json`; this rotates only
the original model frame and is recorded alongside the unmodified grip/aim
actions in captures. The A081 provider
uses device-specific render-model components from the original system rather
than inheriting another controller's grip/aim transforms. Source parameters
are maintained by the runtime component owner.

## UI texture updates

Hover and press reuse unchanged per-control rasters and retain up to three
button/slider visual states. Text, scene properties and client frame IDs
invalidate their own content. Atlas layout changes rebuild the texture; stable
layouts upload only changed rectangles. The private atlas is protected by the
Shell submission fence rather than a device-wide idle in the interaction path.
`XR_UI_PROFILE=1` reports raster cache hits/misses and GPU upload bytes/times;
slow updates are logged even without profiling.

Controller ray feedback shares the window/control hit query, including curved
surfaces, rounded corners and occlusion. A hit extends the beam to its measured
distance and displays a reticle; a miss keeps the idle ray without a target dot.
Captured drags continue to use their target plane. Feedback is resolved after
UI animation in the same frame, and captures include the world endpoint and
distance. DebugInput `ray_probe` performs a read-only hit query when debug input
is enabled. This changes presentation only; device aim/grip parameters remain
owned by the runtime.

## Controller Home recenter

On A081, hold Home on either controller for one second to bring the desktop
center and horizontal heading to the current headset position and gaze. Floor
height and head pitch remain unchanged. Each hold fires once; release Home
before the next hold. A short press does not recenter. Focus loss, stale input,
producer restart or disconnection cancels the hold and requires a fresh release.

The Shell subscribes read-only to the existing `A081_CONTROLLER_POSE_SOCKET`
wire-v2 stream owned by qiyu-controller-tracking because SteamVR reserves the
system button. It reads fresh valid controls independently of controller pose.
Recenter requires valid headset position and orientation, creates a Shell-local
reference space, clears pointer captures and resets the taskbar anchor while
retaining its fixed/follow mode. Sensor state and other applications' runtime
origins are unaffected. Missing controller sockets disable this shortcut.

Xwayland application windows receive the same external grab, menu, close,
minimize, curve and resize controls as native Wayland toplevels. The window
service (0.1.8 or later) supplies `window_controls` from X11 window type,
override-redirect and transient metadata; menus, tooltips and splash surfaces
remain undecorated. Mapping an eligible application also acknowledges its
spatial focus through the normal window-service path.


`scene.horizon-spawn.v1` (Shell 0.2.16) permits independent spatial groups with
`spatial_policy=head_horizon_spawn`. `position_m` is a design-space offset about
an eye at `[0,1.6,0]`; Shell projects the current head yaw onto the horizon,
translates to current valid stereo eye height, and faces the panel toward that
eye. A group freezes once while present; hiding/removing it drops the anchor,
and re-adding it samples the new pose. Recentering clears those frozen frames.
No new group draws before a valid finite stereo pose is available. Existing
world-locked and taskbar-follow policies retain their own anchors. Raster/quad
placement and ray picking share the same frozen frame. This protocol support
requires separate rendered and physical-device acceptance.

### Focus Quad and frame timeline

`--focus-quad=auto` (or `XR_FOCUS_QUAD=auto`) experimentally promotes an acknowledged, visible flat focus window
when the runtime permits three layers and every other surface can be placed
entirely before or behind its plane. `--focus-quad=off` retains the original
projection for A/B comparisons. Projection remains the default: the first native
three-layer trial increased compositor GPU cost. Cadence also degraded after
restarting the projection baseline, so the cadence change is not isolated to Quad. Background projection, focused Quad and alpha
foreground projection share the same reference space and display time. The
window is sampled at its content resolution using the existing linear-light,
rounded-alpha shader; input geometry is unchanged. Focus changes and resize
replace only the required texture target. Curves, intersecting/coplanar geometry,
back-facing views, unavailable resources, and controller poses whose depth has
not been partitioned use the original projection. This conservative controller
fallback is intentional; native Quad with physical controllers is not accepted.

Capture metadata reports active layers and fallback reasons. With Quad active,
`left/right.png` contain the background projection; `foreground-left/right.png`
and `focus-quad.png` are separate GPU readbacks of the other submitted layers.
They must not be represented as a final compositor/headset image.

The bounded timeline retains the latest 4096 rendered frames. Capture requests
write `timeline.json`; normal frames do not write telemetry to disk. Final
`summary.json` also contains the samples. CLOCK_MONOTONIC stage boundaries cover
application work, wait/begin, locate, input/atlas preparation, acquire/wait,
record/submit/completion wait, release and end. An XrTime/monotonic clock pair is
recorded only when KHR_convert_timespec_time is available. CPU wait duration is
not GPU execution time, end-frame return is not scanout, and these observations
do not establish physical motion-to-photon latency.
