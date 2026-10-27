//! Нейтральные настройки показа кадров, которые приложение передаёт рендеру при создании.
//!
//! Перевод в API конкретного рендера (wgpu `PresentMode`/`CompositeAlphaMode`, EGL swap
//! interval) делает сам рендер. Применяются при создании surface, не live.

/// Предпочтительный present mode swapchain-а в нейтральной форме.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPresentMode {
    /// Авто: предпочесть FIFO (VSync), иначе первый доступный режим backend-а.
    Auto,
    /// FIFO: классический VSync, present блокирует до кадрового интервала.
    Fifo,
    /// Mailbox: новый кадр вытесняет очередь.
    Mailbox,
    /// Immediate: без синхронизации, поэтому возможен tearing.
    Immediate,
}

/// Нейтральные surface present настройки, прокидываемые в shell из композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfacePresentSettings {
    /// Предпочтительный present mode.
    pub present_mode: ShellPresentMode,
    /// Желаемая глубина swapchain latency; ноль нормализуется к единице.
    pub max_frame_latency: u32,
}

impl Default for SurfacePresentSettings {
    fn default() -> Self {
        Self {
            present_mode: ShellPresentMode::Auto,
            max_frame_latency: 2,
        }
    }
}
