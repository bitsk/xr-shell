// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, io::{BufReader, Cursor}, path::{Component, Path}};

#[derive(Deserialize)]
struct Manifest {
    schema: String,
    schema_version: u32,
    id: String,
    kind: String,
    texture: Texture,
    space: Space,
}
#[derive(Deserialize)]
struct Texture {
    file: String, sha256: String, width: u32, height: u32,
    colour_space: String, projection: String, uv_origin: String,
    front_uv: [f32; 2], right_uv: [f32; 2], sampler_u: String, sampler_v: String,
}
#[derive(Deserialize)]
struct Space {
    handedness: String, up: String, front: String, right: String,
    yaw_offset_radians: f32, ignore_view_translation: bool,
    lock_to_view_rotation: bool, stereo_depth: String,
}

pub struct Environment {
    pub id: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub yaw: f32,
}

impl Environment {
    pub fn load(path: &Path) -> Result<Self> {
        let m: Manifest = serde_json::from_slice(&fs::read(path).context("read environment manifest")?)?;
        ensure!(m.schema == "org.openkylin.xr.environment" && m.schema_version == 1,
                "unsupported environment schema");
        let t = m.texture;
        ensure!(m.kind == "infinite_panorama" && t.projection == "equirectangular"
            && t.colour_space == "srgb" && t.uv_origin == "top_left"
            && t.front_uv == [0.5, 0.5] && t.right_uv == [0.75, 0.5]
            && t.sampler_u == "repeat" && t.sampler_v == "clamp_to_edge",
            "unsupported panorama projection or colour space");
        let s = m.space;
        ensure!(s.handedness == "right" && s.up == "+Y" && s.front == "-Z" && s.right == "+X"
            && s.ignore_view_translation && !s.lock_to_view_rotation && s.stereo_depth == "infinite"
            && s.yaw_offset_radians.is_finite(), "unsupported environment space");
        ensure!(Path::new(&t.file).components().all(|x| matches!(x, Component::Normal(_))),
                "texture must be relative to the manifest");
        let bytes = fs::read(path.parent().context("manifest parent")?.join(&t.file))?;
        let hash = format!("{:x}", Sha256::digest(&bytes));
        ensure!(hash == t.sha256, "panorama SHA-256 mismatch");
        ensure!(t.width > 0 && t.width <= 16384 && t.height > 0 && t.height <= 8192
            && t.width == 2 * t.height, "invalid panorama dimensions");
        let mut decoder = png::Decoder::new(BufReader::new(Cursor::new(bytes)));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info()?;
        ensure!(reader.info().width == t.width && reader.info().height == t.height,
                "PNG and manifest dimensions disagree");
        let mut decoded = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut decoded)?;
        let data = &decoded[..info.buffer_size()];
        let rgba = match info.color_type {
            png::ColorType::Rgba => data.to_vec(),
            png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
            _ => anyhow::bail!("environment PNG must be RGB or RGBA"),
        };
        Ok(Self { id: m.id, sha256: hash, width: t.width, height: t.height,
                  rgba, yaw: s.yaw_offset_radians })
    }
}
