use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossbeam_channel::Receiver;

use crate::platform::{BackendControlCommand, BackendHandle};
use crate::{
    DesktopCommandSink, DesktopIntegrationError, DesktopIntegrationEvent, DesktopIntegrationResult,
    DesktopIntegrationShutdownOutcome, DesktopSnapshotChange, DesktopSnapshotView,
};

/// Read-only источник latest committed transport snapshot-а.
pub trait LatestSnapshotSource: Send + Sync + 'static {
    fn latest_snapshot(&self) -> DesktopIntegrationResult<DesktopSnapshotView>;
}

/// Latest-only shared storage между app owner и backend thread.
#[derive(Debug, Clone)]
pub struct LatestSnapshotHandle {
    latest_snapshot: Arc<RwLock<DesktopSnapshotView>>,
}

impl LatestSnapshotHandle {
    #[must_use]
    pub fn new(initial: DesktopSnapshotView) -> Self {
        Self {
            latest_snapshot: Arc::new(RwLock::new(initial)),
        }
    }

    pub fn publish_snapshot(
        &self,
        snapshot: DesktopSnapshotView,
    ) -> DesktopIntegrationResult<Option<DesktopSnapshotView>> {
        let mut guard = self
            .latest_snapshot
            .write()
            .map_err(|_| DesktopIntegrationError::SnapshotLockPoisoned)?;
        if snapshot.revision <= guard.revision {
            return Ok(None);
        }
        Ok(Some(std::mem::replace(&mut *guard, snapshot)))
    }
}

impl LatestSnapshotSource for LatestSnapshotHandle {
    fn latest_snapshot(&self) -> DesktopIntegrationResult<DesktopSnapshotView> {
        self.latest_snapshot
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DesktopIntegrationError::SnapshotLockPoisoned)
    }
}

/// Process-lifetime desktop backend handle.
pub struct DesktopIntegration {
    snapshot_handle: LatestSnapshotHandle,
    backend_handle: BackendHandle,
    event_rx: Receiver<DesktopIntegrationEvent>,
}

impl DesktopIntegration {
    /// Запускает backend с neutral app sink после caller-owned instance lease.
    pub fn spawn(
        command_sink: impl DesktopCommandSink,
        initial_snapshot: DesktopSnapshotView,
    ) -> DesktopIntegrationResult<Self> {
        let snapshot_handle = LatestSnapshotHandle::new(initial_snapshot);
        let command_sink: Arc<dyn DesktopCommandSink> = Arc::new(command_sink);
        let (backend_handle, event_rx) =
            crate::platform::spawn_backend(command_sink, snapshot_handle.clone())?;
        Ok(Self {
            snapshot_handle,
            backend_handle,
            event_rx,
        })
    }

    /// Публикует только более новую committed revision.
    pub fn publish_snapshot(
        &self,
        snapshot: DesktopSnapshotView,
    ) -> DesktopIntegrationResult<bool> {
        self.backend_handle.ensure_admission_open()?;
        let Some(previous) = self.snapshot_handle.publish_snapshot(snapshot.clone())? else {
            return Ok(false);
        };
        let change = DesktopSnapshotChange::from_views(&previous, &snapshot);
        if change.has_notifications() {
            self.backend_handle
                .send_control(BackendControlCommand::SnapshotChanged(change))?;
        }
        Ok(true)
    }

    #[must_use]
    pub fn drain_events(&self) -> Vec<DesktopIntegrationEvent> {
        self.event_rx.try_iter().collect()
    }

    pub fn shutdown_until(&mut self, deadline: Instant) -> DesktopIntegrationShutdownOutcome {
        self.backend_handle.shutdown_until(deadline)
    }

    pub fn shutdown(&mut self) -> DesktopIntegrationResult<()> {
        self.backend_handle.shutdown()
    }
}

impl Drop for DesktopIntegration {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::thread;
    use std::time::Duration;

    use crossbeam_channel::unbounded;

    use super::*;
    use crate::{DesktopCommand, DesktopCommandRequestId, EffectiveVolume};

    struct AcceptingCommandSink;
    impl DesktopCommandSink for AcceptingCommandSink {
        fn send_desktop_command(&self, _command: DesktopCommand) -> DesktopIntegrationResult<()> {
            Ok(())
        }
    }

    fn snapshot(revision: u64) -> DesktopSnapshotView {
        let mut snapshot =
            DesktopSnapshotView::neutral(EffectiveVolume::from_player(1.0).expect("volume"));
        snapshot.revision = crate::DesktopSnapshotRevision::new(revision);
        snapshot
    }

    #[test]
    fn stale_revision_cannot_roll_latest_snapshot_back() {
        let handle = LatestSnapshotHandle::new(snapshot(1));
        assert!(
            handle
                .publish_snapshot(snapshot(3))
                .expect("lock")
                .is_some()
        );
        assert!(
            handle
                .publish_snapshot(snapshot(2))
                .expect("lock")
                .is_none()
        );
        assert_eq!(handle.latest_snapshot().expect("lock").revision.get(), 3);
        let _ = DesktopCommandRequestId::new(NonZeroU64::MIN);
    }

    #[test]
    fn terminal_shutdown_closes_snapshot_admission_before_mutation() {
        let snapshot_handle = LatestSnapshotHandle::new(snapshot(1));
        let (control_tx, control_rx) = unbounded();
        let join_handle = thread::spawn(move || {
            assert_eq!(control_rx.recv(), Ok(BackendControlCommand::Shutdown))
        });
        let (_event_tx, event_rx) = unbounded();
        let backend_handle =
            BackendHandle::threaded(Arc::new(AcceptingCommandSink), control_tx, join_handle);
        let mut integration = DesktopIntegration {
            snapshot_handle: snapshot_handle.clone(),
            backend_handle,
            event_rx,
        };
        assert_eq!(
            integration.shutdown_until(Instant::now() + Duration::from_secs(1)),
            DesktopIntegrationShutdownOutcome::Completed
        );
        assert_eq!(
            integration.publish_snapshot(snapshot(2)),
            Err(DesktopIntegrationError::BackendAdmissionClosed)
        );
        assert_eq!(
            snapshot_handle
                .latest_snapshot()
                .expect("lock")
                .revision
                .get(),
            1
        );
    }
}
