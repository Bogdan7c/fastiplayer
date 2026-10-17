// Порт из libass 0.17.5: ass_render.c (get_outline_glyph, ветка `drawing_text`;
// ass_outline_construct, ветка OUTLINE_DRAWING).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
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

//! Метрики и контур псевдоглифа-рисунка (`\p`): то, что для текста делает `place_outline`.
//!
//! Рисунок не имеет шрифта, поэтому его «глиф» строится из разобранных команд
//! ([`crate::drawing`]):
//! - продвижение — ширина рамки точек рисунка, умноженная на масштаб по X;
//! - подъём — высота рамки МИНУС спуск, спуск — `64 * \pbo` (рисунок стоит на базовой линии,
//!   смещённой на `\pbo` единиц рисунка вниз);
//! - контур сдвигается по вертикали на `-подъём * масштаб по Y` (в `PlacedGlyph::outline_offset_y`),
//!   по горизонтали сдвига нет (в отличие от текста, левый край не прижимается к нулю).
//!
//! Масштаб (`transform.scale` libass) лежит в `TextCluster::glyph_scale`: его считает
//! планировщик реплики вместе с `2^-(n-1)` и масштабом кадра.

use std::sync::Arc;

use super::GlyphOutline;
use crate::drawing::{DrawingError, ParsedDrawing, parse_drawing};
use crate::fixed_point::Vector26;
use crate::glyph_info::{GlyphBox, TextCluster};
use crate::outline::round_ties_even_to_i32;
use crate::source_text::DrawingSource;

/// Что стало с рисунком при загрузке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DrawingOutcome {
    /// Контур построен, рисунок участвует в раскладке и рисуется.
    Placed,
    /// Рисунок не дал контура: команды не распознаны (в том числе `n` до `m`, одни
    /// перемещения) или точка вышла за допустимый диапазон. Глиф остаётся (libass оставляет
    /// пустой псевдоглиф в строке), но ничего не рисует.
    NoOutline,
}

/// Разбирает рисунок и заполняет корневой глиф кластера: контур, рамку, подъём, спуск,
/// продвижение (libass: `get_outline_glyph` для `drawing_text.str`).
pub(super) fn place_drawing(cluster: &mut TextCluster, source: &DrawingSource) -> DrawingOutcome {
    let parsed = match parse_drawing(&source.commands) {
        Ok(parsed) => parsed,
        Err(error) => {
            // Текст рисунка в журнал не пишем: реплика может быть приватной.
            log_rejected(&error);
            return DrawingOutcome::NoOutline;
        }
    };
    let has_outline = !parsed.outline.is_empty();
    apply_metrics(cluster, source, parsed);
    if has_outline {
        DrawingOutcome::Placed
    } else {
        DrawingOutcome::NoOutline
    }
}

fn log_rejected(error: &DrawingError) {
    tracing::debug!(%error, "ASS drawing is rejected, the drawing is not drawn");
}

/// Метрики и контур из разобранного рисунка.
fn apply_metrics(cluster: &mut TextCluster, source: &DrawingSource, parsed: ParsedDrawing) {
    let scale = cluster.glyph_scale;
    // libass: `desc = 64 * drawing_pbo; asc = val->asc - desc` в целых `int32`.
    let descent_units = source.baseline_offset.wrapping_mul(64);
    let ascent_units = parsed.height().wrapping_sub(descent_units);
    let offset_y = -f64::from(ascent_units) * scale.vertical;
    let width_units = parsed.width();
    let outline = Arc::new(GlyphOutline::from_outline(parsed.outline));

    let control = &outline.control_box;
    let glyph = &mut cluster.root;
    glyph.outline_offset_y = offset_y;
    glyph.bbox = GlyphBox {
        x_min: round_ties_even_to_i32(f64::from(control.x_min) * scale.horizontal),
        y_min: round_ties_even_to_i32(f64::from(control.y_min) * scale.vertical + offset_y),
        x_max: round_ties_even_to_i32(f64::from(control.x_max) * scale.horizontal),
        y_max: round_ties_even_to_i32(f64::from(control.y_max) * scale.vertical + offset_y),
    };
    glyph.ascent = round_ties_even_to_i32(f64::from(ascent_units) * scale.vertical);
    glyph.descent = round_ties_even_to_i32(f64::from(descent_units) * scale.vertical);
    glyph.advance = Vector26 {
        x: round_ties_even_to_i32(f64::from(width_units) * scale.horizontal),
        y: 0,
    };
    glyph.outline = Some(outline);
    // libass: `cluster_advance = advance` для рисунка.
    cluster.cluster_advance = cluster.root.advance;
}

#[cfg(test)]
mod tests;
