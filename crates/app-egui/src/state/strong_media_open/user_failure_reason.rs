//! Что увидит пользователь, если strong media-open не удался.
//!
//! Отдельный модуль: `strong_media_open.rs` у лимита размера, а эти intent-методы —
//! самостоятельная граница «ошибка открытия → причина для экрана/строки плейлиста».
//! Классификацию технических ошибок делают владельцы (`media_open::local::failure`,
//! `media_open::player_failure`); здесь только решение, показывать ли ошибку вообще.

use super::StrongMediaOpenError;
use crate::media_open::{
    MediaOpenStartError, MediaOpenTerminalOutcome, MediaOpenUserFailureReason,
    PlayerInstallFailureReason,
};

/// Исход strong open с точки зрения пользователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrongMediaOpenUserOutcome {
    /// Не ошибка для пользователя: открытие отменили или coordinator занят другим
    /// открытием. Сообщение не показываем, пишем только лог (решение владельца, сессия 03;
    /// отдельное «занято» — сессия 04).
    Silent,
    /// Показать «Не удалось открыть …» с этой причиной.
    Failed(MediaOpenUserFailureReason),
}

impl StrongMediaOpenError {
    /// Классифицированная причина неудачи, если она известна.
    ///
    /// `None` — ошибка без пользовательской классификации (нарушение протокола, web-отказ
    /// подготовки, отмена): строка плейлиста показывает прежний общий текст.
    pub(crate) const fn user_failure_reason(&self) -> Option<MediaOpenUserFailureReason> {
        match self {
            Self::Terminal(terminal) => terminal.user_failure_reason(),
            _ => None,
        }
    }

    /// Показывать ли ошибку пользователю и с какой причиной.
    ///
    /// Всё, что не отмена/«занято» и не классифицировано, — внутренняя ошибка плеера:
    /// лучше честное «внутренняя ошибка», чем английский debug-дамп протокола.
    pub(crate) const fn user_outcome(&self) -> StrongMediaOpenUserOutcome {
        match self {
            Self::Start(MediaOpenStartError::Busy)
            | Self::Terminal(MediaOpenTerminalOutcome::Cancelled { .. }) => {
                StrongMediaOpenUserOutcome::Silent
            }
            _ => match self.user_failure_reason() {
                Some(reason) => StrongMediaOpenUserOutcome::Failed(reason),
                None => {
                    StrongMediaOpenUserOutcome::Failed(MediaOpenUserFailureReason::PlayerInstall(
                        PlayerInstallFailureReason::InternalError,
                    ))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests;
