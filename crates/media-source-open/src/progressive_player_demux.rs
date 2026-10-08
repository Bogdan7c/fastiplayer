//! Сборка player-facing demuxer-а поверх progressive HTTP/FTP транспорта.
//!
//! Зачем модуль (сессия 16). Раньше seekable HTTP (сервер отвечает `206` на Range)
//! отдавал player-у «голый» Symphonia demuxer. Тогда разбор контейнера шёл прямо в
//! рабочем потоке player-а, и при обрыве сети этот поток засыпал внутри чтения:
//! состояние оставалось `Playing`, Buffering и спиннер не включались, пауза и
//! перемотка не отвечали до таймаута.
//!
//! Теперь любой progressive demuxer работает в отдельном потоке `ProgressiveDemuxer`:
//! - seekable вход — `new_receipted_seekable` с асинхронными seek-квитанциями, как у
//!   HLS/DASH; перемотка идёт через [`PreparedDemuxSeekPort`], который вызывающий
//!   код обязан передать player-у вместе с demuxer-ом;
//! - forward-only вход (`200` без Range) — прежний `ProgressiveDemuxer::new`.
//!
//! Player всегда видит неблокирующий `next_event`: пока сеть восстанавливается, он
//! получает `TemporarilyUnavailable` и сам решает войти в Buffering.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use demux_api::{
    ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
    ProgressiveRuntimeGeneration, ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::{DemuxRetryHint, Demuxer};
use player_core::{
    PreparedDemuxSeekEnqueueError, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};
use source_core::CancellationToken;
use web_media_transport_api::TransportSeekability;

/// Первое (и единственное) поколение runtime-а стабильного progressive ресурса.
///
/// Явный reopen создаёт новый demuxer со своим shared state, поэтому поколение
/// внутри одного runtime-а никогда не растёт.
const PROGRESSIVE_RUNTIME_GENERATION: u64 = 1;

/// Сколько seek-квитанций может ждать одновременно: как у HLS (быстрые повторные
/// перемотки не переполняют очередь, а память остаётся ограниченной).
const MAXIMUM_OUTSTANDING_SEEK_RECEIPTS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(15);

/// Сколько пакетов фоновый demuxer держит готовыми для player-а: как у HLS
/// (~несколько секунд A/V), байты дополнительно ограничены окном prefetch-а.
const PROGRESSIVE_EVENT_QUEUE_CAPACITY: NonZeroUsize = NonZeroUsize::MIN.saturating_add(255);

/// Через сколько player переспрашивает пустую очередь seekable demuxer-а.
///
/// 10 мс — как у HLS: незаметно для глаза и не крутит поток player-а вхолостую,
/// пока сеть восстанавливается.
const SEEKABLE_RETRY_HINT: Duration = Duration::from_millis(10);

/// Demuxer для player-а и, для seekable входа, обязательный seek port.
pub struct ProgressivePlayerDemuxer {
    /// Неблокирующий demuxer, который получает player.
    pub demuxer: Box<dyn Demuxer + Send>,
    /// Seek port для seekable входа; `None` у forward-only потока (перемотки нет).
    pub seek_port: Option<Arc<dyn PreparedDemuxSeekPort>>,
}

/// Переносит открытый progressive demuxer в фоновый поток.
///
/// `prefetch_window_bytes`/`prefetch_chunk_bytes` — уже проверенные бюджеты из
/// `[network]`: очередь событий не заводит второй политики памяти.
pub fn into_player_demuxer(
    demuxer: Box<dyn Demuxer + Send>,
    transport_seekability: TransportSeekability,
    cancellation: CancellationToken,
    prefetch_config: media_prefetch::PrefetchConfig,
) -> Result<ProgressivePlayerDemuxer> {
    let limits = progressive_limits(prefetch_config)?;
    match transport_seekability {
        TransportSeekability::Seekable => {
            let progressive = ProgressiveDemuxer::new_receipted_seekable(
                demuxer,
                cancellation,
                limits,
                DemuxRetryHint::new(SEEKABLE_RETRY_HINT)
                    .context("Seekable progressive retry hint нарушает media-core bounds")?,
                ProgressiveRuntimeGeneration::new(PROGRESSIVE_RUNTIME_GENERATION),
                ProgressiveAsyncSeekLimits::new(MAXIMUM_OUTSTANDING_SEEK_RECEIPTS),
            )
            .context("Не удалось запустить seekable progressive demux worker")?;
            let seek_handle = progressive
                .async_seek_handle()
                .ok_or_else(|| anyhow!("seekable progressive runtime не выдал seek handle"))?;
            Ok(ProgressivePlayerDemuxer {
                demuxer: Box::new(progressive),
                seek_port: Some(Arc::new(ProgressivePreparedDemuxSeekPort {
                    handle: seek_handle,
                })),
            })
        }
        TransportSeekability::Streaming => {
            let retry_hint = DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER)
                .context("Minimum demux retry hint нарушает media-core bounds")?;
            let progressive = ProgressiveDemuxer::new(demuxer, cancellation, limits, retry_hint)
                .context("Не удалось запустить progressive demux worker")?;
            Ok(ProgressivePlayerDemuxer {
                demuxer: Box::new(progressive),
                seek_port: None,
            })
        }
    }
}

/// Ограничивает очередь событий фонового demuxer-а.
///
/// Число событий — фиксированная константа, а не «окно / кусок prefetch-а»:
/// событие — это один пакет (килобайты), а не мегабайтный кусок. Прежняя формула
/// при маленьком окне давала очередь на 1 пакет, и почти каждый проход player-а
/// заканчивался `TemporarilyUnavailable`. Незакрытый demux-retry по правилам
/// player-core запрещает выход из Buffering, поэтому player навсегда застревал в
/// preroll (найдено ручным прогоном сессии 16 с `read_ahead_mb = 1`).
/// Память по-прежнему ограничивает байтовый лимит = окно из `[network]`.
fn progressive_limits(
    prefetch_config: media_prefetch::PrefetchConfig,
) -> Result<ProgressiveDemuxBufferLimits> {
    let encoded_byte_capacity = usize::try_from(prefetch_config.window_bytes())
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| anyhow!("prefetch window нельзя преобразовать в byte capacity"))?;
    Ok(ProgressiveDemuxBufferLimits::new(
        PROGRESSIVE_EVENT_QUEUE_CAPACITY,
        encoded_byte_capacity,
    ))
}

/// Адаптер seek-квитанций progressive runtime-а к нейтральной границе player-а.
struct ProgressivePreparedDemuxSeekPort {
    /// Cloneable handle того же runtime-а, что и demuxer player-а.
    handle: ProgressiveAsyncSeekHandle,
}

impl PreparedDemuxSeekPort for ProgressivePreparedDemuxSeekPort {
    /// Ставит seek в очередь worker-а с точной identity запроса player-а.
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
            .map_err(map_enqueue_error)
    }

    /// Переводит квитанцию worker-а в словарь player-а.
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

/// Сохраняет различие причин отказа постановки seek-а в очередь.
const fn map_enqueue_error(
    error: ProgressiveAsyncSeekEnqueueError,
) -> PreparedDemuxSeekEnqueueError {
    match error {
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
    }
}

#[cfg(test)]
mod tests;
