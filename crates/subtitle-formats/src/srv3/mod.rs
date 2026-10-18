//! srv3 — XML-субтитры YouTube (`<timedtext format="3">`).
//!
//! Конвейер: XML через `bounded-xml-reader` ([`reader`]) -> сырая модель ([`model`]) ->
//! абзацы с проверенным временем ([`timing`]) -> строки слов ([`lines`]) -> события модели
//! ([`rolling`] для окон автосубтитров, прямой выпуск для одиночных абзацев).
//!
//! Что поддержано: пера (цвет, прозрачность, жирный/курсив/подчёркивание, фон), позиция окна
//! (`ap/ah/av`) и выравнивание (`ju`), построчное появление слов (`<s t>`), бегущие окна
//! автосубтитров (`rc`). Остальное (обводка пера, размер, ruby, колонки) считается в отчёте как
//! проигнорированные правила стиля. Подробности — `docs/subtitles/compatibility.md`.
//!
//! Файл без атрибута `format` принимается (часть инструментов его не пишет); `format` не равный
//! `3` (srv1/srv2 — другая структура) — фатальная ошибка `NotSrv3`.

mod lines;
mod model;
mod pen;
mod reader;
mod rolling;
mod timing;
mod window;

use std::collections::BTreeMap;

use subtitle_core::SubtitleEvent;

use self::model::RawDocument;
use self::rolling::{ScheduledParagraph, WindowGroup, emit_window_events};
use self::timing::{TimedParagraph, timed_paragraphs};
use self::window::{UNLIMITED_ROWS_CAP, placement_for};
use crate::document::ParsedSubtitleDocument;
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::report::SubtitleParseReport;

/// Корень srv3 в начале текста: после BOM, пробелов, объявления `<?xml ?>` и комментариев.
///
/// Нужна для определения формата по содержимому: `<timedtext` и за ним пробел, `>` или `/`.
pub(crate) fn has_srv3_signature(text: &str) -> bool {
    let mut rest = text.trim_start_matches('\u{feff}').trim_start();
    // Объявление и комментарии перед корнем; их число ограничено — цикл не бесконечен.
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
    rest.strip_prefix("<timedtext").is_some_and(|after| {
        after
            .chars()
            .next()
            .is_some_and(|next| next.is_whitespace() || next == '>' || next == '/')
    })
}

/// Разбирает декодированный текст srv3.
pub(crate) fn parse_srv3(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<ParsedSubtitleDocument, SubtitleLoadError> {
    let document = reader::read_document(text, limits)?;
    let mut report = SubtitleParseReport::new(limits.max_reported_issues);
    report.record_ignored_style_rules(document.ignored_style_items);
    let paragraphs = timed_paragraphs(&document, &mut report);

    let mut events: Vec<SubtitleEvent> = Vec::new();
    // Окна (`w`) собираются вместе; абзацы вне окон показываются каждый сам по себе.
    let mut windows: BTreeMap<u32, Vec<ScheduledParagraph<'_>>> = BTreeMap::new();
    for timed in paragraphs {
        let scheduled = schedule(timed, &document);
        match scheduled.timed.raw.window_id {
            Some(window_id) => windows.entry(window_id).or_default().push(scheduled),
            // Одиночный абзац — окно из одного абзаца без ограничения строк: одно событие на
            // весь показ, слова появляются по `reveal_at`.
            None => emit_window_events(
                &WindowGroup {
                    paragraphs: vec![scheduled],
                    max_rows: usize::MAX,
                },
                limits.max_events,
                &mut events,
            )?,
        }
    }
    for (window_id, mut window_paragraphs) in windows {
        window_paragraphs
            .sort_by_key(|paragraph| (paragraph.timed.start_ms, paragraph.timed.raw.ordinal));
        let max_rows = window_row_limit(&document, window_id);
        let group = WindowGroup {
            paragraphs: window_paragraphs,
            max_rows,
        };
        emit_window_events(&group, limits.max_events, &mut events)?;
    }
    Ok(ParsedSubtitleDocument::new(events, Vec::new(), report))
}

/// Размещение абзаца: собственные `wp/ws` абзаца перекрывают определённые у окна.
fn schedule<'document>(
    timed: TimedParagraph<'document>,
    document: &RawDocument,
) -> ScheduledParagraph<'document> {
    let window = timed
        .raw
        .window_id
        .and_then(|window_id| document.windows.get(&window_id));
    let position_id = timed
        .raw
        .position_id
        .or(window.and_then(|window| window.position_id));
    let style_id = timed
        .raw
        .style_id
        .or(window.and_then(|window| window.style_id));
    let placement = placement_for(
        position_id.and_then(|id| document.window_positions.get(&id)),
        style_id.and_then(|id| document.window_styles.get(&id)),
    );
    ScheduledParagraph { timed, placement }
}

/// Сколько строк вмещает окно: `rc` позиции окна, иначе потолок «без ограничения».
fn window_row_limit(document: &RawDocument, window_id: u32) -> usize {
    document
        .windows
        .get(&window_id)
        .and_then(|window| window.position_id)
        .and_then(|position_id| document.window_positions.get(&position_id))
        .and_then(|position| position.rows)
        .map_or(UNLIMITED_ROWS_CAP, |rows| rows.min(UNLIMITED_ROWS_CAP))
}
