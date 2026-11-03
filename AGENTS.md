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
