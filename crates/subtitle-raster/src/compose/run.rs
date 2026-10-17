// Порт из libass 0.17.5: ass_render.c (флаги фильтров из render_and_combine_glyphs,
// ass_composite_construct).
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

use std::sync::Arc;

use super::ComposeError;
use super::filter::RunFilter;
use super::glyph_bitmaps::GlyphBitmaps;
use crate::bitmap::{
    BitmapError, CoverageBitmap, PlacedBitmap, combine_bitmaps_with_padding, fix_outline,
    shadow_bitmap,
};
use crate::blur::{BitmapSideLimit, BlurError, BlurScratch, BlurStrength, synth_blur};
use crate::event_fade::EventFade;
use crate::event_style::BorderStyle;
use crate::glyph_cache::{
    ArcId, CompositeCache, CompositeEntry, CompositeKey, PartKey, PlacedMask, SharedMask,
};
use crate::karaoke::KaraokeEffect;
use crate::span_look::SpanLook;

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
    /// Флаги одним числом (для ключа кэша склеек).
    pub(super) fn bits(self) -> u8 {
        u8::from(self.box_border)
            | u8::from(self.nonzero_border) << 1
            | u8::from(self.nonzero_shadow) << 2
            | u8::from(self.fill_in_shadow) << 3
            | u8::from(self.fill_in_border) << 4
    }

    /// Флаги по виду символа (libass: начало цикла `render_and_combine_glyphs`).
    ///
    /// `fade` — затухание реплики: при нём заливка обводки отключена (libass: `fade == 0` в
    /// условии `FILTER_FILL_IN_BORDER`), хотя цвета в `look` остаются «до затухания».
    ///
    /// `effect` — караоке-эффект слова символа: `\kf` и `\ko` принудительно включают
    /// `FILL_IN_SHADOW` (libass: `info->effect_type == EF_KARAOKE_KF / EF_KARAOKE_KO`), потому
    /// что заливка в таком слове бывает прозрачной (вторичный цвет) и тень нужна и под ней.
    pub(super) fn for_glyph(
        look: &SpanLook,
        fade: EventFade,
        effect: Option<KaraokeEffect>,
    ) -> Self {
        let primary = look.colors.primary;
        let box_border = look.border_style == BorderStyle::OpaqueBox;
        let nonzero_border = look.border.is_nonzero();
        let mut nonzero_shadow = look.shadow.is_nonzero();
        // Прозрачность libass = 255 - альфа: `_a(c) != 0xFF` значит «альфа не нулевая».
        let karaoke_fills_shadow =
            matches!(effect, Some(KaraokeEffect::Fill | KaraokeEffect::Outline));
        let fill_in_shadow =
            nonzero_shadow && (karaoke_fills_shadow || primary.alpha != 0 || box_border);
        if !nonzero_border && !fill_in_shadow {
            nonzero_shadow = false;
        }
        let secondary = look.colors.secondary;
        let fill_in_border =
            (nonzero_border && primary.alpha == 255 && secondary.alpha == 255 && fade.is_none())
                || box_border;
        Self {
            box_border,
            nonzero_border,
            nonzero_shadow,
            fill_in_shadow,
            fill_in_border,
        }
    }
}

/// Итоговые слои участка в координатах кадра (libass: `CompositeHashValue` плюс смещение
/// участка). Маски разделяются с кэшем склеек.
#[derive(Debug, Default)]
pub(super) struct RunBitmaps {
    /// Заливка.
    pub(super) fill: Option<PlacedMask>,
    /// Обводка (или плашка).
    pub(super) outline: Option<PlacedMask>,
    /// Тень.
    pub(super) shadow: Option<PlacedMask>,
    /// Сколько слоёв осталось неразмытыми: результат размытия не поместился в предел.
    pub(super) unblurred_oversized_layers: u32,
    /// Сколько слоёв осталось неразмытыми из-за нарушенного внутреннего инварианта
    /// (см. [`LayerBlur::Failed`]).
    pub(super) failed_blur_layers: u32,
}

/// Чем кончилось размытие одного слоя ([`blur_layer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayerBlur {
    /// Слой размыт (или размывать было нечего: слоя нет, размытие нулевое).
    Done,
    /// Слой остался как был: результат не поместился в предел размера (штатная защита).
    SkippedOversized,
    /// Слой остался как был: нарушен внутренний инвариант (недопустимая дисперсия или
    /// несогласованная геометрия битмапа). Это ошибка, а не защита, поэтому считается отдельно.
    Failed,
}

/// Среда размытия композита: рабочая память и предел размера результата.
///
/// Рабочую память ([`BlurScratch`]) создаёт и хранит растеризатор (одна на поток отрисовки), а
/// композиция лишь заимствует её на время вызова: так размытие не выделяет память на каждый
/// участок.
pub(super) struct BlurEnv<'scratch> {
    /// Рабочая память размытия.
    pub(super) scratch: &'scratch mut BlurScratch,
    /// Предел стороны размытого битмапа (`RasterInputLimits::max_canvas_side`).
    pub(super) side_limit: BitmapSideLimit,
}

/// Склейка слоёв участка через кэш (libass: `ass_cache_get(composite_cache, ...)`).
///
/// Ключ - фильтры и части участка с позициями относительно начала участка ([`run_origin`]),
/// поэтому участок, сдвинутый на целое число пикселей, даёт попадание и получает те же маски с
/// другим смещением. Счётчики пропущенного хранятся в записи и возвращаются и при попадании.
pub(super) fn composite_run(
    parts: &[GlyphBitmaps],
    filter: &RunFilter,
    blur_env: &mut BlurEnv<'_>,
    cache: &mut CompositeCache,
) -> Result<RunBitmaps, ComposeError> {
    let origin = run_origin(parts);
    let key = composite_key(parts, filter, origin, blur_env.side_limit);
    let entry = match cache.get(&key) {
        Some(hit) => hit,
        None => {
            let built = construct_composite(parts, filter, origin, blur_env)?;
            cache.insert(key, built)
        }
    };
    Ok(RunBitmaps {
        fill: entry
            .fill
            .as_ref()
            .map(|mask| PlacedMask::new(Arc::clone(mask), origin)),
        outline: entry
            .outline
            .as_ref()
            .map(|mask| PlacedMask::new(Arc::clone(mask), origin)),
        shadow: entry
            .shadow
            .as_ref()
            .map(|mask| PlacedMask::new(Arc::clone(mask), origin)),
        unblurred_oversized_layers: entry.unblurred_oversized_layers,
        failed_blur_layers: entry.failed_blur_layers,
    })
}

/// Начало участка: наименьшая позиция заливки среди его частей (libass: `info->x/y`).
fn run_origin(parts: &[GlyphBitmaps]) -> (i32, i32) {
    let min_x = parts.iter().map(|part| part.fill_position.x).min();
    let min_y = parts.iter().map(|part| part.fill_position.y).min();
    (min_x.unwrap_or(0), min_y.unwrap_or(0))
}

/// Позиция относительно начала участка.
fn relative_to(position: crate::outline::Point, origin: (i32, i32)) -> (i32, i32) {
    // Насыщающее вычитание: позиции из враждебных тегов не должны вызвать переполнения.
    (
        position.x.saturating_sub(origin.0),
        position.y.saturating_sub(origin.1),
    )
}

/// Ключ кэша склеек для участка (libass: `CompositeHashKey`).
fn composite_key(
    parts: &[GlyphBitmaps],
    filter: &RunFilter,
    origin: (i32, i32),
    side_limit: BitmapSideLimit,
) -> CompositeKey {
    CompositeKey {
        filter: filter.cache_key(side_limit),
        parts: parts
            .iter()
            .map(|part| PartKey {
                fill: part.fill.as_ref().map(ArcId::new),
                outline: part.outline.as_ref().map(ArcId::new),
                fill_position: relative_to(part.fill_position, origin),
                outline_position: relative_to(part.outline_position, origin),
            })
            .collect(),
    }
}

/// Объединяет битмапы глифов участка и применяет фильтры (libass: `ass_composite_construct`).
///
/// Порядок как в libass: сложение битмапов с отступом под `\be`, размытие обводки (всегда) и
/// заливки (только без обводки или при плашке), `fix_outline`, тень от уже размытого слоя.
/// Позиции считаются относительно `origin`, результат тоже лежит в этих координатах.
fn construct_composite(
    parts: &[GlyphBitmaps],
    filter: &RunFilter,
    origin: (i32, i32),
    blur_env: &mut BlurEnv<'_>,
) -> Result<CompositeEntry, ComposeError> {
    let flags = filter.flags;
    let padding_px = filter.blur.box_passes.padding_px();
    let fill_parts: Vec<PlacedBitmap<'_>> = parts
        .iter()
        .filter_map(|part| {
            part.fill.as_deref().map(|bitmap| PlacedBitmap {
                bitmap,
                anchor: relative_to(part.fill_position, origin),
            })
        })
        .collect();
    let outline_parts: Vec<PlacedBitmap<'_>> = parts
        .iter()
        .filter_map(|part| {
            part.outline.as_deref().map(|bitmap| PlacedBitmap {
                bitmap,
                anchor: relative_to(part.outline_position, origin),
            })
        })
        .collect();
    let mut fill = combine_bitmaps_with_padding(&fill_parts, padding_px)?;
    let mut outline = combine_bitmaps_with_padding(&outline_parts, padding_px)?;
    let mut shadow: Option<CoverageBitmap> = None;
    let mut unblurred_oversized_layers = 0_u32;
    let mut failed_blur_layers = 0_u32;
    let mut tally = |outcome: LayerBlur| match outcome {
        LayerBlur::Done => {}
        LayerBlur::SkippedOversized => unblurred_oversized_layers += 1,
        LayerBlur::Failed => failed_blur_layers += 1,
    };

    // libass: заливка размывается, только если под ней нет отдельной обводки (или это плашка).
    if !flags.nonzero_border || flags.box_border {
        tally(blur_layer(fill.as_mut(), filter.blur, blur_env));
    }
    tally(blur_layer(outline.as_mut(), filter.blur, blur_env));

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
            .map(|source| shadow_bitmap(&source, filter.shadow_offset.0, filter.shadow_offset.1))
            .transpose()?;
    }

    if flags.fill_in_shadow && !flags.fill_in_border {
        fix_outline_layers(fill.as_ref(), outline.as_mut())?;
    }
    let freeze =
        |layer: Option<CoverageBitmap>| layer.map(|bitmap| Arc::new(SharedMask::freeze(bitmap)));
    Ok(CompositeEntry {
        fill: freeze(fill),
        outline: freeze(outline),
        shadow: freeze(shadow),
        unblurred_oversized_layers,
        failed_blur_layers,
    })
}

/// Размывает один слой на месте; возвращает, чем это кончилось ([`LayerBlur`]).
///
/// libass при нехватке памяти оставляет битмап как был (`ass_gaussian_blur` возвращает
/// `false` до изменений, а `ass_synth_blur` результат не проверяет), поэтому и здесь слой
/// остаётся неразмытым и картинка реплики не пропадает. Разница с libass одна: предел
/// `side_limit` (защита от враждебного `\blur` на огромной картинке) мягче «нет памяти», но
/// действует так же. Такой случай считается, чтобы он был виден в статистике. Остальные
/// ошибки означают нарушенный внутренний инвариант: пишем предупреждение (без текста реплики),
/// оставляем слой как есть и возвращаем [`LayerBlur::Failed`] для отдельного счётчика.
pub(super) fn blur_layer(
    layer: Option<&mut CoverageBitmap>,
    strength: BlurStrength,
    blur_env: &mut BlurEnv<'_>,
) -> LayerBlur {
    let Some(layer) = layer else {
        return LayerBlur::Done;
    };
    match synth_blur(blur_env.scratch, layer, strength, blur_env.side_limit) {
        Ok(()) => LayerBlur::Done,
        Err(BlurError::TooLarge { .. } | BlurError::ExceedsSideLimit { .. }) => {
            LayerBlur::SkippedOversized
        }
        Err(error @ (BlurError::InvalidVariance { .. } | BlurError::InconsistentGeometry)) => {
            tracing::warn!(%error, "subtitle blur skipped: internal invariant violated");
            LayerBlur::Failed
        }
    }
}

/// `fix_outline` для необязательных слоёв (libass допускает пустые битмапы без буфера).
fn fix_outline_layers(
    fill: Option<&CoverageBitmap>,
    outline: Option<&mut CoverageBitmap>,
) -> Result<(), BitmapError> {
    if let (Some(fill), Some(outline)) = (fill, outline) {
        fix_outline(fill, outline)?;
    }
    Ok(())
}
