//! Разбор заголовка ASS из `codec_private` дорожки (MKV) и сборка сценария.

use std::sync::Arc;

use subtitle_core::AssScript;

use super::limits::{AssParseError, AssParseLimits};
use super::lines::AssLines;
use super::report::AssHeaderReport;
use super::scan::c_string_prefix;
use super::script_builder::{LineScope, ScriptBuilder};

/// Разбирает заголовок дорожки: `[Script Info]`, `[V4+ Styles]` / `[V4 Styles]` и
/// `Format:` секции `[Events]`; всё остальное (`[Fonts]`, события) игнорируется.
///
/// Размер проверяется ДО разбора. Байты, не являющиеся UTF-8, декодируются с заменой
/// (U+FFFD) — флаг попадает в отчёт: заголовок с испорченным именем стиля лучше, чем
/// дорожка без стилей.
pub fn parse_ass_script(
    header_bytes: &[u8],
    limits: &AssParseLimits,
) -> Result<(Arc<AssScript>, AssHeaderReport), AssParseError> {
    if header_bytes.len() > limits.max_header_bytes {
        return Err(AssParseError::HeaderTooLarge {
            actual_bytes: header_bytes.len(),
            limit_bytes: limits.max_header_bytes,
        });
    }
    let decoded = String::from_utf8_lossy(header_bytes);
    let had_replacement_characters = matches!(decoded, std::borrow::Cow::Owned(_));
    let mut builder = ScriptBuilder::new(limits, LineScope::HeaderOnly);
    for (_, line) in AssLines::new(c_string_prefix(&decoded)) {
        builder.feed_line(line);
    }
    let report = AssHeaderReport {
        skipped_styles: builder.skipped_styles(),
        had_replacement_characters,
    };
    Ok((Arc::new(builder.finish()), report))
}
