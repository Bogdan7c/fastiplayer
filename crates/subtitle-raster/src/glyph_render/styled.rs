// Порт поведения libass 0.17.5: ass_font.c (ass_font_get_glyph — синтетика;
// ass_get_glyph_outline — линии).
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

//! Контур глифа с синтетическим начертанием и линиями подчёркивания/зачёркивания.

use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use super::decoration::add_decoration_lines;
use super::face_style::{FaceStyleInfo, GlyphVariant};
use super::{FaceScale, GlyphOutline, GlyphOutlineError, load_base_outline};
use crate::fonts::{SyntheticBold, SyntheticItalic};
use crate::outline::Outline;

/// Сдвиг наклона глифов с контурами TrueType в 16.16: совпадает с GDI, «эффективно tan 18,77°».
const SHEAR_TRUETYPE_16_16: i32 = 0x5700;
/// Сдвиг наклона глифов PostScript (CFF) в 16.16: tan 10°.
const SHEAR_POSTSCRIPT_16_16: i32 = 0x2d24;

/// Загружает контур глифа нужного варианта (libass: `ass_font_get_glyph` с синтетикой и
/// `ass_get_glyph_outline` с линиями).
///
/// Порядок как в libass: наклон, затем утолщение, затем линии (линии не наклоняются и не
/// утолщаются). У глифа без векторного контура (пробел) контур пуст, но линии на нём
/// рисуются. Метрики и продвижения синтетика не меняет.
pub(crate) fn load_styled_glyph_outline(
    font: &FontRef<'_>,
    scale: &FaceScale,
    style: &FaceStyleInfo,
    glyph: GlyphId,
    variant: GlyphVariant,
) -> Result<GlyphOutline, GlyphOutlineError> {
    let mut outline = match load_base_outline(font, scale, glyph) {
        Ok(outline) => outline,
        // Пустой контур: рисовать нечего, но линии могут понадобиться.
        Err(GlyphOutlineError::NoOutline { .. }) => Outline::new(),
        Err(error) => return Err(error),
    };
    if variant.synthetic_italic == SyntheticItalic::Needed {
        let shear = if style.is_postscript() {
            SHEAR_POSTSCRIPT_16_16
        } else {
            SHEAR_TRUETYPE_16_16
        };
        outline = outline.sheared(shear)?;
    }
    if variant.synthetic_bold == SyntheticBold::Needed {
        outline = outline.emboldened(scale.synthetic_bold_strength())?;
    }
    if variant.decorations.underline || variant.decorations.strike_out {
        let advance = unhinted_advance_d6(font, scale, glyph);
        add_decoration_lines(&mut outline, scale, style, variant.decorations, advance)?;
    }
    Ok(GlyphOutline::from_outline(outline))
}

/// Продвижение глифа без хинтинга в 26.6 при размере 256 px (`face->glyph->advance.x` после
/// `FT_LOAD_NO_HINTING`). Нет метрик — 0: линии такому глифу не рисуются.
fn unhinted_advance_d6(font: &FontRef<'_>, scale: &FaceScale, glyph: GlyphId) -> i32 {
    let advance_units = font
        .glyph_metrics(Size::unscaled(), LocationRef::default())
        .advance_width(glyph)
        .unwrap_or(0.0);
    scale.units_to_d6(advance_units.round() as i32)
}
