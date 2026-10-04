//! Пользовательская причина локальной ошибки, видимая через ошибку strong media-open.
//!
//! Отдельный модуль: `strong_media_open.rs` у лимита размера, а этот intent-метод —
//! самостоятельный кусок границы «ошибка открытия → причина для строки плейлиста».

use super::StrongMediaOpenError;
use crate::media_open::LocalOpenFailureReason;

impl StrongMediaOpenError {
    /// Причина отказа локального файла, если strong open закончился отказом подготовки.
    ///
    /// `None` — ошибка не про локальный файл (player, протокол, сеть, отмена): вызывающий
    /// код показывает прежний общий текст.
    pub(crate) const fn local_open_failure_reason(&self) -> Option<LocalOpenFailureReason> {
        match self {
            Self::Terminal(terminal) => terminal.local_open_failure_reason(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_open::{
        MediaOpenRequestId, MediaOpenTerminalOutcome, MediaPreparationFailureKind,
    };

    fn request_id() -> MediaOpenRequestId {
        MediaOpenRequestId::from_non_zero(std::num::NonZeroU64::MIN)
    }

    #[test]
    fn local_preparation_terminal_exposes_user_reason() {
        let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
            request_id: request_id(),
            kind: MediaPreparationFailureKind::LocalOpen(LocalOpenFailureReason::FileNotFound),
        });

        assert_eq!(
            error.local_open_failure_reason(),
            Some(LocalOpenFailureReason::FileNotFound)
        );
    }

    #[test]
    fn changed_source_terminal_maps_to_changed_during_open() {
        let error = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
            request_id: request_id(),
            kind: MediaPreparationFailureKind::LocalSourceChanged,
        });

        assert_eq!(
            error.local_open_failure_reason(),
            Some(LocalOpenFailureReason::ChangedDuringOpen)
        );
    }

    #[test]
    fn non_local_failures_have_no_local_reason() {
        let web_failure =
            StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
                request_id: request_id(),
                kind: MediaPreparationFailureKind::ExtractorOpen,
            });
        let player_failure =
            StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PlayerFailed {
                request_id: request_id(),
            });

        assert_eq!(web_failure.local_open_failure_reason(), None);
        assert_eq!(player_failure.local_open_failure_reason(), None);
        assert_eq!(
            StrongMediaOpenError::MissingTerminal.local_open_failure_reason(),
            None
        );
    }
}
