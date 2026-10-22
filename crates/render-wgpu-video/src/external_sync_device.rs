//! Создание WGPU device с расширением Vulkan, которое нужно video renderer-у
//! для явной синхронизации с аппаратным декодером.
//!
//! Зачем. Рендерер переиспользует импортированные DMA-BUF image между кадрами
//! (cpu-parity сессия 03) и оформляет передачу владения «декодер ↔ graphics
//! queue» через `VK_QUEUE_FAMILY_FOREIGN_EXT`. Указывать это семейство в барьере
//! можно только при включённом `VK_EXT_queue_family_foreign`
//! (`VUID-VkImageMemoryBarrier-srcQueueFamilyIndex-09099`). wgpu-hal 30 его не
//! включает, поэтому устройство создаётся через официальный хук wgpu-hal
//! `open_with_callback`: к списку расширений добавляется одно наше, остальное
//! (features, limits, queue) остаётся ровно таким, каким его собрал бы wgpu, а
//! готовое устройство отдаётся wgpu через `Adapter::create_device_from_hal`.
//!
//! Модуль живёт в render-wgpu-video, потому что именно этот crate знает, что
//! нужно video path-у от устройства (как и `required_wgpu_video_texture_features`);
//! shell и GPU-тесты только вызывают его. Fail-closed: `Ok(None)` означает, что
//! расширения нет, и caller создаёт обычное устройство, а импорт DMA-BUF остаётся
//! покадровым.
//!
//! Вторым, необязательным, добавляется `VK_EXT_external_memory_host` (cpu-parity 08b):
//! с ним программный кадр FFmpeg импортируется в GPU прямо из памяти декодера, без
//! CPU-копии в staging (`video::host_planar_upload::host_memory_import`). Если adapter
//! его не даёт, устройство создаётся как раньше, а кадры идут через staging.

use anyhow::{Context, Result};

use crate::dma_buf_import::foreign_queue_family_extension_name;
use crate::video::host_memory_import_extension_name;

/// Создаёт device/queue с `VK_EXT_queue_family_foreign`.
///
/// `Ok(None)` — adapter не Vulkan или расширение недоступно; `Err` — расширение
/// есть, но устройство не создалось (caller решает, откатываться ли).
pub fn create_video_device_with_external_sync(
    adapter: &wgpu::Adapter,
    descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Result<Option<(wgpu::Device, wgpu::Queue)>> {
    let extension_name = foreign_queue_family_extension_name();

    // SAFETY: hal adapter только читается (список расширений) и используется для
    // `open_with_callback` на том же adapter-е; guard не переживает функцию.
    let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::vulkan::Api>() };
    let Some(hal_adapter) = hal_adapter else {
        return Ok(None);
    };
    if !hal_adapter
        .physical_device_capabilities()
        .supports_extension(extension_name)
    {
        return Ok(None);
    }
    let host_memory_extension_name = host_memory_import_extension_name();
    let host_memory_import_supported = hal_adapter
        .physical_device_capabilities()
        .supports_extension(host_memory_extension_name);

    // `request_device` сам проверяет features/limits против adapter-а; hal-путь
    // этого не делает, поэтому те же проверки выполняются здесь явно.
    let missing_features = descriptor.required_features - adapter.features();
    anyhow::ensure!(
        missing_features.is_empty(),
        "Vulkan adapter lacks required wgpu features: {missing_features:?}"
    );
    anyhow::ensure!(
        descriptor.required_limits.check_limits(&adapter.limits()),
        "Vulkan adapter limits are below the requested wgpu limits"
    );

    // SAFETY (контракт `open_with_callback`): callback только ДОБАВЛЯЕТ расширения,
    // поддержку которых adapter подтвердил выше; features/limits и queue create info
    // не меняются и не урезаются.
    let open_device = unsafe {
        hal_adapter.open_with_callback(
            descriptor.required_features,
            &descriptor.required_limits,
            &descriptor.memory_hints,
            Some(Box::new(
                |args: wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
                    // Дубликат в списке расширений запрещён
                    // (`VUID-vkCreateDevice-ppEnabledExtensionNames-01840`).
                    if !args.extensions.contains(&extension_name) {
                        args.extensions.push(extension_name);
                    }
                    if host_memory_import_supported
                        && !args.extensions.contains(&host_memory_extension_name)
                    {
                        args.extensions.push(host_memory_extension_name);
                    }
                },
            )),
        )
    }
    .context("open Vulkan device with VK_EXT_queue_family_foreign")?;

    // SAFETY: `open_device` создан из hal adapter-а этого же `adapter`, а
    // `descriptor.required_features` — ровно те features, с которыми он открыт.
    let (device, queue) = unsafe {
        adapter.create_device_from_hal::<wgpu::hal::vulkan::Api>(open_device, descriptor)
    }
    .context("wrap Vulkan device with VK_EXT_queue_family_foreign into wgpu")?;
    Ok(Some((device, queue)))
}
