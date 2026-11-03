# xr-shell

Work-in-progress Rust OpenXR Shell for UKUI XR.

This checkpoint preserves initial environment loading, Vulkan resource and
panorama rendering modules, shaders and Cargo.lock. The executable entry point
is still a placeholder and the modules are not yet wired into an application.
It has not been compiled or run as an XR Shell. It must not be packaged or
reported as a working desktop.

The implementation plan is maintained in openkylin-xr-docs. The first milestone
is actual PRIMARY_STEREO rendering with the versioned Qingkong environment,
head-pose adjustment and clean OpenXR/Vulkan/session shutdown in QEMU.

Build inputs will use the locked Cargo dependencies and a separately supplied
glslangValidator. Artwork is consumed from xr-environments, not copied here.
