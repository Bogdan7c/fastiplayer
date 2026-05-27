use std::time::Duration;

use render_wgpu_video::WgpuRenderableFrame;
use winit::window::Window;

/// Итог одного render-frame вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderFrameOutcome {
    /// Кадр был отправлен в swapchain и представлен.
    Presented(RenderFrameTiming),

    /// Кадр был пропущен из-за состояния surface/window.
    Dropped(RenderFrameDropReason),

    /// Video render path failed; caller must treat this as fatal media error.
    Failed(RenderFrameFailure),
}

/// Timing одного успешного submit/present участка render loop-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderFrameTiming {
    /// Время от отправки command buffer-а до возврата из `surface_texture.present()`.
    pub submit_present_elapsed: Duration,
}

/// Размер target-а и UI scale без раскрытия egui-wgpu типа наружу.
pub struct RenderScreenDescriptor {
    /// Размер swapchain target-а в пикселях.
    pub size_in_pixels: [u32; 2],

    /// UI scale, полученный от egui context.
    pub pixels_per_point: f32,
}

/// Входные данные одного полного кадра shell renderer-а.
///
/// App layer отвечает за egui tessellation и сбор video frame lease, а shell layer
/// получает уже готовый пакет данных для записи swapchain кадра.
pub struct RenderFrameInput<'frame> {
    /// Окно, для которого выполняется present notification.
    pub window: &'frame Window,

    /// Video frame boundary; `None` означает, что target нужно очистить в чёрный.
    pub video_frame: Option<&'frame WgpuRenderableFrame<'frame>>,

    /// Уже tessellated egui primitives.
    pub egui_paint_jobs: Vec<egui::epaint::ClippedPrimitive>,

    /// Изменения egui textures для текущего кадра.
    pub egui_textures_delta: egui::TexturesDelta,

    /// Размер target-а и UI scale для egui-wgpu.
    pub screen: RenderScreenDescriptor,
}

/// Ошибка video render path, которую app/player layer не должен превращать в fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderFrameFailure {
    /// Сообщение renderer-а для логов и UI.
    pub message: String,
}

impl RenderFrameFailure {
    /// Создаёт failure из renderer error.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Причина пропуска кадра renderer backend-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFrameDropReason {
    /// Surface acquisition не успел завершиться.
    SurfaceTimeout,

    /// Окно occluded, compositor не принимает кадр.
    SurfaceOccluded,

    /// Surface потерян; renderer выполнил reconfigure и ждёт следующий redraw.
    SurfaceLost,

    /// Surface validation error при acquisition.
    SurfaceValidation,

    /// Повторный acquisition после Outdated/Reconfigure тоже не дал frame.
    SurfaceOutdatedRecoveryFailed,
}
