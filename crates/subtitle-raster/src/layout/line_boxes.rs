//! Прямоугольники строк реплики: плотная рамка букв по горизонтали и ячейка по вертикали.
//!
//! Нужны плашке стиля «как YouTube»: она рисуется вокруг каждой строки отдельно. Это наше
//! расширение поверх портированной раскладки libass — алгоритм libass здесь не участвует,
//! мы только читаем её результат (позиции глифов и метрики строк) через намеренный тип
//! [`LineBox`], а не заставляем плашку знать устройство кластеров.

use super::{TextLayout, TextMeasure};
use crate::event_style::EventStyle;
use crate::fixed_point::d6_to_double;
use crate::glyph_info::TextCluster;

/// Прямоугольник одной видимой строки в пикселях холста (до обрезки по холсту).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LineBox {
    /// Левый край плотной рамки букв строки.
    pub(crate) left: f64,
    /// Правый край плотной рамки букв строки.
    pub(crate) right: f64,
    /// Верх ячейки строки (базовая линия минус подъём).
    pub(crate) top: f64,
    /// Низ ячейки строки (базовая линия плюс спуск).
    pub(crate) bottom: f64,
}

/// Строит прямоугольники всех строк, в которых есть видимые буквы, сверху вниз.
///
/// Вызывать надо ПОСЛЕ раскладки и ДО композиции: композиция переводит позиции глифов
/// в координаты холста, а здесь они ещё относительны блоку (смещение добавляется из `layout`).
/// Строка определяется по вертикальной позиции глифа: ближайшая из расчётных базовых
/// линий `measure.lines` (так же, как их расставил `reorder_text`).
pub(crate) fn line_boxes(
    clusters: &[TextCluster],
    layout: &TextLayout,
    style: &EventStyle,
) -> Vec<LineBox> {
    let baselines = baselines(&layout.measure, layout.device_y, style.line_spacing);
    // Горизонтальные границы строк; `None` — в строке пока нет видимых букв.
    let mut spans: Vec<Option<(f64, f64)>> = vec![None; baselines.len()];
    for cluster in clusters.iter().filter(|cluster| !cluster.skip) {
        for glyph in std::iter::once(&cluster.root).chain(cluster.extra.iter()) {
            // Глиф без контура или с пустой рамкой (пробел) плашку не растягивает.
            if glyph.outline.is_none() || glyph.bbox.x_max <= glyph.bbox.x_min {
                continue;
            }
            let baseline = layout.device_y + d6_to_double(glyph.pos.y);
            let Some(line) = nearest_line(&baselines, baseline) else {
                continue;
            };
            let left = layout.device_x + d6_to_double(glyph.pos.x.saturating_add(glyph.bbox.x_min));
            let right =
                layout.device_x + d6_to_double(glyph.pos.x.saturating_add(glyph.bbox.x_max));
            if let Some(slot) = spans.get_mut(line) {
                *slot = Some(match *slot {
                    Some((min, max)) => (min.min(left), max.max(right)),
                    None => (left, right),
                });
            }
        }
    }
    baselines
        .iter()
        .zip(&layout.measure.lines)
        .zip(spans)
        .filter_map(|((baseline, metrics), span)| {
            let (left, right) = span?;
            Some(LineBox {
                left,
                right,
                top: baseline - metrics.ascent,
                bottom: baseline + metrics.descent,
            })
        })
        .collect()
}

/// Базовые линии строк (повторяет шаг пера `reorder_text`: спуск прошлой + подъём этой + интервал).
fn baselines(measure: &TextMeasure, first_baseline: f64, line_spacing: f64) -> Vec<f64> {
    let mut result = Vec::with_capacity(measure.lines.len());
    let mut baseline = first_baseline;
    let mut previous_descent = 0.0;
    for (index, metrics) in measure.lines.iter().enumerate() {
        if index > 0 {
            baseline += previous_descent + metrics.ascent + line_spacing;
        }
        result.push(baseline);
        previous_descent = metrics.descent;
    }
    result
}

/// Номер строки с базовой линией, ближайшей к `baseline`.
fn nearest_line(baselines: &[f64], baseline: f64) -> Option<usize> {
    baselines
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (*a - baseline).abs().total_cmp(&(*b - baseline).abs()))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests;
