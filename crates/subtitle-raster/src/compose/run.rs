// Порт из libass 0.17.5: ass_render.c (флаги фильтров из render_and_combine_glyphs,
// ass_composite_construct без размытия).
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

//! Участок единого стиля: объединение битмапов его глифов в три слоя (заливка, обводка, тень).

use subtitle_core::SubtitleColor;

use super::ComposeError;
use super::glyph_bitmaps::GlyphBitmaps;
use crate::bitmap::{CoverageBitmap, PlacedBitmap, combine_bitmaps, fix_outline, shadow_bitmap};
use crate::event_style::{BorderStyle, EventStyle};
use crate::fixed_point::double_to_d6;

/// Флаги фильтров участка (libass: `FILTER_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FilterFlags {
    /// `BorderStyle = 3`: обводка заменена непрозрачной плашкой на глиф.
    pub(super) box_border: bool,
    /// Толщина обводки ненулевая хотя бы по одной оси.
    pub(super) nonzero_border: bool,
    /// Тень нужна (смещение ненулевое и есть что отбрасывать).
    pub(super) nonzero_shadow: bool,
    /// Заливка полупрозрачна, поэтому тень рисуется и под ней (libass: `FILL_IN_SHADOW`).
    pub(super) fill_in_shadow: bool,
    /// Обводка рисуется под заливкой целиком (libass: `FILL_IN_BORDER`).
    pub(super) fill_in_border: bool,
}

impl FilterFlags {
    /// Флаги по стилю и цвету заливки глифа (libass: начало цикла `render_and_combine_glyphs`).
    pub(super) fn for_glyph(style: &EventStyle, primary: SubtitleColor) -> Self {
        let box_border = style.border_style == BorderStyle::OpaqueBox;
        let nonzero_border = style.border.is_nonzero();
        let mut nonzero_shadow = style.shadow.is_nonzero();
        // Прозрачность libass = 255 - альфа: `_a(c) != 0xFF` значит «альфа не нулевая».
        let fill_in_shadow = nonzero_shadow && (primary.alpha != 0 || box_border);
        if !nonzero_border && !fill_in_shadow {
            nonzero_shadow = false;
        }
        let secondary = style.colors.secondary;
        let fill_in_border =
            (nonzero_border && primary.alpha == 255 && secondary.alpha == 255) || box_border;
        Self {
            box_border,
            nonzero_border,
            nonzero_shadow,
            fill_in_shadow,
            fill_in_border,
        }
    }
}

/// Итоговые слои участка в координатах кадра (libass: `CompositeHashValue`).
#[derive(Debug, Default)]
pub(super) struct RunBitmaps {
    /// Заливка.
    pub(super) fill: Option<CoverageBitmap>,
    /// Обводка (или плашка).
    pub(super) outline: Option<CoverageBitmap>,
    /// Тень.
    pub(super) shadow: Option<CoverageBitmap>,
}

/// Смещение тени, квантованное как в libass при нулевом размытии (маска `7`: шаг 1/8 пикселя).
pub(super) fn quantized_shadow_offset(style: &EventStyle) -> (i32, i32) {
    const SHADOW_MASK: i32 = 7;
    let quantize = |pixels: f64| (double_to_d6(pixels) + (SHADOW_MASK >> 1)) & !SHADOW_MASK;
    (
        quantize(style.shadow.horizontal),
        quantize(style.shadow.vertical),
    )
}

/// Объединяет битмапы глифов участка и применяет фильтры (libass: `ass_composite_construct`).
///
/// Размытие (`\be`, `\blur`) не поддерживается, поэтому поля битмапов не расширяются.
pub(super) fn composite_run(
    parts: &[GlyphBitmaps],
    flags: FilterFlags,
    shadow_offset: (i32, i32),
) -> Result<RunBitmaps, ComposeError> {
    let fill_parts: Vec<PlacedBitmap<'_>> = parts
        .iter()
        .filter_map(|part| {
            part.fill.as_ref().map(|bitmap| PlacedBitmap {
                bitmap,
                anchor: (part.fill_position.x, part.fill_position.y),
            })
        })
        .collect();
    let outline_parts: Vec<PlacedBitmap<'_>> = parts
        .iter()
        .filter_map(|part| {
            part.outline.as_ref().map(|bitmap| PlacedBitmap {
                bitmap,
                anchor: (part.outline_position.x, part.outline_position.y),
            })
        })
        .collect();
    let fill = combine_bitmaps(&fill_parts)?;
    let mut outline = combine_bitmaps(&outline_parts)?;
    let mut shadow: Option<CoverageBitmap> = None;

    if !flags.fill_in_border && !flags.fill_in_shadow {
        fix_outline_layers(fill.as_ref(), outline.as_mut())?;
    }

    if flags.nonzero_shadow {
        let source = if flags.nonzero_border {
            let mut copy = outline.clone();
            if flags.fill_in_border && !flags.fill_in_shadow {
                fix_outline_layers(fill.as_ref(), copy.as_mut())?;
            }
            copy
        } else if flags.box_border {
            // Плашка становится тенью, обводки у глифа нет.
            outline.take()
        } else {
            fill.clone()
        };
        shadow = source
            .map(|source| shadow_bitmap(&source, shadow_offset.0, shadow_offset.1))
            .transpose()?;
    }

    if flags.fill_in_shadow && !flags.fill_in_border {
        fix_outline_layers(fill.as_ref(), outline.as_mut())?;
    }
    Ok(RunBitmaps {
        fill,
        outline,
        shadow,
    })
}

/// `fix_outline` для необязательных слоёв (libass допускает пустые битмапы без буфера).
fn fix_outline_layers(
    fill: Option<&CoverageBitmap>,
    outline: Option<&mut CoverageBitmap>,
) -> Result<(), ComposeError> {
    if let (Some(fill), Some(outline)) = (fill, outline) {
        fix_outline(fill, outline)?;
    }
    Ok(())
}
