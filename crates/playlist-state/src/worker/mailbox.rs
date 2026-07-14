use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    SaveAttemptReport, SaveWakePort, SaveWarning, SaveWorkerEvent, WorkerDisconnectReason,
};

/// Terminal mailbox не растёт при остановленном app drain.
const SAVE_EVENT_CAPACITY: usize = 8;
/// Последний slot резервируется для shutdown-related reporting.
const NORMAL_EVENT_CAPACITY: usize = SAVE_EVENT_CAPACITY - 1;

/// Bounded mailbox отделяет terminal payload от coalesced wake edge.
pub(super) struct WorkerMailbox {
    state: Mutex<MailboxState>,
    wake_pending: AtomicBool,
    wake_port_disabled: AtomicBool,
    wake_port: Arc<dyn SaveWakePort>,
}

struct MailboxState {
    attempt_reports: VecDeque<SaveAttemptReport>,
    latest_warning: Option<SaveWarning>,
    warning_update_pending: bool,
    disconnect: Option<WorkerDisconnectReason>,
    wake_disconnect_pending: bool,
}

impl WorkerMailbox {
    pub(super) fn new(wake_port: Arc<dyn SaveWakePort>) -> Self {
        Self {
            state: Mutex::new(MailboxState {
                attempt_reports: VecDeque::with_capacity(SAVE_EVENT_CAPACITY),
                latest_warning: None,
                warning_update_pending: false,
                disconnect: None,
                wake_disconnect_pending: false,
            }),
            wake_pending: AtomicBool::new(false),
            wake_port_disabled: AtomicBool::new(false),
            wake_port,
        }
    }

    pub(super) fn has_normal_event_capacity(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.attempt_reports.len() < NORMAL_EVENT_CAPACITY)
            .unwrap_or(false)
    }

    pub(super) fn publish_attempt(&self, report: SaveAttemptReport) {
        if let Ok(mut state) = self.state.lock()
            && state.attempt_reports.len() < SAVE_EVENT_CAPACITY
        {
            state.attempt_reports.push_back(report);
        }
        self.publish_wake_edge();
    }

    pub(super) fn publish_warning(&self, warning: Option<SaveWarning>) {
        if let Ok(mut state) = self.state.lock()
            && state.latest_warning != warning
        {
            state.latest_warning = warning;
            state.warning_update_pending = true;
        }
        self.publish_wake_edge();
    }

    pub(super) fn publish_disconnect(&self, reason: WorkerDisconnectReason) {
        if let Ok(mut state) = self.state.lock()
            && state.disconnect.is_none()
        {
            state.disconnect = Some(reason);
        }
        self.publish_wake_edge();
    }

    fn publish_wake_edge(&self) {
        if self.wake_port_disabled.load(Ordering::Acquire)
            || self.wake_pending.swap(true, Ordering::AcqRel)
        {
            return;
        }
        if self.wake_port.wake_save_worker().is_err() {
            self.wake_port_disabled.store(true, Ordering::Release);
            if let Ok(mut state) = self.state.lock() {
                state.wake_disconnect_pending = true;
            }
        }
    }

    pub(super) fn drain(&self) -> Vec<SaveWorkerEvent> {
        let mut events = Vec::new();
        if let Ok(mut state) = self.state.lock() {
            events.extend(
                state
                    .attempt_reports
                    .drain(..)
                    .map(SaveWorkerEvent::AttemptCompleted),
            );
            if state.warning_update_pending {
                events.push(SaveWorkerEvent::WarningChanged(state.latest_warning));
                state.warning_update_pending = false;
            }
            if let Some(reason) = state.disconnect.take() {
                events.push(SaveWorkerEvent::WorkerDisconnected(reason));
            }
            if state.wake_disconnect_pending {
                events.push(SaveWorkerEvent::WakePortDisconnected);
                state.wake_disconnect_pending = false;
            }
        }

        self.wake_pending.store(false, Ordering::Release);
        if self.has_pending_payload() {
            self.publish_wake_edge();
        }
        events
    }

    fn has_pending_payload(&self) -> bool {
        self.state
            .lock()
            .map(|state| {
                !state.attempt_reports.is_empty()
                    || state.warning_update_pending
                    || state.disconnect.is_some()
                    || state.wake_disconnect_pending
            })
            .unwrap_or(false)
    }
}

/// Drop-report ловит unwind, но clean disconnect публикуется явной веткой.
pub(super) struct WorkerExitReporter {
    mailbox: Arc<WorkerMailbox>,
    clean: bool,
}

impl WorkerExitReporter {
    pub(super) fn new(mailbox: Arc<WorkerMailbox>) -> Self {
        Self {
            mailbox,
            clean: false,
        }
    }

    pub(super) fn mark_clean(&mut self) {
        self.clean = true;
    }
}

impl Drop for WorkerExitReporter {
    fn drop(&mut self) {
        if !self.clean {
            self.mailbox
                .publish_disconnect(WorkerDisconnectReason::UnexpectedThreadExit);
        }
    }
}
