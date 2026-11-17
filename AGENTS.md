# Rust XR Shell

Maintain the Rust OpenXR Shell, Vulkan renderer and component protocol here.
Cross-repository design belongs in openkylin-xr-docs; environment artwork is
consumed from xr-environments. Keep source, Cargo.lock and shader inputs
reproducible. Do not copy runtime, kernel or artwork source into this repo.

Preserve the existing UKUI session and use isolated development profiles.
QEMU, package and physical-device acceptance are separate. Installing a package
must not launch XR or replace the active OpenXR runtime. Keep public files free
of private paths, LAN addresses and credentials. Preserve contributor identity
and real dates. Do not create provenance directories.

## XR Spatial Engineering Constraints

Follow `openkylin-xr-docs/wiki/xr-spatial-guidelines-and-issues.md` (ISSUE-XR-01..14, AUDIT-01..06, and Part 3 Controller Spec):
- **User-Facing Orientation & View-Relative Spawn**: Never lock windows or panels to `rotation = [0, 0, 0]` on a flat world XY wall. Compute yaw/pitch toward the binocular center-eye pose (keep roll = 0) and spawn new windows/panels relative to the current horizon-projected head yaw and eye height without co-planar overlap.
- **6DoF Spherical Manipulation & Full Controller Semantics**:
  - **Trigger (`>= 0.65` press, `<= 0.35` release Schmitt hysteresis)**: Primary ray click/drag (`BTN_LEFT = 272`) and UI activation.
  - **Squeeze / Grip (`>= 0.60` press, `<= 0.35` release)**: Direct 6DoF window grab (`Target::Move`) and bimanual scaling from anywhere on a window surface without emitting accidental left-clicks into the client app.
  - **Right-Click (`BTN_RIGHT = 273`) & Middle-Click (`BTN_MIDDLE = 274`)**: Map secondary controller buttons (`A/X`) or stationary trigger long-press (`650 ms`, `< 10 px` drift) to `BTN_RIGHT`, and thumbstick click to `BTN_MIDDLE` / distance reset.
  - **Spatial Double-Click Stabilizer**: Clamp the second click's surface coordinates to the first click's position when a repeat press occurs within `420 ms` and `< 1.5°` (`< 24 px`) ray jitter.
  - **Thumbstick Curve & Hovered Scroll**: Apply a quadratic precision curve outside the `0.18` deadzone; scroll the currently hovered window/panel first (or adjust depth `0.6..4.0 m` while grabbing).
  - **Same-Plane Hit Priority**: Direct geometry hits (`inside_rounded`) must strictly take precedence over expanded `2.2°` hit-slop when ray distances `t` are equal.
- **Spatial Optics & Anti-Aliasing**: Never sample the skybox panorama through glass panels to fake refraction over behind-panel windows. Use binocular view-dependent specular/Fresnel highlights, surface-normal-aligned reticles, mipmapped/anisotropic UI & window textures, and GPU SDF state transitions (keep CPU Cairo strictly for glyph/icon caching).
