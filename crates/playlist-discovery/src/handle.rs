//! App-facing job control handle без раскрытия scheduler/job storage.

use std::sync::Arc;

use crate::job::JobInner;
use crate::{
    AdmissionAckOutcome, AdmissionBatchId, DiscoveryCancellation, DiscoveryCancellationCause,
    DiscoveryEvent, DiscoveryFinalSummary, DiscoveryJobId, DiscoveryProgress, ReprioritizeHint,
    ReprioritizeOutcome,
};

/// Cloneable app handle; worker internals и terminal slot остаются hidden.
#[derive(Clone)]
pub struct DiscoveryJobHandle {
    pub(crate) inner: Arc<JobInner>,
}

impl DiscoveryJobHandle {
    /// Возвращает exact job correlation ID.
    #[must_use]
    pub fn id(&self) -> DiscoveryJobId {
        self.inner.id()
    }

    /// Возвращает typed cancellation/freeze control.
    #[must_use]
    pub fn cancellation(&self) -> DiscoveryCancellation {
        self.inner.cancellation()
    }

    /// Линеаризует terminal cancel cause.
    pub fn cancel(&self, cause: DiscoveryCancellationCause) -> bool {
        self.inner.cancel(cause)
    }

    /// D62 settings-stage freeze без rescan/cancel.
    pub fn freeze_admission(&self) -> bool {
        self.inner.freeze()
    }

    /// D62 rollback продолжает exact job.
    pub fn resume_admission(&self) -> bool {
        self.inner.resume()
    }

    /// Меняет только pending work order/scheduling class.
    pub fn reprioritize(&self, hint: ReprioritizeHint) -> ReprioritizeOutcome {
        self.inner.reprioritize(hint)
    }

    /// Забирает текущие events; record ownership переходит caller-у.
    pub fn drain_events(&self) -> Vec<DiscoveryEvent> {
        self.inner.take_events()
    }

    /// Забирает latest progress snapshot.
    pub fn take_progress(&self) -> Option<DiscoveryProgress> {
        self.inner.take_progress()
    }

    /// Забирает terminal summary exactly once.
    pub fn take_final_summary(&self) -> Option<DiscoveryFinalSummary> {
        self.inner.take_terminal()
    }

    /// Подтверждает successful atomic app commit одного release.
    pub fn acknowledge_admitted_batch(&self, batch_id: AdmissionBatchId) -> AdmissionAckOutcome {
        self.inner.acknowledge_batch(batch_id)
    }

    /// Wake disconnect не уничтожает доступный через handle terminal slot.
    #[must_use]
    pub fn is_wake_disconnected(&self) -> bool {
        self.inner.wake_disconnected()
    }
}
