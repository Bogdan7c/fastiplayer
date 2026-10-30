//! Конец полученного после `seal` — это конец файла, а не повреждение.
//!
//! Когда вкладка ушла (`seal`), оставшиеся байты не придут никогда. Источник
//! отдаёт читателю `Ok(0)` на границе полученного, и демуксер видит файл, обрезанный
//! посреди коробки: Symphonia называет это ошибкой разбора («unexpected end of
//! atom», «truncated declared element data»). Плеер на фатальной ошибке демуксера
//! сразу уходит в `Failed` и не доигрывает то, что уже получено — вопреки решению
//! владельца «доиграть полученное».
//!
//! Честно ли это? Да: после `seal` граница полученного и есть конец файла (так же,
//! как `end_of_stream` в MSE даёт чистый `Ended`). Поэтому тонкая обёртка над
//! демуксером превращает ошибку в обычный конец потока — но **только если ядро
//! записало факт «чтение оборвано seal»**. Любая другая ошибка (битый файл без
//! `seal`, повреждение посреди `mdat`) проходит без изменений.
//!
//! Граница honest-ности: если `seal` уже случился и в полученных байтах есть
//! настоящее повреждение, оно тоже станет концом потока — отличить его от обрыва
//! по тексту ошибки нельзя, а играть до повреждения безопаснее, чем падать.

use std::sync::Arc;
use std::time::Duration;

use media_core::{
    DemuxActiveReadInterruptionCapability, DemuxReadEvent, DemuxSeekCancellationToken,
    DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaAttachments, MediaMetadata,
    TrackInfo,
};

use super::core::FileCore;

/// Демуксер файла, для которого обрыв из-за `seal` — конец потока.
pub(super) struct SealAwareDemuxer {
    /// Настоящий демуксер.
    inner: Box<dyn Demuxer + Send>,
    /// Ядро файла: хранит факт «чтение оборвано seal».
    core: Arc<FileCore>,
}

impl SealAwareDemuxer {
    /// Обёртка над открытым демуксером.
    pub(super) fn new(inner: Box<dyn Demuxer + Send>, core: Arc<FileCore>) -> Self {
        Self { inner, core }
    }
}

impl Demuxer for SealAwareDemuxer {
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

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        match self.inner.next_event() {
            Err(_) if self.core.read_cut_by_seal() => {
                // Текст ошибки не печатаем: он может содержать данные страницы.
                tracing::debug!(
                    "demuxer error after the file read was cut by seal is treated as the end of the stream"
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
mod tests {
    use std::time::Duration;

    use media_core::{DemuxReadEvent, DemuxSeekResult, Demuxer, TrackInfo};
    use source_core::CancellationToken;

    use super::SealAwareDemuxer;
    use crate::range_file::{PushedFilePolicy, RangeRequestIds, RangeRequestNotifier, pushed_file};
    use std::sync::Arc;

    /// Демуксер, который на каждом чтении отвечает ошибкой (как на обрезанном файле).
    struct AlwaysFailing;

    impl Demuxer for AlwaysFailing {
        fn tracks(&self) -> &[TrackInfo] {
            &[]
        }
        fn duration(&self) -> Option<Duration> {
            None
        }
        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            Err(anyhow::anyhow!("unexpected end of atom"))
        }
        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Err(anyhow::anyhow!("seek failed"))
        }
    }

    struct Silent;
    impl RangeRequestNotifier for Silent {
        fn range_requested(&self) {}
    }

    /// Ядро файла размера 1000 и писатель; читателю ещё ничего не пришло.
    fn setup() -> (Arc<super::FileCore>, crate::range_file::PushedFileWriter) {
        let (writer, _opener, requests) = pushed_file(
            PushedFilePolicy::recommended().expect("policy"),
            RangeRequestIds::new(),
            Arc::new(Silent),
        );
        (requests.core_for_tests(), writer)
    }

    #[test]
    fn errors_pass_through_unless_a_read_was_cut_by_seal() {
        let (core, mut writer) = setup();
        let mut demuxer = SealAwareDemuxer::new(Box::new(AlwaysFailing), Arc::clone(&core));
        // Без seal ошибка демуксера — настоящая.
        assert!(demuxer.next_event().is_err());
        // Сам по себе seal ещё ничего не значит: чтение им не оборвано.
        writer.seal();
        assert!(
            demuxer.next_event().is_err(),
            "seal alone does not mask errors"
        );
        // Чтение за пределами полученного после seal оборвано — теперь это конец потока.
        let mut buffer = [0_u8; 8];
        let read = core.read_at(500, &mut buffer, &CancellationToken::new());
        assert!(matches!(read, Ok(0)), "{read:?}");
        assert!(matches!(
            demuxer.next_event(),
            Ok(DemuxReadEvent::EndOfStream)
        ));
        // Перемотка проходит как была: ошибка перемотки не маскируется.
        assert!(demuxer.seek(Duration::ZERO).is_err());
    }
}
