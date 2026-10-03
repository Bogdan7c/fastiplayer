//! Сохранение настоящей причины сбоя потокового источника во время probe.
//!
//! Symphonia 0.6 ищет формат циклом `while let Ok(byte) = mss.read_byte()`:
//! любая ошибка чтения для неё выглядит как конец данных, и наружу выходит
//! вводящее в заблуждение `no suitable format reader found`. Для потокового
//! (`Read`-only) пути этот модуль запоминает первую ошибку чтения, пока идёт probe,
//! и отдаёт её владельцу открытия вместо «формат не найден».
//!
//! Это тот же контракт, что у `ByteSourceFailureObserver` для `ByteSource`-пути:
//! наблюдатель активен только на фазе probe и не меняет runtime-семантику ошибок.

use std::io::{self, Read};
use std::sync::{Arc, Mutex, PoisonError};

use crate::DemuxError;

/// Handle владельца открытия: забирает сохранённую ошибку после probe.
#[derive(Clone, Default)]
pub(crate) struct StreamProbeFailureObserver {
    /// Состояние разделяется с обёрткой reader-а, которая живёт внутри Symphonia.
    state: Arc<Mutex<StreamProbeFailureState>>,
}

/// Состояние одной probe-фазы.
#[derive(Default)]
struct StreamProbeFailureState {
    /// Первая ошибка чтения во время probe — именно она и есть причина отказа.
    first_failure: Option<io::Error>,

    /// После завершения probe обёртка больше ничего не запоминает.
    probe_finished: bool,
}

impl StreamProbeFailureObserver {
    /// Запоминает первую ошибку probe-фазы и возвращает Symphonia её копию.
    ///
    /// `io::Error` нельзя клонировать: оригинал (с исходной цепочкой причин)
    /// остаётся у наблюдателя, а Symphonia получает ошибку того же вида и текста.
    /// После завершения probe ошибка проходит дальше без изменений.
    fn observe(&self, error: io::Error) -> io::Error {
        // `Interrupted` — штатный сигнал «повтори read», а не причина отказа источника.
        if error.kind() == io::ErrorKind::Interrupted {
            return error;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.probe_finished || state.first_failure.is_some() {
            return error;
        }

        let copy_for_symphonia = io::Error::new(error.kind(), error.to_string());
        state.first_failure = Some(error);
        copy_for_symphonia
    }

    /// Завершает неудачный probe: отдаёт настоящую причину, если она была.
    ///
    /// `None` означает, что источник читался без ошибок и формат действительно
    /// не распознан — тогда владелец оставляет исходную ошибку probe.
    pub(crate) fn take_demux_error(&self) -> Option<DemuxError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.probe_finished = true;
        state.first_failure.take().map(DemuxError::Io)
    }

    /// Завершает успешный probe и выключает запоминание для runtime-чтения.
    pub(crate) fn finish_probe_success(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.probe_finished = true;
        state.first_failure = None;
    }
}

/// Обёртка над потоковым reader-ом, сообщающая наблюдателю об ошибках чтения.
pub(crate) struct StreamProbeFailureReader<R> {
    /// Исходный потоковый reader (сеть, канал producer-а и т.п.).
    inner: R,

    /// Общий с владельцем открытия наблюдатель probe-фазы.
    observer: StreamProbeFailureObserver,
}

impl<R: Read> StreamProbeFailureReader<R> {
    /// Оборачивает reader и возвращает handle наблюдателя для владельца открытия.
    pub(crate) fn new_observed(inner: R) -> (Self, StreamProbeFailureObserver) {
        let observer = StreamProbeFailureObserver::default();
        let reader = Self {
            inner,
            observer: observer.clone(),
        };
        (reader, observer)
    }
}

impl<R: Read> Read for StreamProbeFailureReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.inner
            .read(output)
            .map_err(|error| self.observer.observe(error))
    }
}

#[cfg(test)]
#[path = "stream_probe_failure/tests.rs"]
mod tests;
