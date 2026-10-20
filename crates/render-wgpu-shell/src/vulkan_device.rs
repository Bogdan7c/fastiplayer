//! Создание WGPU device для shell-а: сначала с расширением, нужным video
//! renderer-у для переиспользования DMA-BUF image (см.
//! `render_wgpu_video::create_video_device_with_external_sync`), иначе обычный
//! `request_device`. Решение о fallback и его логирование — здесь, чтобы
//! renderer crate оставался без политики shell-а.

use anyhow::{Context, Result};
use render_wgpu_video::{
    create_video_device_with_external_sync, foreign_queue_family_extension_name,
};
use tracing::{info, warn};

/// Что удалось включить при создании устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignQueueFamilySupport {
    /// `VK_EXT_queue_family_foreign` включено; рендерер может переиспользовать image.
    Enabled,

    /// Расширения нет или adapter не Vulkan; импорт DMA-BUF остаётся покадровым.
    Unavailable,
}

impl ForeignQueueFamilySupport {
    /// Метка для логов и диагностики.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Создаёт device/queue, включая `VK_EXT_queue_family_foreign`, когда adapter его даёт.
pub(crate) async fn create_video_device(
    adapter: &wgpu::Adapter,
    descriptor: wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue, ForeignQueueFamilySupport)> {
    match create_video_device_with_external_sync(adapter, &descriptor) {
        Ok(Some((device, queue))) => {
            return Ok((device, queue, ForeignQueueFamilySupport::Enabled));
        }
        Ok(None) => {
            info!(
                extension = ?foreign_queue_family_extension_name(),
                "Vulkan adapter does not offer the foreign queue family extension; DMA-BUF images will be imported per frame"
            );
        }
        Err(error) => {
            warn!(
                error = %error,
                "Could not create the Vulkan device with the foreign queue family extension; falling back to the default wgpu device"
            );
        }
    }

    let (device, queue) = adapter
        .request_device(&descriptor)
        .await
        .context("Cannot create wgpu device with video texture features")?;
    Ok((device, queue, ForeignQueueFamilySupport::Unavailable))
}
