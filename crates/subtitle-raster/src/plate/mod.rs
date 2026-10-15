//! Плашка стиля «как YouTube»: скруглённый прямоугольник за каждой строкой реплики.
//!
//! Это НЕ `BorderStyle` 3/4 из libass, а расширение Fastiplayer. Алгоритм:
//! 1. вокруг плотной рамки каждой строки строится скруглённый прямоугольник
//!    (кривые Безье, в координатах 26.6 как контуры глифов);
//! 2. контур растеризуется портированным растеризатором libass (тот же код, что для глифов,
//!    поэтому сглаживание краёв одинаковое);
//! 3. маски всех строк объединяются поэлементным максимумом в ОДНУ маску на реплику.
//!    Максимум, а не сложение и не наложение: на стыке строк, где прямоугольники
//!    перекрываются, плашка не темнеет вдвое, а её цвет и прозрачность задаются один раз.

use std::sync::Arc;

use crate::appearance::PlateStyle;
use crate::bitmap::{BitmapError, CoverageBitmap, outline_to_bitmap};
use crate::glyph_transform::RASTERIZER_PRECISION;
use crate::layout::LineBox;
use crate::outline::{Outline, OutlineError, Point, SegmentKind};
use crate::overlay::{
    CanvasSize, PixelPoint, SubtitleBitmap, SubtitleBitmapError, SubtitleBitmapKind,
};
use crate::raster::Rasterizer;

/// Почему плашку не удалось построить.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PlateError {
    /// Контур прямоугольника вне допустимого диапазона координат.
    #[error(transparent)]
    Outline(#[from] OutlineError),
    /// Сбой растеризации.
    #[error(transparent)]
    Bitmap(#[from] BitmapError),
    /// Итоговая картинка нарушает контракт выхода.
    #[error(transparent)]
    Image(#[from] SubtitleBitmapError),
}

/// Доля радиуса, на которой лежат контрольные точки кубической Безье, приближающей
/// четверть окружности (4/3 · (√2 − 1)).
const QUARTER_CIRCLE_HANDLE: f64 = 0.552_284_749_830_793_4;

/// Масштаб 26.6: 64 единицы на пиксель.
const SUBPIXEL_SCALE: f64 = 64.0;

/// Прямоугольник в пикселях холста.
#[derive(Debug, Clone, Copy)]
struct PlateRect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

/// Строит единую маску плашки реплики; `None` — рисовать нечего (нет строк, прозрачный
/// цвет или плашка целиком за холстом).
pub(crate) fn build_plate(
    lines: &[LineBox],
    style: &PlateStyle,
    canvas: CanvasSize,
) -> Result<Option<SubtitleBitmap>, PlateError> {
    if style.color.alpha == 0 || lines.is_empty() {
        return Ok(None);
    }
    let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
    let mut line_masks = Vec::with_capacity(lines.len());
    for line in lines {
        let rect = PlateRect {
            left: line.left - style.horizontal_padding,
            right: line.right + style.horizontal_padding,
            top: line.top - style.vertical_padding,
            bottom: line.bottom + style.vertical_padding,
        };
        let outline = rounded_rect_outline(rect, style.corner_radius)?;
        if let Some(mask) = outline_to_bitmap(&mut rasterizer, Some(&outline), None)? {
            line_masks.push(mask);
        }
    }
    let Some(merged) = merge_by_maximum(&line_masks, canvas).and_then(crop_to_ink) else {
        return Ok(None);
    };
    let image = SubtitleBitmap::new(
        PixelPoint {
            x: merged.left,
            y: merged.top,
        },
        merged.width,
        merged.height,
        merged.width as usize,
        Arc::from(merged.pixels),
        style.color,
        SubtitleBitmapKind::Background,
    )?;
    Ok(Some(image))
}

/// Скруглённый прямоугольник одним замкнутым контуром: 4 отрезка + 4 кубические кривые.
///
/// Обход по часовой стрелке в координатах «Y вниз» — то же направление, что у
/// `Outline::add_rect`, поэтому растеризатору (правило ненулевой намотки) он подходит.
/// Радиус ограничен половиной меньшей стороны; нулевой радиус даёт обычный прямоугольник.
fn rounded_rect_outline(rect: PlateRect, radius: f64) -> Result<Outline, OutlineError> {
    let to_units = |value: f64| (value * SUBPIXEL_SCALE).round() as i32;
    let (left, top) = (to_units(rect.left), to_units(rect.top));
    let (right, bottom) = (to_units(rect.right), to_units(rect.bottom));
    let mut outline = Outline::new();
    if right <= left || bottom <= top {
        return Ok(outline);
    }
    let max_radius = f64::from((right - left).min(bottom - top)) / 2.0;
    let radius_units = (radius * SUBPIXEL_SCALE).round().clamp(0.0, max_radius) as i32;
    if radius_units == 0 {
        outline.add_rect(left, top, right, bottom)?;
        return Ok(outline);
    }
    let handle = (f64::from(radius_units) * QUARTER_CIRCLE_HANDLE).round() as i32;
    let r = radius_units;
    let p = Point::new;

    // Каждый сегмент владеет своей начальной точкой (и контрольными точками кривой);
    // конец сегмента — начало следующего, у последнего — начало первого.
    // Верхняя сторона слева направо.
    push_line(&mut outline, p(left + r, top))?;
    // Правый верхний угол.
    push_corner(
        &mut outline,
        [
            p(right - r, top),
            p(right - r + handle, top),
            p(right, top + r - handle),
        ],
    )?;
    // Правая сторона сверху вниз.
    push_line(&mut outline, p(right, top + r))?;
    // Правый нижний угол.
    push_corner(
        &mut outline,
        [
            p(right, bottom - r),
            p(right, bottom - r + handle),
            p(right - r + handle, bottom),
        ],
    )?;
    // Нижняя сторона справа налево.
    push_line(&mut outline, p(right - r, bottom))?;
    // Левый нижний угол.
    push_corner(
        &mut outline,
        [
            p(left + r, bottom),
            p(left + r - handle, bottom),
            p(left, bottom - r + handle),
        ],
    )?;
    // Левая сторона снизу вверх.
    push_line(&mut outline, p(left, bottom - r))?;
    // Левый верхний угол замыкает контур в (left + r, top).
    push_corner(
        &mut outline,
        [
            p(left, top + r),
            p(left, top + r - handle),
            p(left + r - handle, top),
        ],
    )?;
    if !outline.close_contour() {
        return Err(OutlineError::Malformed);
    }
    Ok(outline)
}

/// Добавляет отрезок, начинающийся в `start`.
fn push_line(outline: &mut Outline, start: Point) -> Result<(), OutlineError> {
    outline.push_point(start)?;
    outline.push_segment(SegmentKind::Line);
    Ok(())
}

/// Добавляет кубическую кривую: начальная точка и две контрольные.
fn push_corner(outline: &mut Outline, points: [Point; 3]) -> Result<(), OutlineError> {
    for point in points {
        outline.push_point(point)?;
    }
    outline.push_segment(SegmentKind::Cubic);
    Ok(())
}

/// Результат объединения масок: пиксели с плотным шагом `width` и положение на холсте.
struct MergedMask {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Объединяет маски поэлементным максимумом, обрезая результат по холсту.
fn merge_by_maximum(masks: &[CoverageBitmap], canvas: CanvasSize) -> Option<MergedMask> {
    let canvas_width = i64::from(canvas.width());
    let canvas_height = i64::from(canvas.height());
    // Общая рамка масок, пересечённая с холстом.
    let left = masks.iter().map(|mask| i64::from(mask.left)).min()?.max(0);
    let top = masks.iter().map(|mask| i64::from(mask.top)).min()?.max(0);
    let right = masks
        .iter()
        .map(|mask| i64::from(mask.left) + i64::from(mask.width))
        .max()?
        .min(canvas_width);
    let bottom = masks
        .iter()
        .map(|mask| i64::from(mask.top) + i64::from(mask.height))
        .max()?
        .min(canvas_height);
    if left >= right || top >= bottom {
        return None;
    }
    let width = usize::try_from(right - left).ok()?;
    let height = usize::try_from(bottom - top).ok()?;
    let mut pixels = vec![0_u8; width.checked_mul(height)?];

    for mask in masks {
        for row in 0..mask.height {
            let target_y = i64::from(mask.top) + i64::from(row);
            if target_y < top || target_y >= bottom {
                continue;
            }
            let Some(source_row) = mask.row(row) else {
                continue;
            };
            let first_x = i64::from(mask.left).max(left);
            let last_x = (i64::from(mask.left) + i64::from(mask.width)).min(right);
            if first_x >= last_x {
                continue;
            }
            let source_start = usize::try_from(first_x - i64::from(mask.left)).ok()?;
            let source_len = usize::try_from(last_x - first_x).ok()?;
            let target_start = usize::try_from(target_y - top).ok()? * width
                + usize::try_from(first_x - left).ok()?;
            let source = source_row.get(source_start..source_start + source_len)?;
            let target = pixels.get_mut(target_start..target_start + source_len)?;
            for (slot, value) in target.iter_mut().zip(source) {
                *slot = (*slot).max(*value);
            }
        }
    }
    Some(MergedMask {
        left: i32::try_from(left).ok()?,
        top: i32::try_from(top).ok()?,
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        pixels,
    })
}

/// Отрезает пустые поля маски (растеризатор округляет рамку до тайлов 16x16), чтобы картинка
/// занимала ровно то, что закрашено. `None` — маска целиком пуста.
fn crop_to_ink(mask: MergedMask) -> Option<MergedMask> {
    let width = mask.width as usize;
    let mut first_row = usize::MAX;
    let mut last_row = 0;
    let mut first_column = usize::MAX;
    let mut last_column = 0;
    for (row_index, row) in mask.pixels.chunks_exact(width).enumerate() {
        let Some(first) = row.iter().position(|value| *value != 0) else {
            continue;
        };
        let last = row.iter().rposition(|value| *value != 0).unwrap_or(first);
        first_row = first_row.min(row_index);
        last_row = row_index;
        first_column = first_column.min(first);
        last_column = last_column.max(last);
    }
    if first_row == usize::MAX {
        return None;
    }
    let cropped_width = last_column - first_column + 1;
    let cropped_height = last_row - first_row + 1;
    let mut pixels = Vec::with_capacity(cropped_width * cropped_height);
    for row in mask
        .pixels
        .chunks_exact(width)
        .skip(first_row)
        .take(cropped_height)
    {
        pixels.extend_from_slice(row.get(first_column..=last_column)?);
    }
    Some(MergedMask {
        left: mask.left.checked_add(i32::try_from(first_column).ok()?)?,
        top: mask.top.checked_add(i32::try_from(first_row).ok()?)?,
        width: u32::try_from(cropped_width).ok()?,
        height: u32::try_from(cropped_height).ok()?,
        pixels,
    })
}

#[cfg(test)]
mod tests;
