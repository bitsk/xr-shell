// SPDX-License-Identifier: GPL-3.0-or-later
//! Read QSP1 snapshots from the existing hardware owner. This module never
//! opens V4L2, changes a sensor, or depends on the debug server or browser.
use crate::gpu::{Buffer, Gpu};
use crate::renderer::{colour_layers, colour_range, image_barrier};
use anyhow::{Context, Result, ensure};
use ash::vk;
use serde::Deserialize;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::fd::AsRawFd,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const WIDTH: usize = 964;
const HEIGHT: usize = 544;
const MAX_AGE_NS: u64 = 150_000_000;
fn now_ns() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) } != 0 {
        return 0;
    }
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}
#[derive(Deserialize)]
struct EyeCalibration {
    slot: u32,
    intrinsics: [f32; 4],
    distortion: [f32; 4],
    camera_from_view: [[f32; 3]; 3],
}
#[derive(Deserialize)]
struct Config {
    schema: String,
    version: u32,
    directory: PathBuf,
    source_size: [u32; 2],
    eyes: [EyeCalibration; 2],
    #[serde(default = "unit_gain")]
    colour_gain: [f32; 3],
}
fn unit_gain() -> [f32; 3] {
    [1.; 3]
}
struct Frame {
    generation: u64,
    sequence: u64,
    received: u64,
    rgba: Vec<u8>,
}
struct Stereo {
    received: [u64; 2],
    rgba: Vec<u8>,
}
fn snapshot(directory: &Path, slot: u32, previous: Option<&Frame>) -> Option<Frame> {
    let mut selected: Option<Frame> = None;
    for index in 0..3 {
        let mut f = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join(format!("slot{slot}.{index}.raw")))
        {
            Ok(f) => f,
            Err(_) => continue,
        };
        let meta = f.metadata().ok()?;
        if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            continue;
        }
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
            continue;
        }
        let read = (|| {
            let mut header = [0u8; 64];
            f.read_exact(&mut header).ok()?;
            let word = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
            let wide = |offset| u64::from_le_bytes(header[offset..offset + 8].try_into().unwrap());
            let (stride, bytes) = (word(16) as usize, word(28) as usize);
            let (generation, sequence, received) = (wide(32), wide(40), wide(48));
            // QSP1 header: generation, sequence, received are at 32, 40, 48;
            // the final eight bytes are reserved, not a timestamp.
            if word(0) != 0x31505351
                || word(4) != 1
                || word(8) != 1928
                || word(12) != 1088
                || word(20) != u32::from_le_bytes(*b"pRAA")
                || word(24) != slot
                || !(2410..=8192).contains(&stride)
                || bytes != stride * 1088
                || meta.len() != 64 + bytes as u64
                || received > now_ns()
                || now_ns().saturating_sub(received) >= MAX_AGE_NS
            {
                return None;
            }
            if previous.is_some_and(|p| p.generation == generation && p.sequence >= sequence)
                || selected.as_ref().is_some_and(|p| p.received >= received)
            {
                return None;
            }
            let mut raw = vec![0u8; bytes];
            f.seek(SeekFrom::Start(64)).ok()?;
            f.read_exact(&mut raw).ok()?;
            let mut rgba = vec![0u8; WIDTH * HEIGHT * 4];
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let source = (x * 2 / 4) * 5 + (x * 2 % 4);
                    let top = y * 2 * stride + source;
                    let bottom = top + stride;
                    let dest = (y * WIDTH + x) * 4;
                    // A 2x2 RGGB Bayer cell. Values remain linear RAW; the
                    // UNORM image is converted to sRGB by the output target.
                    rgba[dest] = raw[top];
                    rgba[dest + 1] = ((raw[top + 1] as u16 + raw[bottom] as u16) / 2) as u8;
                    rgba[dest + 2] = raw[bottom + 1];
                    rgba[dest + 3] = 255;
                }
            }
            Some(Frame {
                generation,
                sequence,
                received,
                rgba,
            })
        })();
        unsafe {
            libc::flock(f.as_raw_fd(), libc::LOCK_UN);
        }
        if let Some(frame) = read {
            selected = Some(frame);
        }
    }
    selected
}

pub struct Passthrough {
    gpu: Rc<Gpu>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    sampler: vk::Sampler,
    staging: Buffer,
    _calibration: Buffer,
    latest: Arc<Mutex<Option<Stereo>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    received: [u64; 2],
    initialized: bool,
    uploaded_frames: u64,
}
impl Passthrough {
    pub fn from_environment(gpu: Rc<Gpu>, descriptor: vk::DescriptorSet) -> Result<Option<Self>> {
        let Some(path) = std::env::var_os("XR_PASSTHROUGH_CONFIG") else {
            return Ok(None);
        };
        let config: Config =
            serde_json::from_reader(File::open(path).context("open passthrough configuration")?)?;
        ensure!(
            config.schema == "org.openkylin.xr.passthrough"
                && config.version == 1
                && config.source_size == [1928, 1088]
                && config.directory.is_absolute()
                && config.eyes[0].slot == 5
                && config.eyes[1].slot == 4,
            "unsupported passthrough configuration"
        );
        let mut coefficients = Vec::<f32>::new();
        for eye in &config.eyes {
            ensure!(
                eye.intrinsics
                    .iter()
                    .chain(eye.distortion.iter())
                    .all(|x| x.is_finite())
                    && eye.intrinsics[0] > 0.
                    && eye.intrinsics[1] > 0.,
                "invalid camera intrinsics"
            );
            coefficients.extend(eye.intrinsics);
            coefficients.extend(eye.distortion);
            for row in eye.camera_from_view {
                ensure!(row.iter().all(|x| x.is_finite()), "invalid camera rotation");
                coefficients.extend([row[0], row[1], row[2], 0.]);
            }
        }
        ensure!(
            config
                .colour_gain
                .iter()
                .all(|x| x.is_finite() && *x > 0. && *x <= 16.),
            "invalid colour gain"
        );
        coefficients.extend([
            config.colour_gain[0],
            config.colour_gain[1],
            config.colour_gain[2],
            0.,
        ]);
        let staging = Buffer::host(
            gpu.clone(),
            (WIDTH * 2 * HEIGHT * 4) as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
        )?;
        let calibration = Buffer::host(
            gpu.clone(),
            (coefficients.len() * 4) as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?;
        calibration.write(unsafe {
            std::slice::from_raw_parts(coefficients.as_ptr() as *const u8, coefficients.len() * 4)
        })?;
        let mut p = Self {
            gpu,
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            sampler: vk::Sampler::null(),
            staging,
            _calibration: calibration,
            latest: Arc::new(Mutex::new(Some(Stereo {
                received: [0; 2],
                rgba: vec![0u8; WIDTH * 2 * HEIGHT * 4],
            }))),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            received: [0; 2],
            initialized: false,
            uploaded_frames: 0,
        };
        unsafe {
            let d = &p.gpu.device;
            p.image = d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .extent(vk::Extent3D {
                        width: (WIDTH * 2) as u32,
                        height: HEIGHT as u32,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST),
                None,
            )?;
            let req = d.get_image_memory_requirements(p.image);
            p.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(p.gpu.memory_type(
                        req.memory_type_bits,
                        vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    )?),
                None,
            )?;
            d.bind_image_memory(p.image, p.memory, 0)?;
            p.view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(p.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .subresource_range(colour_range()),
                None,
            )?;
            p.sampler = d.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )?;
            d.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor)
                        .dst_binding(3)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&[vk::DescriptorImageInfo::default()
                            .sampler(p.sampler)
                            .image_view(p.view)
                            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]),
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor)
                        .dst_binding(4)
                        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                        .buffer_info(&[vk::DescriptorBufferInfo::default()
                            .buffer(p._calibration.handle)
                            .range(p._calibration.size)]),
                ],
                &[],
            );
        }
        let latest = p.latest.clone();
        let stop = p.stop.clone();
        p.worker = Some(thread::spawn(move || {
            let mut frames: [Option<Frame>; 2] = [None, None];
            while !stop.load(Ordering::Relaxed) {
                let mut changed = false;
                for eye in 0..2 {
                    if let Some(f) = snapshot(
                        &config.directory,
                        config.eyes[eye].slot,
                        frames[eye].as_ref(),
                    ) {
                        frames[eye] = Some(f);
                        changed = true;
                    }
                }
                if changed
                    && frames.iter().all(|f| {
                        f.as_ref()
                            .is_some_and(|f| now_ns().saturating_sub(f.received) < MAX_AGE_NS)
                    })
                {
                    let mut rgba = vec![0u8; WIDTH * 2 * HEIGHT * 4];
                    for (eye, frame) in frames.iter().enumerate() {
                        let frame = frame.as_ref().unwrap();
                        for y in 0..HEIGHT {
                            let source = &frame.rgba[y * WIDTH * 4..(y + 1) * WIDTH * 4];
                            let target = (y * WIDTH * 2 + eye * WIDTH) * 4;
                            rgba[target..target + WIDTH * 4].copy_from_slice(source);
                        }
                    }
                    if let Ok(mut target) = latest.lock() {
                        *target = Some(Stereo {
                            received: [
                                frames[0].as_ref().unwrap().received,
                                frames[1].as_ref().unwrap().received,
                            ],
                            rgba,
                        });
                    }
                }
                thread::sleep(Duration::from_millis(8));
            }
        }));
        eprintln!(
            "XR_PASSTHROUGH=enabled SOURCE=QSP1 SIZE=964x544 STEREO=left5,right4 COLOUR=raw-linear GEOMETRY=rotation-only"
        );
        Ok(Some(p))
    }
    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({"source":"QSP1", "size_per_eye":[WIDTH,HEIGHT], "slots_left_right":[5,4],
            "active":self.active(), "uploaded_frames":self.uploaded_frames,
            "age_ms":self.received.map(|t| if t==0 {None} else {Some(now_ns().saturating_sub(t) as f64/1e6)}),
            "pair_dq_delta_ms":self.received[0].abs_diff(self.received[1]) as f64/1e6,
            "exposure_sync_verified":false,"physical_extrinsics_verified":false,"colour_calibrated":false})
    }
    pub fn active(&self) -> bool {
        self.initialized
            && self
                .received
                .iter()
                .all(|t| *t > 0 && now_ns().saturating_sub(*t) < MAX_AGE_NS)
    }
    pub fn prepare(&mut self, cmd: vk::CommandBuffer) -> Result<()> {
        let next = match self.latest.try_lock() {
            Ok(mut f) => f.take(),
            Err(_) => None,
        };
        if let Some(frame) = next {
            self.staging.write(&frame.rgba)?;
            unsafe {
                let d = &self.gpu.device;
                let (old, source, stage) = if self.initialized {
                    (
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::AccessFlags::SHADER_READ,
                        vk::PipelineStageFlags::FRAGMENT_SHADER,
                    )
                } else {
                    (
                        vk::ImageLayout::UNDEFINED,
                        vk::AccessFlags::empty(),
                        vk::PipelineStageFlags::TOP_OF_PIPE,
                    )
                };
                d.cmd_pipeline_barrier(
                    cmd,
                    stage,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier(
                        self.image,
                        old,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        source,
                        vk::AccessFlags::TRANSFER_WRITE,
                    )],
                );
                d.cmd_copy_buffer_to_image(
                    cmd,
                    self.staging.handle,
                    self.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(colour_layers())
                        .image_extent(vk::Extent3D {
                            width: (WIDTH * 2) as u32,
                            height: HEIGHT as u32,
                            depth: 1,
                        })],
                );
                d.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier(
                        self.image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::SHADER_READ,
                    )],
                );
            }
            self.received = frame.received;
            self.initialized = true;
            if self.received.iter().all(|t| *t > 0) {
                self.uploaded_frames += 1;
            }
        }
        Ok(())
    }
}
impl Drop for Passthrough {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        unsafe {
            let d = &self.gpu.device;
            d.destroy_sampler(self.sampler, None);
            d.destroy_image_view(self.view, None);
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
        }
    }
}
