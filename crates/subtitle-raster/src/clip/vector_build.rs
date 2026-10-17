// Порт из libass 0.17.5: ass_render.c (blend_vector_clip: построение маски),
// ass_cache.c/ass_render.c (ass_outline_construct для OUTLINE_DRAWING).
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

//! Построение маски векторного клипа из текста рисунка.
//!
//! Маска строится так же, как в `blend_vector_clip`: рисунок разбирается тем же разборщиком, что
//! `\p`, матрица содержит только масштаб кадра и степень точности рисунка (`m00 = screen_scale_x *
//! 2^-(scale-1)`, `m11` аналогично), без `\org`, поворотов и `par_scale`; положение квантуется
//! как у глифа (`quantize_transform` с `first = true`) и растеризуется тем же растеризатором.

use subtitle_core::AssClipMode;

use super::vector::VectorClipMask;
use crate::bitmap::{BitmapError, PixelBounds, RasterRegion};
use crate::drawing::parse_drawing;
use crate::glyph_transform::{
    Matrix3, RASTERIZER_PRECISION, construct_bitmap_within, control_box, quantize_transform,
};
use crate::outline::Outline;
use crate::overlay::CanvasSize;
use crate::raster::Rasterizer;

/// Что нужно знать о векторном клипе до растеризации (считает планировщик реплики).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VectorClipPlan {
    pub(crate) mode: AssClipMode,
    /// Текст рисунка как написан в теге.
    pub(crate) drawing: String,
    /// `m00` матрицы: `screen_scale_x * 2^-(scale-1)`.
    pub(crate) scale_x: f64,
    /// `m11` матрицы: `screen_scale_y * 2^-(scale-1)`.
    pub(crate) scale_y: f64,
}

/// Почему векторный клип не применяется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IgnoredVectorClip {
    /// Рисунок отвергнут разборщиком (точка вне допустимого диапазона): libass считает контур
    /// невалидным и клип не применяет.
    InvalidDrawing,
    /// Рисунок не пустой, но у матрицы нет допустимого квантования (libass: `!quantize_transform`).
    Untransformable,
}

/// Итог построения маски.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VectorClipOutcome {
    /// Маска готова (возможно, пустая).
    Ready(VectorClipMask),
    /// Клип не применяется, картинки остаются как есть.
    Ignored(IgnoredVectorClip),
}

/// Растеризует фигуру клипа, но только ту её часть, что лежит внутри холста `canvas`.
///
/// Картинки реплики всегда целиком внутри холста, а маска влияет лишь на пересечение с
/// картинкой (`blend_vector_clip`). Поэтому всё, что маска покрывает за холстом, на результат
/// не влияет, и растеризовать это не нужно: в противном случае враждебный или просто огромный
/// `\clip(m -50000 -50000 l ...)` выделял бы гигантский битмап (раньше такой клип приходилось
/// отбрасывать целиком, и `\clip` переставал клипать). Значения пикселей внутри холста те же,
/// что дала бы растеризация целиком, то есть результат совпадает с libass для всех видимых
/// пикселей.
pub(crate) fn build_vector_clip_mask(
    plan: &VectorClipPlan,
    canvas: CanvasSize,
) -> Result<VectorClipOutcome, BitmapError> {
    let Ok(parsed) = parse_drawing(&plan.drawing) else {
        return Ok(VectorClipOutcome::Ignored(
            IgnoredVectorClip::InvalidDrawing,
        ));
    };
    let outline = parsed.outline;
    // Рамка контура (не рамка точек рисунка): libass берёт `cbox` самого контура, у пустого — нули.
    let cbox = control_box(&[&outline]);
    let mut matrix: Matrix3 = [
        [plan.scale_x, 0.0, 0.0],
        [0.0, plan.scale_y, 0.0],
        [0.0, 0.0, 1.0],
    ];
    // `first = true`: дробная ошибка позиции здесь не переиспользуется, offset нужен только как приёмник.
    let mut unused_offset = [0.0_f64; 2];
    let Some(key) = quantize_transform(&mut matrix, &mut unused_offset, true, &cbox) else {
        return Ok(VectorClipOutcome::Ignored(
            IgnoredVectorClip::Untransformable,
        ));
    };
    // Холст в координатах битмапа: экранные координаты минус положение начала маски.
    // Размер самого битмапа ограничен размером холста (плюс тайл), отдельный предел не нужен.
    let visible = RasterRegion::Within(PixelBounds {
        left: key.position.x.saturating_neg(),
        top: key.position.y.saturating_neg(),
        right: i32::try_from(canvas.width())
            .unwrap_or(i32::MAX)
            .saturating_sub(key.position.x),
        bottom: i32::try_from(canvas.height())
            .unwrap_or(i32::MAX)
            .saturating_sub(key.position.y),
    });
    let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
    let empty = Outline::new();
    let bitmap =
        construct_bitmap_within(&mut rasterizer, &key, &cbox, [&outline, &empty], visible)?;
    let mask = match bitmap {
        // libass: `ass_outline_to_bitmap` не смог -> нулевой битмап; пересечений нет ни с чем.
        None => VectorClipMask::empty(plan.mode, (key.position.x, key.position.y)),
        Some(bitmap) => VectorClipMask::new(
            plan.mode,
            (
                key.position.x.saturating_add(bitmap.left),
                key.position.y.saturating_add(bitmap.top),
            ),
            (bitmap.width, bitmap.height),
            bitmap.stride,
            bitmap.buffer.into(),
        ),
    };
    Ok(VectorClipOutcome::Ready(mask))
}

#[cfg(test)]
mod tests;
