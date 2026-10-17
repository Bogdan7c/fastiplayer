// Порт из libass 0.17.5: ass_parse.c (ветки `blur`, `be`, `clip`, `iclip`, `pbo`, dtoi32).
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

//! Параметры тегов слоя 3 в состоянии реплики: размытие, клипы, смещение рисунка.
//!
//! Модуль владеет правилами «какое значение сохраняет тег» (зажатие, округление, кто побеждает
//! при повторах). Сами эффекты (размытие, обрезка, рисунки) рисуют другие модули, читая
//! готовые значения через методы [`FilterParams`].
//!
//! Парсер хранит сырые значения: `\t` (анимация) интерполирует их ДО зажатия, поэтому
//! зажатие принадлежит применению тега, а не разбору.

use subtitle_core::{
    ASS_BLUR_MAX_RADIUS, ASS_EDGE_BLUR_MAX, AssClip, AssClipMode, AssClipShape, AssOverride,
    AssPlayRes,
};

use super::tag_weight::TagWeight;
use super::transform_apply::TagSink;

/// Разрешение сценария, которое libass подставляет, когда `PlayRes` не задан (`384x288`):
/// умолчание для [`FilterParams::default`].
const FALLBACK_PLAY_RES: AssPlayRes = AssPlayRes {
    width: 384,
    height: 288,
};

/// Прямоугольный клип `\clip(x0,y0,x1,y1)` / `\iclip(...)` в координатах `PlayRes`.
///
/// Порядок углов не нормализуется: так хранит libass (`clip_x0/y0/x1/y1` и `clip_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RectClip {
    pub(crate) mode: AssClipMode,
    pub(crate) x0: i32,
    pub(crate) y0: i32,
    pub(crate) x1: i32,
    pub(crate) y1: i32,
}

/// Векторный клип: текст рисунка и масштаб его координат (`clip_drawing_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorClip {
    pub(crate) mode: AssClipMode,
    /// Степень точности координат, как у `\p`: делитель `2^(scale-1)`.
    pub(crate) scale: i32,
    pub(crate) drawing: String,
}

/// Значения тегов `\blur`, `\be`, `\clip`/`\iclip`, `\pbo` в текущей точке реплики.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FilterParams {
    /// Разрешение сценария: от него считается исходный прямоугольник клипа, из которого
    /// `\t(\clip(...))` плавно переходит к новому (`clip_x1 = track->PlayResX`).
    play_res: AssPlayRes,
    blur_radius: f64,
    edge_blur: i32,
    rect_clip: Option<RectClip>,
    vector_clip: Option<VectorClip>,
    baseline_offset: f64,
}

impl Default for FilterParams {
    fn default() -> Self {
        Self::for_play_res(FALLBACK_PLAY_RES)
    }
}

impl FilterParams {
    /// Параметры без тегов для сценария с разрешением `play_res`.
    pub(crate) fn for_play_res(play_res: AssPlayRes) -> Self {
        Self {
            play_res,
            blur_radius: 0.0,
            edge_blur: 0,
            rect_clip: None,
            vector_clip: None,
            baseline_offset: 0.0,
        }
    }

    /// Радиус гауссова размытия в единицах сценария, уже в `[0, ASS_BLUR_MAX_RADIUS]`.
    pub(crate) fn blur_radius(&self) -> f64 {
        self.blur_radius
    }

    /// Число проходов `\be`, уже в `[0, ASS_EDGE_BLUR_MAX]`.
    pub(crate) fn edge_blur(&self) -> i32 {
        self.edge_blur
    }

    /// Прямоугольный клип; `None` — тега не было (libass тогда берёт весь `PlayRes`).
    pub(crate) fn rect_clip(&self) -> Option<RectClip> {
        self.rect_clip
    }

    /// Векторный клип; `None` — тега не было.
    pub(crate) fn vector_clip(&self) -> Option<&VectorClip> {
        self.vector_clip.as_ref()
    }

    /// Смещение базовой линии рисунка `\pbo` как написано (libass усекает до целого при
    /// создании глифа).
    pub(crate) fn baseline_offset(&self) -> f64 {
        self.baseline_offset
    }

    /// `\r`: сбрасывает только размытие (`state->be = 0; state->blur = style->Blur`, а у
    /// стилей из файла `Blur` равен 0). Клипы и `\pbo` `\r` не трогает.
    pub(crate) fn reset_blur(&mut self) {
        self.blur_radius = 0.0;
        self.edge_blur = 0;
    }

    /// Применяет тег слоя 3 с полным весом (тег вне `\t`).
    pub(crate) fn apply(&mut self, tag: &AssOverride) {
        self.apply_weighted(tag, TagWeight::FULL);
    }

    /// Применяет тег слоя 3 (`\blur`, `\be`, `\pbo`, `\clip`, `\iclip`) с весом `\t`;
    /// остальные теги не относятся к этому состоянию и игнорируются (их применяет `RenderState`).
    ///
    /// Анимируются `\blur`, `\be` и прямоугольный клип; `\pbo` и векторный клип применяются
    /// сразу и целиком.
    pub(crate) fn apply_weighted(&mut self, tag: &AssOverride, weight: TagWeight) {
        match tag {
            AssOverride::Blur(value) => {
                self.blur_radius = match value {
                    // Зажатие — ПОСЛЕ смешивания (ass_parse.c, ветка `blur`).
                    Some(new) => clamp_blur_radius(weight.blend(self.blur_radius, *new)),
                    None => 0.0,
                };
            }
            AssOverride::EdgeBlur(value) => {
                self.edge_blur = match value {
                    Some(raw) => {
                        clamp_edge_blur(weight.blend_edge_blur_rounded(self.edge_blur, *raw))
                    }
                    None => 0,
                };
            }
            AssOverride::DrawingBaselineOffset(value) => self.baseline_offset = *value,
            AssOverride::Clip(clip) => self.apply_clip(clip, weight),
            _ => {}
        }
    }

    /// Прямоугольник: побеждает последний тег (перезаписывает прямоугольник и режим), а углы
    /// смешиваются со старыми (по умолчанию — весь `PlayRes`) с усечением к целому. Режим
    /// (`\clip` / `\iclip`) переключается сразу. Векторный: побеждает первый
    /// (`!state->clip_drawing_text.str`), не анимируется.
    fn apply_clip(&mut self, clip: &AssClip, weight: TagWeight) {
        match &clip.shape {
            AssClipShape::Rect { x0, y0, x1, y1 } => {
                let old = self.rect_clip.unwrap_or(RectClip {
                    mode: clip.mode,
                    x0: 0,
                    y0: 0,
                    x1: self.play_res.width,
                    y1: self.play_res.height,
                });
                self.rect_clip = Some(RectClip {
                    mode: clip.mode,
                    x0: weight.blend_truncated(old.x0, *x0),
                    y0: weight.blend_truncated(old.y0, *y0),
                    x1: weight.blend_truncated(old.x1, *x1),
                    y1: weight.blend_truncated(old.y1, *y1),
                });
            }
            AssClipShape::Vector { scale, drawing } => {
                if self.vector_clip.is_none() {
                    self.vector_clip = Some(VectorClip {
                        mode: clip.mode,
                        scale: *scale,
                        drawing: drawing.clone(),
                    });
                }
            }
        }
    }
}

impl TagSink for FilterParams {
    fn apply_tag(&mut self, tag: &AssOverride, weight: TagWeight) {
        self.apply_weighted(tag, weight);
    }
}

/// `\blur`: `clamp(v, 0, BLUR_MAX_RADIUS)` (ass_parse.c). NaN из разбора недостижим; на
/// всякий случай приводится к 0, чтобы не пустить его в размытие.
fn clamp_blur_radius(raw: f64) -> f64 {
    if raw.is_nan() {
        return 0.0;
    }
    raw.clamp(0.0, ASS_BLUR_MAX_RADIUS)
}

/// `\be`: после `dtoi32(...)` зажатие `[0, MAX_BE]`. `dtoi32` libass возвращает `INT32_MIN`
/// для NaN и значений вне `i32`, то есть такие значения дают 0, а не 127.
fn clamp_edge_blur(rounded: i32) -> i32 {
    rounded.clamp(0, ASS_EDGE_BLUR_MAX)
}

#[cfg(test)]
mod tests;
