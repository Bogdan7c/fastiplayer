// Порт из libass 0.17.5: ass_render.c (trim_whitespace, measure_text, measure_text_on_eol,
// compute_string_bbox).
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

//! Обрезка лишних пробелов и измерение строк.

use crate::event_style::EventStyle;
use crate::fixed_point::d6_to_double;
use crate::glyph_info::TextCluster;

/// Подъём и спуск одной строки в пикселях кадра (libass: `LineInfo`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LineMetrics {
    /// От базовой линии вверх.
    pub(crate) ascent: f64,
    /// От базовой линии вниз.
    pub(crate) descent: f64,
}

/// Размеры текста, посчитанные [`measure_text`] (libass: поля `TextInfo`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextMeasure {
    /// Метрики строк сверху вниз.
    pub(crate) lines: Vec<LineMetrics>,
    /// Общая высота текста с межстрочным интервалом.
    pub(crate) height: f64,
    /// Отступ обводки над первой строкой (целые пиксели).
    pub(crate) border_top: i32,
    /// Отступ обводки под последней строкой.
    pub(crate) border_bottom: i32,
    /// Отступ обводки по горизонтали.
    pub(crate) border_x: i32,
}

/// Пробел или перевод строки, не открывающий новую строку (libass: `IS_WHITESPACE`).
fn is_trimmable(cluster: &TextCluster) -> bool {
    (cluster.symbol == ' ' || cluster.symbol == '\n') && !cluster.line_start.begins_line()
}

/// Помечает на удаление пробелы в начале и конце строк и лишние рядом с разрывами
/// (libass: `trim_whitespace`).
pub(super) fn trim_whitespace(clusters: &mut [TextCluster]) {
    let length = clusters.len();
    if length == 0 {
        return;
    }
    let mark = |clusters: &mut [TextCluster], index: usize| {
        if let Some(cluster) = clusters.get_mut(index) {
            cluster.skip = true;
            cluster.is_trimmed_whitespace = true;
        }
    };
    let trimmable =
        |clusters: &[TextCluster], index: usize| clusters.get(index).is_some_and(is_trimmable);

    // Хвостовые пробелы всего текста.
    let mut index = length - 1;
    while index > 0 && trimmable(clusters, index) {
        mark(clusters, index);
        index -= 1;
    }

    // Пробелы в начале текста.
    let mut leading = 0;
    while leading < length && trimmable(clusters, leading) {
        mark(clusters, leading);
        leading += 1;
    }
    if let Some(cluster) = clusters.get_mut(leading) {
        cluster.starts_new_run = true;
    }

    // Лишние пробелы вокруг разрывов строк.
    let mut index = 0;
    while index < length {
        let begins_line = clusters
            .get(index)
            .is_some_and(|cluster| cluster.line_start.begins_line());
        if begins_line {
            // Пробелы перед разрывом.
            let mut before = index.saturating_sub(1);
            while before > 0 && trimmable(clusters, before) {
                mark(clusters, before);
                before -= 1;
            }
            // Сам разрыв может быть пробелом.
            let mut next_run_start = index;
            if clusters
                .get(index)
                .is_some_and(|cluster| cluster.symbol == ' ' || cluster.symbol == '\n')
            {
                mark(clusters, index);
                let mut after = index + 1;
                while after < length && trimmable(clusters, after) {
                    mark(clusters, after);
                    after += 1;
                }
                next_run_start = after;
                index = after - 1;
            }
            if let Some(cluster) = clusters.get_mut(next_run_start) {
                cluster.starts_new_run = true;
            }
        }
        index += 1;
    }
}

/// Накопитель измерений одной строки.
struct LineAccumulator {
    max_ascent: i32,
    max_descent: i32,
    max_border_x: f64,
    max_border_y: f64,
    scale: f64,
    empty_trimmed: bool,
}

impl LineAccumulator {
    /// Пустая строка: масштаб 0.5/64 для строк из одних переводов (libass: пол-высоты).
    fn new() -> Self {
        Self {
            max_ascent: 0,
            max_descent: 0,
            max_border_x: 0.0,
            max_border_y: 0.0,
            scale: 0.5 / 64.0,
            empty_trimmed: true,
        }
    }
}

/// Измеряет строки (libass: `measure_text`).
///
/// Метрики обрезанных пробелов в начале и в конце строки игнорируются, кроме случая, когда
/// строка после обрезки пуста. Строка из одного перевода строки имеет половинную высоту.
pub(super) fn measure_text(
    clusters: &[TextCluster],
    line_count: usize,
    style: &EventStyle,
) -> TextMeasure {
    let mut measure = TextMeasure {
        lines: Vec::with_capacity(line_count),
        height: 0.0,
        border_top: 0,
        border_bottom: 0,
        border_x: 0,
    };
    let mut line = LineAccumulator::new();
    for cluster in clusters {
        if cluster.line_start.begins_line() {
            finish_line(&mut measure, &line);
            line = LineAccumulator::new();
        }
        if line.empty_trimmed && !cluster.is_trimmed_whitespace {
            line.empty_trimmed = false;
            // Метрики пробелов в начале строки забываются.
            line.max_ascent = 0;
            line.max_descent = 0;
            line.max_border_x = 0.0;
            line.max_border_y = 0.0;
        } else if !line.empty_trimmed && cluster.is_trimmed_whitespace {
            continue;
        }
        line.max_ascent = line.max_ascent.max(cluster.root.ascent);
        line.max_descent = line.max_descent.max(cluster.root.descent);
        line.max_border_y = line.max_border_y.max(style.border.vertical);
        line.max_border_x = line.max_border_x.max(style.border.horizontal);
        if cluster.symbol != '\n' {
            line.scale = 1.0 / 64.0;
        }
    }
    finish_line(&mut measure, &line);
    let breaks = measure.lines.len().saturating_sub(1);
    measure.height += breaks as f64 * style.line_spacing;
    measure
}

/// Записывает итог строки (libass: `measure_text_on_eol`).
fn finish_line(measure: &mut TextMeasure, line: &LineAccumulator) {
    let ascent = line.scale * f64::from(line.max_ascent);
    let descent = line.scale * f64::from(line.max_descent);
    measure.lines.push(LineMetrics { ascent, descent });
    measure.height += ascent + descent;
    // VSFilter: округление «с запасом» (+0.5, отбрасывание дробной части).
    measure.border_bottom = (line.max_border_y + 0.5) as i32;
    if measure.lines.len() == 1 {
        measure.border_top = measure.border_bottom;
    }
    measure.border_x = measure.border_x.max((line.max_border_x + 0.5) as i32);
}

/// Габариты текста по горизонтали и вертикали, пиксели (libass: `ASS_DRect` из
/// `compute_string_bbox`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TextBounds {
    pub(crate) x_min: f64,
    pub(crate) x_max: f64,
    pub(crate) y_min: f64,
    pub(crate) y_max: f64,
}

/// Габариты строки по позициям кластеров после перестановки (libass: `compute_string_bbox`).
pub(super) fn string_bounds(clusters: &[TextCluster], measure: &TextMeasure) -> TextBounds {
    if clusters.is_empty() {
        return TextBounds {
            x_min: 0.0,
            x_max: 0.0,
            y_min: 0.0,
            y_max: 0.0,
        };
    }
    let first_ascent = measure.lines.first().map_or(0.0, |line| line.ascent);
    let y_min = -first_ascent;
    let mut bounds = TextBounds {
        x_min: 32000.0,
        x_max: -32000.0,
        y_min,
        y_max: y_min + measure.height,
    };
    for cluster in clusters.iter().filter(|cluster| !cluster.skip) {
        let start = d6_to_double(cluster.root.pos.x);
        let end = start + d6_to_double(cluster.cluster_advance.x);
        bounds.x_min = bounds.x_min.min(start);
        bounds.x_max = bounds.x_max.max(end);
    }
    bounds
}
