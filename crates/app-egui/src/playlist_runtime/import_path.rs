//! Импорт плейлиста по уже известному пути (файл, брошенный в окно).
//!
//! Тот же путь, что у `start_playlist_import_dialog`, но без native dialog: предпросмотр
//! и (для замены) подтверждение дальше идут существующими S08/S14A границами.

use std::path::PathBuf;

use super::PlaylistRuntime;
use super::import_transaction::PlaylistImportIntent;

/// Итог запроса импорта по пути: различает причины, по которым импорт не стартовал.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistPathImportStart {
    /// Разбор файла запущен; результат придёт как обычный preview.
    Started,
    /// Runtime закрывается: импорт не принимается.
    RuntimeClosed,
    /// Уже идёт другой импорт (диалог или разбор): ничего не отменено.
    ImportAlreadyRunning,
    /// Поток разбора не удалось создать.
    SpawnFailed,
}

impl PlaylistRuntime {
    /// Запускает импорт плейлиста из `path` с явным намерением (добавить / заменить).
    ///
    /// Шаги зеркалят `start_playlist_import_dialog`: admission, защита от дубля, отмена
    /// стартового apply и прежнего preview/подтверждения, затем разбор в фоне.
    pub(crate) fn start_playlist_import_path(
        &mut self,
        path: PathBuf,
        intent: PlaylistImportIntent,
    ) -> PlaylistPathImportStart {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return PlaylistPathImportStart::RuntimeClosed;
        }
        if self.import_io.is_open() {
            return PlaylistPathImportStart::ImportAlreadyRunning;
        }
        self.supersede_startup_media_apply();
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
        match self.import_io.start_path(path, intent) {
            Ok(true) => PlaylistPathImportStart::Started,
            Ok(false) => PlaylistPathImportStart::ImportAlreadyRunning,
            Err(error) => {
                tracing::warn!(%error, "Не удалось запустить импорт плейлиста по пути");
                PlaylistPathImportStart::SpawnFailed
            }
        }
    }
}

#[cfg(test)]
mod tests;
