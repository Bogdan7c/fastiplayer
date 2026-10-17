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
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct FilterParams {
    blur_radius: f64,
    edge_blur: i32,
    rect_clip: Option<RectClip>,
    vector_clip: Option<VectorClip>,
    baseline_offset: f64,
}

impl FilterParams {
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

    /// Применяет тег слоя 3 (`\blur`, `\be`, `\pbo`, `\clip`, `\iclip`); остальные теги
    /// не относятся к этому состоянию и игнорируются (их применяет `RenderState`).
    pub(crate) fn apply(&mut self, tag: &AssOverride) {
        match tag {
            AssOverride::Blur(value) => self.blur_radius = clamp_blur_radius(value.unwrap_or(0.0)),
            AssOverride::EdgeBlur(value) => {
                self.edge_blur = value.map_or(0, edge_blur_from_raw);
            }
            AssOverride::DrawingBaselineOffset(value) => self.baseline_offset = *value,
            AssOverride::Clip(clip) => self.apply_clip(clip),
            _ => {}
        }
    }

    /// Прямоугольник: побеждает последний тег (перезаписывает прямоугольник и режим).
    /// Векторный: побеждает первый (`!state->clip_drawing_text.str`).
    fn apply_clip(&mut self, clip: &AssClip) {
        match &clip.shape {
            AssClipShape::Rect { x0, y0, x1, y1 } => {
                self.rect_clip = Some(RectClip {
                    mode: clip.mode,
                    x0: *x0,
                    y0: *y0,
                    x1: *x1,
                    y1: *y1,
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

/// `\blur`: `clamp(v, 0, BLUR_MAX_RADIUS)` (ass_parse.c). NaN из разбора недостижим; на
/// всякий случай приводится к 0, чтобы не пустить его в размытие.
fn clamp_blur_radius(raw: f64) -> f64 {
    if raw.is_nan() {
        return 0.0;
    }
    raw.clamp(0.0, ASS_BLUR_MAX_RADIUS)
}

/// `\be`: `dtoi32(v + 0.5)` затем `clamp(0, MAX_BE)`. `dtoi32` libass возвращает `INT32_MIN`
/// для NaN и значений вне `i32`, то есть такие значения дают 0, а не 127. Остальные
/// усекаются к нулю (приведение C `double -> int32_t`).
fn edge_blur_from_raw(raw: f64) -> i32 {
    let shifted = raw + 0.5;
    let truncated =
        if shifted.is_nan() || shifted <= f64::from(i32::MIN) || shifted >= 2_147_483_648.0 {
            i32::MIN
        } else {
            shifted as i32
        };
    truncated.clamp(0, ASS_EDGE_BLUR_MAX)
}

#[cfg(test)]
mod tests;
