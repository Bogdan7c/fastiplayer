//! Обвязка приложения для открытого файла по запросам байтов: блокирующий демуксер
//! уходит в фоновый поток (`ProgressiveDemuxer`), перемотка — через seek port.
//!
//! Повторяет `media_source_open::progressive_player_demux::into_player_demuxer`
//! (приложение, пакет P4, зовёт именно его): dev-зависимость от
//! `media-source-open` тянула бы в тесты весь веб-стек.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use demux_api::{
    ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
    ProgressiveRuntimeGeneration, ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::DemuxRetryHint;
use player_core::{
    PreparedDemuxSeekEnqueueError, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId, PreparedMedia,
};
use pushed_media_source::{FileSeekability, OpenedPushedFile, PushedFilePolicy};

/// Очередь событий фонового демуксера (как у приложения).
const EVENT_QUEUE: NonZeroUsize = NonZeroUsize::MIN.saturating_add(255);
/// Наименьшая очередь пакетов в тестах: больше самого большого пакета фикстур (~2,7 КиБ).
const MIN_QUEUE_BYTES: usize = 8 * 1024;
/// Квитанций перемотки в ожидании (как у приложения).
const SEEK_RECEIPTS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(15);
/// Подсказка повтора пустой очереди.
const RETRY: Duration = Duration::from_millis(10);

/// Адаптер квитанций перемотки к границе плеера.
struct SeekPort {
    handle: ProgressiveAsyncSeekHandle,
}

impl PreparedDemuxSeekPort for SeekPort {
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: media_core::DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        self.handle
            .enqueue(
                ProgressiveSeekFence {
                    runtime_generation: self.handle.runtime_generation(),
                    request_id: ProgressiveSeekRequestId::new(request_id.value()),
                },
                request,
            )
            .map_err(|error| match error {
                ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull => {
                    PreparedDemuxSeekEnqueueError::ReceiptQueueFull
                }
                ProgressiveAsyncSeekEnqueueError::NonMonotonicRequestIdentity => {
                    PreparedDemuxSeekEnqueueError::NonMonotonicRequestIdentity
                }
                ProgressiveAsyncSeekEnqueueError::WorkerStopped => {
                    PreparedDemuxSeekEnqueueError::WorkerStopped
                }
                ProgressiveAsyncSeekEnqueueError::CapabilityAbsent => {
                    PreparedDemuxSeekEnqueueError::CapabilityUnavailable
                }
            })
    }

    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        self.handle
            .poll_receipt()
            .map(|receipt| PreparedDemuxSeekReceipt {
                request_id: PreparedDemuxSeekRequestId::new(receipt.fence.request_id.value()),
                outcome: match receipt.outcome {
                    ProgressiveAsyncSeekOutcome::Succeeded(result) => {
                        PreparedDemuxSeekOutcome::Succeeded(result)
                    }
                    ProgressiveAsyncSeekOutcome::Failed => PreparedDemuxSeekOutcome::Failed,
                    ProgressiveAsyncSeekOutcome::Cancelled => PreparedDemuxSeekOutcome::Cancelled,
                    ProgressiveAsyncSeekOutcome::Superseded => PreparedDemuxSeekOutcome::Superseded,
                    ProgressiveAsyncSeekOutcome::Stale => PreparedDemuxSeekOutcome::Stale,
                },
            })
    }
}

/// Готовит открытый файл к проигрыванию так, как это делает приложение.
pub fn prepare_file(opened: OpenedPushedFile, policy: &PushedFilePolicy) -> PreparedMedia {
    // Тесты берут крошечное окно prefetch (файлы по 100 КиБ), но очередь пакетов не
    // может быть меньше самого большого пакета: берём не меньше `MIN_QUEUE_BYTES`.
    let window = usize::try_from(policy.prefetch().window_bytes()).expect("window");
    let limits = ProgressiveDemuxBufferLimits::new(
        EVENT_QUEUE,
        NonZeroUsize::new(window.max(MIN_QUEUE_BYTES)).expect("non-zero window"),
    );
    let retry = DemuxRetryHint::new(RETRY).expect("retry hint");
    let cancellation = opened.lifecycle_cancellation.clone();
    match opened.seekability {
        FileSeekability::Seekable => {
            let progressive = ProgressiveDemuxer::new_receipted_seekable(
                opened.demuxer,
                cancellation,
                limits,
                retry,
                ProgressiveRuntimeGeneration::new(1),
                ProgressiveAsyncSeekLimits::new(SEEK_RECEIPTS),
            )
            .expect("seekable progressive demuxer");
            let port = Arc::new(SeekPort {
                handle: progressive.async_seek_handle().expect("seek handle"),
            });
            PreparedMedia::from_external_label("pushed file test", Box::new(progressive))
                .with_worker_receipted_demux_seek(port)
        }
        FileSeekability::ForwardOnly => {
            let progressive = ProgressiveDemuxer::new(opened.demuxer, cancellation, limits, retry)
                .expect("forward-only progressive demuxer");
            PreparedMedia::from_external_label("pushed file test", Box::new(progressive))
        }
    }
}
