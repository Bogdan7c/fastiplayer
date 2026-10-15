// Порт из libass 0.17.5: ass_shaper.c (ass_shaper_find_runs, ass_shaper_shape,
// ass_shaper_skip_characters) и ass_render.c (split_style_runs).
//
// Copyright (C) 2011 Grigori Goronzy <greg@chown.ath.cx>
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

//! Шейпинг реплики: от плоского текста к кластерам с глифами и продвижениями.
//!
//! Порядок стадий повторяет libass:
//! 1. `split_style_runs` — граница участка там, где изменился внешний вид символа;
//! 2. выбор лица для каждого символа (основное лицо стиля, затем fallback по глифам);
//! 3. письменность символов и разбиение на участки шейпинга (`shape_run_id`);
//! 4. уровни BiDi;
//! 5. шейпинг каждого участка `harfrust`.
//!
//! Раскладка (позиции глифов, переносы) — в модуле [`crate::layout`].

pub(crate) mod bidi;
mod run_shaper;
pub(crate) mod script;

use skrifa::GlyphId;

use crate::event_style::EventStyle;
use crate::face_cache::{FaceCache, FaceLoadError};
use crate::fonts::{
    FaceId, FaceMatch, FontRequest, GlyphLookup, SubtitleFontLibrary, SyntheticBold,
    SyntheticItalic,
};
use crate::glyph_info::{LineStart, PlacedGlyph, TextCluster};
use crate::glyph_render::FREETYPE_REQUEST_SIZE;
use crate::source_text::{SourceChar, SourceText, SpanLook};
use script::{determine_scripts, is_harfbuzz_ignorable};

/// Лицо по умолчанию для каждого внешнего вида (по индексу в `SourceText::looks`).
///
/// Здесь же принимается решение о синтетике: libass утолщает/наклоняет контур, если у лица
/// нужного начертания нет. Этот шаг пока не портирован, поэтому о нём пишется в журнал.
fn primary_faces(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    source: &SourceText,
    style: &EventStyle,
) -> Result<Vec<FaceId>, FaceLoadError> {
    let mut primary = Vec::with_capacity(source.looks.len());
    for look in &source.looks {
        let matched = resolve_loadable_face(fonts, faces, style, look)?;
        if matched.synthetic_bold == SyntheticBold::Needed
            || matched.synthetic_italic == SyntheticItalic::Needed
        {
            tracing::debug!(
                face = matched.face.index(),
                synthetic_bold = ?matched.synthetic_bold,
                synthetic_italic = ?matched.synthetic_italic,
                "the face lacks the requested style; synthetic emboldening/slanting is not ported, drawing as is"
            );
        }
        primary.push(matched.face);
    }
    Ok(primary)
}

/// Выбирает лицо по имени и подготавливает его; лицо, которое не загрузилось, помечается
/// непригодным и выбор повторяется (встроенный Noto Sans в конце цепочки всегда доступен).
fn resolve_loadable_face(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    style: &EventStyle,
    look: &SpanLook,
) -> Result<FaceMatch, FaceLoadError> {
    loop {
        let matched = fonts.resolve_family(&FontRequest {
            family: style.font_family.as_deref(),
            weight: look.weight,
            style: look.slant,
        });
        match faces.ensure_loaded(fonts, matched.face) {
            Ok(()) => return Ok(matched),
            // Каждый оборот помечает НОВОЕ лицо, поэтому цикл конечен; `false` — лицо
            // встроенное или уже помеченное, повторять бессмысленно.
            Err(error) if !fonts.mark_face_unusable(matched.face) => return Err(error),
            Err(_) => {}
        }
    }
}

/// Создаёт кластеры: выбирает лицо и номинальный глиф каждого символа.
///
/// Невидимые для HarfBuzz символы (`is_harfbuzz_ignorable`) лицо не ищут — как в libass они
/// наследуют лицо соседа, чтобы не рвать участок шейпинга.
fn build_clusters(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    source: &SourceText,
    primary: &[FaceId],
) -> Result<Vec<TextCluster>, FaceLoadError> {
    // `SourceText` гарантирует, что каждый индекс внешнего вида есть в `looks`; пустой список
    // лиц означает пустой текст, и кластеров не будет.
    let Some(default_face) = primary.first().copied() else {
        return Ok(Vec::new());
    };
    let symbols: Vec<char> = source.chars.iter().map(|c| c.ch).collect();
    let scripts = determine_scripts(&symbols);
    let mut clusters: Vec<TextCluster> = Vec::with_capacity(symbols.len());
    for (index, (SourceChar { ch, look }, script_value)) in
        source.chars.iter().zip(scripts).enumerate()
    {
        let primary_face = primary.get(*look).copied().unwrap_or(default_face);
        let (face, glyph) = if is_harfbuzz_ignorable(*ch) {
            let inherited = clusters
                .last()
                .filter(|previous| previous.look == *look)
                .map_or(primary_face, |previous| previous.face);
            (inherited, GlyphId::NOTDEF)
        } else {
            match fonts.glyph_for_char(primary_face, *ch) {
                GlyphLookup::Found { face, glyph } => (face, glyph),
                GlyphLookup::ControlCharacter { face } => (face, GlyphId::NOTDEF),
                GlyphLookup::Missing { face, notdef } => (face, notdef),
            }
        };
        let (face, glyph) = load_or_fall_back_to_notdef(fonts, faces, primary_face, face, glyph)?;
        clusters.push(TextCluster {
            symbol: *ch,
            look: *look,
            face,
            script: script_value,
            bidi_level: 0,
            paragraph_level: 0,
            shape_run_id: 0,
            starts_new_run: index == 0,
            skip: false,
            is_trimmed_whitespace: false,
            line_start: LineStart::NotStarted,
            root: PlacedGlyph::unshaped(glyph, face),
            extra: Vec::new(),
            cluster_advance: crate::fixed_point::Vector26::default(),
        });
    }
    Ok(clusters)
}

/// Подготавливает лицо глифа. Запасное лицо, которое не загрузилось, помечается непригодным,
/// а символ рисуется как `.notdef` основного лица: реплика не теряется из-за одного шрифта.
fn load_or_fall_back_to_notdef(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    primary_face: FaceId,
    face: FaceId,
    glyph: GlyphId,
) -> Result<(FaceId, GlyphId), FaceLoadError> {
    match faces.ensure_loaded(fonts, face) {
        Ok(()) => Ok((face, glyph)),
        Err(error) => {
            if face == primary_face || !fonts.mark_face_unusable(face) {
                return Err(error);
            }
            faces.ensure_loaded(fonts, primary_face)?;
            Ok((primary_face, GlyphId::NOTDEF))
        }
    }
}

/// Границы участков единого стиля (libass: `split_style_runs`).
fn mark_style_runs(clusters: &mut [TextCluster]) {
    let mut previous_look: Option<usize> = None;
    for cluster in clusters.iter_mut() {
        cluster.starts_new_run = previous_look != Some(cluster.look);
        previous_look = Some(cluster.look);
    }
}

/// Номера участков шейпинга (libass: цикл в `ass_shaper_find_runs`).
fn assign_shape_runs(clusters: &mut [TextCluster]) {
    let mut run_id = 0_u32;
    let mut previous: Option<(usize, FaceId, harfrust::Script)> = None;
    for cluster in clusters.iter_mut() {
        let ignorable = is_harfbuzz_ignorable(cluster.symbol);
        if let Some((previous_look, previous_face, previous_script)) = previous {
            let breaks_run = previous_look != cluster.look
                || (!ignorable && previous_face != cluster.face)
                || previous_script != cluster.script
                || cluster.starts_new_run;
            if breaks_run {
                run_id += 1;
            } else if ignorable {
                cluster.face = previous_face;
                cluster.root.face = previous_face;
            }
        }
        cluster.shape_run_id = run_id;
        previous = Some((cluster.look, cluster.face, cluster.script));
    }
}

/// Шейпит реплику и возвращает кластеры в логическом порядке.
///
/// `faces` пополняется всеми лицами, которые понадобились; дальнейшие стадии (контуры,
/// метрики) берут лица оттуда.
pub(crate) fn shape_text(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    source: &SourceText,
    style: &EventStyle,
) -> Result<Vec<TextCluster>, FaceLoadError> {
    let primary = primary_faces(fonts, faces, source, style)?;
    let mut clusters = build_clusters(fonts, faces, source, &primary)?;
    mark_style_runs(&mut clusters);
    assign_shape_runs(&mut clusters);

    let symbols: Vec<char> = clusters.iter().map(|cluster| cluster.symbol).collect();
    let run_starts: Vec<bool> = clusters
        .iter()
        .map(|cluster| cluster.starts_new_run)
        .collect();
    let levels = bidi::resolve_levels(&symbols, &run_starts, style.base_direction);
    for ((cluster, level), paragraph) in clusters
        .iter_mut()
        .zip(&levels.embedding)
        .zip(&levels.paragraph)
    {
        cluster.bidi_level = *level;
        cluster.paragraph_level = *paragraph;
    }

    // libass (shape_harfbuzz): сначала все пропущены, затем шейпер «оживляет» кластеры.
    for cluster in &mut clusters {
        cluster.skip = true;
    }
    let font_scale = style.font_size / FREETYPE_REQUEST_SIZE;
    let mut run_start = 0;
    while run_start < clusters.len() {
        let (run_id, level) = match clusters.get(run_start) {
            Some(first) => (first.shape_run_id, first.bidi_level),
            None => break,
        };
        let mut run_end = run_start + 1;
        while clusters
            .get(run_end)
            .is_some_and(|next| next.shape_run_id == run_id && next.bidi_level == level)
        {
            run_end += 1;
        }
        run_shaper::shape_run(faces, &mut clusters, run_start..run_end, font_scale)?;
        run_start = run_end;
    }
    Ok(clusters)
}

#[cfg(test)]
mod tests;
