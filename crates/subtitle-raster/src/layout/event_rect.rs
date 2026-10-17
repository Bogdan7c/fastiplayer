// Порт из libass 0.17.5: ass_render.c (заполнение `EventImages` в конце `ass_render_event`).
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

//! Прямоугольник реплики в пикселях холста по метрикам текста (не по битмапам).
//!
//! Им libass пользуется для подложки `BorderStyle = 4` (`add_background`) и для разрешения
//! столкновений (`fix_collisions`): рамка включает отступы обводки (VSFilter не сдвигает
//! строки с обводкой больше поля, поэтому слева и сверху возможны отрицательные значения).

use super::TextLayout;

/// Рамка реплики: левый верхний угол и размер (libass: `EventImages.left/top/width/height`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventImageRect {
    /// Левый край.
    pub(crate) left: i32,
    /// Верхний край.
    pub(crate) top: i32,
    /// Ширина.
    pub(crate) width: i32,
    /// Высота.
    pub(crate) height: i32,
}

/// Считает рамку реплики. Приведение `double -> int` отбрасывает дробную часть, как в libass.
///
/// `pixel_aspect_x` — `par_scale_x` libass (1.0 для SRT/WebVTT и ASS без отличия пропорций
/// хранения от кадра): горизонтальные размеры умножаются на него.
pub(crate) fn event_image_rect(layout: &TextLayout, pixel_aspect_x: f64) -> EventImageRect {
    let measure = &layout.measure;
    let first_ascent = measure.lines.first().map_or(0.0, |line| line.ascent);
    EventImageRect {
        top: (layout.device_y - first_ascent - f64::from(measure.border_top)) as i32,
        height: (measure.height + f64::from(measure.border_bottom) + f64::from(measure.border_top))
            as i32,
        left: ((layout.device_x + layout.bounds.x_min) * pixel_aspect_x
            - f64::from(measure.border_x)
            + 0.5) as i32,
        width: ((layout.bounds.x_max - layout.bounds.x_min) * pixel_aspect_x
            + 2.0 * f64::from(measure.border_x)
            + 0.5) as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{TextBounds, TextMeasure, measure::LineMetrics};

    fn layout() -> TextLayout {
        TextLayout {
            measure: TextMeasure {
                lines: vec![LineMetrics {
                    ascent: 30.0,
                    descent: 10.0,
                }],
                height: 40.0,
                border_top: 3,
                border_bottom: 2,
                border_x: 4,
            },
            bounds: TextBounds {
                x_min: 10.0,
                x_max: 110.0,
                y_min: -30.0,
                y_max: 10.0,
            },
            device_x: 50.0,
            device_y: 500.0,
        }
    }

    #[test]
    fn rect_includes_border_padding() {
        let rect = event_image_rect(&layout(), 1.0);
        // top = 500 - 30 - 3, height = 40 + 2 + 3, left = 60 - 4 + 0.5, width = 100 + 8 + 0.5.
        assert_eq!(
            rect,
            EventImageRect {
                left: 56,
                top: 467,
                width: 108,
                height: 45
            }
        );
    }

    #[test]
    fn pixel_aspect_scales_only_horizontal_size() {
        let rect = event_image_rect(&layout(), 2.0);
        assert_eq!(
            rect,
            EventImageRect {
                left: 116,
                top: 467,
                width: 208,
                height: 45
            }
        );
    }
}
