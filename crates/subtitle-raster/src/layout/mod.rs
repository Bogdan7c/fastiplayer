// Порт из libass 0.17.5: ass_render.c (часть ass_render_event: расчёт max_text_width,
// wrap_lines_smart, выбор device_x/device_y для событий без \pos).
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

//! Раскладка реплики на холсте: переносы строк, измерение, порядок BiDi, выравнивание и
//! положение блока текста.
//!
//! Шаги и их порядок те же, что в `ass_render_event` (libass 0.17.5):
//! 1. предварительные позиции по продвижениям ([`wrap::preliminary_layout`]);
//! 2. перенос строк `wrap_lines_smart` = простой проход + выравнивание длин + обрезка
//!    пробелов + измерение;
//! 3. порядок BiDi и позиции на строках ([`reorder::reorder_text`]);
//! 4. выравнивание строк внутри блока;
//! 5. положение блока: для нижнего якоря низ последней строки стоит на `высота − поле`.
//!
//! Все размеры — пиксели кадра. Реплики SRT/WebVTT стоят между целыми полями; реплики ASS
//! задают точную дробную геометрию и, с `\pos`, опорную точку блока (`EventStyle::placement`):
//! масштаб PlayRes -> кадр уже применён строителем стиля.

mod baseline_shear;
mod event_rect;
mod line_boxes;
mod measure;
mod reorder;
mod wrap;

use subtitle_core::ScreenAnchor;

pub(crate) use event_rect::{EventImageRect, event_image_rect};
pub(crate) use line_boxes::{LineBox, line_boxes};
pub(crate) use measure::{TextBounds, TextMeasure};
use reorder::{HorizontalAlignment, LineAlignment, align_lines, reorder_text};
pub(crate) use wrap::preliminary_layout;

use crate::event_style::{BaseDirection, BlockAnchor, EventStyle, RotationOrigin, ScrollPlacement};
use crate::glyph_info::TextCluster;
use crate::karaoke::KaraokePlan;
use crate::overlay::CanvasSize;

/// Итог раскладки реплики.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextLayout {
    /// Измерения строк.
    pub(crate) measure: TextMeasure,
    /// Габариты блока по позициям кластеров.
    pub(crate) bounds: TextBounds,
    /// Смещение блока по горизонтали (`device_x`), пиксели кадра.
    pub(crate) device_x: f64,
    /// Положение базовой линии первой строки (`device_y`), пиксели кадра.
    pub(crate) device_y: f64,
    /// Слова караоке: время и геометрия предварительной раскладки (пусто без караоке).
    pub(crate) karaoke: KaraokePlan,
}

/// Центр поворота реплики в пикселях холста (до растяжения на `par_scale_x`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RotationCenter {
    /// Координата по горизонтали.
    pub(crate) x: f64,
    /// Координата по вертикали.
    pub(crate) y: f64,
}

impl TextLayout {
    /// Центр поворота реплики (libass: `calculate_rotation_params`): точка `\org`, а без неё
    /// опорная точка блока по выравниванию — та же, за которую блок привязывается к `\pos`.
    pub(crate) fn rotation_center(&self, style: &EventStyle) -> RotationCenter {
        match style.rotation_origin() {
            RotationOrigin::AtPoint { x, y } => RotationCenter { x, y },
            RotationOrigin::ByAlignment => {
                let (base_x, base_y) = base_point(&self.bounds, style.alignment);
                RotationCenter {
                    x: self.device_x + base_x,
                    y: self.device_y + base_y,
                }
            }
        }
    }
}

/// Расставляет кластеры по строкам и вычисляет положение блока (все шаги, кроме растеризации).
///
/// Предполагает, что у кластеров уже есть продвижения и рамки (после шейпинга и загрузки
/// контуров). Позиции глифов после вызова — относительно блока текста; перевод в координаты
/// кадра делает композиция с учётом [`TextLayout::device_x`] и [`TextLayout::device_y`].
pub(crate) fn arrange_text(
    clusters: &mut [TextCluster],
    style: &EventStyle,
    canvas: CanvasSize,
) -> TextLayout {
    preliminary_layout(clusters);

    let max_text_width = style.text_width(f64::from(canvas.width()));
    // wrap_lines_smart: проход, выравнивание, обрезка пробелов, измерение.
    let line_count = wrap::wrap_lines_naive(clusters, max_text_width, style.wrap_style);
    wrap::wrap_lines_rebalance(clusters, style.wrap_style);
    measure::trim_whitespace(clusters);
    let measure = measure::measure_text(clusters, line_count, style);
    // libass: `ass_process_karaoke_effects` стоит после переноса строк и до порядка BiDi, потому
    // что читает предварительные позиции глифов (`pos.x` ещё без разрывов строк и выравнивания).
    let karaoke = KaraokePlan::build(clusters);

    let split_by_style = style.base_direction == BaseDirection::LeftToRight;
    let visual_order = reorder_text(clusters, &measure, style, split_by_style);
    let horizontal_alignment = HorizontalAlignment::of(style.alignment);
    align_lines(
        clusters,
        if style.scroll_placement().is_horizontal() {
            // `EVENT_HSCROLL`: justify = halign; halign = LEFT.
            LineAlignment::InWidestLine(horizontal_alignment)
        } else {
            LineAlignment::InTextArea(horizontal_alignment)
        },
        max_text_width,
    );
    let bounds = measure::string_bounds(clusters, &measure);
    // Рамка уже посчитана: сдвиг базовой линии `\fay` габариты блока не меняет (как в libass).
    baseline_shear::apply_baseline_shear(clusters, &visual_order);

    // Сначала позиция по `\pos`/выравниванию, затем прокрутка перекрывает одну ось
    // (libass: «an event can be both positioned and scrolling»).
    let (anchored_x, anchored_y) = match style.block_anchor() {
        BlockAnchor::ByAlignment => (
            style.text_left(),
            block_baseline(style, canvas, &measure, &bounds),
        ),
        BlockAnchor::AtPoint { x, y } => {
            // libass: `device = x2scr_pos(pos) - base_point(bbox, alignment)`.
            let (base_x, base_y) = base_point(&bounds, style.alignment);
            (x - base_x, y - base_y)
        }
    };
    let block_width = bounds.x_max - bounds.x_min;
    let (device_x, device_y) = match style.scroll_placement() {
        ScrollPlacement::None => (anchored_x, anchored_y),
        ScrollPlacement::RightToLeft { origin_x } => (origin_x, anchored_y),
        ScrollPlacement::LeftToRight { right_edge_x } => (right_edge_x - block_width, anchored_y),
        ScrollPlacement::TopToBottom { bottom_edge_y } => {
            (anchored_x, bottom_edge_y - bounds.y_max)
        }
        ScrollPlacement::BottomToTop { top_edge_y } => (anchored_x, top_edge_y - bounds.y_min),
    };
    TextLayout {
        measure,
        bounds,
        device_x,
        device_y,
        karaoke,
    }
}

/// Базовая линия первой строки для события без явной позиции (libass: ветки `valign`).
fn block_baseline(
    style: &EventStyle,
    canvas: CanvasSize,
    measure: &TextMeasure,
    bounds: &TextBounds,
) -> f64 {
    let first_ascent = measure.lines.first().map_or(0.0, |line| line.ascent);
    let margin = style.vertical_margin();
    match style.alignment {
        ScreenAnchor::TopLeft | ScreenAnchor::TopCenter | ScreenAnchor::TopRight => {
            margin + first_ascent
        }
        ScreenAnchor::MiddleLeft | ScreenAnchor::MiddleCenter | ScreenAnchor::MiddleRight => {
            let screen_middle = f64::from(canvas.height()) / 2.0;
            screen_middle - (bounds.y_max + bounds.y_min) / 2.0
        }
        ScreenAnchor::BottomLeft | ScreenAnchor::BottomCenter | ScreenAnchor::BottomRight => {
            // Нижний край блока — на `высота − поле`; первая строка выше на высоту блока.
            let bottom = f64::from(canvas.height()) - margin;
            bottom - measure.height + first_ascent
        }
    }
}

/// Опорная точка блока для `\pos` (libass: `get_base_point`): угол или середина габаритов
/// текста по выравниванию реплики.
fn base_point(bounds: &TextBounds, alignment: ScreenAnchor) -> (f64, f64) {
    let x = match alignment {
        ScreenAnchor::TopLeft | ScreenAnchor::MiddleLeft | ScreenAnchor::BottomLeft => bounds.x_min,
        ScreenAnchor::TopCenter | ScreenAnchor::MiddleCenter | ScreenAnchor::BottomCenter => {
            (bounds.x_max + bounds.x_min) / 2.0
        }
        ScreenAnchor::TopRight | ScreenAnchor::MiddleRight | ScreenAnchor::BottomRight => {
            bounds.x_max
        }
    };
    let y = match alignment {
        ScreenAnchor::TopLeft | ScreenAnchor::TopCenter | ScreenAnchor::TopRight => bounds.y_min,
        ScreenAnchor::MiddleLeft | ScreenAnchor::MiddleCenter | ScreenAnchor::MiddleRight => {
            (bounds.y_max + bounds.y_min) / 2.0
        }
        ScreenAnchor::BottomLeft | ScreenAnchor::BottomCenter | ScreenAnchor::BottomRight => {
            bounds.y_max
        }
    };
    (x, y)
}

#[cfg(test)]
mod tests;
