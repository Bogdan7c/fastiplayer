//! Общее состояние потока, которое видят обе стороны: длительность, первая ошибка
//! и обрыв скачивания (отсечка, модуль [`crate::download_cut`]).

use std::sync::{Mutex, MutexGuard};

use media_core::DemuxRetryHint;

use crate::download_cut::{DownloadCut, PlayableEnd};
use crate::error::PushedStreamError;
use crate::time::PushedDuration;

/// Состояние потока, разделяемое приёмом и плеером.
#[derive(Debug)]
pub(crate) struct StreamState {
    /// Длительность, которую сообщил источник байтов.
    duration: Mutex<PushedDuration>,
    /// Первая ошибка потока: после неё поток мёртв для обеих сторон.
    failure: Mutex<Option<PushedStreamError>>,
    /// Оборван ли вход уходом страницы и где отсечка.
    download_cut: Mutex<DownloadCut>,
    /// Подсказка повтора для плеера.
    retry_hint: DemuxRetryHint,
    /// На связи ли тот, кто приносит байты (писатель потока жив).
    producer: Mutex<ProducerLink>,
}

/// На связи ли поставщик байтов потока (страница через писателя).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProducerLink {
    /// Писатель жив: новые байты ещё могут прийти.
    Connected,
    /// Писатель уничтожен (вкладка закрылась, канал оборвался): новых байтов не будет.
    Gone,
}

impl StreamState {
    /// Живой поток без длительности.
    pub(crate) fn new(retry_hint: DemuxRetryHint) -> Self {
        Self {
            duration: Mutex::new(PushedDuration::Unknown),
            failure: Mutex::new(None),
            download_cut: Mutex::new(DownloadCut::NotCut),
            retry_hint,
            producer: Mutex::new(ProducerLink::Connected),
        }
    }

    /// На связи ли поставщик байтов.
    pub(crate) fn producer_link(&self) -> ProducerLink {
        *lock(&self.producer)
    }

    /// Писатель ушёл: новых байтов не будет никогда (обратного перехода нет).
    pub(crate) fn note_producer_gone(&self) {
        *lock(&self.producer) = ProducerLink::Gone;
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

    /// Оборван ли вход и где отсечка (полосы спрашивают на каждом пакете).
    pub(crate) fn download_cut(&self) -> DownloadCut {
        *lock(&self.download_cut)
    }

    /// Вход оборван уходом страницы: дальше `playable_end` пакеты не выдаются.
    /// Повторный вызов (перемотка во время досмотра) переставляет отсечку.
    pub(crate) fn cut_download(&self, playable_end: PlayableEnd) {
        *lock(&self.download_cut) = DownloadCut::CutShort(playable_end);
    }

    /// Байты пришли снова (вход открыт заново): обрыва больше нет.
    pub(crate) fn resume_download(&self) {
        *lock(&self.download_cut) = DownloadCut::NotCut;
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
