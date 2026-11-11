// SPDX-License-Identifier: GPL-3.0-or-later
use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=GLSLANG_VALIDATOR");
    let compiler = env::var_os("GLSLANG_VALIDATOR").unwrap_or_else(|| "glslangValidator".into());
    for name in [
        "passthrough.comp",
        "panorama.vert",
        "panorama.frag",
        "spatial.vert",
        "spatial.frag",
        "controller.vert",
        "controller.frag",
    ] {
        let input = PathBuf::from("shaders").join(name);
        let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(format!("{name}.spv"));
        println!("cargo:rerun-if-changed={}", input.display());
        let status = Command::new(&compiler)
            .args(["-V", "--target-env", "vulkan1.1", "-o"])
            .arg(output)
            .arg(input)
            .status()
            .expect("run glslangValidator");
        assert!(status.success(), "shader compilation failed");
    }
}
