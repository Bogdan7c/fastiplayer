// Порт из libass 0.17.5: ass_outline.c (`ass_outline_update_min_transformed_x`) и ass_render.c
// (`leftmost_x` в `get_bitmap_glyph` / `render_and_combine_glyphs`).
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

//! Самая левая точка участка на экране.
//!
//! Граница заливки `\kf` считается в предварительной раскладке (до BiDi, выравнивания и
//! поворота), а на экран её переносит `leftmost_x` участка: минимум экранного `x` всех
//! контрольных точек полностью преобразованных контуров. Так граница остаётся вертикальной в
//! экранных координатах при любых `\frx`/`\fry`/`\frz`.

use crate::glyph_transform::Matrix3;
use crate::outline::{OUTLINE_MAX, Outline};

/// Минимальный преобразованный `x` по глифам участка, 26.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LeftmostTracker {
    min_x: i32,
}

impl Default for LeftmostTracker {
    fn default() -> Self {
        // libass: `current_info->leftmost_x = OUTLINE_MAX`.
        Self { min_x: OUTLINE_MAX }
    }
}

impl LeftmostTracker {
    /// Текущий минимум, 26.6 (`OUTLINE_MAX`, если не учтено ни одной точки).
    pub(crate) fn min_x(self) -> i32 {
        self.min_x
    }

    /// Учитывает контур глифа, преобразованный матрицей `matrix` (та, что получает
    /// `quantize_transform`: до сдвига в центр рамки).
    pub(crate) fn include_outline(&mut self, outline: &Outline, matrix: &Matrix3) {
        for point in outline.points() {
            let (x, y) = (f64::from(point.x), f64::from(point.y));
            let z = matrix[2][0] * x + matrix[2][1] * y + matrix[2][2];
            // `FFMAX(z, 0.1)`: при NaN макрос вернёт 0.1, как и `f64::max`.
            let screen_x = (matrix[0][0] * x + matrix[0][1] * y + matrix[0][2]) / z.max(0.1);
            if screen_x.is_nan() {
                continue;
            }
            let clamped = screen_x.clamp(-f64::from(OUTLINE_MAX), f64::from(OUTLINE_MAX));
            // `ass_lrint` после зажатия: значение заведомо помещается в `i32`.
            self.min_x = self.min_x.min(clamped.round_ties_even() as i32);
        }
    }
}
