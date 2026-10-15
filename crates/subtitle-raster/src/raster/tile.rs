// Порт из libass 0.17.5: ass_rasterizer.c, ass_rasterizer.h, c/rasterizer_template.h.
//
// Copyright (C) 2016 Vabishchevich Nikolay <vabnick@gmail.com>
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

//! Заливка одного тайла 16x16 (libass 0.17.5: `c/rasterizer_template.h` при `TILE_SIZE = 16`,
//! `TILE_ORDER = 4`; C-реализация без SIMD).
//!
//! Арифметика повторяет C: промежуточные значения считаются в `int` (здесь `i32`), а при
//! присваивании переменной `int16_t` усекаются до 16 бит. Поэтому каждое такое присваивание
//! оформлено через [`wrap16`] — это не ошибка, а точное воспроизведение переполнения int16.
//! Тайл 32x32 (`TILE_SIZE = 32`) не портирован: libass по умолчанию использует 16x16
//! (`ass_bitmap_engine_init`: `tile_order = 4`, если не задан `ASS_FLAG_LARGE_TILES`).

use super::RasterError;
use super::segment::{SEGFLAG_DN, SEGFLAG_EXACT_LEFT, SEGFLAG_UL_DR, Segment};

/// Порядок тайла (libass: `TILE_ORDER`).
pub(crate) const TILE_ORDER: u32 = 4;
/// Сторона тайла в пикселях.
pub(crate) const TILE_SIZE: usize = 1 << TILE_ORDER;
/// Полное покрытие в условных единицах тайла (libass: `FULL_VALUE = 1 << (14 - TILE_ORDER)`).
const FULL_VALUE: i32 = 1 << (14 - TILE_ORDER);

/// Усечение `int` до `int16_t` (присваивание в C).
fn wrap16(value: i32) -> i32 {
    i32::from(value as i16)
}

/// `RESCALE_AB` из libass для коэффициента полуплоскости.
fn rescale_ab(ab: i32, scale: i32) -> i32 {
    let rounded = i64::from(ab) * i64::from(scale) + (1_i64 << (45 + TILE_ORDER));
    wrap16((rounded >> (46 + TILE_ORDER)) as i32)
}

/// `RESCALE_C` из libass для свободного члена полуплоскости.
fn rescale_c(c: i64, scale: i32) -> i32 {
    let high = (c >> (7 + TILE_ORDER)) as i32;
    let rounded = i64::from(high) * i64::from(scale) + (1_i64 << 44);
    (rounded >> 45) as i32
}

/// Ограничение значения диапазоном `[low, high]` (libass: `FFMINMAX`).
fn clamp_between(value: i32, low: i32, high: i32) -> i32 {
    value.max(low).min(high)
}

/// Строка тайла `[origin + row * stride .. + TILE_SIZE]` внутри буфера.
fn tile_row(
    buf: &mut [u8],
    origin: usize,
    stride: usize,
    row: usize,
) -> Result<&mut [u8], RasterError> {
    let start = row
        .checked_mul(stride)
        .and_then(|offset| offset.checked_add(origin))
        .ok_or(RasterError::BufferTooSmall)?;
    let end = start
        .checked_add(TILE_SIZE)
        .ok_or(RasterError::BufferTooSmall)?;
    buf.get_mut(start..end).ok_or(RasterError::BufferTooSmall)
}

/// Заливает тайл сплошным значением: 255 при `covered`, иначе 0 (libass: `ass_fill_solid_tile`).
pub(super) fn fill_solid_tile(
    buf: &mut [u8],
    origin: usize,
    stride: usize,
    covered: bool,
) -> Result<(), RasterError> {
    let value = if covered { 255 } else { 0 };
    for row in 0..TILE_SIZE {
        tile_row(buf, origin, stride, row)?.fill(value);
    }
    Ok(())
}

/// Заливает тайл полуплоскостью с антиалиасингом (libass: `ass_fill_halfplane_tile`).
///
/// Заливаются пиксели, где `A*x + B*y < C` (A = a*scale, B = b*scale, C = c*scale/64).
/// Результат: `(clamp((CC - min_ab/4) / max_ab) + clamp((CC + min_ab/4) / max_ab) + 1) / 2`
/// с `clamp(Z) = max(-0.5, min(0.5, Z))`.
pub(super) fn fill_halfplane_tile(
    buf: &mut [u8],
    origin: usize,
    stride: usize,
    a: i32,
    b: i32,
    c: i64,
    scale: i32,
) -> Result<(), RasterError> {
    let aa = rescale_ab(a, scale);
    let bb = rescale_ab(b, scale);
    let mut cc = wrap16(rescale_c(c, scale) + FULL_VALUE / 2 - ((aa + bb) >> 1));

    let abs_a = wrap16(aa.abs());
    let abs_b = wrap16(bb.abs());
    let delta = wrap16((abs_a.min(abs_b) + 2) >> 2);

    let mut va1 = [0_i32; TILE_SIZE];
    let mut va2 = [0_i32; TILE_SIZE];
    for (x, (low, high)) in va1.iter_mut().zip(va2.iter_mut()).enumerate() {
        let shifted = wrap16(aa * x as i32);
        *low = wrap16(shifted - delta);
        *high = wrap16(shifted + delta);
    }

    for row in 0..TILE_SIZE {
        let line = tile_row(buf, origin, stride, row)?;
        for ((pixel, low), high) in line.iter_mut().zip(&va1).zip(&va2) {
            let c1 = clamp_between(wrap16(cc - low), 0, FULL_VALUE);
            let c2 = clamp_between(wrap16(cc - high), 0, FULL_VALUE);
            let result = wrap16(c1 + c2) >> (7 - TILE_ORDER);
            *pixel = result.min(255) as u8;
        }
        cc = wrap16(cc - bb);
    }
    Ok(())
}

/// Параметры линии трапеции для [`update_border_line`].
struct BorderLine {
    abs_a: i32,
    b: i32,
    abs_b: i32,
    c: i32,
}

/// Рисует верхнюю или нижнюю неполную строку трапеции с антиалиасингом
/// (libass: `update_border_line`).
fn update_border_line(
    result: &mut [i32; TILE_SIZE],
    columns: &[i32; TILE_SIZE],
    line: &BorderLine,
    up: i32,
    dn: i32,
) {
    let mut size = wrap16(dn - up);
    let mut w = wrap16(FULL_VALUE + (size << (8 - TILE_ORDER)) - line.abs_a);
    w = wrap16(w.min(FULL_VALUE) << (2 * TILE_ORDER - 5));

    let dc_b = wrap16(line.abs_b.wrapping_mul(size) >> 6);
    let dc = wrap16((line.abs_a.min(dc_b) + 2) >> 2);

    let base = wrap16(line.b.wrapping_mul(wrap16(up + dn)) >> 7);
    let offs1 = wrap16(size - (((base + dc).wrapping_mul(w)) >> 16));
    let offs2 = wrap16(size - (((base - dc).wrapping_mul(w)) >> 16));

    size = wrap16(size << 1);
    for (cell, column) in result.iter_mut().zip(columns) {
        let cw = wrap16(((line.c - column).wrapping_mul(w)) >> 16);
        let c1 = clamp_between(wrap16(cw + offs1), 0, size);
        let c2 = clamp_between(wrap16(cw + offs2), 0, size);
        *cell = wrap16(*cell + c1 + c2);
    }
}

/// Заливает тайл по общему списку сегментов (libass: `ass_fill_generic_tile`).
///
/// Каждый отрезок превращается в трапецию вместе со своей проекцией на левую сторону тайла;
/// трапеции складываются в промежуточный буфер с нужным знаком, итог — модуль суммы,
/// ограниченный 255. `winding` — намотка слева от тайла.
pub(super) fn fill_generic_tile(
    buf: &mut [u8],
    origin: usize,
    stride: usize,
    lines: &[Segment],
    winding: i32,
) -> Result<(), RasterError> {
    let mut result = [[0_i32; TILE_SIZE]; TILE_SIZE];
    let mut delta = [0_i32; TILE_SIZE + 2];

    for line in lines {
        // Инварианты из assert в C: отрезок лежит внутри тайла по Y.
        let tile_height = 64 << TILE_ORDER;
        if line.y_min < 0
            || line.y_min >= tile_height
            || line.y_max <= 0
            || line.y_max > tile_height
            || line.y_min > line.y_max
        {
            return Err(RasterError::SegmentOutsideTile);
        }

        let mut up_delta: i32 = if line.flags & SEGFLAG_DN != 0 { 4 } else { 0 };
        let mut dn_delta = up_delta;
        if line.x_min == 0 && line.flags & SEGFLAG_EXACT_LEFT != 0 {
            dn_delta ^= 4;
        }
        if line.flags & SEGFLAG_UL_DR != 0 {
            std::mem::swap(&mut up_delta, &mut dn_delta);
        }

        let mut up = (line.y_min >> 6) as usize;
        let dn = (line.y_max >> 6) as usize;
        let up_pos = line.y_min & 63;
        let up_delta1 = wrap16(up_delta * up_pos);
        let dn_pos = line.y_max & 63;
        let dn_delta1 = wrap16(dn_delta * dn_pos);
        adjust_delta(&mut delta, up + 1, -up_delta1)?;
        adjust_delta(&mut delta, up, -((up_delta << 6) - up_delta1))?;
        adjust_delta(&mut delta, dn + 1, dn_delta1)?;
        adjust_delta(&mut delta, dn, (dn_delta << 6) - dn_delta1)?;
        if line.y_min == line.y_max {
            continue;
        }

        let a = rescale_ab(line.a, line.scale);
        let b = rescale_ab(line.b, line.scale);
        let mut c = wrap16(rescale_c(line.c, line.scale) - (a >> 1) - b * up as i32);

        let mut columns = [0_i32; TILE_SIZE];
        for (x, column) in columns.iter_mut().enumerate() {
            *column = wrap16(a * x as i32);
        }
        let abs_a = wrap16(a.abs());
        let abs_b = wrap16(b.abs());
        let dc = wrap16((abs_a.min(abs_b) + 2) >> 2);
        let base = wrap16(FULL_VALUE / 2 - (b >> 1));
        let dc1 = wrap16(base + dc);
        let dc2 = wrap16(base - dc);
        let border = BorderLine { abs_a, b, abs_b, c };

        if up_pos != 0 {
            if dn == up {
                let row = result.get_mut(up).ok_or(RasterError::SegmentOutsideTile)?;
                update_border_line(row, &columns, &BorderLine { c, ..border }, up_pos, dn_pos);
                continue;
            }
            let row = result.get_mut(up).ok_or(RasterError::SegmentOutsideTile)?;
            update_border_line(row, &columns, &BorderLine { c, ..border }, up_pos, 64);
            up += 1;
            c = wrap16(c - b);
        }
        for row_index in up..dn {
            let row = result
                .get_mut(row_index)
                .ok_or(RasterError::SegmentOutsideTile)?;
            for (cell, column) in row.iter_mut().zip(&columns) {
                let c1 = clamp_between(wrap16(c - column + dc1), 0, FULL_VALUE);
                let c2 = clamp_between(wrap16(c - column + dc2), 0, FULL_VALUE);
                *cell = wrap16(*cell + (wrap16(c1 + c2) >> (7 - TILE_ORDER)));
            }
            c = wrap16(c - b);
        }
        if dn_pos != 0 {
            let row = result.get_mut(dn).ok_or(RasterError::SegmentOutsideTile)?;
            update_border_line(row, &columns, &BorderLine { c, ..border }, 0, dn_pos);
        }
    }

    // Итог: модуль накопленной суммы построчно, ограниченный 255.
    let mut running = wrap16(256 * i32::from(winding as i8));
    for (row_index, row) in result.iter().enumerate() {
        running = wrap16(running + delta.get(row_index).copied().unwrap_or(0));
        let line = tile_row(buf, origin, stride, row_index)?;
        for (pixel, cell) in line.iter_mut().zip(row) {
            let value = wrap16(cell + running);
            let negated = wrap16(-value);
            *pixel = value.max(negated).min(255) as u8;
        }
    }
    Ok(())
}

/// Прибавляет к `delta[index]` значение с усечением до int16.
fn adjust_delta(
    delta: &mut [i32; TILE_SIZE + 2],
    index: usize,
    amount: i32,
) -> Result<(), RasterError> {
    let slot = delta
        .get_mut(index)
        .ok_or(RasterError::SegmentOutsideTile)?;
    *slot = wrap16(*slot + amount);
    Ok(())
}

/// Объединяет тайл `tile` (stride = `TILE_SIZE`) с буфером поэлементным максимумом
/// (libass: `ass_merge_tile`).
pub(super) fn merge_tile(
    buf: &mut [u8],
    origin: usize,
    stride: usize,
    tile: &[u8; TILE_SIZE * TILE_SIZE],
) -> Result<(), RasterError> {
    let (source_rows, _) = tile.as_chunks::<TILE_SIZE>();
    for (row, source) in source_rows.iter().enumerate() {
        let line = tile_row(buf, origin, stride, row)?;
        for (pixel, other) in line.iter_mut().zip(source) {
            *pixel = (*pixel).max(*other);
        }
    }
    Ok(())
}
