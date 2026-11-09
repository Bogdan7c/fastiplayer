//! json3 — JSON-субтитры YouTube (`fmt=json3`): JSON-запись той же модели, что и srv3.
//!
//! Конвейер: JSON через `serde_json` ([`reader`]) -> та же сырая модель `srv3::model::RawDocument`
//! -> общий конвейер srv3 (`srv3::build_parsed_document`: время, строки, бегущие окна, размещение).
//! Своих правил показа у json3 нет: два формата дают одинаковые события для одинакового смысла
//! (это закреплено тестом эквивалентности в `tests/json3_loading.rs`).
//!
//! Соответствие полей srv3 (имена атрибутов — в `reader::*_ATTRIBUTES`): `events[]` с `segs` —
//! абзацы `<p>`, события с `id` и без `segs` — окна `<w>`, `segs[]` — слова `<s>`
//! (`tOffsetMs` = `t`), `pens`/`wsWinStyles`/`wpWinPositions` — `<pen>`/`<ws>`/`<wp>`.
//! Неподдерживаемые поля стилей считаются в отчёте как проигнорированные правила стиля.
//!
//! Защита от враждебного файла: размер входа уже ограничен `max_input_bytes`; вложенность JSON
//! ограничена здесь ([`MAX_JSON_NESTING_DEPTH`]), число событий — `max_events`.

mod reader;
mod signature;

use crate::document::ParsedSubtitleDocument;
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::srv3;

pub(crate) use self::signature::has_json3_signature;

/// Допустимая вложенность JSON: корень > events > событие > segs > seg — 5 уровней, у пера и
/// окон меньше; остальное — запас на расширения YouTube. Это строже встроенного предела
/// `serde_json` (128 уровней, `RecursionLimitExceeded`), поэтому враждебный файл отсекается
/// чистой ошибкой `Json3TooComplex` ещё до построения дерева значений.
pub(crate) const MAX_JSON_NESTING_DEPTH: usize = 16;

/// Разбирает декодированный текст json3.
pub(crate) fn parse_json3(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<ParsedSubtitleDocument, SubtitleLoadError> {
    let document = reader::read_document(text, limits)?;
    srv3::build_parsed_document(&document, limits)
}
