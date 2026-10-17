// Порт поведения libass 0.17.5: ass_font.c (ass_glyph_italicize, ass_glyph_embolden).
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

//! Синтетические наклон и утолщение контура глифа.
//!
//! libass дорисовывает начертание, которого нет в шрифте: наклоняет контур (курсив) и
//! утолщает его (жирный), НЕ трогая метрики и продвижения. FreeType делает это функциями
//! `FT_Outline_Transform` и `FT_Outline_Embolden`; код FreeType сюда не переносится —
//! утолщение написано заново по описанию алгоритма:
//!
//! > каждая точка контура смещается вдоль биссектрисы нормалей соседних рёбер на
//! > `strength / 2`, так что каждое ребро отодвигается наружу ровно на `strength / 2`
//! > («угловое» смещение, как у обводки с острыми углами); затем весь контур сдвигается на
//! > `strength / 2` вправо и вверх, чтобы начало координат глифа осталось на месте, а сам
//! > глиф вырос на `strength` по обеим осям.
//!
//! Дополнительные правила описания: слишком острые углы (косинус угла между рёбрами ниже
//! -15/16) не смещаются по биссектрисе, а смещение у вогнутых углов ограничено длиной более
//! короткого ребра (иначе короткие рёбра «схлопываются» и контур самопересекается).
//!
//! Работа идёт над готовым [`Outline`] (26.6, ось Y вниз) — после того, как перо шрифта уже
//! округлило точки до 1/64 пикселя, как это делает FreeType перед преобразованием.
//! Отличие от libass: skrifa отдаёт явные «средние» on-curve точки между соседними
//! квадратичными контрольными точками (FreeType хранит их неявными), поэтому у квадратичных
//! шрифтов эти промежуточные точки смещаются отдельно; на форму это влияет на доли пикселя.

use std::ops::Range;

use super::{ControlBox, Outline, OutlineError, Point, SegmentKind};
use crate::fixed_point::mul_fix;

/// Направление обхода внешних контуров глифа в системе координат шрифта (Y вверх).
///
/// В libass это `FT_Orientation`; оно решает, с какой стороны контура «снаружи» и в какую
/// сторону смотрят прямоугольники подчёркивания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OuterWinding {
    /// Внешние контуры по часовой стрелке (`FT_ORIENTATION_TRUETYPE`, глифы `glyf`).
    Clockwise,
    /// Внешние контуры против часовой стрелки (`FT_ORIENTATION_POSTSCRIPT`, глифы CFF).
    CounterClockwise,
    /// У контура нулевая площадь или он схлопнут (`FT_ORIENTATION_NONE`).
    Undefined,
}

/// Точки и порядки сегментов одного контура.
struct ContourLayout {
    /// Индексы точек контура в [`Outline::points`].
    points: Range<usize>,
    /// Порядок каждого сегмента контура по очереди.
    kinds: Vec<SegmentKind>,
}

/// Порог косинуса угла между рёбрами в виде дроби 15/16: острее — смещения по биссектрисе нет.
const SHARP_CORNER_COSINE: f64 = -0.9375;

impl Outline {
    /// Диапазоны индексов точек каждого контура (контур — до сегмента с `ends_contour`).
    ///
    /// Ошибка [`OutlineError::Malformed`], если сегменты не согласованы с точками.
    fn contour_ranges(&self) -> Result<Vec<Range<usize>>, OutlineError> {
        Ok(self
            .contour_layouts()?
            .into_iter()
            .map(|layout| layout.points)
            .collect())
    }

    /// Раскладка контуров: точки и порядки сегментов каждого контура.
    fn contour_layouts(&self) -> Result<Vec<ContourLayout>, OutlineError> {
        let mut layouts = Vec::new();
        let mut contour_start = 0_usize;
        let mut next_point = 0_usize;
        let mut kinds: Vec<SegmentKind> = Vec::new();
        for segment in self.segments() {
            next_point += segment.kind.owned_points();
            kinds.push(segment.kind);
            if segment.ends_contour {
                layouts.push(ContourLayout {
                    points: contour_start..next_point,
                    kinds: std::mem::take(&mut kinds),
                });
                contour_start = next_point;
            }
        }
        if contour_start != next_point || next_point != self.points().len() {
            return Err(OutlineError::Malformed);
        }
        Ok(layouts)
    }

    /// Направление обхода внешних контуров (libass: `FT_Outline_Get_Orientation`).
    ///
    /// Определяется знаком суммарной площади многоугольника из контрольных точек: у глифов
    /// внешние контуры больше внутренних («дыр»), поэтому знак задаёт именно внешние.
    /// Пустой контур — `Clockwise` (как у FreeType). Ось Y у нас вниз, поэтому знак площади
    /// относительно системы шрифта (Y вверх) обратный.
    pub(crate) fn outer_winding(&self) -> OuterWinding {
        if self.points().is_empty() {
            return OuterWinding::Clockwise;
        }
        let mut control_box = ControlBox::EMPTY;
        self.update_control_box(&mut control_box);
        if control_box.x_min == control_box.x_max || control_box.y_min == control_box.y_max {
            return OuterWinding::Undefined;
        }
        let Ok(ranges) = self.contour_ranges() else {
            return OuterWinding::Undefined;
        };
        let mut doubled_area_y_down = 0_i128;
        for range in ranges {
            let Some(points) = self.points().get(range) else {
                continue;
            };
            for (index, current) in points.iter().enumerate() {
                let following = points.get(index + 1).or(points.first()).unwrap_or(current);
                doubled_area_y_down += i128::from(current.x) * i128::from(following.y)
                    - i128::from(following.x) * i128::from(current.y);
            }
        }
        match doubled_area_y_down.cmp(&0) {
            std::cmp::Ordering::Greater => OuterWinding::Clockwise,
            std::cmp::Ordering::Less => OuterWinding::CounterClockwise,
            std::cmp::Ordering::Equal => OuterWinding::Undefined,
        }
    }

    /// Наклоняет контур: `x' = x + shear * y` (libass: `ass_glyph_italicize`).
    ///
    /// `shear_16_16` — коэффициент сдвига в формате 16.16 (`FT_Matrix.xy`), положительный
    /// наклоняет верх вправо. Произведение округляется как `FT_MulFix`.
    pub(crate) fn sheared(&self, shear_16_16: i32) -> Result<Self, OutlineError> {
        let mut result = self.clone();
        for point in &mut result.points {
            // Ось Y контура направлена вниз, а FreeType считает Y вверх.
            let height_above_baseline = i64::from(point.y).saturating_neg();
            let shift = mul_fix(height_above_baseline, i64::from(shear_16_16));
            let moved_x = i64::from(point.x).saturating_add(shift);
            *point = checked_point(moved_x, i64::from(point.y))?;
        }
        Ok(result)
    }

    /// Утолщает контур на `strength_d6` (26.6) по обеим осям без смещения начала координат
    /// (libass: `ass_glyph_embolden` -> `FT_Outline_Embolden`). Описание алгоритма — в
    /// документации модуля.
    pub(crate) fn emboldened(&self, strength_d6: i32) -> Result<Self, OutlineError> {
        let winding = self.outer_winding();
        let mut result = self.clone();
        if winding == OuterWinding::Undefined {
            // FreeType отказывается утолщать контур без определённого направления
            // (`Invalid_Argument`), а libass результат не проверяет: глиф остаётся как есть.
            return Ok(result);
        }
        let half_strength = strength_d6 / 2;
        for layout in self.contour_layouts()? {
            let Some(contour) = self.points().get(layout.points.clone()) else {
                continue;
            };
            let implied = implied_on_curve_points(contour, &layout.kinds);
            // FreeType хранит «средние» on-curve точки между квадратичными контрольными
            // неявно: утолщение двигает только настоящие точки, а средние считаются заново.
            let real: Vec<Point> = contour
                .iter()
                .zip(&implied)
                .filter(|(_, is_implied)| !**is_implied)
                .map(|(point, _)| *point)
                .collect();
            let mut moved_real: Vec<Point> = Vec::with_capacity(real.len());
            for (index, point) in real.iter().enumerate() {
                let shift = corner_shift(&real, index, winding, f64::from(half_strength));
                // Y контура вниз: «вверх» для FreeType — это уменьшение Y.
                let new_x = i64::from(point.x) + i64::from(half_strength) + shift.0.round() as i64;
                let new_y = i64::from(point.y) - i64::from(half_strength) - shift.1.round() as i64;
                moved_real.push(checked_point(new_x, new_y)?);
            }
            let moved = restore_implied_points(&moved_real, &implied)?;
            let slots = result
                .points
                .get_mut(layout.points)
                .ok_or(OutlineError::Malformed)?;
            for (slot, point) in slots.iter_mut().zip(moved) {
                *slot = point;
            }
        }
        Ok(result)
    }
}

/// Отмечает «средние» on-curve точки: начало квадратичного сегмента, идущего сразу за
/// квадратичным, лежащее посередине между соседними контрольными точками (в пределах единицы
/// 26.6 из-за округления). Так skrifa отдаёт неявные точки TrueType, которые FreeType и libass
/// не хранят.
fn implied_on_curve_points(contour: &[Point], kinds: &[SegmentKind]) -> Vec<bool> {
    let count = contour.len();
    let mut implied = vec![false; count];
    let mut start_index = 0_usize;
    let mut previous_kind = kinds.last().copied();
    for kind in kinds {
        let both_quadratic =
            *kind == SegmentKind::Quadratic && previous_kind == Some(SegmentKind::Quadratic);
        if both_quadratic && count > 2 {
            let before = contour.get((start_index + count - 1) % count);
            let after = contour.get((start_index + 1) % count);
            let this = contour.get(start_index);
            if let (Some(before), Some(after), Some(this), Some(flag)) =
                (before, after, this, implied.get_mut(start_index))
            {
                let near_middle = |own: i32, first: i32, second: i32| {
                    (i64::from(own) * 2 - i64::from(first) - i64::from(second)).abs() <= 2
                };
                *flag = near_middle(this.x, before.x, after.x)
                    && near_middle(this.y, before.y, after.y);
            }
        }
        start_index += kind.owned_points();
        previous_kind = Some(*kind);
    }
    implied
}

/// Собирает контур из сдвинутых настоящих точек и заново посчитанных средних (libass:
/// `(a + b) >> 1` в координатах с осью Y вниз).
fn restore_implied_points(
    moved_real: &[Point],
    implied: &[bool],
) -> Result<Vec<Point>, OutlineError> {
    let mut real_points = moved_real.iter();
    let mut slots: Vec<Option<Point>> = Vec::with_capacity(implied.len());
    for is_implied in implied {
        slots.push(if *is_implied {
            None
        } else {
            real_points.next().copied()
        });
    }
    let count = slots.len();
    let mut restored = Vec::with_capacity(count);
    for index in 0..count {
        let point = match slots.get(index).copied().flatten() {
            Some(point) => point,
            None => {
                let before = slots.get((index + count - 1) % count).copied().flatten();
                let after = slots.get((index + 1) % count).copied().flatten();
                let (Some(before), Some(after)) = (before, after) else {
                    return Err(OutlineError::Malformed);
                };
                checked_point(
                    (i64::from(before.x) + i64::from(after.x)) >> 1,
                    (i64::from(before.y) + i64::from(after.y)) >> 1,
                )?
            }
        };
        restored.push(point);
    }
    Ok(restored)
}

/// Точка из координат `i64`; вне диапазона контура — ошибка, а не молчаливое насыщение.
fn checked_point(x: i64, y: i64) -> Result<Point, OutlineError> {
    let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
        return Err(OutlineError::PointOutOfRange);
    };
    let point = Point::new(x, y);
    if point.is_in_outline_range() {
        Ok(point)
    } else {
        Err(OutlineError::PointOutOfRange)
    }
}

/// Единичное направление и длина вектора; `None` для нулевого вектора.
fn direction(dx: f64, dy: f64) -> Option<(f64, f64, f64)> {
    let length = dx.hypot(dy);
    (length > 0.0).then(|| (dx / length, dy / length, length))
}

/// Смещение точки `index` контура вдоль биссектрисы нормалей соседних рёбер в системе
/// координат шрифта (Y вверх); возвращает `(dx, dy)` в единицах 26.6 без округления.
fn corner_shift(
    contour: &[Point],
    index: usize,
    winding: OuterWinding,
    strength: f64,
) -> (f64, f64) {
    let count = contour.len();
    let Some(current) = contour.get(index) else {
        return (0.0, 0.0);
    };
    // Y вверх: переворачиваем знак Y исходных координат.
    let up = |point: &Point| (f64::from(point.x), -f64::from(point.y));
    let (cx, cy) = up(current);
    // Ближайшие отличающиеся точки назад и вперёд (совпадающие точки рёбер не образуют).
    let previous = (1..count)
        .filter_map(|step| contour.get((index + count - step) % count))
        .find(|point| *point != current);
    let following = (1..count)
        .filter_map(|step| contour.get((index + step) % count))
        .find(|point| *point != current);
    let (Some(previous), Some(following)) = (previous, following) else {
        return (0.0, 0.0);
    };
    let (px, py) = up(previous);
    let (fx, fy) = up(following);
    let (Some(incoming), Some(outgoing)) =
        (direction(cx - px, cy - py), direction(fx - cx, fy - cy))
    else {
        return (0.0, 0.0);
    };
    let cosine = incoming.0 * outgoing.0 + incoming.1 * outgoing.1;
    if cosine <= SHARP_CORNER_COSINE {
        return (0.0, 0.0);
    }
    let one_plus_cosine = 1.0 + cosine;
    // Внешняя нормаль ребра: слева от хода для обхода по часовой, справа для обратного.
    let clockwise = winding == OuterWinding::Clockwise;
    let normal = |dir: (f64, f64, f64)| {
        if clockwise {
            (-dir.1, dir.0)
        } else {
            (dir.1, -dir.0)
        }
    };
    let (in_normal, out_normal) = (normal(incoming), normal(outgoing));
    let bisector = (in_normal.0 + out_normal.0, in_normal.1 + out_normal.1);
    // Синус угла поворота со знаком: положителен у вогнутого угла (рёбра сходятся).
    let turn = if clockwise {
        incoming.0 * outgoing.1 - incoming.1 * outgoing.0
    } else {
        outgoing.0 * incoming.1 - outgoing.1 * incoming.0
    };
    let shorter_edge = incoming.2.min(outgoing.2);
    let factor = if strength * turn <= shorter_edge * one_plus_cosine {
        strength / one_plus_cosine
    } else {
        shorter_edge / turn
    };
    (bisector.0 * factor, bisector.1 * factor)
}

#[cfg(test)]
mod tests;
