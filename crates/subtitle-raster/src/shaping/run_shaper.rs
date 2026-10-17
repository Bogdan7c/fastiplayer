// Порт из libass 0.17.5: ass_shaper.c (shape_harfbuzz, shape_harfbuzz_process_run,
// set_run_features, cached_h_advance и остальные подмены функций шрифта HarfBuzz).
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

//! Шейпинг одного участка текста через `harfrust`.
//!
//! Участок — подряд идущие символы с одним номером `shape_run_id` и одним уровнем BiDi.
//! Для него строится буфер, задаются направление, письменность, язык и набор возможностей
//! OpenType (как в libass: кернинг выключен, лигатуры включены), после чего результат
//! раскладывается обратно по кластерам реплики.

use harfrust::{
    Direction, Feature, GlyphExtents, ShapeOptions, ShapePlan, Tag, UnicodeBuffer,
    font::{BuiltinFontFuncs, FontFuncs},
};
use std::cell::OnceCell;

use skrifa::FontRef;
use skrifa::outline::OutlineGlyphCollection;
use skrifa::{GlyphId, MetadataProvider};

use super::plan_cache::PlanSelector;
use super::run_cache::ShapedGlyph;
use super::script::language_for_script;
use crate::event_style::AxisPair;
use crate::face_cache::{FaceCache, FaceEntry, FaceLoadError};
use crate::fixed_point::Vector26;
use crate::fonts::FaceId;
use crate::glyph_info::{PlacedGlyph, TextCluster};
use crate::glyph_render::{FaceScale, HintedAdvances};
use crate::outline::round_ties_even_to_i32;
use crate::source_text::KerningMode;

/// Функции шрифта для `harfrust`, повторяющие подмену метрик в libass.
///
/// libass не доверяет HarfBuzz метрики глифов: он отдаёт ему метрики FreeType. Продвижения
/// берутся хинтованными (см. [`crate::glyph_render::HintedAdvances`]), прочие размеры
/// масштабируются так же, как контуры (`FT_MulFix(единицы, y_scale)` при размере 256 px).
struct LibassFontFuncs<'a> {
    scale: FaceScale,
    advances: &'a HintedAdvances,
    font: &'a FontRef<'a>,
    /// Контуры шрифта: разбор таблиц `glyf`/`CFF` нужен только для глифа, чьё хинтованное
    /// продвижение ещё не посчитано, поэтому создаются при первой необходимости.
    outlines: OnceCell<OutlineGlyphCollection<'a>>,
}

impl LibassFontFuncs<'_> {
    fn scaled(&self, units: i32) -> i32 {
        self.scale.units_to_d6(units)
    }
}

impl FontFuncs for LibassFontFuncs<'_> {
    fn advance_width(&mut self, builtin: &BuiltinFontFuncs, glyph: GlyphId) -> i32 {
        let hinted = match self.advances.known_advance(glyph) {
            Some(known) => known,
            None => {
                let outlines = self.outlines.get_or_init(|| self.font.outline_glyphs());
                self.advances.advance(outlines, glyph)
            }
        };
        hinted.unwrap_or_else(|| self.scaled(builtin.advance_width(glyph)))
    }

    fn advance_height(&mut self, builtin: &BuiltinFontFuncs, glyph: GlyphId) -> i32 {
        self.scaled(builtin.advance_height(glyph))
    }

    fn vertical_origin(&mut self, builtin: &BuiltinFontFuncs, glyph: GlyphId) -> (i32, i32) {
        let (x, y) = builtin.vertical_origin(glyph);
        (self.scaled(x), self.scaled(y))
    }

    fn extents(&mut self, builtin: &BuiltinFontFuncs, glyph: GlyphId) -> Option<GlyphExtents> {
        let raw = builtin.extents(glyph)?;
        Some(GlyphExtents {
            x_bearing: self.scaled(raw.x_bearing),
            y_bearing: self.scaled(raw.y_bearing),
            width: self.scaled(raw.width),
            height: self.scaled(raw.height),
        })
    }
}

/// Набор возможностей OpenType участка (libass: `init_features` + `set_run_features`).
///
/// Кернинг по умолчанию выключен (`Kerning: no` у дорожки, `ass_shaper_set_kerning`),
/// лигатуры включены, но выключаются у участка с межбуквенным интервалом (`\fsp` != 0:
/// «disable ligatures if horizontal spacing is non-standard»), вертикальные замены (`vert`,
/// `vkna`) выключены — режим `@шрифт` не поддерживается.
fn run_features(kerning: KerningMode, has_letter_spacing: bool) -> [Feature; 5] {
    let kern_value = u32::from(kerning == KerningMode::Enabled);
    let ligature_value = u32::from(!has_letter_spacing);
    [
        Feature::new(Tag::new(b"vert"), 0, ..),
        Feature::new(Tag::new(b"vkna"), 0, ..),
        Feature::new(Tag::new(b"kern"), kern_value, ..),
        Feature::new(Tag::new(b"liga"), ligature_value, ..),
        Feature::new(Tag::new(b"clig"), ligature_value, ..),
    ]
}

/// Шейпит участок `run` и раскладывает глифы по кластерам.
///
/// Множители `scale_x`, `scale_y` (libass: `info->scale_x/scale_y`: `font_size / 256` с учётом
/// `\fscx`/`\fscy`), переводящие значения шейпера (1/64 пикселя при размере 256) в 26.6
/// пикселей кадра, берутся у первого кластера участка: участок единого вида
/// (`split_style_runs`) имеет один размер и масштаб. `kerning` — настройка дорожки.
///
/// Результат шейпинга (глифы при размере 256 px) берётся из кэша лица, если такой участок уже
/// шейпился (`ShapedRunCache`, один на растеризатор); масштаб кластера применяется после кэша, поэтому один
/// результат обслуживает любые размеры и `\fscx`/`\fscy`.
///
/// Перед вызовом у всех кластеров реплики должно быть `skip = true`: кластер, которому
/// шейпер не вернул ни одного глифа (символ поглощён лигатурой), так и остаётся пропущенным.
pub(super) fn shape_run(
    faces: &FaceCache,
    clusters: &mut [TextCluster],
    run: std::ops::Range<usize>,
    kerning: KerningMode,
) -> Result<(), FaceLoadError> {
    let Some(first) = clusters.get(run.start) else {
        return Ok(());
    };
    let glyph_scale = first.glyph_scale;
    let face = first.face;
    let selector = PlanSelector {
        direction: if first.bidi_level % 2 == 1 {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        },
        script: first.script,
        kerning: kerning == KerningMode::Enabled,
        ligatures: !first.has_letter_spacing,
    };
    let entry = faces.get(face)?;
    let symbols = clusters
        .get(run.clone())
        .unwrap_or(&[])
        .iter()
        .map(|cluster| cluster.symbol);
    let shaped = faces
        .shaped_runs()
        .glyphs_for(face, selector, symbols, |symbols| {
            shape_glyphs(entry, selector, symbols)
        })?;
    place_glyphs(clusters, run.start, &shaped, glyph_scale, face);
    Ok(())
}

/// Откуда шейпинг берёт план возможностей OpenType.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanSource {
    /// Из кэша планов лица (продакшен).
    FaceCache,
    /// План строит сам `harfrust` по свойствам буфера. Нужен тестам: кэшированный план обязан
    /// давать тот же результат, что собственный план `harfrust`.
    #[cfg(test)]
    HarfrustOwn,
}

/// Шейпит текст участка `harfrust` без кэша результатов (план берётся из кэша планов лица).
fn shape_glyphs(
    entry: &FaceEntry,
    selector: PlanSelector,
    symbols: &str,
) -> Result<Vec<ShapedGlyph>, FaceLoadError> {
    shape_glyphs_with(entry, selector, symbols, PlanSource::FaceCache)
}

/// То же, что [`shape_glyphs`], с явным источником плана ([`PlanSource`]).
pub(super) fn shape_glyphs_with(
    entry: &FaceEntry,
    selector: PlanSelector,
    symbols: &str,
    plan_source: PlanSource,
) -> Result<Vec<ShapedGlyph>, FaceLoadError> {
    let font = entry.font_ref()?;
    let shaper = entry.shaper_data().shaper(&font).build();

    let mut buffer = UnicodeBuffer::new();
    for (relative, symbol) in symbols.chars().enumerate() {
        buffer.add(symbol, u32::try_from(relative).unwrap_or(u32::MAX));
    }
    buffer.set_direction(selector.direction);
    buffer.set_script(selector.script);
    let language = language_for_script(selector.script);
    if let Some(language) = &language {
        buffer.set_language(language.clone());
    }

    let kerning = if selector.kerning {
        KerningMode::Enabled
    } else {
        KerningMode::Disabled
    };
    let features = run_features(kerning, !selector.ligatures);
    // План совпадает с тем, который `harfrust` строил бы сам по свойствам буфера, но берётся из
    // кэша лица (см. [`PlanSelector`]).
    let plan = match plan_source {
        PlanSource::FaceCache => Some(entry.shape_plans().plan_for(selector, || {
            ShapePlan::new(
                &shaper,
                selector.direction,
                Some(selector.script),
                language.as_ref(),
                &features,
            )
        })),
        #[cfg(test)]
        PlanSource::HarfrustOwn => None,
    };
    let mut funcs = LibassFontFuncs {
        scale: *entry.scale(),
        advances: entry.advances(),
        font: &font,
        outlines: OnceCell::new(),
    };
    let options = ShapeOptions::new()
        .scale(Some(entry.scale().shaping_scale()))
        .features(&features)
        .plan(plan.as_deref())
        .font_funcs(Some(&mut funcs));
    let shaped = shaper.shape(buffer, options);

    Ok(shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .map(|(info, position)| ShapedGlyph {
            cluster: info.cluster,
            glyph_id: info.glyph_id,
            x_advance: position.x_advance,
            y_advance: position.y_advance,
            x_offset: position.x_offset,
            y_offset: position.y_offset,
        })
        .collect())
}

/// Раскладывает глифы шейпера по кластерам участка, начинающегося с `run_start`.
fn place_glyphs(
    clusters: &mut [TextCluster],
    run_start: usize,
    shaped: &[ShapedGlyph],
    glyph_scale: AxisPair,
    face: FaceId,
) {
    for shaped_glyph in shaped {
        let index = run_start + shaped_glyph.cluster as usize;
        let Some(cluster) = clusters.get_mut(index) else {
            continue;
        };
        let glyph = PlacedGlyph {
            advance: Vector26 {
                x: round_ties_even_to_i32(
                    f64::from(shaped_glyph.x_advance) * glyph_scale.horizontal,
                ),
                y: round_ties_even_to_i32(
                    -f64::from(shaped_glyph.y_advance) * glyph_scale.vertical,
                ),
            },
            offset: Vector26 {
                x: round_ties_even_to_i32(
                    f64::from(shaped_glyph.x_offset) * glyph_scale.horizontal,
                ),
                y: round_ties_even_to_i32(-f64::from(shaped_glyph.y_offset) * glyph_scale.vertical),
            },
            ..PlacedGlyph::unshaped(GlyphId::new(shaped_glyph.glyph_id), face)
        };
        let advance = glyph.advance;
        if cluster.skip {
            // Первый глиф кластера занимает корневой слот (libass: `info->skip = false`).
            cluster.root = glyph;
            cluster.skip = false;
        } else {
            cluster.extra.push(glyph);
        }
        cluster.cluster_advance.add_saturating(advance);
    }
}
