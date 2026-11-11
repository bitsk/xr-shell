// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    environment::Environment,
    gpu::{Buffer, Gpu},
};
use anyhow::{Context, Result, ensure};
use ash::{vk, vk::Handle};
use openxr as xr;
use std::{io::Cursor, rc::Rc, sync::Arc};

struct DepthTarget {
    gpu: Rc<Gpu>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
}
impl DepthTarget {
    fn new(gpu: Rc<Gpu>, extent: vk::Extent2D) -> Result<Self> {
        let mut depth = Self {
            gpu,
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
        };
        unsafe {
            let d = &depth.gpu.device;
            depth.image = d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::D32_SFLOAT)
                    .extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?;
            let req = d.get_image_memory_requirements(depth.image);
            depth.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(depth.gpu.memory_type(
                        req.memory_type_bits,
                        vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    )?),
                None,
            )?;
            d.bind_image_memory(depth.image, depth.memory, 0)?;
            depth.view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(depth.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::D32_SFLOAT)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::DEPTH)
                            .level_count(1)
                            .layer_count(1),
                    ),
                None,
            )?;
        }
        Ok(depth)
    }
}
impl Drop for DepthTarget {
    fn drop(&mut self) {
        unsafe {
            self.gpu.device.destroy_image_view(self.view, None);
            self.gpu.device.destroy_image(self.image, None);
            self.gpu.device.free_memory(self.memory, None);
        }
    }
}

struct ImageTarget {
    image: vk::Image,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    depth: Option<DepthTarget>,
}
pub struct Eye {
    gpu: Rc<Gpu>,
    pub swapchain: xr::Swapchain<xr::Vulkan>,
    pub extent: vk::Extent2D,
    images: Vec<ImageTarget>,
}
impl Drop for Eye {
    fn drop(&mut self) {
        unsafe {
            for image in &self.images {
                self.gpu.device.destroy_framebuffer(image.framebuffer, None);
                self.gpu.device.destroy_image_view(image.view, None);
            }
        }
    }
}

struct AtlasEntry {
    position: (u32, u32),
    size: (u32, u32),
    pixels: Arc<[u8]>,
}

struct AtlasUpload {
    staging: Option<Buffer>,
    copies: Vec<vk::BufferImageCopy>,
    gutters: Vec<vk::ImageBlit>,
    clear: bool,
}
pub struct Renderer {
    gpu: Rc<Gpu>,
    pub eyes: Vec<Eye>,
    pub format: vk::Format,
    pub foreground: Vec<Eye>,
    pub quad: Option<Eye>,
    pub focus_quad: Option<FocusQuad>,
    pub quad_reason: String,
    pub controller_visuals: u8,
    geometry: Vec<crate::desktop::spatial_ui::Geometry>,
    partition: Vec<i8>,
    quad_disabled: bool,
    quad_crop_enabled: bool,
    pub foreground_regions: Vec<(vk::Rect2D, xr::Fovf)>,

    pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    spatial_pipeline: vk::Pipeline,
    opaque_pipeline: vk::Pipeline,
    controllers: [Option<crate::controller_model::Model>; 2],
    controller_states: [crate::input::HandState; 2],
    ray_feedback: [Option<crate::spatial::RayFeedback>; 2],
    draw_count: u32,
    uv_map: std::collections::BTreeMap<String, [f32; 4]>,
    layout: vk::PipelineLayout,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor: vk::DescriptorSet,
    image: vk::Image,
    image_memory: vk::DeviceMemory,
    image_view: vk::ImageView,
    sampler: vk::Sampler,
    atlas_sampler: vk::Sampler,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    shaders: Vec<vk::ShaderModule>,
    yaw: f32,
    passthrough: Option<crate::passthrough::Passthrough>,
    passthrough_fallback: Option<Buffer>,
    passthrough_active: bool,
    atlas: Option<Atlas>,
    atlas_upload: Option<AtlasUpload>,
    atlas_entries: std::collections::BTreeMap<String, AtlasEntry>,
    ui_uniform: Option<Buffer>,
    panel_uniforms: Vec<f32>,
    ui_offset: [f32; 3],
    ui_anchor_ready: bool,
}

impl Renderer {
    pub fn new(
        gpu: Rc<Gpu>,
        session: &xr::Session<xr::Vulkan>,
        views: &[xr::ViewConfigurationView],
        environment: &Environment,
        scale: f32,
    ) -> Result<Self> {
        eprintln!("XR_INIT_STAGE=swapchain-formats STATUS=begin");
        let formats = session
            .enumerate_swapchain_formats()
            .context("xrEnumerateSwapchainFormats")?;
        eprintln!("XR_INIT_STAGE=swapchain-formats STATUS=complete FORMATS={formats:?}");
        let format = [vk::Format::R8G8B8A8_SRGB, vk::Format::B8G8R8A8_SRGB]
            .into_iter()
            .find(|f| formats.contains(&(f.as_raw() as _)))
            .ok_or_else(|| anyhow::anyhow!("no supported sRGB OpenXR swapchain format"))?;
        let mut r = Self {
            gpu,
            eyes: Vec::new(),
            format,
            foreground: Vec::new(),
            quad: None,
            focus_quad: None,
            quad_reason: "no-focus".into(),
            controller_visuals: 3,
            geometry: Vec::new(),
            partition: Vec::new(),
            quad_disabled: false,
            quad_crop_enabled: std::env::var("XR_FOCUS_QUAD_CROP").as_deref() != Ok("off"),
            foreground_regions: Vec::new(),
            pass: vk::RenderPass::null(),
            pipeline: vk::Pipeline::null(),
            spatial_pipeline: vk::Pipeline::null(),
            opaque_pipeline: vk::Pipeline::null(),
            controllers: [None, None],
            controller_states: [crate::input::HandState::default(); 2],
            ray_feedback: [None; 2],
            draw_count: 0,
            uv_map: std::collections::BTreeMap::new(),
            layout: vk::PipelineLayout::null(),
            descriptor_layout: vk::DescriptorSetLayout::null(),
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor: vk::DescriptorSet::null(),
            image: vk::Image::null(),
            image_memory: vk::DeviceMemory::null(),
            image_view: vk::ImageView::null(),
            sampler: vk::Sampler::null(),
            atlas_sampler: vk::Sampler::null(),
            pool: vk::CommandPool::null(),
            cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            shaders: Vec::new(),
            yaw: environment.yaw,
            passthrough: None,
            passthrough_fallback: None,
            passthrough_active: false,
            atlas: None,
            atlas_upload: None,
            atlas_entries: std::collections::BTreeMap::new(),
            ui_uniform: None,
            panel_uniforms: Vec::new(),
            ui_offset: [0.; 3],
            ui_anchor_ready: true,
        };
        // Everything allocated below is already owned by r, including on error.
        unsafe {
            let d = &r.gpu.device;
            r.pool = d.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(r.gpu.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            r.cmd = d.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(r.pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            r.fence = d.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;
        }
        eprintln!("XR_INIT_STAGE=renderer-resources STATUS=begin FORMAT={format:?}");
        r.upload(environment)
            .context("upload panorama Vulkan texture")?;
        r.init_panels()
            .context("initialize spatial panel Vulkan resources")?;
        r.pipeline().context("create panorama Vulkan pipeline")?;
        r.create_spatial_pipeline()
            .context("create spatial Vulkan pipeline")?;
        match crate::passthrough::Passthrough::from_environment(r.gpu.clone(), r.descriptor) {
            Ok(passthrough) => r.passthrough = passthrough,
            Err(error) => eprintln!("XR_PASSTHROUGH=disabled ERROR={error:#}"),
        }
        r.controllers = crate::controller_model::load_models(r.gpu.clone(), r.pass);
        eprintln!("XR_INIT_STAGE=renderer-resources STATUS=complete");
        for (index, v) in views.iter().enumerate() {
            let extent = vk::Extent2D {
                width: ((v.recommended_image_rect_width as f32 * scale).round() as u32)
                    .max(1)
                    .min(v.max_image_rect_width),
                height: ((v.recommended_image_rect_height as f32 * scale).round() as u32)
                    .max(1)
                    .min(v.max_image_rect_height),
            };
            eprintln!(
                "XR_INIT_STAGE=swapchain-create STATUS=begin EYE={index} SIZE={}x{} FORMAT={format:?} SAMPLES=1 RECOMMENDED_SAMPLES={} MAX_SAMPLES={} ARRAY_SIZE=1 FACE_COUNT=1 MIP_COUNT=1 USAGE=COLOR_ATTACHMENT|TRANSFER_SRC",
                extent.width,
                extent.height,
                v.recommended_swapchain_sample_count,
                v.max_swapchain_sample_count
            );
            let eye = r.create_target(session, extent)?;
            println!(
                "XR_EYE={index} SIZE={}x{} IMAGES={} FORMAT={format:?}",
                extent.width,
                extent.height,
                eye.images.len()
            );
            r.eyes.push(eye);
        }
        Ok(r)
    }

    fn begin(&self) -> Result<()> {
        unsafe {
            self.gpu
                .device
                .wait_for_fences(&[self.fence], true, u64::MAX)?;
            self.gpu.device.reset_fences(&[self.fence])?;
            self.gpu
                .device
                .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())?;
            self.gpu.device.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            Ok(())
        }
    }
    fn submit(&self) -> Result<()> {
        unsafe {
            self.gpu.device.end_command_buffer(self.cmd)?;
            self.gpu.device.queue_submit(
                self.gpu.queue,
                &[vk::SubmitInfo::default().command_buffers(&[self.cmd])],
                self.fence,
            )?;
            // Complete the stereo batch before releasing the acquired XR images.
            self.gpu
                .device
                .wait_for_fences(&[self.fence], true, u64::MAX)?;
            Ok(())
        }
    }

    fn upload(&mut self, environment: &Environment) -> Result<()> {
        let staging = Buffer::host(
            self.gpu.clone(),
            environment.rgba.len() as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
        )?;
        staging.write(&environment.rgba)?;
        let extent = vk::Extent3D {
            width: environment.width,
            height: environment.height,
            depth: 1,
        };
        let levels = mip_count(environment.width, environment.height);
        unsafe {
            let d = &self.gpu.device;
            let features = self
                .gpu
                .instance
                .get_physical_device_format_properties(self.gpu.physical, vk::Format::R8G8B8A8_SRGB)
                .optimal_tiling_features;
            ensure!(
                features.contains(
                    vk::FormatFeatureFlags::BLIT_SRC
                        | vk::FormatFeatureFlags::BLIT_DST
                        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
                ),
                "UI sRGB format needs filtered mip blits"
            );
            self.image = d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .extent(extent)
                    .mip_levels(levels)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(
                        vk::ImageUsageFlags::TRANSFER_DST
                            | vk::ImageUsageFlags::TRANSFER_SRC
                            | vk::ImageUsageFlags::SAMPLED,
                    )
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?;
            let req = d.get_image_memory_requirements(self.image);
            self.image_memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(self.gpu.memory_type(
                        req.memory_type_bits,
                        vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    )?),
                None,
            )?;
            d.bind_image_memory(self.image, self.image_memory, 0)?;
            self.begin()?;
            let to_transfer = image_barrier(
                self.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            )
            .subresource_range(colour_range().level_count(levels));
            d.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            d.cmd_copy_buffer_to_image(
                self.cmd,
                staging.handle,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(colour_layers())
                    .image_extent(extent)],
            );
            generate_mips(
                &self.gpu,
                self.cmd,
                self.image,
                environment.width,
                environment.height,
                levels,
            );
            self.submit()?;
            self.image_view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(self.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .subresource_range(colour_range().level_count(levels)),
                None,
            )?;
            self.sampler = d.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::REPEAT)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .anisotropy_enable(self.gpu.anisotropy > 1.)
                    .max_anisotropy(self.gpu.anisotropy)
                    .max_lod((levels - 1) as f32),
                None,
            )?;
            self.atlas_sampler = d.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .anisotropy_enable(self.gpu.anisotropy > 1.)
                    .max_anisotropy(self.gpu.anisotropy)
                    // Sixteen duplicated edge texels isolate the first four levels,
                    // including the two-tap anisotropic footprint at the coarsest LOD.
                    .max_lod(3.),
                None,
            )?;
            self.descriptor_layout = d.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::VERTEX),
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(1)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::VERTEX),
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(2)
                        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::VERTEX),
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(3)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(4)
                        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                ]),
                None,
            )?;
            self.descriptor_pool = d.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(1)
                    .pool_sizes(&[
                        vk::DescriptorPoolSize {
                            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                            descriptor_count: 3,
                        },
                        vk::DescriptorPoolSize {
                            ty: vk::DescriptorType::STORAGE_BUFFER,
                            descriptor_count: 2,
                        },
                    ]),
                None,
            )?;
            self.descriptor = d.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.descriptor_pool)
                    .set_layouts(&[self.descriptor_layout]),
            )?[0];
            let images = [vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(self.image_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            d.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(self.descriptor)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&images)],
                &[],
            );
            let fallback = Buffer::host(
                self.gpu.clone(),
                48 * 4,
                vk::BufferUsageFlags::STORAGE_BUFFER,
            )?;
            fallback.write(&[0u8; 48 * 4])?;
            d.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.descriptor)
                        .dst_binding(3)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&images),
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.descriptor)
                        .dst_binding(4)
                        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                        .buffer_info(&[vk::DescriptorBufferInfo::default()
                            .buffer(fallback.handle)
                            .range(fallback.size)]),
                ],
                &[],
            );
            self.passthrough_fallback = Some(fallback);
        }
        Ok(())
    }

    fn pipeline(&mut self) -> Result<()> {
        unsafe {
            let d = &self.gpu.device;
            let attachments = [
                vk::AttachmentDescription::default()
                    .format(self.format)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL),
                vk::AttachmentDescription::default()
                    .format(vk::Format::D32_SFLOAT)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::CLEAR)
                    .store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
            ];
            let colour = [vk::AttachmentReference {
                attachment: 0,
                layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            }];
            let depth = vk::AttachmentReference {
                attachment: 1,
                layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            };
            let subpasses = [vk::SubpassDescription::default()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(&colour)
                .depth_stencil_attachment(&depth)];
            let deps = [vk::SubpassDependency::default()
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .dst_subpass(0)
                .src_stage_mask(
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                        | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                )
                .dst_stage_mask(
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                        | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
                )
                .dst_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                )];
            self.pass = d.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&attachments)
                    .subpasses(&subpasses)
                    .dependencies(&deps),
                None,
            )?;
            for bytes in [
                include_bytes!(concat!(env!("OUT_DIR"), "/panorama.vert.spv")).as_slice(),
                include_bytes!(concat!(env!("OUT_DIR"), "/panorama.frag.spv")).as_slice(),
            ] {
                let code = ash::util::read_spv(&mut Cursor::new(bytes))?;
                self.shaders.push(d.create_shader_module(
                    &vk::ShaderModuleCreateInfo::default().code(&code),
                    None,
                )?);
            }
            let stages = [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(self.shaders[0])
                    .name(c"main"),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(self.shaders[1])
                    .name(c"main"),
            ];
            self.layout = d.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&[self.descriptor_layout])
                    .push_constant_ranges(&[vk::PushConstantRange {
                        stage_flags: vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::VERTEX,
                        offset: 0,
                        size: 64,
                    }]),
                None,
            )?;
            let vertices = vk::PipelineVertexInputStateCreateInfo::default();
            let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
            let viewport = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);
            let raster = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
                .line_width(1.0);
            let multi = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let colour = [
                vk::PipelineColorBlendAttachmentState::default().color_write_mask(
                    vk::ColorComponentFlags::R
                        | vk::ColorComponentFlags::G
                        | vk::ColorComponentFlags::B
                        | vk::ColorComponentFlags::A,
                ),
            ];
            let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&colour);
            let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic =
                vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
            let depth_state = vk::PipelineDepthStencilStateCreateInfo::default();
            let create = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertices)
                .input_assembly_state(&assembly)
                .viewport_state(&viewport)
                .rasterization_state(&raster)
                .multisample_state(&multi)
                .depth_stencil_state(&depth_state)
                .color_blend_state(&blend)
                .dynamic_state(&dynamic)
                .layout(self.layout)
                .render_pass(self.pass)
                .subpass(0);
            match d.create_graphics_pipelines(vk::PipelineCache::null(), &[create], None) {
                Ok(p) => self.pipeline = p[0],
                Err((partial, error)) => {
                    for p in partial {
                        d.destroy_pipeline(p, None);
                    }
                    return Err(error.into());
                }
            }
            for shader in self.shaders.drain(..) {
                d.destroy_shader_module(shader, None);
            }
            Ok(())
        }
    }

    pub fn update_controllers(&mut self, states: &[crate::input::HandState; 2]) {
        self.controller_states = *states;
    }

    pub fn update_ray_feedback(&mut self, feedback: [Option<crate::spatial::RayFeedback>; 2]) {
        self.ray_feedback = feedback;
    }

    pub fn controller_status(&self) -> serde_json::Value {
        serde_json::json!({
            "source": "OpenXR action grip/aim spaces at predicted display time",
            "draw_mask":self.controller_visuals,
            "model_to_grip_calibrated": false,
            "model_binding_source": "Resource controller-anchors.json; model-only rotation, tracked grip and aim unchanged",
            "hands": self.controllers.iter().zip(&self.controller_states).enumerate().map(|(hand,(model,state))| {
                let pose = |p: xr::Posef| serde_json::json!({"position_m":[p.position.x,p.position.y,p.position.z],"orientation_xyzw":[p.orientation.x,p.orientation.y,p.orientation.z,p.orientation.w]});
                serde_json::json!({"hand":if hand==0 {"left"} else {"right"}, "loaded":model.is_some(), "directory":model.as_ref().map(|m|&m.directory), "visible":self.controller_visuals & 1 != 0 && model.is_some() && state.grip.is_some(), "rays_visible":self.controller_visuals & 2 != 0 && model.is_some() && state.aim.is_some(), "grip":state.grip.map(pose), "aim":state.aim.map(pose), "ray_feedback":self.ray_feedback[hand], "ray_length_m":self.ray_feedback[hand].map_or(crate::spatial::IDLE_RAY_LENGTH_M, |h|h.distance_m), "source_binding_present":model.as_ref().is_some_and(|m|m.source_binding_present), "source_rotation_xyzw":model.as_ref().map(|m|{let q=m.source_rotation;[q.x,q.y,q.z,q.w]}), "model_pose":model.as_ref().and_then(|m|state.grip.map(|p|pose(m.model_pose(p)))), "select":state.select,"secondary":state.secondary,"thumb_click":state.thumb_click,"trigger":state.trigger,"squeeze":state.squeeze,"menu":state.menu,"stick":state.stick})
            }).collect::<Vec<_>>()
        })
    }

    fn create_target(
        &self,
        session: &xr::Session<xr::Vulkan>,
        extent: vk::Extent2D,
    ) -> Result<Eye> {
        let index = "layer";
        let format = self.format;
        let swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
                create_flags: xr::SwapchainCreateFlags::EMPTY,
                usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                    | xr::SwapchainUsageFlags::TRANSFER_SRC,
                format: format.as_raw() as _,
                sample_count: 1,
                width: extent.width,
                height: extent.height,
                face_count: 1,
                array_size: 1,
                mip_count: 1,
            }).with_context(|| format!("xrCreateSwapchain(eye={index}, {}x{}, format={format:?}, samples=1, array=1, usage=COLOR_ATTACHMENT|TRANSFER_SRC)", extent.width, extent.height))?;
        eprintln!("XR_INIT_STAGE=swapchain-create STATUS=complete EYE={index}");
        let mut eye = Eye {
            gpu: self.gpu.clone(),
            swapchain,
            extent,
            images: Vec::new(),
        };
        eprintln!("XR_INIT_STAGE=swapchain-images STATUS=begin EYE={index}");
        let images = eye
            .swapchain
            .enumerate_images()
            .with_context(|| format!("xrEnumerateSwapchainImages(eye={index})"))?;
        eprintln!(
            "XR_INIT_STAGE=swapchain-images STATUS=complete EYE={index} IMAGES={}",
            images.len()
        );
        for (image_index, image) in images.into_iter().enumerate() {
            eye.images.push(ImageTarget {
                image: vk::Image::from_raw(image),
                view: vk::ImageView::null(),
                framebuffer: vk::Framebuffer::null(),
                depth: None,
            });
            let target = eye.images.last_mut().unwrap();
            target.depth = Some(DepthTarget::new(self.gpu.clone(), extent)?);
            unsafe {
                target.view = self
                    .gpu
                    .device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(target.image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(format)
                            .subresource_range(colour_range()),
                        None,
                    )
                    .with_context(|| {
                        format!("vkCreateImageView(eye={index}, image={image_index})")
                    })?;
                target.framebuffer = self
                    .gpu
                    .device
                    .create_framebuffer(
                        &vk::FramebufferCreateInfo::default()
                            .render_pass(self.pass)
                            .attachments(&[target.view, target.depth.as_ref().unwrap().view])
                            .width(extent.width)
                            .height(extent.height)
                            .layers(1),
                        None,
                    )
                    .with_context(|| {
                        format!("vkCreateFramebuffer(eye={index}, image={image_index})")
                    })?;
            }
        }
        Ok(eye)
    }
    fn record_eye(
        &self,
        target: &Eye,
        pass: DrawPass,
        eye_index: usize,
        image_index: usize,
        view: &xr::View,
        capture: bool,
        region: Option<vk::Rect2D>,
    ) -> Result<Option<Buffer>> {
        ensure!(
            image_index < target.images.len(),
            "XR swapchain index out of bounds"
        );
        let image = &target.images[image_index];
        let readback = if capture {
            Some(Buffer::host(
                self.gpu.clone(),
                target.extent.width as u64 * target.extent.height as u64 * 4,
                vk::BufferUsageFlags::TRANSFER_DST,
            )?)
        } else {
            None
        };
        unsafe {
            let d = &self.gpu.device;
            let rect = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: target.extent,
            };
            d.cmd_begin_render_pass(
                self.cmd,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.pass)
                    .framebuffer(image.framebuffer)
                    .render_area(if capture {
                        rect
                    } else {
                        region.unwrap_or(rect)
                    })
                    .clear_values(&[
                        vk::ClearValue::default(),
                        vk::ClearValue {
                            depth_stencil: vk::ClearDepthStencilValue {
                                depth: 1.,
                                stencil: 0,
                            },
                        },
                    ]),
                vk::SubpassContents::INLINE,
            );
            if matches!(pass, DrawPass::Foreground | DrawPass::Quad) {
                // The shared pass uses DONT_CARE for the opaque panorama.
                // Alpha layers must explicitly clear reused swapchain images;
                // unwritten pixels otherwise retain undefined/previous content.
                d.cmd_clear_attachments(
                    self.cmd,
                    &[vk::ClearAttachment::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .color_attachment(0)
                        .clear_value(vk::ClearValue::default())],
                    &[vk::ClearRect::default()
                        .rect(if capture {
                            rect
                        } else {
                            region.unwrap_or(rect)
                        })
                        .base_array_layer(0)
                        .layer_count(1)],
                );
            }
            d.cmd_set_viewport(
                self.cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: target.extent.width as f32,
                    height: target.extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            d.cmd_set_scissor(self.cmd, 0, &[region.unwrap_or(rect)]);
            d.cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            d.cmd_bind_descriptor_sets(
                self.cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[self.descriptor],
                &[],
            );
            let q = view.pose.orientation;
            let constants = [
                view.fov.angle_left.tan(),
                view.fov.angle_right.tan(),
                view.fov.angle_up.tan(),
                view.fov.angle_down.tan(),
                q.x,
                q.y,
                q.z,
                q.w,
                self.yaw.cos(),
                self.yaw.sin(),
                if self.passthrough_active { 1.0 } else { 0.0 },
                eye_index as f32,
                view.pose.position.x,
                view.pose.position.y,
                view.pose.position.z,
                0.0,
            ];
            let bytes = std::slice::from_raw_parts(constants.as_ptr() as *const u8, 64);
            d.cmd_push_constants(
                self.cmd,
                self.layout,
                vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::VERTEX,
                0,
                bytes,
            );
            if matches!(pass, DrawPass::All | DrawPass::Background) {
                d.cmd_draw(self.cmd, 3, 1, 0, 0);
            }
            for (hand, (model, state)) in self
                .controllers
                .iter()
                .zip(&self.controller_states)
                .enumerate()
            {
                if let Some(model) = model
                    .as_ref()
                    .filter(|_| matches!(pass, DrawPass::All) && self.controller_visuals & 1 != 0)
                {
                    model.draw(
                        self.cmd,
                        view,
                        state,
                        self.ray_feedback[hand].as_ref(),
                        false,
                    );
                }
            }
            // Controller drawing uses a separate descriptor and push-constant layout.
            d.cmd_bind_descriptor_sets(
                self.cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[self.descriptor],
                &[],
            );
            d.cmd_push_constants(
                self.cmd,
                self.layout,
                vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::VERTEX,
                0,
                bytes,
            );
            if self.ui_anchor_ready && self.draw_count > 0 {
                // Opaque window/glyph cores establish per-pixel depth before
                // sorted translucent glass. Intersecting opaque panels no longer
                // depend on their centre-distance order.
                for pipeline in [self.opaque_pipeline, self.spatial_pipeline] {
                    d.cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
                    if matches!(pass, DrawPass::All) {
                        d.cmd_draw(self.cmd, 192 + 36 * 6, self.draw_count, 0, 0);
                    } else {
                        let desired = match pass {
                            DrawPass::Background => -1,
                            DrawPass::Foreground => 1,
                            _ => 0,
                        };
                        for (index, part) in self.partition.iter().enumerate() {
                            if *part == desired {
                                d.cmd_draw(
                                    self.cmd,
                                    if desired == 0 { 192 } else { 192 + 36 * 6 },
                                    1,
                                    0,
                                    index as u32,
                                );
                            }
                        }
                    }
                }
            }
            for (hand, (model, state)) in self
                .controllers
                .iter()
                .zip(&self.controller_states)
                .enumerate()
            {
                if let Some(model) = model
                    .as_ref()
                    .filter(|_| matches!(pass, DrawPass::All) && self.controller_visuals & 2 != 0)
                {
                    model.draw(
                        self.cmd,
                        view,
                        state,
                        self.ray_feedback[hand].as_ref(),
                        true,
                    );
                }
            }
            d.cmd_end_render_pass(self.cmd);
            if let Some(buffer) = &readback {
                d.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier(
                        image.image,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                        vk::AccessFlags::TRANSFER_READ,
                    )],
                );
                d.cmd_copy_image_to_buffer(
                    self.cmd,
                    image.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(colour_layers())
                        .image_extent(vk::Extent3D {
                            width: target.extent.width,
                            height: target.extent.height,
                            depth: 1,
                        })],
                );
                let host = [vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(buffer.handle)
                    .size(vk::WHOLE_SIZE)];
                d.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &host,
                    &[],
                );
                d.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier(
                        image.image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::AccessFlags::TRANSFER_READ,
                        vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    )],
                );
            }
        }
        Ok(readback)
    }

    /// Both eyes share one recording/submission. The single completion wait
    /// protects XR release and optional readback; no CPU wait splits the eyes.
    pub fn passthrough_status(&self) -> Option<serde_json::Value> {
        self.passthrough.as_ref().map(|p| {
            let mut status = p.status();
            status["render_active"] = self.passthrough_active.into();
            status
        })
    }
    pub fn passthrough_submitted(&mut self) {
        if let Some(p) = &mut self.passthrough {
            p.submitted(self.passthrough_active);
        }
    }
    pub fn sample_passthrough_pose(
        &mut self,
        instance: &xr::Instance,
        session: &xr::Session<xr::Vulkan>,
        space: &xr::Space,
    ) {
        let Some(p) = self.passthrough.as_mut().filter(|p| p.motion_enabled()) else {
            return;
        };
        // Sample "now", not predictedDisplayTime: associating a predicted pose
        // with a current monotonic stamp would shift the entire pose history.
        let before = crate::timeline::now() as u64;
        let time = if instance.exts().khr_convert_timespec_time.is_some() {
            instance.now().ok()
        } else {
            None
        };
        let after = crate::timeline::now() as u64;
        if after.saturating_sub(before) > 1_000_000 {
            p.reset_pose_history();
            return;
        }
        if let Some((flags, views)) =
            time.and_then(|t| session.locate_views(crate::VIEW_TYPE, t, space).ok())
        {
            p.observe_views(flags, &views, before + (after - before) / 2);
        } else {
            p.reset_pose_history();
        }
    }
    pub fn reset_passthrough_pose_history(&mut self) {
        if let Some(p) = &mut self.passthrough {
            p.reset_pose_history();
        }
    }
    pub fn render_stereo(
        &mut self,
        indices: &[u32],
        views: &[xr::View],
        capture: bool,
    ) -> Result<Vec<Option<Vec<u8>>>> {
        self.begin()?;
        if let Some(passthrough) = &mut self.passthrough {
            passthrough.prepare(self.cmd, views)?;
        }
        self.passthrough_active = self.passthrough.as_ref().is_some_and(|p| p.active());
        self.record_atlas_upload();
        let mut readbacks: Vec<_> = (0..2)
            .map(|eye| {
                self.record_eye(
                    &self.eyes[eye],
                    if self.focus_quad.is_some() {
                        DrawPass::Background
                    } else {
                        DrawPass::All
                    },
                    eye,
                    indices[eye] as usize,
                    &views[eye],
                    capture,
                    None,
                )
            })
            .collect::<Result<_>>()?;
        if let Some(focus) = &self.focus_quad {
            for eye in 0..2 {
                readbacks.push(self.record_eye(
                    &self.foreground[eye],
                    DrawPass::Foreground,
                    eye,
                    indices[eye + 2] as usize,
                    &views[eye],
                    capture,
                    Some(self.foreground_regions[eye].0),
                )?);
            }
            readbacks.push(self.record_eye(
                self.quad.as_ref().unwrap(),
                DrawPass::Quad,
                0,
                indices[4] as usize,
                &focus.view(),
                capture,
                None,
            )?);
        }
        self.submit()?;
        if self.atlas_upload.take().is_some() {
            self.atlas.as_mut().unwrap().initialized = true;
        }
        readbacks
            .into_iter()
            .map(|buffer| {
                buffer
                    .map(|buffer| {
                        let mut rgba = buffer.read()?;
                        if self.format == vk::Format::B8G8R8A8_SRGB {
                            for p in rgba.chunks_exact_mut(4) {
                                p.swap(0, 2);
                            }
                        }
                        Ok(rgba)
                    })
                    .transpose()
            })
            .collect()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.gpu.device.device_wait_idle();
            self.foreground.clear();
            self.quad = None;
            self.eyes.clear();
            self.controllers = [None, None];
            let d = &self.gpu.device;
            d.destroy_pipeline(self.spatial_pipeline, None);
            d.destroy_pipeline(self.opaque_pipeline, None);
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.layout, None);
            for shader in self.shaders.drain(..) {
                d.destroy_shader_module(shader, None);
            }
            d.destroy_render_pass(self.pass, None);
            d.destroy_descriptor_pool(self.descriptor_pool, None);
            d.destroy_descriptor_set_layout(self.descriptor_layout, None);
            d.destroy_sampler(self.sampler, None);
            d.destroy_sampler(self.atlas_sampler, None);
            d.destroy_image_view(self.image_view, None);
            d.destroy_image(self.image, None);
            d.free_memory(self.image_memory, None);
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
        }
    }
}

fn mip_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).leading_zeros()
}
fn align_tile(n: u32) -> u32 {
    (n + 15) & !15
}
// Eight non-overlapping destinations repeat the edge texels, including the
// alignment tail. Sources are strictly inside the tile, so a same-image blit
// never reads a destination region (including across other tiles).
fn gutter_blits(x: u32, y: u32, w: u32, h: u32) -> Vec<vk::ImageBlit> {
    let xs = [
        (x, x + 1, x - 16, x),
        (x, x + w, x, x + w),
        (x + w - 1, x + w, x + w, x - 16 + align_tile(w + 32)),
    ];
    let ys = [
        (y, y + 1, y - 16, y),
        (y, y + h, y, y + h),
        (y + h - 1, y + h, y + h, y - 16 + align_tile(h + 32)),
    ];
    let mut regions = Vec::with_capacity(8);
    let offset = |x, y, z| vk::Offset3D {
        x: x as i32,
        y: y as i32,
        z,
    };
    for (i, &(sx0, sx1, dx0, dx1)) in xs.iter().enumerate() {
        for (j, &(sy0, sy1, dy0, dy1)) in ys.iter().enumerate() {
            if i == 1 && j == 1 {
                continue;
            }
            regions.push(
                vk::ImageBlit::default()
                    .src_subresource(colour_layers())
                    .src_offsets([offset(sx0, sy0, 0), offset(sx1, sy1, 1)])
                    .dst_subresource(colour_layers())
                    .dst_offsets([offset(dx0, dy0, 0), offset(dx1, dy1, 1)]),
            );
        }
    }
    regions
}
unsafe fn generate_mips(
    gpu: &Gpu,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    width: u32,
    height: u32,
    levels: u32,
) {
    unsafe {
        let d = &gpu.device;
        for level in 1..levels {
            let range = colour_range().base_mip_level(level - 1);
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[image_barrier(
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::TRANSFER_READ,
                )
                .subresource_range(range)],
            );
            let extent = |l: u32| vk::Offset3D {
                x: (width >> l).max(1) as i32,
                y: (height >> l).max(1) as i32,
                z: 1,
            };
            let blit = vk::ImageBlit::default()
                .src_subresource(colour_layers().mip_level(level - 1))
                .src_offsets([vk::Offset3D::default(), extent(level - 1)])
                .dst_subresource(colour_layers().mip_level(level))
                .dst_offsets([vk::Offset3D::default(), extent(level)]);
            d.cmd_blit_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[image_barrier(
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_READ,
                    vk::AccessFlags::SHADER_READ,
                )
                .subresource_range(range)],
            );
        }
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[image_barrier(
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::SHADER_READ,
            )
            .subresource_range(colour_range().base_mip_level(levels - 1))],
        );
    }
}

pub(crate) fn colour_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}
pub(crate) fn colour_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .layer_count(1)
}
pub(crate) fn image_barrier(
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src: vk::AccessFlags,
    dst: vk::AccessFlags,
) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .image(image)
        .old_layout(old)
        .new_layout(new)
        .src_access_mask(src)
        .dst_access_mask(dst)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .subresource_range(colour_range())
}

struct Atlas {
    gpu: Rc<Gpu>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    width: u32,
    height: u32,
    initialized: bool,
}
impl Atlas {
    fn new(gpu: Rc<Gpu>, width: u32, height: u32) -> Result<Self> {
        let mut a = Self {
            gpu,
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            width,
            height,
            initialized: false,
        };
        unsafe {
            let d = &a.gpu.device;
            a.image = d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    })
                    .mip_levels(mip_count(width, height))
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(
                        vk::ImageUsageFlags::TRANSFER_DST
                            | vk::ImageUsageFlags::TRANSFER_SRC
                            | vk::ImageUsageFlags::SAMPLED,
                    )
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?;
            let req = d.get_image_memory_requirements(a.image);
            a.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(a.gpu.memory_type(
                        req.memory_type_bits,
                        vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    )?),
                None,
            )?;
            d.bind_image_memory(a.image, a.memory, 0)?;
            a.view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(a.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_SRGB)
                    .subresource_range(colour_range().level_count(mip_count(width, height))),
                None,
            )?;
        }
        Ok(a)
    }
}
impl Drop for Atlas {
    fn drop(&mut self) {
        unsafe {
            let d = &self.gpu.device;
            d.destroy_image_view(self.view, None);
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
        }
    }
}
const UI_UNIFORM_BYTES: u64 = 2048 * 128;
impl Renderer {
    fn record_atlas_upload(&self) {
        let Some(upload) = &self.atlas_upload else {
            return;
        };
        let a = self.atlas.as_ref().unwrap();
        unsafe {
            let d = &self.gpu.device;
            d.cmd_pipeline_barrier(
                self.cmd,
                if a.initialized {
                    vk::PipelineStageFlags::FRAGMENT_SHADER
                } else {
                    vk::PipelineStageFlags::TOP_OF_PIPE
                },
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[image_barrier(
                    a.image,
                    if a.initialized {
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                    } else {
                        vk::ImageLayout::UNDEFINED
                    },
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    if a.initialized {
                        vk::AccessFlags::SHADER_READ
                    } else {
                        vk::AccessFlags::empty()
                    },
                    vk::AccessFlags::TRANSFER_WRITE,
                )
                .subresource_range(colour_range().level_count(mip_count(a.width, a.height)))],
            );
            if upload.clear {
                d.cmd_clear_color_image(
                    self.cmd,
                    a.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue { float32: [0.; 4] },
                    &[colour_range()],
                );
                // Order the clear before interior writes to the same mip.
                d.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier(
                        a.image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::TRANSFER_WRITE,
                    )],
                );
            }
            if let Some(staging) = &upload.staging {
                d.cmd_copy_buffer_to_image(
                    self.cmd,
                    staging.handle,
                    a.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &upload.copies,
                );
                d.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier(
                        a.image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::GENERAL,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
                    )],
                );
                d.cmd_blit_image(
                    self.cmd,
                    a.image,
                    vk::ImageLayout::GENERAL,
                    a.image,
                    vk::ImageLayout::GENERAL,
                    &upload.gutters,
                    vk::Filter::NEAREST,
                );
                d.cmd_pipeline_barrier(
                    self.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier(
                        a.image,
                        vk::ImageLayout::GENERAL,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::TRANSFER_WRITE,
                    )],
                );
            }
            generate_mips(
                &self.gpu,
                self.cmd,
                a.image,
                a.width,
                a.height,
                mip_count(a.width, a.height),
            );
        }
    }
    fn init_panels(&mut self) -> Result<()> {
        self.ui_uniform = Some(Buffer::host(
            self.gpu.clone(),
            UI_UNIFORM_BYTES,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?);
        self.update_panels(&[])
    }
    pub fn pending_atlas_stats(&self) -> (usize, u64, bool) {
        self.atlas_upload.as_ref().map_or((0, 0, false), |u| {
            (
                u.copies.len(),
                u.copies
                    .iter()
                    .map(|c| c.image_extent.width as u64 * c.image_extent.height as u64 * 4)
                    .sum(),
                u.clear,
            )
        })
    }
    pub fn update_panels(&mut self, panels: &[crate::desktop::Panel]) -> Result<()> {
        let started = std::time::Instant::now();
        ensure!(panels.len() <= 2048, "draw item limit");
        let width = panels
            .iter()
            .map(|p| align_tile(p.width + 32))
            .max()
            .unwrap_or(4)
            .max(512)
            .next_power_of_two()
            .min(4096);
        let mut packing = Vec::new();
        let (mut x, mut y, mut row) = (0u32, 0u32, 0u32);
        for p in panels {
            if x + align_tile(p.width + 32) > width {
                x = 0;
                y += row;
                row = 0;
            }
            packing.push((x + 16, y + 16));
            x += align_tile(p.width + 32);
            row = row.max(align_tile(p.height + 32));
        }
        let height = (y + row).max(32).next_power_of_two();
        ensure!(height <= 8192, "spatial atlas capacity");
        // A second update before a render replaces the pending batch with a
        // complete snapshot; never lose a tile staged by the previous update.
        let full = self.atlas_upload.is_some()
            || self
                .atlas
                .as_ref()
                .is_none_or(|a| a.width != width || a.height != height)
            || panels.len() != self.atlas_entries.len()
            || panels.iter().zip(&packing).any(|(p, position)| {
                self.atlas_entries
                    .get(&p.id)
                    .is_none_or(|e| e.size != (p.width, p.height) || e.position != *position)
            });
        let changed: Vec<_> = panels
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                (full
                    || self
                        .atlas_entries
                        .get(&p.id)
                        .is_none_or(|e| !Arc::ptr_eq(&e.pixels, &p.rgba)))
                .then_some(i)
            })
            .collect();
        self.uv_map.clear();
        for (p, (x, y)) in panels.iter().zip(&packing) {
            self.uv_map.insert(
                p.id.clone(),
                [
                    (*x as f32 + 0.5) / width as f32,
                    (*y as f32 + 0.5) / height as f32,
                    (p.width - 1) as f32 / width as f32,
                    (p.height - 1) as f32 / height as f32,
                ],
            );
        }
        if !changed.is_empty() || self.atlas.as_ref().is_none_or(|a| !a.initialized) {
            let mut pixels = Vec::new();
            let mut copies = Vec::new();
            let mut gutters = Vec::new();
            for &i in &changed {
                let p = &panels[i];
                let (x, y) = packing[i];
                let offset = pixels.len() as u64;
                pixels.extend_from_slice(&p.rgba);
                copies.push(
                    vk::BufferImageCopy::default()
                        .buffer_offset(offset)
                        .image_subresource(colour_layers())
                        .image_offset(vk::Offset3D {
                            x: x as i32,
                            y: y as i32,
                            z: 0,
                        })
                        .image_extent(vk::Extent3D {
                            width: p.width,
                            height: p.height,
                            depth: 1,
                        }),
                );
                gutters.extend(gutter_blits(x, y, p.width, p.height));
            }
            let upload_bytes = pixels.len();
            let staging = if pixels.is_empty() {
                None
            } else {
                let buffer = Buffer::host(
                    self.gpu.clone(),
                    upload_bytes as u64,
                    vk::BufferUsageFlags::TRANSFER_SRC,
                )?;
                buffer.write(&pixels)?;
                Some(buffer)
            };
            // The preceding render completed before XR release. Keep this batch
            // alive through the next stereo submission and its completion fence.
            if self
                .atlas
                .as_ref()
                .is_none_or(|a| a.width != width || a.height != height)
            {
                self.atlas = Some(Atlas::new(self.gpu.clone(), width, height)?);
            }
            self.atlas_upload = Some(AtlasUpload {
                staging,
                copies,
                gutters,
                clear: full,
            });
            if crate::ui_profile() || started.elapsed().as_millis() >= 8 {
                eprintln!(
                    "XR_UI_UPLOAD staged=true full={full} regions={} bytes={upload_bytes} atlas={}x{} elapsed_us={}",
                    changed.len(),
                    width,
                    height,
                    started.elapsed().as_micros()
                );
            }
        } // unchanged textures require neither allocation nor a GPU upload
        self.atlas_entries = panels
            .iter()
            .zip(packing)
            .map(|(p, position)| {
                (
                    p.id.clone(),
                    AtlasEntry {
                        position,
                        size: (p.width, p.height),
                        pixels: p.rgba.clone(),
                    },
                )
            })
            .collect();
        unsafe {
            let images = [vk::DescriptorImageInfo::default()
                .sampler(self.atlas_sampler)
                .image_view(self.atlas.as_ref().unwrap().view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let buffer = [vk::DescriptorBufferInfo::default()
                .buffer(self.ui_uniform.as_ref().unwrap().handle)
                .range(UI_UNIFORM_BYTES)];
            self.gpu.device.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.descriptor)
                        .dst_binding(1)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&images),
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.descriptor)
                        .dst_binding(2)
                        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                        .buffer_info(&buffer),
                ],
                &[],
            );
        }
        let poses: Vec<_> = panels
            .iter()
            .map(|p| crate::desktop::spatial_ui::Geometry {
                id: p.id.clone(),
                frame: p.frame,
                radius: p.radius,
                rim: p.rim,
                border: p.border,
                material: p.material,
                anchor: p.frame.position,
                group: p.id.clone(),
                layer: 0.,
                tint: [0.; 4],
                controls: [0.; 4],
            })
            .collect();
        self.update_panel_poses(&poses, [0., 1.6, 0.])
    }
    pub fn set_ui_anchor(&mut self, offset: Option<[f32; 3]>) {
        self.ui_anchor_ready = offset.is_some();
        self.ui_offset = offset.unwrap_or([0.; 3]);
    }

    pub fn update_panel_poses(
        &mut self,
        poses: &[crate::desktop::spatial_ui::Geometry],
        origin: [f32; 3],
    ) -> Result<()> {
        let mut order: Vec<_> = poses.iter().collect();
        order.sort_by(|a, b| {
            let distance = |p: &crate::desktop::spatial_ui::Geometry| {
                crate::spatial::add(p.anchor, self.ui_offset)
                    .iter()
                    .zip(origin)
                    .map(|(a, b)| (a - b) * (a - b))
                    .sum::<f32>()
            };
            if a.group == b.group {
                a.layer.total_cmp(&b.layer)
            } else {
                distance(b)
                    .total_cmp(&distance(a))
                    .then(a.group.cmp(&b.group))
            }
        });
        self.geometry = order.iter().map(|p| (*p).clone()).collect();
        self.draw_count = order.len() as u32;
        let mut values = Vec::with_capacity(order.len() * 32);
        for p in order {
            let mut f = p.frame;
            f.position = crate::spatial::add(f.position, self.ui_offset);
            let uv = self
                .uv_map
                .get(&p.id)
                .ok_or_else(|| anyhow::anyhow!("draw image missing for {}", p.id))?;
            values.extend_from_slice(&[
                f.position[0],
                f.position[1],
                f.position[2],
                p.material,
                f.right[0],
                f.right[1],
                f.right[2],
                f.size[0],
                f.down[0],
                f.down[1],
                f.down[2],
                f.size[1],
                uv[0],
                uv[1],
                uv[2],
                uv[3],
                f.logical[0],
                f.logical[1],
                p.radius,
                p.rim,
                p.border,
                f.curve,
                match p.material as u32 {
                    1 => 0.006,
                    2 => 0.003,
                    3 => 0.002,
                    _ => 0.,
                },
                0.,
            ]);
            values.extend_from_slice(&p.tint);
            values.extend_from_slice(&p.controls);
        }
        self.panel_uniforms = values;
        self.ui_uniform.as_ref().unwrap().write(unsafe {
            std::slice::from_raw_parts(
                self.panel_uniforms.as_ptr() as *const u8,
                self.panel_uniforms.len() * 4,
            )
        })
    }
    fn create_spatial_pipeline(&mut self) -> Result<()> {
        unsafe {
            let d = &self.gpu.device;
            let mut modules = Vec::new();
            for bytes in [
                include_bytes!(concat!(env!("OUT_DIR"), "/spatial.vert.spv")).as_slice(),
                include_bytes!(concat!(env!("OUT_DIR"), "/spatial.frag.spv")).as_slice(),
            ] {
                let code = ash::util::read_spv(&mut Cursor::new(bytes))?;
                modules.push(d.create_shader_module(
                    &vk::ShaderModuleCreateInfo::default().code(&code),
                    None,
                )?);
            }
            let stages = [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(modules[0])
                    .name(c"main"),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(modules[1])
                    .name(c"main"),
            ];
            let vertices = vk::PipelineVertexInputStateCreateInfo::default();
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
            let attachments = [vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::ONE)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(vk::ColorComponentFlags::RGBA)];
            let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
            let states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&states);
            let depth_state = vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(true)
                .depth_write_enable(false)
                .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
            let create = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertices)
                .input_assembly_state(&assembly)
                .viewport_state(&viewport)
                .rasterization_state(&raster)
                .multisample_state(&multi)
                .depth_stencil_state(&depth_state)
                .color_blend_state(&blend)
                .dynamic_state(&dynamic)
                .layout(self.layout)
                .render_pass(self.pass);
            let result = d.create_graphics_pipelines(vk::PipelineCache::null(), &[create], None);
            self.spatial_pipeline = result.map_err(|(_, e)| e)?[0];
            let opaque_value = 1_u32.to_ne_bytes();
            let entries = [vk::SpecializationMapEntry::default()
                .constant_id(0)
                .offset(0)
                .size(4)];
            let specialization = vk::SpecializationInfo::default()
                .map_entries(&entries)
                .data(&opaque_value);
            let mut opaque_stages = stages;
            opaque_stages[1] = opaque_stages[1].specialization_info(&specialization);
            let opaque_depth = depth_state.depth_write_enable(true);
            let opaque_create = create
                .stages(&opaque_stages)
                .depth_stencil_state(&opaque_depth);
            let result =
                d.create_graphics_pipelines(vk::PipelineCache::null(), &[opaque_create], None);
            for module in modules {
                d.destroy_shader_module(module, None);
            }
            self.opaque_pipeline = result.map_err(|(_, e)| e)?[0];
            Ok(())
        }
    }
}

include!("focus_quad.rs");
