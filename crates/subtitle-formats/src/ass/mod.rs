//! Разбор ASS/SSA (слой 1): заголовок, стили, события, теги слоя 1.
//!
//! Модуль — порт поведения libass 0.17.5 (`ass.c`, `ass_parse.c`, `ass_utils.c`): правила
//! терпимости к мусору повторены, потому что реальные файлы пишут десятки редакторов.
//! Результат — нейтральная модель из `subtitle-core` (`AssScript`, `AssCue`).
//!
//! Три входа:
//! - [`parse_ass_script`]: заголовок дорожки (`codec_private` MKV);
//! - файл `.ass/.ssa` целиком: через `load_text_subtitle` (`TextSubtitleFormat::Ass`);
//! - событие из пакета MKV: [`crate::parse_embedded_ass_event`].
//!
//! Устройство файлов:
//! - `scan` — числа и токены «как в C/libass»;
//! - `lines` — разбиение на строки;
//! - `script_builder` + `style_line` + `event_fields` — секции и их строки;
//! - `tags` + `text` — блоки `{...}` и текст реплики;
//! - `chunk` — раскладка полей пакета; `file` — файл целиком;
//! - `report`, `limits` — счётчики, лимиты, ошибки.

pub(crate) mod chunk;
mod event_fields;
pub(crate) mod file;
mod hard_overrides;
mod limits;
mod lines;
mod report;
mod scan;
mod script_builder;
mod script_parse;
mod style_line;
mod tags;
mod text;

#[cfg(test)]
mod script_builder_tests;
#[cfg(test)]
mod tests;

pub use limits::{AssParseError, AssParseLimits};
pub use report::{AssHeaderReport, AssParseReport, AssTagStats};
pub use script_parse::parse_ass_script;

/// Начинается ли текст как сценарий ASS/SSA: первая непустая строка — `[Script Info]`
/// (без учёта регистра; BOM и пробелы допускаются).
pub(crate) fn has_ass_signature(text: &str) -> bool {
    const SIGNATURE: &str = "[Script Info]";
    let first_line = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty());
    first_line.is_some_and(|line| {
        line.as_bytes()
            .get(..SIGNATURE.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(SIGNATURE.as_bytes()))
    })
}
