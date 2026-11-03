// SPDX-License-Identifier: GPL-3.0-or-later
//! Immutable Qingkong art crops. Insets use source pixels, including our gutter.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Cairo-compatible premultiplied BGRA, with a transparent sampling gutter.
    pub bgra: Vec<u8>,
    /// Left, top, right, bottom; zero means scale the complete overlay or icon.
    pub insets: [u32; 4],
}

#[derive(Deserialize)]
struct Region {
    file: String,
    rect_xyxy: [u32; 4],
    insets_ltrb: [u32; 4],
}

#[derive(Deserialize)]
struct Regions {
    schema: u32,
    runtime_padding_px: u32,
    alpha_cutoff_inclusive: u8,
    materials: BTreeMap<String, Region>,
    icons: BTreeMap<String, Region>,
}

struct Decoded {
    width: u32,
    height: u32,
    channels: usize,
    pixels: Vec<u8>,
}

#[derive(Default)]
struct Library {
    materials: BTreeMap<String, Bitmap>,
    icons: BTreeMap<String, Bitmap>,
}

static LIBRARY: OnceLock<Library> = OnceLock::new();

pub fn material(name: &str) -> Option<&'static Bitmap> {
    LIBRARY.get_or_init(load_library).materials.get(name)
}

pub fn icon(id: &str) -> Option<&'static Bitmap> {
    LIBRARY.get_or_init(load_library).icons.get(id)
}

fn directories() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("XR_SPATIAL_ART_DIR").filter(|p| !p.is_empty()) {
        paths.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(Path::parent) {
            paths.push(prefix.join("share/xr/art/qingkong"));
        }
    }
    paths.push(PathBuf::from("/usr/share/xr/art/qingkong"));
    let mut unique = Vec::new();
    for path in paths {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    unique
}

fn load_regions(path: &Path) -> Result<Regions> {
    let regions: Regions = serde_json::from_reader(BufReader::new(
        File::open(path).context("open regions.json")?,
    ))
    .context("decode regions.json")?;
    ensure!(regions.schema == 1, "unsupported art region schema");
    ensure!(regions.runtime_padding_px == 2, "expected two-pixel gutter");
    ensure!(
        regions.alpha_cutoff_inclusive == 8,
        "expected alpha cutoff 8"
    );
    Ok(regions)
}

fn decode(path: &Path) -> Result<Decoded> {
    let mut decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info()?;
    ensure!(
        reader.info().width > 0
            && reader.info().height > 0
            && reader.info().width <= 8192
            && reader.info().height <= 8192,
        "invalid art PNG dimensions"
    );
    let mut pixels = vec![0; reader.output_buffer_size()];
    let output = reader.next_frame(&mut pixels)?;
    ensure!(
        output.bit_depth == png::BitDepth::Eight,
        "art PNG must be 8-bit"
    );
    let channels = match output.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        _ => anyhow::bail!("art PNG must be RGB or RGBA"),
    };
    pixels.truncate(output.buffer_size());
    ensure!(
        pixels.len() == output.width as usize * output.height as usize * channels,
        "invalid art PNG pixel length"
    );
    Ok(Decoded {
        width: output.width,
        height: output.height,
        channels,
        pixels,
    })
}

fn crop(image: &Decoded, region: &Region) -> Result<Bitmap> {
    let [x0, y0, x1, y1] = region.rect_xyxy;
    ensure!(
        x1 > x0 && y1 > y0 && x1 <= image.width && y1 <= image.height,
        "art crop exceeds PNG bounds"
    );
    let source_width = x1 - x0;
    let source_height = y1 - y0;
    let [left, top, right, bottom] = region.insets_ltrb;
    ensure!(
        u64::from(left) + u64::from(right) < u64::from(source_width)
            && u64::from(top) + u64::from(bottom) < u64::from(source_height),
        "art cap insets leave no centre"
    );
    let width = source_width + 4;
    let height = source_height + 4;
    let mut bgra = vec![0; width as usize * height as usize * 4];
    for y in 0..source_height {
        for x in 0..source_width {
            let source =
                ((y0 + y) as usize * image.width as usize + (x0 + x) as usize) * image.channels;
            let pixel = &image.pixels[source..source + image.channels];
            let alpha = if image.channels == 4 { pixel[3] } else { 255 };
            if alpha <= 8 {
                continue;
            }
            let target = ((y + 2) as usize * width as usize + (x + 2) as usize) * 4;
            for (channel, value) in [pixel[2], pixel[1], pixel[0]].into_iter().enumerate() {
                bgra[target + channel] = ((u32::from(value) * u32::from(alpha) + 127) / 255) as u8;
            }
            bgra[target + 3] = alpha;
        }
    }
    let insets = if region.insets_ltrb == [0; 4] {
        [0; 4]
    } else {
        region.insets_ltrb.map(|v| v + 2)
    };
    Ok(Bitmap {
        width,
        height,
        bgra,
        insets,
    })
}

fn load_library() -> Library {
    let mut errors = Vec::new();
    let mut chosen = None;
    for directory in directories() {
        match load_regions(&directory.join("regions.json")) {
            Ok(regions) => {
                chosen = Some((directory, regions));
                break;
            }
            Err(error) => errors.push(format!("{}: {error:#}", directory.display())),
        }
    }
    let Some((directory, regions)) = chosen else {
        eprintln!(
            "XR_ART_FALLBACK library=qingkong reason={} fallback=procedural",
            errors.join("; ")
        );
        return Library::default();
    };
    if !errors.is_empty() {
        eprintln!(
            "XR_ART_FALLBACK selected={} skipped={} fallback=next-art-directory",
            directory.display(),
            errors.join("; ")
        );
    }
    let mut library = Library::default();
    let mut decoded: BTreeMap<String, Option<Decoded>> = BTreeMap::new();
    for (kind, source, target) in [
        ("material", &regions.materials, &mut library.materials),
        ("icon", &regions.icons, &mut library.icons),
    ] {
        for (name, region) in source {
            let image = decoded.entry(region.file.clone()).or_insert_with(|| {
                match decode(&directory.join(&region.file)) {
                    Ok(image) => Some(image),
                    Err(error) => {
                        eprintln!(
                            "XR_ART_FALLBACK file={} reason={error:#} fallback=procedural",
                            directory.join(&region.file).display()
                        );
                        None
                    }
                }
            });
            let Some(image) = image else { continue };
            match crop(image, region) {
                Ok(bitmap) => {
                    println!(
                        "XR_ART_RESOURCE kind={kind} id={name} file={} rect={:?} size={}x{} insets={:?} alpha_cutoff=8 gutter=2",
                        region.file, region.rect_xyxy, bitmap.width, bitmap.height, bitmap.insets
                    );
                    target.insert(name.clone(), bitmap);
                }
                Err(error) => {
                    eprintln!(
                        "XR_ART_FALLBACK kind={kind} id={name} reason={error:#} fallback=procedural"
                    );
                }
            }
        }
    }
    println!(
        "XR_ART_LIBRARY directory={} materials={} icons={} format=premultiplied-BGRA",
        directory.display(),
        library.materials.len(),
        library.icons.len()
    );
    library
}
