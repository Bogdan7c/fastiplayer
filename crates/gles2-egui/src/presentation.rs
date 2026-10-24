//! Показ кадра: ожидание вертикальной развёртки, прозрачность окна, отпуск и возврат окна.
//!
//! Типы нейтральные: адаптер плеера (`render-gles2-shell`, сессия 09) переводит в них
//! `render_shell_api::SurfacePresentSettings`:
//! - `Auto`/`Fifo` → [`VsyncMode::WaitForVblank`], `Immediate` → [`VsyncMode::Immediate`],
//!   `Mailbox` в EGL нет — адаптер сообщает фолбэк;
//! - у EGL нет управления задержкой кадра (`max_frame_latency`): её держит драйвер, адаптер
//!   отвечает явным no-op-исходом.
//!
//! Инвариант «одно окно — одна surface» (решение владельца 22): пока окно отпущено
//! ([`DetachOutcome::Detached`]), EGLSurface на нём нет, а новая создаётся только
//! [`crate::Gles2Context::reattach_presentation`].

use crate::context::driver::EglCallError;
use crate::loss::ContextLoss;

/// Ждать ли вертикальной развёртки при показе (`eglSwapInterval`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VsyncMode {
    /// `eglSwapInterval(1)`: показ по развёртке, без разрывов. По умолчанию.
    WaitForVblank,
    /// `eglSwapInterval(0)`: показ сразу, возможны разрывы.
    Immediate,
}

impl VsyncMode {
    /// Интервал `eglSwapInterval`.
    pub(crate) const fn swap_interval(self) -> i32 {
        match self {
            Self::WaitForVblank => 1,
            Self::Immediate => 0,
        }
    }
}

/// Какую прозрачность окна просит вызывающий.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowAlphaRequest {
    /// Непрозрачное окно: углы квадратные.
    Opaque,
    /// Прозрачное окно, если драйвер даёт конфиг с альфой (скруглённые углы). Иначе честный
    /// фолбэк [`WindowAlphaMode::Opaque`] с причиной.
    TransparentPreferred,
}

/// Настройки показа от вызывающего (не захардкожены в crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PresentationSettings {
    /// Ожидание развёртки.
    pub vsync: VsyncMode,
    /// Прозрачность окна. Меняется только пересозданием контекста: конфиг EGL выбирается
    /// один раз.
    pub alpha: WindowAlphaRequest,
}

impl Default for PresentationSettings {
    /// Как у wgpu-рендера по умолчанию: развёртка + непрозрачное окно.
    fn default() -> Self {
        Self {
            vsync: VsyncMode::WaitForVblank,
            alpha: WindowAlphaRequest::Opaque,
        }
    }
}

/// Почему окно показывается непрозрачным.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpaqueWindowReason {
    /// Так попросил вызывающий.
    Requested,
    /// Конфига с альфой нет (на X11 — для visual окна): углы квадратные, как у wgpu при
    /// `Opaque`-surface.
    NoAlphaConfig,
    /// Цель без окна (offscreen-стенд): прозрачности показа нет вовсе.
    OffscreenTarget,
}

/// Что получилось с прозрачностью.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowAlphaMode {
    /// Кадр с альфой, композитор смешивает его как premultiplied (Wayland и ARGB-visual X11).
    /// Маска углов работает.
    PremultipliedTransparent,
    /// Окно непрозрачное, углы квадратные.
    Opaque {
        /// Почему.
        reason: OpaqueWindowReason,
        /// У буфера кадра всё же есть канал альфы (другого конфига не нашлось). Тогда художник
        /// обязан писать альфу = 1, иначе композитор просветит окно.
        framebuffer_has_alpha: bool,
    },
}

/// Смысл альфы offscreen-цели ([`crate::Gles2Context::offscreen_with_alpha`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OffscreenAlpha {
    /// Непрозрачная цель, как окно без альфы: углы квадратные.
    Opaque,
    /// Прозрачный premultiplied-слой, как окно с прозрачностью у композитора: маска углов
    /// работает ([`WindowAlphaMode::PremultipliedTransparent`]).
    PremultipliedLayer,
}

impl WindowAlphaMode {
    /// Можно ли скруглять углы окна маской.
    pub fn supports_transparent_corners(self) -> bool {
        matches!(self, Self::PremultipliedTransparent)
    }
}

/// Какой интервал показа реально стоит.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapIntervalOutcome {
    /// Как просили.
    AsRequested {
        /// Интервал.
        interval: i32,
    },
    /// EGL прижал интервал к границам конфига (`EGL_MIN/MAX_SWAP_INTERVAL`, EGL 1.5, 3.10.3).
    Clamped {
        /// Что просили.
        requested: i32,
        /// Что стоит.
        applied: i32,
    },
}

impl SwapIntervalOutcome {
    /// Интервал, который реально стоит.
    pub fn applied_interval(self) -> i32 {
        match self {
            Self::AsRequested { interval } => interval,
            Self::Clamped { applied, .. } => applied,
        }
    }

    /// Исход для интервала и границ конфига (чистая функция, как это делает EGL).
    pub(crate) fn for_config(requested: i32, minimum: i32, maximum: i32) -> Self {
        let applied = requested.clamp(minimum.min(maximum), maximum.max(minimum));
        if applied == requested {
            Self::AsRequested {
                interval: requested,
            }
        } else {
            Self::Clamped { requested, applied }
        }
    }
}

/// Оконная система цели (для диагностики и решений вызывающего).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowSystem {
    /// Нативный Wayland (`wl_egl_window`).
    Wayland,
    /// X11 (в том числе XWayland).
    X11,
    /// Без окна (EGL surfaceless).
    Offscreen,
}

/// Состояние показа сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PresentationState {
    /// Оконная система.
    pub window_system: WindowSystem,
    /// Последние принятые настройки.
    pub settings: PresentationSettings,
    /// Что вышло с прозрачностью.
    pub alpha: WindowAlphaMode,
    /// Что вышло с интервалом (`None` у offscreen-цели).
    pub swap_interval: Option<SwapIntervalOutcome>,
    /// Отпущено ли окно.
    pub detached: bool,
}

/// Исход перенастройки показа на месте.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresentationReconfigured {
    /// Интервал применён.
    Applied(SwapIntervalOutcome),
    /// Настройки уже действуют — драйвер не трогали (no-op отличим от применения).
    AlreadyActive,
}

/// Исход отпуска окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[must_use]
pub enum DetachOutcome {
    /// EGLSurface уничтожена, конфиг и контекст сохранены.
    Detached,
    /// Окно уже было отпущено.
    AlreadyDetached,
}

/// Исход возврата окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReattachOutcome {
    /// Новая EGLSurface на том же display/конфиге.
    Reattached,
    /// Окно и так было привязано.
    AlreadyAttached,
}

/// Отказ операций показа.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresentationError {
    /// Окно отпущено: перенастраивать нечего.
    #[error("presentation is detached from the window")]
    Detached,
    /// Прозрачность выбирается конфигом EGL при создании; смена — только пересозданием.
    #[error("window alpha can only change by recreating the GLES2 context")]
    AlphaChangeRequiresRecreation,
    /// У offscreen-цели нет окна.
    #[error("offscreen target has no window presentation")]
    NotAWindowTarget,
    /// Новая EGLSurface на окне не создалась (возврат окна).
    #[error("EGL window surface could not be created: {0}")]
    SurfaceCreation(EglCallError),
    /// Контекст потерян.
    #[error("GLES2 context lost: {0}")]
    ContextLost(ContextLoss),
    /// Отказ драйвера.
    #[error("EGL presentation call failed: {0}")]
    Driver(EglCallError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_interval_is_clamped_to_config_bounds() {
        assert_eq!(
            SwapIntervalOutcome::for_config(1, 0, 4),
            SwapIntervalOutcome::AsRequested { interval: 1 }
        );
        assert_eq!(
            SwapIntervalOutcome::for_config(0, 1, 1),
            SwapIntervalOutcome::Clamped {
                requested: 0,
                applied: 1
            }
        );
        // Перевёрнутые границы от сломанного драйвера не роняют clamp (паника std).
        assert_eq!(
            SwapIntervalOutcome::for_config(3, 2, 1).applied_interval(),
            2
        );
    }

    #[test]
    fn only_transparent_mode_rounds_corners() {
        assert!(WindowAlphaMode::PremultipliedTransparent.supports_transparent_corners());
        assert!(
            !WindowAlphaMode::Opaque {
                reason: OpaqueWindowReason::NoAlphaConfig,
                framebuffer_has_alpha: false
            }
            .supports_transparent_corners()
        );
    }
}
