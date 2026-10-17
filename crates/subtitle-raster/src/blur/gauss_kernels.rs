// Порт из libass 0.17.5: c/blur_template.h, c/c_blur.c (ядра гауссова каскада).
//
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

//! Ядра гауссова каскада над плоскостями `i16` в диапазоне `[0, 0x4000]`.
//!
//! # Раскладка и почему результат побитово совпадает с libass
//!
//! libass хранит промежуточные картинки «полосами» шириной 8 отсчётов (`STRIPE_WIDTH`) —
//! это приём для SIMD, он не влияет на значения. Здесь плоскость — обычный построчный
//! массив [`Plane`] с шагом строки, кратным [`LANES`]. Всё, что C-код читает за краем
//! картинки, он получает из нулевой строки/полосы, значит, математически изображение
//! продолжено нулями во все стороны. Здесь то же самое достигается нулевым
//! дополнением строк в буфере.
//!
//! **Инвариант плоскости:** отсчёты правее `width` (до шага строки) равны нулю. Все
//! ядра сохраняют его, потому что каждая формула переводит нули в нули
//! (`shrink`: `(0+2)>>2 = 0`, `expand`: `(0+0+1)>>1 = 0`, `blur`: `0x8000>>16 = 0`),
//! а ядра, меняющие ширину, считают весь шаг новой строки из продолжения нулями.
//!
//! Арифметика (в том числе `uint16_t`/`int16_t`-приведения C) повторена операция в
//! операцию через `wrapping_*`: на допустимых входах переполнений нет, а на недопустимых
//! результат остаётся определённым и тем же, что у libass.
//!
//! # Скорость
//!
//! Горячие циклы работают по срезам одинаковой длины и по массивам `[T; LANES]`, без
//! условий внутри, чтобы LLVM автовекторизовал их. Строки-соседи для вертикальных ядер
//! выбираются один раз на выходную строку (нулевая строка вместо выходящих за край).

/// Ширина «линии» в отсчётах: шаг строки плоскости кратен ей. Совпадает с `1 << align_order`
/// C-движка, чем гарантируется то же число обрабатываемых за проход отсчётов.
pub(super) const LANES: usize = 16;

/// Округляет ширину вверх до кратного [`LANES`] — шаг строки плоскости.
pub(super) const fn plane_stride(width: usize) -> usize {
    width.div_ceil(LANES) * LANES
}

/// Размеры плоскости. Шаг строки всегда [`plane_stride`] от ширины.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Plane {
    /// Видимая ширина в отсчётах.
    pub(super) width: usize,
    /// Высота в строках.
    pub(super) height: usize,
}

impl Plane {
    /// Шаг строки в отсчётах.
    pub(super) const fn stride(self) -> usize {
        plane_stride(self.width)
    }

    /// Число отсчётов в буфере плоскости.
    pub(super) const fn len(self) -> usize {
        self.stride() * self.height
    }
}

/// Рабочие буферы строк для ядер, меняющих ширину. Принадлежат владельцу [`super::BlurScratch`].
#[derive(Debug, Default)]
pub(super) struct RowScratch {
    /// Строка с нулевыми полями слева/справа для горизонтальных фильтров.
    padded: Vec<i16>,
    /// Чётные отсчёты строки (для `shrink_horz`).
    even: Vec<i16>,
    /// Нечётные отсчёты строки (для `shrink_horz`).
    odd: Vec<i16>,
    /// Нулевая строка вместо строк, выходящих за край (никогда не пишется).
    zero_row: Vec<i16>,
}

impl RowScratch {
    /// Сколько байт держат буферы строк (ёмкость, а не заполненная длина).
    pub(super) fn retained_bytes(&self) -> usize {
        (self.padded.capacity()
            + self.even.capacity()
            + self.odd.capacity()
            + self.zero_row.capacity())
            * size_of::<i16>()
    }

    /// Гарантирует, что нулевая строка не короче `stride`.
    fn zero_row(&mut self, stride: usize) -> &[i16] {
        if self.zero_row.len() < stride {
            self.zero_row.resize(stride, 0);
        }
        &self.zero_row[..stride]
    }
}

/// Растёт до `len` нулями; существующее содержимое не трогает. Возвращает срез ровно `len`.
fn grown_zeroed<T: Copy + Default>(buffer: &mut Vec<T>, len: usize) -> &mut [T] {
    if buffer.len() < len {
        buffer.resize(len, T::default());
    }
    &mut buffer[..len]
}

/// Растягивает 8-бит покрытие до `[0, 0x4000]` (libass: `ass_stripe_unpack`).
fn unpack_pixel(value: u8) -> i16 {
    let value = u16::from(value);
    // ((v << 7) | (v >> 1)) + 1 не превышает 0x8000, переполнения u16 нет.
    ((((value << 7) | (value >> 1)) + 1) >> 1) as i16
}

/// Ступенчатое дрожание (dither) 2x2 при упаковке: чётная строка `8, 40`, нечётная `56, 24`.
const DITHER: [[i16; LANES]; 2] = [
    [8, 40, 8, 40, 8, 40, 8, 40, 8, 40, 8, 40, 8, 40, 8, 40],
    [
        56, 24, 56, 24, 56, 24, 56, 24, 56, 24, 56, 24, 56, 24, 56, 24,
    ],
];

/// Сжимает отсчёт обратно в 8 бит с дрожанием (libass: `ass_stripe_pack`).
fn pack_pixel(value: i16, dither: i16) -> u8 {
    // C: (uint16_t)(src - (src >> 8) + dither) >> 6, затем усечение до uint8_t.
    (value.wrapping_sub(value >> 8).wrapping_add(dither) as u16 >> 6) as u8
}

/// 8-бит битмап -> плоскость (libass: `ass_stripe_unpack`). Читает только `plane.width`
/// столбцов каждой строки, остальное в плоскости обнуляет.
pub(super) fn unpack(dst: &mut [i16], src: &[u8], src_stride: usize, plane: Plane) {
    let stride = plane.stride();
    for (dst_row, src_row) in dst
        .chunks_exact_mut(stride)
        .zip(src.chunks(src_stride))
        .take(plane.height)
    {
        let (visible, padding) = dst_row.split_at_mut(plane.width);
        for (out, &pixel) in visible.iter_mut().zip(src_row) {
            *out = unpack_pixel(pixel);
        }
        padding.fill(0);
    }
}

/// Плоскость -> 8-бит буфер с шагом строки `plane.stride()` (libass: `ass_stripe_pack`).
pub(super) fn pack(dst: &mut [u8], src: &[i16], plane: Plane) {
    let stride = plane.stride();
    for (y, (dst_row, src_row)) in dst
        .chunks_exact_mut(stride)
        .zip(src.chunks_exact(stride))
        .take(plane.height)
        .enumerate()
    {
        let dither = &DITHER[y & 1];
        let (dst_lanes, _) = dst_row.as_chunks_mut::<LANES>();
        let (src_lanes, _) = src_row.as_chunks::<LANES>();
        for (dst_lane, src_lane) in dst_lanes.iter_mut().zip(src_lanes) {
            for k in 0..LANES {
                dst_lane[k] = pack_pixel(src_lane[k], dither[k]);
            }
        }
    }
}

/// Один выходной отсчёт сжатия вдвое ядром `[1, 5, 10, 10, 5, 1] / 32` с точной
/// последовательностью округлений C (libass: `shrink_func`).
#[inline(always)]
pub(super) fn shrink_func(p1p: i16, p1n: i16, z0p: i16, z0n: i16, n1p: i16, n1n: i16) -> i16 {
    let (p1p, p1n, z0p, z0n, n1p, n1n) = (
        i32::from(p1p),
        i32::from(p1n),
        i32::from(z0p),
        i32::from(z0n),
        i32::from(n1p),
        i32::from(n1n),
    );
    let mut r = (p1p + p1n + n1p + n1n) >> 1;
    r = (r + z0p + z0n) >> 1;
    r = (r + p1n + n1p) >> 1;
    ((r + z0p + z0n + 2) >> 2) as i16
}

/// Пара выходных отсчётов увеличения вдвое ядрами `[5, 10, 1]` и `[1, 10, 5]` (`expand_func`).
#[inline(always)]
pub(super) fn expand_func(p1: i16, z0: i16, n1: i16) -> (i16, i16) {
    // C приводит промежуточные суммы к uint16_t: это арифметика по модулю 2^16 без знака.
    let (p1, z0, n1) = (p1 as u16, z0 as u16, n1 as u16);
    let r = (p1.wrapping_add(n1) >> 1).wrapping_add(z0) >> 1;
    let positive = (r.wrapping_add(p1) >> 1).wrapping_add(z0).wrapping_add(1) >> 1;
    let negative = (r.wrapping_add(n1) >> 1).wrapping_add(z0).wrapping_add(1) >> 1;
    (positive as i16, negative as i16)
}

/// Строка `index` плоскости либо нулевая строка, если индекс вне `0..height`.
fn row_or_zero<'a>(
    plane_data: &'a [i16],
    stride: usize,
    height: usize,
    index: isize,
    zero_row: &'a [i16],
) -> &'a [i16] {
    match usize::try_from(index) {
        Ok(row) if row < height => &plane_data[row * stride..][..stride],
        _ => zero_row,
    }
}

/// Сжатие вдвое по вертикали (libass: `ass_shrink_vert`). Выход: высота `(h + 5) >> 1`.
pub(super) fn shrink_vert(dst: &mut [i16], src: &[i16], src_plane: Plane, rows: &mut RowScratch) {
    let stride = src_plane.stride();
    let dst_height = (src_plane.height + 5) >> 1;
    let zero = rows.zero_row(stride);
    for (y, dst_row) in dst.chunks_exact_mut(stride).take(dst_height).enumerate() {
        // Шесть строк источника 2y-4 .. 2y+1 (за краем — нули).
        let first = 2 * y as isize - 4;
        let at = |k: isize| row_or_zero(src, stride, src_plane.height, first + k, zero);
        let (p1p, p1n, z0p, z0n, n1p, n1n) = (at(0), at(1), at(2), at(3), at(4), at(5));
        for x in 0..stride {
            dst_row[x] = shrink_func(p1p[x], p1n[x], z0p[x], z0n[x], n1p[x], n1n[x]);
        }
    }
}

/// Сжатие вдвое по горизонтали (libass: `ass_shrink_horz`). Выход: ширина `(w + 5) >> 1`.
pub(super) fn shrink_horz(dst: &mut [i16], src: &[i16], src_plane: Plane, rows: &mut RowScratch) {
    let src_stride = src_plane.stride();
    let dst_plane = Plane {
        width: (src_plane.width + 5) >> 1,
        height: src_plane.height,
    };
    let dst_stride = dst_plane.stride();
    // Чётные/нечётные отсчёты строки со сдвигом на два «двойных» отсчёта слева
    // (отсчёты -4..-1 в исходных столбцах — нули): E[j + 2] = src[2j], O[j + 2] = src[2j + 1].
    let lane_len = dst_stride + 2;
    let even = grown_zeroed(&mut rows.even, lane_len);
    let odd = grown_zeroed(&mut rows.odd, lane_len);
    for (dst_row, src_row) in dst
        .chunks_exact_mut(dst_stride)
        .zip(src.chunks_exact(src_stride))
        .take(dst_plane.height)
    {
        even.fill(0);
        odd.fill(0);
        for ((e, o), pair) in even[2..]
            .iter_mut()
            .zip(odd[2..].iter_mut())
            .zip(src_row.as_chunks::<2>().0)
        {
            *e = pair[0];
            *o = pair[1];
        }
        // Окно шести отсчётов для выхода j: E[j], O[j], E[j+1], O[j+1], E[j+2], O[j+2].
        let (e0, e1, e2) = (
            &even[..dst_stride],
            &even[1..][..dst_stride],
            &even[2..][..dst_stride],
        );
        let (o0, o1, o2) = (
            &odd[..dst_stride],
            &odd[1..][..dst_stride],
            &odd[2..][..dst_stride],
        );
        for j in 0..dst_stride {
            dst_row[j] = shrink_func(e0[j], o0[j], e1[j], o1[j], e2[j], o2[j]);
        }
    }
}

/// Увеличение вдвое по вертикали (libass: `ass_expand_vert`). Выход: высота `2h + 4`.
pub(super) fn expand_vert(dst: &mut [i16], src: &[i16], src_plane: Plane, rows: &mut RowScratch) {
    let stride = src_plane.stride();
    let zero = rows.zero_row(stride);
    // Пара выходных строк (2m, 2m + 1) строится из строк источника m-2, m-1, m.
    for (m, pair) in dst
        .chunks_exact_mut(2 * stride)
        .take(src_plane.height + 2)
        .enumerate()
    {
        let (positive_row, negative_row) = pair.split_at_mut(stride);
        let at = |k: isize| row_or_zero(src, stride, src_plane.height, m as isize + k, zero);
        let (p1, z0, n1) = (at(-2), at(-1), at(0));
        for x in 0..stride {
            let (positive, negative) = expand_func(p1[x], z0[x], n1[x]);
            positive_row[x] = positive;
            negative_row[x] = negative;
        }
    }
}

/// Увеличение вдвое по горизонтали (libass: `ass_expand_horz`). Выход: ширина `2w + 4`.
pub(super) fn expand_horz(dst: &mut [i16], src: &[i16], src_plane: Plane, rows: &mut RowScratch) {
    let src_stride = src_plane.stride();
    let dst_plane = Plane {
        width: 2 * src_plane.width + 4,
        height: src_plane.height,
    };
    let dst_stride = dst_plane.stride();
    // Пара выходных столбцов (2m, 2m + 1) строится из исходных m-2, m-1, m;
    // P[t] = src[t - 2]. Источник правее `width` — нули, поэтому копируем не больше половины
    // выходного шага (он не меньше width + 2).
    let pairs = dst_stride / 2;
    let padded = grown_zeroed(&mut rows.padded, pairs + 2);
    // Буфер переиспользуется между шагами: левое поле (два нуля) восстанавливаем явно.
    padded[..2].fill(0);
    let copy_len = src_stride.min(pairs);
    for (dst_row, src_row) in dst
        .chunks_exact_mut(dst_stride)
        .zip(src.chunks_exact(src_stride))
        .take(dst_plane.height)
    {
        padded[2..2 + copy_len].copy_from_slice(&src_row[..copy_len]);
        padded[2 + copy_len..].fill(0);
        let (p1, z0, n1) = (
            &padded[..pairs],
            &padded[1..][..pairs],
            &padded[2..][..pairs],
        );
        for (m, out) in dst_row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let (positive, negative) = expand_func(p1[m], z0[m], n1[m]);
            out[0] = positive;
            out[1] = negative;
        }
    }
}

/// Веса ядра, размноженные на ширину линии: умножение двух векторов `i16` LLVM превращает
/// в `pmullw/pmulhw` (на SSE2 нет 32-битного умножения, а множитель-скаляр он не узнаёт).
type SplatWeights = [[i16; LANES]; 8];

fn splat_weights(weights: &[i16; 8]) -> SplatWeights {
    std::array::from_fn(|tap| [weights[tap]; LANES])
}

/// Взвешенная сумма отсчётов вокруг центра: `center + ((0x8000 + sum) >> 16)`, где
/// `sum = Σ_i (left_i - center) * w_i + (right_i - center) * w_i` (ядро `blur_horz/vert`).
#[inline(always)]
fn blur_lane<const RADIUS: usize>(
    center: &[i16; LANES],
    left: [&[i16; LANES]; RADIUS],
    right: [&[i16; LANES]; RADIUS],
    weights: &SplatWeights,
) -> [i16; LANES] {
    let mut acc = [0x8000i32; LANES];
    // Левые и правые ветви ядра суммируются отдельными циклами: если сложить их в одном,
    // LLVM выносит общий вес за скобки `(l - c + r - c) * w` и вместо дешёвого
    // умножения 16x16 -> 32 (`pmullw/pmulhw`) получает 32-битное умножение, которого на
    // SSE2 нет. Результат тот же: сложение по модулю 2^32 ассоциативно.
    for tap in 0..RADIUS {
        for k in 0..LANES {
            // C: (int16_t)(a - c) * param — разность усекается до int16.
            let term = i32::from(left[tap][k].wrapping_sub(center[k])) * i32::from(weights[tap][k]);
            acc[k] = acc[k].wrapping_add(term);
        }
    }
    for tap in 0..RADIUS {
        for k in 0..LANES {
            let term =
                i32::from(right[tap][k].wrapping_sub(center[k])) * i32::from(weights[tap][k]);
            acc[k] = acc[k].wrapping_add(term);
        }
    }
    let mut out = [0i16; LANES];
    for k in 0..LANES {
        out[k] = center[k].wrapping_add((acc[k] >> 16) as i16);
    }
    out
}

/// Ссылка на `LANES` отсчётов среза с позиции `start`.
#[inline(always)]
fn lane_at(data: &[i16], start: usize) -> &[i16; LANES] {
    // Границы гарантирует вызывающий: ядра выделяют буфер с запасом (см. `blur_horz`).
    data[start..start + LANES]
        .first_chunk::<LANES>()
        .unwrap_or(&[0; LANES])
}

/// Размытие по горизонтали ядром радиуса `RADIUS` (libass: `ass_blur{4..8}_horz`).
/// Выход: ширина `w + 2 * RADIUS`.
pub(super) fn blur_horz<const RADIUS: usize>(
    dst: &mut [i16],
    src: &[i16],
    src_plane: Plane,
    weights: &[i16; 8],
    rows: &mut RowScratch,
) {
    let weights = &splat_weights(weights);
    let src_stride = src_plane.stride();
    let dst_plane = Plane {
        width: src_plane.width + 2 * RADIUS,
        height: src_plane.height,
    };
    let dst_stride = dst_plane.stride();
    // P[t] = src[t - 2R]: окно выхода j — P[j ..= j + 2R], центр P[j + R].
    let padded = grown_zeroed(&mut rows.padded, dst_stride + 2 * RADIUS);
    // Буфер переиспользуется между шагами: левое поле (2R нулей) восстанавливаем явно,
    // правое — на каждой строке ниже.
    padded[..2 * RADIUS].fill(0);
    for (dst_row, src_row) in dst
        .chunks_exact_mut(dst_stride)
        .zip(src.chunks_exact(src_stride))
        .take(dst_plane.height)
    {
        padded[2 * RADIUS..2 * RADIUS + src_stride].copy_from_slice(src_row);
        padded[2 * RADIUS + src_stride..].fill(0);
        for (chunk, dst_lane) in dst_row.as_chunks_mut::<LANES>().0.iter_mut().enumerate() {
            let base = chunk * LANES;
            let center = lane_at(padded, base + RADIUS);
            let left = std::array::from_fn(|tap| lane_at(padded, base + RADIUS - (tap + 1)));
            let right = std::array::from_fn(|tap| lane_at(padded, base + RADIUS + tap + 1));
            *dst_lane = blur_lane::<RADIUS>(center, left, right, weights);
        }
    }
}

/// Размытие по вертикали ядром радиуса `RADIUS` (libass: `ass_blur{4..8}_vert`).
/// Выход: высота `h + 2 * RADIUS`.
pub(super) fn blur_vert<const RADIUS: usize>(
    dst: &mut [i16],
    src: &[i16],
    src_plane: Plane,
    weights: &[i16; 8],
    rows: &mut RowScratch,
) {
    let weights = &splat_weights(weights);
    let stride = src_plane.stride();
    let zero = rows.zero_row(stride);
    for (y, dst_row) in dst
        .chunks_exact_mut(stride)
        .take(src_plane.height + 2 * RADIUS)
        .enumerate()
    {
        // Центр — строка y - R; ветви ядра — строки y - R ∓ tap.
        let at = |offset: isize| {
            row_or_zero(
                src,
                stride,
                src_plane.height,
                y as isize - RADIUS as isize + offset,
                zero,
            )
        };
        let center_row = at(0);
        let left_rows: [&[i16]; RADIUS] = std::array::from_fn(|tap| at(-(tap as isize + 1)));
        let right_rows: [&[i16]; RADIUS] = std::array::from_fn(|tap| at(tap as isize + 1));
        for (chunk, dst_lane) in dst_row.as_chunks_mut::<LANES>().0.iter_mut().enumerate() {
            let base = chunk * LANES;
            let center = lane_at(center_row, base);
            let left = std::array::from_fn(|tap| lane_at(left_rows[tap], base));
            let right = std::array::from_fn(|tap| lane_at(right_rows[tap], base));
            *dst_lane = blur_lane::<RADIUS>(center, left, right, weights);
        }
    }
}

/// Размытие по горизонтали с выбором ядра по радиусу `4..=8` (иное значение невозможно:
/// [`super::gauss_method::find_best_method`] зажимает радиус; сюда оно попадёт как 8).
pub(super) fn blur_horz_dispatch(
    radius: usize,
    dst: &mut [i16],
    src: &[i16],
    src_plane: Plane,
    weights: &[i16; 8],
    rows: &mut RowScratch,
) {
    match radius {
        4 => blur_horz::<4>(dst, src, src_plane, weights, rows),
        5 => blur_horz::<5>(dst, src, src_plane, weights, rows),
        6 => blur_horz::<6>(dst, src, src_plane, weights, rows),
        7 => blur_horz::<7>(dst, src, src_plane, weights, rows),
        _ => blur_horz::<8>(dst, src, src_plane, weights, rows),
    }
}

/// То же, что [`blur_horz_dispatch`], по вертикали.
pub(super) fn blur_vert_dispatch(
    radius: usize,
    dst: &mut [i16],
    src: &[i16],
    src_plane: Plane,
    weights: &[i16; 8],
    rows: &mut RowScratch,
) {
    match radius {
        4 => blur_vert::<4>(dst, src, src_plane, weights, rows),
        5 => blur_vert::<5>(dst, src, src_plane, weights, rows),
        6 => blur_vert::<6>(dst, src, src_plane, weights, rows),
        7 => blur_vert::<7>(dst, src, src_plane, weights, rows),
        _ => blur_vert::<8>(dst, src, src_plane, weights, rows),
    }
}
