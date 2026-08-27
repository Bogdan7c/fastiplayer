//! Provider-neutral boundary для demux seek, который завершается worker receipt-ом.

use std::fmt;
use std::sync::Arc;

use media_core::{DemuxSeekRequest, DemuxSeekResult};

/// Монотонная identity одного player-owned demux seek intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreparedDemuxSeekRequestId(u64);

impl PreparedDemuxSeekRequestId {
    /// Создаёт identity из session-owned монотонного счётчика.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает raw value только adapter-у provider fence-а.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Terminal worker outcome без provider-specific vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedDemuxSeekOutcome {
    /// Demux worker вернул authoritative decode-safe anchor.
    Succeeded(DemuxSeekResult),
    /// Demux worker завершил operational seek ошибкой.
    Failed,
    /// Media lifecycle отменил request.
    Cancelled,
    /// Более новый seek заменил этот request.
    Superseded,
    /// Receipt относится к старому runtime generation.
    Stale,
}

/// At-most-once terminal receipt одного demux seek intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedDemuxSeekReceipt {
    /// Exact player-owned request identity.
    pub request_id: PreparedDemuxSeekRequestId,
    /// Terminal provider-neutral outcome.
    pub outcome: PreparedDemuxSeekOutcome,
}

/// Ошибка enqueue до передачи request ownership demux worker-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedDemuxSeekEnqueueError {
    /// Bounded receipt queue заполнена.
    ReceiptQueueFull,
    /// Request identity нарушила монотонность.
    NonMonotonicRequestIdentity,
    /// Worker уже остановлен.
    WorkerStopped,
    /// Adapter потерял promised receipt capability.
    CapabilityUnavailable,
}

/// Определяет, какая позиция authoritative после доказанного demux landing-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PreparedDemuxSeekLandingPolicy {
    /// Demux начинает с decode point не позже target-а, а player скрывает preroll до target-а.
    #[default]
    DecodeForwardToTarget,
    /// Demux доказал ближайший допустимый post-target landing, который и становится playback base.
    AuthoritativePostTarget,
}

impl fmt::Display for PreparedDemuxSeekEnqueueError {
    /// Публикует только категорию без provider/runtime internals.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReceiptQueueFull => "demux seek receipt queue is full",
            Self::NonMonotonicRequestIdentity => "demux seek request identity is not monotonic",
            Self::WorkerStopped => "demux seek worker has stopped",
            Self::CapabilityUnavailable => "demux seek receipt capability is unavailable",
        })
    }
}

impl std::error::Error for PreparedDemuxSeekEnqueueError {}

/// Nonblocking control port, который composition owner сохраняет до type erasure demuxer-а.
pub trait PreparedDemuxSeekPort: Send + Sync {
    /// Передаёт seek blocking demux worker-у без ожидания network/parser work.
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError>;

    /// Забирает следующий terminal receipt ровно один раз.
    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt>;
}

/// Explicit prepared-media seek execution mode.
pub enum PreparedDemuxSeekMode {
    /// Existing providers выполняют demux seek синхронно на player-owner turn-е.
    Synchronous,
    /// Blocking provider выполняет seek на worker-е и возвращает authoritative receipt.
    WorkerReceipted {
        /// Exact runtime port этого prepared media.
        port: Arc<dyn PreparedDemuxSeekPort>,
        /// Source-owned contract между requested target и authoritative landing position.
        landing_policy: PreparedDemuxSeekLandingPolicy,
    },
}

impl Default for PreparedDemuxSeekMode {
    /// Сохраняет прежнюю семантику всех existing providers.
    fn default() -> Self {
        Self::Synchronous
    }
}

impl fmt::Debug for PreparedDemuxSeekMode {
    /// Не раскрывает provider/runtime internals.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Synchronous => formatter.write_str("Synchronous"),
            Self::WorkerReceipted { landing_policy, .. } => formatter
                .debug_struct("WorkerReceipted")
                .field("landing_policy", landing_policy)
                .finish_non_exhaustive(),
        }
    }
}
