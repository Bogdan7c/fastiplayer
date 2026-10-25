//! Типизированная ошибка render bridge на границе shell -> worker.
//!
//! Вынесено из `worker.rs` (лимит размера модуля): это цельный словарь ошибок рендера
//! со своими конструкторами и переводом в `PlayerError`, у него один владелец.

use video_present_core::VideoFrameLease;

use crate::{PlayerError, PlayerErrorKind};

/// Категория ошибки render bridge на границе shell -> worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerRenderErrorKind {
    /// Render thread не смог получить renderer resource по handle lease-а.
    MissingRenderResources,

    /// Backend resource lookup завершился poisoned/fatal состоянием.
    RenderResourceLookupFailed,

    /// Renderer отказал decoded frame metadata или plane contract.
    UnsupportedFrameFormat,

    /// Renderer device/surface не смог завершить render frame.
    RenderDeviceLost,

    /// Смена видеодекодера посреди файла (например, переход HW -> SW режима Dynamic)
    /// провалилась после установки нового бэкенда: видео не может продолжиться.
    /// Это не отказ видеокарты, поэтому пользователь не должен видеть текст про GPU.
    VideoBackendSwitchFailed,
}

/// Типизированная ошибка render bridge, которую shell отправляет в worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRenderError {
    /// Машиночитаемая категория render ошибки.
    pub kind: PlayerRenderErrorKind,

    /// Render generation кадра, если ошибка связана с конкретным lease.
    pub render_generation: Option<u64>,

    /// Opaque frame handle, если ошибка связана с конкретным decoded frame.
    pub frame_handle: Option<u64>,

    /// Сообщение для logs/UI без backend-specific Debug payload.
    pub message: String,
}

impl PlayerRenderError {
    /// Создаёт ошибку отсутствующего renderer resource для конкретного lease-а.
    #[must_use]
    pub fn missing_render_resources(lease: &VideoFrameLease) -> Self {
        Self {
            kind: PlayerRenderErrorKind::MissingRenderResources,
            render_generation: Some(lease.render_generation()),
            frame_handle: Some(lease.resource_handle().0),
            message: format!(
                "Render resources are missing for {} frame handle {} in generation {}",
                lease.decoded_frame().format(),
                lease.resource_handle().0,
                lease.render_generation()
            ),
        }
    }

    /// Создаёт ошибку fatal renderer resource lookup для конкретного lease-а.
    #[must_use]
    pub fn render_resource_lookup_failed(lease: &VideoFrameLease) -> Self {
        Self {
            kind: PlayerRenderErrorKind::RenderResourceLookupFailed,
            render_generation: Some(lease.render_generation()),
            frame_handle: Some(lease.resource_handle().0),
            message: format!(
                "Render resource lookup failed for {} frame handle {} in generation {}",
                lease.decoded_frame().format(),
                lease.resource_handle().0,
                lease.render_generation()
            ),
        }
    }

    /// Создаёт ошибку renderer boundary validation для конкретного lease-а.
    #[must_use]
    pub fn unsupported_frame_format(lease: &VideoFrameLease, message: impl Into<String>) -> Self {
        Self {
            kind: PlayerRenderErrorKind::UnsupportedFrameFormat,
            render_generation: Some(lease.render_generation()),
            frame_handle: Some(lease.resource_handle().0),
            message: message.into(),
        }
    }

    /// Создаёт ошибку отказа render device/surface.
    #[must_use]
    pub fn render_device_lost(message: impl Into<String>) -> Self {
        Self {
            kind: PlayerRenderErrorKind::RenderDeviceLost,
            render_generation: None,
            frame_handle: None,
            message: message.into(),
        }
    }

    /// Создаёт ошибку провала смены видеодекодера посреди файла.
    #[must_use]
    pub fn video_backend_switch_failed(message: impl Into<String>) -> Self {
        Self {
            kind: PlayerRenderErrorKind::VideoBackendSwitchFailed,
            render_generation: None,
            frame_handle: None,
            message: message.into(),
        }
    }

    /// Конвертирует typed render error в существующий player error snapshot contract.
    #[must_use]
    pub fn to_player_error(&self) -> PlayerError {
        let kind = match self.kind {
            PlayerRenderErrorKind::MissingRenderResources
            | PlayerRenderErrorKind::RenderResourceLookupFailed
            | PlayerRenderErrorKind::UnsupportedFrameFormat => {
                PlayerErrorKind::UnsupportedRenderFormat
            }
            PlayerRenderErrorKind::RenderDeviceLost => PlayerErrorKind::RenderDeviceLost,
            // Внутренний сбой плеера: ни формат кадра, ни видеокарта тут ни при чём.
            PlayerRenderErrorKind::VideoBackendSwitchFailed => PlayerErrorKind::RuntimeError,
        };

        PlayerError::new(
            kind,
            format!("Video render bridge failed: {}", self.message),
        )
    }
}
