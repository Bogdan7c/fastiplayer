//! Общие помощники разбора пакетов: декодирование, лимиты, сборка итогового исхода.

use subtitle_core::{CuePlacement, StyledText, SubtitleLayer, SubtitleTextCue, TextSpan};

use super::{EmbeddedCue, EmbeddedCueError, EmbeddedCueLimits, EmbeddedCueOutcome};

/// Текст пакета и признак того, что при декодировании были замены.
pub(super) struct DecodedPayload {
    pub(super) text: String,
    pub(super) had_replacement_characters: bool,
}

/// UTF-8 с заменой нарушений на U+FFFD; факт замены не теряется.
pub(super) fn decode_utf8_lossy(bytes: &[u8]) -> DecodedPayload {
    match String::from_utf8_lossy(bytes) {
        std::borrow::Cow::Borrowed(valid) => DecodedPayload {
            text: valid.to_owned(),
            had_replacement_characters: false,
        },
        std::borrow::Cow::Owned(repaired) => DecodedPayload {
            text: repaired,
            had_replacement_characters: true,
        },
    }
}

/// Проверяет лимит символов декодированной нагрузки.
pub(super) fn check_char_limit(
    text: &str,
    limits: &EmbeddedCueLimits,
) -> Result<(), EmbeddedCueError> {
    let actual_chars = text.chars().count();
    if actual_chars > limits.max_chars {
        return Err(EmbeddedCueError::TextTooLong {
            actual_chars,
            limit_chars: limits.max_chars,
        });
    }
    Ok(())
}

/// CRLF и одиночный CR приводятся к LF.
pub(super) fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Убирает пустые края: переводы строки и пробельные спаны в начале и конце.
pub(super) fn trim_blank_edges(mut spans: Vec<TextSpan>) -> StyledText {
    let is_blank = |span: &TextSpan| match &span.content {
        subtitle_core::SpanContent::LineBreak => true,
        subtitle_core::SpanContent::Text(text) => text.trim().is_empty(),
    };
    while spans.last().is_some_and(is_blank) {
        spans.pop();
    }
    let leading_blank = spans.iter().take_while(|span| is_blank(span)).count();
    spans.drain(..leading_blank);
    StyledText::new(spans)
}

/// Собирает исход: текст без видимых символов — `Empty`, иначе `Cue`.
pub(super) fn finish_cue(
    text: StyledText,
    placement: CuePlacement,
    layer: SubtitleLayer,
    had_replacement_characters: bool,
    had_invalid_styling: bool,
) -> EmbeddedCueOutcome {
    if text.plain_text().trim().is_empty() {
        return EmbeddedCueOutcome::Empty;
    }
    EmbeddedCueOutcome::Cue(EmbeddedCue {
        cue: SubtitleTextCue { text, placement },
        layer,
        had_replacement_characters,
        had_invalid_styling,
    })
}
