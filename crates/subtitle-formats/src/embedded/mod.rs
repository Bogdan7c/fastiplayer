//! Разбор ОДНОЙ реплики из пакета контейнера (Matroska, MP4): без таймингов в полезной нагрузке.
//!
//! В отличие от [`crate::load_text_subtitle`] (файл целиком, тайминги внутри текста), пакет
//! контейнера несёт только содержимое реплики: начало и длительность даёт демультиплексор,
//! поэтому вызывающий код сам собирает `SubtitleEvent` из результата [`parse_embedded_cue`].
//!
//! Правила устойчивости те же, что у файлов: плохой пакет — типизированная ошибка
//! ([`EmbeddedCueError`]), вызывающий пропускает пакет и считает его; воспроизведение не
//! останавливается. Пакет без видимого текста — НЕ ошибка, а исход [`EmbeddedCueOutcome::Empty`]
//! (в mov_text так кодируется «пустой промежуток»).
//!
//! Кодировка: UTF-8 с нарушениями декодируется с заменой (U+FFFD) и помечается флагом
//! `had_replacement_characters` — реплика важнее идеальной кодировки. UTF-16 (mov_text) с
//! нарушениями — [`EmbeddedCueError::InvalidEncoding`]: у него нет безопасного «лучшего приближения».

mod ass_event;
mod ass_line;
mod mov_text;
mod text_lines;

#[cfg(test)]
mod tests;

use subtitle_core::{CuePlacement, SubtitleLayer, SubtitleTextCue};

pub use ass_event::{EmbeddedAssEvent, parse_embedded_ass_event};

use crate::srt::parse_srt_markup_text;
use crate::webvtt::parse_embedded_webvtt_cue_text;

/// Формат полезной нагрузки пакета (по идентификатору кодека дорожки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedCueFormat {
    /// Matroska `S_TEXT/UTF8`: UTF-8 текст с той же разметкой, что в файлах SRT.
    SrtText,
    /// Matroska `S_TEXT/WEBVTT`: текст реплики WebVTT (без времени и настроек).
    WebVttCueText,
    /// Matroska `S_TEXT/ASS` и `S_TEXT/SSA`: строка
    /// `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`.
    ///
    /// SSA в Matroska хранится в той же раскладке из 9 полей (libass `ass_process_chunk`
    /// не различает их): в SSA на месте `Layer` лежит историческое `Marked`, и оно, если не
    /// число, просто даёт слой по умолчанию. Теги `{...}` пока вырезаются (стили ASS — сессии 08–10).
    AssEventLine,
    /// MP4 `tx3g` (3GPP TS 26.245): длина + текст + необязательные блоки (`styl`).
    MovText,
}

/// Пределы, защищающие от враждебных пакетов. Числа приходят из конфигурации; `Default` нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedCueLimits {
    /// Максимальный размер пакета в байтах; проверяется ДО декодирования.
    pub max_payload_bytes: usize,
    /// Максимальное число символов (Unicode scalar values) в декодированной нагрузке.
    pub max_chars: usize,
}

impl EmbeddedCueLimits {
    pub const fn new(max_payload_bytes: usize, max_chars: usize) -> Self {
        Self {
            max_payload_bytes,
            max_chars,
        }
    }
}

/// Конкретная причина «битого» пакета.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EmbeddedCueMalformation {
    /// ASS-строка содержит меньше девяти полей.
    #[error("ASS event line has {found} fields, expected {expected}")]
    AssTooFewFields { expected: usize, found: usize },
    /// ASS-пакет не содержит всех полей, которые требует раскладка `Format:` сценария
    /// (или в раскладке нет `Text`).
    #[error("ASS event packet does not match the script's event format")]
    AssMissingEventField,
    /// mov_text: нет двухбайтной длины текста.
    #[error("mov_text sample is shorter than its 2-byte length prefix")]
    MovTextMissingLength,
    /// mov_text: заявленная длина текста больше самого пакета.
    #[error("mov_text text length {declared} exceeds the sample payload {available}")]
    MovTextLengthExceedsPayload { declared: usize, available: usize },
}

/// Ошибки разбора одного пакета. Вызывающий пропускает пакет и увеличивает счётчик.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EmbeddedCueError {
    /// Пакет больше `max_payload_bytes`.
    #[error("subtitle packet is too large ({actual_bytes} bytes, limit {limit_bytes} bytes)")]
    PayloadTooLarge {
        actual_bytes: usize,
        limit_bytes: usize,
    },
    /// Декодированный текст длиннее `max_chars`.
    #[error("subtitle packet text is too long ({actual_chars} chars, limit {limit_chars})")]
    TextTooLong {
        actual_chars: usize,
        limit_chars: usize,
    },
    /// UTF-16 с нарушениями (нечётная длина, одиночный суррогат).
    #[error("subtitle packet text is not valid UTF-16")]
    InvalidEncoding,
    /// Структура пакета нарушена.
    #[error("malformed subtitle packet: {0}")]
    Malformed(EmbeddedCueMalformation),
}

/// Успешно разобранная реплика (без времени: его задаёт контейнер).
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedCue {
    /// Текст со стилями автора и размещением.
    pub cue: SubtitleTextCue,
    /// Слой (ASS `Layer`); для остальных форматов — [`SubtitleLayer::DEFAULT`].
    pub layer: SubtitleLayer,
    /// Часть байтов UTF-8 была заменена на U+FFFD.
    pub had_replacement_characters: bool,
    /// Стилевая часть пакета (mov_text `styl`) повреждена и проигнорирована; текст цел.
    pub had_invalid_styling: bool,
}

/// Исход разбора корректного пакета.
#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddedCueOutcome {
    /// Реплика с видимым текстом.
    Cue(EmbeddedCue),
    /// Пакет корректен, но видимого текста нет (маркер «пустого промежутка», как в mov_text).
    Empty,
}

/// Разбирает полезную нагрузку одного пакета субтитровой дорожки.
pub fn parse_embedded_cue(
    payload: &[u8],
    format: EmbeddedCueFormat,
    limits: &EmbeddedCueLimits,
) -> Result<EmbeddedCueOutcome, EmbeddedCueError> {
    // Размер проверяем до любого декодирования: враждебный пакет не должен выделять память.
    if payload.len() > limits.max_payload_bytes {
        return Err(EmbeddedCueError::PayloadTooLarge {
            actual_bytes: payload.len(),
            limit_bytes: limits.max_payload_bytes,
        });
    }
    match format {
        EmbeddedCueFormat::SrtText => parse_text_payload(payload, limits, |text| {
            let normalized = text_lines::normalize_line_endings(text);
            let trimmed_lines: Vec<&str> = normalized.split('\n').map(str::trim).collect();
            let parsed = parse_srt_markup_text(trimmed_lines.join("\n").trim());
            let placement = CuePlacement {
                anchor: parsed.anchor,
                ..CuePlacement::default()
            };
            (parsed.text, placement)
        }),
        EmbeddedCueFormat::WebVttCueText => parse_text_payload(payload, limits, |text| {
            let normalized = text_lines::normalize_line_endings(text);
            (
                parse_embedded_webvtt_cue_text(&normalized),
                CuePlacement::default(),
            )
        }),
        EmbeddedCueFormat::AssEventLine => ass_line::parse_ass_event_line(payload, limits),
        EmbeddedCueFormat::MovText => mov_text::parse_mov_text_sample(payload, limits),
    }
}

/// Общий путь «UTF-8 текст -> StyledText + размещение» для SRT и WebVTT.
fn parse_text_payload(
    payload: &[u8],
    limits: &EmbeddedCueLimits,
    parse: impl FnOnce(&str) -> (subtitle_core::StyledText, CuePlacement),
) -> Result<EmbeddedCueOutcome, EmbeddedCueError> {
    let decoded = text_lines::decode_utf8_lossy(payload);
    text_lines::check_char_limit(&decoded.text, limits)?;
    let (text, placement) = parse(&decoded.text);
    Ok(text_lines::finish_cue(
        text,
        placement,
        SubtitleLayer::DEFAULT,
        decoded.had_replacement_characters,
        false,
    ))
}
