// Порт поведения libass 0.17.5 (ISC): ass_parse.c (ass_get_next_char), ass_render.c (parse_events).
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

//! Текст реплики ASS -> последовательность [`AssTextItem`] (порт `parse_events` и
//! `ass_get_next_char`: что считается блоком тегов, а что — обычным символом).
//!
//! Правила libass:
//! - `{` открывает блок только если дальше есть `}` (ищется первая — вложенность не
//!   поддерживается); `{` без пары и одиночная `}` — обычные символы;
//! - `\N` — жёсткий перенос, `\n` — мягкий (зависит от режима переноса на момент чтения,
//!   решает растеризатор), `\h` — неразрывный пробел, `\{` и `\}` — литеральные скобки;
//!   любой другой `\x` остаётся обычным текстом вместе с обратной косой чертой;
//! - табуляция превращается в пробел;
//! - текст обрывается на первом NUL (C-строка).

use subtitle_core::AssTextItem;

use super::report::AssTagStats;
use super::scan::c_string_prefix;
use super::tags::{TagBudget, parse_override_block};

/// Неразрывный пробел (`\h`).
const NO_BREAK_SPACE: char = '\u{a0}';

/// Разбирает поле `Text` в элементы.
///
/// `max_tags_per_event` ограничивает число тегов в реплике; превышение считается в `stats`.
pub(super) fn parse_event_text(
    raw_text: &str,
    max_tags_per_event: usize,
    stats: &mut AssTagStats,
) -> Vec<AssTextItem> {
    let text = c_string_prefix(raw_text);
    let mut budget = TagBudget::new(max_tags_per_event);
    let mut builder = ItemBuilder::default();
    let mut pos = 0usize;
    // Позиция последней `}`: если текущая позиция уже правее, ни один `{` дальше не откроет
    // блок, и повторный поиск `}` (O(n) на каждый `{`) не нужен — иначе текст из многих `{`
    // без `}` разбирался бы за O(n²). Семантика libass (`strchr` на каждый блок) сохранена.
    let last_close = text.rfind('}');
    while let Some(rest) = text.get(pos..).filter(|rest| !rest.is_empty()) {
        if let Some(block_len) = block_length(rest, pos, last_close) {
            // `{` + содержимое + `}`: содержимое — всё между скобками.
            let content = rest.get(1..block_len - 1).unwrap_or_default();
            builder.push_overrides(parse_override_block(content, &mut budget, stats));
            pos += block_len;
            continue;
        }
        let consumed = match next_text_unit(rest) {
            TextUnit::Char(character, len) => {
                builder.push_char(character);
                len
            }
            TextUnit::HardBreak => {
                builder.push_item(AssTextItem::HardBreak);
                2
            }
            TextUnit::SoftBreak => {
                builder.push_item(AssTextItem::SoftBreak);
                2
            }
        };
        pos += consumed;
    }
    builder.finish()
}

/// Длина блока `{...}` вместе со скобками, если `rest` начинается с `{` и дальше есть `}`.
///
/// `pos` — смещение `rest` в полном тексте, `last_close` — смещение последней `}` в нём.
fn block_length(rest: &str, pos: usize, last_close: Option<usize>) -> Option<usize> {
    if !rest.starts_with('{') {
        return None;
    }
    // Закрывающих скобок правее нет — `{` обычный символ (без сканирования хвоста).
    if last_close.is_none_or(|close| close < pos) {
        return None;
    }
    rest.find('}').map(|close| close + 1)
}

/// Следующая единица обычного текста.
enum TextUnit {
    /// Символ и сколько байт он занял в исходной строке.
    Char(char, usize),
    HardBreak,
    SoftBreak,
}

/// `ass_get_next_char`: табуляция, escape-последовательности или обычный символ.
fn next_text_unit(rest: &str) -> TextUnit {
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return TextUnit::Char(' ', 0);
    };
    if first == '\t' {
        return TextUnit::Char(' ', 1);
    }
    if first == '\\' {
        match chars.next() {
            Some('N') => return TextUnit::HardBreak,
            Some('n') => return TextUnit::SoftBreak,
            Some('h') => return TextUnit::Char(NO_BREAK_SPACE, 2),
            Some('{') => return TextUnit::Char('{', 2),
            Some('}') => return TextUnit::Char('}', 2),
            _ => {}
        }
    }
    TextUnit::Char(first, first.len_utf8())
}

/// Накопитель: склеивает подряд идущие символы в один `Text`, пустые блоки не сохраняет.
#[derive(Default)]
struct ItemBuilder {
    items: Vec<AssTextItem>,
    pending_text: String,
}

impl ItemBuilder {
    fn push_char(&mut self, character: char) {
        self.pending_text.push(character);
    }

    fn flush_text(&mut self) {
        if !self.pending_text.is_empty() {
            let text = std::mem::take(&mut self.pending_text);
            self.items.push(AssTextItem::Text(text));
        }
    }

    fn push_item(&mut self, item: AssTextItem) {
        self.flush_text();
        self.items.push(item);
    }

    fn push_overrides(&mut self, overrides: Vec<subtitle_core::AssOverride>) {
        if !overrides.is_empty() {
            self.push_item(AssTextItem::Overrides(overrides));
        }
    }

    fn finish(mut self) -> Vec<AssTextItem> {
        self.flush_text();
        self.items
    }
}
