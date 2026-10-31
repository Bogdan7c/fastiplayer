//! Заглушка слежения за config-ом для систем, где своей реализации пока нет.
//!
//! Плеер работает как раньше: изменения настроек, записанные другой копией, он увидит
//! после перезапуска. Под Windows здесь встанет своя реализация (например,
//! `ReadDirectoryChangesW` за каталогом config-а) с тем же контрактом, что у `linux.rs`:
//! сигнал только для имени файла config-а и один сигнал «сверься» после подписки.

use std::convert::Infallible;
use std::path::Path;

use super::{ConfigFileChangePublisher, ConfigFileWatchStopped, ConfigFileWatchUnavailable};

/// Платформенная часть [`super::ConfigFileWatch`]; значение этого типа создать нельзя.
pub(super) struct PlatformConfigWatch {
    never: Infallible,
}

impl PlatformConfigWatch {
    /// Всегда «не следим».
    pub(super) fn start(
        _config_file: &Path,
        _publisher: ConfigFileChangePublisher,
    ) -> Result<Self, ConfigFileWatchUnavailable> {
        Err(ConfigFileWatchUnavailable::UnsupportedPlatform)
    }

    /// Недостижимо: значения типа не существует.
    pub(super) fn stop(&mut self) -> ConfigFileWatchStopped {
        match self.never {}
    }
}
