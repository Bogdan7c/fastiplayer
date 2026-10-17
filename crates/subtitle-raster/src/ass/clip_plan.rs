// Порт из libass 0.17.5: ass_render.c (ass_render_event: «fix clip coordinates»,
// blend_vector_clip: построение матрицы).
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

//! Перевод `\clip` / `\iclip` реплики из координат сценария в экранные величины.
//!
//! Это единственное место, где клип знает про `PlayRes` и масштаб кадра: дальше стадия
//! [`crate::clip`] работает только с пикселями холста.

use super::drawing_scale::coordinate_weight;
use super::filter_params::{FilterParams, RectClip, VectorClip};
use super::frame_scale::{AssFrameScale, EventScale};
use crate::clip::{ScreenRectClip, VectorClipPlan};
use crate::overlay::CanvasSize;

/// Клипы реплики в экранных величинах (до растеризации маски).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EventClipPlan {
    /// Прямоугольный клип, если автор его задал.
    pub(crate) rect: Option<ScreenRectClip>,
    /// Векторный клип, если автор его задал.
    pub(crate) vector: Option<VectorClipPlan>,
}

impl EventClipPlan {
    /// Строит план по итоговому состоянию тегов реплики (клип действует на всю реплику, поэтому
    /// берётся состояние в конце разбора, как `state->clip_*` у libass).
    ///
    /// `explicit` — реплика «явная»: тогда углы зажимаются в кадр. `scale` — множители кадра
    /// этой реплики (`screen_scale_x/y` с учётом пользовательского масштаба текста, как у libass).
    pub(crate) fn from_filters(
        filters: &FilterParams,
        frame: &AssFrameScale,
        scale: &EventScale,
        canvas: CanvasSize,
        explicit: bool,
    ) -> Self {
        Self {
            rect: filters
                .rect_clip()
                .map(|clip| screen_rect(clip, frame, canvas, explicit)),
            vector: filters.vector_clip().map(|clip| vector_plan(clip, scale)),
        }
    }
}

/// Углы прямоугольника в пикселях холста (libass: `lround(x2scr_pos_scaled(...))`,
/// `lround(y2scr_pos(...))` и зажим в кадр у явных реплик).
fn screen_rect(
    clip: RectClip,
    frame: &AssFrameScale,
    canvas: CanvasSize,
    explicit: bool,
) -> ScreenRectClip {
    // lround: округление от нуля; `as i32` насыщается, NaN невозможен (углы — целые).
    let to_x = |x: i32| {
        frame
            .x_to_screen_ignoring_pixel_aspect(f64::from(x))
            .round() as i32
    };
    let to_y = |y: i32| frame.y_to_screen(f64::from(y)).round() as i32;
    let (mut x0, mut y0) = (to_x(clip.x0), to_y(clip.y0));
    let (mut x1, mut y1) = (to_x(clip.x1), to_y(clip.y1));
    if explicit {
        // «We still need to clip against screen boundaries»: каждый угол отдельно.
        x0 = x0.max(0);
        y0 = y0.max(0);
        x1 = x1.min(i32::try_from(canvas.width()).unwrap_or(i32::MAX));
        y1 = y1.min(i32::try_from(canvas.height()).unwrap_or(i32::MAX));
    }
    ScreenRectClip::new(clip.mode, [x0, y0, x1, y1], canvas)
}

/// Матрица фигуры: масштаб кадра на `2^-(scale-1)` (libass: `blend_vector_clip`).
fn vector_plan(clip: &VectorClip, scale: &EventScale) -> VectorClipPlan {
    // `scale` — int32 из тега; показатель берётся по модулю 32, как у `\p`.
    let weight = coordinate_weight(clip.scale as u32);
    VectorClipPlan {
        mode: clip.mode,
        drawing: clip.drawing.clone(),
        scale_x: scale.screen_x * weight,
        scale_y: scale.screen_y * weight,
    }
}

#[cfg(test)]
mod tests;
