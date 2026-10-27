//! GPU-контекст рендера: wgpu instance, adapter, device, queue и учёт потери устройства.
//!
//! Вынесено из `shell.rs` в сессии 02 плана `user/gles2-renderer/`: рядом с окном появилась
//! вторая «цель показа» (offscreen-текстура для conformance-тестов без окна), и создание
//! device/queue стало общим для обеих. Цель показа (surface окна или текстура) живёт в
//! [`super::present_target`], здесь — только то, что от неё не зависит.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use render_shell_api::{RendererDeviceLossReason, RendererDeviceLost, RendererGpuDrainError};
use render_wgpu_video::required_wgpu_video_texture_features;
use tracing::{info, instrument};
use winit::window::Window;

use super::present_target::{PresentTarget, WindowPresentTarget};
use crate::SurfacePresentSettings;
use crate::surface_settings::{
    choose_opaque_alpha_mode, choose_present_mode, choose_surface_format,
};

/// GPU ресурсы рендера, не зависящие от цели показа.
pub(crate) struct GpuContext {
    /// wgpu instance — нужна импорту DMA-BUF (Vulkan-объекты того же стека).
    pub(crate) instance: wgpu::Instance,

    /// wgpu adapter — нужен импорту DMA-BUF и проверке расширений.
    pub(crate) adapter: wgpu::Adapter,

    /// Логическое устройство GPU, через которое создаются ресурсы.
    pub(crate) device: wgpu::Device,

    /// Очередь команд для отправки кадров и загрузок на GPU.
    pub(crate) queue: wgpu::Queue,

    /// Формат цели показа, под который собраны пайплайны.
    pub(crate) target_format: wgpu::TextureFormat,

    /// Typed device-lost state, который callback WGPU публикует lifecycle owner-у.
    device_lost: Arc<Mutex<Option<GpuDeviceLost>>>,
}

/// Доказанная потеря устройства в форме wgpu (до перевода в нейтральный словарь).
#[derive(Debug, Clone, PartialEq, Eq)]
struct GpuDeviceLost {
    /// Причина, сообщённая WGPU backend-ом.
    reason: wgpu::DeviceLostReason,

    /// Backend diagnostic message для логов и UI failure report-а.
    message: String,
}

impl GpuDeviceLost {
    /// Переводит причину wgpu в нейтральный словарь контракта.
    fn into_neutral(self) -> RendererDeviceLost {
        let reason = match self.reason {
            wgpu::DeviceLostReason::Unknown => RendererDeviceLossReason::Unknown,
            wgpu::DeviceLostReason::Destroyed => RendererDeviceLossReason::Destroyed,
        };
        RendererDeviceLost {
            reason,
            message: self.message,
        }
    }
}

/// Устройство, созданное под выбранный adapter (до выбора цели показа).
struct CreatedVideoDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    device_lost: Arc<Mutex<Option<GpuDeviceLost>>>,
}

impl GpuContext {
    /// Создаёт GPU контекст и цель показа для окна на готовом WGPU Vulkan instance
    /// (`vulkan_instance()` или instance пробы того же запуска).
    ///
    /// Последовательность:
    /// 1. WGPU Vulkan instance без display handle ownership (приходит снаружи)
    /// 2. Surface — привязка к окну
    /// 3. Adapter — выбор GPU, совместимого с surface
    /// 4. Device/Queue — с feature gates, нужными video renderer boundary
    /// 5. Surface configuration — настройка swapchain
    #[instrument(skip(window, instance), fields(window = "fastiplayer"))]
    pub(crate) async fn new_for_window(
        window: Arc<Window>,
        surface_present: SurfacePresentSettings,
        instance: wgpu::Instance,
    ) -> Result<(Self, PresentTarget)> {
        let surface = instance
            .create_surface(window.clone())
            .context("Could not create the window surface")?;

        info!("Requesting a GPU adapter");

        let adapter = request_low_power_adapter(&instance, Some(&surface))
            .await
            .context("Couldn't get a GPU adapter. Check Vulkan drivers")?;
        reject_software_adapter(&adapter)?;
        let created = create_video_device_for_adapter(&adapter).await?;

        let surface_caps = surface.get_capabilities(&adapter);

        // Предпочитаем 8-bit формат: текущий SDR/NV12 shader пишет обычный RGBA.
        let surface_format = choose_surface_format(&surface_caps.formats)?;

        let present_mode =
            choose_present_mode(&surface_caps.present_modes, surface_present.present_mode)?;
        // Окно всегда непрозрачное: прозрачные composite-режимы не выбираются.
        let alpha_mode = choose_opaque_alpha_mode(&surface_caps.alpha_modes)?;

        // 0 кадров latency недопустимо для swapchain — нормализуем к 1.
        let desired_maximum_frame_latency = surface_present.max_frame_latency.max(1);

        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency,
            // wgpu 30: `Auto` воспроизводит историческое поведение (sRGB для наших
            // 8-битных форматов). HDR-вывод на экран — отдельная фича из бэклога.
            color_space: wgpu::SurfaceColorSpace::Auto,
        };

        surface.configure(&created.device, &surface_config);

        info!(
            format = ?surface_format,
            present_mode = ?present_mode,
            desired_maximum_frame_latency,
            width = size.width,
            height = size.height,
            "Surface is set to be set up"
        );

        let target = PresentTarget::Window(WindowPresentTarget::new(
            window,
            surface,
            surface_config,
            surface_present,
        ));
        Ok((
            Self {
                instance,
                adapter,
                device: created.device,
                queue: created.queue,
                target_format: surface_format,
                device_lost: created.device_lost,
            },
            target,
        ))
    }

    /// Создаёт GPU контекст без окна для conformance-тестов (решение владельца, сессия 02).
    ///
    /// Adapter и device создаются тем же путём, что в production (Vulkan, video features,
    /// `VK_EXT_queue_family_foreign`), только без surface. `Ok(None)` — в системе нет
    /// Vulkan-адаптера: тест обязан явно сообщить о пропуске.
    #[cfg(test)]
    pub(crate) async fn new_headless(target_format: wgpu::TextureFormat) -> Result<Option<Self>> {
        let instance = vulkan_instance();
        let Ok(adapter) = request_low_power_adapter(&instance, None).await else {
            return Ok(None);
        };
        let created = create_video_device_for_adapter(&adapter).await?;
        Ok(Some(Self {
            instance,
            adapter,
            device: created.device,
            queue: created.queue,
            target_format,
            device_lost: created.device_lost,
            // Без окна прозрачной маски углов нет.
        }))
    }

    /// Дожидается завершения уже отправленной работы GPU перед заменой device/surface.
    pub(crate) fn wait_for_gpu_idle(&self, timeout: Duration) -> Result<(), RendererGpuDrainError> {
        if let Some(device_lost) = self.device_lost_state()? {
            return Err(RendererGpuDrainError::DeviceLost(
                device_lost.into_neutral(),
            ));
        }

        let poll_result = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(timeout),
        });

        if let Some(device_lost) = self.device_lost_state()? {
            return Err(RendererGpuDrainError::DeviceLost(
                device_lost.into_neutral(),
            ));
        }

        match poll_result {
            Ok(_) => Ok(()),
            Err(wgpu::PollError::Timeout) => Err(RendererGpuDrainError::Timeout),
            // Нейтральное имя прежнего `WrongSubmissionIndex`: API отклонил само ожидание.
            Err(wgpu::PollError::WrongSubmissionIndex(_, _)) => {
                Err(RendererGpuDrainError::WaitRejected)
            }
        }
    }

    /// Читает callback state без предположений о причине poll error-а.
    fn device_lost_state(&self) -> Result<Option<GpuDeviceLost>, RendererGpuDrainError> {
        self.device_lost
            .lock()
            .map(|state| state.clone())
            .map_err(|_| RendererGpuDrainError::DeviceLostStatePoisoned)
    }
}

/// WGPU Vulkan instance без владения display handle (как было в `GpuContext::new`).
///
/// GPU-тесты шелла берут этот же instance: production работает только через Vulkan, а GL-бэкенд
/// wgpu в тестах открыл бы тот же `EGLDisplay`, что и GLES2-рендер паритетных тестов (Mesa отдаёт
/// один display на платформу), и `eglTerminate` одного убивал бы другой (сессия 04).
pub(crate) fn vulkan_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

/// Выбирает энергоэффективный adapter; с surface — только совместимый с ним. Пробу выбора
/// ([`crate::probe`]) делает тот же вызов без surface: проба и создание оценивают один адаптер.
pub(crate) async fn request_low_power_adapter(
    instance: &wgpu::Instance,
    compatible_surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, wgpu::RequestAdapterError> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface,
            force_fallback_adapter: false,
            // wgpu 30: `false` сохраняет прежнее поведение — реальные лимиты адаптера
            // без округления до «корзин» (защита от fingerprinting нужна браузерам).
            apply_limit_buckets: false,
        })
        .await?;

    let adapter_info = adapter.get_info();
    info!(
        name = %adapter_info.name,
        vendor = %adapter_info.vendor,
        device_type = ?adapter_info.device_type,
        backend = ?adapter_info.backend,
        "Selected GPU adapter"
    );
    Ok(adapter)
}

/// Не принимает программный адаптер (lavapipe/llvmpipe) молча.
///
/// `request_adapter` вернёт CPU-растеризатор, если других адаптеров нет. Играть через него
/// дороже, чем через GLES2 на аппаратном GPU, поэтому создание рендера падает с понятной
/// ошибкой, и каталог приложения идёт на следующего кандидата. Только для окна: GPU-тесты
/// без окна (`new_headless`) на CI штатно работают на lavapipe.
fn reject_software_adapter(adapter: &wgpu::Adapter) -> Result<()> {
    let adapter_info = adapter.get_info();
    ensure_hardware_adapter(adapter_info.device_type, &adapter_info.name)
}

/// Чистое правило [`reject_software_adapter`]: `Cpu` — отказ с названием адаптера.
pub(crate) fn ensure_hardware_adapter(
    device_type: wgpu::DeviceType,
    adapter_name: &str,
) -> Result<()> {
    if device_type == wgpu::DeviceType::Cpu {
        anyhow::bail!(
            "Only a software Vulkan adapter is available ({adapter_name}); a hardware GPU is required"
        );
    }
    Ok(())
}

/// Создаёт device/queue с video features и подписку на потерю устройства.
async fn create_video_device_for_adapter(adapter: &wgpu::Adapter) -> Result<CreatedVideoDevice> {
    let required_features = required_wgpu_video_texture_features(adapter);
    info!(
        nv12_texture_format = required_features.contains(wgpu::Features::TEXTURE_FORMAT_NV12),
        p010_texture_format = required_features.contains(wgpu::Features::TEXTURE_FORMAT_P010),
        texture_format_16bit_norm =
            required_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
        "WGPU device with video texture features"
    );

    // Устройство создаётся с `VK_EXT_queue_family_foreign`, если adapter его даёт:
    // это позволяет video renderer-у переиспользовать DMA-BUF image между кадрами.
    let (device, queue, foreign_queue_family) = crate::vulkan_device::create_video_device(
        adapter,
        wgpu::DeviceDescriptor {
            label: Some("fastiplayer device"),
            required_features,
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        },
    )
    .await?;
    info!(
        foreign_queue_family = foreign_queue_family.diagnostic_label(),
        "WGPU device external memory synchronization"
    );

    let device_lost = Arc::new(Mutex::new(None));
    let device_lost_callback_state = device_lost.clone();
    device.set_device_lost_callback(move |reason, message| {
        match device_lost_callback_state.lock() {
            Ok(mut state) => {
                *state = Some(GpuDeviceLost { reason, message });
            }
            Err(_) => {
                tracing::error!("Could not write WGPU device-lost state: mutex poisoned");
            }
        }
    });

    Ok(CreatedVideoDevice {
        device,
        queue,
        device_lost,
    })
}
