//! Приём drag & drop из внешних приложений (additive-расширение winit 0.30).
//!
//! Зачем это нужно. Штатные `WindowEvent::HoveredFile` / `DroppedFile` в winit 0.30
//! приходят только на X11, не несут позицию курсора и умеют только `file://`-пути.
//! На Wayland data device в winit не реализован вообще. Этот модуль добавляет
//! отдельный канал по образцу `winit` 0.31 (`DragEntered` / `DragMoved` /
//! `DragDropped` / `DragLeft`), не меняя публичный `WindowEvent` (его исчерпывающе
//! матчит `egui-winit`).
//!
//! # Как получать события
//!
//! События приходят через [`ApplicationHandler::external_drag_event`]
//! (метод имеет реализацию по умолчанию, ничего не делающую, поэтому существующие
//! приложения продолжают компилироваться). Через устаревший closure-API
//! `EventLoop::run` эти события не доставляются.
//!
//! # Контракт жеста
//!
//! Один жест — это последовательность `Entered`, ноль или более `Moved`, затем
//! ровно один из двух финалов: `Dropped` либо `Left`. Если данные не удалось
//! прочитать (ошибка, превышение лимита, таймаут источника), жест завершается
//! событием `Left`, а причина пишется в лог (`tracing::warn`).
//!
//! # Переносимость
//!
//! События генерируются на Linux (X11 и Wayland). На остальных платформах модуль
//! существует, но события не приходят: там продолжают работать штатные
//! `DroppedFile` / `HoveredFile`.
//!
//! # Замена на winit 0.31
//!
//! Типы здесь намеренно повторяют понятия 0.31 (позиция, URI-список, текст), чтобы
//! приложение могло заменить один переходник и выбросить этот патч.
//!
//! [`ApplicationHandler::external_drag_event`]: crate::application::ApplicationHandler::external_drag_event

use crate::dpi::PhysicalPosition;

/// Событие жеста перетаскивания из внешнего приложения.
///
/// Позиции — физические пиксели в системе координат клиентской области окна
/// (с учётом коэффициента масштабирования окна).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExternalDragEvent {
    /// Курсор с перетаскиваемыми данными вошёл в окно, и данные поддерживаемого вида.
    Entered {
        /// Позиция курсора внутри окна.
        position: PhysicalPosition<f64>,
    },
    /// Курсор переместился над окном во время жеста.
    Moved {
        /// Новая позиция курсора внутри окна.
        position: PhysicalPosition<f64>,
    },
    /// Данные брошены в окно и полностью прочитаны.
    Dropped {
        /// Позиция, в которой пользователь отпустил кнопку.
        position: PhysicalPosition<f64>,
        /// Содержимое жеста: сырые URI и/или простой текст.
        payload: ExternalDragPayload,
    },
    /// Жест закончился без броска (курсор ушёл, отмена, ошибка чтения данных).
    Left,
}

/// Содержимое брошенных данных в «сыром» виде.
///
/// Патч намеренно ничего не интерпретирует: `file://` не превращается в путь и не
/// раскодируется из percent-encoding. Так не теряются байты не-UTF-8 имён файлов, а
/// решение «что это за ссылка» остаётся у приложения (в winit 0.31 URI приходят так
/// же строками).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExternalDragPayload {
    /// URI из `text/uri-list` (или ссылки из `text/x-moz-url`) в порядке источника;
    /// комментарии и пустые строки уже убраны.
    uris: Vec<String>,
    /// Простой текст, если источник отдал только его (например, выделенный текст).
    plain_text: Option<String>,
}

impl ExternalDragPayload {
    /// Создаёт содержимое жеста из разобранных частей (только внутри крейта).
    #[cfg_attr(not(free_unix), allow(dead_code))]
    pub(crate) fn new(uris: Vec<String>, plain_text: Option<String>) -> Self {
        Self { uris, plain_text }
    }

    /// Сырые RFC 3986 URI в порядке источника.
    pub fn uris(&self) -> &[String] {
        &self.uris
    }

    /// Простой текст, если он есть.
    pub fn plain_text(&self) -> Option<&str> {
        self.plain_text.as_deref()
    }

    /// `true`, если нет ни URI, ни текста.
    pub fn is_empty(&self) -> bool {
        self.uris.is_empty() && self.plain_text.is_none()
    }

    /// Разбирает содержимое на владеемые части: `(uris, plain_text)`.
    pub fn into_parts(self) -> (Vec<String>, Option<String>) {
        (self.uris, self.plain_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_accessors_return_constructed_parts() {
        let payload = ExternalDragPayload::new(
            vec!["file:///a%20b".to_owned(), "https://example.org/".to_owned()],
            Some("текст".to_owned()),
        );

        assert_eq!(payload.uris(), ["file:///a%20b", "https://example.org/"]);
        assert_eq!(payload.plain_text(), Some("текст"));
        assert!(!payload.is_empty());
        assert_eq!(
            payload.into_parts(),
            (
                vec!["file:///a%20b".to_owned(), "https://example.org/".to_owned()],
                Some("текст".to_owned())
            )
        );
    }

    #[test]
    fn default_payload_is_empty() {
        let payload = ExternalDragPayload::default();

        assert!(payload.is_empty());
        assert!(payload.uris().is_empty());
        assert_eq!(payload.plain_text(), None);
    }
}
