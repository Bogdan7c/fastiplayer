// Порт поведения libass 0.17.5 (ISC): ass.c (ass_process_chunk).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
// Copyright (C) 2009 Grigori Goronzy <greg@geekmind.org>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Событие ASS из пакета контейнера (порт `ass_process_chunk`): поля
//! `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`.

use std::sync::Arc;

use subtitle_core::{AssCue, AssScript};

use super::event_fields::{BrokenEventLine, parse_event_tail};
use super::hard_overrides::scan_hard_overrides;
use super::limits::AssParseLimits;
use super::report::AssTagStats;
use super::scan::{FieldCursor, TrailingSpaces, atoi, c_string_prefix, parse_int_header};
use super::text::parse_event_text;

/// Сколько первых полей раскладки `Format:` пакет не повторяет (`Layer, Start, End`):
/// вместо них в начале пакета стоят `ReadOrder` и `Layer`, а время даёт контейнер.
const FORMAT_FIELDS_NOT_IN_CHUNK: usize = 3;

/// Разобранный пакет.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedChunk {
    pub(crate) read_order: i32,
    pub(crate) layer: i32,
    pub(crate) cue: AssCue,
}

/// Пакет не соответствует раскладке полей сценария.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrokenChunk;

/// Разбирает текст пакета. Заголовок сценария нужен и для поиска стиля, и для раскладки
/// полей (`script.event_format()`).
pub(crate) fn parse_chunk(
    chunk_text: &str,
    script: &Arc<AssScript>,
    ass_limits: &AssParseLimits,
    tag_stats: &mut AssTagStats,
) -> Result<ParsedChunk, BrokenChunk> {
    let text = c_string_prefix(chunk_text);
    let mut cursor = FieldCursor::new(text);
    let read_order = atoi(cursor.next_token(TrailingSpaces::Keep).ok_or(BrokenChunk)?);
    let layer = parse_int_header(cursor.next_token(TrailingSpaces::Keep).ok_or(BrokenChunk)?);
    let remaining_fields = script
        .event_format()
        .fields()
        .get(FORMAT_FIELDS_NOT_IN_CHUNK..)
        .unwrap_or_default();
    let raw = parse_event_tail(remaining_fields, cursor.remainder())
        .map_err(|BrokenEventLine| BrokenChunk)?;
    let cue = AssCue {
        script: Arc::clone(script),
        style: script.lookup_style(&raw.style_name),
        read_order,
        margins: raw.margins,
        name: raw.name,
        effect: raw.effect,
        hard_overrides: scan_hard_overrides(&raw.text),
        text: parse_event_text(&raw.text, ass_limits.max_override_tags_per_event, tag_stats),
    };
    Ok(ParsedChunk {
        read_order,
        layer,
        cue,
    })
}
