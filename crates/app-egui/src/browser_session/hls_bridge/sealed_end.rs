//! Вкладка ушла посреди HLS — доигрываем скачанное (решение 42), а не падаем.
//!
//! После ухода страницы переводчик запечатывает очередь ресурсов поколения (`seal`):
//! новые запросы и недочитанные тела получают «источник закрыт». HLS-движок честно
//! превращает это в ошибку демуксера, а плеер на ошибке демуксера уходит в `Failed`
//! — процесс кончил бы «отказом», хотя всё скачанное могло доиграться. Это тот же
//! случай, что у файла `<video src>` (`pushed-media-source::range_file::sealed_end`).
//!
//! Обёртка делает ровно то же, что обёртка файла, но признак берёт у очереди
//! ресурсов ([`ResourceFetchRequests::seal_cut`]):
//! - чтение оборвано печатью → ошибка демуксера становится штатным концом потока
//!   (`EndOfStream`), а `data_end()` — «данные обрезаны» (`CutShort{playable_end: None}`):
//!   позиция на конце остаётся там, где встал показ, и не прыгает в длительность;
//! - печати не было или она ничего не оборвала → всё проходит как есть: настоящая
//!   порча и отказ ресурса посреди просмотра остаются ошибкой (`playback_failed`).
//!
//! Остальное — перемотка, ожидание данных, дорожки, длительность, перематываемость —
//! чистый проброс. Граница честности та же, что у файла: после печати настоящая порча
//! в скачанном тоже станет концом (отличить её от обрыва по ошибке нельзя).

use std::time::Duration;

use media_core::{
    DemuxActiveReadInterruptionCapability, DemuxDataEnd, DemuxDataWait, DemuxDataWaitProbe,
    DemuxReadEvent, DemuxSeekCancellationToken, DemuxSeekRequest, DemuxSeekResult,
    DemuxSeekability, Demuxer, MediaAttachments, MediaMetadata, TrackInfo,
};
use pushed_media_source::{ResourceFetchRequests, ResourceSealCut};

/// Демуксер HLS-поколения, для которого обрыв печатью — конец потока.
pub(crate) struct SealAwareHlsDemuxer {
    /// Настоящий демуксер native HLS.
    inner: Box<dyn Demuxer + Send>,
    /// Очередь ресурсов поколения: знает, оборвала ли печать чтение.
    requests: ResourceFetchRequests,
}

impl SealAwareHlsDemuxer {
    /// Обёртка над демуксером поколения и его очередью ресурсов.
    pub(crate) fn new(inner: Box<dyn Demuxer + Send>, requests: ResourceFetchRequests) -> Self {
        Self { inner, requests }
    }

    /// Оборвала ли печать чтение движка.
    fn cut_by_seal(&self) -> bool {
        self.requests.seal_cut() == ResourceSealCut::ReadCutBySeal
    }
}

impl Demuxer for SealAwareHlsDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.inner.media_metadata()
    }

    fn take_attachments(&mut self) -> MediaAttachments {
        self.inner.take_attachments()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.inner.seekability()
    }

    fn active_read_interruption(&self) -> DemuxActiveReadInterruptionCapability {
        self.inner.active_read_interruption()
    }

    /// Чтение оборвано печатью — данные обрезаны; иначе ответ настоящего демуксера.
    fn data_end(&self) -> DemuxDataEnd {
        if self.cut_by_seal() {
            // Где во времени кончились скачанные сегменты, очередь не знает: позицию
            // поставят часы показа плеера.
            DemuxDataEnd::CutShort { playable_end: None }
        } else {
            self.inner.data_end()
        }
    }

    fn data_wait(&self) -> DemuxDataWait {
        self.inner.data_wait()
    }

    fn data_wait_probe(&self) -> Option<DemuxDataWaitProbe> {
        self.inner.data_wait_probe()
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        match self.inner.next_event() {
            Err(_) if self.cut_by_seal() => {
                // Текст ошибки не печатаем: в нём бывают данные страницы.
                tracing::debug!(
                    "HLS demuxer error after the resource queue was cut by seal is the end of the stream"
                );
                Ok(DemuxReadEvent::EndOfStream)
            }
            other => other,
        }
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek(timestamp)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek_with_request(request)
    }

    fn seek_with_cancellable_preview_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.inner
            .seek_with_cancellable_preview_request(request, cancellation)
    }

    fn seek_with_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek_with_receipted_request(request)
    }

    fn seek_with_cancellable_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.inner
            .seek_with_cancellable_receipted_request(request, cancellation)
    }
}

#[cfg(test)]
#[path = "sealed_end_tests.rs"]
mod tests;
