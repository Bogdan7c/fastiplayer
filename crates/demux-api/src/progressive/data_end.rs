//! Проброс «каким был конец данных» (`Demuxer::data_end`) из фонового потока.
//!
//! Внутренний (блокирующий) demuxer живёт в фоновом потоке, и спросить его с потока
//! плеера нельзя. Поэтому фоновый поток, прочитав у inner-а `EndOfStream`, снимает его
//! ответ `data_end()` и кладёт в общее состояние ДО публикации события: плеер,
//! получивший `EndOfStream`, уже видит правильный ответ (публикация идёт под тем же
//! замком очереди). До первого конца ответ — «конец настоящий», как у demuxer-а без шва.
//!
//! Нужен источникам, чьи данные обрезаны раньше конца медиа (файл `<video src>`, у
//! которого ушла вкладка браузера): иначе плеер на `Ended` прыгал бы позицией в
//! длительность.

use media_core::DemuxDataEnd;

use super::worker::ProgressiveSharedState;

impl ProgressiveSharedState {
    /// Фоновый поток: ответ inner-а на его `EndOfStream`.
    pub(super) fn note_inner_data_end(&self, data_end: DemuxDataEnd) {
        self.lock_queue().inner_data_end = data_end;
    }

    /// Поток плеера: последний снятый ответ inner-а.
    pub(super) fn inner_data_end(&self) -> DemuxDataEnd {
        self.lock_queue().inner_data_end
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::thread;
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use media_core::{
        DemuxDataEnd, DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, Demuxer,
        MediaTime, Packet, TrackId, TrackInfo, TrackKind,
    };
    use source_core::CancellationToken;

    use crate::{
        ProgressiveAsyncSeekLimits, ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
        ProgressiveRuntimeGeneration,
    };

    /// Обрезанный файл: один пакет, потом конец; «обрезано» становится известно только
    /// на конце (как флаг `read_cut_by_seal` источника файла).
    struct TruncatedFileDemuxer {
        /// Перематывается ли (файл известного размера — да).
        seekability: DemuxSeekability,
        /// Пакет уже отдан.
        packet_emitted: bool,
        /// Конец уже прочитан: с этого момента данные «обрезаны».
        reached_end: bool,
    }

    impl TruncatedFileDemuxer {
        fn boxed(seekability: DemuxSeekability) -> Box<dyn Demuxer + Send> {
            Box::new(Self {
                seekability,
                packet_emitted: false,
                reached_end: false,
            })
        }
    }

    impl Demuxer for TruncatedFileDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }

        fn seekability(&self) -> DemuxSeekability {
            self.seekability
        }

        fn data_end(&self) -> DemuxDataEnd {
            if self.reached_end {
                DemuxDataEnd::CutShort { playable_end: None }
            } else {
                DemuxDataEnd::Complete
            }
        }

        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            if self.packet_emitted {
                self.reached_end = true;
                return Ok(DemuxReadEvent::EndOfStream);
            }
            self.packet_emitted = true;
            Ok(DemuxReadEvent::Packet(Packet::new_unbounded(
                TrackId::new(1),
                TrackKind::Video,
                Duration::ZERO,
                None,
                true,
                Bytes::from_static(&[0x65]),
            )))
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    fn limits() -> ProgressiveDemuxBufferLimits {
        ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(4).expect("positive"),
            NonZeroUsize::new(16).expect("positive"),
        )
    }

    fn retry_hint() -> DemuxRetryHint {
        DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER).expect("valid retry hint")
    }

    /// Читает события до конца потока (ожидание — только в потоке теста).
    fn read_to_end(progressive: &mut ProgressiveDemuxer) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match progressive.next_event().expect("readable") {
                DemuxReadEvent::EndOfStream => return,
                DemuxReadEvent::TemporarilyUnavailable(_) => {
                    assert!(
                        Instant::now() < deadline,
                        "the worker did not reach the end"
                    );
                    thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
                }
                _ => {}
            }
        }
    }

    /// Поток вперёд (без перемотки): до конца — «настоящий», после — ответ inner-а.
    #[test]
    fn forward_only_runtime_reports_the_data_end_of_the_inner_demuxer() {
        let mut progressive = ProgressiveDemuxer::new(
            TruncatedFileDemuxer::boxed(DemuxSeekability::NotSeekable {
                reason: media_core::TimelineNotSeekableReason::SourceNotSeekable,
            }),
            CancellationToken::new(),
            limits(),
            retry_hint(),
        )
        .expect("worker starts");
        assert_eq!(progressive.data_end(), DemuxDataEnd::Complete);

        read_to_end(&mut progressive);

        assert_eq!(
            progressive.data_end(),
            DemuxDataEnd::CutShort { playable_end: None }
        );
    }

    /// Перематываемый файл (путь `<video src>` известного размера) — то же.
    #[test]
    fn seekable_runtime_reports_the_data_end_of_the_inner_demuxer() {
        let mut progressive = ProgressiveDemuxer::new_receipted_seekable(
            TruncatedFileDemuxer::boxed(DemuxSeekability::Seekable),
            CancellationToken::new(),
            limits(),
            retry_hint(),
            ProgressiveRuntimeGeneration::new(1),
            ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(4).expect("positive")),
        )
        .expect("worker starts");
        assert_eq!(progressive.data_end(), DemuxDataEnd::Complete);

        read_to_end(&mut progressive);

        assert_eq!(
            progressive.data_end(),
            DemuxDataEnd::CutShort { playable_end: None }
        );
    }
}
