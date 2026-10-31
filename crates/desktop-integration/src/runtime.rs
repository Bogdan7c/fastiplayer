use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossbeam_channel::Receiver;
use media_core::MediaTime;

use crate::platform::{BackendControlCommand, BackendHandle};
use crate::{
    DesktopCommandSink, DesktopIntegrationError, DesktopIntegrationEvent, DesktopIntegrationResult,
    DesktopIntegrationShutdownOutcome, DesktopSnapshotChange, DesktopSnapshotView,
    DesktopWindowRequestSink, MprisIdentity,
};

/// Read-only источник latest committed transport snapshot-а.
pub trait LatestSnapshotSource: Send + Sync + 'static {
    fn latest_snapshot(&self) -> DesktopIntegrationResult<DesktopSnapshotView>;

    /// Только позиция latest snapshot-а: getter MPRIS `Position` не должен клонировать
    /// весь вид (metadata со строками). Реализация по умолчанию — через полный снимок.
    fn latest_position(&self) -> DesktopIntegrationResult<MediaTime> {
        self.latest_snapshot().map(|snapshot| snapshot.position)
    }
}

/// Исход дешёвого обновления позиции в latest snapshot-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPositionOutcome {
    /// Позиция в latest snapshot-е заменена; revision и сигналы D-Bus не затронуты.
    Updated,
    /// Позиция уже была такой же; ничего не записано.
    Unchanged,
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

    /// Обновляет только позицию в latest snapshot-е: без клона вида, без смены revision.
    pub fn update_position(
        &self,
        position: MediaTime,
    ) -> DesktopIntegrationResult<DesktopPositionOutcome> {
        let mut guard = self
            .latest_snapshot
            .write()
            .map_err(|_| DesktopIntegrationError::SnapshotLockPoisoned)?;
        if guard.position == position {
            return Ok(DesktopPositionOutcome::Unchanged);
        }
        guard.position = position;
        Ok(DesktopPositionOutcome::Updated)
    }
}

impl LatestSnapshotSource for LatestSnapshotHandle {
    fn latest_snapshot(&self) -> DesktopIntegrationResult<DesktopSnapshotView> {
        self.latest_snapshot
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DesktopIntegrationError::SnapshotLockPoisoned)
    }

    fn latest_position(&self) -> DesktopIntegrationResult<MediaTime> {
        self.latest_snapshot
            .read()
            .map(|guard| guard.position)
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
        Self::spawn_with_identity(
            MprisIdentity::StandardPlayer,
            None,
            command_sink,
            initial_snapshot,
        )
    }

    /// Сервер MPRIS окна браузерного режима (04c): имя `…fastiplayer.browser`, `Identity`
    /// «Fastiplayer (browser)», `CanRaise`/`CanQuit` = true — просьбы `Raise`/`Quit`
    /// уходят в `window_sink`. Обычный плеер этим именем не пользуется.
    pub fn spawn_browser_window(
        command_sink: impl DesktopCommandSink,
        window_sink: Arc<dyn DesktopWindowRequestSink>,
        initial_snapshot: DesktopSnapshotView,
    ) -> DesktopIntegrationResult<Self> {
        Self::spawn_with_identity(
            MprisIdentity::BrowserSession,
            Some(window_sink),
            command_sink,
            initial_snapshot,
        )
    }

    fn spawn_with_identity(
        identity: MprisIdentity,
        window_sink: Option<Arc<dyn DesktopWindowRequestSink>>,
        command_sink: impl DesktopCommandSink,
        initial_snapshot: DesktopSnapshotView,
    ) -> DesktopIntegrationResult<Self> {
        let snapshot_handle = LatestSnapshotHandle::new(initial_snapshot);
        let command_sink: Arc<dyn DesktopCommandSink> = Arc::new(command_sink);
        let (backend_handle, event_rx) = crate::platform::spawn_backend(
            identity,
            window_sink,
            command_sink,
            snapshot_handle.clone(),
        )?;
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

    /// Дешёвое обновление позиции: revision не растёт, `SnapshotChanged` в backend не уходит
    /// (D-Bus и так не сигналит `Position`, клиенты читают её getter-ом).
    ///
    /// Admission как у [`Self::publish_snapshot`]: после terminal shutdown —
    /// `Err(BackendAdmissionClosed)`, состояние не меняется.
    pub fn publish_position(
        &self,
        position: MediaTime,
    ) -> DesktopIntegrationResult<DesktopPositionOutcome> {
        self.backend_handle.ensure_admission_open()?;
        self.snapshot_handle.update_position(position)
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
        // Drop не может вернуть ошибку, но и терять её нельзя: timeout, паника
        // backend-потока или разрыв канала фиксируются в логе. Повторный
        // shutdown после явного вызова возвращает Ok (AlreadyCompleted).
        if let Err(error) = self.shutdown() {
            tracing::warn!(?error, "Desktop integration didn’t end cleanly with drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use crossbeam_channel::unbounded;

    use super::*;
    use crate::{DesktopCommand, EffectiveVolume};
    use media_core::MediaTime;

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

    /// Интеграция с живым control-каналом: тест читает всё, что backend получил бы.
    fn integration_with_control_probe(
        initial: DesktopSnapshotView,
    ) -> (
        DesktopIntegration,
        crossbeam_channel::Receiver<BackendControlCommand>,
    ) {
        let (control_tx, control_rx) = unbounded();
        // Поток-заглушка просто живёт до закрытия канала; shutdown в тестах не нужен.
        let join_handle = thread::spawn(|| {});
        let (_event_tx, event_rx) = unbounded();
        let backend_handle =
            BackendHandle::threaded(Arc::new(AcceptingCommandSink), control_tx, join_handle);
        let integration = DesktopIntegration {
            snapshot_handle: LatestSnapshotHandle::new(initial),
            backend_handle,
            event_rx,
        };
        (integration, control_rx)
    }

    #[test]
    fn position_only_update_is_visible_without_revision_or_backend_signal() {
        let (integration, control_rx) = integration_with_control_probe(snapshot(5));
        let position = MediaTime::from_secs(42);

        assert_eq!(
            integration.publish_position(position),
            Ok(DesktopPositionOutcome::Updated)
        );

        let latest = integration.snapshot_handle.latest_snapshot().expect("lock");
        assert_eq!(latest.position, position);
        assert_eq!(latest.revision.get(), 5, "revision must stay untouched");
        assert_eq!(
            integration.snapshot_handle.latest_position(),
            Ok(position),
            "cheap getter must see the new position"
        );
        assert!(
            control_rx.try_recv().is_err(),
            "position-only update must not reach the backend control channel"
        );
        // Повтор той же позиции — явный Unchanged.
        assert_eq!(
            integration.publish_position(position),
            Ok(DesktopPositionOutcome::Unchanged)
        );
    }

    #[test]
    fn position_update_does_not_touch_other_snapshot_fields() {
        let mut initial = snapshot(2);
        initial.metadata.title = Some("keep me".to_owned());
        initial.shuffle = true;
        let (integration, _control_rx) = integration_with_control_probe(initial.clone());

        integration
            .publish_position(MediaTime::from_secs(9))
            .expect("open admission");

        let mut expected = initial;
        expected.position = MediaTime::from_secs(9);
        assert_eq!(
            integration.snapshot_handle.latest_snapshot().expect("lock"),
            expected
        );
    }

    #[test]
    fn position_update_after_terminal_shutdown_is_refused_and_state_unchanged() {
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
            integration.publish_position(MediaTime::from_secs(77)),
            Err(DesktopIntegrationError::BackendAdmissionClosed)
        );
        assert_eq!(snapshot_handle.latest_position(), Ok(MediaTime::ZERO));
    }
}
