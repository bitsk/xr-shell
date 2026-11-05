// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result, ensure};
use ash::{vk, vk::Handle};
use openxr as xr;
use std::{
    ffi::{CStr, c_void},
    ops::Deref,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Default)]
pub struct ValidationStats {
    pub errors: AtomicU64,
    pub warnings: AtomicU64,
}

unsafe extern "system" fn validation_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut c_void,
) -> vk::Bool32 {
    unsafe {
        let stats = &*(user as *const ValidationStats);
        if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
            stats.errors.fetch_add(1, Ordering::Relaxed);
        } else {
            stats.warnings.fetch_add(1, Ordering::Relaxed);
        }
        if !data.is_null() && !(*data).p_message.is_null() {
            eprintln!(
                "VULKAN_VALIDATION: {}",
                CStr::from_ptr((*data).p_message).to_string_lossy()
            );
        }
    }
    vk::FALSE
}

pub struct DeviceOwner(ash::Device);
impl Deref for DeviceOwner {
    type Target = ash::Device;
    fn deref(&self) -> &ash::Device {
        &self.0
    }
}
impl Drop for DeviceOwner {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.device_wait_idle();
            self.0.destroy_device(None);
        }
    }
}
pub struct InstanceOwner {
    handle: ash::Instance,
    debug: Option<ash::ext::debug_utils::Instance>,
    messenger: vk::DebugUtilsMessengerEXT,
    pub validation: Arc<ValidationStats>,
}
impl Deref for InstanceOwner {
    type Target = ash::Instance;
    fn deref(&self) -> &ash::Instance {
        &self.handle
    }
}
impl Drop for InstanceOwner {
    fn drop(&mut self) {
        unsafe {
            if let Some(debug) = &self.debug {
                debug.destroy_debug_utils_messenger(self.messenger, None);
            }
            self.handle.destroy_instance(None);
        }
    }
}

// Drop order is intentional: device before instance, loader last. Child GPU
// objects hold Rc<Gpu> so an early return cannot unload their Vulkan functions.
pub struct Gpu {
    pub device: DeviceOwner,
    pub instance: InstanceOwner,
    pub physical: vk::PhysicalDevice,
    pub family: u32,
    pub queue: vk::Queue,
    pub memory: vk::PhysicalDeviceMemoryProperties,
    pub name: String,
    pub anisotropy: f32,
    _loader: ash::Entry,
}

impl Gpu {
    pub fn new(xr: &xr::Instance, system: xr::SystemId, validation: bool) -> Result<Rc<Self>> {
        let requirements = xr.graphics_requirements::<xr::Vulkan>(system)?;
        ensure!(
            requirements.min_api_version_supported <= xr::Version::new(1, 1, 0)
                && requirements.max_api_version_supported.major() >= 1,
            "runtime does not accept Vulkan 1.1"
        );
        unsafe {
            let entry = ash::Entry::load()?;
            let mut layers = Vec::new();
            let mut extensions = Vec::new();
            if validation {
                ensure!(
                    entry
                        .enumerate_instance_layer_properties()?
                        .iter()
                        .any(|p| CStr::from_ptr(p.layer_name.as_ptr())
                            == c"VK_LAYER_KHRONOS_validation"),
                    "requested Khronos validation layer is unavailable"
                );
                layers.push(c"VK_LAYER_KHRONOS_validation".as_ptr());
                extensions.push(ash::ext::debug_utils::NAME.as_ptr());
            }
            let app = vk::ApplicationInfo::default()
                .application_name(c"UKUI XR Shell")
                .application_version(1)
                .engine_name(c"xr-shell")
                .api_version(vk::API_VERSION_1_1);
            let create = vk::InstanceCreateInfo::default()
                .application_info(&app)
                .enabled_layer_names(&layers)
                .enabled_extension_names(&extensions);
            let raw = xr
                .create_vulkan_instance(
                    system,
                    std::mem::transmute(entry.static_fn().get_instance_proc_addr),
                    &create as *const _ as *const _,
                )?
                .map_err(vk::Result::from_raw)?;
            let mut instance = InstanceOwner {
                handle: ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(raw as _)),
                debug: None,
                messenger: vk::DebugUtilsMessengerEXT::null(),
                validation: Arc::new(ValidationStats::default()),
            };
            if validation {
                let debug = ash::ext::debug_utils::Instance::new(&entry, &instance);
                let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                    .message_severity(
                        vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                            | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                    )
                    .message_type(
                        vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                            | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                            | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                    )
                    .pfn_user_callback(Some(validation_message))
                    .user_data(Arc::as_ptr(&instance.validation) as *mut c_void);
                instance.messenger = debug.create_debug_utils_messenger(&info, None)?;
                instance.debug = Some(debug);
                println!("XR_VALIDATION_LAYER=VK_LAYER_KHRONOS_validation");
            }
            let physical = vk::PhysicalDevice::from_raw(
                xr.vulkan_graphics_device(system, instance.handle().as_raw() as _)? as _,
            );
            let properties = instance.get_physical_device_properties(physical);
            ensure!(
                properties.api_version >= vk::API_VERSION_1_1,
                "Vulkan 1.1 required"
            );
            let family = instance
                .get_physical_device_queue_family_properties(physical)
                .iter()
                .position(|p| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .context("no graphics queue")? as u32;
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&priorities)];
            let features = instance.get_physical_device_features(physical);
            let enabled = vk::PhysicalDeviceFeatures::default()
                .sampler_anisotropy(features.sampler_anisotropy != 0);
            let anisotropy = if features.sampler_anisotropy != 0 {
                properties.limits.max_sampler_anisotropy.min(2.)
            } else {
                1.
            };
            let create = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_features(&enabled);
            let raw = xr
                .create_vulkan_device(
                    system,
                    std::mem::transmute(entry.static_fn().get_instance_proc_addr),
                    physical.as_raw() as _,
                    &create as *const _ as *const _,
                )?
                .map_err(vk::Result::from_raw)?;
            let device = DeviceOwner(ash::Device::load(
                instance.fp_v1_0(),
                vk::Device::from_raw(raw as _),
            ));
            let queue = device.get_device_queue(family, 0);
            let memory = instance.get_physical_device_memory_properties(physical);
            let name = CStr::from_ptr(properties.device_name.as_ptr())
                .to_string_lossy()
                .into_owned();
            println!("XR_VULKAN_DEVICE={name}");
            eprintln!(
                "XR_TEXTURE_FILTER mipmap=linear anisotropy={anisotropy} atlas_max_lod=3 atlas_edge_padding=16"
            );
            Ok(Rc::new(Self {
                device,
                instance,
                physical,
                family,
                queue,
                memory,
                name,
                anisotropy,
                _loader: entry,
            }))
        }
    }

    pub fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32> {
        (0..self.memory.memory_type_count)
            .find(|&i| {
                bits & (1 << i) != 0
                    && self.memory.memory_types[i as usize]
                        .property_flags
                        .contains(flags)
            })
            .context("compatible Vulkan memory type not found")
    }

    pub fn session(
        &self,
        xr: &xr::Instance,
        system: xr::SystemId,
    ) -> Result<(
        xr::Session<xr::Vulkan>,
        xr::FrameWaiter,
        xr::FrameStream<xr::Vulkan>,
    )> {
        Ok(unsafe {
            xr.create_session::<xr::Vulkan>(
                system,
                &xr::vulkan::SessionCreateInfo {
                    instance: self.instance.handle().as_raw() as _,
                    physical_device: self.physical.as_raw() as _,
                    device: self.device.handle().as_raw() as _,
                    queue_family_index: self.family,
                    queue_index: 0,
                },
            )?
        })
    }
}

pub struct Buffer {
    gpu: Rc<Gpu>,
    pub handle: vk::Buffer,
    memory: vk::DeviceMemory,
    pub size: u64,
}
impl Buffer {
    pub fn host(gpu: Rc<Gpu>, size: u64, usage: vk::BufferUsageFlags) -> Result<Self> {
        let mut b = Self {
            gpu,
            handle: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            size,
        };
        unsafe {
            b.handle = b.gpu.device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?;
            let req = b.gpu.device.get_buffer_memory_requirements(b.handle);
            let ty = b.gpu.memory_type(
                req.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?;
            b.memory = b.gpu.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ty),
                None,
            )?;
            b.gpu.device.bind_buffer_memory(b.handle, b.memory, 0)?;
        }
        Ok(b)
    }
    pub fn write(&self, data: &[u8]) -> Result<()> {
        ensure!(data.len() as u64 <= self.size, "staging buffer overflow");
        unsafe {
            let mapped = self.gpu.device.map_memory(
                self.memory,
                0,
                self.size,
                vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped as *mut u8, data.len());
            self.gpu.device.unmap_memory(self.memory);
        }
        Ok(())
    }
    pub fn read(&self) -> Result<Vec<u8>> {
        unsafe {
            let mapped = self.gpu.device.map_memory(
                self.memory,
                0,
                self.size,
                vk::MemoryMapFlags::empty(),
            )?;
            let result =
                std::slice::from_raw_parts(mapped as *const u8, self.size as usize).to_vec();
            self.gpu.device.unmap_memory(self.memory);
            Ok(result)
        }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            self.gpu.device.destroy_buffer(self.handle, None);
            self.gpu.device.free_memory(self.memory, None);
        }
    }
}
