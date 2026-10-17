// Порт из libass 0.17.5: ass_blur.c (calc_gauss, coeff_filter, calc_matrix, calc_coeff,
// find_best_method).
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

//! Выбор параметров каскадного гауссова размытия по дисперсии (`ass_blur.c:30-147`).
//!
//! Идея libass: большой радиус = уменьшить картинку вдвое `level` раз, применить
//! фильтр с небольшим ядром (9..17 отсчётов), увеличить обратно. Коэффициенты ядра
//! получаются из решения задачи наименьших квадратов для Фурье-образа результата.
//! Все вычисления в `f64` повторяют порядок операций C-кода: от этого зависит
//! побитовое совпадение целых коэффициентов.

// Численный порт: индексные циклы повторяют формулы C один в один и читаются лучше итераторов.
#![allow(clippy::needless_range_loop)]

use super::params::frexp;

/// Наименьший и наибольший радиус основного фильтра (отсчётов в одну сторону).
const MIN_RADIUS: usize = 4;
const MAX_RADIUS: usize = 8;

/// Способ размытия вдоль одной оси (libass: `BlurMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlurMethod {
    /// Сколько раз картинку уменьшают вдвое перед фильтром.
    pub(super) level: u32,
    /// Радиус основного фильтра, `4..=8`.
    pub(super) radius: usize,
    /// Коэффициенты ядра в формате с фиксированной точкой 0.16; используются первые `radius`.
    pub(super) coeff: [i16; 8],
}

/// Выбирает уровень, радиус и коэффициенты для дисперсии `r2` (libass: `find_best_method`).
///
/// `r2` должна быть конечной и неотрицательной — это проверяет вызывающий.
pub(super) fn find_best_method(r2: f64) -> BlurMethod {
    let mut mu = [0.0f64; MAX_RADIUS];
    let level;
    let radius;
    if r2 < 0.5 {
        level = 0;
        radius = MIN_RADIUS;
        mu[1] = 0.085 * r2 * r2 * r2;
        mu[0] = 0.5 * r2 - 4.0 * mu[1];
        mu[2] = 0.0;
        mu[3] = 0.0;
    } else {
        let (frac, exponent) = frexp((0.11569 * r2 + 0.20591047).sqrt());
        // Дисперсия конечна, поэтому порядок неотрицателен: аргумент frexp не меньше 0.45.
        level = exponent.max(0) as u32;
        let mul = 0.25f64.powi(level.min(i32::MAX as u32) as i32);
        // `(int)` в C усекает к нулю, `as i32` делает то же.
        let reduced = (((10.1525 + 0.8335 * mul) * (1.0 - frac)) as i32).max(0);
        radius = ((8 - reduced).max(MIN_RADIUS as i32) as usize).min(MAX_RADIUS);
        calc_coeff(&mut mu, radius, r2, mul);
    }
    let mut coeff = [0i16; 8];
    for (slot, &weight) in coeff.iter_mut().zip(&mu).take(radius) {
        // C: `blur->coeff[i] = (int) (0x10000 * mu[i] + 0.5)` с неявным усечением до int16_t.
        *slot = (0x10000 as f64 * weight + 0.5) as i32 as i16;
    }
    BlurMethod {
        level,
        radius,
        coeff,
    }
}

/// Значения гауссианы `exp(-x^2 / (2 r2)) / sqrt(2 pi r2)` в точках `0..n` (libass: `calc_gauss`).
fn calc_gauss(res: &mut [f64], n: usize, r2: f64) {
    let alpha = 0.5 / r2;
    let mut mul = (-alpha).exp();
    let mul2 = mul * mul;
    let mut cur = (alpha / std::f64::consts::PI).sqrt();

    res[0] = cur;
    cur *= mul;
    res[1] = cur;
    for slot in res.iter_mut().take(n).skip(2) {
        mul *= mul2;
        cur *= mul;
        *slot = cur;
    }
}

/// Свёртка частотной характеристики с 7-отсчётным ядром уменьшения/увеличения, на месте
/// (libass: `coeff_filter`).
fn coeff_filter(coeff: &mut [f64], n: usize, kernel: &[f64; 4]) {
    let mut prev1 = coeff[1];
    let mut prev2 = coeff[2];
    let mut prev3 = coeff[3];
    for i in 0..n {
        let res = coeff[i] * kernel[0]
            + (prev1 + coeff[i + 1]) * kernel[1]
            + (prev2 + coeff[i + 2]) * kernel[2]
            + (prev3 + coeff[i + 3]) * kernel[3];
        prev3 = prev2;
        prev2 = prev1;
        prev1 = coeff[i];
        coeff[i] = res;
    }
}

/// Строит и обращает матрицу нормальных уравнений (libass: `calc_matrix`).
fn calc_matrix(mat: &mut [[f64; 8]; 8], mat_freq: &[f64], n: usize) {
    for i in 0..n {
        mat[i][i] = mat_freq[2 * i + 2] + 3.0 * mat_freq[0] - 4.0 * mat_freq[i + 1];
        for j in (i + 1)..n {
            let value = mat_freq[i + j + 2]
                + mat_freq[j - i]
                + 2.0 * (mat_freq[0] - mat_freq[i + 1] - mat_freq[j + 1]);
            mat[i][j] = value;
            mat[j][i] = value;
        }
    }

    // Обращение (транспонированное) методом Гаусса–Жордана без выбора главного элемента.
    for k in 0..n {
        let z = 1.0 / mat[k][k];
        mat[k][k] = 1.0;
        for i in 0..n {
            if i == k {
                continue;
            }
            let mul = mat[i][k] * z;
            mat[i][k] = 0.0;
            for j in 0..n {
                let pivot_row_value = mat[k][j];
                mat[i][j] -= pivot_row_value * mul;
            }
        }
        for value in mat[k].iter_mut().take(n) {
            *value *= z;
        }
    }
}

/// Решает задачу наименьших квадратов для ядра основного фильтра (libass: `calc_coeff`).
///
/// `n` — радиус ядра (`1..=8`), `r2` — желаемая дисперсия, `mul` — множитель масштаба
/// (`0.25^level`).
fn calc_coeff(mu: &mut [f64; 8], n: usize, r2: f64, mul: f64) {
    const W: f64 = 12096.0;
    let kernel = [
        (((3280.0 / W) * mul + 1092.0 / W) * mul + 2520.0 / W) * mul + 5204.0 / W,
        (((-2460.0 / W) * mul - 273.0 / W) * mul - 210.0 / W) * mul + 2943.0 / W,
        (((984.0 / W) * mul - 546.0 / W) * mul - 924.0 / W) * mul + 486.0 / W,
        (((-164.0 / W) * mul + 273.0 / W) * mul - 126.0 / W) * mul + 17.0 / W,
    ];

    let mut mat_freq = [0.0f64; 17];
    mat_freq[..4].copy_from_slice(&kernel);
    coeff_filter(&mut mat_freq, 7, &kernel);

    let mut vec_freq = [0.0f64; 12];
    calc_gauss(&mut vec_freq, n + 4, r2 * mul);
    coeff_filter(&mut vec_freq, n + 1, &kernel);

    let mut mat = [[0.0f64; 8]; 8];
    calc_matrix(&mut mat, &mat_freq, n);

    let mut vec = [0.0f64; 8];
    for (i, slot) in vec.iter_mut().enumerate().take(n) {
        *slot = mat_freq[0] - mat_freq[i + 1] - vec_freq[0] + vec_freq[i + 1];
    }

    for i in 0..n {
        let mut res = 0.0;
        for j in 0..n {
            res += mat[i][j] * vec[j];
        }
        // FFMAX(0, res): отрицательные веса отбрасываются.
        mu[i] = if 0.0 > res { 0.0 } else { res };
    }
}
