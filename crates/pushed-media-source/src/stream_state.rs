//! Общее состояние потока, которое видят обе стороны: длительность и первая ошибка.

use std::sync::{Mutex, MutexGuard};

use media_core::DemuxRetryHint;

use crate::error::PushedStreamError;
use crate::time::PushedDuration;

/// Состояние потока, разделяемое приёмом и плеером.
#[derive(Debug)]
pub(crate) struct StreamState {
    /// Длительность, которую сообщил источник байтов.
    duration: Mutex<PushedDuration>,
    /// Первая ошибка потока: после неё поток мёртв для обеих сторон.
    failure: Mutex<Option<PushedStreamError>>,
    /// Подсказка повтора для плеера.
    retry_hint: DemuxRetryHint,
}

impl StreamState {
    /// Живой поток без длительности.
    pub(crate) fn new(retry_hint: DemuxRetryHint) -> Self {
        Self {
            duration: Mutex::new(PushedDuration::Unknown),
            failure: Mutex::new(None),
            retry_hint,
        }
    }

    /// Текущая длительность.
    pub(crate) fn duration(&self) -> PushedDuration {
        *lock(&self.duration)
    }

    /// Новая длительность от источника байтов.
    pub(crate) fn set_duration(&self, duration: PushedDuration) {
        *lock(&self.duration) = duration;
    }

    /// Первая ошибка, если поток уже мёртв.
    pub(crate) fn failure(&self) -> Option<PushedStreamError> {
        lock(&self.failure).clone()
    }

    /// Запоминает ошибку, если она первая, и возвращает действующую (первую).
    ///
    /// Первая ошибка — причина; всё, что сломалось после неё, — следствие.
    pub(crate) fn record_failure(&self, error: PushedStreamError) -> PushedStreamError {
        lock(&self.failure).get_or_insert(error).clone()
    }

    /// Подсказка повтора для плеера.
    pub(crate) const fn retry_hint(&self) -> DemuxRetryHint {
        self.retry_hint
    }
}

/// Захватывает значение; отравленный mutex не мешает читать состояние.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
