//! Что увидит пользователь, если strong media-open не удался.
//!
//! Отдельный модуль: `strong_media_open.rs` у лимита размера, а эти intent-методы —
//! самостоятельная граница «ошибка открытия → причина для экрана/строки плейлиста».
//! Классификацию технических ошибок делают владельцы (`media_open::local::failure`,
//! `media_open::player_failure`); здесь только решение, показывать ли ошибку вообще.

use player_core::MediaInstallCancellationCause;

use super::StrongMediaOpenError;
use crate::media_open::{
    MediaOpenStartError, MediaOpenTerminalOutcome, MediaOpenUserFailureReason,
    PlayerInstallFailureReason,
};

/// Исход strong open с точки зрения пользователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrongMediaOpenUserOutcome {
    /// Не ошибка для пользователя: открытие отменили. Сообщение не показываем, только лог
    /// (решение владельца, сессия 03).
    Cancelled,
    /// Coordinator занят другим открытием: не ошибка, а временное «Файл ещё открывается»
    /// (сессия 04).
    Busy,
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

    /// Открытие оборвал сам пользователь или завершение приложения (не ошибка).
    ///
    /// `true` только для доказанного `Cancelled` с причиной из списка ниже; любой
    /// другой исход (в том числе отмена по иным причинам) остаётся прежним.
    pub(crate) const fn is_cancelled_by_user_or_shutdown(&self) -> bool {
        match self {
            Self::Terminal(MediaOpenTerminalOutcome::Cancelled { cause, .. }) => {
                cancellation_cause_is_user_or_shutdown(*cause)
            }
            _ => false,
        }
    }

    /// Показывать ли ошибку пользователю и с какой причиной.
    ///
    /// Всё, что не отмена/«занято» и не классифицировано, — внутренняя ошибка плеера:
    /// лучше честное «внутренняя ошибка», чем английский debug-дамп протокола.
    pub(crate) const fn user_outcome(&self) -> StrongMediaOpenUserOutcome {
        match self {
            Self::Start(MediaOpenStartError::Busy) => StrongMediaOpenUserOutcome::Busy,
            Self::Terminal(MediaOpenTerminalOutcome::Cancelled { .. }) => {
                StrongMediaOpenUserOutcome::Cancelled
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

/// Причина отмены, за которой стоит действие пользователя либо закрытие приложения.
///
/// Match без `_`: новая причина отмены обязана получить явное решение, чтобы
/// настоящая ошибка случайно не стала «тихой».
const fn cancellation_cause_is_user_or_shutdown(cause: MediaInstallCancellationCause) -> bool {
    match cause {
        // Пользователь нажал Stop / отменил подготовку; приложение закрывается.
        MediaInstallCancellationCause::UserCancelled
        | MediaInstallCancellationCause::TransportStop
        | MediaInstallCancellationCause::LifecycleShutdown => true,
        // Замена запросом (supersede) обрабатывает startup-флаг; остальное — не про
        // действие пользователя, поведение прежнее.
        MediaInstallCancellationCause::Superseded
        | MediaInstallCancellationCause::StructuralInvalidation
        | MediaInstallCancellationCause::LifecycleSuspended => false,
    }
}

#[cfg(test)]
mod tests;
