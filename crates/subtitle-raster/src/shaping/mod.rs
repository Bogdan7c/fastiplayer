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

use crate::event_style::{AxisPair, BaseDirection, EventStyle};
use crate::face_cache::{FaceCache, FaceLoadError};
use crate::fixed_point::double_to_d6;
use crate::fonts::{
    FaceId, FaceMatch, FontRequest, GlyphLookup, SubtitleFontLibrary, TextLanguagePreference,
};
use crate::glyph_info::{LineStart, PlacedGlyph, TextCluster};
use crate::glyph_render::FREETYPE_REQUEST_SIZE;
use crate::source_text::{SourceChar, SourceText, SpanLook};
use script::{determine_scripts, is_harfbuzz_ignorable};

/// Лицо по умолчанию для каждого внешнего вида (по индексу в `SourceText::looks`).
///
/// Синтетические жирность и наклон здесь не решаются: они зависят и от лица, из которого
/// взят конкретный глиф (запасное лицо может быть другим), поэтому вариант контура выбирает
/// `glyph_render::retrieve_glyphs` по самому лицу глифа.
fn primary_faces(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    source: &SourceText,
) -> Result<Vec<FaceId>, FaceLoadError> {
    let mut primary = Vec::with_capacity(source.looks.len());
    for look in &source.looks {
        primary.push(resolve_loadable_face(fonts, faces, look)?.face);
    }
    Ok(primary)
}

/// Выбирает лицо по имени и подготавливает его; лицо, которое не загрузилось, помечается
/// непригодным и выбор повторяется (встроенный Noto Sans в конце цепочки всегда доступен).
fn resolve_loadable_face(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    look: &SpanLook,
) -> Result<FaceMatch, FaceLoadError> {
    loop {
        let matched = fonts.resolve_family(&FontRequest {
            family: look.font_family.as_deref(),
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
    language: TextLanguagePreference,
) -> Result<Vec<TextCluster>, FaceLoadError> {
    // `SourceText` гарантирует, что каждый индекс внешнего вида есть в `looks`; пустой список
    // лиц означает пустой текст, и кластеров не будет.
    let Some(default_face) = primary.first().copied() else {
        return Ok(Vec::new());
    };
    let symbols: Vec<char> = source.chars.iter().map(|c| c.ch).collect();
    let scripts = determine_scripts(&symbols);
    let mut clusters: Vec<TextCluster> = Vec::with_capacity(symbols.len());
    // Системные fallback-лица, уже выбранные в этой реплике, в порядке первого использования.
    let mut used_system_faces: Vec<(FaceId, FaceId)> = Vec::new();
    for (
        index,
        (
            SourceChar {
                ch,
                look,
                fade,
                drawing,
            },
            script_value,
        ),
    ) in source.chars.iter().zip(scripts).enumerate()
    {
        let primary_face = primary.get(*look).copied().unwrap_or(default_face);
        // Рисунок `\p`: ни поиска шрифта, ни шейпинга (libass: `shape_harfbuzz`,
        // `ass_shaper_find_runs`); лицо — основное лицо вида, глиф не используется.
        let drawing_source = drawing.and_then(|id| source.drawings.get(id.0));
        let (face, glyph) = if drawing_source.is_some() {
            (primary_face, GlyphId::NOTDEF)
        } else {
            let found = if is_harfbuzz_ignorable(*ch) {
                let inherited = clusters
                    .last()
                    .filter(|previous| previous.look == *look)
                    .map_or(primary_face, |previous| previous.face);
                (inherited, GlyphId::NOTDEF)
            } else {
                match fonts.glyph_for_char(primary_face, *ch, language) {
                    GlyphLookup::Found { face, glyph } => apply_system_run_continuity(
                        fonts,
                        &mut used_system_faces,
                        primary_face,
                        *ch,
                        (face, glyph),
                    ),
                    GlyphLookup::ControlCharacter { face } => (face, GlyphId::NOTDEF),
                    GlyphLookup::Missing { face, notdef } => (face, notdef),
                }
            };
            load_or_fall_back_to_notdef(fonts, faces, primary_face, found.0, found.1)?
        };
        let look_style = source.looks.get(*look);
        let (mut glyph_scale, mut letter_spacing_d6, has_letter_spacing) = look_style.map_or(
            (AxisPair::uniform(0.0), 0, false),
            cluster_scale_and_spacing,
        );
        if let Some(drawing_source) = drawing_source {
            // libass: у рисунка `hspacing_scaled = 0`, масштаб контура считает планировщик.
            glyph_scale = drawing_source.outline_scale;
            letter_spacing_d6 = 0;
        }
        clusters.push(TextCluster {
            symbol: *ch,
            look: *look,
            fade: *fade,
            glyph_scale,
            letter_spacing_d6,
            has_letter_spacing,
            baseline_shear: look_style.map_or(0.0, |known| known.shear.vertical),
            border: look_style.map_or(AxisPair::uniform(0.0), |known| known.border),
            drawing: drawing_source.and(*drawing),
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

/// Масштаб глифа по осям, межбуквенный интервал в 26.6 и признак «интервал задан»
/// (libass: `parse_events` — `hspacing_scaled` и `fix_glyph_scaling`).
///
/// Размер шрифта переводится во множитель к контуру 256 px, а `\fscx`/`\fscy` умножают
/// его по осям. Интервал домножается на масштаб по X ДО нормализации размера, как в libass.
fn cluster_scale_and_spacing(look: &SpanLook) -> (AxisPair, i32, bool) {
    let size_multiplier = look.font_size / FREETYPE_REQUEST_SIZE;
    let glyph_scale = AxisPair {
        horizontal: look.scale.horizontal * size_multiplier,
        vertical: look.scale.vertical * size_multiplier,
    };
    let letter_spacing_d6 = double_to_d6(look.letter_spacing * look.scale.horizontal);
    (glyph_scale, letter_spacing_d6, look.letter_spacing != 0.0)
}

/// Непрерывность системного fallback (libass `ass_font_get_index` перебирает лица, уже
/// добавленные в шрифт, раньше нового запроса): если найденное лицо системное, а символ есть
/// в системном лице, которое реплика уже использовала для того же основного лица, берём его,
/// а не «лучшее по рангу». Так соседние символы остаются в одном лице, пока оно их покрывает.
/// Основное, встроенные и вложенные лица (они выше по приоритету) не трогаются.
fn apply_system_run_continuity(
    fonts: &SubtitleFontLibrary,
    used_system_faces: &mut Vec<(FaceId, FaceId)>,
    primary_face: FaceId,
    ch: char,
    found: (FaceId, GlyphId),
) -> (FaceId, GlyphId) {
    if !fonts.is_system_face(found.0) {
        return found;
    }
    let used_for_primary: Vec<FaceId> = used_system_faces
        .iter()
        .filter(|(primary, _)| *primary == primary_face)
        .map(|(_, face)| *face)
        .collect();
    if let Some(reused) = fonts.glyph_in_used_system_faces(&used_for_primary, ch) {
        return reused;
    }
    used_system_faces.push((primary_face, found.0));
    found
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
///
/// Рисунок всегда стоит в своём участке: участок рвётся по обе стороны от него
/// (`info->drawing_text.str || last->drawing_text.str`), даже если вид у соседей тот же.
fn mark_style_runs(clusters: &mut [TextCluster]) {
    let mut previous: Option<(usize, bool)> = None;
    for cluster in clusters.iter_mut() {
        let is_drawing = cluster.drawing.is_some();
        cluster.starts_new_run = match previous {
            None => true,
            Some((previous_look, previous_is_drawing)) => {
                previous_look != cluster.look || is_drawing || previous_is_drawing
            }
        };
        previous = Some((cluster.look, is_drawing));
    }
}

/// Номера участков шейпинга (libass: цикл в `ass_shaper_find_runs`).
///
/// `split_by_style` — базовое направление «слева направо» (в libass это `!whole_text_layout`):
/// тогда символ с межбуквенным интервалом (`\fsp`) всегда начинает свой участок.
fn assign_shape_runs(clusters: &mut [TextCluster], split_by_style: bool) {
    let mut run_id = 0_u32;
    let mut previous: Option<(usize, FaceId, harfrust::Script)> = None;
    for cluster in clusters.iter_mut() {
        let ignorable = is_harfbuzz_ignorable(cluster.symbol);
        if let Some((previous_look, previous_face, previous_script)) = previous {
            let breaks_run = previous_look != cluster.look
                || (!ignorable && previous_face != cluster.face)
                || previous_script != cluster.script
                || cluster.starts_new_run
                || (split_by_style && cluster.has_letter_spacing);
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
/// метрики) берут лица оттуда. `language` задаёт региональную форму CJK при системном fallback.
pub(crate) fn shape_text(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    source: &SourceText,
    style: &EventStyle,
    language: TextLanguagePreference,
) -> Result<Vec<TextCluster>, FaceLoadError> {
    let primary = primary_faces(fonts, faces, source)?;
    let mut clusters = build_clusters(fonts, faces, source, &primary, language)?;
    mark_style_runs(&mut clusters);
    let split_by_style = style.base_direction == BaseDirection::LeftToRight;
    assign_shape_runs(&mut clusters, split_by_style);

    let symbols: Vec<char> = clusters.iter().map(|cluster| cluster.symbol).collect();
    // Граница абзаца BiDi перед символом: новый участок стиля или интервал у предыдущего
    // символа (libass: `glyphs[i + 1].starts_new_run || glyphs[i].hspacing`).
    let run_starts: Vec<bool> = clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| {
            let previous_has_spacing = index
                .checked_sub(1)
                .and_then(|previous| clusters.get(previous))
                .is_some_and(|previous| previous.has_letter_spacing);
            cluster.starts_new_run || previous_has_spacing
        })
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
        if clusters
            .get(run_start)
            .is_some_and(|first| first.drawing.is_some())
        {
            // Рисунок не шейпится: «оживляем» кластер, глиф и метрики придут из
            // `retrieve_glyphs` (libass: `glyphs[i].skip = false; continue`).
            if let Some(cluster) = clusters.get_mut(run_start) {
                cluster.skip = false;
            }
        } else {
            run_shaper::shape_run(faces, &mut clusters, run_start..run_end, source.kerning)?;
        }
        run_start = run_end;
    }
    Ok(clusters)
}

#[cfg(test)]
mod tests;
