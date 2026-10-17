// Порт из libass 0.17.5: ass_render.c (get_bitmap_glyph, ass_outline_construct
// для OUTLINE_BORDER и OUTLINE_BOX).
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

//! Битмапы одного глифа: заливка и обводка (или плашка) в целых пикселях кадра.

use std::sync::Arc;

use super::ComposeError;
use super::run::FilterFlags;
use crate::bitmap::{BitmapError, CoverageBitmap};
use crate::event_style::AxisPair;
use crate::fixed_point::Vector26;
use crate::glyph_cache::{BitmapCache, BitmapSource, OutlineCache};
use crate::glyph_info::PlacedGlyph;
use crate::glyph_render::GlyphOutline;
use crate::glyph_transform::{
    GlyphPlacement, Matrix3, border_matrix, glyph_transform_matrix, plan_border_stroke,
    quantize_transform, scale_and_offset_matrix,
};
use crate::outline::Point;
use crate::raster::Rasterizer;
use crate::span_look::SpanLook;

/// Битмапы глифа и их позиции в пикселях кадра (libass: `info->bm`, `info->bm_o`, `pos`, `pos_o`).
///
/// Маски общие с кэшем масок глифов (`Arc`): ключ кэша склеек сравнивает их по адресу.
#[derive(Debug, Default)]
pub(super) struct GlyphBitmaps {
    /// Заливка.
    pub(super) fill: Option<Arc<CoverageBitmap>>,
    /// Обводка или плашка.
    pub(super) outline: Option<Arc<CoverageBitmap>>,
    /// Позиция заливки (точка привязки битмапа).
    pub(super) fill_position: Point,
    /// Позиция обводки.
    pub(super) outline_position: Point,
    /// Обводку построить не удалось (ошибка, а не «обводка не нужна»): глиф остаётся без неё.
    pub(super) border_failed: bool,
    /// Глиф пропущен целиком: его битмап после преобразования шире предела стороны.
    pub(super) oversized: bool,
}

/// Всё, что нужно знать о глифе для построения битмапов.
pub(super) struct GlyphBitmapRequest<'a> {
    /// Глиф с абсолютной позицией в 26.6.
    pub(super) glyph: &'a PlacedGlyph,
    /// Положение относительно центра поворота, 26.6 (libass: `info->shift`).
    pub(super) shift: Vector26,
    /// Его контур (общий с кэшем контуров: по его адресу ищутся маски и обводки).
    pub(super) outline: &'a Arc<GlyphOutline>,
    /// Масштаб контура по осям (libass: `info->scale_x`, `info->scale_y`): `font_size / 256`
    /// с учётом `\fscx`/`\fscy`.
    pub(super) glyph_scale: AxisPair,
    /// Масштаб, по отношению осей которого пересчитывается наклон `\fax`/`\fay` (libass:
    /// `info->scale_x/scale_y` в `calc_transform_matrix`). У текста совпадает с `glyph_scale`;
    /// у рисунка это `\fscx`/`\fscy` без масштаба кадра и без `2^-(n-1)`.
    pub(super) shear_scale: AxisPair,
    /// Межбуквенный интервал глифа после масштаба в 26.6 (libass: `hspacing_scaled`); входит в
    /// ширину плашки `BorderStyle = 3`.
    pub(super) letter_spacing_d6: i32,
    /// Растяжение по горизонтали `par_scale_x` (1.0, если пропорции хранения и кадра совпадают).
    pub(super) pixel_aspect_x: f64,
    /// Вид символа (толщина обводки).
    pub(super) look: &'a SpanLook,
    /// Флаги фильтров участка.
    pub(super) flags: FilterFlags,
}

/// Состояние участка, общее для его глифов (libass: `offset` и `first` в `get_bitmap_glyph`).
pub(super) struct RunPhase<'a> {
    /// Растеризатор (переиспользуется между глифами).
    pub(super) rasterizer: &'a mut Rasterizer,
    /// Кэш контуров: обводки и плашка.
    pub(super) outlines: &'a mut OutlineCache,
    /// Кэш масок глифов: растеризация идёт только на промахе.
    pub(super) bitmaps: &'a mut BitmapCache,
    /// Дробная ошибка позиции первого глифа участка.
    pub(super) offset: &'a mut [f64; 2],
    /// У участка ещё нет ни одного глифа с битмапом (пересчитывает `offset`).
    pub(super) first: bool,
}

/// Строит битмапы глифа (libass: `get_bitmap_glyph`).
///
/// `Ok` с пустым результатом — глиф не рисуется (пустой контур или преобразование вне
/// допустимого диапазона); ошибка — сбой растеризации, который нельзя молча проглотить.
pub(super) fn build_glyph_bitmaps(
    request: &GlyphBitmapRequest<'_>,
    phase: &mut RunPhase<'_>,
) -> Result<GlyphBitmaps, ComposeError> {
    match build_glyph_bitmaps_unbounded(request, phase) {
        // Предел стороны: заливка и обводка глифа пропускаются вместе (без картинки глиф не
        // нужен), реплика и остальные глифы рисуются. Текст реплики в журнал не пишем.
        Err(ComposeError::Bitmap(BitmapError::ExceedsSideLimit {
            width,
            height,
            limit,
        })) => {
            tracing::debug!(
                width,
                height,
                limit,
                "glyph bitmap exceeds the side limit after transform, glyph skipped"
            );
            Ok(GlyphBitmaps {
                oversized: true,
                ..GlyphBitmaps::default()
            })
        }
        other => other,
    }
}

/// Тело [`build_glyph_bitmaps`]: ошибка предела стороны приходит сюда как есть.
fn build_glyph_bitmaps_unbounded(
    request: &GlyphBitmapRequest<'_>,
    phase: &mut RunPhase<'_>,
) -> Result<GlyphBitmaps, ComposeError> {
    let GlyphBitmapRequest { outline, flags, .. } = request;
    let GlyphMatrices {
        base: base_matrix,
        scaled: scaled_matrix,
    } = glyph_matrices(request);
    let mut matrix: Matrix3 = scaled_matrix;

    let mut result = GlyphBitmaps::default();
    let Some(fill_key) =
        quantize_transform(&mut matrix, phase.offset, phase.first, &outline.control_box)
    else {
        return Ok(result);
    };
    result.fill = phase.bitmaps.mask(
        phase.rasterizer,
        &BitmapSource::Outline(Arc::clone(outline)),
        &fill_key,
    )?;
    result.fill_position = fill_key.position;
    result.outline_position = fill_key.position;

    let border_bitmap = if flags.box_border {
        if !(flags.nonzero_border || flags.nonzero_shadow) {
            return Ok(result);
        }
        build_box_bitmap(request, phase, &base_matrix)?
    } else {
        if !flags.nonzero_border {
            return Ok(result);
        }
        match build_stroked_bitmap(request, phase, &matrix, &scaled_matrix)? {
            StrokeOutcome::SameAsFill => {
                // Толщина обводки округлилась до нуля: libass отдаёт заливку как обводку.
                result.outline = result.fill.clone();
                return Ok(result);
            }
            StrokeOutcome::Bitmap(bitmap) => bitmap,
            StrokeOutcome::Nothing => return Ok(result),
            StrokeOutcome::Failed => {
                result.border_failed = true;
                return Ok(result);
            }
        }
    };

    match border_bitmap {
        Some((bitmap, position)) => {
            result.outline = Some(bitmap);
            result.outline_position = position;
            if result.fill.is_none() {
                result.fill_position = position;
            }
        }
        None => result.outline_position = result.fill_position,
    }
    Ok(result)
}

/// Матрицы глифа: без масштаба контура и с ним.
pub(super) struct GlyphMatrices {
    /// Матрица положения и поворота (libass: `m1`), нужна и плашке `BorderStyle = 3`.
    pub(super) base: Matrix3,
    /// Она же с масштабом и сдвигом контура (libass: `m2`): ею рисуется контур глифа и
    /// измеряется самая левая точка караоке.
    pub(super) scaled: Matrix3,
}

/// Матрицы глифа (libass: `calc_transform_matrix` и вычисление `m2` в `get_bitmap_glyph`).
pub(super) fn glyph_matrices(request: &GlyphBitmapRequest<'_>) -> GlyphMatrices {
    let glyph = request.glyph;
    let base = glyph_transform_matrix(&GlyphPlacement {
        rotation: request.look.rotation,
        shear: request.look.shear,
        scale: (request.shear_scale.horizontal, request.shear_scale.vertical),
        shift: request.shift,
        ascent: glyph.ascent,
        frame_position: glyph.pos,
        pixel_aspect_x: request.pixel_aspect_x,
        camera_scale: request.look.camera_scale,
    });
    let scaled = scale_and_offset_matrix(
        &base,
        (request.glyph_scale.horizontal, request.glyph_scale.vertical),
        // Сдвиг контура: у текста нулевой, у рисунка ставит базовую линию (`\pbo`).
        (0.0, glyph.outline_offset_y),
    );
    GlyphMatrices { base, scaled }
}

/// Результат построения обводки.
enum StrokeOutcome {
    /// Обводка не нужна: у глифа остаётся только заливка.
    Nothing,
    /// Обводка нужна, но её не удалось построить (слишком толстая, контур слишком велик или сбой
    /// обводчика):
    /// у глифа остаётся только заливка, сбой попадает в счётчик `skipped_borders`.
    Failed,
    /// Толщина после квантования нулевая: обводка совпадает с заливкой.
    SameAsFill,
    /// Битмап обводки (может быть пустым: `None`) и его позиция.
    Bitmap(Option<(Arc<CoverageBitmap>, Point)>),
}

/// Битмап плашки для `BorderStyle = 3` (libass: ветка `FILTER_BORDER_STYLE_3`).
fn build_box_bitmap(
    request: &GlyphBitmapRequest<'_>,
    phase: &mut RunPhase<'_>,
    base_matrix: &Matrix3,
) -> Result<Option<(Arc<CoverageBitmap>, Point)>, ComposeError> {
    let glyph = request.glyph;
    // Поля плашки: толщина обводки в 1/64 пикселя.
    let mut border_x = 64.0 * request.look.border.horizontal / request.pixel_aspect_x;
    let mut border_y = 64.0 * request.look.border.vertical;
    // Ширина плашки включает межбуквенный интервал (libass: `hspacing_scaled + advance.x`).
    let mut width = f64::from(request.letter_spacing_d6) + f64::from(glyph.advance.x);
    let mut height = f64::from(glyph.ascent + glyph.descent);

    // VSFilter считает размеры плашки с двойным масштабом: размеры, уже умноженные на масштаб
    // глифа, умножаются ещё раз на `scale_x * scale_fix` — то есть на `\fscx`/`\fscy` вида
    // символа (`scale_fix` = 256 / размер шрифта сокращает множитель размера).
    let original_scale = request.look.scale;
    border_x *= original_scale.horizontal;
    border_y *= original_scale.vertical;
    width *= original_scale.horizontal;
    height *= original_scale.vertical;

    // «Чтобы не было зазоров»: минимум одна единица 26.6 на пиксель.
    border_x = border_x.max(64.0);
    border_y = border_y.max(64.0);

    let scale = (
        (width + 2.0 * border_x) / 64.0,
        (height + 2.0 * border_y) / 64.0,
    );
    let offset = (-border_x, -border_y - f64::from(glyph.ascent));
    let mut matrix = *base_matrix;
    for (row, source) in matrix.iter_mut().zip(base_matrix.iter()) {
        row[0] = source[0] * scale.0;
        row[1] = source[1] * scale.1;
        row[2] = source[0] * offset.0 + source[1] * offset.1 + source[2];
    }

    // Единичный квадрат один на кэш контуров (libass: `OUTLINE_BOX`); его рамка - сам квадрат.
    let unit_box = phase.outlines.unit_box()?;
    let Some(key) = quantize_transform(&mut matrix, phase.offset, false, &unit_box.control_box)
    else {
        return Ok(None);
    };
    let bitmap = phase
        .bitmaps
        .mask(phase.rasterizer, &BitmapSource::Outline(unit_box), &key)?;
    Ok(bitmap.map(|bitmap| (bitmap, key.position)))
}

/// Обводка контура глифа (libass: ветка `OUTLINE_BORDER`).
fn build_stroked_bitmap(
    request: &GlyphBitmapRequest<'_>,
    phase: &mut RunPhase<'_>,
    quantized_matrix: &Matrix3,
    scaled_matrix: &Matrix3,
) -> Result<StrokeOutcome, ComposeError> {
    let outline = request.outline;
    // Толщина в единицах контура (контур лежит в масштабе 256 px, глиф уменьшен на scale).
    let border = (
        64.0 * request.look.border.horizontal
            / request.glyph_scale.horizontal
            / request.pixel_aspect_x,
        64.0 * request.look.border.vertical / request.glyph_scale.vertical,
    );
    // Обводка не помещается в допустимый диапазон libass: она отбрасывается (как в libass),
    // но это сбой запрошенной обводки, поэтому он попадает в счётчик.
    let Some(plan) = plan_border_stroke(quantized_matrix, &outline.control_box, border) else {
        tracing::debug!("glyph border is too thick for the outline range, drawing without border");
        return Ok(StrokeOutcome::Failed);
    };
    if plan.border_x == 0 && plan.border_y == 0 {
        return Ok(StrokeOutcome::SameAsFill);
    }
    if outline.outline.is_empty() {
        // libass: у пустого контура обводка пустая (`!outline[0].n_points`).
        return Ok(StrokeOutcome::Bitmap(None));
    }
    // Обводка контура берётся из кэша контуров (в том числе неудача: слишком большой контур или
    // сбой обводчика - глиф остаётся без обводки, libass: «Cannot stroke outline»).
    let Some(stroked) = phase.outlines.stroked(outline, &plan) else {
        return Ok(StrokeOutcome::Failed);
    };
    let mut matrix = border_matrix(scaled_matrix, &plan);
    let Some(key) = quantize_transform(&mut matrix, phase.offset, false, &stroked.control_box)
    else {
        return Ok(StrokeOutcome::Nothing);
    };
    let bitmap = phase.bitmaps.mask(
        phase.rasterizer,
        &BitmapSource::Stroked(Arc::clone(&stroked)),
        &key,
    )?;
    Ok(StrokeOutcome::Bitmap(
        bitmap.map(|bitmap| (bitmap, key.position)),
    ))
}
