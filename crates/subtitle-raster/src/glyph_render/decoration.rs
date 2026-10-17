// Порт поведения libass 0.17.5: ass_font.c (ass_get_glyph_outline — подчёркивание и
// зачёркивание, добавляемые к контуру глифа).
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

//! Подчёркивание и зачёркивание как прямоугольники в контуре глифа.
//!
//! libass не рисует линии отдельно: к контуру каждого глифа с положительным продвижением
//! добавляется прямоугольник во всю ширину продвижения (`0..advance`), поэтому линия
//! получает ту же обводку, тень и заливку, что и буква, а соседние глифы смыкаются в
//! сплошную линию.

use super::FaceScale;
use super::face_style::{DecorationLine, FaceStyleInfo};
use crate::outline::synthetic::OuterWinding;
use crate::outline::{OUTLINE_MAX, Outline, OutlineError};
use crate::span_look::TextDecorations;

/// Верх и низ линии в 26.6 при размере 256 px, Y вниз (`low` < `high`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineSpan {
    top: i32,
    bottom: i32,
}

/// Положение линии в 26.6: позиция и толщина округляются «к ближайшему» сдвигом (как в
/// libass), центр линии лежит на позиции из шрифта, толщина делится пополам вниз.
///
/// `None` — линия не помещается в допустимый диапазон контура (libass её пропускает).
fn line_span(scale: &FaceScale, line: DecorationLine) -> Option<LineSpan> {
    let position = scale.decoration_units_to_d6(line.position_units);
    let size = scale.decoration_units_to_d6(line.thickness_units);
    let top = -position - (size >> 1);
    let bottom = top + size;
    let limit = i64::from(OUTLINE_MAX);
    if top < -limit || bottom > limit {
        return None;
    }
    Some(LineSpan {
        top: i32::try_from(top).ok()?,
        bottom: i32::try_from(bottom).ok()?,
    })
}

/// Добавляет к контуру линии подчёркивания и зачёркивания.
///
/// `advance_d6` — продвижение глифа без хинтинга (26.6, как `face->glyph->advance.x` при
/// `FT_LOAD_NO_HINTING`); при нулевом или отрицательном продвижении линии не рисуются
/// (невидимые и комбинирующие символы). Направление обхода прямоугольника совпадает с
/// направлением внешних контуров глифа, иначе при перекрытии с буквой заливка по правилу
/// ненулевого обхода «вырезала» бы дыры.
pub(crate) fn add_decoration_lines(
    outline: &mut Outline,
    scale: &FaceScale,
    style: &FaceStyleInfo,
    decorations: TextDecorations,
    advance_d6: i32,
) -> Result<(), OutlineError> {
    if advance_d6 <= 0 {
        return Ok(());
    }
    let mut spans: Vec<LineSpan> = Vec::with_capacity(2);
    if decorations.underline
        && let Some(span) = style.underline().and_then(|line| line_span(scale, line))
    {
        spans.push(span);
    }
    if decorations.strike_out
        && let Some(span) = style.strike_out().and_then(|line| line_span(scale, line))
    {
        spans.push(span);
    }
    if spans.is_empty() {
        return Ok(());
    }
    // Направление считается по контуру самой буквы, до добавления прямоугольников.
    let clockwise = outline.outer_winding() == OuterWinding::Clockwise;
    for span in spans {
        let (first_y, second_y) = if clockwise {
            (span.top, span.bottom)
        } else {
            (span.bottom, span.top)
        };
        outline.add_rect(0, first_y, advance_d6, second_y)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
