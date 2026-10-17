// Порт из libass 0.17.5: c/c_be_blur.c, ass_bitmap.c (be_blur_pre, be_blur_post).
//
// Copyright (C) 2009-2022 libass contributors
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

//! Box-размытие `\be`: ядро `[1 2 1; 2 4 2; 1 2 1] / 16` над 8-бит буфером на месте
//! (как в VSFilter; libass: `ass_be_blur_c`).
//!
//! За краем картинки считаются нули. Тройка проходов `\be >= 2` работает в 6-битном
//! диапазоне (`pre`/`post`), чтобы суммы 16 слагаемых не переполняли 8 бит промежуточно
//! в SIMD-версиях libass; Rust-версия сохраняет те же округления.
//!
//! Работает только по видимым `width` столбцам; байты шага строки правее не читаются и
//! не меняются.

/// Рабочие строки горизонтальных сумм. Принадлежат владельцу [`super::BlurScratch`].
#[derive(Debug, Default)]
pub(super) struct BoxScratch {
    /// Исходная строка с нулевыми полями: `[0, b0, .., b(w-1), 0]`.
    padded: Vec<u16>,
    /// Горизонтальные суммы `H` предыдущей, текущей и следующей строк.
    previous: Vec<u16>,
    current: Vec<u16>,
    next: Vec<u16>,
}

impl BoxScratch {
    /// Сколько байт держат буферы строк (ёмкость, а не заполненная длина).
    pub(super) fn retained_bytes(&self) -> usize {
        (self.padded.capacity()
            + self.previous.capacity()
            + self.current.capacity()
            + self.next.capacity())
            * size_of::<u16>()
    }
}

/// Подготавливает `buffer` к `len` элементам, не сохраняя старое содержимое.
fn prepared(buffer: &mut Vec<u16>, len: usize) -> &mut [u16] {
    if buffer.len() < len {
        buffer.resize(len, 0);
    }
    &mut buffer[..len]
}

/// `H[x] = b[x-1] + 2 b[x] + b[x+1]` для строки (за краем нули).
fn horizontal_sums(row: &[u8], padded: &mut [u16], sums: &mut [u16]) {
    let width = row.len();
    padded[0] = 0;
    for (slot, &pixel) in padded[1..=width].iter_mut().zip(row) {
        *slot = u16::from(pixel);
    }
    padded[width + 1] = 0;
    let (left, middle, right) = (
        &padded[..width],
        &padded[1..][..width],
        &padded[2..][..width],
    );
    for x in 0..width {
        // C: цепочка `sliding_sum` в uint16_t, то есть арифметика по модулю 2^16.
        sums[x] = left[x]
            .wrapping_add(middle[x])
            .wrapping_add(middle[x])
            .wrapping_add(right[x]);
    }
}

/// Один проход box-размытия на месте по первым `width` столбцам и `height` строкам
/// (libass: `ass_be_blur_c`).
pub(super) fn box_blur_pass(
    buf: &mut [u8],
    stride: usize,
    width: usize,
    height: usize,
    scratch: &mut BoxScratch,
) {
    if width == 0 || height == 0 {
        return;
    }
    let BoxScratch {
        padded,
        previous,
        current,
        next,
    } = scratch;
    let padded = prepared(padded, width + 2);
    // Строка выше первой — нули (`H[-1] = 0`).
    prepared(previous, width).fill(0);
    horizontal_sums(&buf[..width], padded, prepared(current, width));
    for y in 0..height {
        if y + 1 < height {
            let row = &buf[(y + 1) * stride..][..width];
            horizontal_sums(row, padded, prepared(next, width));
        } else {
            // Строка ниже последней — нули (`H[h] = 0`).
            prepared(next, width).fill(0);
        }
        let dst = &mut buf[y * stride..][..width];
        let (above, here, below) = (&previous[..width], &current[..width], &next[..width]);
        for x in 0..width {
            let total = above[x]
                .wrapping_add(here[x])
                .wrapping_add(here[x])
                .wrapping_add(below[x]);
            dst[x] = (total >> 4) as u8;
        }
        // previous <- current <- next; next получит новое значение на следующем шаге.
        std::mem::swap(previous, current);
        std::mem::swap(current, next);
    }
}

/// Перевод 8-бит значения в 6-бит перед серией проходов (libass: `be_blur_pre`).
///
/// Эквивалентно `(value * 64 + 127) / 255` для 0..=256.
pub(super) fn scale_down_for_passes(buf: &mut [u8], stride: usize, width: usize, height: usize) {
    for row in buf.chunks_mut(stride).take(height) {
        for value in &mut row[..width] {
            *value = ((*value >> 1) + 1) >> 1;
        }
    }
}

/// Обратный перевод 6-бит -> 8-бит после серии проходов (libass: `be_blur_post`).
///
/// Эквивалентно `(value * 255 + 32) / 64` для 0..=96; нужны лишь значения до 64.
pub(super) fn scale_up_after_passes(buf: &mut [u8], stride: usize, width: usize, height: usize) {
    for row in buf.chunks_mut(stride).take(height) {
        for value in &mut row[..width] {
            // C: `(value << 2) - (value > 32)` с усечением до uint8_t.
            *value = (u16::from(*value) << 2).wrapping_sub(u16::from(*value > 32)) as u8;
        }
    }
}
