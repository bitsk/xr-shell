# Vultra · 高性能空间合成器

Vultra 是 **openKylin XR 空间智能版本**中 **HyperSpace XR 空间桌面**的合成核心。采用 Rust 模块化开发，基于 OpenXR 与 Vulkan 实现双眼渲染与三维空间界面合成。任务栏、空间控件、窗口服务与系统控制以独立组件协作。晴空（Qingkong）是一款可选空间主题。`xr-shell` 是当前仓库、包与可执行文件的技术标识。

Rust OpenXR/Vulkan spatial compositor for HyperSpace. The first implementation loads a
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
`view_state`: valid/invalid and tracked/untracked counts cover the early input
view location, including orientation-invalid frames skipped by the renderer.
The separate `render_view_state` covers late locations, so each counter still
counts at most one observation per frame. First/last mean eye positions and
their predicted display times are
recorded only from two finite returned eye positions with `POSITION_VALID`;
they remain null when no such sample exists. These diagnostics neither adjust
poses or UI anchors nor change rendering/input validity gates. A tracked flag
is separate from a valid flag, and frame submission is separate from physical
tracking and wearer acceptance.

Head views are located before input handling and again after scene preparation
and swapchain waits, immediately before rendering. Both calls use the runtime's
unchanged `predictedDisplayTime`; rendered views also supply projection-layer
poses, FOVs and focus-quad crop bounds. Invalid late views release all acquired
images and skip submission. This gives the runtime a later opportunity to use
new tracking observations, but does not establish that it actually refreshed its
internal sample or that the compositor's final reprojection used the same time.

Timeline schema 4 records `input_view_locate_mono_ns` and
`render_view_locate_mono_ns` call intervals, separate input/render head poses and
flags, and `render_runtime_now_xr_ns` with its monotonic clock bracket. Compare
the paired runtime time with `predicted_display_xr_ns` to measure the remaining
application prediction horizon. These fields do not measure sensor sample age,
final compositor warp time or physical motion-to-photon latency. The existing
record/submit stage includes the second locate call; its own interval is separate.
`render_mono_ns` has nine boundaries ending at queue submission;
`retire_mono_ns` separately brackets the post-`xrEndFrame` completion wait and
query/readback/staging retirement. `end_frame_mono_ns[0..1]` still brackets the
XR call itself. The enclosing final frame stage includes retirement; its total
must not be interpreted as time inside `xrEndFrame`.

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

Both eyes share one Vulkan submission on the queue bound to the XR session.
After enqueueing, Shell releases its images and calls `xrEndFrame` before
waiting for that submission's fence, as permitted by
[the Vulkan enable2 swapchain contract](https://registry.khronos.org/OpenXR/specs/1.1-khr/html/xrspec.html#XR_KHR_vulkan_enable2).
The completion wait still precedes all query/readback access, staging release,
and next-frame resource updates. Submission, release and end-frame errors retain
pending buffers in Renderer until idle cleanup. Only one Shell batch is in
flight; initialization uploads remain synchronous. This changes CPU submission
order, not GPU execution cost, and does not establish the runtime's final warp
deadline or physical latency. Browser observation has separate copying and
video-encoding costs.

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

Panels have rounded 6 mm sidewalls (3 mm client edges and 2 mm control relief).
Glass uses linear-light premultiplied compositing with per-eye specular and
Fresnel highlights. It does not sample the sky through overlapping windows.
Dock capsules use a neutral light tint at 0.68 alpha and a single thin
28%/8% top-left/bottom-right rim. Face shading has no vertical tint gradient
or inner glow. Shadows are neutral, limited to a 12 px spread, and fade out
inside the face silhouette instead of tinting the translucent interior.
Icon and battery trays do not cast individual shadows.

The three taskbar centres follow a horizontal 2.2 m radius at the same height,
0.62 m below the reference eye. Each face tilts up by about 16 degrees.
The centre is 620 px wide, the sides 340 px, and
the arc-length gaps are 24 px. Each capsule and its child geometry use the same
2.2 m curve for rendering and ray projection. The centre viewing distance
is about 2.29 m.
App trays use 0.14 alpha (0.32 hovered), retain the existing 10 mm GPU hover
lift and carry live 14x4 px running bars without a surrounding glow.

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
interaction profile. KHR simple select and Vive digital trigger remain primary
clicks. Touch/Index analog trigger uses 0.65 press / 0.35 release hysteresis;
grip uses 0.60 / 0.35. A/X (Index A, Vive trackpad click) is secondary/right
button 273; thumbstick click is middle button 274. Trigger is left button 272.

Grip over a toplevel window, embedded SurfaceReference or grab/orb companion
starts spatial movement without sending a client mouse click. A second hand
can join the grab to scale, including a trigger press on an already grabbed
window. Cancellation restores the initial transform; normal release commits
any bimanual application resize. Simultaneous grip wins over trigger. Each hand
owns one interaction until release; refused held buttons never replay later.

Spatial repeated clicks on the same surface and button within 40–420 ms snap
to the first press coordinates when the ray differs by at most 1.5 degrees OR
the local displacement is at most 24 pixels. The matching release stays fixed;
a drag beyond 24 pixels resumes movement and cannot seed another click pair.
Focus loss, cancellation, another target/button and surface size changes break
the pair. Client double-click timing/settings still determine application behavior.

The stick uses a radial 0.18 deadzone and quadratic precision response. Scroll
prefers the hovered surface, applying its focus and pointer position before the
scroll event; empty space falls back to keyboard focus. Occluding UI consumes
the ray. Existing launcher/Wi-Fi paging controls use 0.65 raw deflection with a
280 ms per-hand cooldown. A live surface capture prevents another hand from
stealing the legacy seat. While grabbing, Y changes reach at up to 1.2 m/s;
the window remains within the existing 0.6–4.0 m eye-distance bounds.

Button identity is retained while waiting for FocusApplied and during cancel.
Releasing before focus acknowledgement drops the pending click without sending
an orphan release. Pose/action loss, recenter, back and session focus loss
cancel the capture; held controls must return below release threshold before
reactivation. Tracking validity and OEM grip/aim/model transforms are unchanged.

With the existing debug guard, ray requests accept `button: 273` or `274`,
`squeeze: true`, or `scroll: [dx, dy]`. These call the production routing paths;
they do not inject a valid hardware pose. Optional trigger hold-to-right-click,
companion double-click shortcuts and stick-click distance reset remain separate
follow-ups; this implementation retains immediate trigger drag behavior.

## Original controller models

Separate device assets provide `left`/`right` `mesh.json` and `texture.png`
under an explicit `XR_CONTROLLER_MODEL_DIR`. Without device artwork, Vultra
uses built-in neutral controller meshes and aim rays.
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

## Runtime recenter

System/Home buttons belong to the OpenXR runtime. Vultra consumes
`ReferenceSpaceChangePending` at its effective frame time, cancels pointer
captures and reanchors the desktop. It does not read device controller packets.

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

The foreground projection renders and submits a per-eye rectangle enclosing all
foreground geometry, including thickness and two native pixels of padding. Its
sub-image and FOV are cropped together, preserving pixel density, real eye poses
and three-layer ordering. Near-plane crossings retain the full eye.
`XR_FOCUS_QUAD_CROP=off` retains full foreground images for A/B/A measurement.
Transparent foreground and Quad targets are cleared before drawing, including
when a swapchain image is reused after a resize. Swapchain allocations and the
completion fence remain unchanged.

Capture metadata reports active layers, crop rectangles/FOVs and fallback reasons. With Quad active,
`left/right.png` contain the background projection; `foreground-left/right.png`
and `focus-quad.png` are separate GPU readbacks of the other submitted layers.
They must not be represented as a final compositor/headset image.

The bounded timeline retains the latest 4096 rendered frames. Capture requests
write `timeline.json`; normal frames do not write telemetry to disk. Final
`summary.json` also contains the samples. CLOCK_MONOTONIC stage boundaries cover
application work, wait/begin, locate, input/atlas preparation, acquire/wait,
record/submit, release, end and resource retirement. An XrTime/monotonic clock pair is
recorded only when KHR_convert_timespec_time is available. CPU wait duration is
not GPU execution time, end-frame return is not scanout, and these observations
do not establish physical motion-to-photon latency.

### Controller rendering investigation

With `--debug-input` and the existing `debug.v1` capability, `DebugInput`
`{"kind":"controller_visuals","mode":"rays"}` hides the model while retaining
its rays. Mode `none` hides both; `all` restores both.
These switches preserve real action sampling, grip/aim poses, hit routing and
captures. Default `all` is restored on restart. Never treat hidden geometry as
loss of tracking or as physical controller acceptance.

`{"kind":"spatial_fast_path","enabled":false}` selects the previous preparation
path for same-session A/B/A measurements; default `true` prepares the UI atlas
once after input per rendered frame, traverses node siblings once, and animates
window companions without rebuilding all scene geometry. Atlas updates remain
complete when a pending upload must be replaced; upload fences, native pixels,
mipmaps and controller calibration are unchanged. Restore `true` after testing.

Timeline samples also contain located head/grip/aim poses, the draw mask,
four input/preparation stage durations (five monotonic boundaries), the selected
preparation path and `atlas_update` as `[regions, source_bytes, full_replacement]`.
`head_pose` uses the binocular midpoint position and left-view orientation;
`head_flags` retains the runtime validity bits. The last field describes the
pending upload before submission, not GPU traffic
or execution time. Samples remain bounded and are written only on capture/exit.

## Runtime-owned passthrough

`XR_ENVIRONMENT_MODE=opaque` selects the theme background (default).
`XR_ENVIRONMENT_MODE=passthrough` requires the runtime to enumerate
`ALPHA_BLEND`; unavailable support is an error, not an opaque fallback.
Vultra clears its background to transparent and submits alpha-enabled OpenXR
layers. Camera capture, calibration, lens correction, exposure timing and
motion compensation belong to XROS/runtime or the streaming headset.
No QSP1 reader, RAW10 decoder, camera calibration or device pose tap ships here.

The current native MIX runtime reports OPAQUE only. Its previous Shell-owned
camera path is removed; native passthrough requires runtime implementation
before it can be enabled again. WiVRn can advertise ALPHA_BLEND when its client
supports passthrough. Capability enumeration and a build do not establish
physical passthrough quality or latency acceptance.

## 光学双手

手输入会话通过 `XR_EXT_hand_tracking` 获取左右手各 26 个关节，使用真实位置、
方向、有效性及运行时半径绘制可弯曲的掌面与手指表面；无效手隐藏。
捏合沿已有 trigger 点击/拖动，抓握沿 squeeze 执行 6DoF 抓取及双手缩放。
模型与输入共同使用当前 OpenXR 参考空间，由运行时管理参考空间重置。
有手部模型时保持同一投影层深度关系，避免焦点 quad 截断手指。

`--hand-reference-capture FILE` 是只读保存几何的 GPU 截图入口：必须不启用 components、
指定 capture-dir 和 1..30 秒；记录文件必须标注 `source: recorded-capture-only`。
它不附加桌面动作、不发布追踪，也不提交显示投影层，仅用同一 Vulkan 模型路径
生成可复核的双眼截图。该诊断不能替代真人手部对齐或捏合/抓取验收。

## 会话交互开关

默认按每只手的 OpenXR interaction profile 选择手柄或光学手部显示；关节来自
`XR_EXT_hand_tracking`，手部瞄准、激活与抓握动作来自
[`XR_EXT_hand_interaction`](https://registry.khronos.org/OpenXR/specs/1.1/html/xrspec.html#XR_EXT_hand_interaction)。
来源改变取消已有抓取并要求按键释放；光学手部不发送震动。
不通过关节存在与否猜测物理手柄是否活跃，也不在 Shell 再做设备跟踪预测。

XROS 可显式提供 `XR_SESSION_CONTROLS_FILE`、`XR_INPUT_STATUS_FILE` 和
`XR_SCENE_CONTROLS_STATUS_FILE` 接入系统管理策略。未配置时不创建文件轮询线程；
配置后状态缺失或过期不当作可用输入。系统偏好不代表运行时已实现该能力。

## Frame performance observations

The bounded timeline includes CPU frame/input/draw stages, thread CPU time,
pose flags, model/atlas counters and skipped render reasons. Create an empty
`timeline-request` in the existing summary directory for a JSON-only snapshot;
this does not request a screen capture. Export work remains visible before the
next frame. Set `XR_SHELL_GPU_TIMING=1` only in a diagnostic launch to collect
optional GPU timestamp intervals after the existing completion fence.
See [the profiling guide](https://github.com/bitsk/openkylin-xr-docs/blob/main/wiki/frame-profiling.md)
for clock boundaries, system correlation and acceptance limits.
