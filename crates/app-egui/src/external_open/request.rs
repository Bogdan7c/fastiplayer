//! Типы «внешнего запроса открытия»: что пользователь принёс в окно и куда.
//!
//! Запрос не зависит от источника (drag & drop сегодня, «Открыть с помощью» из второго
//! экземпляра в сессии 13): оба источника сводятся к [`ExternalOpenRequest`] и дальше идут
//! одним и тем же конвейером классификации и маршрутизации.

use std::fmt;
use std::path::PathBuf;

/// Куда брошены данные: решает место броска, а не тип данных.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropTarget {
    /// Панель плейлиста: добавить в конец очереди.
    Playlist,
    /// Область видео (и любое другое место окна): открыть как новую очередь.
    Video,
}

/// Ссылка http(s), принесённая в окно.
///
/// В URL бывают токены и логины, поэтому `Debug` намеренно ничего не раскрывает:
/// значение не должно попасть в лог случайным `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DroppedWebUrl(String);

impl DroppedWebUrl {
    /// Запоминает уже очищенную от пробелов ссылку.
    pub(crate) fn new(url: String) -> Self {
        Self(url)
    }

    /// Исходный текст ссылки: только для передачи сервисам (классификатор Add URL),
    /// не для логов и UI.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DroppedWebUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DroppedWebUrl(<redacted>)")
    }
}

/// Один элемент брошенных данных после разбора URI, но до обращения к файловой системе.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExternalOpenItem {
    /// Локальный путь из `file://` без потерь (байты не-UTF-8 сохранены).
    LocalPath(PathBuf),
    /// Ссылка http(s).
    WebUrl(DroppedWebUrl),
    /// Что-то, что плеер открывать не умеет (`ftp:`, `file://чужой-хост/…`, мусор).
    Unsupported {
        /// Схема в нижнем регистре; пустая, если схемы нет.
        scheme: String,
    },
}

/// Ровно один запрос на один жест броска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExternalOpenRequest {
    /// Куда брошено.
    pub(crate) target: DropTarget,
    /// Элементы в порядке источника (порядок выбора в файловом менеджере).
    pub(crate) items: Vec<ExternalOpenItem>,
}
