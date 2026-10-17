//! ASS-пакет Matroska с полным разбором: стиль, отступы, теги (слой 1).
//!
//! Работает вместе с заголовком дорожки (`parse_ass_script`): раскладка полей и список стилей
//! берутся из сценария. Старый путь без заголовка — [`super::EmbeddedCueFormat::AssEventLine`]
//! (только текст) — остаётся запасным, когда у дорожки нет `codec_private`.
//!
//! Дедупликация по `ReadOrder` (MKV повторяет события в блоках) — задача вызывающего кода:
//! здесь `read_order` лишь возвращается.

use std::sync::Arc;

use subtitle_core::{AssCue, AssScript, SubtitleLayer};

use super::text_lines::{check_char_limit, decode_utf8_lossy};
use super::{EmbeddedCueError, EmbeddedCueLimits, EmbeddedCueMalformation};
use crate::ass::chunk::{BrokenChunk, parse_chunk};
use crate::ass::file::has_visible_text;
use crate::ass::{AssParseLimits, AssTagStats};

/// Разобранное событие ASS из пакета (время задаёт контейнер).
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedAssEvent {
    /// Поле `ReadOrder` (для дедупликации повторов).
    pub read_order: i32,
    /// Слой (`Layer`); в SSA на этом месте `Marked`, нечисловое даёт 0.
    pub layer: SubtitleLayer,
    pub cue: AssCue,
    /// Часть байтов UTF-8 была заменена на U+FFFD.
    pub had_replacement_characters: bool,
}

impl EmbeddedAssEvent {
    /// Рисует ли событие хоть что-то (блоки одних тегов и пустой текст — нет).
    /// libass такие события тоже принимает, но показывать там нечего.
    pub fn has_visible_text(&self) -> bool {
        has_visible_text(&self.cue.text)
    }
}

/// Разбирает пакет `S_TEXT/ASS` / `S_TEXT/SSA` по сценарию дорожки.
///
/// Ошибки: пакет больше лимита, текст длиннее лимита, нарушена раскладка полей
/// ([`EmbeddedCueMalformation::AssMissingEventField`]) — вызывающий пропускает пакет и
/// считает его. Счётчики тегов копятся в `tag_stats` (один накопитель на дорожку).
pub fn parse_embedded_ass_event(
    payload: &[u8],
    script: &Arc<AssScript>,
    limits: &EmbeddedCueLimits,
    ass_limits: &AssParseLimits,
    tag_stats: &mut AssTagStats,
) -> Result<EmbeddedAssEvent, EmbeddedCueError> {
    // Размер проверяем до декодирования: враждебный пакет не должен выделять память.
    if payload.len() > limits.max_payload_bytes {
        return Err(EmbeddedCueError::PayloadTooLarge {
            actual_bytes: payload.len(),
            limit_bytes: limits.max_payload_bytes,
        });
    }
    let decoded = decode_utf8_lossy(payload);
    check_char_limit(&decoded.text, limits)?;
    let parsed =
        parse_chunk(&decoded.text, script, ass_limits, tag_stats).map_err(|BrokenChunk| {
            EmbeddedCueError::Malformed(EmbeddedCueMalformation::AssMissingEventField)
        })?;
    Ok(EmbeddedAssEvent {
        read_order: parsed.read_order,
        layer: SubtitleLayer::new(parsed.layer),
        cue: parsed.cue,
        had_replacement_characters: decoded.had_replacement_characters,
    })
}
