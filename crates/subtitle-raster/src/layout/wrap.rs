// Порт из libass 0.17.5: ass_render.c (wrap_lines_naive, wrap_lines_rebalance,
// rewind_trailing_spaces, preliminary_layout).
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

//! Перенос строк: простой проход по ширине и выравнивание длин строк.
//!
//! Работает над позициями «до перестановки BiDi» (логический порядок), как в libass.
//! Разрывы строк возможны только после пробела (`' '`) — у libass без `libunibreak`
//! (`ASS_FEATURE_WRAP_UNICODE` выключен по умолчанию) других разрешённых разрывов нет.

use crate::event_style::WrapStyle;
use crate::fixed_point::{Vector26, d6_to_double};
use crate::glyph_info::{LineStart, TextCluster};

/// Пробел — единственное место, где автоматический перенос разрешён.
fn allows_break(cluster: &TextCluster) -> bool {
    cluster.symbol == ' '
}

/// Явный перевод строки.
fn forces_break(cluster: &TextCluster) -> bool {
    cluster.symbol == '\n'
}

/// Предварительные позиции без учёта BiDi (libass: `preliminary_layout`): каждый кластер
/// стоит там, где закончился предыдущий; глифы кластера идут друг за другом.
pub(crate) fn preliminary_layout(clusters: &mut [TextCluster]) {
    let mut pen = Vector26::default();
    for cluster in clusters.iter_mut() {
        let mut cluster_pen = pen;
        for glyph in cluster.glyphs_mut() {
            glyph.pos = cluster_pen;
            cluster_pen.add_saturating(glyph.advance);
        }
        pen.add_saturating(cluster.cluster_advance);
    }
}

/// Простой перенос: новая строка начинается после первого разрешённого разрыва, если
/// текущая стала шире допустимой (libass: `wrap_lines_naive`). Возвращает число строк.
pub(super) fn wrap_lines_naive(
    clusters: &mut [TextCluster],
    max_text_width: f64,
    wrap_style: WrapStyle,
) -> usize {
    let mut line_count = 1;
    let mut line_first = 0_usize;
    let mut last_breakable: Option<usize> = None;
    for index in 0..clusters.len() {
        let (Some(first), Some(current)) = (clusters.get(line_first), clusters.get(index)) else {
            break;
        };
        let line_start_offset = d6_to_double(first.left_edge());
        let length = d6_to_double(current.right_edge()) - line_start_offset;

        let mut break_at: Option<usize> = None;
        let mut break_kind = LineStart::NotStarted;
        if forces_break(current) {
            break_kind = LineStart::AfterHardBreak;
            break_at = Some(index);
        } else if length >= max_text_width
            && current.symbol != ' '
            && wrap_style != WrapStyle::NoWrap
        {
            break_kind = LineStart::AfterSoftBreak;
            break_at = last_breakable;
        }
        if allows_break(current) {
            last_breakable = Some(index);
        }

        if let Some(break_index) = break_at {
            let lead = break_index + 1;
            if lead < clusters.len() {
                if let Some(lead_cluster) = clusters.get_mut(lead) {
                    lead_cluster.line_start = break_kind;
                }
                last_breakable = None;
                line_first = lead;
                line_count += 1;
            }
        }
    }
    line_count
}

/// Назад от начала следующей строки к последнему символу, не являющемуся пробелом
/// (libass: `rewind_trailing_spaces`; `start2` не разыменовывается, он может быть «концом»).
fn rewind_trailing_spaces(clusters: &[TextCluster], start1: usize, start2: usize) -> usize {
    let mut index = start2.saturating_sub(1);
    while index > start1
        && clusters
            .get(index)
            .is_some_and(|cluster| cluster.symbol == ' ')
    {
        index -= 1;
    }
    index
}

/// Длина участка строки между рамками двух кластеров, пиксели.
fn span_length(clusters: &[TextCluster], from: usize, to: usize) -> f64 {
    let (Some(start), Some(end)) = (clusters.get(from), clusters.get(to)) else {
        return 0.0;
    };
    d6_to_double(end.right_edge() - start.left_edge())
}

/// Сдвигает мягкие разрывы так, чтобы длины соседних строк сблизились
/// (libass: `wrap_lines_rebalance`). Число строк не меняется.
pub(super) fn wrap_lines_rebalance(clusters: &mut [TextCluster], wrap_style: WrapStyle) {
    if wrap_style == WrapStyle::EndOfLine {
        return;
    }
    let length = clusters.len();
    let mut finished = false;
    while !finished {
        finished = true;
        // Начала трёх последовательных строк; `third` может указывать за конец массива.
        let mut third = 0_usize;
        let mut second: Option<usize> = None;
        for index in 0..=length {
            let starts_line = index == length
                || clusters
                    .get(index)
                    .is_some_and(|cluster| cluster.line_start.begins_line());
            if starts_line {
                let first = second;
                second = Some(third);
                third = index;
                if let (Some(line1), Some(line2)) = (first, second)
                    && clusters
                        .get(line2)
                        .is_some_and(|cluster| cluster.line_start == LineStart::AfterSoftBreak)
                {
                    match try_move_break(clusters, line1, line2, third) {
                        BreakMove::NotBeneficial => {}
                        BreakMove::Skip => continue,
                        BreakMove::Moved(new_start) => {
                            second = Some(new_start);
                            finished = false;
                        }
                    }
                }
            }
        }
    }
}

/// Результат попытки сдвинуть мягкий разрыв.
enum BreakMove {
    /// Сдвиг не уменьшает разницу длин строк.
    NotBeneficial,
    /// Слово занимает всю первую строку: объединять строки нельзя
    /// (libass: «Merging linebreaks is never beneficial»).
    Skip,
    /// Разрыв перенесён: новая строка начинается с этого кластера.
    Moved(usize),
}

/// Пробует перенести последнее слово первой строки на вторую.
fn try_move_break(
    clusters: &mut [TextCluster],
    line1: usize,
    line2: usize,
    line3: usize,
) -> BreakMove {
    // Последнее слово первой строки: без хвостовых пробелов, затем назад до разрешённого разрыва.
    let old_end = rewind_trailing_spaces(clusters, line1, line2);
    let mut word_start = old_end;
    while word_start > line1 && !clusters.get(word_start).is_some_and(allows_break) {
        word_start -= 1;
    }
    let mut new_end = word_start;
    while new_end > line1
        && clusters
            .get(new_end)
            .is_some_and(|cluster| cluster.symbol == ' ')
    {
        new_end -= 1;
    }
    if clusters
        .get(word_start)
        .is_some_and(|cluster| cluster.symbol == ' ')
    {
        word_start += 1;
    }
    if word_start == line1 {
        return BreakMove::Skip;
    }

    let line2_end = rewind_trailing_spaces(clusters, line2, line3);
    let length1 = span_length(clusters, line1, old_end);
    let length2 = span_length(clusters, line2, line2_end);
    let length1_new = span_length(clusters, line1, new_end);
    let length2_new = span_length(clusters, word_start, line2_end);

    if (length1_new - length2_new).abs() < (length1 - length2).abs() {
        if let Some(cluster) = clusters.get_mut(word_start) {
            cluster.line_start = LineStart::AfterSoftBreak;
        }
        if let Some(cluster) = clusters.get_mut(line2) {
            cluster.line_start = LineStart::NotStarted;
        }
        BreakMove::Moved(word_start)
    } else {
        BreakMove::NotBeneficial
    }
}
