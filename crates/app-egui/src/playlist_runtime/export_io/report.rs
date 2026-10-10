//! Типизированный итог экспорта плейлиста (B4): что записано, что пропущено и
//! почему не удалось записать файл. Текст для пользователя здесь не строится —
//! это делает `messages.rs`.

use std::io;

use atomic_file_store::{AtomicFileWriteCause, AtomicFileWriteFailure};
use playlist_io::PlaylistExportSkipCounts;

/// Итог успешной записи документа: сколько треков попало в файл и сколько
/// записей пришлось пропустить.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlaylistExportReport {
    /// Число треков, реально записанных в документ.
    pub(super) exported_tracks: usize,
    /// Пропущенные записи по категориям причин.
    pub(super) skipped: PlaylistExportSkipCounts,
    /// Составные группы записаны отдельными строками (формат не хранит границ).
    pub(super) flattened_compound_groups: bool,
}

/// Человеческая классификация сбоя записи файла (не путать с пропуском записей:
/// сбой записи отменяет весь экспорт, а целевой файл остаётся прежним).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlaylistExportWriteFailure {
    /// Нет прав на запись в выбранную папку.
    PermissionDenied,
    /// Файловая система только для чтения.
    ReadOnlyFilesystem,
    /// На диске нет места (или исчерпана квота).
    DiskFull,
    /// Выбранной папки не существует.
    DirectoryMissing,
    /// Любая другая причина.
    Other,
}

impl PlaylistExportWriteFailure {
    /// Сворачивает безопасный класс ошибки atomic writer-а в причину для UI.
    pub(super) const fn classify(failure: AtomicFileWriteFailure) -> Self {
        match failure.cause {
            AtomicFileWriteCause::Io(kind) => Self::from_io_kind(kind),
            AtomicFileWriteCause::TempNameAttemptsExhausted => Self::Other,
        }
    }

    /// Отдельная функция по `ErrorKind`, чтобы маппинг ENOSPC/EROFS/EACCES
    /// тестировался без файловой системы.
    pub(super) const fn from_io_kind(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => Self::DiskFull,
            io::ErrorKind::NotFound => Self::DirectoryMissing,
            _ => Self::Other,
        }
    }
}
