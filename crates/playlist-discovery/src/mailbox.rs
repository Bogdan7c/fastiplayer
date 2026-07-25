//! Job-owned bounded mailbox, coalesced wake/progress и lossless terminal slot.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::{
    AdmissionDirection, AdmittedBatch, DiscoveryEvent, DiscoveryFinalSummary, DiscoveryProgress,
    VERIFIED_RECORD_BUFFER_LIMIT,
};

/// Максимум marker/batch events; progress и terminal имеют отдельные slots.
pub const DISCOVERY_EVENT_LIMIT: usize = 1_024;

/// Ошибка app-provided wake boundary без зависимости от winit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakeDisconnected;

/// Лёгкий app-owned сигнал: payload остаётся в job mailbox.
pub trait DiscoveryWakePort: Send + Sync + 'static {
    /// Просит owner неблокирующе drain-ить discovery state.
    fn wake(&self) -> Result<(), WakeDisconnected>;
}

#[derive(Default)]
struct MailboxState {
    events: VecDeque<DiscoveryEvent>,
    buffered_record_count: usize,
    latest_progress: Option<DiscoveryProgress>,
    terminal: Option<DiscoveryFinalSummary>,
}

/// Один заранее созданный mailbox принадлежит job-у до terminal drain.
pub(crate) struct JobMailbox {
    state: Mutex<MailboxState>,
    wake_coordinator: Arc<WakeCoordinator>,
}

/// Один wake edge принадлежит всему process-lifetime discovery owner-у.
pub(crate) struct WakeCoordinator {
    wake_port: Arc<dyn DiscoveryWakePort>,
    wake_pending: AtomicBool,
    wake_disconnected: AtomicBool,
    mailboxes: Mutex<Vec<Weak<JobMailbox>>>,
}

impl JobMailbox {
    pub(crate) fn new(wake_coordinator: Arc<WakeCoordinator>) -> Arc<Self> {
        let mailbox = Arc::new(Self {
            state: Mutex::new(MailboxState::default()),
            wake_coordinator: wake_coordinator.clone(),
        });
        wake_coordinator.register(&mailbox);
        mailbox
    }

    fn has_payload(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !state.events.is_empty() || state.latest_progress.is_some() || state.terminal.is_some()
    }

    fn notify(&self) {
        self.wake_coordinator.notify();
    }

    fn finish_drain(&self) {
        self.wake_coordinator.finish_drain();
    }
}

impl WakeCoordinator {
    pub(crate) fn new(wake_port: Arc<dyn DiscoveryWakePort>) -> Arc<Self> {
        Arc::new(Self {
            wake_port,
            wake_pending: AtomicBool::new(false),
            wake_disconnected: AtomicBool::new(false),
            mailboxes: Mutex::new(Vec::new()),
        })
    }

    fn register(&self, mailbox: &Arc<JobMailbox>) {
        self.mailboxes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::downgrade(mailbox));
    }
}

impl JobMailbox {
    pub(crate) fn publish_progress(&self, progress: DiscoveryProgress) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .latest_progress = Some(progress);
        self.notify();
    }

    pub(crate) fn publish_batch(&self, batch: AdmittedBatch) -> bool {
        let record_count = batch.records().len();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.events.len() == DISCOVERY_EVENT_LIMIT
            || state.buffered_record_count + record_count > VERIFIED_RECORD_BUFFER_LIMIT
        {
            return false;
        }
        state.buffered_record_count += record_count;
        state.events.push_back(DiscoveryEvent::AdmittedBatch(batch));
        drop(state);
        self.notify();
        true
    }

    pub(crate) fn publish_marker(&self, event: DiscoveryEvent) {
        let marker_key = event_marker_key(&event);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(marker_key) = marker_key
            && let Some(existing) = state
                .events
                .iter_mut()
                .rev()
                .find(|queued| event_marker_key(queued) == Some(marker_key))
        {
            *existing = event;
        } else if state.events.len() < DISCOVERY_EVENT_LIMIT {
            state.events.push_back(event);
        }
        drop(state);
        self.notify();
    }

    pub(crate) fn publish_terminal(&self, summary: DiscoveryFinalSummary) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.terminal.is_some() {
            return false;
        }
        state.terminal = Some(summary);
        drop(state);
        self.notify();
        true
    }

    pub(crate) fn remaining_record_capacity(&self) -> usize {
        VERIFIED_RECORD_BUFFER_LIMIT.saturating_sub(
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .buffered_record_count,
        )
    }

    pub(crate) fn take_events(&self) -> Vec<DiscoveryEvent> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let events: Vec<_> = state.events.drain(..).collect();
        state.buffered_record_count = 0;
        drop(state);
        self.finish_drain();
        events
    }

    pub(crate) fn take_progress(&self) -> Option<DiscoveryProgress> {
        let progress = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .latest_progress
            .take();
        self.finish_drain();
        progress
    }

    pub(crate) fn take_terminal(&self) -> Option<DiscoveryFinalSummary> {
        let terminal = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .terminal
            .take();
        self.finish_drain();
        terminal
    }

    pub(crate) fn discard_all_events(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.events.clear();
        state.buffered_record_count = 0;
    }

    pub(crate) fn wake_disconnected(&self) -> bool {
        self.wake_coordinator.wake_disconnected()
    }
}

impl WakeCoordinator {
    pub(crate) fn wake_disconnected(&self) -> bool {
        self.wake_disconnected.load(Ordering::Acquire)
    }

    fn notify(&self) {
        if self
            .wake_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && self.wake_port.wake().is_err()
        {
            self.wake_disconnected.store(true, Ordering::Release);
        }
    }

    fn finish_drain(&self) {
        self.wake_pending.store(false, Ordering::Release);
        let has_pending = {
            let mut mailboxes = self
                .mailboxes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            mailboxes.retain(|mailbox| mailbox.strong_count() != 0);
            mailboxes
                .iter()
                .filter_map(Weak::upgrade)
                .any(|mailbox| mailbox.has_payload())
        };
        if has_pending {
            self.notify();
        }
    }
}

fn event_marker_key(event: &DiscoveryEvent) -> Option<(u8, AdmissionDirection)> {
    match event {
        DiscoveryEvent::AdmittedBatch(_) => None,
        DiscoveryEvent::AdmissionAdvanced(marker) => Some((0, marker.direction())),
        DiscoveryEvent::FrontierReady(ready) => Some((1, ready.direction())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::{DiscoveryJobId, DiscoveryJobKind};

    #[derive(Default)]
    struct CountingWake {
        count: AtomicUsize,
    }

    impl DiscoveryWakePort for CountingWake {
        fn wake(&self) -> Result<(), WakeDisconnected> {
            self.count.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[test]
    fn publish_while_drain_clears_edge_keeps_latest_progress_and_bounded_wakes() {
        for _ in 0..256 {
            let wake = Arc::new(CountingWake::default());
            let coordinator = WakeCoordinator::new(wake.clone());
            let mailbox = JobMailbox::new(coordinator);
            let job_id = DiscoveryJobId::from_counter(1).unwrap();
            mailbox.publish_progress(progress(job_id, 0));
            let barrier = Arc::new(Barrier::new(2));
            let producer_mailbox = mailbox.clone();
            let producer_barrier = barrier.clone();
            let producer = std::thread::spawn(move || {
                producer_barrier.wait();
                producer_mailbox.publish_progress(progress(job_id, 1));
            });

            barrier.wait();
            let first = mailbox.take_progress();
            producer.join().unwrap();
            let second = mailbox.take_progress();
            // Обе race-развязки сводятся к одному deterministic aggregation path,
            // чтобы short-circuit не делал coverage самой проверки scheduler-dependent.
            let newest_processed = first
                .into_iter()
                .chain(second)
                .map(|progress| progress.processed)
                .max();
            assert_eq!(newest_processed, Some(1));
            assert!(wake.count.load(Ordering::Acquire) <= 2);
        }
    }

    #[test]
    fn progress_flood_without_drain_has_one_outstanding_wake() {
        let wake = Arc::new(CountingWake::default());
        let coordinator = WakeCoordinator::new(wake.clone());
        let mailbox = JobMailbox::new(coordinator);
        let job_id = DiscoveryJobId::from_counter(1).unwrap();
        for processed in 0..1_000 {
            mailbox.publish_progress(progress(job_id, processed));
        }
        assert_eq!(wake.count.load(Ordering::Acquire), 1);
        assert_eq!(mailbox.take_progress().unwrap().processed, 999);
    }

    fn progress(job_id: DiscoveryJobId, processed: usize) -> DiscoveryProgress {
        DiscoveryProgress {
            job_id,
            kind: DiscoveryJobKind::VisibleRefresh,
            processed,
            total: 1_000,
        }
    }
}
