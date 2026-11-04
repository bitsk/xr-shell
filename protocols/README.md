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

The renderer currently rasterises Kit primitives and application snapshots
through an explicit CPU path before Vulkan samples spatial planes into the
runtime's two eye swapchains. DMA-BUF, optimized retained rendering and SDK
build closure on clean target distributions remain separate milestones.

- `taskbar.v1`: System UI sends `TaskbarIntent{mode:fixed|follow}` and receives
  `TaskbarApplied{mode}`. The Shell starts fixed, freezes its current rigid
  taskbar pose on fixed, and smoothly resumes shared following on follow.
  Snapshots expose `taskbar_mode`; System UI persists acknowledged choices
  and restores them at startup. Independent world-locked panels stay fixed.
