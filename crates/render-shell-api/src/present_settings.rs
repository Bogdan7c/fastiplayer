//! Нейтральные настройки показа кадров, которые приложение передаёт рендеру при создании.
//!
//! Перевод в API конкретного рендера (wgpu `PresentMode`/`CompositeAlphaMode`, EGL swap
//! interval и конфиг с альфой) делает сам рендер. Применяются при создании surface, не live.

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

/// Политика выбора alpha mode для desktop surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAlphaPreference {
    /// Выбирать только непрозрачную композицию для обычных прямоугольных окон.
    Opaque,
    /// Предпочесть подтверждённую прозрачность, но безопасно продолжить с opaque fallback.
    TransparentPreferred,
}

/// Нейтральные surface present настройки, прокидываемые в shell из композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfacePresentSettings {
    /// Предпочтительный present mode.
    pub present_mode: ShellPresentMode,
    /// Желаемая глубина swapchain latency; ноль нормализуется к единице.
    pub max_frame_latency: u32,
    /// Намерение вызывающего слоя относительно прозрачной desktop-композиции.
    pub alpha_preference: SurfaceAlphaPreference,
}

impl Default for SurfacePresentSettings {
    fn default() -> Self {
        Self {
            present_mode: ShellPresentMode::Auto,
            max_frame_latency: 2,
            alpha_preference: SurfaceAlphaPreference::Opaque,
        }
    }
}
