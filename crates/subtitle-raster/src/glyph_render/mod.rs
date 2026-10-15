// Порт из libass 0.17.5: ass_font.c (ass_face_set_size, ass_font_get_asc_desc,
// ass_font_get_glyph с hinting = NONE) и ass_render.c (fix_glyph_scaling,
// ass_outline_construct для OUTLINE_GLYPH).
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

//! Контуры глифов и масштаб лица «как у libass» при `ASS_HINTING_NONE`.
//!
//! ## Схема масштабирования libass
//! При отключённом хинтинге libass всегда просит у FreeType размер 256 пикселей
//! (`fix_glyph_scaling`: «достаточно большой, чтобы не было округлений сетки») и потом
//! уменьшает готовый контур матрицей на `font_size / 256`. Поэтому здесь:
//! 1. [`FaceScale`] — масштаб 16.16 для запроса `FT_SIZE_REQUEST_TYPE_REAL_DIM` высотой
//!    256 px: ячейка лица равна `ascender - descender` по Windows-метрикам
//!    ([`crate::fonts::FaceMetrics`]);
//! 2. [`load_glyph_outline`] достаёт контур skrifa в единицах шрифта (`Size::unscaled`) и сам
//!    масштабирует каждую координату целочисленным `mul_fix` — как FreeType, а не float-масштабом
//!    skrifa, который моделирует другой запрос размера (`FT_Set_Pixel_Size`).

mod advance;
mod retrieve;
mod scale_pen;

use skrifa::outline::DrawSettings;
use skrifa::{FontRef, GlyphId, MetadataProvider, instance::Size};

use crate::fixed_point::{div_fix, double_to_d6, mul_fix};
use crate::fonts::FaceMetrics;
use crate::outline::pen::GlyphOutlinePen;
use crate::outline::{ControlBox, Outline, OutlineError};
pub(crate) use advance::HintedAdvances;
pub(crate) use retrieve::retrieve_glyphs;
use scale_pen::FreeTypeScalePen;

/// Размер лица в пикселях, который libass запрашивает у FreeType при отключённом хинтинге
/// (`fix_glyph_scaling`: «arbitrary, not too small to prevent grid fitting rounding effects»).
pub(crate) const FREETYPE_REQUEST_SIZE: f64 = 256.0;

/// Масштаб лица для запроса размера [`FREETYPE_REQUEST_SIZE`] (метрики `FT_Size`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FaceScale {
    /// Единиц шрифта на em.
    units_per_em: u16,
    /// Масштаб 16.16: единицы шрифта -> 1/64 пикселя при размере 256 px (`y_scale`).
    unit_scale: i64,
    /// Масштаб для шейпера (libass: `(x_scale * upem) >> 16`).
    shaping_scale: i32,
    /// Максимальный подъём в 26.6 (libass: `ass_font_get_asc_desc`).
    ascent: i32,
    /// Максимальный спуск в 26.6 (положительное число).
    descent: i32,
}

impl FaceScale {
    /// Считает масштаб по метрикам лица; `None` — вырожденное лицо (нулевая ячейка).
    pub(crate) fn from_metrics(metrics: &FaceMetrics) -> Option<Self> {
        // REAL_DIM: ячейка = ascender - descender (descender у нас отрицательный).
        let cell_height = i64::from(metrics.ascender) - i64::from(metrics.descender);
        if cell_height <= 0 || metrics.units_per_em == 0 {
            return None;
        }
        let requested_height = i64::from(double_to_d6(FREETYPE_REQUEST_SIZE));
        let unit_scale = div_fix(requested_height, cell_height);
        let shaping_scale = (unit_scale * i64::from(metrics.units_per_em)) >> 16;
        Some(Self {
            units_per_em: metrics.units_per_em,
            unit_scale,
            shaping_scale: i32::try_from(shaping_scale).ok()?,
            ascent: i32::try_from(mul_fix(i64::from(metrics.ascender), unit_scale)).ok()?,
            descent: i32::try_from(mul_fix(-i64::from(metrics.descender), unit_scale)).ok()?,
        })
    }

    /// Масштаб шейпера: значения при нём измеряются в 1/64 пикселя при размере 256 px.
    pub(crate) fn shaping_scale(&self) -> i32 {
        self.shaping_scale
    }

    /// Максимальный подъём лица в 26.6 при размере 256 px.
    pub(crate) fn ascent(&self) -> i32 {
        self.ascent
    }

    /// Максимальный спуск лица в 26.6 при размере 256 px.
    pub(crate) fn descent(&self) -> i32 {
        self.descent
    }

    /// Размер в пикселях на em для хинтера: тот же масштаб, но в виде, который принимает skrifa
    /// (кратный 1/64, как у запроса размера FreeType).
    pub(crate) fn hinting_pixels_per_em(&self) -> f32 {
        let units_per_em_scaled = (self.unit_scale * i64::from(self.units_per_em) + 0x8000) >> 16;
        units_per_em_scaled as f32 / 64.0
    }

    /// Переводит расстояние в единицах шрифта в 26.6 при размере 256 px (как `FT_MulFix`).
    pub(crate) fn units_to_d6(&self, units: i32) -> i32 {
        i32::try_from(mul_fix(i64::from(units), self.unit_scale)).unwrap_or(i32::MAX)
    }
}

impl FaceScale {
    /// То же для дробного числа единиц (середины неявных on-curve точек); округление
    /// к ближайшему, ничьи — от нуля. Для целых единиц совпадает с [`Self::units_to_d6`].
    pub(crate) fn fractional_units_to_d6(&self, units: f64) -> i64 {
        (units * self.unit_scale as f64 / 65536.0).round() as i64
    }
}

/// Контур глифа в 26.6 при размере 256 px (ось Y вниз) и его рамка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlyphOutline {
    /// Контур.
    pub(crate) outline: Outline,
    /// Рамка контрольных точек; нули, если контур пуст (libass: `val->cbox`).
    pub(crate) control_box: ControlBox,
}

/// Почему контур глифа не загружен.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum GlyphOutlineError {
    /// В шрифте нет контура для этого номера глифа (например, глиф из bitmap-таблицы).
    #[error("glyph {glyph:?} has no vector outline")]
    NoOutline {
        /// Номер глифа.
        glyph: GlyphId,
    },
    /// skrifa не смог прочитать глиф.
    #[error("glyph {glyph:?} cannot be loaded: {reason}")]
    Load {
        /// Номер глифа.
        glyph: GlyphId,
        /// Причина от skrifa.
        reason: String,
    },
    /// Контур не помещается в допустимый диапазон libass.
    #[error("glyph outline does not fit into the libass range: {0}")]
    Conversion(#[from] OutlineError),
}

/// Загружает контур глифа (libass: `ass_font_get_glyph` + `ass_get_glyph_outline`).
///
/// Синтетическая жирность и наклон здесь не применяются: решение «нужны ли они» принимает
/// вызывающий по [`crate::fonts::FaceMatch`].
pub(crate) fn load_glyph_outline(
    font: &FontRef<'_>,
    scale: &FaceScale,
    glyph: GlyphId,
) -> Result<GlyphOutline, GlyphOutlineError> {
    let Some(outline_glyph) = font.outline_glyphs().get(glyph) else {
        return Err(GlyphOutlineError::NoOutline { glyph });
    };
    let mut pen = FreeTypeScalePen::new(GlyphOutlinePen::new(), *scale);
    outline_glyph
        .draw(DrawSettings::unhinted(Size::unscaled(), &[][..]), &mut pen)
        .map_err(|error| GlyphOutlineError::Load {
            glyph,
            reason: error.to_string(),
        })?;
    let outline = pen.into_inner().finish()?;

    let mut control_box = ControlBox::EMPTY;
    outline.update_control_box(&mut control_box);
    if control_box.is_empty() {
        // libass: пустая рамка превращается в нули.
        control_box = ControlBox {
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
        };
    }
    Ok(GlyphOutline {
        outline,
        control_box,
    })
}

#[cfg(test)]
mod tests;
