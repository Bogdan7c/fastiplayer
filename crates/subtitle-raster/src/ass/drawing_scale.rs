// Порт из libass 0.17.5: ass_render.c (get_outline_glyph — расчёт `scale` рисунка).
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

//! Масштаб контура рисунка `\p<n>` в пиксели кадра.
//!
//! Координаты рисунка делятся на `2^(n-1)` (`\p1` — пиксели сценария, `\p2` — полпикселя и так
//! далее), затем растягиваются как текст: `\fscx`/`\fscy` и масштаб кадра. По горизонтали
//! масштаб кадра ещё делится на `par_scale_x`.

use super::frame_scale::EventScale;
use crate::event_style::AxisPair;

/// Множитель `1 / 2^(n-1)` координат рисунка (libass: `w` в `get_outline_glyph`).
///
/// Показатель берётся по модулю 32 (`lshiftwrapi`): при `n = 33` множитель снова 1, а при
/// `n = 32` сдвиг даёт отрицательное число, и libass обнуляет множитель — рисунок исчезает.
pub(super) fn coordinate_weight(drawing_scale: u32) -> f64 {
    let base = (1_u32 << (drawing_scale.wrapping_sub(1) & 31)) as i32;
    if base > 0 { 1.0 / f64::from(base) } else { 0.0 }
}

/// Масштаб контура рисунка по осям (libass: `transform.scale`).
///
/// `letter_scale` — `\fscx`/`\fscy` вида символа (`GlyphInfo::scale_x/scale_y` рисунка:
/// размер шрифта на него не влияет, `scale_fix = 1`).
pub(super) fn outline_scale(
    letter_scale: AxisPair,
    drawing_scale: u32,
    frame: &EventScale,
) -> AxisPair {
    let weight = coordinate_weight(drawing_scale);
    AxisPair {
        // Порядок операций как в C: `scale_x * w * screen_scale_x / par_scale_x`.
        horizontal: letter_scale.horizontal * weight * frame.screen_x / frame.pixel_aspect_x,
        vertical: letter_scale.vertical * weight * frame.screen_y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(screen_x: f64, screen_y: f64, pixel_aspect_x: f64) -> EventScale {
        EventScale {
            screen_x,
            screen_y,
            border_x: screen_x,
            border_y: screen_y,
            blur_x: screen_x,
            blur_y: screen_y,
            pixel_aspect_x,
        }
    }

    #[test]
    fn weight_halves_with_every_drawing_scale_step() {
        assert_eq!(coordinate_weight(1), 1.0);
        assert_eq!(coordinate_weight(2), 0.5);
        assert_eq!(coordinate_weight(4), 0.125);
        assert_eq!(coordinate_weight(31), 1.0 / f64::from(1_u32 << 30));
    }

    #[test]
    fn weight_wraps_the_shift_like_libass() {
        // 1 << 31 отрицательно как `int32`: libass даёт нулевой множитель.
        assert_eq!(coordinate_weight(32), 0.0);
        // Сдвиг по модулю 32: 33 -> сдвиг 0.
        assert_eq!(coordinate_weight(33), 1.0);
    }

    #[test]
    fn outline_scale_applies_letter_scale_frame_scale_and_pixel_aspect() {
        let scale = outline_scale(
            AxisPair {
                horizontal: 2.0,
                vertical: 0.5,
            },
            2,
            &frame(3.0, 4.0, 1.5),
        );
        // 2 * 0.5 * 3 / 1.5 и 0.5 * 0.5 * 4.
        assert_eq!(scale.horizontal, 2.0);
        assert_eq!(scale.vertical, 1.0);
    }
}
