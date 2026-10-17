// Порт из libass 0.17.5: ass_render.c (blend_vector_clip), c/c_blend_bitmaps.c
// (ass_mul_bitmaps_c, ass_imul_bitmaps_c).
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

//! Векторный клип `\clip([scale,] drawing)` / `\iclip(...)`: маска фигуры и её наложение на картинки.
//!
//! Маска растеризуется один раз на реплику (см. [`super::vector_build`]) и хранится в экранных
//! координатах. К каждой картинке она применяется как в `blend_vector_clip`:
//! - обычный клип: нет пересечения -> картинка пропадает (у libass она остаётся с нулевым
//!   размером, у нас пустых картинок нет по инварианту контракта); есть -> маски перемножаются
//!   `(a*b+255)>>8` и картинка обрезается до пересечения;
//! - инверсный клип: нет пересечения -> картинка как есть; есть -> целая картинка, а на
//!   пересечении `(dst*(255-src)+255)>>8`.

use std::sync::Arc;

use subtitle_core::AssClipMode;

use crate::overlay::{PixelPoint, SubtitleBitmap};

/// Растеризованная фигура векторного клипа.
///
/// Пустая фигура (нет контуров) допустима: `width == height == 0`, пересечения нет ни с чем,
/// поэтому обычный клип скрывает всё, а инверсный ничего (так же ведёт себя libass).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorClipMask {
    mode: AssClipMode,
    /// Левый верхний угол маски на холсте (libass: `pos + clip_bm->left/top`).
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    stride: usize,
    coverage: Arc<[u8]>,
}

/// Что стало с картинкой после векторного клипа.
#[derive(Debug)]
pub(crate) enum VectorClipEffect {
    /// Картинка не затронута (инверсный клип без пересечения).
    Unchanged,
    /// Картинка целиком скрыта (обычный клип без пересечения).
    Removed,
    /// Новая картинка с наложенной маской.
    Replaced(SubtitleBitmap),
}

/// Пересечение картинки с маской клипа в координатах обеих.
struct Overlap {
    image_left: usize,
    image_top: usize,
    mask_left: usize,
    mask_top: usize,
    width: usize,
    height: usize,
}

impl VectorClipMask {
    /// Маска фигуры; `coverage` — `height` строк с шагом `stride`.
    pub(crate) fn new(
        mode: AssClipMode,
        corner: (i32, i32),
        size: (i32, i32),
        stride: usize,
        coverage: Arc<[u8]>,
    ) -> Self {
        Self {
            mode,
            left: corner.0,
            top: corner.1,
            width: size.0,
            height: size.1,
            stride,
            coverage,
        }
    }

    /// Пустая фигура: ни одной закрашенной области.
    pub(crate) fn empty(mode: AssClipMode, corner: (i32, i32)) -> Self {
        Self::new(mode, corner, (0, 0), 0, Arc::from([]))
    }

    /// Размер маски в памяти (для учёта в кэше реплик).
    pub(crate) fn mask_bytes(&self) -> usize {
        self.coverage.len()
    }

    /// Накладывает клип на картинку (libass: тело цикла `blend_vector_clip`).
    pub(crate) fn apply(&self, image: &SubtitleBitmap) -> VectorClipEffect {
        let overlap = self.overlap_with(image);
        match (self.mode, overlap) {
            (AssClipMode::Inside, None) => VectorClipEffect::Removed,
            (AssClipMode::Outside, None) => VectorClipEffect::Unchanged,
            (AssClipMode::Inside, Some(overlap)) => self
                .multiplied(image, &overlap)
                .map_or(VectorClipEffect::Removed, VectorClipEffect::Replaced),
            (AssClipMode::Outside, Some(overlap)) => self
                .with_hole(image, &overlap)
                .map_or(VectorClipEffect::Unchanged, VectorClipEffect::Replaced),
        }
    }

    /// Пересечение картинки и маски; `None`, если оно пусто.
    fn overlap_with(&self, image: &SubtitleBitmap) -> Option<Overlap> {
        let origin = image.origin();
        let image_x = i64::from(origin.x);
        let image_y = i64::from(origin.y);
        let mask_x = i64::from(self.left);
        let mask_y = i64::from(self.top);
        let left = image_x.max(mask_x);
        let top = image_y.max(mask_y);
        let right = (image_x + i64::from(image.width())).min(mask_x + i64::from(self.width));
        let bottom = (image_y + i64::from(image.height())).min(mask_y + i64::from(self.height));
        if right <= left || bottom <= top {
            return None;
        }
        Some(Overlap {
            image_left: usize::try_from(left - image_x).ok()?,
            image_top: usize::try_from(top - image_y).ok()?,
            mask_left: usize::try_from(left - mask_x).ok()?,
            mask_top: usize::try_from(top - mask_y).ok()?,
            width: usize::try_from(right - left).ok()?,
            height: usize::try_from(bottom - top).ok()?,
        })
    }

    /// Обычный клип: пересечение, где маска картинки умножена на маску клипа (`mul_bitmaps`).
    fn multiplied(&self, image: &SubtitleBitmap, overlap: &Overlap) -> Option<SubtitleBitmap> {
        let mut result = vec![0_u8; overlap.width.checked_mul(overlap.height)?];
        for (row, out_row) in result.chunks_exact_mut(overlap.width).enumerate() {
            let image_row = self.rows_of_image(image, overlap, row)?;
            let mask_row = self.row_of_mask(overlap, row)?;
            for ((out, &image_value), &mask_value) in
                out_row.iter_mut().zip(image_row).zip(mask_row)
            {
                *out = ((u32::from(image_value) * u32::from(mask_value) + 255) >> 8) as u8;
            }
        }
        let origin = image.origin();
        image.with_mask(
            PixelPoint {
                x: origin.x.checked_add_unsigned(overlap.image_left as u32)?,
                y: origin.y.checked_add_unsigned(overlap.image_top as u32)?,
            },
            u32::try_from(overlap.width).ok()?,
            u32::try_from(overlap.height).ok()?,
            overlap.width,
            Arc::from(result),
        )
    }

    /// Инверсный клип: вся картинка, на пересечении маска картинки ослаблена маской клипа
    /// (`imul_bitmaps`).
    fn with_hole(&self, image: &SubtitleBitmap, overlap: &Overlap) -> Option<SubtitleBitmap> {
        let stride = image.stride();
        let mut result = image.coverage().to_vec();
        for row in 0..overlap.height {
            let mask_row = self.row_of_mask(overlap, row)?;
            let start = (overlap.image_top + row)
                .checked_mul(stride)?
                .checked_add(overlap.image_left)?;
            let image_row = result.get_mut(start..start.checked_add(overlap.width)?)?;
            for (image_value, &mask_value) in image_row.iter_mut().zip(mask_row) {
                *image_value =
                    ((u32::from(*image_value) * (255 - u32::from(mask_value)) + 255) >> 8) as u8;
            }
        }
        image.with_mask(
            image.origin(),
            image.width(),
            image.height(),
            stride,
            Arc::from(result),
        )
    }

    /// Строка `row` пересечения в маске картинки.
    fn rows_of_image<'a>(
        &self,
        image: &'a SubtitleBitmap,
        overlap: &Overlap,
        row: usize,
    ) -> Option<&'a [u8]> {
        let start = (overlap.image_top + row)
            .checked_mul(image.stride())?
            .checked_add(overlap.image_left)?;
        image
            .coverage()
            .get(start..start.checked_add(overlap.width)?)
    }

    /// Строка `row` пересечения в маске клипа.
    fn row_of_mask(&self, overlap: &Overlap, row: usize) -> Option<&[u8]> {
        let start = (overlap.mask_top + row)
            .checked_mul(self.stride)?
            .checked_add(overlap.mask_left)?;
        self.coverage.get(start..start.checked_add(overlap.width)?)
    }
}

#[cfg(test)]
mod tests;
