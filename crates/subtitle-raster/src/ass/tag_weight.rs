// Порт из libass 0.17.5: ass_parse.c (`dtoi32`, `calc_anim`, `calc_anim_int32`, `change_color`,
// `change_alpha`, ветка `complex_tag("t")` — расчёт коэффициента `k`).
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

//! Вес тега `\t`: «насколько тег уже применён» (в libass — `pwr`) и арифметика смешивания.
//!
//! Тег `\t(t1,t2,accel,\теги)` не меняет вложенные теги сам: он задаёт коэффициент
//! `k ∈ [0, 1]`, с которым libass смешивает СТАРОЕ значение состояния с НОВЫМ
//! (`old * (1 - k) + new * k`). Обычный тег вне `\t` — это тот же разбор с `k = 1`
//! ([`TagWeight::FULL`]). Все формулы смешивания живут здесь, чтобы каждая анимируемая
//! ветка ([`super::render_state::RenderState`], [`super::filter_params::FilterParams`])
//! округляла и усекала в точности как соответствующая ветка C.

use subtitle_core::{AssRgb, AssTransformTiming};

use super::animation::AssEventClock;

/// `dtoi32` libass: преобразование `double -> int32_t` без UB. NaN и значения вне `i32`
/// дают `INT32_MIN`, остальные усекаются к нулю.
pub(crate) fn dtoi32(value: f64) -> i32 {
    if value.is_nan() || value <= f64::from(i32::MIN) || value >= f64::from(i32::MAX) + 1.0 {
        return i32::MIN;
    }
    value as i32
}

/// Коэффициент `pwr` анимации `\t`: 0 — старое значение, 1 — новое.
///
/// Значение может выходить за `[0, 1]` (ускорение `accel <= 0`, дырявая арифметика `uint32`
/// при невозможных временах) и даже быть NaN — libass считает ровно так же, поэтому здесь
/// нет зажатия.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TagWeight(f64);

impl TagWeight {
    /// Тег применён целиком (обычный тег вне `\t`, либо `\t` уже завершён).
    pub(crate) const FULL: Self = Self(1.0);
    /// Тег ещё не начал действовать.
    pub(crate) const NONE: Self = Self(0.0);

    /// Вес с заданным значением (для вычислений вроде `pow` в [`transform_weight_at`]).
    pub(crate) const fn new(value: f64) -> Self {
        Self(value)
    }

    /// Само число `pwr`.
    pub(crate) const fn value(self) -> f64 {
        self.0
    }

    /// Точные биты веса: единица сравнения в ключе кэша (см. `AssAnimationKey`).
    pub(crate) fn to_bits(self) -> u64 {
        self.0.to_bits()
    }

    /// `calc_anim`: `(1 - pwr) * old + new * pwr`.
    pub(crate) fn blend(self, old: f64, new: f64) -> f64 {
        (1.0 - self.0) * old + new * self.0
    }

    /// Смесь целых, как `state->clip_x0 * (1 - pwr) + x0 * pwr`, присвоенная переменной `int`:
    /// усечение к нулю (при выходе за диапазон `as` насыщает вместо UB языка C).
    pub(crate) fn blend_truncated(self, old: i32, new: i32) -> i32 {
        self.blend(f64::from(old), f64::from(new)) as i32
    }

    /// Относительный размер шрифта `\fs+N` / `\fs-N`: `font_size * (1 + pwr * delta / 10)`.
    pub(crate) fn scale_relative(self, font_size: f64, delta: f64) -> f64 {
        font_size * (1.0 + self.0 * delta / 10.0)
    }

    /// `calc_anim_int32`: смесь двух `uint32` (в виде `double`), результат — `dtoi32`.
    fn blend_masked_channel(self, old: u32, new: u32) -> i32 {
        dtoi32(self.blend(f64::from(old), f64::from(new)))
    }

    /// `change_color`: каждый канал смешивается отдельно в своей позиции слова.
    ///
    /// libass работает со словом `0xAABBGGRR` (после `ass_bswap32`) и считает смесь над
    /// `value & 0xff0000` (B), `& 0x00ff00` (G), `& 0x0000ff` (R) — то есть над числами,
    /// сдвинутыми на 16, 8 и 0 бит. Усечение `dtoi32` происходит ДО маски, поэтому порядок
    /// операций здесь воспроизведён буквально: так совпадают и округление, и случаи с
    /// выходом за диапазон.
    pub(crate) fn blend_rgb(self, old: AssRgb, new: AssRgb) -> AssRgb {
        let channel = |old_byte: u8, new_byte: u8, shift: u32| -> u8 {
            let mask = 0xff_u32 << shift;
            let blended = self
                .blend_masked_channel(u32::from(old_byte) << shift, u32::from(new_byte) << shift);
            // `& mask` над int32: как в C (отрицательные и INT32_MIN дают 0 в нужных битах).
            (((blended as u32) & mask) >> shift) as u8
        };
        AssRgb::new(
            channel(old.red, new.red, 0),
            channel(old.green, new.green, 8),
            channel(old.blue, new.blue, 16),
        )
    }

    /// `change_alpha`: `(uint8_t) calc_anim_int32(new, old, pwr)` — младший байт усечённой смеси.
    ///
    /// `new` — сырой `int32` из `parse_alpha_tag`; в смесь он входит как `uint32` (неявное
    /// приведение параметра `calc_anim_int32`), поэтому отрицательные значения дают большие
    /// числа, а не маленькие.
    pub(crate) fn blend_alpha(self, old: u8, new: i32) -> u8 {
        self.blend_masked_channel(u32::from(old), new as u32) as u8
    }

    /// `\be`: `dtoi32(be * (1 - pwr) + value * pwr + 0.5)` — до зажатия `[0, MAX_BE]`.
    ///
    /// VSFilter всегда прибавляет 0.5, даже к отрицательным числам.
    pub(crate) fn blend_edge_blur_rounded(self, old: i32, raw: f64) -> i32 {
        dtoi32(self.blend(f64::from(old), raw) + 0.5)
    }
}

/// Коэффициент `k` тега `\t` на часах реплики (ветка `complex_tag("t")` libass).
pub(crate) fn transform_weight(timing: &AssTransformTiming, clock: &AssEventClock) -> TagWeight {
    transform_weight_at(timing, clock.elapsed_ms(), clock.duration_ms())
}

/// Времена `\t` после подстановки умолчаний: `t2 == 0` означает «до конца реплики».
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TransformSpan {
    /// Начало анимации, мс от начала реплики.
    pub(crate) start_ms: i32,
    /// Конец анимации, мс от начала реплики.
    pub(crate) end_ms: i32,
    /// Показатель степени `accel`.
    pub(crate) accel: f64,
}

impl TransformSpan {
    /// Времена и ускорение по форме тега; нулевой `t2` заменяется длительностью реплики
    /// (именно `t2 == 0`, а не «любое неположительное»).
    pub(crate) fn new(timing: &AssTransformTiming, duration_ms: i32) -> Self {
        let (start_ms, end_ms, accel) = match *timing {
            AssTransformTiming::Default => (0, 0, 1.0),
            AssTransformTiming::Accel(accel) => (0, 0, accel),
            AssTransformTiming::Range { t1_ms, t2_ms } => (t1_ms, t2_ms, 1.0),
            AssTransformTiming::RangeAccel {
                t1_ms,
                t2_ms,
                accel,
            } => (t1_ms, t2_ms, accel),
        };
        Self {
            start_ms,
            end_ms: if end_ms == 0 { duration_ms } else { end_ms },
            accel,
        }
    }

    /// `k` на `elapsed_ms` от начала реплики.
    ///
    /// Арифметика как в C: `delta_t = (uint32_t) t2 - t1` кладётся в `int32_t`, числитель —
    /// `(int32_t)((uint32_t) t - t1)`; до `t1` вес 0, с `t2` — 1 (проверка `t < t1` первая).
    pub(crate) fn weight_at(&self, elapsed_ms: i32) -> TagWeight {
        if elapsed_ms < self.start_ms {
            return TagWeight::NONE;
        }
        if elapsed_ms >= self.end_ms {
            return TagWeight::FULL;
        }
        let delta = (self.end_ms as u32).wrapping_sub(self.start_ms as u32) as i32;
        let passed = (elapsed_ms as u32).wrapping_sub(self.start_ms as u32) as i32;
        TagWeight::new((f64::from(passed) / f64::from(delta)).powf(self.accel))
    }
}

/// То же, что [`transform_weight`], но по готовым миллисекундам (для тестов и классификации).
pub(crate) fn transform_weight_at(
    timing: &AssTransformTiming,
    elapsed_ms: i32,
    duration_ms: i32,
) -> TagWeight {
    TransformSpan::new(timing, duration_ms).weight_at(elapsed_ms)
}

#[cfg(test)]
mod tests;
