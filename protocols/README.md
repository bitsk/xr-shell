# XR component protocol SDK 0.2

`xr-shell-protocols` is owned by Shell and consumed as a pinned source SDK by
Dock, system-ui and the Wayland service. `Node` in `src/lib.rs` defines primitive
fields; the Shell validates graph transactions atomically at a frame boundary.
This is the first local implementation of protocol major 1, minor 0. It is not
a stable C ABI, a remote protocol, or a third-party security sandbox.

The same-user `SOCK_SEQPACKET` endpoint defaults to
`$XDG_RUNTIME_DIR/xr-desktop/shell.sock`. Shell creates its directory as 0700
and socket as 0600, checks peer credentials, and rejects an existing socket.
All JSON metadata is bounded to 65536 bytes. All identity, generation,
revision, epoch, request and frame counters are decimal strings. Dimensions,
rectangles and bounded indices are JSON numbers. Only immutable SHM commits
carry a single SCM_RIGHTS FD; other messages carry none.

Use `Client::connect(path, role, required)` with `core.v1` and the needed
capabilities. `welcome` contains negotiated capabilities, all connection
identities, `root_id` and `scene_revision`. `send(kind,payload,fds)` returns a
request ID; `recv(timeout_ms)` yields a `Packet` with the envelope and owned,
close-on-exec FDs. The client owns event dispatch and signal handling.

Roles are `dock`, `system_ui`, `window_server`; only one window server is
allowed. Capabilities are negotiated as an intersection, never inferred from
version alone. Normal messages include the connection identity envelope.
Unknown major versions, unsupported capabilities and stale generations fail
explicitly. Candidate capabilities become accepted only with integration
runtime records; successful compilation does not grant acceptance.

The M0 contract is implemented by these messages:

- `scene.v1`: `SceneTransaction{base_revision,revision,operations}` contains
  flat `upsert_node` fields from `Node`, `remove_node{node_id}`, or
  `set_root_policy{policy,logical_size,size_m,position_m}`. Revisions advance
  by one; invalid transactions return `SceneRejected` without partial state.
  Component-local rectangles use logical pixels, +X right/+Y down. Scene
  roots are metres in +Y-up/-Z-front world coordinates. Policies are
  `world_locked` and `dock_lazy_follow`.
  With negotiated `scene.spatial-policy.v1`, an independent `spatial_panel`
  Group may set `spatial_policy` to either policy (default `world_locked`).
  All `dock_lazy_follow` roots and groups share one Shell-owned rigid anchor,
  including across component processes. Capturing any followed node freezes
  the whole taskbar until release/Cancel. Unrelated panels remain world locked.
- `scene.horizon-spawn.v1`: an independent spatial Group may opt into
  `spatial_policy=head_horizon_spawn` in addition to `scene.spatial-policy.v1`.
  Its `position_m` describes an offset from the design eye `[0,1.6,0]`.
  Shell samples current valid stereo eye height and horizon-projected head yaw,
  computes a user-facing pitch/yaw, and freezes that frame once while the Group
  exists. Removal drops the anchor; re-adding or recentering samples a new one.
  Rendering and ray hits use that same frozen frame. No new group is shown
  until a valid pose is available. This policy does not apply to scene roots.
- `snapshot.v1`: `GetSnapshot`, `Snapshot`, `StateChanged`; the Shell publishes
  windows, active workspace, acknowledged focus epoch, component scene
  revisions and bounded system/clipboard summaries. A revision gap requires
  another snapshot. Node trees are not replicated in these broadcasts.
  Only the window service sends `WindowState` and `WindowRemoved`.
- `input.v1`: `InputEvent` includes target node, local coordinates, phase,
  action, interaction/capture IDs, pointer/source, monotonic nanoseconds and
  focus epoch. Capture belongs to Shell, survives leaving a hot region, and
  ends on release or `Cancel`. Clients match press/release and Cancel using
  the Kit state machine; an arbitrary release is never an activation.
- `focus.v1`: Shell sends `FocusChanged`; only an acknowledged `FocusApplied`
  permits `SurfaceInput`. Evdev keycodes and Linux BTN codes are explicit;
  the Wayland owner creates serials and performs xkb conversion.
- `shm.v1`: registered surface snapshots use `ShmBufferCommit` fields from the
  M0 contract: generation/surface/buffer/frame/commit IDs, dimensions, stride,
  offset, length, format/alpha/colour space and damage rectangles. The
  immutable memfd requires all WRITE/GROW/SHRINK/SEAL seals. Shell copies the
  validated bytes and emits exactly one `BufferRelease`; sender wl_buffer
  release and this snapshot release are distinct lifetimes.

The implementation also defines these explicit extensions:

- `window.v1`: `WindowIntent{intent:launch|focus|close|resize|move,...}` uses
  installed desktop IDs or existing surface keys, never shell commands.
  Launch/resize/close go to the window service. Its `RequestResult` echoes
  `origin_component_id` and `origin_request_id`; success means the operation
  was accepted by that service, not that an application or physical display
  has passed acceptance. Window/task state still comes from actual surfaces.
- `workspace.v1`: `WorkspaceIntent{workspace:0..3}` changes Shell visibility
  and focus; `WorkspaceApplied` and snapshots return authoritative state.
- `system.v1`: `SystemState` is published by the system adapter. `KeyIntent`
  and `TextCommit` route only to acknowledged surface focus. `InputAccepted`
  is returned after a successful window-service result, not after queueing.
- `clipboard.v1`: `ClipboardSet{mime_type,text}`, actual `ClipboardChanged`
  and `GetClipboard`. UTF-8 text is bounded to 60000 bytes. Snapshot preview
  is never a replacement for full selection data.
- `debug.v1` is offered only with Shell `--debug-input`. `GetScene` pages at
  most 16 nodes for one component. `DebugInput` supports explicit local
  pointer/ray/key/text parameters and cancellation. This is a developer
  input channel; it does not manufacture tracked OpenXR poses.

Both scene clients and surfaces are generation-owned. Disconnect removes
owned resources, capture and state; ordinary process reconnect cannot inherit
old IDs. `SessionEnding` precedes normal Shell socket closure. Components
handle it as normal shutdown; unannounced disconnection is a failure.

The renderer caches glyphs, icons and application snapshots on the CPU, while
Vulkan evaluates spatial geometry, lighting and interaction appearance into the
two eye swapchains. DMA-BUF and SDK build closure on clean target distributions
remain separate milestones.

- `taskbar.v1`: System UI sends `TaskbarIntent{mode:fixed|follow}` and receives
  `TaskbarApplied{mode}`. The Shell starts fixed, freezes its current rigid
  taskbar pose on fixed, and smoothly resumes shared following on follow.
  Snapshots expose `taskbar_mode`; System UI persists acknowledged choices
  and restores them at startup. Independent world-locked panels stay fixed.

- `scene.spherical-placement.v1` adds `yaw_deg`, `pitch_deg`, `distance_m` and
  `user_facing_billboard` to horizon-spawn Groups. Distances are 0.6–4 m;
  panel centres use yaw ±35° and pitch −25°..+10°. Shell resolves one frozen
  frame from a valid binocular midpoint and its horizontal heading. Older
  horizon offsets remain supported. Kit's `Widget::horizon_panel` emits the
  explicit form; clients must negotiate both horizon and spherical capabilities.
- Controller rays have independent pointer IDs 2/3, hover and capture slots.
  Two hands can grab one window to scale it; the legacy Wayland seat retains a
  single application pointer-button owner. Cancelling one spatial pointer does
  not release another pointer's capture. In debug sessions, ray parameters may
  specify `source=debug_ray:left|debug_ray:right|mouse` and a bounded
  `depth_delta_m`. These are developer parameters, not tracked controller proof.

Spatial backgrounds, rounded geometry, slider tracks, optical lighting and
hover/press appearance are evaluated by Vulkan. Cairo caches glyphs/icons and
application snapshots; hover/press alone reuse the same bitmap. Opaque pixel
cores establish depth before sorted translucent UI. Both eyes are recorded in
one batch, with one completion wait before XR release. Per-frame resource rings,
order-independent transparency and native composition-layer promotion remain
separate work.

Buttons inside a `head_horizon_spawn` panel may use the SDK constants
`PANEL_MOVE_ACTION` and `PANEL_SCALE_ACTION`. Shell consumes these actions
without emitting client InputEvents. Hold the primary trigger on the move handle
to translate the panel at the captured ray distance, facing the current eye;
hold it on the scale handle and drag outward/inward for uniform scaling about
the panel center. A gesture permits 0.65 to 1.8 times its starting size. Logical
key dimensions remain unchanged, and rendering and hit tests share the adjusted
frame. Release commits, Cancel restores the starting frame, and a second handle
capture on that panel is ignored. Scene updates retain the frame; removal or
recenter resets it through the normal horizon spawn policy.
