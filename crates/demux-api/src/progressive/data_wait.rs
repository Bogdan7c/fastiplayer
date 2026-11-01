//! Проброс «чего ждёт источник» (`Demuxer::data_wait`) из фонового потока.
//!
//! Внутренний (блокирующий) demuxer живёт в фоновом потоке и обычно стоит там внутри
//! чтения — ждёт байтов. Спросить его с потока плеера нельзя, а обновить общий ответ
//! он не может, пока ждёт. Поэтому до запуска потока у inner-а берётся
//! потокобезопасный вопрос ([`media_core::Demuxer::data_wait_probe`]), и поток плеера
//! задаёт его сам. Нет вопроса — ответ «не уточняю», как у demuxer-а без шва.
//!
//! Нужен файлу `<video src>` моста (решение 150): его байты по запросам несёт живая
//! страница, и перемотка, ждущая медленный ответ страницы, не должна сдаваться по
//! сроку плеера.
//!
//! Замки: вопрос копируется из очереди под её замком, а задаётся уже без него —
//! вопрос берёт свой замок источника, и порядок замков с фоновым потоком не
//! пересекается.

use media_core::{DemuxDataWait, DemuxDataWaitProbe};

use super::worker::ProgressiveSharedState;

impl ProgressiveSharedState {
    /// До запуска фонового потока: вопрос inner-а (или его отсутствие).
    pub(super) fn install_inner_data_wait_probe(&self, probe: Option<DemuxDataWaitProbe>) {
        self.lock_queue().inner_data_wait_probe = probe;
    }

    /// Поток плеера: ответ inner-а сейчас.
    pub(super) fn inner_data_wait(&self) -> DemuxDataWait {
        let probe = self.lock_queue().inner_data_wait_probe.clone();
        probe.map_or(DemuxDataWait::Unattributed, |probe| probe.data_wait())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use media_core::{
        DemuxDataWait, DemuxDataWaitProbe, DemuxReadEvent, DemuxRetryHint, Demuxer,
        LiveProducerProgress, TrackInfo,
    };
    use source_core::CancellationToken;

    use crate::{ProgressiveDemuxBufferLimits, ProgressiveDemuxer};

    /// Неперематываемый inner, который «ждёт страницу»: счётчик принесённого — общий.
    struct AwaitingPageDemuxer {
        delivered: Arc<AtomicU64>,
    }

    impl Demuxer for AwaitingPageDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn data_wait_probe(&self) -> Option<DemuxDataWaitProbe> {
            let delivered = Arc::clone(&self.delivered);
            Some(DemuxDataWaitProbe::new(move || {
                DemuxDataWait::LiveProducer {
                    progress: LiveProducerProgress::from_delivered_bytes(
                        delivered.load(Ordering::Relaxed),
                    ),
                }
            }))
        }

        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            // Как чтение, ждущее байтов страницы: фоновый поток стоит здесь.
            std::thread::sleep(Duration::from_millis(20));
            Ok(DemuxReadEvent::TemporarilyUnavailable(DemuxRetryHint::new(
                DemuxRetryHint::MIN_RETRY_AFTER,
            )?))
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
            anyhow::bail!("not seekable")
        }

        fn seekability(&self) -> media_core::DemuxSeekability {
            media_core::DemuxSeekability::NotSeekable {
                reason: media_core::TimelineNotSeekableReason::SourceNotSeekable,
            }
        }
    }

    /// Ответ inner-а, ждущего в фоновом потоке, доходит до плеера и меняется вживую.
    #[test]
    fn the_player_sees_the_live_answer_of_an_inner_waiting_in_the_worker() {
        let delivered = Arc::new(AtomicU64::new(10));
        let progressive = ProgressiveDemuxer::new(
            Box::new(AwaitingPageDemuxer {
                delivered: Arc::clone(&delivered),
            }),
            CancellationToken::new(),
            ProgressiveDemuxBufferLimits::new(
                std::num::NonZeroUsize::new(4).expect("non-zero"),
                std::num::NonZeroUsize::new(1 << 16).expect("non-zero"),
            ),
            DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER).expect("hint"),
        )
        .expect("worker starts");

        let live = |bytes| DemuxDataWait::LiveProducer {
            progress: LiveProducerProgress::from_delivered_bytes(bytes),
        };
        assert_eq!(progressive.data_wait(), live(10));
        delivered.store(42, Ordering::Relaxed);
        assert_eq!(progressive.data_wait(), live(42));
    }
}
