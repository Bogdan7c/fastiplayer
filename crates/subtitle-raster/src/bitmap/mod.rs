// Порт из libass 0.17.5: ass_bitmap.c, ass_bitmap.h, c/c_blend_bitmaps.c.
//
// Copyright (C) 2016 Vabishchevich Nikolay <vabnick@gmail.com>
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

//! Битмап покрытия и операции над ним (libass 0.17.5: `ass_bitmap.c`, `c/c_blend_bitmaps.c`).
//!
//! [`CoverageBitmap`] — аналог `Bitmap` из libass: 8-битная маска со смещением `left/top` в
//! пикселях, ширина строки (`stride`) кратна 16 (`align_order = 4` C-движка). Размерность
//! растеризации — тайлами 16x16, поэтому ширина и высота растеризованного битмапа кратны 16.
//!
//! Не портировано (волны 3+): размытие (`ass_synth_blur`, гауссиан и box blur) и умножение
//! битмапов (`imul`, `mul`) — они нужны только тегам `\blur`, `\be` и клипам.

#[cfg(test)]
mod tests;

use crate::outline::{Outline, OutlineError};
use crate::raster::{FillWindow, OutlineLayer, RasterError, Rasterizer, TILE_SIZE};

/// Выравнивание строки битмапа (libass: `1 << align_order`, C-движок: 16).
const STRIDE_ALIGNMENT: usize = 16;

/// Почему битмап не построен или операция над ним невозможна.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BitmapError {
    /// Ошибка растеризации или разбора контура.
    #[error(transparent)]
    Raster(#[from] RasterError),
    /// Габариты глифа слишком велики (libass: «Glyph bounding box too large»).
    #[error("glyph bounding box is too large: {width}x{height}")]
    TooLarge {
        /// Ширина в пикселях.
        width: i64,
        /// Высота в пикселях.
        height: i64,
    },
    /// Битмап шире или выше заданного предела (`Rasterizer::max_bitmap_side`): выделять его не
    /// нужно, глиф пропускается вызывающим.
    #[error("bitmap {width}x{height} exceeds the side limit {limit}")]
    ExceedsSideLimit {
        /// Ширина в пикселях.
        width: i64,
        /// Высота в пикселях.
        height: i64,
        /// Предел стороны.
        limit: u32,
    },
    /// Дробный сдвиг вне диапазона `0..64` (1/64 пикселя).
    #[error("fractional shift must be in 0..64, got ({shift_x}, {shift_y})")]
    InvalidShift {
        /// Сдвиг по X.
        shift_x: i32,
        /// Сдвиг по Y.
        shift_y: i32,
    },
    /// Размеры, шаг строки и длина буфера битмапа противоречат друг другу.
    #[error("bitmap geometry is inconsistent with its buffer")]
    InconsistentGeometry,
}

impl From<OutlineError> for BitmapError {
    fn from(error: OutlineError) -> Self {
        Self::Raster(error.into())
    }
}

/// Маска покрытия (libass: `Bitmap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageBitmap {
    /// Левый край в пикселях относительно точки привязки.
    pub(crate) left: i32,
    /// Верхний край в пикселях относительно точки привязки.
    pub(crate) top: i32,
    /// Видимая ширина (libass: `w`), для растеризованных битмапов равна `stride`.
    pub(crate) width: i32,
    /// Высота.
    pub(crate) height: i32,
    /// Шаг строки в байтах (кратен 16, не меньше ширины).
    pub(crate) stride: usize,
    /// Маска покрытия `height * stride` байт.
    pub(crate) buffer: Vec<u8>,
}

impl CoverageBitmap {
    /// Создаёт нулевой битмап `width x height` (libass: `ass_alloc_bitmap` с `zero = true`).
    ///
    /// Шаг строки округляется вверх до 16. Размер буфера ограничен `i32::MAX`, как в libass
    /// («too often we use ints as offset for bitmaps»).
    pub(crate) fn zeroed(width: i32, height: i32) -> Result<Self, BitmapError> {
        let (Ok(width_px), Ok(height_px)) = (usize::try_from(width), usize::try_from(height))
        else {
            return Err(BitmapError::TooLarge {
                width: i64::from(width),
                height: i64::from(height),
            });
        };
        let stride = width_px.div_ceil(STRIDE_ALIGNMENT) * STRIDE_ALIGNMENT;
        let max_bytes = (i32::MAX as usize).saturating_sub(STRIDE_ALIGNMENT);
        let fits = stride
            .checked_mul(height_px)
            .is_some_and(|bytes| bytes <= max_bytes);
        if !fits {
            return Err(BitmapError::TooLarge {
                width: i64::from(width),
                height: i64::from(height),
            });
        }
        Ok(Self {
            left: 0,
            top: 0,
            width,
            height,
            stride,
            buffer: vec![0; stride * height_px],
        })
    }

    /// Строка `row` (`stride` байт) либо `None`, если такой строки нет.
    pub(crate) fn row(&self, row: i32) -> Option<&[u8]> {
        let row = usize::try_from(row).ok()?;
        let start = row.checked_mul(self.stride)?;
        self.buffer.get(start..start.checked_add(self.stride)?)
    }

    /// Проверяет согласованность размеров, шага и длины буфера.
    fn check_geometry(&self) -> Result<(usize, usize), BitmapError> {
        let width = usize::try_from(self.width).map_err(|_| BitmapError::InconsistentGeometry)?;
        let height = usize::try_from(self.height).map_err(|_| BitmapError::InconsistentGeometry)?;
        let required = self.stride.checked_mul(height);
        if self.stride < width || required.is_none_or(|bytes| self.buffer.len() < bytes) {
            return Err(BitmapError::InconsistentGeometry);
        }
        Ok((width, height))
    }
}

/// Растеризует один или два контура в битмап (libass: `ass_outline_to_bitmap`).
///
/// `primary` и `additional` — контуры, рисуемые вместе (например, две границы обводки);
/// любой может отсутствовать. `Ok(None)` — контуры пусты (в libass функция возвращает `false`
/// без ошибки в журнале): рисовать нечего. Рамка расширяется на 1/64 пикселя, чтобы обойти
/// медленный путь растеризатора, и ещё на пиксель под [`shift_bitmap`], затем выравнивается
/// до границ тайла 16x16.
pub(crate) fn outline_to_bitmap(
    rasterizer: &mut Rasterizer,
    primary: Option<&Outline>,
    additional: Option<&Outline>,
) -> Result<Option<CoverageBitmap>, BitmapError> {
    if let Some(outline) = primary {
        rasterizer.set_outline(outline, OutlineLayer::Primary)?;
    }
    if let Some(outline) = additional {
        let layer = if primary.is_some() {
            OutlineLayer::Additional
        } else {
            OutlineLayer::Primary
        };
        rasterizer.set_outline(outline, layer)?;
    }
    let bbox = rasterizer.bounding_box();
    if bbox.is_empty() {
        return Ok(None);
    }

    // Рамка в пикселях: нижняя граница уменьшается на 1/64, верхняя увеличивается на 127/64.
    let x_min = (bbox.x_min - 1) >> 6;
    let y_min = (bbox.y_min - 1) >> 6;
    let x_max = (bbox.x_max + 127) >> 6;
    let y_max = (bbox.y_max + 127) >> 6;
    let width = i64::from(x_max) - i64::from(x_min);
    let height = i64::from(y_max) - i64::from(y_min);

    if let Some(side_limit) = rasterizer.max_bitmap_side()
        && (width > i64::from(side_limit) || height > i64::from(side_limit))
    {
        return Err(BitmapError::ExceedsSideLimit {
            width,
            height,
            limit: side_limit,
        });
    }
    let tile_mask = TILE_SIZE as i64 - 1;
    let limit = i64::from(i32::MAX) - tile_mask;
    if width < 0 || height < 0 || width > limit || height > limit {
        return Err(BitmapError::TooLarge { width, height });
    }
    // Ширина и высота, выровненные вверх до целого числа тайлов (проверено выше: помещаются в i32).
    let tile_width = ((width + tile_mask) & !tile_mask) as i32;
    let tile_height = ((height + tile_mask) & !tile_mask) as i32;

    let mut bitmap = CoverageBitmap::zeroed(tile_width, tile_height)?;
    bitmap.left = x_min;
    bitmap.top = y_min;
    let window = FillWindow {
        x0: x_min,
        y0: y_min,
        width: bitmap.stride,
        height: tile_height as usize,
        stride: bitmap.stride,
    };
    rasterizer.fill(&mut bitmap.buffer, window)?;
    Ok(Some(bitmap))
}

/// Вычитает половину маски глифа из маски обводки (libass: `ass_fix_outline`).
///
/// Так обводка под заливкой выглядит лучше (там, где заливка полупрозрачна). Формула для
/// пересечения битмапов: `outline = outline > glyph ? outline - glyph / 2 : 0`. Обходятся
/// столбцы до `stride` (как в C), а не до видимой ширины.
pub(crate) fn fix_outline(
    glyph: &CoverageBitmap,
    outline: &mut CoverageBitmap,
) -> Result<(), BitmapError> {
    glyph.check_geometry()?;
    outline.check_geometry()?;
    let stride_glyph = glyph.stride as i64;
    let stride_outline = outline.stride as i64;
    let left = i64::from(outline.left.max(glyph.left));
    let top = i64::from(outline.top.max(glyph.top));
    let right =
        (i64::from(outline.left) + stride_outline).min(i64::from(glyph.left) + stride_glyph);
    let bottom = (i64::from(outline.top) + i64::from(outline.height))
        .min(i64::from(glyph.top) + i64::from(glyph.height));
    if right <= left || bottom <= top {
        return Ok(());
    }
    let columns = (right - left) as usize;
    for row in 0..(bottom - top) {
        let glyph_start =
            (top - i64::from(glyph.top) + row) * stride_glyph + (left - i64::from(glyph.left));
        let outline_start = (top - i64::from(outline.top) + row) * stride_outline
            + (left - i64::from(outline.left));
        let (Ok(glyph_start), Ok(outline_start)) =
            (usize::try_from(glyph_start), usize::try_from(outline_start))
        else {
            return Err(BitmapError::InconsistentGeometry);
        };
        let glyph_row = glyph
            .buffer
            .get(glyph_start..glyph_start + columns)
            .ok_or(BitmapError::InconsistentGeometry)?;
        let outline_row = outline
            .buffer
            .get_mut(outline_start..outline_start + columns)
            .ok_or(BitmapError::InconsistentGeometry)?;
        for (outline_value, glyph_value) in outline_row.iter_mut().zip(glyph_row) {
            *outline_value = if *outline_value > *glyph_value {
                *outline_value - glyph_value / 2
            } else {
                0
            };
        }
    }
    Ok(())
}

/// Сдвигает битмап на дробную часть пикселя, выраженную в 26.6 (libass: `ass_shift_bitmap`).
///
/// Часть значения `shift * value / 64` каждого пикселя переносится в соседний справа
/// (затем сверху вниз). Как в C, сложение с соседом выполняется по модулю 256 (`uint8_t`).
pub(crate) fn shift_bitmap(
    bitmap: &mut CoverageBitmap,
    shift_x: i32,
    shift_y: i32,
) -> Result<(), BitmapError> {
    if !(0..64).contains(&shift_x) || !(0..64).contains(&shift_y) {
        return Err(BitmapError::InvalidShift { shift_x, shift_y });
    }
    let (width, height) = bitmap.check_geometry()?;
    let stride = bitmap.stride;
    let shift_x = shift_x as u32;
    let shift_y = shift_y as u32;
    // Доля значения, переносимая соседу: `value * shift >> 6` (результат < value).
    let moved = |value: u8, shift: u32| ((u32::from(value) * shift) >> 6) as u8;

    if shift_x != 0 {
        for row in bitmap.buffer.chunks_mut(stride).take(height) {
            for x in (1..width).rev() {
                let Some(left_value) = row.get(x - 1).copied() else {
                    return Err(BitmapError::InconsistentGeometry);
                };
                let part = moved(left_value, shift_x);
                if let Some(slot) = row.get_mut(x - 1) {
                    *slot = left_value - part;
                }
                if let Some(slot) = row.get_mut(x) {
                    *slot = slot.wrapping_add(part);
                }
            }
        }
    }
    if shift_y != 0 {
        for x in 0..width {
            for y in (1..height).rev() {
                let upper = (y - 1) * stride + x;
                let lower = y * stride + x;
                let Some(upper_value) = bitmap.buffer.get(upper).copied() else {
                    return Err(BitmapError::InconsistentGeometry);
                };
                let part = moved(upper_value, shift_y);
                if let Some(slot) = bitmap.buffer.get_mut(upper) {
                    *slot = upper_value - part;
                }
                if let Some(slot) = bitmap.buffer.get_mut(lower) {
                    *slot = slot.wrapping_add(part);
                }
            }
        }
    }
    Ok(())
}

/// Битмап вместе с позицией привязки (libass: `BitmapRef` — пара `bm` и `pos`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlacedBitmap<'bitmap> {
    /// Сам битмап.
    pub(crate) bitmap: &'bitmap CoverageBitmap,
    /// Позиция точки привязки: полная позиция битмапа = `anchor + (left, top)`.
    pub(crate) anchor: (i32, i32),
}

/// Складывает несколько битмапов в один (часть `ass_composite_construct` без размытия).
///
/// Результат — объединяющий прямоугольник; левый верхний угол записан в `left/top`
/// (абсолютные координаты). Значения складываются с насыщением до 255
/// (`ass_add_bitmaps_c`). Один битмап копируется без перевыделения (как в libass при `be = 0`).
/// Пустой список даёт `None`.
pub(crate) fn combine_bitmaps(
    parts: &[PlacedBitmap<'_>],
) -> Result<Option<CoverageBitmap>, BitmapError> {
    if let [only] = parts {
        let mut copy = only.bitmap.clone();
        copy.left += only.anchor.0;
        copy.top += only.anchor.1;
        return Ok(Some(copy));
    }
    // Объединяющий прямоугольник: [left, right) x [top, bottom).
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for part in parts {
        let left = part.anchor.0 + part.bitmap.left;
        let top = part.anchor.1 + part.bitmap.top;
        let right = left + part.bitmap.width;
        let bottom = top + part.bitmap.height;
        bounds = Some(match bounds {
            None => (left, top, right, bottom),
            Some((l, t, r, b)) => (l.min(left), t.min(top), r.max(right), b.max(bottom)),
        });
    }
    let Some((left, top, right, bottom)) = bounds else {
        return Ok(None);
    };
    let mut combined = CoverageBitmap::zeroed(right - left, bottom - top)?;
    combined.left = left;
    combined.top = top;
    for part in parts {
        add_bitmap(&mut combined, part)?;
    }
    Ok(Some(combined))
}

/// Прибавляет `part` к `destination` с насыщением (libass: `ass_add_bitmaps_c`).
fn add_bitmap(
    destination: &mut CoverageBitmap,
    part: &PlacedBitmap<'_>,
) -> Result<(), BitmapError> {
    let (width, height) = part.bitmap.check_geometry()?;
    let x = i64::from(part.anchor.0 + part.bitmap.left - destination.left);
    let y = i64::from(part.anchor.1 + part.bitmap.top - destination.top);
    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
        return Err(BitmapError::InconsistentGeometry);
    };
    for row in 0..height {
        let source_start = row * part.bitmap.stride;
        let destination_start = (y + row) * destination.stride + x;
        let source = part
            .bitmap
            .buffer
            .get(source_start..source_start + width)
            .ok_or(BitmapError::InconsistentGeometry)?;
        let target = destination
            .buffer
            .get_mut(destination_start..destination_start + width)
            .ok_or(BitmapError::InconsistentGeometry)?;
        for (target_value, source_value) in target.iter_mut().zip(source) {
            *target_value = target_value.saturating_add(*source_value);
        }
    }
    Ok(())
}

/// Строит битмап тени из битмапа-источника (часть `ass_composite_construct`).
///
/// Копия сдвигается на целую часть смещения (`>> 6`, округление к минус бесконечности) и на
/// дробную (`& 63`) через [`shift_bitmap`]. Смещение задано в 26.6.
pub(crate) fn shadow_bitmap(
    source: &CoverageBitmap,
    offset_x: i32,
    offset_y: i32,
) -> Result<CoverageBitmap, BitmapError> {
    let mut shadow = source.clone();
    shadow.left += offset_x >> 6;
    shadow.top += offset_y >> 6;
    shift_bitmap(&mut shadow, offset_x & 63, offset_y & 63)?;
    Ok(shadow)
}
