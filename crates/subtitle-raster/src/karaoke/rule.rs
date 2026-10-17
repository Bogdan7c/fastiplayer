// Порт из libass 0.17.5: ass_render.c (`render_text`: выбор цвета и границы по эффекту,
// `render_and_combine_glyphs`: пересчёт `effect_timing` в экранный `x`).
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

//! Правило караоке одного участка готовой реплики.

use subtitle_core::SubtitleColor;

use super::mark::KaraokeEffect;
use super::plan::KaraokeWord;
use crate::clip::ImagePaint;
use crate::fixed_point::d6_to_double;

/// Что нарисовать для участка на конкретный момент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunPaint {
    /// Как красить заливку участка.
    pub(crate) glyph: ImagePaint,
    /// Рисовать ли обводку (у `\ko` она скрыта до начала слога).
    pub(crate) outline_visible: bool,
}

/// Когда картинка участка изменится в следующий раз.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextChange {
    /// Больше не изменится.
    Never,
    /// Меняется на каждом кадре (идёт плавная заливка).
    EveryFrame,
    /// Не изменится до этой миллисекунды от начала реплики (включительно она уже новая).
    AtMs(i64),
}

/// Слово, к которому относится участок, и данные для переноса его границы на экран.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct KaraokeRunRule {
    word: KaraokeWord,
    /// Позиция корневого глифа ПЕРВОГО символа участка в предварительной раскладке, 26.6.
    /// libass берёт `effect_timing` первого глифа для всей объединённой картинки участка.
    first_origin_x: i32,
    /// Самая левая точка участка на экране, 26.6 (только для `\kf`).
    leftmost_x: i32,
    /// Растяжение по горизонтали `par_scale_x`.
    pixel_aspect_x: f64,
    /// Основной цвет участка с затуханием (`c[0]`).
    primary: SubtitleColor,
    /// Вторичный цвет участка с затуханием (`c[1]`).
    secondary: SubtitleColor,
}

/// Исходные данные правила (собираются при композиции участка).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RuleInputs {
    pub(crate) word: KaraokeWord,
    pub(crate) first_origin_x: i32,
    pub(crate) leftmost_x: i32,
    pub(crate) pixel_aspect_x: f64,
    pub(crate) primary: SubtitleColor,
    pub(crate) secondary: SubtitleColor,
}

impl KaraokeRunRule {
    /// Правило участка.
    pub(crate) fn new(inputs: RuleInputs) -> Self {
        Self {
            word: inputs.word,
            first_origin_x: inputs.first_origin_x,
            leftmost_x: inputs.leftmost_x,
            pixel_aspect_x: inputs.pixel_aspect_x,
            primary: inputs.primary,
            secondary: inputs.secondary,
        }
    }

    /// Раскраска участка на момент `elapsed_ms` (мс от начала реплики). `None` — часов нет, и
    /// все слоги считаются спетыми.
    pub(crate) fn paint_at(&self, elapsed_ms: Option<i64>) -> RunPaint {
        let progress = elapsed_ms.map_or(super::plan::WordProgress::Finished, |elapsed| {
            self.word.progress_at(elapsed)
        });
        // `info->effect_timing = x - info->pos.x` первого глифа участка.
        let timing = progress.x().wrapping_sub(self.first_origin_x);
        let (left, right) = if progress.colors_swapped() {
            (self.secondary, self.primary)
        } else {
            (self.primary, self.secondary)
        };
        let sung_color = if timing > 0 { left } else { right };
        match self.word.effect {
            KaraokeEffect::Fill => RunPaint {
                glyph: ImagePaint::SplitAt {
                    x: self.screen_split_x(timing),
                    left,
                    right,
                },
                outline_visible: true,
            },
            KaraokeEffect::Plain => RunPaint {
                glyph: ImagePaint::Solid(sung_color),
                outline_visible: true,
            },
            KaraokeEffect::Outline => RunPaint {
                glyph: ImagePaint::Solid(sung_color),
                outline_visible: timing > 0,
            },
        }
    }

    /// `lround(leftmost_x / 64 + effect_timing / 64 * par_scale_x)`: абсолютный экранный `x`
    /// границы (`lround` округляет ничьи от нуля, в отличие от `ass_lrint`).
    fn screen_split_x(&self, timing: i32) -> i32 {
        let screen = d6_to_double(self.leftmost_x) + d6_to_double(timing) * self.pixel_aspect_x;
        screen.round() as i64 as i32
    }

    /// Когда картинка участка изменится после момента `elapsed_ms`.
    pub(crate) fn next_change(&self, elapsed_ms: i64) -> NextChange {
        if elapsed_ms < self.word.start_ms {
            NextChange::AtMs(self.word.start_ms)
        } else if elapsed_ms < self.word.end_ms {
            NextChange::EveryFrame
        } else {
            NextChange::Never
        }
    }
}
