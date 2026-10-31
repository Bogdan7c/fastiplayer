//! Что показать человеку, когда изменения соседней копии принять не удалось.
//!
//! Только перевод типизированного исхода в английский текст тоста (пользовательские
//! тексты английские, `mem:app-egui/user-facing-language`). Решение «показывать ли» —
//! у владельца доски (`shared_board.rs`), показ — у центра уведомлений окна.

/// Почему изменения из файла не применены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsFileNotice {
    /// Файл записан, но не читается (битый, не файл, нет прав).
    UnreadableFile,
    /// Файл записан более новой версией плеера.
    NewerFileVersion,
    /// Значения прочитаны, но применить их здесь не удалось; всё вернулось как было.
    AdoptionFailed,
    /// Применить не удалось, и возврат к прежним настройкам тоже не удался.
    AdoptionRollbackFailed,
}

impl SettingsFileNotice {
    /// Текст тоста.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::UnreadableFile => {
                "Settings file was changed but can't be read; keeping current settings."
            }
            Self::NewerFileVersion => {
                "Settings file was saved by a newer Fastiplayer version; keeping current settings."
            }
            Self::AdoptionFailed => {
                "Settings changed in another Fastiplayer window could not be applied here; \
                 keeping current settings."
            }
            Self::AdoptionRollbackFailed => {
                "Settings changed in another Fastiplayer window could not be applied here, and \
                 the previous settings could not be fully restored; restart Fastiplayer."
            }
        }
    }
}
