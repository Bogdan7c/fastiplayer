//! Bounded opt-in control plane для asynchronous progressive seek receipts.

use std::num::NonZeroUsize;
use std::sync::Arc;

use media_core::{
    DemuxActiveReadInterruptionCapability, DemuxActiveReadInterruptionReason,
    DemuxSeekCancellationToken, DemuxSeekRequest, DemuxSeekResult,
};

use super::worker::{ProgressiveSeekCommand, ProgressiveSharedState};

/// Stable identity одного opt-in asynchronous seek request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProgressiveSeekRequestId(u64);

impl ProgressiveSeekRequestId {
    /// Создаёт caller-owned monotonic identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает raw value только для fence comparison.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Generation конкретного progressive runtime instance-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProgressiveRuntimeGeneration(u64);

impl ProgressiveRuntimeGeneration {
    /// Создаёт explicit generation composition owner-а.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Exact request identity + runtime generation fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgressiveSeekFence {
    /// Runtime instance generation.
    pub runtime_generation: ProgressiveRuntimeGeneration,
    /// Monotonic request identity.
    pub request_id: ProgressiveSeekRequestId,
}

/// Explicit bound outstanding receipts, включая pending/in-flight requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressiveAsyncSeekLimits {
    /// Максимум requests без drained terminal receipt-а.
    maximum_outstanding_receipts: NonZeroUsize,
}

impl ProgressiveAsyncSeekLimits {
    /// Создаёт bounded receipt policy.
    #[must_use]
    pub const fn new(maximum_outstanding_receipts: NonZeroUsize) -> Self {
        Self {
            maximum_outstanding_receipts,
        }
    }

    /// Возвращает exact outstanding bound.
    #[must_use]
    pub const fn maximum_outstanding_receipts(self) -> usize {
        self.maximum_outstanding_receipts.get()
    }
}

/// Secret-safe terminal outcome worker-owned seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressiveAsyncSeekOutcome {
    /// Worker выполнил seek и вернул authoritative result.
    Succeeded(DemuxSeekResult),
    /// Inner transactional seek завершился typed operational error-ом.
    Failed,
    /// Shared lifecycle cancellation остановила request.
    Cancelled,
    /// Более новый request supersede-ил pending/in-flight request.
    Superseded,
    /// Fence принадлежит другому runtime generation.
    Stale,
}

/// At-most-once terminal receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressiveAsyncSeekReceipt {
    /// Stable identity, которую consumer сравнивает с current intent.
    pub fence: ProgressiveSeekFence,
    /// Ровно один terminal outcome.
    pub outcome: ProgressiveAsyncSeekOutcome,
}

/// Ошибка enqueue до передачи ownership worker-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProgressiveAsyncSeekEnqueueError {
    /// Runtime создан legacy constructor-ом без receipt capability.
    #[error("progressive runtime does not expose asynchronous seek receipts")]
    CapabilityAbsent,
    /// Caller не drain-ит receipts и исчерпал explicit bound.
    #[error("progressive asynchronous seek receipt queue is full")]
    ReceiptQueueFull,
    /// Request identity не больше предыдущей accepted identity.
    #[error("progressive asynchronous seek request identity is not monotonic")]
    NonMonotonicRequestIdentity,
    /// Worker уже terminal и не примет request.
    #[error("progressive asynchronous seek worker has stopped")]
    WorkerStopped,
}

/// Cloneable provider-neutral control handle, который переживает type erasure demuxer-а.
#[derive(Clone)]
pub struct ProgressiveAsyncSeekHandle {
    /// Shared bounded state остаётся единственной command/receipt authority.
    pub(super) shared: Arc<ProgressiveSharedState>,
    /// Exact generation помогает composition owner-у строить fence без догадок.
    pub(super) runtime_generation: ProgressiveRuntimeGeneration,
}

impl ProgressiveAsyncSeekHandle {
    /// Возвращает generation именно этого progressive runtime-а.
    #[must_use]
    pub const fn runtime_generation(&self) -> ProgressiveRuntimeGeneration {
        self.runtime_generation
    }

    /// Передаёт latest seek intent blocking worker-у без ожидания I/O/parser-а.
    pub fn enqueue(
        &self,
        fence: ProgressiveSeekFence,
        request: DemuxSeekRequest,
    ) -> Result<(), ProgressiveAsyncSeekEnqueueError> {
        let mut queue = self.shared.lock_queue();
        let Some(async_seek) = queue.async_seek.as_ref() else {
            return Err(ProgressiveAsyncSeekEnqueueError::CapabilityAbsent);
        };
        if queue.worker_stopped {
            return Err(ProgressiveAsyncSeekEnqueueError::WorkerStopped);
        }
        if async_seek
            .last_accepted_request_id
            .is_some_and(|previous| fence.request_id.value() <= previous)
        {
            return Err(ProgressiveAsyncSeekEnqueueError::NonMonotonicRequestIdentity);
        }
        if async_seek.outstanding_receipts >= async_seek.limits.maximum_outstanding_receipts() {
            return Err(ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull);
        }

        if let Some(async_seek) = queue.async_seek.as_mut() {
            async_seek.last_accepted_request_id = Some(fence.request_id.value());
            async_seek.outstanding_receipts = async_seek.outstanding_receipts.saturating_add(1);
        }
        if fence.runtime_generation != self.runtime_generation {
            if let Some(async_seek) = queue.async_seek.as_mut() {
                async_seek
                    .worker_pending_receipts
                    .push_back(ProgressiveAsyncSeekReceipt {
                        fence,
                        outcome: ProgressiveAsyncSeekOutcome::Stale,
                    });
            }
            self.shared.capacity_available.notify_all();
            return Ok(());
        }

        queue.current_generation = queue.current_generation.wrapping_add(1);
        queue.messages.clear();
        queue.queued_encoded_bytes = 0;
        if let Some(active_cancellation) = &queue.active_seek_cancellation {
            // `Completed` token делает этот cancel no-op; физически прерывается только
            // ещё не завершившийся request, а не уже committed replacement source.
            active_cancellation.cancel();
        }
        if let Some(superseded_command) = queue.pending_seek.take() {
            if let Some(superseded_cancellation) = superseded_command.cancellation() {
                superseded_cancellation.cancel();
            }
            if let Some(superseded_fence) = superseded_command.receipt_fence()
                && let Some(async_seek) = queue.async_seek.as_mut()
            {
                async_seek
                    .worker_pending_receipts
                    .push_back(ProgressiveAsyncSeekReceipt {
                        fence: superseded_fence,
                        outcome: ProgressiveAsyncSeekOutcome::Superseded,
                    });
            }
        }
        let request_cancellation = DemuxSeekCancellationToken::new();
        queue.pending_seek = Some(ProgressiveSeekCommand::Receipted {
            generation: queue.current_generation,
            request,
            fence,
            cancellation: request_cancellation,
        });
        if let DemuxActiveReadInterruptionCapability::Supported(port) =
            &queue.active_read_interruption
        {
            // Команда уже authoritative в queue, а mutex ещё не даёт worker-у начать rollback
            // reopen без pending replacement. Port contract запрещает I/O, wait и queue callback.
            let _interruption_request = port.request_active_read_interruption(
                DemuxActiveReadInterruptionReason::ReceiptedSeekEnqueued,
            );
        }
        self.shared.capacity_available.notify_all();
        Ok(())
    }

    /// Забирает следующий terminal receipt ровно один раз и освобождает bound.
    pub fn poll_receipt(&self) -> Option<ProgressiveAsyncSeekReceipt> {
        let mut queue = self.shared.lock_queue();
        let async_seek = queue.async_seek.as_mut()?;
        let receipt = async_seek.completed_receipts.pop_front()?;
        async_seek.outstanding_receipts = async_seek.outstanding_receipts.saturating_sub(1);
        Some(receipt)
    }
}

impl std::fmt::Debug for ProgressiveAsyncSeekHandle {
    /// Не раскрывает queue/parser internals.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgressiveAsyncSeekHandle")
            .field("runtime_generation", &self.runtime_generation)
            .finish_non_exhaustive()
    }
}
