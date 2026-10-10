//! Кнопка «Повторить» на ошибке открытия (UX edge cases 2, B9).
//!
//! Модуль владеет **целью повтора** — ответом на вопрос «что именно открыть заново». Цель
//! задаёт тот, кто породил ошибку (кнопка Open, startup-открытие ссылки, очередь, player),
//! потому что только он знает, чем исходное действие выполнялось. Центр уведомлений
//! (`state::notifications`) цель лишь хранит рядом с ошибкой и не интерпретирует; UI знает
//! только «кнопку рисовать или нет» (`RetryOffer`). Осмысленность повтора для причин
//! отказа открытия решает `media_open::retry_advice`.
//!
//! Исполнение (`executor`) идёт тем же путём, что и исходное действие: не заводит
//! параллельного способа открытия, поэтому «занято» (сессия 18), подтверждение замены
//! очереди и обработка повторного отказа работают так же, как у первой попытки.

use std::path::PathBuf;
use std::time::Duration;

use playlist_core::PlaylistItemId;

use crate::external_open::request::DroppedWebUrl;
use crate::media_open::{MediaOpenUserFailureReason, RetryAdvice};

mod executor;
#[cfg(test)]
pub(crate) use executor::{LocalOpenSlot, MediaRetryPort, RetryStart, execute_media_retry};
mod failure_report;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

/// С какой позиции переоткрывать упавшее воспроизведение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReopenPosition {
    /// Позиция неизвестна или нулевая: открыть с начала.
    FromStart,
    /// Продолжить с позиции, на которой воспроизведение оборвалось.
    At(Duration),
}

/// Что именно повторить по кнопке «Повторить».
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaRetryTarget {
    /// Открыть этот локальный файл тем же путём, что кнопка Open (замена очереди одним файлом).
    LocalFile(PathBuf),
    /// Открыть эту ссылку тем же путём, что ссылка, брошенная на видео (замена очереди).
    /// Значение не раскрывается в `Debug`.
    WebUrl(DroppedWebUrl),
    /// Открыть заново этот элемент очереди (обычный Play строки).
    QueueItem(PlaylistItemId),
    /// Снять остановку очереди (B8) и продолжить: то же, что Play на остановленном элементе.
    ResumeBlockedQueue,
    /// Переоткрыть элемент, воспроизведение которого оборвалось (потеря сети посреди просмотра).
    ReopenFailedPlayback { position: ReopenPosition },
}

/// Решение владельца ошибки: предлагать ли «Повторить» и что при этом повторять.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetryPlan {
    /// Повтор не предлагается: кнопки не будет.
    NotOffered,
    /// Рядом с ошибкой рисуется «Повторить», нажатие повторяет `MediaRetryTarget`.
    Offered(MediaRetryTarget),
}

impl RetryPlan {
    /// План по причине отказа открытия: кнопка только там, где повтор осмыслен
    /// (`MediaOpenUserFailureReason::retry_advice`).
    pub(crate) fn for_reason(reason: MediaOpenUserFailureReason, target: MediaRetryTarget) -> Self {
        match reason.retry_advice() {
            RetryAdvice::Offer => Self::Offered(target),
            RetryAdvice::Withhold => Self::NotOffered,
        }
    }
}

impl ReopenPosition {
    /// Позиция обрыва из snapshot-а player-а: нулевая означает «с начала».
    pub(crate) fn from_playback_position(position: Duration) -> Self {
        if position.is_zero() {
            Self::FromStart
        } else {
            Self::At(position)
        }
    }
}
