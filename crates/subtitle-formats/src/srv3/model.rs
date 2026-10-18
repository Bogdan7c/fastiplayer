//! Сырая модель srv3 после чтения XML: ещё без правил показа (окна, бегущие строки, очистка текста).
//!
//! Модель намеренно «глупая»: поля повторяют атрибуты файла как есть. Решения о смысле
//! (какие пары `t/d` корректны, как склеивать окна) принимают модули `lines` и `rolling`.

use std::collections::HashMap;

use super::pen::Pen;
use super::window::{WindowPosition, WindowStyle};

/// Определение окна из `<body><w id wp ws/>`: пара ссылок на позицию и стиль окна.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct WindowDefinition {
    /// Ссылка на `<wp id>` окна.
    pub(super) position_id: Option<u32>,
    /// Ссылка на `<ws id>` окна.
    pub(super) style_id: Option<u32>,
}

/// Один кусок текста абзаца: слово/фраза `<s>` либо текст прямо внутри `<p>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawSpan {
    /// Смещение появления от начала абзаца, мс (`<s t>`); `None` — атрибута нет.
    pub(super) offset_ms: Option<i64>,
    /// Ссылка на перо (`<s p>`); `None` — наследуется перо абзаца.
    pub(super) pen_id: Option<u32>,
    /// Текст уже с раскрытыми сущностями; `<br/>` и переводы строки внутри — символ `\n`.
    pub(super) text: String,
    /// Текст лежал прямо в `<p>` (не в `<s>`): соседние такие куски склеиваются в один.
    pub(super) is_direct_text: bool,
}

/// Абзац `<p>` как записан в файле.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawParagraph {
    /// Порядковый номер абзаца в файле с нуля: заменяет номер строки в отчёте.
    pub(super) ordinal: usize,
    /// Начало, мс; `None` — атрибут `t` отсутствует или нечитаем.
    pub(super) start_ms: Option<i64>,
    /// Длительность, мс; `None` — атрибута `d` нет; `Some(Err)` не хранится — нечитаемое = `None`.
    pub(super) duration_ms: Option<i64>,
    /// Окно (`w`): абзацы одного окна показываются вместе (бегущие строки автосубтитров).
    pub(super) window_id: Option<u32>,
    /// Собственная позиция абзаца (`wp`); перекрывает позицию окна.
    pub(super) position_id: Option<u32>,
    /// Собственный стиль окна абзаца (`ws`); перекрывает стиль окна.
    pub(super) style_id: Option<u32>,
    /// Перо абзаца по умолчанию (`p`).
    pub(super) pen_id: Option<u32>,
    /// `a="1"`: абзац-«добавление» (в автосубтитрах содержит лишь перевод строки).
    pub(super) is_append: bool,
    /// Куски текста в порядке файла.
    pub(super) spans: Vec<RawSpan>,
}

/// Весь файл после чтения XML.
#[derive(Debug, Default)]
pub(super) struct RawDocument {
    /// Пера по `id`.
    pub(super) pens: HashMap<u32, Pen>,
    /// Стили окон по `id`.
    pub(super) window_styles: HashMap<u32, WindowStyle>,
    /// Позиции окон по `id`.
    pub(super) window_positions: HashMap<u32, WindowPosition>,
    /// Окна из тела по `id`.
    pub(super) windows: HashMap<u32, WindowDefinition>,
    /// Абзацы в порядке файла.
    pub(super) paragraphs: Vec<RawParagraph>,
    /// Сколько элементов стилей (`pen`/`ws`/`wp`) содержали неподдерживаемые атрибуты или
    /// повторяющиеся/нечитаемые определения: каждый считается один раз.
    pub(super) ignored_style_items: usize,
}
