//! TTML / IMSC 1.1 Text Profile (подмножество) — субтитры DASH и стриминга (`<tt xmlns="http://www.w3.org/ns/ttml">`).
//!
//! Конвейер: XML через `bounded-xml-reader` ([`reader`]) -> сырое дерево ([`model`]) ->
//! стили ([`style`]), регионы ([`layout`]) и параметры времени ([`timing`]) ->
//! обход абзацев с окнами показа ([`paragraphs`]) -> нарезка по моментам смены ([`emit`]) со
//! сборкой текста ([`cue_text`]) и размещением ([`region_placement`]) -> события модели.
//!
//! Что поддержано — в `docs/subtitles/compatibility.md` (раздел «TTML / IMSC»). Всё
//! нераспознанное (стили вне подмножества, `timeBase` кроме `media`, `timeContainer="seq"`,
//! анимация) считается в `SubtitleParseReport::ignored_style_rules` и файл не ломает.

mod color;
mod cue_text;
mod emit;
mod layout;
mod model;
mod paragraphs;
mod reader;
mod region_placement;
mod style;
mod timing;
mod window;

use crate::document::ParsedSubtitleDocument;
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::report::SubtitleParseReport;
use paragraphs::{DocumentContext, collect_paragraphs};
use reader::TTML_NAMESPACE;
use style::StyleBook;
use timing::TimeParameters;
pub use window::TtmlSampleWindow;

/// Корень TTML в начале текста: после BOM, пробелов, объявления `<?xml ?>` и комментариев.
///
/// Нужна для определения формата по содержимому: корневой элемент с локальным именем `tt`
/// (с префиксом или без), у которого объявлено пространство имён TTML.
pub(crate) fn has_ttml_signature(text: &str) -> bool {
    let mut rest = text.trim_start_matches('\u{feff}').trim_start();
    // Объявление и комментарии перед корнем; их число ограничено длиной текста.
    loop {
        if let Some(after_declaration) = rest.strip_prefix("<?") {
            let Some(end) = after_declaration.find("?>") else {
                return false;
            };
            rest = after_declaration[end + 2..].trim_start();
        } else if let Some(after_comment) = rest.strip_prefix("<!--") {
            let Some(end) = after_comment.find("-->") else {
                return false;
            };
            rest = after_comment[end + 3..].trim_start();
        } else {
            break;
        }
    }
    let Some(tag) = rest.strip_prefix('<') else {
        return false;
    };
    let Some(tag_end) = start_tag_end(tag) else {
        return false;
    };
    let start_tag = &tag[..tag_end];
    let name = start_tag
        .split(|character: char| character.is_whitespace() || character == '/')
        .next()
        .unwrap_or("");
    let local_name = name.rsplit(':').next().unwrap_or("");
    local_name == "tt" && declares_ttml_namespace(start_tag)
}

/// Позиция `>` конца открывающего тега с учётом кавычек в значениях атрибутов.
fn start_tag_end(tag: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (index, character) in tag.char_indices() {
        match (quote, character) {
            (Some(open), current) if current == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

/// В теге есть значение атрибута, равное ровно пространству имён TTML.
fn declares_ttml_namespace(start_tag: &str) -> bool {
    start_tag.contains("xmlns")
        && [
            format!("\"{TTML_NAMESPACE}\""),
            format!("'{TTML_NAMESPACE}'"),
        ]
        .iter()
        .any(|quoted| start_tag.contains(quoted.as_str()))
}

/// Разбирает декодированный текст TTML.
pub(crate) fn parse_ttml(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<ParsedSubtitleDocument, SubtitleLoadError> {
    parse_ttml_in_window(text, limits, None)
}

/// Разбирает TTML; `sample_window` - интервал сэмпла контейнера, если документ из него
/// (см. [`TtmlSampleWindow`]): реплики обрезаются окном, абзацы без конца идут до его конца.
pub(crate) fn parse_ttml_in_window(
    text: &str,
    limits: &SubtitleParseLimits,
    sample_window: Option<TtmlSampleWindow>,
) -> Result<ParsedSubtitleDocument, SubtitleLoadError> {
    let tree = reader::read_tree(text, limits)?;
    let mut report = SubtitleParseReport::new(limits.max_reported_issues);
    let mut ignored_count = tree.unsupported_construct_count;

    let root = tree.element(tree.root).ok_or(SubtitleLoadError::NotTtml)?;
    let parsed_time = TimeParameters::from_root(root);
    ignored_count += parsed_time.ignored_count;
    let styles = StyleBook::from_tree(&tree);
    ignored_count += styles.ignored_count();
    let regions = layout::RegionBook::from_tree(&tree, &styles);
    ignored_count += regions.ignored_count();

    let context = DocumentContext {
        tree: &tree,
        styles: &styles,
        regions: &regions,
        time: parsed_time.parameters,
    };
    let collected = collect_paragraphs(&context, &mut report)?;
    ignored_count += collected.ignored_count;
    let events = emit::emit_events(&collected, sample_window, limits.max_events, &mut report)?;
    report.record_ignored_style_rules(ignored_count);
    // Регионы WebVTT-модели (`SubtitleRegion`) не нужны: прямоугольник региона уже выражен в
    // `CuePlacement` каждой реплики (см. `region_placement`).
    Ok(ParsedSubtitleDocument::new(events, Vec::new(), report))
}
