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

/// То же усечение, но результат сразу в `i16` (для массивов, которые векторизует компилятор).
///
/// Сложение, вычитание и умножение по модулю 2^16 дают те же 16 бит, что и `int`-вычисление с
/// усечением в конце, поэтому цепочки таких операций можно считать прямо в `i16` с `wrapping_*`.
fn to_i16(value: i32) -> i16 {
    value as i16
}

/// `RESCALE_AB` из libass для коэффициента полуплоскости.
///
/// В C результат присваивается `int16_t`, то есть усекается. Для любых `i32`-входов модуль
/// значения не больше 2^31 * 2^31 / 2^50 = 2^12, а для сегментов, нормализованных
/// `Segment::from_line` (`max(|a|, |b|) * scale ~ 2^60`), - около 2^11, так что усечение здесь
/// формальность и `wrap16` ничего не меняет. `debug_assert!` закрепляет это допущение на случай
/// смены `TILE_ORDER` или типов: тогда усечение начало бы влиять на результат, и его нужно
/// заметить сразу. Усечения, которые на самом деле срабатывают (`a * x`, `c - b * y`), делают
/// `to_i16` и `wrapping_*` ниже; их сверяет с дословной транскрипцией C `tile_reference_tests`.
fn rescale_ab(ab: i32, scale: i32) -> i32 {
    let rounded = i64::from(ab) * i64::from(scale) + (1_i64 << (45 + TILE_ORDER));
    let shifted = rounded >> (46 + TILE_ORDER);
    debug_assert!(
        i16::try_from(shifted).is_ok(),
        "rescaled coefficient {shifted} does not fit int16 (a/b = {ab}, scale = {scale})"
    );
    wrap16(shifted as i32)
}

/// `RESCALE_C` из libass для свободного члена полуплоскости.
fn rescale_c(c: i64, scale: i32) -> i32 {
    let high = (c >> (7 + TILE_ORDER)) as i32;
    let rounded = i64::from(high) * i64::from(scale) + (1_i64 << 44);
    (rounded >> 45) as i32
}

/// Ограничение значения диапазоном `[low, high]` (libass: `FFMINMAX`).
fn clamp_between(value: i16, low: i16, high: i16) -> i16 {
    value.max(low).min(high)
}

/// Проверяет, что тайл целиком лежит в буфере, и отдаёт буфер, начиная с первой строки тайла.
///
/// Проверка делается один раз на тайл, чтобы построчные циклы работали с массивами фиксированной
/// длины без проверок границ на каждый пиксель.
fn tile_region(buf: &mut [u8], origin: usize, stride: usize) -> Result<&mut [u8], RasterError> {
    let last_row_end = (TILE_SIZE - 1)
        .checked_mul(stride)
        .and_then(|offset| offset.checked_add(origin))
        .and_then(|offset| offset.checked_add(TILE_SIZE))
        .ok_or(RasterError::BufferTooSmall)?;
    if stride < TILE_SIZE || last_row_end > buf.len() {
        return Err(RasterError::BufferTooSmall);
    }
    buf.get_mut(origin..).ok_or(RasterError::BufferTooSmall)
}

/// Строка тайла как массив фиксированной длины: `row` - начало строки в буфере.
fn row_array(row: &mut [u8]) -> Result<&mut [u8; TILE_SIZE], RasterError> {
    row.first_chunk_mut::<TILE_SIZE>()
        .ok_or(RasterError::BufferTooSmall)
}

/// Заливает тайл сплошным значением: 255 при `covered`, иначе 0 (libass: `ass_fill_solid_tile`).
pub(super) fn fill_solid_tile(
    buf: &mut [u8],
    origin: usize,
    stride: usize,
    covered: bool,
) -> Result<(), RasterError> {
    let value = if covered { 255 } else { 0 };
    let region = tile_region(buf, origin, stride)?;
    for row in region.chunks_mut(stride).take(TILE_SIZE) {
        row_array(row)?.fill(value);
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
    let mut cc = to_i16(rescale_c(c, scale) + FULL_VALUE / 2 - ((aa + bb) >> 1));

    let abs_a = wrap16(aa.abs());
    let abs_b = wrap16(bb.abs());
    let delta = to_i16((abs_a.min(abs_b) + 2) >> 2);
    let bb = to_i16(bb);

    let mut va1 = [0_i16; TILE_SIZE];
    let mut va2 = [0_i16; TILE_SIZE];
    for (x, (low, high)) in va1.iter_mut().zip(va2.iter_mut()).enumerate() {
        let shifted = to_i16(aa * x as i32);
        *low = shifted.wrapping_sub(delta);
        *high = shifted.wrapping_add(delta);
    }

    let region = tile_region(buf, origin, stride)?;
    for row in region.chunks_mut(stride).take(TILE_SIZE) {
        let line = row_array(row)?;
        for (index, pixel) in line.iter_mut().enumerate() {
            let c1 = clamp_between(cc.wrapping_sub(va1[index]), 0, FULL_VALUE as i16);
            let c2 = clamp_between(cc.wrapping_sub(va2[index]), 0, FULL_VALUE as i16);
            let result = c1.wrapping_add(c2) >> (7 - TILE_ORDER);
            *pixel = result.min(255) as u8;
        }
        cc = cc.wrapping_sub(bb);
    }
    Ok(())
}

/// Параметры линии трапеции для [`update_border_line`].
struct BorderLine {
    abs_a: i16,
    b: i16,
    abs_b: i16,
    c: i16,
}

/// Рисует верхнюю или нижнюю неполную строку трапеции с антиалиасингом
/// (libass: `update_border_line`).
fn update_border_line(
    result: &mut [i16; TILE_SIZE],
    columns: &[i16; TILE_SIZE],
    line: &BorderLine,
    up: i32,
    dn: i32,
) {
    // Скаляры считаются в `i32`, как `int` в C, и усекаются там же, где C присваивает `int16_t`.
    let mut size = wrap16(dn - up);
    let mut w = wrap16(FULL_VALUE + (size << (8 - TILE_ORDER)) - i32::from(line.abs_a));
    w = wrap16(w.min(FULL_VALUE) << (2 * TILE_ORDER - 5));

    let dc_b = wrap16(i32::from(line.abs_b).wrapping_mul(size) >> 6);
    let dc = wrap16((i32::from(line.abs_a).min(dc_b) + 2) >> 2);

    let base = wrap16(i32::from(line.b).wrapping_mul(wrap16(up + dn)) >> 7);
    let offs1 = to_i16(size - (((base + dc).wrapping_mul(w)) >> 16));
    let offs2 = to_i16(size - (((base - dc).wrapping_mul(w)) >> 16));

    size = wrap16(size << 1);
    let size = to_i16(size);
    for (cell, column) in result.iter_mut().zip(columns) {
        let cw = to_i16((i32::from(line.c) - i32::from(*column)).wrapping_mul(w) >> 16);
        let c1 = clamp_between(cw.wrapping_add(offs1), 0, size);
        let c2 = clamp_between(cw.wrapping_add(offs2), 0, size);
        *cell = cell.wrapping_add(c1.wrapping_add(c2));
    }
}

/// Заливает тайл по общему списку сегментов (libass: `ass_fill_generic_tile`).
///
/// Каждый отрезок превращается в трапецию вместе со своей проекцией на левую сторону тайла;
/// трапеции складываются в промежуточный буфер с нужным знаком, итог — модуль суммы,
/// ограниченный 255. `winding` — намотка слева от тайла.
///
/// Буферы - `i16`, как в C (`int16_t res[16][16]`): строки тайла векторизуются по 8 значений.
pub(super) fn fill_generic_tile(
    buf: &mut [u8],
    origin: usize,
    stride: usize,
    lines: &[Segment],
    winding: i32,
) -> Result<(), RasterError> {
    let mut result = [[0_i16; TILE_SIZE]; TILE_SIZE];
    let mut delta = [0_i16; TILE_SIZE + 2];

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
        let mut c = to_i16(rescale_c(line.c, line.scale) - (a >> 1) - b * up as i32);

        let mut columns = [0_i16; TILE_SIZE];
        for (x, column) in columns.iter_mut().enumerate() {
            *column = to_i16(a * x as i32);
        }
        let abs_a = wrap16(a.abs());
        let abs_b = wrap16(b.abs());
        let dc = to_i16((abs_a.min(abs_b) + 2) >> 2);
        let b = to_i16(b);
        let base = to_i16(FULL_VALUE / 2 - (i32::from(b) >> 1));
        let dc1 = base.wrapping_add(dc);
        let dc2 = base.wrapping_sub(dc);
        let border = BorderLine {
            abs_a: to_i16(abs_a),
            b,
            abs_b: to_i16(abs_b),
            c,
        };

        if up_pos != 0 {
            if dn == up {
                let row = result.get_mut(up).ok_or(RasterError::SegmentOutsideTile)?;
                update_border_line(row, &columns, &BorderLine { c, ..border }, up_pos, dn_pos);
                continue;
            }
            let row = result.get_mut(up).ok_or(RasterError::SegmentOutsideTile)?;
            update_border_line(row, &columns, &BorderLine { c, ..border }, up_pos, 64);
            up += 1;
            c = c.wrapping_sub(b);
        }
        let full = FULL_VALUE as i16;
        for row_index in up..dn {
            let row = result
                .get_mut(row_index)
                .ok_or(RasterError::SegmentOutsideTile)?;
            for (cell, column) in row.iter_mut().zip(&columns) {
                let shifted = c.wrapping_sub(*column);
                let c1 = clamp_between(shifted.wrapping_add(dc1), 0, full);
                let c2 = clamp_between(shifted.wrapping_add(dc2), 0, full);
                *cell = cell.wrapping_add(c1.wrapping_add(c2) >> (7 - TILE_ORDER));
            }
            c = c.wrapping_sub(b);
        }
        if dn_pos != 0 {
            let row = result.get_mut(dn).ok_or(RasterError::SegmentOutsideTile)?;
            update_border_line(row, &columns, &BorderLine { c, ..border }, 0, dn_pos);
        }
    }

    // Итог: модуль накопленной суммы построчно, ограниченный 255.
    let mut running = to_i16(256 * i32::from(winding as i8));
    let region = tile_region(buf, origin, stride)?;
    for ((row_buf, row), row_delta) in region
        .chunks_mut(stride)
        .take(TILE_SIZE)
        .zip(&result)
        .zip(&delta)
    {
        running = running.wrapping_add(*row_delta);
        let line = row_array(row_buf)?;
        for (pixel, cell) in line.iter_mut().zip(row) {
            let value = cell.wrapping_add(running);
            let negated = value.wrapping_neg();
            *pixel = value.max(negated).min(255) as u8;
        }
    }
    Ok(())
}

/// Прибавляет к `delta[index]` значение с усечением до int16.
fn adjust_delta(
    delta: &mut [i16; TILE_SIZE + 2],
    index: usize,
    amount: i32,
) -> Result<(), RasterError> {
    let slot = delta
        .get_mut(index)
        .ok_or(RasterError::SegmentOutsideTile)?;
    *slot = slot.wrapping_add(to_i16(amount));
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
    let region = tile_region(buf, origin, stride)?;
    for (row_buf, source) in region.chunks_mut(stride).take(TILE_SIZE).zip(source_rows) {
        let line = row_array(row_buf)?;
        for (pixel, other) in line.iter_mut().zip(source) {
            *pixel = (*pixel).max(*other);
        }
    }
    Ok(())
}
