use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use tracing::debug;

use crate::{
    DesktopBackendKind, DesktopCommandSink, DesktopIntegrationError, DesktopIntegrationEvent,
    DesktopIntegrationResult, DesktopSnapshotChange, LatestSnapshotHandle,
};

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
        }
    }

    /// Создаёт no-op handle для platform stubs.
    pub(crate) fn stub(command_sink: Arc<dyn DesktopCommandSink>) -> Self {
        Self {
            command_sink,
            control_tx: None,
            join_handle: None,
        }
    }

    /// Возвращает command sink для neutral convenience APIs.
    pub(crate) fn command_sink(&self) -> Arc<dyn DesktopCommandSink> {
        Arc::clone(&self.command_sink)
    }

    /// Отправляет control event backend-у.
    pub(crate) fn send_control(
        &self,
        command: BackendControlCommand,
    ) -> DesktopIntegrationResult<()> {
        let Some(control_tx) = &self.control_tx else {
            return Ok(());
        };

        control_tx
            .try_send(command)
            .map_err(|_| DesktopIntegrationError::BackendChannelDisconnected)
    }

    /// Штатно завершает backend thread.
    pub(crate) fn shutdown(&mut self) -> DesktopIntegrationResult<()> {
        if let Some(control_tx) = self.control_tx.take() {
            if let Err(error) = control_tx.try_send(BackendControlCommand::Shutdown) {
                debug!(error = %error, "Desktop integration backend shutdown channel is closed");
            }
        }

        if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .map_err(|_| DesktopIntegrationError::BackendThreadPanicked)?;
        }

        Ok(())
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
#[allow(dead_code)]
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
