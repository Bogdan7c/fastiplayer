//! Жизненный цикл «показа» рендера на окне: перенастройка на месте и «отпустить/вернуть окно».
//!
//! Сессия 02a плана `user/gles2-renderer/`. Инвариант оконной системы: **у одного окна одна
//! surface**. Mesa WSI на Wayland заводит на каждую Vulkan-surface объект `wp_fifo_v1`, а
//! протокол запрещает второй такой объект на одном `wl_surface` (композитор рвёт соединение);
//! у EGL (будущий GLES2) на одном окне тоже одна `EGLSurface`. Поэтому:
//!
//! - изменение только настроек показа (present mode, задержка) применяется к ТЕКУЩЕЙ surface
//!   ([`crate::RenderShell::reconfigure_presentation`]) — без нового device и без пересоздания
//!   импорта кадров;
//! - полное пересоздание рендера сначала просит старый рендер «отпустить окно»
//!   ([`crate::RenderShell::detach_presentation`]) и только потом создаёт новый на том же окне;
//!   откат возвращает окно старому рендеру ([`crate::RenderShell::reattach_presentation`]).
//!
//! Все исходы типизированы: «применено», «уже так» (no-op), «отказ, прежняя конфигурация
//! работает» и «отказ, рабочей конфигурации нет» вызывающий код обязан различать.
//!
//! # Когда можно вызывать
//!
//! Только между кадрами: в момент вызова у приложения не должно быть живого
//! [`crate::AcquiredSurfaceFrame`] этого рендера (у wgpu перенастройка и уничтожение
//! swapchain при живом `SurfaceTexture` — паника). Приложение применяет настройки до
//! `acquire_surface_frame` своего тика, и билет не переживает тик.

use std::fmt;

use crate::ShellPresentMode;

/// Итог успешной перенастройки показа на месте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationReconfigured {
    /// Новая конфигурация применена так, как просили.
    AsRequested,

    /// Запрошенного present mode у surface нет: применён запасной режим по тем же правилам,
    /// что при создании рендера (FIFO, иначе первый доступный). Задержка применена.
    PresentModeFallback {
        /// Что просили.
        requested: ShellPresentMode,
    },

    /// Действующая конфигурация уже такая (например, `Auto` и `Fifo` дают один режим):
    /// surface не трогали, запомнены только новые запрошенные настройки.
    AlreadyActive,
}

/// Почему перенастройка показа на месте не применена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentationReconfigureError {
    /// Рендер сейчас не держит окно (после `detach_presentation`): применять не к чему.
    /// Ничего не изменено.
    PresentationDetached,

    /// Surface отклонила новую конфигурацию, прежняя восстановлена: показ продолжается.
    RejectedPreviousRestored {
        /// Сообщение API/драйвера о новой конфигурации.
        message: String,
    },

    /// Surface отклонила и новую, и прежнюю конфигурацию: у окна нет рабочей конфигурации
    /// показа. Рендер будет пробовать восстановить прежнюю на каждом `acquire_surface_frame`.
    PresentationLost {
        /// Сообщения об отказе новой и прежней конфигурации.
        message: String,
    },
}

impl fmt::Display for PresentationReconfigureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PresentationDetached => {
                formatter.write_str("renderer does not hold the window presentation")
            }
            Self::RejectedPreviousRestored { message } => write!(
                formatter,
                "surface rejected the new presentation; previous one restored: {message}"
            ),
            Self::PresentationLost { message } => write!(
                formatter,
                "surface rejected both the new and the previous presentation: {message}"
            ),
        }
    }
}

impl std::error::Error for PresentationReconfigureError {}

/// Итог «отпустить окно».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "повторное отпускание — no-op; вызывающий код обязан понимать, что окно уже было отпущено"]
pub enum PresentationDetachOutcome {
    /// Surface окна уничтожена (у Vulkan — swapchain и `VkSurfaceKHR`); конфигурация и само
    /// окно запомнены для возврата.
    Detached,

    /// Окно уже было отпущено: ничего не сделано.
    AlreadyDetached,
}

/// Итог «вернуть окно».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationReattachOutcome {
    /// Новая surface создана на прежнем окне и настроена прежней конфигурацией.
    Reattached,

    /// Рендер и так держит окно: ничего не сделано.
    AlreadyAttached,
}

/// Почему не удалось вернуть окно рендеру. Рендер остаётся без окна (кадров не выдаёт).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentationReattachError {
    /// Оконная система/API не дали новую surface на прежнем окне.
    SurfaceCreation {
        /// Сообщение API.
        message: String,
    },

    /// Новая surface несовместима с устройством рендера или его прежней конфигурацией
    /// (формат, present mode, режим прозрачности).
    IncompatibleSurface {
        /// Что именно не совпало.
        message: String,
    },

    /// Surface отклонила прежнюю конфигурацию.
    ConfigureRejected {
        /// Сообщение API/драйвера.
        message: String,
    },
}

impl fmt::Display for PresentationReattachError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SurfaceCreation { message } => {
                write!(
                    formatter,
                    "window surface could not be created again: {message}"
                )
            }
            Self::IncompatibleSurface { message } => {
                write!(formatter, "new window surface is incompatible: {message}")
            }
            Self::ConfigureRejected { message } => write!(
                formatter,
                "new window surface rejected the previous configuration: {message}"
            ),
        }
    }
}

impl std::error::Error for PresentationReattachError {}
