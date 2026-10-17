// Порт из libass 0.17.5: ass_render.c (render_glyph без обратного клипа, my_draw_bitmap,
// add_background).
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

//! Превращение битмапов в картинки выхода: обрезка по холсту и подложка события.

use std::sync::Arc;

use subtitle_core::SubtitleColor;

use super::ComposeError;
use crate::bitmap::CoverageBitmap;
use crate::event_style::EventStyle;
use crate::layout::{EventImageRect, TextLayout, event_image_rect};
use crate::overlay::{CanvasSize, PixelPoint, SubtitleBitmap, SubtitleBitmapKind};

/// Обрезает битмап по холсту и строит картинку (libass: `render_glyph`).
///
/// Инвариант выхода «картинка целиком внутри холста» выполняется обрезкой маски, а не
/// отбрасыванием картинки. `None` — после обрезки ничего не осталось или цвет прозрачен
/// (libass выбрасывает полностью прозрачные картинки в `ass_render_frame`).
pub(super) fn clipped_image(
    bitmap: &CoverageBitmap,
    color: SubtitleColor,
    kind: SubtitleBitmapKind,
    canvas: CanvasSize,
) -> Result<Option<SubtitleBitmap>, ComposeError> {
    if color.alpha == 0 {
        return Ok(None);
    }
    let canvas_width = i64::from(canvas.width());
    let canvas_height = i64::from(canvas.height());
    let (width, height) = (i64::from(bitmap.width), i64::from(bitmap.height));
    let (dst_x, dst_y) = (i64::from(bitmap.left), i64::from(bitmap.top));

    // Видимая часть битмапа в его собственных координатах.
    let visible_x0 = (-dst_x).max(0);
    let visible_y0 = (-dst_y).max(0);
    let visible_x1 = width.min(canvas_width - dst_x);
    let visible_y1 = height.min(canvas_height - dst_y);
    if visible_x0 >= visible_x1 || visible_y0 >= visible_y1 {
        return Ok(None);
    }

    let stride = bitmap.stride;
    let (Ok(x0), Ok(y0), Ok(x1), Ok(y1)) = (
        usize::try_from(visible_x0),
        usize::try_from(visible_y0),
        usize::try_from(visible_x1),
        usize::try_from(visible_y1),
    ) else {
        return Ok(None);
    };
    let visible_width = x1 - x0;
    let visible_height = y1 - y0;
    // Копируем ровно нужные строки с прежним шагом: последняя строка без хвоста шага.
    // Границы считаем с проверкой: несогласованная геометрия битмапа — ошибка, а не паника.
    let range = y0
        .checked_mul(stride)
        .and_then(|row_offset| row_offset.checked_add(x0))
        .and_then(|start| {
            let last_row = (visible_height - 1).checked_mul(stride)?;
            let end = start.checked_add(last_row)?.checked_add(visible_width)?;
            Some(start..end)
        });
    let Some(slice) = range.and_then(|range| bitmap.buffer.get(range)) else {
        return Err(ComposeError::Bitmap(
            crate::bitmap::BitmapError::InconsistentGeometry,
        ));
    };
    let origin = PixelPoint {
        x: i32::try_from(dst_x + visible_x0).unwrap_or(0),
        y: i32::try_from(dst_y + visible_y0).unwrap_or(0),
    };
    let image = SubtitleBitmap::new(
        origin,
        u32::try_from(visible_width).unwrap_or(0),
        u32::try_from(visible_height).unwrap_or(0),
        stride,
        Arc::from(slice),
        color,
        kind,
    )?;
    Ok(Some(image))
}

/// Подложка под всей репликой для `BorderStyle = 4` (libass: `add_background`).
///
/// Границы берутся из габаритов текста с отступами обводки (`EventImages`), расширяются на
/// смещение тени и обрезаются по холсту. Цвет — `BackColour` стиля.
pub(super) fn event_background(
    style: &EventStyle,
    layout: &TextLayout,
    canvas: CanvasSize,
    pixel_aspect_x: f64,
) -> Result<Option<SubtitleBitmap>, ComposeError> {
    // Границы события по метрикам текста (libass: `event_images`).
    let EventImageRect {
        left,
        top,
        width,
        height,
    } = event_image_rect(layout, pixel_aspect_x);

    // Тень расширяет подложку только в положительную сторону (lround = округление от нуля).
    let size_x = if style.shadow.horizontal > 0.0 {
        style.shadow.horizontal.round() as i32
    } else {
        0
    };
    let size_y = if style.shadow.vertical > 0.0 {
        style.shadow.vertical.round() as i32
    } else {
        0
    };
    // Все суммы насыщающие: габариты текста и тень приходят из раскладки и стиля, и при
    // экстремальных значениях лучше упереться в край холста, чем переполнить i32.
    let clamp = |value: i32, upper: u32| value.clamp(0, i32::try_from(upper).unwrap_or(i32::MAX));
    let box_left = clamp(left.saturating_sub(size_x), canvas.width());
    let box_top = clamp(top.saturating_sub(size_y), canvas.height());
    let box_right = clamp(
        left.saturating_add(width).saturating_add(size_x),
        canvas.width(),
    );
    let box_bottom = clamp(
        top.saturating_add(height).saturating_add(size_y),
        canvas.height(),
    );
    let box_width = box_right.saturating_sub(box_left);
    let box_height = box_bottom.saturating_sub(box_top);
    if box_width < 1 || box_height < 1 {
        return Ok(None);
    }
    let color = style.colors.back;
    if color.alpha == 0 {
        return Ok(None);
    }
    let (Ok(columns), Ok(rows)) = (usize::try_from(box_width), usize::try_from(box_height)) else {
        return Ok(None);
    };
    // Размер маски: ширина и высота ограничены холстом, но произведение считаем с проверкой.
    let Some(cells) = columns.checked_mul(rows) else {
        return Ok(None);
    };
    let image = SubtitleBitmap::new(
        PixelPoint {
            x: box_left,
            y: box_top,
        },
        u32::try_from(box_width).unwrap_or(0),
        u32::try_from(box_height).unwrap_or(0),
        columns,
        Arc::from(vec![0xFF_u8; cells]),
        color,
        SubtitleBitmapKind::Background,
    )?;
    Ok(Some(image))
}
