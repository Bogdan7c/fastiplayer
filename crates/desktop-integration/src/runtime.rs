use std::sync::{Arc, RwLock};

use crossbeam_channel::Receiver;
use player_core::{PlayerCommand, PlayerSnapshot};

use crate::platform::{BackendControlCommand, BackendHandle};
use crate::{
    DesktopCommandSink, DesktopIntegrationError, DesktopIntegrationEvent, DesktopIntegrationResult,
    DesktopSnapshotChange, DesktopSnapshotView,
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
        self.backend_handle
            .command_sink()
            .send_desktop_command(command)
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
