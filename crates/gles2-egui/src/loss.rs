//! Потеря контекста: сброс GPU, зависание драйвера, событие питания.
//!
//! На Ironlake это не теория: в разведке 00 GPU дважды зависал под обычной нагрузкой, один раз со
//! сбросом контекста. Потеря — типизированное [`ContextLoss`], процесс живёт дальше.
//! Вызывающий (позже `render-gles2-shell`) переводит её в существующий путь «device lost»
//! контракта рендера: `VideoFrameImport::release_after_device_lost` + пересоздание рендера.
//!
//! Источники сигнала:
//! - `EGL_CONTEXT_LOST` из любого вызова EGL (`eglMakeCurrent`, `eglSwapBuffers`, EGL 1.5, 3.1);
//! - `glGetGraphicsResetStatus*` (`GL_EXT_robustness`/`GL_KHR_robustness`), если контекст создан
//!   со стратегией `EGL_LOSE_CONTEXT_ON_RESET_EXT`.

use std::fmt;

/// `GL_NO_ERROR`: сброса не было.
const GL_NO_ERROR: u32 = 0;
/// `GL_GUILTY_CONTEXT_RESET_EXT`.
const GL_GUILTY_CONTEXT_RESET: u32 = 0x8253;
/// `GL_INNOCENT_CONTEXT_RESET_EXT`.
const GL_INNOCENT_CONTEXT_RESET: u32 = 0x8254;
/// `GL_UNKNOWN_CONTEXT_RESET_EXT`.
const GL_UNKNOWN_CONTEXT_RESET: u32 = 0x8255;

/// Почему контекст потерян.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextLossReason {
    /// Сброс вызвали команды этого контекста (`GUILTY`).
    GuiltyReset,
    /// Сброс вызвал кто-то другой (`INNOCENT`) — например, зависание соседнего процесса.
    InnocentReset,
    /// Драйвер не знает виновника (`UNKNOWN`) или вернул нестандартный код.
    UnknownReset,
    /// EGL сообщил `EGL_CONTEXT_LOST` без подробностей.
    EglContextLost,
}

/// Где заметили потерю.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextLossDetection {
    /// Ответ EGL на вызов (`eglMakeCurrent`, `eglSwapBuffers`, sync).
    EglCall,
    /// Опрос `glGetGraphicsResetStatus*`.
    ResetStatusQuery,
}

/// Потеря контекста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContextLoss {
    /// Почему.
    pub reason: ContextLossReason,
    /// Где заметили.
    pub detection: ContextLossDetection,
}

impl ContextLoss {
    /// Потеря по `EGL_CONTEXT_LOST`.
    pub(crate) const fn from_egl() -> Self {
        Self {
            reason: ContextLossReason::EglContextLost,
            detection: ContextLossDetection::EglCall,
        }
    }

    /// Потеря по коду `glGetGraphicsResetStatus*`; `None` — сброса не было.
    pub(crate) fn from_reset_status(status: u32) -> Option<Self> {
        let reason = match status {
            GL_NO_ERROR => return None,
            GL_GUILTY_CONTEXT_RESET => ContextLossReason::GuiltyReset,
            GL_INNOCENT_CONTEXT_RESET => ContextLossReason::InnocentReset,
            GL_UNKNOWN_CONTEXT_RESET => ContextLossReason::UnknownReset,
            // Нестандартный ненулевой код — тоже сброс: продолжать рисовать в такой контекст
            // нельзя.
            _ => ContextLossReason::UnknownReset,
        };
        Some(Self {
            reason,
            detection: ContextLossDetection::ResetStatusQuery,
        })
    }
}

impl fmt::Display for ContextLoss {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} (detected by {:?})",
            self.reason, self.detection
        )
    }
}

/// Здоровье контекста. После потери все операции отвечают `ContextLost`, в драйвер больше не
/// ходим: состояние GL после сброса не определено (спецификация `GL_EXT_robustness`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextHealth {
    /// Можно рисовать.
    Healthy,
    /// Потерян навсегда; нужен новый контекст.
    Lost(ContextLoss),
}

impl ContextHealth {
    /// `Err` с потерей, если контекст потерян.
    pub(crate) fn ensure_healthy(self) -> Result<(), ContextLoss> {
        match self {
            Self::Healthy => Ok(()),
            Self::Lost(loss) => Err(loss),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_status_codes_map_to_reasons() {
        assert_eq!(ContextLoss::from_reset_status(GL_NO_ERROR), None);
        let reasons = [
            (GL_GUILTY_CONTEXT_RESET, ContextLossReason::GuiltyReset),
            (GL_INNOCENT_CONTEXT_RESET, ContextLossReason::InnocentReset),
            (GL_UNKNOWN_CONTEXT_RESET, ContextLossReason::UnknownReset),
            (0x1234, ContextLossReason::UnknownReset),
        ];
        for (status, reason) in reasons {
            let loss = ContextLoss::from_reset_status(status);
            assert_eq!(
                loss,
                Some(ContextLoss {
                    reason,
                    detection: ContextLossDetection::ResetStatusQuery
                }),
                "status {status:#x}"
            );
        }
    }

    #[test]
    fn lost_health_stays_an_error() {
        let loss = ContextLoss::from_egl();
        assert_eq!(ContextHealth::Lost(loss).ensure_healthy(), Err(loss));
        assert_eq!(ContextHealth::Healthy.ensure_healthy(), Ok(()));
    }
}
