use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossbeam_channel::Receiver;
use player_core::{PlayerCommand, PlayerSnapshot};

use crate::platform::{BackendControlCommand, BackendHandle};
use crate::{
    DesktopCommandSink, DesktopIntegrationError, DesktopIntegrationEvent, DesktopIntegrationResult,
    DesktopIntegrationShutdownOutcome, DesktopSnapshotChange, DesktopSnapshotView,
};

/// Read-only источник latest snapshot для platform backend-ов.
pub trait LatestSnapshotSource: Send + Sync + 'static {
    /// Возвращает последний опубликованный `PlayerSnapshot`.
    fn latest_snapshot(&self) -> DesktopIntegrationResult<PlayerSnapshot>;
}

/// Shared latest snapshot storage между app shell и desktop backend thread.
#[derive(Debug, Clone)]
pub struct LatestSnapshotHandle {
    /// Единственное состояние: последний read-only snapshot.
    latest_snapshot: Arc<RwLock<PlayerSnapshot>>,
}

impl LatestSnapshotHandle {
    /// Создаёт storage с empty player snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            latest_snapshot: Arc::new(RwLock::new(PlayerSnapshot::empty())),
        }
    }

    /// Публикует новый snapshot и возвращает previous snapshot для diff-а.
    pub fn publish_snapshot(
        &self,
        snapshot: PlayerSnapshot,
    ) -> DesktopIntegrationResult<PlayerSnapshot> {
        let mut snapshot_guard = self
            .latest_snapshot
            .write()
            .map_err(|_| DesktopIntegrationError::SnapshotLockPoisoned)?;
        let previous_snapshot = std::mem::replace(&mut *snapshot_guard, snapshot);

        Ok(previous_snapshot)
    }
}

impl Default for LatestSnapshotHandle {
    /// Создаёт default storage без media.
    fn default() -> Self {
        Self::new()
    }
}

impl LatestSnapshotSource for LatestSnapshotHandle {
    /// Читает latest snapshot без доступа к player internals.
    fn latest_snapshot(&self) -> DesktopIntegrationResult<PlayerSnapshot> {
        self.latest_snapshot
            .read()
            .map(|snapshot_guard| snapshot_guard.clone())
            .map_err(|_| DesktopIntegrationError::SnapshotLockPoisoned)
    }
}

/// Верхнеуровневый desktop integration runtime.
pub struct DesktopIntegration {
    /// Shared latest snapshot source для backend-а.
    snapshot_handle: LatestSnapshotHandle,

    /// Platform backend handle и shutdown path.
    backend_handle: BackendHandle,

    /// События backend-а для shell logging/telemetry.
    event_rx: Receiver<DesktopIntegrationEvent>,
}

impl DesktopIntegration {
    /// Запускает platform backend с command sink-ом worker boundary.
    pub fn spawn(command_sink: impl DesktopCommandSink) -> DesktopIntegrationResult<Self> {
        let snapshot_handle = LatestSnapshotHandle::new();
        let command_sink: Arc<dyn DesktopCommandSink> = Arc::new(command_sink);
        let (backend_handle, event_rx) =
            crate::platform::spawn_backend(command_sink, snapshot_handle.clone())?;

        Ok(Self {
            snapshot_handle,
            backend_handle,
            event_rx,
        })
    }

    /// Публикует latest player snapshot и уведомляет backend о signalled property changes.
    pub fn publish_snapshot(&self, snapshot: &PlayerSnapshot) -> DesktopIntegrationResult<()> {
        // После terminal request snapshot storage тоже становится read-only: иначе UI мог бы
        // наблюдать локальный commit, который уже невозможно экспортировать platform backend-у.
        self.backend_handle.ensure_admission_open()?;
        let previous_snapshot = self.snapshot_handle.publish_snapshot(snapshot.clone())?;
        let previous_view = DesktopSnapshotView::from_player_snapshot(&previous_snapshot);
        let current_view = DesktopSnapshotView::from_player_snapshot(snapshot);
        let change = DesktopSnapshotChange::from_views(&previous_view, &current_view);

        if change.has_property_changes() {
            self.backend_handle
                .send_control(BackendControlCommand::SnapshotChanged(change))?;
        }

        Ok(())
    }

    /// Забирает события backend-а без блокировки UI thread.
    #[must_use]
    pub fn drain_events(&self) -> Vec<DesktopIntegrationEvent> {
        self.event_rx.try_iter().collect()
    }

    /// Convenience wrapper для тестов и будущих platform adapters.
    pub fn send_command(&self, command: PlayerCommand) -> DesktopIntegrationResult<()> {
        self.backend_handle.ensure_admission_open()?;
        self.backend_handle
            .command_sink()
            .send_desktop_command(command)
    }

    /// Terminal shutdown с общим абсолютным deadline process owner-а.
    ///
    /// При `TimedOut` backend handle остаётся внутри `DesktopIntegration`, поэтому следующий
    /// вызов может безопасно reap-нуть завершившийся thread. После такого timeout-а Drop никогда
    /// не выполняет blocking join: process owner обязан сохранить terminal lifecycle и выйти.
    pub fn shutdown_until(&mut self, deadline: Instant) -> DesktopIntegrationShutdownOutcome {
        self.backend_handle.shutdown_until(deadline)
    }

    /// Явно останавливает backend thread.
    pub fn shutdown(&mut self) -> DesktopIntegrationResult<()> {
        self.backend_handle.shutdown()
    }
}

impl Drop for DesktopIntegration {
    /// Drop path не должен оставлять D-Bus/backend thread после закрытия app shell.
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use crossbeam_channel::unbounded;

    use super::*;

    /// Lifecycle test sink не отправляет команды настоящему player worker-у.
    struct AcceptingCommandSink;

    impl DesktopCommandSink for AcceptingCommandSink {
        fn send_desktop_command(&self, _command: PlayerCommand) -> DesktopIntegrationResult<()> {
            Ok(())
        }
    }

    #[test]
    fn terminal_shutdown_closes_snapshot_admission_before_mutation() {
        let snapshot_handle = LatestSnapshotHandle::new();
        let mut published_snapshot = PlayerSnapshot::empty();
        published_snapshot.source_label = Some("committed-before-shutdown".to_string());
        snapshot_handle
            .publish_snapshot(published_snapshot.clone())
            .expect("test snapshot lock is healthy");

        let (control_tx, control_rx) = unbounded();
        let join_handle = thread::spawn(move || {
            assert_eq!(control_rx.recv(), Ok(BackendControlCommand::Shutdown));
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
        let mut rejected_snapshot = PlayerSnapshot::empty();
        rejected_snapshot.source_label = Some("must-not-be-published".to_string());
        assert_eq!(
            integration.publish_snapshot(&rejected_snapshot),
            Err(DesktopIntegrationError::BackendAdmissionClosed)
        );
        assert_eq!(
            snapshot_handle
                .latest_snapshot()
                .expect("test snapshot lock is healthy"),
            published_snapshot
        );
    }
}
