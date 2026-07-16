use crate::{DesktopIntegrationError, DesktopSnapshotChange};

/// Platform backend, который обслуживает текущую сборку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopBackendKind {
    /// Linux MPRIS поверх D-Bus session bus.
    LinuxMpris,

    /// Stub для платформ, где backend ещё не реализован.
    Stub,
}

/// События desktop integration boundary для shell telemetry/logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopIntegrationEvent {
    /// Backend thread создан и готов принимать snapshot updates.
    BackendStarted { backend: DesktopBackendKind },

    /// Backend thread завершил работу по штатному shutdown.
    BackendStopped { backend: DesktopBackendKind },

    /// Backend сообщил recoverable ошибку без падения приложения.
    BackendError {
        backend: DesktopBackendKind,
        error: DesktopIntegrationError,
    },

    /// Snapshot изменил MPRIS-signalled properties.
    SnapshotPropertiesChanged {
        backend: DesktopBackendKind,
        change: DesktopSnapshotChange,
    },
}
