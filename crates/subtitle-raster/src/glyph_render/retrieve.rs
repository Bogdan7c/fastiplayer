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

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::GlyphId;

use super::{GlyphOutline, GlyphOutlineError, load_glyph_outline};
use crate::face_cache::{FaceCache, FaceLoadError};
use crate::fonts::{FaceId, FontStyle};
use crate::glyph_info::{GlyphBox, PlacedGlyph, TextCluster};
use crate::outline::{ControlBox, Outline, round_ties_even_to_i32};
use crate::render_diagnostics::RenderDiagnostics;
use crate::source_text::SpanLook;

/// Контуры, уже загруженные для текущей реплики (один глиф встречается много раз).
#[derive(Default)]
struct OutlineMemo {
    entries: HashMap<(FaceId, GlyphId), Option<Arc<GlyphOutline>>>,
}

impl OutlineMemo {
    /// Контур глифа или `None`, если он не загрузился (глиф тогда не рисуется).
    fn get(
        &mut self,
        faces: &FaceCache,
        face: FaceId,
        glyph: GlyphId,
    ) -> Result<Option<Arc<GlyphOutline>>, FaceLoadError> {
        if let Some(known) = self.entries.get(&(face, glyph)) {
            return Ok(known.clone());
        }
        let entry = faces.get(face)?;
        let font = entry.font_ref()?;
        let loaded = match load_glyph_outline(&font, entry.scale(), glyph) {
            Ok(outline) => Some(Arc::new(outline)),
            // Глиф без векторного контура (пробел, пустой глиф): рисовать нечего, но метрики
            // лица остаются в силе — как у пустого контура FreeType.
            Err(GlyphOutlineError::NoOutline { .. }) => Some(Arc::new(GlyphOutline {
                outline: Outline::new(),
                control_box: ControlBox {
                    x_min: 0,
                    y_min: 0,
                    x_max: 0,
                    y_max: 0,
                },
            })),
            Err(error) => {
                tracing::debug!(%error, face = face.index(), "glyph outline is unavailable, the glyph is not drawn");
                None
            }
        };
        self.entries.insert((face, glyph), loaded.clone());
        Ok(loaded)
    }
}

/// Рамка и метрики глифа в пикселях кадра (libass: `get_outline_glyph`).
fn place_outline(
    glyph: &mut PlacedGlyph,
    outline: Arc<GlyphOutline>,
    ascent: i32,
    descent: i32,
    scale: f64,
) {
    let control = &outline.control_box;
    glyph.bbox = GlyphBox {
        x_min: round_ties_even_to_i32(f64::from(control.x_min) * scale),
        y_min: round_ties_even_to_i32(f64::from(control.y_min) * scale),
        x_max: round_ties_even_to_i32(f64::from(control.x_max) * scale),
        y_max: round_ties_even_to_i32(f64::from(control.y_max) * scale),
    };
    glyph.ascent = round_ties_even_to_i32(f64::from(ascent) * scale);
    glyph.descent = round_ties_even_to_i32(f64::from(descent) * scale);
    glyph.outline = Some(outline);
}

/// Загружает контуры всех глифов и добавляет межсимвольный зазор после курсива.
///
/// `font_scale` — множитель `font_size / 256`. Лица должны быть заранее подготовлены в
/// `faces`; контур, который не загрузился, оставляет глиф без рамки и метрик (libass тоже
/// пропускает такой глиф). Такие глифы считаются в `skipped_glyphs` возвращаемого отчёта.
pub(crate) fn retrieve_glyphs(
    faces: &FaceCache,
    clusters: &mut [TextCluster],
    looks: &[SpanLook],
    font_scale: f64,
) -> Result<RenderDiagnostics, FaceLoadError> {
    let mut memo = OutlineMemo::default();
    let mut diagnostics = RenderDiagnostics::default();
    for cluster in clusters.iter_mut() {
        for glyph in cluster.glyphs_mut() {
            let Some(outline) = memo.get(faces, glyph.face, glyph.glyph)? else {
                diagnostics.skipped_glyphs = diagnostics.skipped_glyphs.saturating_add(1);
                continue;
            };
            let scale = faces.get(glyph.face)?.scale();
            let (ascent, descent) = (scale.ascent(), scale.descent());
            place_outline(glyph, outline, ascent, descent, font_scale);
        }
    }
    add_space_after_italic(clusters, looks);
    Ok(diagnostics)
}

/// Курсивный символ перед прямым: расширяет продвижение на вылет глифа вправо
/// (libass: «Add additional space after italic to non-italic style changes»).
fn add_space_after_italic(clusters: &mut [TextCluster], looks: &[SpanLook]) {
    let is_italic = |cluster: &TextCluster| {
        looks
            .get(cluster.look)
            .is_some_and(|look| look.slant == FontStyle::Italic)
    };
    for index in 1..clusters.len() {
        let (Some(current), Some(previous)) = (clusters.get(index), clusters.get(index - 1)) else {
            continue;
        };
        if !is_italic(previous) || is_italic(current) {
            continue;
        }
        // Назад через курсивные символы без видимой рамки (пробелы).
        let mut back = index - 1;
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
}
