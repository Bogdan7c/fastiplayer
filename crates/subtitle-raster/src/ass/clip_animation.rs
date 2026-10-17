// Порт из libass 0.17.5: ass_parse.c (ветки `clip` / `iclip` внутри `\t`) и ass_render.c
// (ass_render_event: «fix clip coordinates»).
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

//! Покадровый прямоугольный клип для `\t(\clip(...))`.
//!
//! ## Зачем
//! Решение владельца: клип — стадия ПОСЛЕ кэша реплики (`crate::clip`), поэтому `\t`, который
//! двигает только клип, не должен рисовать текст заново: картинки реплики остаются в кэше,
//! а прямоугольник пересчитывается на момент каждого кадра.
//!
//! ## Что хранится
//! Реплика, в которой есть `\t` с прямоугольным клипом, получает [`ClipAnimation`]: урезанную
//! копию её тегов (только `\clip`/`\iclip` и `\t`, внутри `\t` — снова только они), масштаб
//! кадра и признак «явной» реплики. Этого достаточно, чтобы на любых часах пройти теги в том же
//! порядке и по тем же правилам, что и полный разбор (`RenderState`), и получить
//! экранный прямоугольник ([`ClipAnimation::rect_at`]). Остальные теги на клип не влияют
//! (`\r` клипы не сбрасывает), поэтому их можно отбросить.
//!
//! ## Что НЕ входит
//! Векторный клип не анимируется и строится один раз при рисовании реплики; вес `\t`,
//! который двигает только клип, в ключ кэша не входит (см. `AssAnimationKey`). `\t`,
//! меняющий и клип, и вид букв (`\t(\clip(...)\fs50)`), даёт запись кэша на каждый вес, но
//! клип для кадра всё равно считается здесь — по часам кадра.

use subtitle_core::{AssCue, AssOverride, AssPlayRes, AssTextItem, AssTransform};

use super::animation::AssEventClock;
use super::clip_plan::screen_rect;
use super::filter_params::FilterParams;
use super::frame_scale::AssFrameScale;
use super::scroll::ScrollBand;
use super::transform_apply::apply_transform;
use super::transform_scan::animates_rect_clip;
use crate::clip::ScreenRectClip;
use crate::overlay::CanvasSize;

/// Описание прямоугольного клипа реплики, зависящего от времени.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClipAnimation {
    /// Только теги клипа и `\t`, в порядке записи.
    overrides: Vec<AssOverride>,
    play_res: AssPlayRes,
    frame: AssFrameScale,
    canvas: CanvasSize,
    /// Реплика явная: углы зажимаются в кадр (`ass_render_event`).
    explicit: bool,
    /// Полоса `Scroll up/down`: клип на любой момент зажат в неё по высоте.
    scroll_band: Option<ScrollBand>,
}

impl ClipAnimation {
    /// Описание клипа реплики; `None`, если в ней нет `\t` с прямоугольным клипом (тогда клип
    /// от времени не зависит и считается один раз вместе с планом).
    pub(crate) fn for_cue(
        cue: &AssCue,
        frame: &AssFrameScale,
        canvas: CanvasSize,
        explicit: bool,
    ) -> Option<Self> {
        if !animates_rect_clip(cue) {
            return None;
        }
        let mut overrides = Vec::new();
        for item in &cue.text {
            if let AssTextItem::Overrides(tags) = item {
                keep_clip_tags(tags, &mut overrides);
            }
        }
        Some(Self {
            overrides,
            play_res: cue.script.info().play_res,
            frame: *frame,
            canvas,
            explicit,
            scroll_band: None,
        })
    }

    /// Задаёт полосу прокрутки: она применяется к прямоугольнику на каждом кадре.
    pub(super) fn limit_to_scroll_band(&mut self, band: ScrollBand) {
        self.scroll_band = Some(band);
    }

    /// Экранный прямоугольник клипа на часах `clock`; `None` — прямоугольного клипа нет.
    pub(crate) fn rect_at(&self, clock: &AssEventClock) -> Option<ScreenRectClip> {
        let mut filters = FilterParams::for_play_res(self.play_res);
        for tag in &self.overrides {
            match tag {
                AssOverride::Transform(transform) => {
                    apply_transform(&mut filters, transform, Some(clock));
                }
                other => filters.apply(other),
            }
        }
        let rect = filters
            .rect_clip()
            .map(|clip| screen_rect(clip, &self.frame, self.canvas, self.explicit));
        match self.scroll_band {
            None => rect,
            Some(band) => Some(
                rect.unwrap_or_else(|| ScreenRectClip::whole_canvas(self.canvas))
                    .limited_to_rows(band.top, band.bottom),
            ),
        }
    }
}

/// Копирует в `out` теги клипа и `\t` (с урезанным содержимым); `\t` без клипов внутри
/// сохраняются тоже: вложенный `\t` подменяет вес для следующих за ним тегов.
fn keep_clip_tags(tags: &[AssOverride], out: &mut Vec<AssOverride>) {
    for tag in tags {
        match tag {
            AssOverride::Clip(_) => out.push(tag.clone()),
            AssOverride::Transform(transform) => {
                let mut nested = Vec::new();
                keep_clip_tags(&transform.tags, &mut nested);
                out.push(AssOverride::Transform(AssTransform {
                    timing: transform.timing,
                    tags: nested,
                }));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
