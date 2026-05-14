use anyhow::Result;
use tracing::info;
use wgpu::hal::api::Vulkan;

/// Выбирает wgpu texture features для video import/render boundary.
fn required_video_texture_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let mut features = wgpu::Features::TEXTURE_FORMAT_NV12;

    if adapter
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_P010)
    {
        features |= wgpu::Features::TEXTURE_FORMAT_P010;
    }

    if adapter
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    {
        features |= wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
    }

    features
}

/// Единый Vulkan instance для decode + render.
///
/// Владеет `wgpu::Instance`, созданной из `wgpu_hal::vulkan::Instance::init_with_callback()`
/// с подключенными video queue extensions (если доступны). Это позволяет:
/// - иметь один Vulkan instance для wgpu render и Vulkan Video decode;
/// - добавлять video extensions на уровне instance creation;
/// - получать доступ к `ash::Instance` через wgpu_hal guard.
///
/// Если video extensions недоступны — создаётся обычный wgpu::Instance
/// (рендеринг работает, decode будет unavailable).
pub struct UnifiedVulkanInstance {
    pub wgpu_instance: wgpu::Instance,
}

impl UnifiedVulkanInstance {
    /// Создает UnifiedVulkanInstance с video queue extensions (best effort).
    ///
    /// Последовательность:
    /// 1. Пытаемся `wgpu_hal::vulkan::Instance::init_with_callback()` с video extensions.
    /// 2. Если падает — fallback на стандартный `wgpu::Instance::new()`.
    pub fn new() -> Result<Self> {
        let hal_desc = wgpu_hal::InstanceDescriptor {
            name: "youtube-player",
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            telemetry: None,
            display: None,
        };

        // Попытка 1: создать instance с video extensions.
        let hal_instance = unsafe {
            wgpu_hal::vulkan::Instance::init_with_callback(
                &hal_desc,
                // Video extensions (VK_KHR_video_queue, VK_KHR_video_decode_queue)
                // являются device extensions, не instance extensions.
                // Добавлять их в instance creation нельзя — Vulkan loader
                // не знает их как instance extensions и отклонит create_instance.
                None,
            )
        };

        match hal_instance {
            Ok(hal_instance) => {
                let wgpu_instance = unsafe { wgpu::Instance::from_hal::<Vulkan>(hal_instance) };
                info!("UnifiedVulkanInstance created from wgpu-hal");
                Ok(Self { wgpu_instance })
            }
            Err(e) => {
                info!(
                    error = %e,
                    "wgpu-hal init failed, falling back to standard wgpu::Instance::new()"
                );
                let wgpu_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::VULKAN,
                    flags: wgpu::InstanceFlags::default(),
                    backend_options: wgpu::BackendOptions::default(),
                    display: None,
                    memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                });
                Ok(Self { wgpu_instance })
            }
        }
    }

    /// Получить доступ к `ash::Instance` через wgpu_hal guard.
    ///
    /// Возвращает `None` если instance не Vulkan backend.
    pub fn with_ash_instance<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&ash::Instance) -> R,
    {
        unsafe {
            self.wgpu_instance
                .as_hal::<Vulkan>()
                .map(|hal| f(hal.shared_instance().raw_instance()))
        }
    }

    /// Получить доступ к `ash::Entry` через wgpu_hal guard.
    ///
    /// Возвращает `None` если instance не Vulkan backend.
    pub fn with_entry<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&ash::Entry) -> R,
    {
        unsafe {
            self.wgpu_instance
                .as_hal::<Vulkan>()
                .map(|hal| f(hal.shared_instance().entry()))
        }
    }

    /// Создаёт wgpu::Device с video decode extensions (best effort).
    ///
    /// Использует `wgpu_hal::vulkan::Adapter::open_with_callback` для добавления
    /// VK_KHR_video_queue, VK_KHR_video_decode_queue, VK_KHR_video_decode_vp9
    /// в enabled extensions, и video decode queue family в queue create infos.
    ///
    /// Если video device creation невозможна — fallback на стандартный `adapter.request_device()`.
    pub async fn create_device_with_video(
        &self,
        adapter: &wgpu::Adapter,
    ) -> Result<(wgpu::Device, wgpu::Queue)> {
        // Получаем ash::Instance и physical_device напрямую через hal guards.
        let (ash_inst, physical_device) = unsafe {
            let hal = self
                .wgpu_instance
                .as_hal::<Vulkan>()
                .ok_or_else(|| anyhow::anyhow!("Instance is not Vulkan backend"))?;
            let hal_adapter = adapter
                .as_hal::<Vulkan>()
                .ok_or_else(|| anyhow::anyhow!("Adapter is not Vulkan backend"))?;
            (
                hal.shared_instance().raw_instance(),
                hal_adapter.raw_physical_device(),
            )
        };

        // 1. Определяем video decode queue family.
        let queue_families =
            unsafe { ash_inst.get_physical_device_queue_family_properties(physical_device) };
        let video_decode_family = queue_families
            .iter()
            .enumerate()
            .find(|(_, qf)| qf.queue_flags.as_raw() & 0x00000020 != 0)
            .map(|(i, _)| i as u32);

        if let Some(family) = video_decode_family {
            info!(
                video_decode_queue_family = family,
                "Video decode queue family found"
            );
        } else {
            info!("Video decode queue family not found");
        }

        // 2. Проверяем поддерживаемые device extensions.
        let supported_exts =
            unsafe { ash_inst.enumerate_device_extension_properties(physical_device) };
        let has_video_exts = match supported_exts {
            Ok(exts) => {
                let needed = [
                    ash::khr::video_queue::NAME,
                    ash::khr::video_decode_queue::NAME,
                    c"VK_KHR_video_decode_vp9",
                ];
                needed.iter().all(|needed_name| {
                    exts.iter().any(|ext| {
                        let ext_name =
                            unsafe { std::ffi::CStr::from_ptr(ext.extension_name.as_ptr()) };
                        ext_name == *needed_name
                    })
                })
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to enumerate device extensions");
                false
            }
        };

        if !has_video_exts {
            info!("Video decode extensions not supported by device, using standard request_device");
            let features = required_video_texture_features(adapter);
            info!(
                p010_texture_format = features.contains(wgpu::Features::TEXTURE_FORMAT_P010),
                texture_format_16bit_norm =
                    features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
                "Requesting wgpu video texture features"
            );
            return adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("youtube-player device"),
                    required_features: features,
                    required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create wgpu device: {:?}", e));
        }

        // 3. Получаем hal adapter для open_with_callback.
        let hal_adapter = unsafe {
            adapter
                .as_hal::<Vulkan>()
                .ok_or_else(|| anyhow::anyhow!("Adapter is not Vulkan backend"))?
        };

        let features = required_video_texture_features(adapter);
        info!(
            p010_texture_format = features.contains(wgpu::Features::TEXTURE_FORMAT_P010),
            texture_format_16bit_norm =
                features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
            "Requesting wgpu video texture features"
        );
        let limits = wgpu::Limits::default().using_resolution(adapter.limits());
        let memory_hints = wgpu::MemoryHints::MemoryUsage;

        // 4. Открываем hal device с callback — добавляем video extensions и queue family.
        let hal_open_device = unsafe {
            hal_adapter
                .open_with_callback(
                    features,
                    &limits,
                    &memory_hints,
                    Some(Box::new(
                        move |args: wgpu_hal::vulkan::CreateDeviceCallbackArgs| {
                            args.extensions.push(ash::khr::video_queue::NAME);
                            args.extensions.push(ash::khr::video_decode_queue::NAME);
                            args.extensions.push(c"VK_KHR_video_decode_vp9");

                            if let Some(family) = video_decode_family {
                                let video_queue_info = ash::vk::DeviceQueueCreateInfo::default()
                                    .queue_family_index(family)
                                    .queue_priorities(&[1.0f32]);
                                args.queue_create_infos.push(video_queue_info);
                            }
                        },
                    )),
                )
                .map_err(|e| anyhow::anyhow!("Failed to open hal device: {:?}", e))?
        };

        // 5. Создаём wgpu device из hal device.
        let desc = wgpu::DeviceDescriptor {
            label: Some("youtube-player device"),
            required_features: features,
            required_limits: limits,
            memory_hints,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        };

        unsafe {
            adapter
                .create_device_from_hal::<Vulkan>(hal_open_device, &desc)
                .map_err(|e| anyhow::anyhow!("Failed to create wgpu device from hal: {:?}", e))
        }
    }
}
