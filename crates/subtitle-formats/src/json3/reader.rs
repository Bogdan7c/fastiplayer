//! Чтение json3 через `serde_json` -> `srv3::model::RawDocument`.
//!
//! Здесь только перекладывание полей: имя поля json3 -> имя атрибута srv3. Разбор значений
//! (цвета, проценты, выравнивание) делают те же `Pen`/`WindowStyle`/`WindowPosition`, что и у
//! srv3, поэтому поддержка и счёт «проигнорированных правил стиля» у двух форматов совпадают.

use serde_json::{Map, Value};

use super::MAX_JSON_NESTING_DEPTH;
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::srv3::model::{RawDocument, RawParagraph, RawSpan, WindowDefinition};
use crate::srv3::pen::Pen;
use crate::srv3::window::{WindowPosition, WindowStyle};

/// Имя атрибута-заглушки для полей json3, которых нет в srv3: разбор srv3 считает любое
/// незнакомое имя неподдерживаемым атрибутом. Нужна, чтобы случайно совпавшее короткое имя
/// (`b`, `i`, ...) не было принято за поддерживаемое.
const UNSUPPORTED_ATTRIBUTE: &str = "unsupported";

/// Поля пера json3 -> атрибуты `<pen>`. Остальные (`fsFontStyle`, `etEdgeType`, `ecEdgeColor`,
/// `ofOffset`, ...) не поддерживаются и считаются. `szPenSize` -> `sz`: проценты, 100 = без изменений.
const PEN_ATTRIBUTES: [(&str, &str); 8] = [
    ("bAttr", "b"),
    ("iAttr", "i"),
    ("uAttr", "u"),
    ("fcForeColor", "fc"),
    ("foForeAlpha", "fo"),
    ("bcBackColor", "bc"),
    ("boBackAlpha", "bo"),
    ("szPenSize", "sz"),
];
/// Поля цвета: в json3 это целое `0xRRGGBB`, в srv3 — строка `#RRGGBB`.
const PEN_COLOR_FIELDS: [&str; 2] = ["fcForeColor", "bcBackColor"];

/// Поля стиля окна json3 -> атрибуты `<ws>` (`pdPrintDir`, `sdScrollDir`, `mhModeHint` — нет).
const WINDOW_STYLE_ATTRIBUTES: [(&str, &str); 1] = [("juJustifCode", "ju")];

/// Поля позиции окна json3 -> атрибуты `<wp>` (`ccCols` не поддерживается, как `cc` в srv3).
const WINDOW_POSITION_ATTRIBUTES: [(&str, &str); 4] = [
    ("apPoint", "ap"),
    ("ahHorPos", "ah"),
    ("avVerPos", "av"),
    ("rcRows", "rc"),
];

/// Читает json3 из UTF-8 текста.
///
/// Порядок проверок: вложенность (`Json3TooComplex`) -> синтаксис (`MalformedJson3`) -> форма
/// документа (`NotJson3`) -> число событий (`TooManyEvents`).
pub(super) fn read_document(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<RawDocument, SubtitleLoadError> {
    // `serde_json` не принимает BOM; декодер текста его мог оставить.
    let text = text.trim_start_matches('\u{feff}');
    if exceeds_nesting_limit(text) {
        return Err(SubtitleLoadError::Json3TooComplex);
    }
    let root: Value = serde_json::from_str(text).map_err(|_| SubtitleLoadError::MalformedJson3)?;
    let events = events_of(&root)?;
    if events.len() > limits.max_events {
        return Err(SubtitleLoadError::TooManyEvents {
            limit: limits.max_events,
        });
    }
    let mut document = RawDocument::default();
    read_pens(&root, &mut document);
    read_window_styles(&root, &mut document);
    read_window_positions(&root, &mut document);
    let mut paragraph_count = 0;
    for event in events {
        read_event(event, &mut paragraph_count, &mut document);
    }
    Ok(document)
}

/// Превышена ли вложенность `{`/`[` (скобки внутри строк не считаются).
fn exceeds_nesting_limit(text: &str) -> bool {
    let mut depth = 0_usize;
    let mut inside_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if inside_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => inside_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => inside_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_NESTING_DEPTH {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// Массив событий; корень обязан быть объектом с `events` (массив) либо с `wireMagic`.
fn events_of(root: &Value) -> Result<&[Value], SubtitleLoadError> {
    let Value::Object(fields) = root else {
        return Err(SubtitleLoadError::NotJson3);
    };
    match fields.get("events") {
        Some(Value::Array(events)) => Ok(events),
        // `events` другого типа — это не json3.
        Some(_) => Err(SubtitleLoadError::NotJson3),
        // Заголовок без событий — корректный пустой документ.
        None if fields.contains_key("wireMagic") => Ok(&[]),
        None => Err(SubtitleLoadError::NotJson3),
    }
}

/// Таблица стилей корня (`pens`, `wsWinStyles`, `wpWinPositions`): элементы идут подряд,
/// номер в массиве = `id`, на который ссылаются события (как `id` у `<pen>`/`<ws>`/`<wp>`).
fn table_of<'root>(root: &'root Value, name: &str) -> &'root [Value] {
    match root.get(name) {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

fn read_pens(root: &Value, document: &mut RawDocument) {
    for (index, item) in table_of(root, "pens").iter().enumerate() {
        let (Some(id), Some(fields)) = (id_from_index(index), item.as_object()) else {
            document.ignored_style_items += 1;
            continue;
        };
        let attributes = attributes_of(fields, |name| lookup(&PEN_ATTRIBUTES, name));
        let parsed = Pen::from_attributes(borrowed_pairs(&attributes));
        if parsed.has_ignored_attributes {
            document.ignored_style_items += 1;
        }
        // При повторе id действует первое определение (как в srv3).
        document.pens.entry(id).or_insert(parsed.pen);
    }
}

fn read_window_styles(root: &Value, document: &mut RawDocument) {
    for (index, item) in table_of(root, "wsWinStyles").iter().enumerate() {
        let (Some(id), Some(fields)) = (id_from_index(index), item.as_object()) else {
            document.ignored_style_items += 1;
            continue;
        };
        let attributes = attributes_of(fields, |name| lookup(&WINDOW_STYLE_ATTRIBUTES, name));
        let parsed = WindowStyle::from_attributes(borrowed_pairs(&attributes));
        if parsed.has_ignored_attributes {
            document.ignored_style_items += 1;
        }
        document.window_styles.entry(id).or_insert(parsed.style);
    }
}

fn read_window_positions(root: &Value, document: &mut RawDocument) {
    for (index, item) in table_of(root, "wpWinPositions").iter().enumerate() {
        let (Some(id), Some(fields)) = (id_from_index(index), item.as_object()) else {
            document.ignored_style_items += 1;
            continue;
        };
        let attributes = attributes_of(fields, |name| lookup(&WINDOW_POSITION_ATTRIBUTES, name));
        let parsed = WindowPosition::from_attributes(borrowed_pairs(&attributes));
        if parsed.has_ignored_attributes {
            document.ignored_style_items += 1;
        }
        document
            .window_positions
            .entry(id)
            .or_insert(parsed.position);
    }
}

/// Одно событие: с `segs` — абзац, с `id` без `segs` — определение окна, прочее — пустой абзац
/// (он попадёт в отчёт как пропущенная реплика, а не исчезнет молча).
fn read_event(event: &Value, paragraph_count: &mut usize, document: &mut RawDocument) {
    let empty = Map::new();
    let fields = event.as_object().unwrap_or(&empty);
    if !fields.contains_key("segs")
        && let Some(window_id) = number_field::<u32>(fields, "id")
    {
        document
            .windows
            .entry(window_id)
            .or_insert(WindowDefinition {
                position_id: number_field(fields, "wpWinPosId"),
                style_id: number_field(fields, "wsWinStyleId"),
            });
        return;
    }
    let ordinal = *paragraph_count;
    *paragraph_count += 1;
    document.paragraphs.push(RawParagraph {
        ordinal,
        start_ms: number_field(fields, "tStartMs"),
        duration_ms: number_field(fields, "dDurationMs"),
        window_id: number_field(fields, "wWinId"),
        position_id: number_field(fields, "wpWinPosId"),
        style_id: number_field(fields, "wsWinStyleId"),
        pen_id: number_field(fields, "pPenId"),
        is_append: number_field::<u8>(fields, "aAppend") == Some(1),
        spans: spans_of(fields),
    });
}

/// Слова абзаца из `segs`; нечитаемый элемент даёт пустое слово (текст пропадёт, остальное нет).
fn spans_of(event_fields: &Map<String, Value>) -> Vec<RawSpan> {
    let Some(Value::Array(segments)) = event_fields.get("segs") else {
        return Vec::new();
    };
    let empty = Map::new();
    segments
        .iter()
        .map(|segment| {
            let fields = segment.as_object().unwrap_or(&empty);
            RawSpan {
                offset_ms: number_field(fields, "tOffsetMs"),
                pen_id: number_field(fields, "pPenId"),
                text: fields
                    .get("utf8")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                // В json3 нет «текста прямо в абзаце»: каждый кусок — `seg`.
                is_direct_text: false,
            }
        })
        .collect()
}

/// Числовое поле; отсутствующее, нечитаемое или вне диапазона типа = `None` (как в srv3).
fn number_field<T: TryFrom<i64>>(fields: &Map<String, Value>, name: &str) -> Option<T> {
    T::try_from(fields.get(name)?.as_i64()?).ok()
}

fn id_from_index(index: usize) -> Option<u32> {
    u32::try_from(index).ok()
}

fn lookup(table: &[(&str, &'static str)], json_name: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(name, _)| *name == json_name)
        .map(|(_, srv3_name)| *srv3_name)
}

/// Поля json3 -> пары «имя атрибута srv3 — значение строкой». Неизвестные поля получают
/// имя-заглушку, цвета переводятся `0xRRGGBB` -> `#RRGGBB`.
fn attributes_of(
    fields: &Map<String, Value>,
    srv3_name_of: impl Fn(&str) -> Option<&'static str>,
) -> Vec<(&'static str, String)> {
    fields
        .iter()
        .map(|(name, value)| {
            let srv3_name = srv3_name_of(name).unwrap_or(UNSUPPORTED_ATTRIBUTE);
            let text = if PEN_COLOR_FIELDS.contains(&name.as_str()) {
                color_text(value)
            } else {
                scalar_text(value)
            };
            (srv3_name, text)
        })
        .collect()
}

fn borrowed_pairs<'a>(
    attributes: &'a [(&'static str, String)],
) -> impl Iterator<Item = (&'a str, &'a str)> {
    attributes
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
}

/// Целое `0xRRGGBB` -> `#RRGGBB`; всё остальное -> заведомо нечитаемая строка.
fn color_text(value: &Value) -> String {
    match value.as_u64() {
        Some(rgb) if rgb <= 0xFF_FFFF => format!("#{rgb:06X}"),
        _ => String::from("invalid"),
    }
}

/// Число -> его запись; прочее (строка, bool, объект) -> заведомо нечитаемая строка.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::Number(number) => number.to_string(),
        _ => String::from("invalid"),
    }
}
