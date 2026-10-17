// Порт из libass 0.17.5: ass_render.c (calc_transform_matrix).
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

//! Матрица глифа с поворотами `\frx \fry \frz`, наклоном `\fax \fay` и перспективой.
//!
//! Это сердце «статических» тегов слоя 2. Глиф лежит в системе координат с началом в центре
//! поворота (`\org` либо опорная точка реплики): его положение относительно центра — это
//! `shift`. Матрица делает три шага:
//! 1. наклон (`fax`, `fay`) и сдвиг на `shift`; член `ascent * fax` держит опору наклона на
//!    базовой линии, а не на верхней кромке ячейки;
//! 2. повороты в порядке Z, X, Y;
//! 3. перспектива: камера стоит на расстоянии [`CAMERA_DISTANCE`] от плоскости кадра, поэтому
//!    поворот вокруг X/Y даёт настоящее сужение, а часть глифа может оказаться за камерой
//!    (`z <= 0`) — такой глиф `quantize_transform` отбрасывает.
//!
//! Порядок операций и числа повторяют `calc_transform_matrix` из libass (двойная точность
//! без перестановок), потому что результат потом квантуется до 1/8 пикселя и сравнивается с
//! эталоном побитно.

use super::{CAMERA_DISTANCE, Matrix3};
use crate::fixed_point::Vector26;

/// Углы поворота в градусах (`\frx`, `\fry`, `\frz`). Ноль по всем осям — без поворота.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Rotation3d {
    /// Вокруг горизонтальной оси: верх уходит от зрителя при положительном угле.
    pub(crate) x_degrees: f64,
    /// Вокруг вертикальной оси.
    pub(crate) y_degrees: f64,
    /// В плоскости кадра (против часовой стрелки при положительном угле).
    pub(crate) z_degrees: f64,
}

impl Rotation3d {
    /// Повернуто ли хоть по одной оси.
    pub(crate) fn is_rotated(&self) -> bool {
        self.x_degrees != 0.0 || self.y_degrees != 0.0 || self.z_degrees != 0.0
    }
}

/// Наклон букв (`\fax`, `\fay`), безразмерные коэффициенты. Ноль — без наклона.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct ShearFactors {
    /// `\fax`: горизонтальный сдвиг на единицу высоты.
    pub(crate) horizontal: f64,
    /// `\fay`: вертикальный сдвиг на единицу ширины.
    pub(crate) vertical: f64,
}

impl ShearFactors {
    /// Есть ли наклон по какой-либо оси.
    pub(crate) fn is_sheared(&self) -> bool {
        self.horizontal != 0.0 || self.vertical != 0.0
    }
}

/// Всё, что нужно матрице одного глифа.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GlyphPlacement {
    /// Повороты глифа.
    pub(crate) rotation: Rotation3d,
    /// Наклон глифа.
    pub(crate) shear: ShearFactors,
    /// Масштаб контура по осям (libass: `info->scale_x`, `info->scale_y`); обе величины > 0.
    pub(crate) scale: (f64, f64),
    /// Положение глифа относительно центра поворота, 26.6 в пикселях кадра до растяжения на
    /// `par_scale_x` (libass: `info->shift`; включает смещение тени).
    pub(crate) shift: Vector26,
    /// Подъём глифа в 26.6 (libass: `info->asc`): опора наклона `\fax`.
    pub(crate) ascent: i32,
    /// Положение глифа в кадре, 26.6 (libass: `info->pos` после `render_and_combine_glyphs`).
    pub(crate) frame_position: Vector26,
    /// `par_scale_x` libass (1.0, если пропорции хранения и кадра равны).
    pub(crate) pixel_aspect_x: f64,
}

/// Порт `calc_transform_matrix`: матрица «контур глифа -> экран» без масштаба контура.
///
/// Масштаб и сдвиг контура (`tr->scale`, `tr->offset`) применяются отдельно
/// ([`super::scale_and_offset_matrix`]).
pub(crate) fn glyph_transform_matrix(placement: &GlyphPlacement) -> Matrix3 {
    let to_radians = std::f64::consts::PI / 180.0;
    let frx = to_radians * placement.rotation.x_degrees;
    let fry = to_radians * placement.rotation.y_degrees;
    let frz = to_radians * placement.rotation.z_degrees;

    let (sx, cx) = (-frx.sin(), frx.cos());
    let (sy, cy) = (fry.sin(), fry.cos());
    let (sz, cz) = (-frz.sin(), frz.cos());

    let (scale_x, scale_y) = placement.scale;
    // Наклон пересчитывается в единицы контура: он задан относительно «квадратного» глифа.
    let fax = placement.shear.horizontal * scale_x / scale_y;
    let fay = placement.shear.vertical * scale_y / scale_x;
    let shift_x = f64::from(placement.shift.x);
    let shift_y = f64::from(placement.shift.y);
    let x1 = [1.0, fax, shift_x + f64::from(placement.ascent) * fax];
    let y1 = [fay, 1.0, shift_y];

    let mut x2 = [0.0_f64; 3];
    let mut y2 = [0.0_f64; 3];
    for i in 0..3 {
        x2[i] = x1[i] * cz - y1[i] * sz;
        y2[i] = x1[i] * sz + y1[i] * cz;
    }

    let mut y3 = [0.0_f64; 3];
    let mut z3 = [0.0_f64; 3];
    for i in 0..3 {
        y3[i] = y2[i] * cx;
        z3[i] = y2[i] * sx;
    }

    let mut x4 = [0.0_f64; 3];
    let mut z4 = [0.0_f64; 3];
    for i in 0..3 {
        x4[i] = x2[i] * cy - z3[i] * sy;
        z4[i] = x2[i] * sy + z3[i] * cy;
    }

    // Камера: у libass `20000 * blur_scale_y`; у нас `blur_scale_y = 1`.
    let dist = CAMERA_DISTANCE;
    z4[2] += dist;

    let scale_x_screen = dist * placement.pixel_aspect_x;
    let offset_x = f64::from(placement.frame_position.x) - shift_x * placement.pixel_aspect_x;
    let offset_y = f64::from(placement.frame_position.y) - shift_y;
    let mut matrix = [[0.0_f64; 3]; 3];
    for i in 0..3 {
        matrix[0][i] = z4[i] * offset_x + x4[i] * scale_x_screen;
        matrix[1][i] = z4[i] * offset_y + y3[i] * dist;
        matrix[2][i] = z4[i];
    }
    matrix
}
