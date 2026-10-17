// Порт из libass 0.17.5: ass_render.c (FilterDesc из render_and_combine_glyphs,
// quantize_blur, restore_blur).
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

//! Описание фильтров участка (libass: `FilterDesc`): флаги слоёв, размытие и смещение тени.
//!
//! Размытие и смещение тени считаются вместе, потому что маска огрубления смещения зависит от
//! квантованного радиуса размытия: чем сильнее размытие, тем грубее допустимо смещение тени.

use super::run::FilterFlags;
use crate::blur::{
    BitmapSideLimit, BlurStrength, BoxBlurPasses, GaussianVariance, blur_radius_scale,
    quantize_blur, quantize_shadow_offset, restore_blur,
};
use crate::event_fade::EventFade;
use crate::fixed_point::double_to_d6;
use crate::glyph_cache::FilterKey;
use crate::karaoke::KaraokeEffect;
use crate::span_look::SpanLook;

/// Всё, что композиция участка берёт из его вида (libass: `FilterDesc`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct RunFilter {
    /// Какие слои нужны и как они перекрываются.
    pub(super) flags: FilterFlags,
    /// Размытие, восстановленное из квантованных уровней (то, что реально применяется).
    pub(super) blur: BlurStrength,
    /// Смещение тени в 26.6, огрублённое маской размытия по каждой оси.
    pub(super) shadow_offset: (i32, i32),
}

impl RunFilter {
    /// Часть ключа кэша склеек, описывающая фильтры (libass: `CompositeHashKey::filter`).
    pub(super) fn cache_key(&self, blur_side_limit: BitmapSideLimit) -> FilterKey {
        FilterKey {
            flags: self.flags.bits(),
            gaussian_bits: [
                self.blur.gaussian.x.to_bits(),
                self.blur.gaussian.y.to_bits(),
            ],
            box_passes: self.blur.box_passes.count(),
            shadow_offset: self.shadow_offset,
            blur_side_limit: blur_side_limit.0,
        }
    }

    /// Фильтры по виду первого символа участка (libass: начало участка в
    /// `render_and_combine_glyphs`).
    #[cfg(test)]
    pub(super) fn for_look(look: &SpanLook, fade: EventFade) -> Self {
        Self::for_look_in_karaoke(look, fade, None)
    }

    /// То же, но с караоке-эффектом слова символа (он меняет флаги тени).
    pub(super) fn for_look_in_karaoke(
        look: &SpanLook,
        fade: EventFade,
        effect: Option<KaraokeEffect>,
    ) -> Self {
        let flags = FilterFlags::for_glyph(look, fade, effect);
        // Порядок умножений как в libass: `blur * (blur_scale * radius_scale)`.
        let radius_scale = blur_radius_scale();
        let blur_x = quantize_blur(look.blur.radius * (look.blur.scale.horizontal * radius_scale));
        let blur_y = quantize_blur(look.blur.radius * (look.blur.scale.vertical * radius_scale));
        // Тень вне `NONZERO_SHADOW` не нужна, смещение тогда не используется; libass кладёт 0.
        let shadow_offset = if flags.nonzero_shadow {
            (
                quantize_shadow_offset(double_to_d6(look.shadow.horizontal), blur_x.shadow_mask),
                quantize_shadow_offset(double_to_d6(look.shadow.vertical), blur_y.shadow_mask),
            )
        } else {
            (0, 0)
        };
        Self {
            flags,
            blur: BlurStrength {
                gaussian: GaussianVariance {
                    x: restore_blur(blur_x.level),
                    y: restore_blur(blur_y.level),
                },
                box_passes: BoxBlurPasses::clamped(look.blur.edge_passes),
            },
            shadow_offset,
        }
    }
}

#[cfg(test)]
mod tests;
