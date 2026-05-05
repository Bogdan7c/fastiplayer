use anyhow::Result;
use ash::vk;
use std::ffi::CStr;
use tracing::info;

use crate::instance::UnifiedVulkanInstance;

/// Vulkan device с video decode capability.
///
/// Владеет `ash::Device` (клоном из wgpu device) и предоставляет:
/// - graphics queue (из wgpu);
/// - optional video decode queue (отдельный queue family);
/// - флаг `has_vp9_decode` — результат проверки расширений.
pub struct VulkanVideoDevice {
    pub ash_device: ash::Device,
    pub physical_device: vk::PhysicalDevice,
    pub graphics_queue: vk::Queue,
    pub graphics_queue_family: u32,
    pub video_decode_queue: Option<vk::Queue>,
    pub video_decode_queue_family: Option<u32>,
    pub has_vp9_decode: bool,
}

/// VK_QUEUE_VIDEO_DECODE_BIT_KHR = 0x00000020
const VK_QUEUE_VIDEO_DECODE_BIT_KHR: u32 = 0x00000020;

/// Имя VP9 decode extension (не включено в ash 0.38, определяем вручную).
const VK_KHR_VIDEO_DECODE_VP9: &[u8] = b"VK_KHR_video_decode_vp9\0";

impl VulkanVideoDevice {
    /// Извлекает Vulkan handles из wgpu device и определяет video decode capabilities.
    ///
    /// Требует `UnifiedVulkanInstance` для доступа к `ash::Instance` (query queue families
    /// и extensions).
    pub unsafe fn from_wgpu_device(
        unified: &UnifiedVulkanInstance,
        wgpu_device: &wgpu::Device,
    ) -> Result<Self> {
        use wgpu::hal::api::Vulkan;

        let hal_device = unsafe {
            wgpu_device
                .as_hal::<Vulkan>()
                .ok_or_else(|| anyhow::anyhow!("wgpu device is not Vulkan backend"))?
        };

        let raw_device = hal_device.raw_device();
        let physical_device = hal_device.raw_physical_device();
        let graphics_queue = hal_device.raw_queue();
        let graphics_queue_family = hal_device.queue_family_index();

        // Клонируем ash::Device handle (ash::Device — это Arc-обертка, clone дешевый).
        let ash_device = raw_device.clone();

        // Определяем video decode queue family через ash::Instance.
        let (video_decode_queue_family, video_decode_queue) = unified
            .with_ash_instance(|ash_inst| {
                let queue_families = unsafe {
                    ash_inst.get_physical_device_queue_family_properties(physical_device)
                };

                let video_family = queue_families
                    .iter()
                    .enumerate()
                    .find(|(_, qf)| qf.queue_flags.as_raw() & VK_QUEUE_VIDEO_DECODE_BIT_KHR != 0)
                    .map(|(i, _)| i as u32);

                let video_queue =
                    video_family.map(|family| unsafe { ash_device.get_device_queue(family, 0) });

                (video_family, video_queue)
            })
            .unwrap_or((None, None));

        // Проверяем наличие VK_KHR_video_decode_vp9 среди device extensions.
        let has_vp9_decode = unified
            .with_ash_instance(|ash_inst| {
                let extensions =
                    unsafe { ash_inst.enumerate_device_extension_properties(physical_device) };
                match extensions {
                    Ok(exts) => {
                        let vp9_name = CStr::from_bytes_with_nul(VK_KHR_VIDEO_DECODE_VP9).unwrap();
                        exts.iter().any(|ext| {
                            let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
                            name == vp9_name
                        })
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to enumerate device extensions");
                        false
                    }
                }
            })
            .unwrap_or(false);

        info!(
            physical_device = ?physical_device,
            graphics_queue_family = graphics_queue_family,
            video_decode_queue_family = ?video_decode_queue_family,
            has_vp9_decode = has_vp9_decode,
            "VulkanVideoDevice initialized"
        );

        Ok(Self {
            ash_device,
            physical_device,
            graphics_queue,
            graphics_queue_family,
            video_decode_queue,
            video_decode_queue_family,
            has_vp9_decode,
        })
    }

    /// Возвращает true если GPU поддерживает VK_KHR_video_decode_vp9.
    pub fn has_vp9_decode_support(&self) -> bool {
        self.has_vp9_decode
    }
}
