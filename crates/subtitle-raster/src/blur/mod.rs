// Порт из libass 0.17.5: ass_blur.c (ass_gaussian_blur), ass_bitmap.c (ass_synth_blur).
//
// Copyright (C) 2015 Vabishchevich Nikolay <vabnick@gmail.com>
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

//! Размытие битмапов покрытия для `\blur` и `\be` (libass 0.17.5: `ass_blur.c`,
//! `c/blur_template.h`, `c/c_blur.c`, `c/c_be_blur.c`, `ass_synth_blur`).
//!
//! Модуль владеет только математикой размытия и параметрами; склейка с композитом
//! (какие битмапы размывать, тень после размытия) — в `compose`.
//!
//! # Границы
//!
//! * [`synth_blur`] — вход композита: гауссиан (если дисперсия больше порога), затем `\be`.
//! * [`gaussian_blur`] / [`box_blur`] — отдельные шаги.
//! * [`params`-функции](quantize_blur) — квантование радиуса, дисперсия, отступ `\be`.
//!
//! Гауссиан **увеличивает** битмап (`left/top` сдвигаются, ширина/высота растут), `\be`
//! работает на месте. Инвариант [`CoverageBitmap`] (шаг строки кратен 16, нули правее
//! `width`) сохраняется.
//!
//! # Владение буферами
//!
//! [`BlurScratch`] — рабочая память размытия (две плоскости `i16`, строки). Её создаёт и
//! хранит вызывающий (по одной на поток растеризации) и передаёт в каждый вызов:
//! так горячий путь не выделяет память, кроме самого выходного битмапа. Буферы только
//! растут; [`BlurScratch::release_memory`] возвращает память системе.
//!
//! # Побитовое совпадение с libass
//!
//! Результат идентичен C-версии (`align_order = 4`) на всех входах; доказательства — в
//! `tests.rs` (золотые файлы, снятые с libass) и в `#[ignore]`-сравнении с живой
//! библиотекой (`c_parity.rs`).

mod box_kernel;
mod gauss_kernels;
mod gauss_method;
mod params;

#[cfg(test)]
mod c_parity;
#[cfg(test)]
mod tests;

#[cfg_attr(
    not(test),
    expect(unused_imports, reason = "параметры читает compose в пакете P3b")
)]
pub(crate) use params::{
    BoxBlurPasses, QuantizedBlur, blur_radius_scale, quantize_blur, quantize_shadow_offset,
    restore_blur,
};

use crate::bitmap::CoverageBitmap;
use box_kernel::{BoxScratch, box_blur_pass, scale_down_for_passes, scale_up_after_passes};
use gauss_kernels::{
    LANES, Plane, RowScratch, blur_horz_dispatch, blur_vert_dispatch, expand_horz, expand_vert,
    pack, plane_stride, shrink_horz, shrink_vert, unpack,
};
use gauss_method::{BlurMethod, find_best_method};

/// Порог дисперсии, выше которого `ass_synth_blur` включает гауссиан (`ass_bitmap.c:73`).
const GAUSSIAN_VARIANCE_THRESHOLD: f64 = 0.001;
/// Предел числа уровней каскада: выше результат заведомо не помещается в `i32`.
const MAX_CASCADE_LEVEL: u32 = 27;
/// Максимум байт буфера битмапа, как в `ass_alloc_bitmap`/`CoverageBitmap::zeroed`.
const MAX_BITMAP_BYTES: u64 = i32::MAX as u64 - LANES as u64;
/// Максимум отсчётов рабочей плоскости (`ass_gaussian_blur`: `INT_MAX / 4`).
const MAX_PLANE_SAMPLES: u64 = i32::MAX as u64 / 4;

/// Почему размытие не выполнено. Битмап при ошибке не меняется.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub(crate) enum BlurError {
    /// Дисперсия отрицательная или не число.
    #[error("blur variance must be finite and non-negative, got ({x}, {y})")]
    InvalidVariance {
        /// Дисперсия по X.
        x: f64,
        /// Дисперсия по Y.
        y: f64,
    },
    /// Увеличенный битмап не помещается в допустимые размеры (libass: `ass_gaussian_blur`
    /// возвращает `false`).
    #[error("blurred bitmap would be too large: {width}x{height}")]
    TooLarge {
        /// Ширина результата в пикселях.
        width: u64,
        /// Высота результата в пикселях.
        height: u64,
    },
    /// Увеличенный битмап шире или выше заданного вызывающим предела стороны.
    #[error("blurred bitmap {width}x{height} exceeds the side limit {limit}")]
    ExceedsSideLimit {
        /// Ширина результата в пикселях.
        width: u64,
        /// Высота результата в пикселях.
        height: u64,
        /// Предел стороны.
        limit: u32,
    },
    /// Размеры, шаг строки и длина буфера входного битмапа противоречат друг другу.
    #[error("bitmap geometry is inconsistent with its buffer")]
    InconsistentGeometry,
}

/// Предел стороны результата: размытие не выделяет битмап больше этого (защита холста).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BitmapSideLimit(pub(crate) u32);

impl BitmapSideLimit {
    /// Без ограничения сверх размеров, допустимых `CoverageBitmap`.
    #[cfg(test)]
    pub(crate) const UNLIMITED: Self = Self(u32::MAX);
}

/// Дисперсия гауссиана по осям в пикселях в квадрате (libass: `r2x`, `r2y`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GaussianVariance {
    /// По горизонтали.
    pub(crate) x: f64,
    /// По вертикали.
    pub(crate) y: f64,
}

impl GaussianVariance {
    /// Достаточна ли дисперсия, чтобы libass вообще запустил гауссиан (`ass_synth_blur`).
    pub(crate) fn is_visible(self) -> bool {
        self.x > GAUSSIAN_VARIANCE_THRESHOLD || self.y > GAUSSIAN_VARIANCE_THRESHOLD
    }
}

/// Полный набор размытия композита: гауссиан, затем box-размытие (libass: аргументы `ass_synth_blur`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BlurStrength {
    /// Гауссиан `\blur`.
    pub(crate) gaussian: GaussianVariance,
    /// Box-размытие `\be`.
    pub(crate) box_passes: BoxBlurPasses,
}

/// Рабочая память размытия; см. раздел «Владение буферами» в документации модуля.
#[derive(Debug, Default)]
pub(crate) struct BlurScratch {
    /// Плоскость-источник текущего шага каскада.
    front: Vec<i16>,
    /// Плоскость-приёмник текущего шага каскада.
    back: Vec<i16>,
    rows: RowScratch,
    box_rows: BoxScratch,
}

impl BlurScratch {
    /// Пустая рабочая память; буферы выделяются при первом размытии.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Освобождает накопленные буферы (следующее размытие выделит их заново).
    pub(crate) fn release_memory(&mut self) {
        *self = Self::default();
    }

    /// Освобождает буферы, если вся удержанная память (плоскости каскада, строки ядер и
    /// строки box-размытия) больше `max_bytes`: одно очень большое размытие не должно навсегда
    /// оставлять память занятой.
    pub(crate) fn release_if_larger_than(&mut self, max_bytes: usize) {
        if self.retained_bytes() > max_bytes {
            self.release_memory();
        }
    }

    /// Сколько байт держит рабочая память между кадрами: плоскости каскада, буферы строк ядер
    /// и буферы строк box-размытия (то, что освобождает [`Self::release_memory`]).
    pub(crate) fn retained_bytes(&self) -> usize {
        (self.front.capacity() + self.back.capacity()) * size_of::<i16>()
            + self.rows.retained_bytes()
            + self.box_rows.retained_bytes()
    }
}

/// Размытие композита целиком: гауссиан (если дисперсия видима), затем `\be`
/// (libass: `ass_synth_blur`).
///
/// Пустой битмап (`width == 0` или `height == 0`) не меняется — как `!bm->buffer` в C.
///
/// Если гауссиан пропущен из-за размера (`TooLarge`/`ExceedsSideLimit`), `\be`-проходы всё
/// равно выполняются, а ошибка пропуска возвращается после них.
pub(crate) fn synth_blur(
    scratch: &mut BlurScratch,
    bitmap: &mut CoverageBitmap,
    strength: BlurStrength,
    limit: BitmapSideLimit,
) -> Result<(), BlurError> {
    if bitmap.width == 0 || bitmap.height == 0 {
        return Ok(());
    }
    if strength.gaussian.is_visible() {
        match gaussian_blur(scratch, bitmap, strength.gaussian, limit) {
            Ok(()) => {}
            // libass: `ass_gaussian_blur` при нехватке памяти возвращает `false` до изменений,
            // а `ass_synth_blur` результат не проверяет и всё равно применяет `\be`. Поэтому
            // пропущенный гауссиан не отменяет box-проходы; вызывающий узнаёт о пропуске по
            // возвращённой ошибке (после того как box-блюр отработал).
            Err(skipped @ (BlurError::TooLarge { .. } | BlurError::ExceedsSideLimit { .. })) => {
                box_blur(scratch, bitmap, strength.box_passes)?;
                return Err(skipped);
            }
            Err(other) => return Err(other),
        }
    }
    box_blur(scratch, bitmap, strength.box_passes)
}

/// Проверяет согласованность размеров входного битмапа и возвращает `(width, height)`.
fn checked_dimensions(bitmap: &CoverageBitmap) -> Result<(usize, usize), BlurError> {
    let width = usize::try_from(bitmap.width).map_err(|_| BlurError::InconsistentGeometry)?;
    let height = usize::try_from(bitmap.height).map_err(|_| BlurError::InconsistentGeometry)?;
    let required = bitmap.stride.checked_mul(height);
    if bitmap.stride < width || required.is_none_or(|bytes| bitmap.buffer.len() < bytes) {
        return Err(BlurError::InconsistentGeometry);
    }
    Ok((width, height))
}

/// Геометрия результата гауссиана, посчитанная до выделения памяти.
struct GaussianPlan {
    method_x: BlurMethod,
    method_y: BlurMethod,
    end_width: usize,
    end_height: usize,
}

/// Считает размеры результата и проверяет пределы (libass: начало `ass_gaussian_blur`).
fn plan_gaussian(
    width: usize,
    height: usize,
    variance: GaussianVariance,
    limit: BitmapSideLimit,
) -> Result<GaussianPlan, BlurError> {
    let valid = |value: f64| value.is_finite() && value >= 0.0;
    if !valid(variance.x) || !valid(variance.y) {
        return Err(BlurError::InvalidVariance {
            x: variance.x,
            y: variance.y,
        });
    }
    let method_x = find_best_method(variance.x);
    // C: при равных дисперсиях метод копируется, а не считается заново (результат тот же).
    let method_y = if variance.y == variance.x {
        method_x
    } else {
        find_best_method(variance.y)
    };
    let too_large = |width: u64, height: u64| BlurError::TooLarge { width, height };
    if method_x.level > MAX_CASCADE_LEVEL || method_y.level > MAX_CASCADE_LEVEL {
        return Err(too_large(u64::MAX, u64::MAX));
    }
    // offset = ((2 r + 9) << level) - 5; end = ((size + offset) & ~((1 << level) - 1)) - 4.
    let end_size = |size: usize, method: BlurMethod| -> u64 {
        let offset = (((2 * method.radius + 9) as u64) << method.level) - 5;
        let mask = !((1u64 << method.level) - 1);
        ((size as u64 + offset) & mask) - 4
    };
    let end_width = end_size(width, method_x);
    let end_height = end_size(height, method_y);

    let stride = plane_stride(usize::try_from(end_width).unwrap_or(usize::MAX)) as u64;
    let bytes = stride.saturating_mul(end_height);
    // C: размер плоскости считается по полосам в 8 отсчётов.
    let plane_samples = end_width
        .div_ceil(8)
        .saturating_mul(8)
        .saturating_mul(end_height);
    if bytes > MAX_BITMAP_BYTES || plane_samples > MAX_PLANE_SAMPLES {
        return Err(too_large(end_width, end_height));
    }
    if end_width > u64::from(limit.0) || end_height > u64::from(limit.0) {
        return Err(BlurError::ExceedsSideLimit {
            width: end_width,
            height: end_height,
            limit: limit.0,
        });
    }
    Ok(GaussianPlan {
        method_x,
        method_y,
        end_width: end_width as usize,
        end_height: end_height as usize,
    })
}

/// Приближённое гауссово размытие с каскадом уменьшений (libass: `ass_gaussian_blur`).
///
/// Увеличивает битмап: `left -= ((radius_x + 4) << level_x) - 4`, `top` аналогично.
/// Выполняется всегда, даже при нулевой дисперсии по одной оси (как в C);
/// решение «размывать ли вообще» — в [`synth_blur`]. При ошибке битмап не меняется.
pub(crate) fn gaussian_blur(
    scratch: &mut BlurScratch,
    bitmap: &mut CoverageBitmap,
    variance: GaussianVariance,
    limit: BitmapSideLimit,
) -> Result<(), BlurError> {
    let (width, height) = checked_dimensions(bitmap)?;
    if width == 0 || height == 0 {
        return Ok(());
    }
    let plan = plan_gaussian(width, height, variance, limit)?;
    let (method_x, method_y) = (plan.method_x, plan.method_y);

    // Распаковка в 16-бит плоскость `front`.
    let mut plane = Plane { width, height };
    let BlurScratch {
        front,
        back,
        rows,
        box_rows: _,
    } = scratch;
    ensure_len(front, plane.len());
    unpack(
        &mut front[..plane.len()],
        &bitmap.buffer,
        bitmap.stride,
        plane,
    );

    // Шаги каскада в порядке C: shrink_vert × ly, shrink_horz × lx, blur_horz, blur_vert,
    // expand_horz × lx, expand_vert × ly. Каждый шаг пишет в `back` и меняет буферы местами.
    for _ in 0..method_y.level {
        let next = Plane {
            width: plane.width,
            height: (plane.height + 5) >> 1,
        };
        ensure_len(back, next.len());
        shrink_vert(&mut back[..next.len()], &front[..plane.len()], plane, rows);
        std::mem::swap(front, back);
        plane = next;
    }
    for _ in 0..method_x.level {
        let next = Plane {
            width: (plane.width + 5) >> 1,
            height: plane.height,
        };
        ensure_len(back, next.len());
        shrink_horz(&mut back[..next.len()], &front[..plane.len()], plane, rows);
        std::mem::swap(front, back);
        plane = next;
    }
    let next = Plane {
        width: plane.width + 2 * method_x.radius,
        height: plane.height,
    };
    ensure_len(back, next.len());
    blur_horz_dispatch(
        method_x.radius,
        &mut back[..next.len()],
        &front[..plane.len()],
        plane,
        &method_x.coeff,
        rows,
    );
    std::mem::swap(front, back);
    plane = next;
    let next = Plane {
        width: plane.width,
        height: plane.height + 2 * method_y.radius,
    };
    ensure_len(back, next.len());
    blur_vert_dispatch(
        method_y.radius,
        &mut back[..next.len()],
        &front[..plane.len()],
        plane,
        &method_y.coeff,
        rows,
    );
    std::mem::swap(front, back);
    plane = next;
    for _ in 0..method_x.level {
        let next = Plane {
            width: 2 * plane.width + 4,
            height: plane.height,
        };
        ensure_len(back, next.len());
        expand_horz(&mut back[..next.len()], &front[..plane.len()], plane, rows);
        std::mem::swap(front, back);
        plane = next;
    }
    for _ in 0..method_y.level {
        let next = Plane {
            width: plane.width,
            height: 2 * plane.height + 4,
        };
        ensure_len(back, next.len());
        expand_vert(&mut back[..next.len()], &front[..plane.len()], plane, rows);
        std::mem::swap(front, back);
        plane = next;
    }
    // Каскад по построению даёт ровно рассчитанный размер (assert в C).
    debug_assert_eq!(
        (plane.width, plane.height),
        (plan.end_width, plan.end_height)
    );

    // Упаковка в новый буфер битмапа; старый заменяется целиком (как ass_realloc_bitmap).
    let mut packed = vec![0u8; plane.len()];
    pack(&mut packed, &front[..plane.len()], plane);
    bitmap.left -= (((method_x.radius + 4) << method_x.level) - 4) as i32;
    bitmap.top -= (((method_y.radius + 4) << method_y.level) - 4) as i32;
    bitmap.width = plane.width as i32;
    bitmap.height = plane.height as i32;
    bitmap.stride = plane.stride();
    bitmap.buffer = packed;
    Ok(())
}

/// Растёт до `len` нулями; содержимое не очищает (шаги каскада перезаписывают всё).
fn ensure_len(buffer: &mut Vec<i16>, len: usize) {
    if buffer.len() < len {
        buffer.resize(len, 0);
    }
}

/// Box-размытие `\be` на месте (libass: часть `ass_synth_blur` после гауссиана).
///
/// `passes == 0` — без изменений. `\be = 1` — один 8-бит проход; больше — 6-бит серия
/// (`pre`, `passes - 1` проходов, `post`) и финальный 8-бит проход. Размер битмапа не
/// меняется: отступ под размытие должен быть заложен композитом ([`BoxBlurPasses::padding_px`]).
pub(crate) fn box_blur(
    scratch: &mut BlurScratch,
    bitmap: &mut CoverageBitmap,
    passes: BoxBlurPasses,
) -> Result<(), BlurError> {
    let (width, height) = checked_dimensions(bitmap)?;
    let stride = bitmap.stride;
    let buf = &mut bitmap.buffer[..];
    let rows = &mut scratch.box_rows;
    let mut remaining = passes.count();
    if remaining == 0 {
        return Ok(());
    }
    remaining -= 1;
    if remaining > 0 {
        scale_down_for_passes(buf, stride, width, height);
        for _ in 0..remaining {
            box_blur_pass(buf, stride, width, height, rows);
        }
        scale_up_after_passes(buf, stride, width, height);
    }
    box_blur_pass(buf, stride, width, height, rows);
    Ok(())
}
