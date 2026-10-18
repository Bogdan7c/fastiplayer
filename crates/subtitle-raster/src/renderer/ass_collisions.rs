// Порт из libass 0.17.5: ass_render.c (overlap, cmp_rect_y0, fit_rect, fix_collisions,
// get_render_priv — память положений реплик между кадрами).
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

//! Столкновения реплик ASS: по слоям, с памятью положений между кадрами.
//!
//! Отличия от простого разрешения столкновений текстовых реплик (`collisions`):
//! - реплики делятся на группы одного слоя (`Layer`), внутри группы столкновения разрешаются
//!   независимо (libass: `fix_collisions` на каждую группу);
//! - рамка реплики считается по метрикам текста, а не по картинкам;
//! - положение, в котором реплика однажды встала, ЗАПОМИНАЕТСЯ (libass: `priv->top/height`):
//!   пока соседняя реплика исчезла, остальные не «прыгают» обратно. Память держится, пока
//!   реплика активна, сбрасывается при смене холста или настроек ASS, разрыве времени (seek/scrub) и при изменении высоты
//!   реплики;
//! - реплики с `detect = false` (`\pos` и др.) не двигаются и в расчёте не участвуют.
//!
//! Память ограничена числом реплик текущего кадра: после каждого расчёта удаляются записи
//! реплик, которых в кадре нет.

use std::collections::HashMap;

use subtitle_core::SubtitleLayer;

use super::cue_render::PixelRect;
use super::request::CueKey;
use crate::appearance::CollisionDirection;
use crate::ass::AssRenderOptions;
use crate::overlay::CanvasSize;

/// Реплика ASS для разрешения столкновений. Список должен быть отсортирован по
/// `(layer, ReadOrder)`: группы слоя идут подряд.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AssCollisionItem {
    /// Идентичность реплики (ключ памяти положений).
    pub(crate) key: CueKey,
    /// Слой.
    pub(crate) layer: SubtitleLayer,
    /// Рамка реплики; `None` — реплика не участвует (нет места или `detect = false`).
    pub(crate) rect: Option<PixelRect>,
    /// Куда сдвигать.
    pub(crate) direction: CollisionDirection,
}

/// Запомненное положение реплики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredRect {
    rect: PixelRect,
}

/// Память положений и условия, при которых она действительна.
#[derive(Debug, Default)]
pub(crate) struct AssCollisionMemory {
    /// Холст и настройки, для которых записаны положения (libass: `render_id`).
    context: Option<(CanvasSize, AssRenderOptions)>,
    stored: HashMap<CueKey, StoredRect>,
}

impl AssCollisionMemory {
    /// Сколько положений помнит (для тестов и статистики).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.stored.len()
    }

    /// Забывает всё: смена шрифтов (рамки реплик могли измениться) и разрыв времени
    /// (`SubtitleRasterizer::forget_layout_history`: после прыжка положение реплик в цели не
    /// должно зависеть от пути, которым пришли).
    pub(crate) fn clear(&mut self) {
        self.stored.clear();
        self.context = None;
    }

    /// Считает вертикальный сдвиг каждой реплики (в пикселях; 0 — на месте) и обновляет память.
    pub(crate) fn resolve(
        &mut self,
        items: &[AssCollisionItem],
        canvas: CanvasSize,
        options: &AssRenderOptions,
    ) -> Vec<i32> {
        // Другой холст или другие настройки — другая раскладка: прежние положения недействительны.
        if self.context != Some((canvas, *options)) {
            self.stored.clear();
            self.context = Some((canvas, *options));
        }
        let mut shifts = vec![0_i32; items.len()];
        let mut group_start = 0;
        while group_start < items.len() {
            let layer = items.get(group_start).map(|item| item.layer);
            let group_end = items
                .iter()
                .enumerate()
                .skip(group_start)
                .find(|(_, item)| Some(item.layer) != layer)
                .map_or(items.len(), |(index, _)| index);
            let group = items.get(group_start..group_end).unwrap_or(&[]);
            let group_shifts = shifts.get_mut(group_start..group_end);
            if let Some(group_shifts) = group_shifts {
                self.resolve_group(group, group_shifts);
            }
            group_start = group_end;
        }
        // Память не растёт: оставляем только реплики этого кадра.
        let active: std::collections::HashSet<CueKey> = items.iter().map(|item| item.key).collect();
        self.stored.retain(|key, _| active.contains(key));
        shifts
    }

    /// Одна группа слоя (libass: `fix_collisions`).
    fn resolve_group(&mut self, group: &[AssCollisionItem], shifts: &mut [i32]) {
        let mut used: Vec<PixelRect> = Vec::with_capacity(group.len());
        let mut fixed = vec![false; group.len()];

        // Фаза 1: реплики с запомненным положением остаются на нём, если оно ещё годится.
        for (index, item) in group.iter().enumerate() {
            let Some(rect) = item.rect else { continue };
            let Some(stored) = self.stored.get(&item.key).copied() else {
                continue;
            };
            let same_height = stored.rect.bottom - stored.rect.top == rect.bottom - rect.top;
            let free = used.iter().all(|other| !overlaps(&stored.rect, other));
            if same_height && free {
                used.push(stored.rect);
                fixed[index] = true;
                if let Some(shift) = shifts.get_mut(index) {
                    *shift = stored.rect.top.saturating_sub(rect.top);
                }
            } else {
                // Высота изменилась или место занято: положение забывается.
                self.stored.remove(&item.key);
            }
        }
        used.sort_by_key(|rect| rect.top);

        // Фаза 2: остальные ищут свободное место и запоминают его.
        for (index, item) in group.iter().enumerate() {
            let Some(rect) = item.rect else { continue };
            if fixed.get(index).copied().unwrap_or(true) {
                continue;
            }
            let shift = fit_rect(&rect, &mut used, item.direction);
            if let Some(slot) = shifts.get_mut(index) {
                *slot = shift;
            }
            self.stored.insert(
                item.key,
                StoredRect {
                    rect: PixelRect {
                        top: rect.top.saturating_add(shift),
                        bottom: rect.bottom.saturating_add(shift),
                        ..rect
                    },
                },
            );
        }
    }
}

/// Пересекаются ли рамки по площади (касание не в счёт; libass: `overlap`).
fn overlaps(first: &PixelRect, second: &PixelRect) -> bool {
    !(first.top >= second.bottom
        || second.top >= first.bottom
        || first.left >= second.right
        || second.left >= first.right)
}

/// Сдвиг, при котором рамка не налезает на занятые (libass: `fit_rect`); итоговая рамка
/// добавляется в `used`, список остаётся отсортированным по верхнему краю.
fn fit_rect(rect: &PixelRect, used: &mut Vec<PixelRect>, direction: CollisionDirection) -> i32 {
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
    used.push(PixelRect {
        top: rect.top.saturating_add(shift),
        bottom: rect.bottom.saturating_add(shift),
        ..*rect
    });
    used.sort_by_key(|placed| placed.top);
    shift
}

#[cfg(test)]
mod tests;
