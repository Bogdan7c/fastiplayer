// Порт из libass 0.17.5: ass_shaper.c (части ass_shaper_shape про уровни BiDi и
// ass_shaper_reorder), на `unicode-bidi` вместо FriBidi.
//
// Copyright (C) 2011 Grigori Goronzy <greg@chown.ath.cx>
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

//! Двунаправленный текст: уровни вложенности и порядок символов на строке.
//!
//! ## Что делает libass
//! 1. Уровни считаются абзац за абзацем (`fribidi_get_par_embedding_levels`). Абзац кончается
//!    на разделителе абзацев (FriBidi `BS`: `\n` и т.п.). При базовом направлении «слева
//!    направо» абзац дополнительно рвётся на границе стиля (`starts_new_run`): каждый участок
//!    единого стиля — отдельный абзац. При автоопределении («whole text layout») границы
//!    стиля игнорируются.
//! 2. После переноса строк порядок на экране считается построчно (`fribidi_reorder_line`):
//!    правило L1 (хвостовые пробелы и разделители возвращаются на базовый уровень) и L2
//!    (разворот уровней). Строка рвётся также на разделителях абзацев и на `starts_new_run`.
//!
//! ## Отличия от FriBidi
//! `unicode-bidi` реализует более новую редакцию UBA (парные скобки BD16, изоляты), а libass
//! вызывает FriBidi без разбора скобок. На обычных субтитрах результат совпадает; различие
//! возможно только для смешанного по направлению текста со скобками.

use unicode_bidi::{BidiClass, BidiInfo, Level, bidi_class};

use crate::event_style::BaseDirection;

/// Максимальный уровень, который мы передаём в `unicode-bidi` (предел UBA — 125).
const MAX_BIDI_LEVEL: u8 = 125;

/// Уровни BiDi для каждого символа реплики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BidiLevels {
    /// Уровень вложенности символа (libass: `emblevels`).
    pub(crate) embedding: Vec<u8>,
    /// Уровень абзаца, которому принадлежит символ (0 — слева направо).
    pub(crate) paragraph: Vec<u8>,
}

/// Строка из `symbols[range]` как `String` и смещения символов в ней.
fn segment_text(symbols: &[char]) -> (String, Vec<usize>) {
    let mut text = String::with_capacity(symbols.len());
    let mut offsets = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        offsets.push(text.len());
        text.push(*symbol);
    }
    (text, offsets)
}

/// Уровни одного куска текста; возвращает пары `(embedding, paragraph)` на каждый символ.
fn levels_for_segment(symbols: &[char], default_level: Option<Level>) -> Vec<(u8, u8)> {
    let (text, offsets) = segment_text(symbols);
    let info = BidiInfo::new(&text, default_level);
    offsets
        .iter()
        .map(|offset| {
            let embedding = info.levels.get(*offset).map_or(0, Level::number);
            let paragraph = info
                .paragraphs
                .iter()
                .find(|paragraph| paragraph.range.contains(offset))
                .map_or(0, |paragraph| paragraph.level.number());
            (embedding, paragraph)
        })
        .collect()
}

/// Считает уровни BiDi (libass: часть `ass_shaper_shape`).
///
/// `new_run_starts[i]` — перед символом `i` начался новый участок единого стиля. Для
/// [`BaseDirection::LeftToRight`] такие места делят абзацы; для [`BaseDirection::Auto`]
/// игнорируются.
pub(crate) fn resolve_levels(
    symbols: &[char],
    new_run_starts: &[bool],
    base: BaseDirection,
) -> BidiLevels {
    if symbols.iter().all(char::is_ascii) {
        // В ASCII нет ни сильных RTL-символов, ни явных управляющих BiDi: все уровни равны 0
        // (проверено тестом против полного расчёта), и разбор `unicode-bidi` не нужен.
        return BidiLevels {
            embedding: vec![0; symbols.len()],
            paragraph: vec![0; symbols.len()],
        };
    }
    resolve_levels_in_full(symbols, new_run_starts, base)
}

/// Полный расчёт уровней через `unicode-bidi` без быстрых путей ([`resolve_levels`]).
fn resolve_levels_in_full(
    symbols: &[char],
    new_run_starts: &[bool],
    base: BaseDirection,
) -> BidiLevels {
    let mut embedding = Vec::with_capacity(symbols.len());
    let mut paragraph = Vec::with_capacity(symbols.len());
    let split_by_style = base == BaseDirection::LeftToRight;
    let default_level = match base {
        BaseDirection::LeftToRight => Some(Level::ltr()),
        BaseDirection::Auto => None,
    };
    let mut segment_start = 0;
    for index in 0..symbols.len() {
        let is_last = index + 1 == symbols.len();
        let ends_paragraph = symbols
            .get(index)
            .is_some_and(|symbol| bidi_class(*symbol) == BidiClass::B);
        let next_starts_run =
            split_by_style && new_run_starts.get(index + 1).copied().unwrap_or(false);
        if is_last || ends_paragraph || next_starts_run {
            let segment = symbols.get(segment_start..=index).unwrap_or(&[]);
            for (embedding_level, paragraph_level) in levels_for_segment(segment, default_level) {
                embedding.push(embedding_level);
                paragraph.push(paragraph_level);
            }
            segment_start = index + 1;
        }
    }
    BidiLevels {
        embedding,
        paragraph,
    }
}

/// Символ — разделитель абзацев (FriBidi `BS`: перевод строки и подобные).
pub(crate) fn is_paragraph_separator(symbol: char) -> bool {
    bidi_class(symbol) == BidiClass::B
}

/// Символ — пробельный/форматирующий (FriBidi: `FRIBIDI_IS_EXPLICIT_OR_ISOLATE_OR_BN_OR_WS`).
fn is_whitespace_like(class: BidiClass) -> bool {
    matches!(
        class,
        BidiClass::WS
            | BidiClass::BN
            | BidiClass::LRE
            | BidiClass::RLE
            | BidiClass::LRO
            | BidiClass::RLO
            | BidiClass::PDF
            | BidiClass::LRI
            | BidiClass::RLI
            | BidiClass::FSI
            | BidiClass::PDI
    )
}

/// Порядок отображения символов одной строки (libass: `fribidi_reorder_line`).
///
/// `levels` и `symbols` — одной длины, в логическом порядке; `base_level` — уровень абзаца.
/// Результат: для каждой визуальной позиции — индекс символа в `symbols` (то есть
/// `visual[v]` = логический индекс символа, который окажется на месте `v`).
pub(crate) fn visual_order(symbols: &[char], levels: &[u8], base_level: u8) -> Vec<usize> {
    if base_level == 0 && levels.iter().all(|level| *level == 0) {
        // Все уровни нулевые: разворачивать нечего, порядок логический (L1 уровни не повышает).
        return (0..symbols.len()).collect();
    }
    visual_order_in_full(symbols, levels, base_level)
}

/// Порядок отображения через `unicode-bidi` без быстрых путей ([`visual_order`]).
fn visual_order_in_full(symbols: &[char], levels: &[u8], base_level: u8) -> Vec<usize> {
    let classes: Vec<BidiClass> = symbols.iter().map(|symbol| bidi_class(*symbol)).collect();
    let mut adjusted: Vec<u8> = levels.to_vec();
    // L1.1 + L1.2: разделители абзацев и сегментов получают базовый уровень вместе с
    // пробелами перед ними; пробелы в конце строки — тоже.
    let mut preceding_whitespace_is_reset = true;
    for index in (0..classes.len()).rev() {
        let class = classes.get(index).copied().unwrap_or(BidiClass::L);
        if matches!(class, BidiClass::B | BidiClass::S) {
            preceding_whitespace_is_reset = true;
            set_level(&mut adjusted, index, base_level);
        } else if preceding_whitespace_is_reset && is_whitespace_like(class) {
            set_level(&mut adjusted, index, base_level);
        } else {
            preceding_whitespace_is_reset = false;
        }
    }
    let typed_levels: Vec<Level> = adjusted
        .iter()
        .map(|level| Level::new((*level).min(MAX_BIDI_LEVEL)).unwrap_or_else(|_| Level::ltr()))
        .collect();
    BidiInfo::reorder_visual(&typed_levels)
}

/// Записывает уровень, если индекс в пределах массива.
fn set_level(levels: &mut [u8], index: usize, level: u8) {
    if let Some(slot) = levels.get_mut(index) {
        *slot = level;
    }
}

#[cfg(test)]
mod tests;
