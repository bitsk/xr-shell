// SPDX-License-Identifier: GPL-3.0-or-later
//! Device artwork is installed separately; all poses come from OpenXR actions.
use crate::{
    gpu::{Buffer, Gpu},
    input::HandState,
};
use anyhow::{Context, Result, ensure};
use ash::vk;
use openxr as xr;
use serde::Deserialize;
use std::{
    io::{BufReader, Cursor},
    path::Path,
    rc::Rc,
};

#[derive(Deserialize)]
struct Mesh {
    vertices: Vec<[f32; 8]>,
}

pub struct Model {
    gpu: Rc<Gpu>,
    vertices: Option<Buffer>,
    beam: Option<Buffer>,
    count: u32,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    sampler: vk::Sampler,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor: vk::DescriptorSet,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    upload_pool: vk::CommandPool,
    upload_fence: vk::Fence,
    modules: Vec<vk::ShaderModule>,
    pub directory: String,
}

impl Model {
    pub fn load(gpu: Rc<Gpu>, pass: vk::RenderPass, directory: &Path) -> Result<Self> {
        let mesh: Mesh = serde_json::from_reader(BufReader::new(std::fs::File::open(
            directory.join("mesh.json"),
        )?))?;
        ensure!(
            !mesh.vertices.is_empty()
                && mesh.vertices.len() % 3 == 0
                && mesh.vertices.len() <= 3_000_000,
            "invalid triangle count"
        );
        ensure!(
            mesh.vertices.iter().flatten().all(|v| v.is_finite()),
            "nonfinite mesh vertex"
        );
        let mut decoder = png::Decoder::new(BufReader::new(std::fs::File::open(
            directory.join("texture.png"),
        )?));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info()?;
        ensure!(
            reader.info().width <= 8192 && reader.info().height <= 8192,
            "controller texture too large"
        );
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels)?;
        pixels.truncate(info.buffer_size());
        let pixels = match info.color_type {
            png::ColorType::Rgba => pixels,
            png::ColorType::Rgb => pixels
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            _ => anyhow::bail!("controller texture must be RGB or RGBA"),
        };
        let mut m = Self {
            gpu,
            vertices: None,
            beam: None,
            count: mesh.vertices.len() as u32,
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            sampler: vk::Sampler::null(),
            descriptor_layout: vk::DescriptorSetLayout::null(),
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor: vk::DescriptorSet::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            upload_pool: vk::CommandPool::null(),
            upload_fence: vk::Fence::null(),
            modules: Vec::new(),
            directory: directory.display().to_string(),
        };
        let buffer = Buffer::host(
            m.gpu.clone(),
            (mesh.vertices.len() * 32) as u64,
            vk::BufferUsageFlags::VERTEX_BUFFER,
        )?;
        buffer.write(bytes(&mesh.vertices))?;
        m.vertices = Some(buffer);
        // Two crossing thin surfaces keep the beam visible from either eye.
        let mut beam = Vec::new();
        for axis in 0..2 {
            let near = if axis == 0 {
                [0.0008, 0., 0.]
            } else {
                [0., 0.0008, 0.]
            };
            let points = [
                [near[0], near[1], -0.025],
                [-near[0], -near[1], -0.025],
                [-near[0], -near[1], -1.5],
                [near[0], near[1], -1.5],
            ];
            for i in [0, 1, 2, 0, 2, 3] {
                let p = points[i];
                beam.push([p[0], p[1], p[2], 0., 1., 0., 0., 0.]);
            }
        }
        let buffer = Buffer::host(
            m.gpu.clone(),
            (beam.len() * 32) as u64,
            vk::BufferUsageFlags::VERTEX_BUFFER,
        )?;
        buffer.write(bytes(&beam))?;
        m.beam = Some(buffer);
        m.upload(&pixels, info.width, info.height)?;
        m.pipeline(pass)?;
        eprintln!(
            "XR_CONTROLLER_MODEL directory={} vertices={} texture={}x{}",
            m.directory, m.count, info.width, info.height
        );
        Ok(m)
    }

    fn upload(&mut self, pixels: &[u8], width: u32, height: u32) -> Result<()> {
        let staging = Buffer::host(
            self.gpu.clone(),
            pixels.len() as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
        )?;
        staging.write(pixels)?;
        unsafe {
            let d = &self.gpu.device;
            let extent = vk::Extent3D {
                width,
                height,
                depth: 1,
            };
            self.image = d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .extent(extent)
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?;
            let req = d.get_image_memory_requirements(self.image);
            self.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(self.gpu.memory_type(
                        req.memory_type_bits,
                        vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    )?),
                None,
            )?;
            d.bind_image_memory(self.image, self.memory, 0)?;
            self.upload_pool = d.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(self.gpu.family),
                None,
            )?;
            self.upload_fence = d.create_fence(&vk::FenceCreateInfo::default(), None)?;
            let cmd = d.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.upload_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            d.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let range = vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1);
            let barrier = vk::ImageMemoryBarrier::default()
                .image(self.image)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(range);
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            d.cmd_copy_buffer_to_image(
                cmd,
                staging.handle,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(extent)],
            );
            let barrier = barrier
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ);
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            d.end_command_buffer(cmd)?;
            d.queue_submit(
                self.gpu.queue,
                &[vk::SubmitInfo::default().command_buffers(&[cmd])],
                self.upload_fence,
            )?;
            d.wait_for_fences(&[self.upload_fence], true, u64::MAX)?;
            self.view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(self.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .subresource_range(range),
                None,
            )?;
            self.sampler = d.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::REPEAT)
                    .address_mode_v(vk::SamplerAddressMode::REPEAT)
                    .max_lod(0.),
                None,
            )?;
            self.descriptor_layout = d.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                ]),
                None,
            )?;
            self.descriptor_pool = d.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(1)
                    .pool_sizes(&[vk::DescriptorPoolSize {
                        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        descriptor_count: 1,
                    }]),
                None,
            )?;
            self.descriptor = d.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.descriptor_pool)
                    .set_layouts(&[self.descriptor_layout]),
            )?[0];
            d.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(self.descriptor)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&[vk::DescriptorImageInfo::default()
                        .sampler(self.sampler)
                        .image_view(self.view)
                        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)])],
                &[],
            );
        }
        Ok(())
    }

    fn pipeline(&mut self, pass: vk::RenderPass) -> Result<()> {
        unsafe {
            let d = &self.gpu.device;
            for source in [
                include_bytes!(concat!(env!("OUT_DIR"), "/controller.vert.spv")).as_slice(),
                include_bytes!(concat!(env!("OUT_DIR"), "/controller.frag.spv")).as_slice(),
            ] {
                let code = ash::util::read_spv(&mut Cursor::new(source))?;
                self.modules.push(d.create_shader_module(
                    &vk::ShaderModuleCreateInfo::default().code(&code),
                    None,
                )?);
            }
            let stages = [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(self.modules[0])
                    .name(c"main"),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(self.modules[1])
                    .name(c"main"),
            ];
            let bindings = [vk::VertexInputBindingDescription {
                binding: 0,
                stride: 32,
                input_rate: vk::VertexInputRate::VERTEX,
            }];
            let attrs = [
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32B32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 0,
                    format: vk::Format::R32G32B32_SFLOAT,
                    offset: 12,
                },
                vk::VertexInputAttributeDescription {
                    location: 2,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 24,
                },
            ];
            let vertex = vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&bindings)
                .vertex_attribute_descriptions(&attrs);
            let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
            let viewport = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);
            let raster = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .line_width(1.);
            let multi = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let depth = vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(true)
                .depth_write_enable(true)
                .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
            let attachments = [vk::PipelineColorBlendAttachmentState::default()
                .color_write_mask(vk::ColorComponentFlags::RGBA)];
            let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
            let states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&states);
            self.layout = d.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&[self.descriptor_layout])
                    .push_constant_ranges(&[vk::PushConstantRange {
                        stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        offset: 0,
                        size: 96,
                    }]),
                None,
            )?;
            let create = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertex)
                .input_assembly_state(&assembly)
                .viewport_state(&viewport)
                .rasterization_state(&raster)
                .multisample_state(&multi)
                .depth_stencil_state(&depth)
                .color_blend_state(&blend)
                .dynamic_state(&dynamic)
                .layout(self.layout)
                .render_pass(pass);
            self.pipeline =
                match d.create_graphics_pipelines(vk::PipelineCache::null(), &[create], None) {
                    Ok(p) => p[0],
                    Err((partial, error)) => {
                        for p in partial {
                            d.destroy_pipeline(p, None);
                        }
                        return Err(error.into());
                    }
                };
        }
        Ok(())
    }

    pub unsafe fn draw(&self, cmd: vk::CommandBuffer, view: &xr::View, state: &HandState) {
        unsafe {
            let d = &self.gpu.device;
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            d.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[self.descriptor],
                &[],
            );
            for (pose, beam) in [(state.grip, false), (state.aim, true)] {
                let Some(pose) = pose else {
                    continue;
                };
                let q = view.pose.orientation;
                let p = view.pose.position;
                let mq = pose.orientation;
                let mp = pose.position;
                let constants = [
                    view.fov.angle_left.tan(),
                    view.fov.angle_right.tan(),
                    view.fov.angle_up.tan(),
                    view.fov.angle_down.tan(),
                    q.x,
                    q.y,
                    q.z,
                    q.w,
                    p.x,
                    p.y,
                    p.z,
                    0.,
                    mq.x,
                    mq.y,
                    mq.z,
                    mq.w,
                    mp.x,
                    mp.y,
                    mp.z,
                    if beam { 1. } else { 0. },
                    if state.select { 1. } else { 0. },
                    0.,
                    0.,
                    0.,
                ];
                d.cmd_push_constants(
                    cmd,
                    self.layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    bytes(&constants),
                );
                let buffer = if beam {
                    self.beam.as_ref()
                } else {
                    self.vertices.as_ref()
                }
                .unwrap();
                d.cmd_bind_vertex_buffers(cmd, 0, &[buffer.handle], &[0]);
                d.cmd_draw(cmd, if beam { 12 } else { self.count }, 1, 0, 0);
            }
        }
    }
}

fn bytes<T>(slice: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast(), std::mem::size_of_val(slice)) }
}
impl Drop for Model {
    fn drop(&mut self) {
        unsafe {
            let d = &self.gpu.device;
            // Also covers an interrupted upload before the model becomes visible.
            let _ = d.device_wait_idle();
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.layout, None);
            for module in self.modules.drain(..) {
                d.destroy_shader_module(module, None);
            }
            d.destroy_descriptor_pool(self.descriptor_pool, None);
            d.destroy_descriptor_set_layout(self.descriptor_layout, None);
            d.destroy_sampler(self.sampler, None);
            d.destroy_image_view(self.view, None);
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
            d.destroy_fence(self.upload_fence, None);
            d.destroy_command_pool(self.upload_pool, None);
        }
    }
}

pub fn load_models(gpu: Rc<Gpu>, pass: vk::RenderPass) -> [Option<Model>; 2] {
    let directory = std::env::var_os("XR_CONTROLLER_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/usr/share/xr/devices/a081/controllers".into());
    ["left", "right"].map(|hand| {
        match Model::load(gpu.clone(), pass, &directory.join(hand))
            .with_context(|| format!("load {hand} controller model"))
        {
            Ok(model) => Some(model),
            Err(error) => {
                eprintln!("XR_CONTROLLER_MODEL_UNAVAILABLE hand={hand} error={error:#}");
                None
            }
        }
    })
}
