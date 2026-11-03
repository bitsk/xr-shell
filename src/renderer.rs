// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{environment::Environment, gpu::{Buffer, Gpu}};
use anyhow::{Result, ensure};
use ash::{vk, vk::Handle};
use openxr as xr;
use std::{io::Cursor, rc::Rc};

struct ImageTarget { image: vk::Image, view: vk::ImageView, framebuffer: vk::Framebuffer }
pub struct Eye {
    gpu: Rc<Gpu>,
    pub swapchain: xr::Swapchain<xr::Vulkan>,
    pub extent: vk::Extent2D,
    images: Vec<ImageTarget>,
}
impl Drop for Eye {
    fn drop(&mut self) { unsafe {
        for image in &self.images {
            self.gpu.device.destroy_framebuffer(image.framebuffer, None);
            self.gpu.device.destroy_image_view(image.view, None);
        }
    } }
}

pub struct Renderer {
    gpu: Rc<Gpu>,
    pub eyes: Vec<Eye>,
    pub format: vk::Format,
    pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor: vk::DescriptorSet,
    image: vk::Image,
    image_memory: vk::DeviceMemory,
    image_view: vk::ImageView,
    sampler: vk::Sampler,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    shaders: Vec<vk::ShaderModule>,
    yaw: f32,
}

impl Renderer {
    pub fn new(gpu: Rc<Gpu>, session: &xr::Session<xr::Vulkan>,
               views: &[xr::ViewConfigurationView], environment: &Environment, scale: f32) -> Result<Self> {
        let formats = session.enumerate_swapchain_formats()?;
        let format = [vk::Format::R8G8B8A8_SRGB, vk::Format::B8G8R8A8_SRGB].into_iter()
            .find(|f| formats.contains(&(f.as_raw() as _)))
            .ok_or_else(|| anyhow::anyhow!("no supported sRGB OpenXR swapchain format"))?;
        let mut r = Self {
            gpu, eyes: Vec::new(), format, pass: vk::RenderPass::null(), pipeline: vk::Pipeline::null(),
            layout: vk::PipelineLayout::null(), descriptor_layout: vk::DescriptorSetLayout::null(),
            descriptor_pool: vk::DescriptorPool::null(), descriptor: vk::DescriptorSet::null(),
            image: vk::Image::null(), image_memory: vk::DeviceMemory::null(), image_view: vk::ImageView::null(),
            sampler: vk::Sampler::null(), pool: vk::CommandPool::null(), cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(), shaders: Vec::new(), yaw: environment.yaw,
        };
        // Everything allocated below is already owned by r, including on error.
        unsafe {
            let d = &r.gpu.device;
            r.pool = d.create_command_pool(&vk::CommandPoolCreateInfo::default()
                .queue_family_index(r.gpu.family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None)?;
            r.cmd = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
                .command_pool(r.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
            r.fence = d.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)?;
        }
        r.upload(environment)?;
        r.pipeline()?;
        for (index, v) in views.iter().enumerate() {
            let extent = vk::Extent2D {
                width: ((v.recommended_image_rect_width as f32 * scale).round() as u32).max(1).min(v.max_image_rect_width),
                height: ((v.recommended_image_rect_height as f32 * scale).round() as u32).max(1).min(v.max_image_rect_height),
            };
            let swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
                create_flags: xr::SwapchainCreateFlags::EMPTY,
                usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::TRANSFER_SRC,
                format: format.as_raw() as _, sample_count: 1, width: extent.width, height: extent.height,
                face_count: 1, array_size: 1, mip_count: 1,
            })?;
            let mut eye = Eye { gpu: r.gpu.clone(), swapchain, extent, images: Vec::new() };
            for image in eye.swapchain.enumerate_images()? {
                eye.images.push(ImageTarget { image: vk::Image::from_raw(image),
                    view: vk::ImageView::null(), framebuffer: vk::Framebuffer::null() });
                let target = eye.images.last_mut().unwrap();
                unsafe {
                    target.view = r.gpu.device.create_image_view(&vk::ImageViewCreateInfo::default()
                        .image(target.image).view_type(vk::ImageViewType::TYPE_2D).format(format)
                        .subresource_range(colour_range()), None)?;
                    target.framebuffer = r.gpu.device.create_framebuffer(&vk::FramebufferCreateInfo::default()
                        .render_pass(r.pass).attachments(&[target.view]).width(extent.width).height(extent.height).layers(1), None)?;
                }
            }
            println!("XR_EYE={index} SIZE={}x{} IMAGES={} FORMAT={format:?}", extent.width, extent.height, eye.images.len());
            r.eyes.push(eye);
        }
        Ok(r)
    }

    fn begin(&self) -> Result<()> { unsafe {
        self.gpu.device.wait_for_fences(&[self.fence], true, u64::MAX)?;
        self.gpu.device.reset_fences(&[self.fence])?;
        self.gpu.device.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())?;
        self.gpu.device.begin_command_buffer(self.cmd, &vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
        Ok(())
    } }
    fn submit(&self) -> Result<()> { unsafe {
        self.gpu.device.end_command_buffer(self.cmd)?;
        self.gpu.device.queue_submit(self.gpu.queue,
            &[vk::SubmitInfo::default().command_buffers(&[self.cmd])], self.fence)?;
        // S1 deliberately completes GPU work before releasing each XR image.
        self.gpu.device.wait_for_fences(&[self.fence], true, u64::MAX)?;
        Ok(())
    } }

    fn upload(&mut self, environment: &Environment) -> Result<()> {
        let staging = Buffer::host(self.gpu.clone(), environment.rgba.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;
        staging.write(&environment.rgba)?;
        let extent = vk::Extent3D { width: environment.width, height: environment.height, depth: 1 };
        unsafe {
            let d = &self.gpu.device;
            self.image = d.create_image(&vk::ImageCreateInfo::default().image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_SRGB).extent(extent).mip_levels(1).array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1).tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE).initial_layout(vk::ImageLayout::UNDEFINED), None)?;
            let req = d.get_image_memory_requirements(self.image);
            self.image_memory = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size)
                .memory_type_index(self.gpu.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?), None)?;
            d.bind_image_memory(self.image, self.image_memory, 0)?;
            self.begin()?;
            let to_transfer = image_barrier(self.image, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(), vk::AccessFlags::TRANSFER_WRITE);
            d.cmd_pipeline_barrier(self.cmd, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(), &[], &[], &[to_transfer]);
            d.cmd_copy_buffer_to_image(self.cmd, staging.handle, self.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default().image_subresource(colour_layers()).image_extent(extent)]);
            let to_sample = image_barrier(self.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::SHADER_READ);
            d.cmd_pipeline_barrier(self.cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(), &[], &[], &[to_sample]);
            self.submit()?;
            self.image_view = d.create_image_view(&vk::ImageViewCreateInfo::default().image(self.image)
                .view_type(vk::ImageViewType::TYPE_2D).format(vk::Format::R8G8B8A8_SRGB)
                .subresource_range(colour_range()), None)?;
            self.sampler = d.create_sampler(&vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR).min_filter(vk::Filter::LINEAR)
                .mipmap_mode(vk::SamplerMipmapMode::NEAREST).address_mode_u(vk::SamplerAddressMode::REPEAT)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE).address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .max_lod(0.0), None)?;
            self.descriptor_layout = d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default()
                .bindings(&[vk::DescriptorSetLayoutBinding::default().binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)]), None)?;
            self.descriptor_pool = d.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1)
                .pool_sizes(&[vk::DescriptorPoolSize { ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, descriptor_count: 1 }]), None)?;
            self.descriptor = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.descriptor_pool).set_layouts(&[self.descriptor_layout]))?[0];
            let images = [vk::DescriptorImageInfo::default().sampler(self.sampler).image_view(self.image_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            d.update_descriptor_sets(&[vk::WriteDescriptorSet::default().dst_set(self.descriptor).dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&images)], &[]);
        }
        Ok(())
    }

    fn pipeline(&mut self) -> Result<()> { unsafe {
        let d = &self.gpu.device;
        let attachments = [vk::AttachmentDescription::default().format(self.format).samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::DONT_CARE).store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE).stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED).final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let colour = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
        let subpasses = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&colour)];
        let deps = [vk::SubpassDependency::default().src_subpass(vk::SUBPASS_EXTERNAL).dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT).dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)];
        self.pass = d.create_render_pass(&vk::RenderPassCreateInfo::default()
            .attachments(&attachments).subpasses(&subpasses).dependencies(&deps), None)?;
        for bytes in [include_bytes!(concat!(env!("OUT_DIR"), "/panorama.vert.spv")).as_slice(),
                      include_bytes!(concat!(env!("OUT_DIR"), "/panorama.frag.spv")).as_slice()] {
            let code = ash::util::read_spv(&mut Cursor::new(bytes))?;
            self.shaders.push(d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)?);
        }
        let stages = [vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(self.shaders[0]).name(c"main"),
                      vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(self.shaders[1]).name(c"main")];
        self.layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&[self.descriptor_layout])
            .push_constant_ranges(&[vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::FRAGMENT, offset: 0, size: 48 }]), None)?;
        let vertices = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default().polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE).front_face(vk::FrontFace::COUNTER_CLOCKWISE).line_width(1.0);
        let multi = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let colour = [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::R
            | vk::ColorComponentFlags::G | vk::ColorComponentFlags::B | vk::ColorComponentFlags::A)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&colour);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let create = vk::GraphicsPipelineCreateInfo::default().stages(&stages).vertex_input_state(&vertices)
            .input_assembly_state(&assembly).viewport_state(&viewport).rasterization_state(&raster)
            .multisample_state(&multi).color_blend_state(&blend).dynamic_state(&dynamic)
            .layout(self.layout).render_pass(self.pass).subpass(0);
        match d.create_graphics_pipelines(vk::PipelineCache::null(), &[create], None) {
            Ok(p) => self.pipeline = p[0],
            Err((partial, error)) => { for p in partial { d.destroy_pipeline(p, None); } return Err(error.into()); }
        }
        for shader in self.shaders.drain(..) { d.destroy_shader_module(shader, None); }
        Ok(())
    } }

    pub fn render(&self, eye: usize, image_index: usize, view: &xr::View, capture: bool) -> Result<Option<Vec<u8>>> {
        let target = &self.eyes[eye];
        ensure!(image_index < target.images.len(), "XR swapchain index out of bounds");
        let image = &target.images[image_index];
        let readback = if capture { Some(Buffer::host(self.gpu.clone(), target.extent.width as u64
            * target.extent.height as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST)?) } else { None };
        self.begin()?;
        unsafe {
            let d = &self.gpu.device;
            let rect = vk::Rect2D { offset: vk::Offset2D::default(), extent: target.extent };
            d.cmd_begin_render_pass(self.cmd, &vk::RenderPassBeginInfo::default().render_pass(self.pass)
                .framebuffer(image.framebuffer).render_area(rect), vk::SubpassContents::INLINE);
            d.cmd_set_viewport(self.cmd, 0, &[vk::Viewport { x: 0.0, y: 0.0, width: target.extent.width as f32,
                height: target.extent.height as f32, min_depth: 0.0, max_depth: 1.0 }]);
            d.cmd_set_scissor(self.cmd, 0, &[rect]);
            d.cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            d.cmd_bind_descriptor_sets(self.cmd, vk::PipelineBindPoint::GRAPHICS, self.layout, 0, &[self.descriptor], &[]);
            let q = view.pose.orientation;
            let constants = [view.fov.angle_left.tan(), view.fov.angle_right.tan(), view.fov.angle_up.tan(), view.fov.angle_down.tan(),
                q.x, q.y, q.z, q.w, self.yaw.cos(), self.yaw.sin(), 0.0, 0.0];
            let bytes = std::slice::from_raw_parts(constants.as_ptr() as *const u8, 48);
            d.cmd_push_constants(self.cmd, self.layout, vk::ShaderStageFlags::FRAGMENT, 0, bytes);
            d.cmd_draw(self.cmd, 3, 1, 0, 0);
            d.cmd_end_render_pass(self.cmd);
            if let Some(buffer) = &readback {
                d.cmd_pipeline_barrier(self.cmd, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT, vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(), &[], &[], &[image_barrier(image.image,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE, vk::AccessFlags::TRANSFER_READ)]);
                d.cmd_copy_image_to_buffer(self.cmd, image.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buffer.handle,
                    &[vk::BufferImageCopy::default().image_subresource(colour_layers())
                        .image_extent(vk::Extent3D { width: target.extent.width, height: target.extent.height, depth: 1 })]);
                let host = [vk::BufferMemoryBarrier::default().src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ).src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED).buffer(buffer.handle).size(vk::WHOLE_SIZE)];
                d.cmd_pipeline_barrier(self.cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(), &[], &host, &[]);
                d.cmd_pipeline_barrier(self.cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::DependencyFlags::empty(), &[], &[], &[image_barrier(image.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::AccessFlags::TRANSFER_READ, vk::AccessFlags::COLOR_ATTACHMENT_WRITE)]);
            }
        }
        self.submit()?;
        if let Some(buffer) = readback {
            let mut rgba = buffer.read()?;
            if self.format == vk::Format::B8G8R8A8_SRGB { for p in rgba.chunks_exact_mut(4) { p.swap(0, 2); } }
            Ok(Some(rgba))
        } else { Ok(None) }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) { unsafe {
        let _ = self.gpu.device.device_wait_idle();
        self.eyes.clear();
        let d = &self.gpu.device;
        d.destroy_pipeline(self.pipeline, None);
        d.destroy_pipeline_layout(self.layout, None);
        for shader in self.shaders.drain(..) { d.destroy_shader_module(shader, None); }
        d.destroy_render_pass(self.pass, None);
        d.destroy_descriptor_pool(self.descriptor_pool, None);
        d.destroy_descriptor_set_layout(self.descriptor_layout, None);
        d.destroy_sampler(self.sampler, None);
        d.destroy_image_view(self.image_view, None);
        d.destroy_image(self.image, None);
        d.free_memory(self.image_memory, None);
        d.destroy_fence(self.fence, None);
        d.destroy_command_pool(self.pool, None);
    } }
}

fn colour_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1)
}
fn colour_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1)
}
fn image_barrier(image: vk::Image, old: vk::ImageLayout, new: vk::ImageLayout,
                 src: vk::AccessFlags, dst: vk::AccessFlags) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default().image(image).old_layout(old).new_layout(new)
        .src_access_mask(src).dst_access_mask(dst).src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED).subresource_range(colour_range())
}
