//! Классификация ошибок открытия локального файла в причину, понятную пользователю.
//!
//! Здесь только «бизнес-решение»: какая техническая ошибка какой человеческой причине
//! соответствует. Тексты для экрана строит отдельный модуль `crate::local_open_message`,
//! чтобы формулировки можно было менять, не трогая классификацию, и наоборот.

use std::error::Error;
use std::io;

use demux_api::{DemuxFactoryOpenError, DemuxOpenError, DemuxProbeRejection};
use media_source_open::local_media::LocalDemuxOpenError;
use source_core::SourceError;

use super::PrepareLocalOpenError;

/// Почему локальный файл не открылся — в терминах пользователя, без технических деталей.
///
/// Тип `Copy` и не содержит ни пути, ни текста ОС: его безопасно передавать через
/// coordinator, playlist runtime и UI, не рискуя раскрыть каталог или внутренний жаргон.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalOpenFailureReason {
    /// Файла по этому пути нет (в том числе удалён во время открытия).
    FileNotFound,
    /// ОС запретила доступ к файлу.
    AccessDenied,
    /// Путь указывает на папку.
    IsDirectory,
    /// Файл нулевого размера.
    EmptyFile,
    /// Ни один разборщик не узнал содержимое либо контейнер не поддерживается.
    UnrecognizedFormat,
    /// Контейнер узнан, но данные битые или обрезаны.
    DamagedOrTruncated,
    /// Прочая ошибка чтения с диска.
    ReadFailed,
    /// Чтение заголовка не уложилось в лимит времени (например, медленный сетевой диск).
    ReadTimedOut,
    /// Контейнер открылся, но в нём нет ни видео-, ни аудиодорожек.
    NoAudioOrVideo,
    /// Файл изменился, пока мы его открывали.
    ChangedDuringOpen,
    /// Сбой внутри плеера, не связанный с самим файлом.
    InternalError,
}

/// Итог неудачной подготовки с точки зрения пользователя.
///
/// Отмена вынесена в отдельный вариант, а не в «причину»: отмена — не ошибка,
/// и вызывающий код обязан не показывать по ней красное сообщение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalOpenFailureOutcome {
    /// Подготовку отменили (пользователь, замена запроса или выход из программы).
    Cancelled,
    /// Настоящая ошибка с причиной для показа.
    Failed(LocalOpenFailureReason),
}

impl PrepareLocalOpenError {
    /// Переводит техническую ошибку в итог для пользователя.
    pub(crate) fn user_outcome(&self) -> LocalOpenFailureOutcome {
        let reason = match self {
            Self::Cancelled => return LocalOpenFailureOutcome::Cancelled,
            Self::Source(SourceError::Cancelled) => return LocalOpenFailureOutcome::Cancelled,
            Self::Source(source_error) => classify_source_error(source_error),
            Self::Directory => LocalOpenFailureReason::IsDirectory,
            Self::EmptyFile => LocalOpenFailureReason::EmptyFile,
            Self::Demux(demux_error) => classify_local_demux_error(demux_error),
            Self::NoAudioVideoTracks => LocalOpenFailureReason::NoAudioOrVideo,
            Self::SourceChangedDuringPreparation => LocalOpenFailureReason::ChangedDuringOpen,
            Self::RevalidationIo(io_error) => classify_io_error_kind(io_error.kind()),
        };
        LocalOpenFailureOutcome::Failed(reason)
    }

    /// Полная техническая цепочка причин для лога.
    ///
    /// Часть уровней прячет причину в `#[source]` (её `Display` не печатает), а часть
    /// уже встраивает текст вложенной ошибки в свой `Display` (`{0}`, `{source}`).
    /// Поэтому обходим `Error::source()`, но пропускаем звено, чей текст уже есть в
    /// цепочке, — иначе одна и та же ошибка ОС попадала в лог по три раза. Имени файла
    /// и пути здесь нет: `io::Error` от `File::open`/`stat` путь не содержит, а
    /// demux-причины построены из байтов файла, а не из его расположения.
    pub(crate) fn diagnostic_chain(&self) -> String {
        let mut chain = self.to_string();
        let mut cause = self.source();
        while let Some(current_cause) = cause {
            let cause_text = current_cause.to_string();
            if !chain.contains(&cause_text) {
                chain.push_str(": ");
                chain.push_str(&cause_text);
            }
            cause = current_cause.source();
        }
        chain
    }
}

/// Ошибки локального источника различаем по `io::ErrorKind`, сохранённому в `#[source]`.
fn classify_source_error(source_error: &SourceError) -> LocalOpenFailureReason {
    match source_error {
        SourceError::LocalIo { source, .. } => classify_io_error_kind(source.kind()),
        // `LocalFileSource` порождает только `LocalIo` и `Cancelled` (отмена разобрана выше).
        // Сетевые варианты сюда попасть не могут; если это когда-то изменится,
        // честнее показать «внутреннюю ошибку», чем выдумывать причину про файл.
        _ => LocalOpenFailureReason::InternalError,
    }
}

fn classify_io_error_kind(kind: io::ErrorKind) -> LocalOpenFailureReason {
    match kind {
        io::ErrorKind::NotFound => LocalOpenFailureReason::FileNotFound,
        io::ErrorKind::PermissionDenied => LocalOpenFailureReason::AccessDenied,
        io::ErrorKind::IsADirectory => LocalOpenFailureReason::IsDirectory,
        io::ErrorKind::TimedOut => LocalOpenFailureReason::ReadTimedOut,
        _ => LocalOpenFailureReason::ReadFailed,
    }
}

fn classify_local_demux_error(demux_error: &LocalDemuxOpenError) -> LocalOpenFailureReason {
    match demux_error {
        // Набор разборщиков не собрался — это проблема плеера, а не файла.
        LocalDemuxOpenError::RegistrySetup(_) => LocalOpenFailureReason::InternalError,
        LocalDemuxOpenError::Open(open_error) => classify_demux_open_error(open_error),
    }
}

/// Матч исчерпывающий: новый вариант в `demux-api` заставит явно решить его причину.
fn classify_demux_open_error(open_error: &DemuxOpenError) -> LocalOpenFailureReason {
    match open_error {
        DemuxOpenError::NoMatch => LocalOpenFailureReason::UnrecognizedFormat,
        DemuxOpenError::ProbeRejected(rejection) => classify_probe_rejection(rejection),
        DemuxOpenError::FactoryRejected { source, .. } => classify_factory_rejection(source),
        // Конфликт регистраций и несовпадение с требуемым контейнером — внутренние
        // решения registry, файл тут не виноват.
        DemuxOpenError::AmbiguousMatch { .. } | DemuxOpenError::UnexpectedContainer { .. } => {
            LocalOpenFailureReason::InternalError
        }
    }
}

fn classify_probe_rejection(rejection: &DemuxProbeRejection) -> LocalOpenFailureReason {
    match rejection {
        DemuxProbeRejection::Truncated { .. } | DemuxProbeRejection::Malformed { .. } => {
            LocalOpenFailureReason::DamagedOrTruncated
        }
        // Ошибка чтения уже превращена в строку внутри demux-api, `io::ErrorKind` потерян.
        // Каталог и нулевой размер отсечены раньше, поэтому остаётся общая «ошибка чтения».
        DemuxProbeRejection::InputFailure { .. } => LocalOpenFailureReason::ReadFailed,
        DemuxProbeRejection::DeadlineExceeded { .. } => LocalOpenFailureReason::ReadTimedOut,
        // Отмену `LocalDemuxOpenError::is_cancelled()` превращает в `Cancelled` ещё в
        // `prepare_local_open`; сюда она дойти не должна. Несовпадение формы входа —
        // контракт registry, а не свойство файла.
        DemuxProbeRejection::Cancelled | DemuxProbeRejection::UnsupportedInput { .. } => {
            LocalOpenFailureReason::InternalError
        }
    }
}

fn classify_factory_rejection(rejection: &DemuxFactoryOpenError) -> LocalOpenFailureReason {
    match rejection {
        // Разборщик узнал контейнер, но сломался на его содержимом.
        DemuxFactoryOpenError::Backend(_) => LocalOpenFailureReason::DamagedOrTruncated,
        // Контейнер узнан, но конкретный вариант не поддерживается.
        DemuxFactoryOpenError::Rejected { .. } => LocalOpenFailureReason::UnrecognizedFormat,
        DemuxFactoryOpenError::Cancelled | DemuxFactoryOpenError::UnsupportedInput { .. } => {
            LocalOpenFailureReason::InternalError
        }
    }
}

#[cfg(test)]
mod tests;
