// Порт из libass 0.17.5: ass_shaper.c (ass_shaper_reorder) и ass_render.c (reorder_text,
// align_lines).
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

//! Порядок символов на строках (BiDi) и выравнивание строк по горизонтали.

use subtitle_core::ScreenAnchor;

use super::measure::TextMeasure;
use crate::event_style::EventStyle;
use crate::fixed_point::{Vector26, d6_to_double, double_to_d6};
use crate::glyph_info::TextCluster;
use crate::shaping::bidi::{is_paragraph_separator, visual_order};

/// Карта «визуальная позиция -> логический индекс» для всей реплики
/// (libass: `ass_shaper_reorder`). Строка рвётся на переносах, разделителях абзацев и на
/// границах участков стиля.
fn reorder_map(clusters: &[TextCluster], split_by_style: bool) -> Vec<usize> {
    let mut map: Vec<usize> = (0..clusters.len()).collect();
    let mut segment_start = 0;
    for index in 0..clusters.len() {
        let ends_segment = index + 1 == clusters.len()
            || clusters
                .get(index + 1)
                .is_some_and(|next| next.line_start.begins_line())
            || clusters
                .get(index)
                .is_some_and(|current| is_paragraph_separator(current.symbol))
            || (split_by_style
                && (clusters
                    .get(index + 1)
                    .is_some_and(|next| next.starts_new_run)
                    // libass: `glyphs[i].hspacing` — символ с `\fsp` замыкает отрезок.
                    || clusters
                        .get(index)
                        .is_some_and(|current| current.has_letter_spacing)));
        if !ends_segment {
            continue;
        }
        let segment = clusters.get(segment_start..=index).unwrap_or(&[]);
        let symbols: Vec<char> = segment.iter().map(|cluster| cluster.symbol).collect();
        let levels: Vec<u8> = segment.iter().map(|cluster| cluster.bidi_level).collect();
        let base_level = segment.first().map_or(0, |cluster| cluster.paragraph_level);
        for (visual, logical) in visual_order(&symbols, &levels, base_level)
            .into_iter()
            .enumerate()
        {
            if let Some(slot) = map.get_mut(segment_start + visual) {
                *slot = segment_start + logical;
            }
        }
        segment_start = index + 1;
    }
    map
}

/// Расставляет кластеры по строкам в порядке отображения (libass: `reorder_text`).
///
/// После вызова позиции глифов отсчитываются от левого верхнего угла блока текста: `x` — от
/// начала строки (до выравнивания), `y` — вниз от базовой линии первой строки.
pub(super) fn reorder_text(
    clusters: &mut [TextCluster],
    measure: &TextMeasure,
    style: &EventStyle,
    split_by_style: bool,
) {
    let map = reorder_map(clusters, split_by_style);
    let mut pen = Vector26::default();
    let mut line_number = 1_usize;
    for (visual, logical) in map.iter().copied().enumerate() {
        let starts_line = clusters
            .get(visual)
            .is_some_and(|cluster| cluster.line_start.begins_line());
        if starts_line {
            pen.x = 0;
            let descent = measure
                .lines
                .get(line_number - 1)
                .map_or(0.0, |line| line.descent);
            let ascent = measure
                .lines
                .get(line_number)
                .map_or(0.0, |line| line.ascent);
            for step in [descent, ascent, style.line_spacing] {
                pen.y = pen.y.saturating_add(double_to_d6(step));
            }
            line_number += 1;
        }
        let Some(cluster) = clusters.get_mut(logical) else {
            continue;
        };
        if cluster.skip {
            continue;
        }
        let mut cluster_pen = pen;
        pen.add_saturating(cluster.cluster_advance);
        for glyph in cluster.glyphs_mut() {
            glyph.pos = glyph.offset.plus_saturating(cluster_pen);
            cluster_pen.add_saturating(glyph.advance);
        }
    }
}

/// Горизонтальное выравнивание в пределах ширины блока: левое, центр, правое.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HorizontalAlignment {
    Left,
    Center,
    Right,
}

impl HorizontalAlignment {
    /// По якорю на экране.
    pub(crate) fn of(anchor: ScreenAnchor) -> Self {
        match anchor {
            ScreenAnchor::BottomLeft | ScreenAnchor::MiddleLeft | ScreenAnchor::TopLeft => {
                Self::Left
            }
            ScreenAnchor::BottomCenter | ScreenAnchor::MiddleCenter | ScreenAnchor::TopCenter => {
                Self::Center
            }
            ScreenAnchor::BottomRight | ScreenAnchor::MiddleRight | ScreenAnchor::TopRight => {
                Self::Right
            }
        }
    }
}

/// Рисуемый символ участвует в ширине строки.
fn contributes_width(cluster: &TextCluster) -> bool {
    !cluster.skip && cluster.symbol != '\n' && cluster.symbol != '\0'
}

/// Сдвигает каждую строку по горизонтали согласно выравниванию (libass: `align_lines` при
/// автоматическом выравнивании текста внутри блока, `justify = auto`).
///
/// Ширина строки — сумма продвижений её нескрытых кластеров; блок шириной `max_text_width`
/// (между левым и правым полем).
pub(super) fn align_lines(
    clusters: &mut [TextCluster],
    alignment: HorizontalAlignment,
    max_text_width: f64,
) {
    let length = clusters.len();
    let mut line_first = 0_usize;
    for index in 1..=length {
        let starts_line = index == length
            || clusters
                .get(index)
                .is_some_and(|cluster| cluster.line_start.begins_line());
        if !starts_line {
            continue;
        }
        let line = clusters.get_mut(line_first..index).unwrap_or(&mut []);
        let line_width: f64 = line
            .iter()
            .filter(|cluster| contributes_width(cluster))
            .map(|cluster| d6_to_double(cluster.cluster_advance.x))
            .sum();
        let shift = match alignment {
            HorizontalAlignment::Left => 0.0,
            HorizontalAlignment::Right => max_text_width - line_width,
            HorizontalAlignment::Center => (max_text_width - line_width) / 2.0,
        };
        for cluster in line {
            for glyph in cluster.glyphs_mut() {
                glyph.pos.x = glyph.pos.x.saturating_add(double_to_d6(shift));
            }
        }
        line_first = index;
    }
}
