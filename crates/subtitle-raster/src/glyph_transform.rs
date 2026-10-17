// Порт из libass 0.17.5: ass_render.c (quantize_transform, restore_transform,
// ass_bitmap_construct, часть get_bitmap_glyph про точность обводки).
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

//! Квантование преобразования контура в экранные координаты (без поворотов и перспективы).
//!
//! libass не растеризует контур «как есть»: перед растеризацией матрица `контур -> экран`
//! квантуется (позиция до 1/8 пикселя, коэффициенты матрицы до заданной точности), чтобы
//! одинаковые глифы в близких позициях давали одну и ту же маску (кэш битмапов). Для
//! побитного совпадения с libass квантование обязано быть тем же, поэтому здесь сохранены
//! числа, порядок операций и округление `double`.
//!
//! Модуль общий для производственного конвейера текста и для тестового конвейера рисунков
//! (`libass_parity`): оба обязаны вести себя как один и тот же код libass.

#![allow(
    clippy::needless_range_loop,
    reason = "матричные циклы повторяют индексы из ass_render.c один в один"
)]

use crate::bitmap::{BitmapError, CoverageBitmap, RasterRegion, outline_to_bitmap_within};
use crate::outline::{ControlBox, OUTLINE_MAX, Outline, Point, round_ties_even_to_i32};
use crate::raster::Rasterizer;

mod rotation;
pub(crate) use rotation::{GlyphPlacement, Rotation3d, ShearFactors, glyph_transform_matrix};

/// Допустимая ошибка обводки в единицах контура (libass: `STROKER_PRECISION`).
pub(crate) const STROKER_PRECISION: i32 = 16;
/// Допустимая ошибка аппроксимации сплайнов (libass: `RASTERIZER_PRECISION`).
pub(crate) const RASTERIZER_PRECISION: i32 = 16;
/// Грубая оценка ошибки преобразования в 1/64 пикселя (libass: `POSITION_PRECISION`).
pub(crate) const POSITION_PRECISION: f64 = 8.0;
/// Порядок субпиксельного квантования позиции (libass: `SUBPIXEL_ORDER`, 1/8 пикселя).
pub(crate) const SUBPIXEL_ORDER: i32 = 3;
/// Предел масштаба перспективы (libass: `MAX_PERSP_SCALE`).
pub(crate) const MAX_PERSP_SCALE: f64 = 16.0;
/// Базовое расстояние «камеры» в матрице глифа; libass умножает его на `blur_scale_y` кадра
/// (`calc_transform_matrix`: `dist = 20000 * blur_scale_y`), см. `GlyphPlacement::camera_scale`.
pub(crate) const CAMERA_DISTANCE: f64 = 20000.0;

/// Матрица 3x3 преобразования контура (однородные координаты, как в libass).
pub(crate) type Matrix3 = [[f64; 3]; 3];

/// Результат `quantize_transform` (libass: поля `BitmapHashKey`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct QuantizedTransform {
    /// Целая часть позиции центра в пикселях.
    pub(crate) position: Point,
    /// Дробная часть позиции в 1/8 пикселя (0..8).
    pub(crate) offset: [i32; 2],
    /// Первая строка квантованной матрицы 2x2.
    pub(crate) matrix_x: [i32; 2],
    /// Вторая строка квантованной матрицы 2x2.
    pub(crate) matrix_y: [i32; 2],
    /// Квантованная строка перспективы (`m_zx`, `m_zy`); `[0, 0]` — аффинное преобразование
    /// без поворотов вокруг осей X и Y, контур переводится быстрым 2D-путём.
    pub(crate) matrix_z: [i32; 2],
}

impl QuantizedTransform {
    /// У преобразования есть перспектива: контур переводится через деление на `z`
    /// (libass: `matrix_z.x || matrix_z.y`).
    pub(crate) fn has_perspective(&self) -> bool {
        self.matrix_z != [0, 0]
    }
}

/// `|value| < limit` (в C: `fabs(v) < max`); NaN даёт `false`.
fn is_within(value: f64, limit: f64) -> bool {
    value.abs() < limit
}

/// Показатель степени двойки `e` из `frexp`: `x = m * 2^e`, `m` в `[0.5, 1)`.
///
/// Для нуля, бесконечностей, NaN и денормализованных чисел возвращает 0: такие входы
/// отсекаются раньше (`is_within`), а масштаб обводки всегда конечный и положительный.
fn frexp_exponent(value: f64) -> i32 {
    if value == 0.0 || !value.is_finite() {
        return 0;
    }
    let biased = ((value.to_bits() >> 52) & 0x7ff) as i32;
    if biased == 0 {
        return 0;
    }
    biased - 1022
}

/// `ldexp(value, exponent)`.
fn ldexp(value: f64, exponent: i32) -> f64 {
    value * 2.0_f64.powi(exponent)
}

/// Матрица глифа без поворотов и сдвигов (libass: `calc_transform_matrix` при нулевых
/// `frx/fry/frz/fax/fay`, `par_scale_x = 1`): переносит начало координат в точку
/// `(pos_x, pos_y)` в 1/64 пикселя. Нужна эталонным тестам рисунков, где повороты не
/// поддерживаются, а `par_scale_x = 1`; текст строит матрицу через [`glyph_transform_matrix`].
#[cfg(test)]
pub(crate) fn unrotated_glyph_matrix(pos_x: i32, pos_y: i32) -> Matrix3 {
    [
        [CAMERA_DISTANCE, 0.0, CAMERA_DISTANCE * f64::from(pos_x)],
        [0.0, CAMERA_DISTANCE, CAMERA_DISTANCE * f64::from(pos_y)],
        [0.0, 0.0, CAMERA_DISTANCE],
    ]
}

/// Применяет масштаб и сдвиг контура к матрице глифа (libass: `get_bitmap_glyph`, вычисление
/// `m2` из `m1`, `tr->scale` и `tr->offset`).
pub(crate) fn scale_and_offset_matrix(
    base: &Matrix3,
    scale: (f64, f64),
    offset: (f64, f64),
) -> Matrix3 {
    let mut scaled = *base;
    for (row, source) in scaled.iter_mut().zip(base.iter()) {
        row[0] = source[0] * scale.0;
        row[1] = source[1] * scale.1;
        row[2] = source[0] * offset.0 + source[1] * offset.1 + source[2];
    }
    scaled
}

/// Порт `quantize_transform` (libass: `ass_render.c`), включая квантование перспективы.
///
/// `matrix` меняется на месте (начало координат переносится в центр рамки), `offset` — вход
/// (при `first = false`) и выход (при `first = true`): глифы одного участка делят дробную
/// ошибку позиции первого глифа.
pub(crate) fn quantize_transform(
    matrix: &mut Matrix3,
    offset: &mut [f64; 2],
    first: bool,
    cbox: &ControlBox,
) -> Option<QuantizedTransform> {
    const MAX_VALUE: f64 = 1_000_000.0;
    let x0 = (f64::from(cbox.x_min) + f64::from(cbox.x_max)) / 2.0;
    let y0 = (f64::from(cbox.y_min) + f64::from(cbox.y_max)) / 2.0;
    let dx = (f64::from(cbox.x_max) - f64::from(cbox.x_min)) / 2.0 + 64.0;
    let dy = (f64::from(cbox.y_max) - f64::from(cbox.y_min)) / 2.0 + 64.0;

    for row in matrix.iter_mut() {
        row[2] += row[0] * x0 + row[1] * y0;
    }
    if matrix[2][2] <= 0.0 {
        return None;
    }

    let w = 1.0 / matrix[2][2];
    let mut center = [matrix[0][2] * w, matrix[1][2] * w];
    for i in 0..2 {
        for j in 0..2 {
            matrix[i][j] -= matrix[2][j] * center[i];
        }
    }

    let delta = if first { [0.0, 0.0] } else { *offset };
    let mut quantized_center = [0_i32; 2];
    for i in 0..2 {
        center[i] /= f64::from(64 >> SUBPIXEL_ORDER);
        center[i] -= delta[i];
        if !is_within(center[i], MAX_VALUE) {
            return None;
        }
        quantized_center[i] = round_ties_even_to_i32(center[i]);
    }

    let z0 = matrix[2][2] - matrix[2][0].abs() * dx - matrix[2][1].abs() * dy;
    let w = 1.0 / POSITION_PRECISION / z0.max(matrix[2][2] / MAX_PERSP_SCALE);
    let mul = [dx * w, dy * w];

    let mut quantized = [[0_i32; 2]; 2];
    for i in 0..2 {
        for j in 0..2 {
            let value = matrix[i][j] * mul[j];
            if !is_within(value, MAX_VALUE) {
                return None;
            }
            quantized[i][j] = round_ties_even_to_i32(value);
        }
    }

    // Строка перспективы квантуется с шагом, привязанным к уже квантованной матрице 2x2
    // (комментарий к выводу шагов — в `quantize_transform` libass).
    let qmx = quantized[0][0].abs() + quantized[0][1].abs();
    let qmy = quantized[1][0].abs() + quantized[1][1].abs();
    let scale_z = POSITION_PRECISION * f64::from(qmx.max(qmy));
    let mut quantized_z = [0_i32; 2];
    for j in 0..2 {
        let value = matrix[2][j] * mul[j] * scale_z;
        if !is_within(value, MAX_VALUE) {
            return None;
        }
        quantized_z[j] = round_ties_even_to_i32(value);
    }

    if first {
        offset[0] = center[0] - f64::from(quantized_center[0]);
        offset[1] = center[1] - f64::from(quantized_center[1]);
    }
    Some(QuantizedTransform {
        position: Point::new(
            quantized_center[0] >> SUBPIXEL_ORDER,
            quantized_center[1] >> SUBPIXEL_ORDER,
        ),
        offset: [
            quantized_center[0] & ((1 << SUBPIXEL_ORDER) - 1),
            quantized_center[1] & ((1 << SUBPIXEL_ORDER) - 1),
        ],
        matrix_x: quantized[0],
        matrix_y: quantized[1],
        matrix_z: quantized_z,
    })
}

/// Порт `restore_transform`: матрица 3x3 для `transformed_2d`/`transformed_3d` по
/// квантованному ключу.
///
/// Без перспективы (`matrix_z == 0`) третья строка равна `[0, 0, 1]` и годятся первые две
/// строки; с перспективой нужна вся матрица.
pub(crate) fn restore_transform(key: &QuantizedTransform, cbox: &ControlBox) -> Matrix3 {
    let x0 = (f64::from(cbox.x_min) + f64::from(cbox.x_max)) / 2.0;
    let y0 = (f64::from(cbox.y_min) + f64::from(cbox.y_max)) / 2.0;
    let dx = (f64::from(cbox.x_max) - f64::from(cbox.x_min)) / 2.0 + 64.0;
    let dy = (f64::from(cbox.y_max) - f64::from(cbox.y_min)) / 2.0 + 64.0;

    let q_x = POSITION_PRECISION / dx;
    let q_y = POSITION_PRECISION / dy;
    let mut m = [[0.0_f64; 3]; 3];
    m[0][0] = f64::from(key.matrix_x[0]) * q_x;
    m[0][1] = f64::from(key.matrix_x[1]) * q_y;
    m[1][0] = f64::from(key.matrix_y[0]) * q_x;
    m[1][1] = f64::from(key.matrix_y[1]) * q_y;

    let qmx = key.matrix_x[0].abs() + key.matrix_x[1].abs();
    let qmy = key.matrix_y[0].abs() + key.matrix_y[1].abs();
    // libass делит на `max(qmx, qmy)` без проверки: при вырожденной матрице получается
    // бесконечность и NaN (битмапа нет). Здесь вырожденный случай даёт нулевую перспективу.
    let denominator = f64::from(qmx.max(qmy));
    let scale_z = if denominator > 0.0 {
        1.0 / POSITION_PRECISION / denominator
    } else {
        0.0
    };
    m[2][0] = f64::from(key.matrix_z[0]) * q_x * scale_z;
    m[2][1] = f64::from(key.matrix_z[1]) * q_y * scale_z;

    m[2][2] = (1.0 + m[2][0].abs() * dx + m[2][1].abs() * dy).min(MAX_PERSP_SCALE);

    let center = [
        f64::from(key.offset[0] * (64 >> SUBPIXEL_ORDER)),
        f64::from(key.offset[1] * (64 >> SUBPIXEL_ORDER)),
    ];
    for i in 0..2 {
        for j in 0..3 {
            m[i][j] += m[2][j] * center[i];
        }
    }
    for row in &mut m {
        row[2] -= row[0] * x0 + row[1] * y0;
    }
    m
}

/// Рамка контрольных точек нескольких контуров; пустая -> нули (libass: `cbox`).
pub(crate) fn control_box(outlines: &[&Outline]) -> ControlBox {
    let mut cbox = ControlBox::EMPTY;
    for outline in outlines {
        outline.update_control_box(&mut cbox);
    }
    if cbox.is_empty() {
        ControlBox {
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
        }
    } else {
        cbox
    }
}

/// Растеризует контур(ы) по квантованному ключу (libass: `ass_bitmap_construct`).
///
/// `None` — контур пуст или преобразование вышло за допустимый диапазон (libass в этом
/// случае тоже оставляет битмап нулевым); ошибки растеризатора возвращаются как есть.
pub(crate) fn construct_bitmap(
    rasterizer: &mut Rasterizer,
    key: &QuantizedTransform,
    cbox: &ControlBox,
    outlines: [&Outline; 2],
) -> Result<Option<CoverageBitmap>, BitmapError> {
    construct_bitmap_within(rasterizer, key, cbox, outlines, RasterRegion::Everything)
}

/// [`construct_bitmap`], но растеризуется только часть внутри `region` (координаты - как у
/// битмапа, то есть относительно позиции `key.position`).
pub(crate) fn construct_bitmap_within(
    rasterizer: &mut Rasterizer,
    key: &QuantizedTransform,
    cbox: &ControlBox,
    outlines: [&Outline; 2],
    region: RasterRegion,
) -> Result<Option<CoverageBitmap>, BitmapError> {
    let matrix = restore_transform(key, cbox);
    // Перспектива есть только при поворотах вокруг осей X/Y: тогда нужен путь с делением на z.
    let transformed = if key.has_perspective() {
        (
            outlines[0].transformed_3d(&matrix),
            outlines[1].transformed_3d(&matrix),
        )
    } else {
        let affine = [matrix[0], matrix[1]];
        (
            outlines[0].transformed_2d(&affine),
            outlines[1].transformed_2d(&affine),
        )
    };
    let (Ok(first), Ok(second)) = transformed else {
        // libass: `ass_outline_transform_*` не удалась -> `memset(bm, 0)`, битмапа нет.
        return Ok(None);
    };
    outline_to_bitmap_within(rasterizer, Some(&first), Some(&second), region)
}

/// Параметры обводки, посчитанные по матрице (libass: середина `get_bitmap_glyph`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BorderStrokePlan {
    /// Порядок степени двойки, на который масштабируется исходный контур по X.
    pub(crate) scale_order_x: i32,
    /// То же по Y.
    pub(crate) scale_order_y: i32,
    /// Толщина обводки по X в единицах `STROKER_PRECISION` (не умножена на точность).
    pub(crate) border_x: i32,
    /// То же по Y.
    pub(crate) border_y: i32,
}

/// Выбирает масштаб контура перед обводкой и квантованную толщину обводки.
///
/// `matrix` — матрица глифа ПОСЛЕ `quantize_transform` (начало координат в центре рамки);
/// `outline_box` — рамка исходного контура; `border` — толщина обводки в единицах контура
/// (1/64 пикселя контура, уже поделённая на масштаб глифа). `None` — обводка не помещается в
/// `OUTLINE_MAX` (libass в этом случае отбрасывает обводку).
pub(crate) fn plan_border_stroke(
    matrix: &Matrix3,
    outline_box: &ControlBox,
    border: (f64, f64),
) -> Option<BorderStrokePlan> {
    let (bord_x, bord_y) = border;
    // Оценка половины габаритов контура после обводки.
    let dx = f64::from(outline_box.x_max - outline_box.x_min) / 2.0 + (bord_x + 64.0);
    let dy = f64::from(outline_box.y_max - outline_box.y_min) / 2.0 + (bord_y + 64.0);
    let (mxx, mxy) = (matrix[0][0].abs(), matrix[0][1].abs());
    let (myx, myy) = (matrix[1][0].abs(), matrix[1][1].abs());
    let (mzx, mzy) = (matrix[2][0].abs(), matrix[2][1].abs());

    let z0 = matrix[2][2] - mzx * dx - mzy * dy;
    let mut w = 1.0 / z0.max(matrix[2][2] / MAX_PERSP_SCALE);
    let x_lim = mxx * dx + mxy * dy;
    let y_lim = myx * dx + myy * dy;
    let rz = x_lim.max(y_lim) * w;

    w *= f64::from(STROKER_PRECISION) / POSITION_PRECISION;
    let scale_order_x = frexp_exponent(w * (mxx.max(myx) + mzx * rz));
    let scale_order_y = frexp_exponent(w * (mxy.max(myy) + mzy * rz));
    let scaled_x = ldexp(bord_x, scale_order_x);
    let scaled_y = ldexp(bord_y, scale_order_y);
    // Отрицание условия ловит и NaN (libass: `!(bord_x < OUTLINE_MAX && bord_y < OUTLINE_MAX)`).
    if !(scaled_x < f64::from(OUTLINE_MAX) && scaled_y < f64::from(OUTLINE_MAX)) {
        return None;
    }
    Some(BorderStrokePlan {
        scale_order_x,
        scale_order_y,
        border_x: round_ties_even_to_i32(scaled_x / f64::from(STROKER_PRECISION)),
        border_y: round_ties_even_to_i32(scaled_y / f64::from(STROKER_PRECISION)),
    })
}

/// Матрица глифа для обводки: колонки базовой матрицы делятся на масштаб контура обводки
/// (libass: `m[i][0] = ldexp(m2[i][0], -scale_ord_x)` и т.д.).
pub(crate) fn border_matrix(scaled: &Matrix3, plan: &BorderStrokePlan) -> Matrix3 {
    let mut result = *scaled;
    for row in &mut result {
        row[0] = ldexp(row[0], -plan.scale_order_x);
        row[1] = ldexp(row[1], -plan.scale_order_y);
    }
    result
}

#[cfg(test)]
mod tests;
