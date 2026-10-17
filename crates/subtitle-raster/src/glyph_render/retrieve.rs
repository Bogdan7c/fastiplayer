// Порт из libass 0.17.5: ass_render.c (retrieve_glyphs, get_outline_glyph).
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

//! Загрузка контуров для всех глифов реплики и расчёт их рамок в пикселях кадра.

use std::sync::Arc;

use super::drawing::{DrawingOutcome, place_cached_drawing};
use super::{GlyphOutline, GlyphVariant};
use crate::event_style::AxisPair;
use crate::face_cache::{FaceCache, FaceLoadError};
use crate::fonts::FontStyle;
use crate::glyph_cache::OutlineCache;
use crate::glyph_info::{GlyphBox, PlacedGlyph, TextCluster};
use crate::outline::round_ties_even_to_i32;
use crate::render_diagnostics::RenderDiagnostics;
use crate::source_text::{DrawingSource, SpanLook};

/// Рамка и метрики глифа в пикселях кадра (libass: `get_outline_glyph`).
///
/// По горизонтали контур масштабируется `scale.horizontal`, по вертикали (и подъём/спуск) —
/// `scale.vertical`: `\fscx` и `\fscy` независимы.
fn place_outline(
    glyph: &mut PlacedGlyph,
    outline: Arc<GlyphOutline>,
    ascent: i32,
    descent: i32,
    scale: AxisPair,
) {
    let control = &outline.control_box;
    glyph.bbox = GlyphBox {
        x_min: round_ties_even_to_i32(f64::from(control.x_min) * scale.horizontal),
        y_min: round_ties_even_to_i32(f64::from(control.y_min) * scale.vertical),
        x_max: round_ties_even_to_i32(f64::from(control.x_max) * scale.horizontal),
        y_max: round_ties_even_to_i32(f64::from(control.y_max) * scale.vertical),
    };
    glyph.ascent = round_ties_even_to_i32(f64::from(ascent) * scale.vertical);
    glyph.descent = round_ties_even_to_i32(f64::from(descent) * scale.vertical);
    glyph.outline = Some(outline);
}

/// Загружает контуры всех глифов, добавляет межсимвольный зазор после курсива и межбуквенный
/// интервал (libass: `retrieve_glyphs`).
///
/// Масштаб по осям берётся у каждого кластера (`TextCluster::glyph_scale`): размер шрифта и
/// `\fscx`/`\fscy` у участков реплики могут быть разными. Вариант контура (синтетическое
/// утолщение/наклон, линии) выбирается по лицу КАЖДОГО глифа и виду символа. Лица должны быть
/// заранее подготовлены в `faces`; контур, который не загрузился, оставляет глиф без рамки и
/// метрик (libass тоже пропускает такой глиф). Такие глифы считаются в `skipped_glyphs`
/// возвращаемого отчёта.
///
/// Кластер-рисунок (`TextCluster::drawing`) берёт контур не из шрифта, а из команд рисования
/// в `drawings`; рисунок без контура считается в `invalid_ass_drawings`.
///
/// Контуры берутся из долгоживущего кэша контуров `outlines` (libass: `outline_cache`), общего
/// для всех реплик растеризатора.
pub(crate) fn retrieve_glyphs(
    faces: &FaceCache,
    outlines: &mut OutlineCache,
    clusters: &mut [TextCluster],
    looks: &[SpanLook],
    drawings: &[DrawingSource],
) -> Result<RenderDiagnostics, FaceLoadError> {
    let mut diagnostics = RenderDiagnostics::default();
    for index in 0..clusters.len() {
        let Some(cluster) = clusters.get_mut(index) else {
            continue;
        };
        match cluster.drawing.and_then(|id| drawings.get(id.0)) {
            Some(source) => {
                let entry = outlines.drawing(&source.commands);
                if place_cached_drawing(cluster, source, &entry) == DrawingOutcome::NoOutline {
                    diagnostics.invalid_ass_drawings =
                        diagnostics.invalid_ass_drawings.saturating_add(1);
                }
            }
            None => load_font_glyphs(faces, outlines, cluster, looks, &mut diagnostics)?,
        }
        // Порядок как в libass: сначала зазор после курсива (он читает продвижение предыдущих
        // кластеров, уже с их интервалом), затем интервал самого символа.
        add_space_after_italic(clusters, index, looks);
        if let Some(cluster) = clusters.get_mut(index) {
            cluster.cluster_advance.x = cluster
                .cluster_advance
                .x
                .saturating_add(cluster.letter_spacing_d6);
        }
    }
    Ok(diagnostics)
}

/// Загружает контуры всех глифов кластера из шрифта и считает рамки и метрики.
fn load_font_glyphs(
    faces: &FaceCache,
    outlines: &mut OutlineCache,
    cluster: &mut TextCluster,
    looks: &[SpanLook],
    diagnostics: &mut RenderDiagnostics,
) -> Result<(), FaceLoadError> {
    let glyph_scale = cluster.glyph_scale;
    let look = looks.get(cluster.look);
    for glyph in cluster.glyphs_mut() {
        let face_entry = faces.get(glyph.face)?;
        let variant = look.map_or(GlyphVariant::PLAIN, |known| {
            face_entry.style().variant_for(known)
        });
        let Some(outline) = outlines.glyph(faces, glyph.face, glyph.glyph, variant)? else {
            diagnostics.skipped_glyphs = diagnostics.skipped_glyphs.saturating_add(1);
            continue;
        };
        let scale = face_entry.scale();
        let (ascent, descent) = (scale.ascent(), scale.descent());
        place_outline(glyph, outline, ascent, descent, glyph_scale);
    }
    Ok(())
}

/// Курсивный символ перед прямым: расширяет продвижение на вылет глифа вправо
/// (libass: «Add additional space after italic to non-italic style changes»).
///
/// Вызывается для символа `index` сразу после загрузки его контура.
fn add_space_after_italic(clusters: &mut [TextCluster], index: usize, looks: &[SpanLook]) {
    let is_italic = |cluster: &TextCluster| {
        looks
            .get(cluster.look)
            .is_some_and(|look| look.slant == FontStyle::Italic)
    };
    let Some(previous_index) = index.checked_sub(1) else {
        return;
    };
    let (Some(current), Some(previous)) = (clusters.get(index), clusters.get(previous_index))
    else {
        return;
    };
    if !is_italic(previous) || is_italic(current) {
        return;
    }
    // Назад через курсивные символы без видимой рамки (пробелы).
    let mut back = previous_index;
    while back > 0
        && clusters.get(back).is_some_and(|cluster| {
            cluster.root.bbox.x_max - cluster.root.bbox.x_min == 0 && is_italic(cluster)
        })
    {
        back -= 1;
    }
    if let Some(cluster) = clusters.get_mut(back)
        && cluster.root.bbox.x_max > cluster.cluster_advance.x
    {
        cluster.cluster_advance.x = cluster.root.bbox.x_max;
    }
}
