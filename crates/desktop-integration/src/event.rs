use crate::{DesktopIntegrationError, DesktopSnapshotChange};

/// Platform backend, который обслуживает текущую сборку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopBackendKind {
    /// Linux MPRIS поверх D-Bus session bus.
    LinuxMpris,

    /// Stub для платформ, где backend ещё не реализован.
    Stub,
}

/// Что рабочий стол просит у окна через корневой интерфейс MPRIS (только личность
/// `MprisIdentity::BrowserSession`; у обычного плеера `Raise`/`Quit` не предлагаются).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopWindowRequest {
    /// `org.mpris.MediaPlayer2.Raise`: вывести окно вперёд.
    Raise,
    /// `org.mpris.MediaPlayer2.Quit`: закрыть окно так же, как закрыл бы человек.
    Quit,
}

/// Кто исполняет просьбы рабочего стола к окну. Зовётся с потока D-Bus: реализация
/// обязана быстро положить просьбу в свой ящик и разбудить цикл окна (не блокируясь).
pub trait DesktopWindowRequestSink: Send + Sync + 'static {
    /// Принимает одну просьбу; потерять её молча допустимо только при закрытом окне.
    fn request_window(&self, request: DesktopWindowRequest);
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
