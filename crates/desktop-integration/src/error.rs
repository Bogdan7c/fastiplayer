use thiserror::Error;

/// Единый тип ошибок neutral desktop integration boundary.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DesktopIntegrationError {
    /// Команда не была принята playback worker boundary.
    #[error("desktop command send failed: {0}")]
    CommandSendFailed(String),

    /// Latest snapshot lock повреждён panic-ом другого thread-а.
    #[error("desktop snapshot lock is poisoned")]
    SnapshotLockPoisoned,

    /// Platform backend не смог стартовать или обслужить platform API.
    #[error("desktop backend error: {0}")]
    Backend(String),

    /// Thread backend-а не был создан операционной системой.
    #[error("desktop backend thread spawn failed: {0}")]
    ThreadSpawn(String),

    /// Канал управления backend-ом уже закрыт.
    #[error("desktop backend control channel is disconnected")]
    BackendChannelDisconnected,

    /// Terminal shutdown уже закрыл admission новых desktop операций.
    #[error("desktop backend admission is closed for terminal shutdown")]
    BackendAdmissionClosed,

    /// Legacy shutdown вызван после bounded timeout-а и не может снова блокироваться.
    #[error("desktop backend shutdown deadline elapsed")]
    BackendShutdownTimedOut,

    /// Backend thread завершился panic-ом.
    #[error("desktop backend thread panicked during shutdown")]
    BackendThreadPanicked,
}

/// Result alias для public desktop integration API.
pub type DesktopIntegrationResult<T> = Result<T, DesktopIntegrationError>;
