// Порт из libass 0.17.5: ass_parse.c (`ass_apply_transition_effects` — пересчёт задержки
// и `scroll_shift`) и ass_render.c (`ass_render_event` — положение и клип прокрутки).
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

//! Прокрутка реплики эффектами `Banner`, `Scroll up`, `Scroll down`.
//!
//! Модуль владеет временной арифметикой прокрутки и переводом её в экранные величины:
//! - [`ScrollClock`] — «на сколько пикселей PlayRes реплика уже уехала» (`scroll_shift`);
//! - [`scroll_placement`] — куда прокрутка ставит блок текста (его край на экране);
//! - [`scroll_band`] — полоса `Scroll up/down`, за которую текст не выходит (клип по высоте).
//!
//! ## Задержка не ступенчатая
//! `scroll_shift = (time - Start) / delay` — деление `double`, поэтому положение меняется
//! каждую миллисекунду, а не «на целый пиксель раз в `delay` мс». Из-за этого картинка
//! прокручиваемой реплики честно меняется на каждом кадре.
//!
//! ## Пересчёт задержки
//! VSFilter работает в координатах хранения, а `delay` задан в PlayRes. libass сохраняет это
//! поведение только для малых значений: `delay / scale` усекается до целого, не меньше 1, и
//! умножается обратно (`scale = layout_res / PlayRes` по соответствующей оси). Поэтому `0`
//! и отрицательные задержки дают 1 мс на пиксель хранения, а не на пиксель PlayRes.

use subtitle_core::{AssBannerDirection, AssEffect, AssScrollDirection};

use super::frame_scale::AssFrameScale;
use crate::event_style::ScrollPlacement;

/// Часы прокрутки одной реплики: эффективная задержка в мс на пиксель PlayRes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScrollClock {
    /// Задержка после пересчёта (`delay` в `ass_apply_transition_effects`).
    delay_ms: f64,
}

impl ScrollClock {
    /// Часы для эффекта реплики; `None`, если эффект не прокручивает (нет эффекта или libass
    /// его отклонил из-за нехватки чисел).
    pub(crate) fn for_effect(effect: AssEffect, frame: &AssFrameScale) -> Option<Self> {
        let (written_delay_ms, scale) = match effect {
            AssEffect::None => return None,
            AssEffect::Banner { delay_ms, .. } => (delay_ms, frame.layout_per_play_res_x()),
            AssEffect::Scroll { delay_ms, .. } => (delay_ms, frame.layout_per_play_res_y()),
        };
        Some(Self {
            delay_ms: rescaled_delay(written_delay_ms, scale),
        })
    }

    /// `scroll_shift` через `elapsed_ms` от начала реплики, в пикселях PlayRes.
    pub(crate) fn shift_after(&self, elapsed_ms: i32) -> f64 {
        f64::from(elapsed_ms) / self.delay_ms
    }
}

/// `((int) FFMAX(delay / scale, 1)) * scale`; `as i32` насыщается там, где C даёт UB.
fn rescaled_delay(written_delay_ms: i32, scale: f64) -> f64 {
    let in_storage_pixels = (f64::from(written_delay_ms) / scale).max(1.0);
    f64::from(in_storage_pixels as i32) * scale
}

/// Куда прокрутка ставит блок текста при сдвиге `shift` (пиксели PlayRes).
///
/// Без эффекта — [`ScrollPlacement::None`]. Направления (ass_render.c, ~2889-2919):
/// `Banner` без флага — `SCROLL_RL`, с флагом — `SCROLL_LR`; `Scroll up` — `SCROLL_BT`,
/// `Scroll down` — `SCROLL_TB`.
pub(crate) fn scroll_placement(
    effect: AssEffect,
    shift: f64,
    frame: &AssFrameScale,
) -> ScrollPlacement {
    match effect {
        AssEffect::None => ScrollPlacement::None,
        AssEffect::Banner {
            direction: AssBannerDirection::RightToLeft,
            ..
        } => ScrollPlacement::RightToLeft {
            origin_x: frame.x_to_screen(frame.play_res_x() - shift),
        },
        AssEffect::Banner {
            direction: AssBannerDirection::LeftToRight,
            ..
        } => ScrollPlacement::LeftToRight {
            right_edge_x: frame.x_to_screen(shift),
        },
        AssEffect::Scroll {
            direction: AssScrollDirection::Down,
            y_top,
            ..
        } => ScrollPlacement::TopToBottom {
            bottom_edge_y: frame.y_to_screen(f64::from(y_top) + shift),
        },
        AssEffect::Scroll {
            direction: AssScrollDirection::Up,
            y_bottom,
            ..
        } => ScrollPlacement::BottomToTop {
            top_edge_y: frame.y_to_screen(f64::from(y_bottom) - shift),
        },
    }
}

/// Полоса по высоте в пикселях холста, в которой виден текст `Scroll up/down`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrollBand {
    /// `lround(y2scr_pos(scroll_y0))`.
    pub(crate) top: i32,
    /// `lround(y2scr_pos(scroll_y1))`.
    pub(crate) bottom: i32,
}

/// Полоса прокрутки реплики; `None` у `Banner` и у реплик без эффекта (клипа по высоте нет).
pub(crate) fn scroll_band(effect: AssEffect, frame: &AssFrameScale) -> Option<ScrollBand> {
    let AssEffect::Scroll {
        y_top, y_bottom, ..
    } = effect
    else {
        return None;
    };
    // lround: округление от нуля.
    let to_row = |y: i32| frame.y_to_screen(f64::from(y)).round() as i32;
    Some(ScrollBand {
        top: to_row(y_top),
        bottom: to_row(y_bottom),
    })
}

#[cfg(test)]
mod tests;
