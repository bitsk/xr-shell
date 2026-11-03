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

On normal completion, `summary.json` records runtime, GPU, asset hash, frames,
per-eye submission counts, state transitions, validation counts and cleanup
status. It is placed in the capture directory when supplied, otherwise in
`$XDG_STATE_HOME/xr-shell/` or `$HOME/.local/state/xr-shell/`. Program errors
return nonzero and record the available failure state; successful compilation
alone is not runtime acceptance.

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
