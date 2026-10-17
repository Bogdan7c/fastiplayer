// Порт из libass 0.17.5: ass_render.c (quantize_blur, restore_blur, ass_be_padding).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
// Copyright (C) 2011 Grigori Goronzy <greg@chown.ath.cx>
// Copyright (C) 2015-2022 libass contributors
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

//! Математика параметров размытия: квантование радиуса, дисперсия, отступ `\be`
//! (libass 0.17.5: `ass_render.c:2398-2444`, `2513-2523`, `2633-2641`).
//!
//! Квантование нужно кэшу: близкие радиусы дают одну и ту же картинку, поэтому в ключ
//! композита кладётся целый `level`, а дисперсия восстанавливается из него
//! ([`restore_blur`]). Тот же `shadow_mask` огрубляет смещение тени, чтобы оно не точнее
//! размытия, которое её размазывает.

/// Точность размытия как доля полного диапазона входа (libass: `BLUR_PRECISION`).
const BLUR_PRECISION: f64 = 1.0 / 256.0;
/// Грубая оценка ошибки преобразования в долях 1/64 пикселя (libass: `POSITION_PRECISION`).
const POSITION_PRECISION: f64 = 8.0;
/// Верхний предел `\be` (libass: `ass_parse.c`, зажим в `[0, 127]`).
const BOX_BLUR_MAX_PASSES: i32 = 127;

/// Результат [`quantize_blur`] (libass: возврат `quantize_blur` и `*shadow_mask`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuantizedBlur {
    /// Квантованный уровень размытия (ключ кэша; `0` — размытия нет).
    pub(crate) level: i32,
    /// Маска огрубления смещения тени: `(1 << ord) - 1`, при нулевом размытии `7`.
    pub(crate) shadow_mask: i32,
}

/// Множитель радиуса размытия `\blur` (libass: `2 / sqrt(log(256))`, `ass_render.c:2513`).
///
/// Радиус в пикселях кадра перед квантованием: `blur * state.blur_scale * blur_radius_scale()`.
pub(crate) fn blur_radius_scale() -> f64 {
    2.0 / 256f64.ln().sqrt()
}

/// Квантует радиус размытия (libass: `quantize_blur`).
///
/// `radius` — уже масштабированный радиус (см. [`blur_radius_scale`]), неотрицательный:
/// парсер тегов зажимает `\blur` в `[0, 100]`. На отрицательных и нечисловых значениях
/// функция не паникует, но результат не имеет смысла.
pub(crate) fn quantize_blur(radius: f64) -> QuantizedBlur {
    let scale = 64.0 * BLUR_PRECISION / POSITION_PRECISION;
    let radius = radius * scale;
    // ord = floor(log2(BLUR_PRECISION * r + POSITION_PRECISION)); floor(log2(x)) = frexp(x/2).exp
    let (_, order) = frexp((1.0 + radius) * (POSITION_PRECISION / 2.0));
    // Сдвиг u32 определён только для 0..=31; в libass порядок здесь всегда 3..=10.
    let shadow_mask = (1u32 << order.clamp(0, 31)).wrapping_sub(1);
    // `ass_lrint` — округление к ближайшему чётному (режим по умолчанию `lrint`).
    let level = (radius.ln_1p() / BLUR_PRECISION).round_ties_even() as i32;
    QuantizedBlur {
        level,
        shadow_mask: shadow_mask as i32,
    }
}

/// Восстанавливает дисперсию `r2` (в пикселях в квадрате) из квантованного уровня
/// (libass: `restore_blur`).
pub(crate) fn restore_blur(level: i32) -> f64 {
    let scale = 64.0 * BLUR_PRECISION / POSITION_PRECISION;
    let sigma = (BLUR_PRECISION * f64::from(level)).exp_m1() / scale;
    sigma * sigma
}

/// Огрубляет смещение тени маской [`QuantizedBlur::shadow_mask`]
/// (libass: `ass_render.c:2521-2522`, смещение уже в 1/64 пикселя).
pub(crate) fn quantize_shadow_offset(offset_d6: i32, shadow_mask: i32) -> i32 {
    (offset_d6.wrapping_add(shadow_mask >> 1)) & !shadow_mask
}

/// Число проходов box-размытия `\be`, зажатое в `0..=127` (как делает парсер libass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BoxBlurPasses(u8);

impl BoxBlurPasses {
    /// Без box-размытия.
    #[cfg(test)]
    pub(crate) const NONE: Self = Self(0);

    /// Зажимает значение тега `\be` в допустимый диапазон `0..=127`.
    pub(crate) fn clamped(value: i32) -> Self {
        Self(value.clamp(0, BOX_BLUR_MAX_PASSES) as u8)
    }

    /// Число проходов.
    pub(crate) fn count(self) -> u32 {
        u32::from(self.0)
    }

    /// Отступ с каждой стороны композита, нужный проходам (libass: `ass_be_padding`).
    ///
    /// `\be` размывает картинку на месте, поэтому композит заранее увеличивается на этот
    /// отступ: 0..=3 проходов — столько же пикселей, 4..=7 — 4, больше — 5.
    pub(crate) fn padding_px(self) -> i32 {
        match self.0 {
            0..=3 => i32::from(self.0),
            4..=7 => 4,
            _ => 5,
        }
    }
}

/// Разложение числа на мантиссу в `[0.5, 1)` и порядок (как `frexp` из C).
///
/// Для нуля и нечисловых значений возвращает `(value, 0)`.
pub(super) fn frexp(value: f64) -> (f64, i32) {
    if value == 0.0 || !value.is_finite() {
        return (value, 0);
    }
    // Денормализованные числа домножаем на 2^54, чтобы порядок читался из битов.
    let (normal, adjustment) = if value.abs() < f64::MIN_POSITIVE {
        (value * 2f64.powi(54), -54)
    } else {
        (value, 0)
    };
    let bits = normal.to_bits();
    let exponent_field = ((bits >> 52) & 0x7ff) as i32;
    // Порядок 1022 в смещённом виде соответствует мантиссе из диапазона [0.5, 1).
    let mantissa_bits = (bits & !(0x7ffu64 << 52)) | (1022u64 << 52);
    (
        f64::from_bits(mantissa_bits),
        exponent_field - 1022 + adjustment,
    )
}
