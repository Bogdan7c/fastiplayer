// Порт из libass 0.17.5: ass_render.c (render_glyph, render_glyph_i, «fix clip coordinates» в
// ass_render_event).
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

//! Прямоугольный клип `\clip(x0,y0,x1,y1)` / `\iclip(...)`: геометрия на экране и обрезка картинок.
//!
//! Прямоугольник уже переведён в пиксели холста (`lround(x2scr_pos_scaled)` и `lround(y2scr_pos)`,
//! зажат в кадр — это делает планировщик реплики, см. `ass::clip_plan`). Углы НЕ упорядочены:
//! так их хранит libass, и перевёрнутый прямоугольник даёт пустой обычный клип и «ничего не
//! скрывающий» инверсный.
//!
//! Обычный клип оставляет у каждой картинки одну часть (`render_glyph`), инверсный режет её на
//! до четырёх кусков вокруг «дыры» (`render_glyph_i`): слева, над дырой, под дырой, справа.
//! Порядок кусков важен: он определяет порядок картинок в списке, который сравнивает паритет.

use subtitle_core::AssClipMode;

use super::paint::ImagePaint;
use crate::overlay::{CanvasSize, LocalRect, SubtitleBitmap};

/// Прямоугольный клип в пикселях холста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScreenRectClip {
    mode: AssClipMode,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    /// Холст: обычный клип дополнительно зажимается в кадр (`render_glyph`).
    canvas: CanvasSize,
}

impl ScreenRectClip {
    /// Клип с углами `(x0, y0)` и `(x1, y1)` в пикселях холста (порядок углов не важен для
    /// хранения: он сохраняется как есть).
    pub(crate) fn new(mode: AssClipMode, corners: [i32; 4], canvas: CanvasSize) -> Self {
        let [x0, y0, x1, y1] = corners;
        Self {
            mode,
            x0,
            y0,
            x1,
            y1,
            canvas,
        }
    }

    /// Клип на весь холст (клип по умолчанию явной реплики без `\clip`).
    pub(crate) fn whole_canvas(canvas: CanvasSize) -> Self {
        let corners = [
            0,
            0,
            i32::try_from(canvas.width()).unwrap_or(i32::MAX),
            i32::try_from(canvas.height()).unwrap_or(i32::MAX),
        ];
        Self::new(AssClipMode::Inside, corners, canvas)
    }

    /// Зажимает вертикальные границы в полосу `[top, bottom]` (libass: `clip_y0 = FFMAX(clip_y0,
    /// y0)`, `clip_y1 = FFMIN(clip_y1, y1)` у `Scroll up/down`). Режим клипа не меняется: у
    /// `\iclip` libass тоже просто сдвигает границы дыры.
    pub(crate) fn limited_to_rows(self, top: i32, bottom: i32) -> Self {
        Self {
            y0: self.y0.max(top),
            y1: self.y1.min(bottom),
            ..self
        }
    }

    /// Куски картинки, которые остаются видимыми, в порядке libass; кладёт их в `out`.
    ///
    /// Картинка уже лежит внутри холста (её обрезала сборка реплики), поэтому «зажим кусков в
    /// экран» из `render_glyph_i` здесь не нужен: он ничего бы не изменил. `paint` — раскраска
    /// караоке: каждый видимый кусок режется по `brk` на левую и правую части внутри того же
    /// шага, как в libass (для инверсного клипа порядок «кусок 1 слева, кусок 1 справа,
    /// кусок 2 слева ...»).
    pub(crate) fn push_visible_parts(
        &self,
        image: &SubtitleBitmap,
        paint: ImagePaint,
        out: &mut Vec<SubtitleBitmap>,
    ) {
        let origin = image.origin();
        let bounds = ImageBounds {
            dst_x: i64::from(origin.x),
            dst_y: i64::from(origin.y),
            width: i64::from(image.width()),
            height: i64::from(image.height()),
        };
        match self.mode {
            AssClipMode::Inside => {
                if let Some(region) = self.inside_region(bounds) {
                    paint.push_region(image, region, out);
                }
            }
            AssClipMode::Outside => {
                for region in self.outside_regions(bounds) {
                    paint.push_region(image, region, out);
                }
            }
        }
    }

    /// Видимая часть при обычном клипе (libass: `render_glyph`); `None` — ничего не осталось.
    fn inside_region(&self, bounds: ImageBounds) -> Option<LocalRect> {
        let canvas_width = i64::from(self.canvas.width());
        let canvas_height = i64::from(self.canvas.height());
        // FFMINMAX(clip, 0, размер кадра).
        let clip_x0 = i64::from(self.x0).clamp(0, canvas_width);
        let clip_y0 = i64::from(self.y0).clamp(0, canvas_height);
        let clip_x1 = i64::from(self.x1).clamp(0, canvas_width);
        let clip_y1 = i64::from(self.y1).clamp(0, canvas_height);
        let left = (clip_x0 - bounds.dst_x).max(0);
        let top = (clip_y0 - bounds.dst_y).max(0);
        let right = (clip_x1 - bounds.dst_x).min(bounds.width);
        let bottom = (clip_y1 - bounds.dst_y).min(bounds.height);
        local_rect(left, top, right, bottom)
    }

    /// Видимые куски при инверсном клипе (libass: `render_glyph_i`), от одного до четырёх.
    fn outside_regions(&self, bounds: ImageBounds) -> Vec<LocalRect> {
        // Дыра в координатах картинки (углы не упорядочены, как в libass).
        let hole_x0 = i64::from(self.x0) - bounds.dst_x;
        let hole_y0 = i64::from(self.y0) - bounds.dst_y;
        let hole_x1 = i64::from(self.x1) - bounds.dst_x;
        let hole_y1 = i64::from(self.y1) - bounds.dst_y;
        let (width, height) = (bounds.width, bounds.height);
        let candidates = [
            // Слева от дыры, на всю высоту.
            (0, 0, hole_x0.min(width), height),
            // Над дырой.
            (
                if hole_x0 < 0 { 0 } else { hole_x0 },
                0,
                hole_x1.min(width),
                hole_y0.min(height),
            ),
            // Под дырой.
            (
                if hole_x0 < 0 { 0 } else { hole_x0 },
                if hole_y1 < 0 { 0 } else { hole_y1 },
                hole_x1.min(width),
                height,
            ),
            // Справа от дыры, на всю высоту.
            (if hole_x1 < 0 { 0 } else { hole_x1 }, 0, width, height),
        ];
        candidates
            .into_iter()
            .filter_map(|(left, top, right, bottom)| local_rect(left, top, right, bottom))
            .collect()
    }
}

/// Положение и размер картинки на холсте.
#[derive(Debug, Clone, Copy)]
struct ImageBounds {
    dst_x: i64,
    dst_y: i64,
    width: i64,
    height: i64,
}

/// Непустой прямоугольник внутри картинки; `None`, если он пуст или вывернут.
fn local_rect(left: i64, top: i64, right: i64, bottom: i64) -> Option<LocalRect> {
    if right <= left || bottom <= top {
        return None;
    }
    Some(LocalRect {
        x0: u32::try_from(left).ok()?,
        y0: u32::try_from(top).ok()?,
        x1: u32::try_from(right).ok()?,
        y1: u32::try_from(bottom).ok()?,
    })
}

#[cfg(test)]
mod tests;
