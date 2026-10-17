// Порт из libass 0.17.5: ass_render.c (разрез по `brk` в `render_glyph` и `render_glyph_i`).
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

//! Раскраска картинки при сборке кадра: цвет, разрез караоке по вертикали, скрытие.
//!
//! Караоке решает, КАК красить картинку (стадия [`crate::karaoke`]), а клип решает, КАКАЯ
//! часть картинки видна. `render_glyph` libass делает оба шага разом, и от порядка зависит
//! порядок картинок в списке, поэтому разрез живёт здесь, рядом с клипом: видимый кусок
//! картинки режется по `brk` на левую и правую части (в этом порядке).

use subtitle_core::SubtitleColor;

use crate::overlay::{LocalRect, SubtitleBitmap};

/// Как красить картинку реплики на этом кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImagePaint {
    /// Как в кэше реплики (цвет и маска без изменений).
    AsStored,
    /// Картинка не рисуется (обводка `\ko` до начала слога).
    Hidden,
    /// Одним цветом (`\k`, `\ko`: до или после начала слога).
    Solid(SubtitleColor),
    /// Левее вертикали `x` (абсолютный пиксель холста) цвет `left`, правее — `right`
    /// (libass: `render_glyph(..., color, color2, brk)`).
    SplitAt {
        /// Абсолютная координата границы, пиксели холста.
        x: i32,
        /// Цвет левее границы.
        left: SubtitleColor,
        /// Цвет правее границы.
        right: SubtitleColor,
    },
}

impl ImagePaint {
    /// Кладёт в `out` видимые части `region` картинки `image` с этой раскраской.
    ///
    /// Полностью прозрачные части отбрасываются: libass убирает такие картинки из итогового
    /// списка (`_a(color) == 0xFF`), а караоке может сделать прозрачным любой из двух цветов.
    pub(crate) fn push_region(
        self,
        image: &SubtitleBitmap,
        region: LocalRect,
        out: &mut Vec<SubtitleBitmap>,
    ) {
        match self {
            Self::AsStored => out.extend(image.cropped(region)),
            Self::Hidden => {}
            Self::Solid(color) => push_colored(image, region, color, out),
            Self::SplitAt { x, left, right } => {
                // Граница в координатах картинки; могут быть и вне картинки, и огромными.
                let brk = i64::from(x) - i64::from(image.origin().x);
                let (x0, x1) = (i64::from(region.x0), i64::from(region.x1));
                let mut left_end = brk;
                if left_end > x0 {
                    left_end = left_end.min(x1);
                    push_piece(image, region, (x0, left_end), left, out);
                }
                if left_end < x1 {
                    left_end = left_end.max(x0);
                    push_piece(image, region, (left_end, x1), right, out);
                }
            }
        }
    }

    /// Раскраска, при которой картинка целиком остаётся как есть.
    pub(crate) fn is_as_stored(self) -> bool {
        self == Self::AsStored
    }
}

/// Часть `columns` (по горизонтали, координаты картинки) области `region` цветом `color`.
fn push_piece(
    image: &SubtitleBitmap,
    region: LocalRect,
    columns: (i64, i64),
    color: SubtitleColor,
    out: &mut Vec<SubtitleBitmap>,
) {
    let (Ok(x0), Ok(x1)) = (u32::try_from(columns.0), u32::try_from(columns.1)) else {
        return;
    };
    let piece = LocalRect { x0, x1, ..region };
    push_colored(image, piece, color, out);
}

/// Часть картинки `region` цветом `color`; прозрачный цвет часть убирает.
fn push_colored(
    image: &SubtitleBitmap,
    region: LocalRect,
    color: SubtitleColor,
    out: &mut Vec<SubtitleBitmap>,
) {
    if color.alpha == 0 {
        return;
    }
    out.extend(image.cropped(region).map(|part| part.with_color(color)));
}
