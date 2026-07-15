use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use tracing::debug;

#[cfg(not(target_os = "linux"))]
use crate::DesktopBackendKind;
use crate::{
    DesktopCommandSink, DesktopIntegrationError, DesktopIntegrationEvent, DesktopIntegrationResult,
    DesktopIntegrationShutdownOutcome, DesktopIntegrationShutdownTransportFailure,
    DesktopSnapshotChange, LatestSnapshotHandle,
};

/// Короткий interval нужен только между non-blocking `is_finished` checks.
const SHUTDOWN_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod stub;
#[cfg(target_os = "windows")]
mod windows;

/// Control messages от neutral runtime к platform backend thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendControlCommand {
    /// Snapshot изменил MPRIS-signalled properties.
    SnapshotChanged(DesktopSnapshotChange),

    /// Нужно штатно завершить backend.
    Shutdown,
}

/// Handle platform backend-а, скрывающий thread/channel детали.
pub(crate) struct BackendHandle {
    /// Command sink хранится здесь, чтобы lifetime совпадал с backend-ом.
    command_sink: Arc<dyn DesktopCommandSink>,

    /// Канал управления backend thread-ом.
    control_tx: Option<Sender<BackendControlCommand>>,

    /// Join handle, если backend реально стартовал thread.
    join_handle: Option<thread::JoinHandle<()>>,

    /// Terminal request уже отправлен или была сделана единственная попытка отправки.
    shutdown_requested: bool,

    /// Хотя бы один bounded drain завершился timeout-ом.
    shutdown_timed_out: bool,

    /// Transport failure первой и единственной отправки terminal request.
    shutdown_transport_failure: Option<DesktopIntegrationShutdownTransportFailure>,

    /// Сохранённый terminal результат после единственного join-а.
    terminal_outcome: Option<BackendTerminalOutcome>,

    /// Успешный terminal результат уже был возвращён owner-у.
    completion_reported: bool,
}

/// Internal terminal result отделяет сохранённое состояние от public no-op outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendTerminalOutcome {
    /// Backend завершён без panic-а и transport failure.
    Completed,

    /// Backend thread завершился panic-ом.
    ThreadPanicked,

    /// Terminal control request не был принят backend transport-ом.
    TransportFailed(DesktopIntegrationShutdownTransportFailure),
}

impl BackendHandle {
    /// Создаёт handle для backend thread-а.
    pub(crate) fn threaded(
        command_sink: Arc<dyn DesktopCommandSink>,
        control_tx: Sender<BackendControlCommand>,
        join_handle: thread::JoinHandle<()>,
    ) -> Self {
        Self {
            command_sink,
            control_tx: Some(control_tx),
            join_handle: Some(join_handle),
            shutdown_requested: false,
            shutdown_timed_out: false,
            shutdown_transport_failure: None,
            terminal_outcome: None,
            completion_reported: false,
        }
    }

    /// Создаёт no-op handle для platform stubs.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn stub(command_sink: Arc<dyn DesktopCommandSink>) -> Self {
        Self {
            command_sink,
            control_tx: None,
            join_handle: None,
            shutdown_requested: false,
            shutdown_timed_out: false,
            shutdown_transport_failure: None,
            terminal_outcome: None,
            completion_reported: false,
        }
    }

    /// Возвращает command sink для neutral convenience APIs.
    pub(crate) fn command_sink(&self) -> Arc<dyn DesktopCommandSink> {
        Arc::clone(&self.command_sink)
    }

    /// Проверяет, что terminal shutdown ещё не закрыл desktop admission.
    pub(crate) fn ensure_admission_open(&self) -> DesktopIntegrationResult<()> {
        if self.shutdown_requested {
            return Err(DesktopIntegrationError::BackendAdmissionClosed);
        }

        Ok(())
    }

    /// Отправляет control event backend-у.
    pub(crate) fn send_control(
        &self,
        command: BackendControlCommand,
    ) -> DesktopIntegrationResult<()> {
        self.ensure_admission_open()?;

        let Some(control_tx) = &self.control_tx else {
            return Ok(());
        };

        control_tx
            .try_send(command)
            .map_err(|_| DesktopIntegrationError::BackendChannelDisconnected)
    }

    /// Запрашивает terminal shutdown и ждёт только до абсолютного deadline.
    pub(crate) fn shutdown_until(
        &mut self,
        deadline: Instant,
    ) -> DesktopIntegrationShutdownOutcome {
        self.request_shutdown_once();

        loop {
            self.join_finished_backend();
            if self.terminal_outcome.is_some() {
                return self.report_terminal_outcome();
            }

            let now = Instant::now();
            if now >= deadline {
                self.shutdown_timed_out = true;
                return DesktopIntegrationShutdownOutcome::TimedOut;
            }

            let remaining = deadline.saturating_duration_since(now);
            thread::park_timeout(remaining.min(SHUTDOWN_DRAIN_POLL_INTERVAL));
        }
    }

    /// Сохраняет прежний blocking cleanup для owner-ов без bounded timeout-а.
    pub(crate) fn shutdown(&mut self) -> DesktopIntegrationResult<()> {
        let outcome = if self.shutdown_timed_out {
            self.shutdown_until(Instant::now())
        } else {
            self.shutdown_blocking()
        };

        match outcome {
            DesktopIntegrationShutdownOutcome::Completed
            | DesktopIntegrationShutdownOutcome::AlreadyCompleted => Ok(()),
            DesktopIntegrationShutdownOutcome::TimedOut => {
                Err(DesktopIntegrationError::BackendShutdownTimedOut)
            }
            DesktopIntegrationShutdownOutcome::ThreadPanicked => {
                Err(DesktopIntegrationError::BackendThreadPanicked)
            }
            DesktopIntegrationShutdownOutcome::TransportFailed(
                DesktopIntegrationShutdownTransportFailure::ControlChannelDisconnected,
            ) => Err(DesktopIntegrationError::BackendChannelDisconnected),
        }
    }

    /// Единожды закрывает control admission и отправляет terminal command.
    fn request_shutdown_once(&mut self) {
        if self.shutdown_requested {
            return;
        }

        self.shutdown_requested = true;
        let Some(control_tx) = self.control_tx.take() else {
            return;
        };

        if let Err(error) = control_tx.try_send(BackendControlCommand::Shutdown) {
            debug!(error = %error, "Desktop integration backend shutdown channel is closed");
            self.shutdown_transport_failure =
                Some(DesktopIntegrationShutdownTransportFailure::ControlChannelDisconnected);
        }
    }

    /// Выполняет join только после подтверждения `is_finished`.
    fn join_finished_backend(&mut self) {
        let backend_finished = self
            .join_handle
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished);
        if !backend_finished {
            return;
        }

        self.join_backend_now();
    }

    /// Consume-ит единственную join authority и сохраняет terminal result.
    fn join_backend_now(&mut self) {
        if self.terminal_outcome.is_some() {
            return;
        }

        let join_result = self.join_handle.take().map(thread::JoinHandle::join);
        self.terminal_outcome = Some(match join_result {
            Some(Err(_)) => BackendTerminalOutcome::ThreadPanicked,
            Some(Ok(())) | None => match self.shutdown_transport_failure {
                Some(failure) => BackendTerminalOutcome::TransportFailed(failure),
                None => BackendTerminalOutcome::Completed,
            },
        });
    }

    /// Blocking join допустим только для legacy cleanup без предшествующего timeout-а.
    fn shutdown_blocking(&mut self) -> DesktopIntegrationShutdownOutcome {
        self.request_shutdown_once();
        self.join_backend_now();
        self.report_terminal_outcome()
    }

    /// Преобразует сохранённый terminal state в idempotent public outcome.
    fn report_terminal_outcome(&mut self) -> DesktopIntegrationShutdownOutcome {
        match self.terminal_outcome {
            Some(BackendTerminalOutcome::Completed) if self.completion_reported => {
                DesktopIntegrationShutdownOutcome::AlreadyCompleted
            }
            Some(BackendTerminalOutcome::Completed) => {
                self.completion_reported = true;
                DesktopIntegrationShutdownOutcome::Completed
            }
            Some(BackendTerminalOutcome::ThreadPanicked) => {
                DesktopIntegrationShutdownOutcome::ThreadPanicked
            }
            Some(BackendTerminalOutcome::TransportFailed(failure)) => {
                DesktopIntegrationShutdownOutcome::TransportFailed(failure)
            }
            None => DesktopIntegrationShutdownOutcome::TimedOut,
        }
    }

    /// После typed timeout Drop не блокирует и не detach-ит ещё живой thread.
    fn shutdown_for_drop(&mut self) {
        if !self.shutdown_timed_out {
            let _ = self.shutdown_blocking();
            return;
        }

        self.request_shutdown_once();
        self.join_finished_backend();
        if self.terminal_outcome.is_some() {
            return;
        }

        if let Some(join_handle) = self.join_handle.take() {
            // Process owner после D68 timeout обязан немедленно завершить process. Не вызываем
            // `JoinHandle::drop`, потому что это detach; leaked handle сохраняет join authority
            // до освобождения process address space и не позволяет Drop снова заблокироваться.
            std::mem::forget(join_handle);
        }
    }
}

impl Drop for BackendHandle {
    /// Гарантирует cleanup даже если верхний neutral wrapper изменится в будущем.
    fn drop(&mut self) {
        self.shutdown_for_drop();
    }
}

/// Запускает backend, выбранный compile-time platform cfg.
pub(crate) fn spawn_backend(
    command_sink: Arc<dyn DesktopCommandSink>,
    snapshot_source: LatestSnapshotHandle,
) -> DesktopIntegrationResult<(BackendHandle, Receiver<DesktopIntegrationEvent>)> {
    let (event_tx, event_rx) = unbounded();

    #[cfg(target_os = "linux")]
    let backend_handle = linux::spawn(command_sink, snapshot_source, event_tx)?;

    #[cfg(target_os = "macos")]
    let backend_handle = macos::spawn(command_sink, snapshot_source, event_tx)?;

    #[cfg(target_os = "windows")]
    let backend_handle = windows::spawn(command_sink, snapshot_source, event_tx)?;

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let backend_handle = stub::spawn(command_sink, snapshot_source, event_tx)?;

    Ok((backend_handle, event_rx))
}

/// Helper для stub modules.
#[cfg(not(target_os = "linux"))]
pub(crate) fn spawn_stub_backend(
    command_sink: Arc<dyn DesktopCommandSink>,
    _snapshot_source: LatestSnapshotHandle,
    event_tx: Sender<DesktopIntegrationEvent>,
) -> DesktopIntegrationResult<BackendHandle> {
    if let Err(error) = event_tx.send(DesktopIntegrationEvent::BackendStarted {
        backend: DesktopBackendKind::Stub,
    }) {
        debug!(error = %error, "Desktop integration stub event receiver is closed");
    }

    Ok(BackendHandle::stub(command_sink))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crossbeam_channel::{bounded, unbounded};
    use player_core::PlayerCommand;

    use super::*;

    /// Минимальный neutral sink: lifecycle-тестам не нужен настоящий player worker.
    struct AcceptingCommandSink;

    impl DesktopCommandSink for AcceptingCommandSink {
        fn send_desktop_command(&self, _command: PlayerCommand) -> DesktopIntegrationResult<()> {
            Ok(())
        }
    }

    /// Создаёт typed backend handle вокруг полностью управляемого test thread-а.
    fn test_backend(
        control_tx: Sender<BackendControlCommand>,
        join_handle: thread::JoinHandle<()>,
    ) -> BackendHandle {
        BackendHandle::threaded(Arc::new(AcceptingCommandSink), control_tx, join_handle)
    }

    #[test]
    fn delayed_backend_timeout_retains_join_authority_and_later_reaps() {
        let (control_tx, control_rx) = unbounded();
        let (release_tx, release_rx) = bounded::<()>(1);
        let join_handle = thread::spawn(move || {
            assert_eq!(control_rx.recv(), Ok(BackendControlCommand::Shutdown));
            assert_eq!(release_rx.recv(), Ok(()));
        });
        let mut backend = test_backend(control_tx, join_handle);

        assert_eq!(
            backend.shutdown_until(Instant::now()),
            DesktopIntegrationShutdownOutcome::TimedOut
        );
        assert!(backend.join_handle.is_some());

        release_tx.send(()).expect("test release channel is open");
        assert_eq!(
            backend.shutdown_until(Instant::now() + Duration::from_secs(1)),
            DesktopIntegrationShutdownOutcome::Completed
        );
        assert!(backend.join_handle.is_none());
        assert_eq!(
            backend.shutdown_until(Instant::now()),
            DesktopIntegrationShutdownOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn clean_shutdown_closes_admission_and_sends_request_once() {
        let request_count = Arc::new(AtomicUsize::new(0));
        let thread_request_count = Arc::clone(&request_count);
        let (control_tx, control_rx) = unbounded();
        let join_handle = thread::spawn(move || {
            while let Ok(command) = control_rx.recv() {
                assert_eq!(command, BackendControlCommand::Shutdown);
                thread_request_count.fetch_add(1, Ordering::SeqCst);
            }
        });
        let mut backend = test_backend(control_tx, join_handle);

        assert_eq!(
            backend.shutdown_until(Instant::now() + Duration::from_secs(1)),
            DesktopIntegrationShutdownOutcome::Completed
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            backend.ensure_admission_open(),
            Err(DesktopIntegrationError::BackendAdmissionClosed)
        );
        assert_eq!(
            backend.shutdown_until(Instant::now()),
            DesktopIntegrationShutdownOutcome::AlreadyCompleted
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn backend_panic_is_a_typed_terminal_outcome() {
        let (control_tx, control_rx) = unbounded();
        let join_handle = thread::spawn(move || {
            assert_eq!(control_rx.recv(), Ok(BackendControlCommand::Shutdown));
            panic!("synthetic backend panic");
        });
        let mut backend = test_backend(control_tx, join_handle);

        assert_eq!(
            backend.shutdown_until(Instant::now() + Duration::from_secs(1)),
            DesktopIntegrationShutdownOutcome::ThreadPanicked
        );
        assert_eq!(
            backend.shutdown_until(Instant::now()),
            DesktopIntegrationShutdownOutcome::ThreadPanicked
        );
    }

    #[test]
    fn disconnected_transport_is_typed_and_not_retried() {
        let (control_tx, control_rx) = unbounded();
        drop(control_rx);
        let join_handle = thread::spawn(|| {});
        let mut backend = test_backend(control_tx, join_handle);
        let expected = DesktopIntegrationShutdownOutcome::TransportFailed(
            DesktopIntegrationShutdownTransportFailure::ControlChannelDisconnected,
        );

        assert_eq!(
            backend.shutdown_until(Instant::now() + Duration::from_secs(1)),
            expected
        );
        assert_eq!(backend.shutdown_until(Instant::now()), expected);
        assert!(backend.control_tx.is_none());
    }

    #[test]
    fn drop_after_typed_timeout_is_nonblocking_and_thread_can_finish() {
        let (control_tx, control_rx) = unbounded();
        let (shutdown_observed_tx, shutdown_observed_rx) = bounded::<()>(1);
        let (release_tx, release_rx) = bounded::<()>(1);
        let (thread_finished_tx, thread_finished_rx) = bounded::<()>(1);
        let join_handle = thread::spawn(move || {
            assert_eq!(control_rx.recv(), Ok(BackendControlCommand::Shutdown));
            shutdown_observed_tx
                .send(())
                .expect("shutdown observer is alive");
            assert_eq!(release_rx.recv(), Ok(()));
            thread_finished_tx
                .send(())
                .expect("thread completion observer is alive");
        });
        let mut backend = test_backend(control_tx, join_handle);

        assert_eq!(
            backend.shutdown_until(Instant::now()),
            DesktopIntegrationShutdownOutcome::TimedOut
        );
        shutdown_observed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("backend observed the terminal request");

        let (drop_finished_tx, drop_finished_rx) = bounded::<()>(1);
        thread::spawn(move || {
            drop(backend);
            drop_finished_tx.send(()).expect("drop observer is alive");
        });
        drop_finished_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("Drop after typed timeout must not wait for backend completion");

        release_tx.send(()).expect("test release channel is open");
        thread_finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("leaked join authority does not prevent backend completion");
    }
}
