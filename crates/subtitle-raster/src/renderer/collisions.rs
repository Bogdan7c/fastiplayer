// Порт из libass 0.17.5: ass_render.c (fit_rect, fix_collisions) в минимальном объёме.
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

//! Разрешение столкновений реплик: налезающие друг на друга реплики раздвигаются по вертикали.
//!
//! Это упрощённый перенос `fix_collisions` libass 0.17.5. Что сохранено:
//! - реплики обрабатываются по порядку списка; первая стоит на месте, каждая следующая
//!   сдвигается ровно настолько, чтобы не налезать на уже поставленные (`fit_rect`);
//! - нижние реплики поднимаются, остальные опускаются (`shift_direction`);
//! - реплики не пересекаются, если их прямоугольники имеют общую площадь (касание не в счёт).
//!
//! Что НЕ перенесено: libass запоминает положение реплик между кадрами (`priv->top` и т. д.),
//! чтобы реплика не прыгала, когда соседняя исчезла. У нас раскладка пересчитывается на
//! каждый кадр по текущему списку: порядок списка стабилен (задаёт вызывающий), поэтому
//! положение детерминировано; «прыжок» возможен только в момент, когда реплика реально
//! исчезла или появилась.
//!
//! Сдвиг ограничен холстом: реплика не уходит за его край (libass в этом случае обрезает
//! картинки, а наш контракт требует целых картинок внутри холста).

use super::cue_render::PixelRect;
use crate::appearance::CollisionDirection;

/// Реплика для разрешения столкновений.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CollisionItem {
    /// Рамка реплики; `None` — ничего не рисует и не участвует.
    pub(crate) extent: Option<PixelRect>,
    /// Куда сдвигать.
    pub(crate) direction: CollisionDirection,
}

/// Вычисляет вертикальный сдвиг каждой реплики (в пикселях; 0 — на месте).
pub(crate) fn resolve_vertical_shifts(items: &[CollisionItem], canvas_height: u32) -> Vec<i32> {
    let canvas_height = i32::try_from(canvas_height).unwrap_or(i32::MAX);
    // Уже поставленные прямоугольники, отсортированные по верхнему краю (libass: `used`).
    let mut used: Vec<PixelRect> = Vec::with_capacity(items.len());
    let mut shifts = Vec::with_capacity(items.len());
    for item in items {
        let Some(extent) = item.extent else {
            shifts.push(0);
            continue;
        };
        let wanted = fit_rect(&extent, &used, item.direction);
        // Не выпускаем реплику за холст: допустимый сдвиг — [-top, высота - bottom].
        let lowest_allowed = extent.top.saturating_neg();
        let highest_allowed = canvas_height.saturating_sub(extent.bottom);
        let shift = wanted.clamp(
            lowest_allowed.min(highest_allowed),
            highest_allowed.max(lowest_allowed),
        );
        used.push(PixelRect {
            top: extent.top.saturating_add(shift),
            bottom: extent.bottom.saturating_add(shift),
            ..extent
        });
        used.sort_by_key(|rect| rect.top);
        shifts.push(shift);
    }
    shifts
}

/// Сдвиг, при котором `rect` не налезает на `used` (libass: `fit_rect`).
///
/// Для сдвига вниз `used` обходится сверху вниз, для сдвига вверх — снизу вверх; сдвиг
/// накапливается, поэтому реплика «перешагивает» цепочку занятых прямоугольников.
fn fit_rect(rect: &PixelRect, used: &[PixelRect], direction: CollisionDirection) -> i32 {
    let mut shift = 0_i32;
    let mut visit = |fixed: &PixelRect| {
        let no_overlap = rect.bottom.saturating_add(shift) <= fixed.top
            || rect.top.saturating_add(shift) >= fixed.bottom
            || rect.right <= fixed.left
            || rect.left >= fixed.right;
        if no_overlap {
            return;
        }
        shift = match direction {
            CollisionDirection::Down => fixed.bottom.saturating_sub(rect.top),
            CollisionDirection::Up => fixed.top.saturating_sub(rect.bottom),
        };
    };
    match direction {
        CollisionDirection::Down => used.iter().for_each(&mut visit),
        CollisionDirection::Up => used.iter().rev().for_each(&mut visit),
    }
    shift
}

#[cfg(test)]
mod tests;
