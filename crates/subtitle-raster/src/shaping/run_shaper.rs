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
    Direction, Feature, GlyphExtents, ShapeOptions, Tag, UnicodeBuffer,
    font::{BuiltinFontFuncs, FontFuncs},
};
use skrifa::outline::OutlineGlyphCollection;
use skrifa::{GlyphId, MetadataProvider};

use super::script::language_for_script;
use crate::face_cache::{FaceCache, FaceLoadError};
use crate::fixed_point::Vector26;
use crate::glyph_info::{PlacedGlyph, TextCluster};
use crate::glyph_render::{FaceScale, HintedAdvances};
use crate::outline::round_ties_even_to_i32;

/// Функции шрифта для `harfrust`, повторяющие подмену метрик в libass.
///
/// libass не доверяет HarfBuzz метрики глифов: он отдаёт ему метрики FreeType. Продвижения
/// берутся хинтованными (см. [`crate::glyph_render::HintedAdvances`]), прочие размеры
/// масштабируются так же, как контуры (`FT_MulFix(единицы, y_scale)` при размере 256 px).
struct LibassFontFuncs<'a> {
    scale: FaceScale,
    advances: &'a HintedAdvances,
    outlines: OutlineGlyphCollection<'a>,
}

impl LibassFontFuncs<'_> {
    fn scaled(&self, units: i32) -> i32 {
        self.scale.units_to_d6(units)
    }
}

impl FontFuncs for LibassFontFuncs<'_> {
    fn advance_width(&mut self, builtin: &BuiltinFontFuncs, glyph: GlyphId) -> i32 {
        self.advances
            .advance(&self.outlines, glyph)
            .unwrap_or_else(|| self.scaled(builtin.advance_width(glyph)))
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
/// Кернинг по умолчанию выключен (`Kerning: no` у дорожки), лигатуры включены, вертикальные
/// замены (`vert`, `vkna`) выключены — режим `@шрифт` не поддерживается.
fn run_features() -> [Feature; 5] {
    [
        Feature::new(Tag::new(b"vert"), 0, ..),
        Feature::new(Tag::new(b"vkna"), 0, ..),
        Feature::new(Tag::new(b"kern"), 0, ..),
        Feature::new(Tag::new(b"liga"), 1, ..),
        Feature::new(Tag::new(b"clig"), 1, ..),
    ]
}

/// Шейпит участок `run` и раскладывает глифы по кластерам.
///
/// `font_scale` — множитель `font_size / 256` (libass: `info->scale_x`), переводящий значения
/// шейпера (1/64 пикселя при размере 256) в 26.6 пикселей кадра.
///
/// Перед вызовом у всех кластеров реплики должно быть `skip = true`: кластер, которому
/// шейпер не вернул ни одного глифа (символ поглощён лигатурой), так и остаётся пропущенным.
pub(super) fn shape_run(
    faces: &FaceCache,
    clusters: &mut [TextCluster],
    run: std::ops::Range<usize>,
    font_scale: f64,
) -> Result<(), FaceLoadError> {
    let Some(first) = clusters.get(run.start) else {
        return Ok(());
    };
    let face = first.face;
    let script = first.script;
    let level = first.bidi_level;
    let entry = faces.get(face)?;
    let font = entry.font_ref()?;
    let shaper = entry.shaper_data().shaper(&font).build();

    let mut buffer = UnicodeBuffer::new();
    for (relative, cluster) in clusters.get(run.clone()).unwrap_or(&[]).iter().enumerate() {
        buffer.add(cluster.symbol, u32::try_from(relative).unwrap_or(u32::MAX));
    }
    buffer.set_direction(if level % 2 == 1 {
        Direction::RightToLeft
    } else {
        Direction::LeftToRight
    });
    buffer.set_script(script);
    if let Some(language) = language_for_script(script) {
        buffer.set_language(language);
    }

    let features = run_features();
    let mut funcs = LibassFontFuncs {
        scale: *entry.scale(),
        advances: entry.advances(),
        outlines: font.outline_glyphs(),
    };
    let options = ShapeOptions::new()
        .scale(Some(entry.scale().shaping_scale()))
        .features(&features)
        .font_funcs(Some(&mut funcs));
    let shaped = shaper.shape(buffer, options);

    for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let index = run.start + info.cluster as usize;
        let Some(cluster) = clusters.get_mut(index) else {
            continue;
        };
        let glyph = PlacedGlyph {
            advance: Vector26 {
                x: round_ties_even_to_i32(f64::from(position.x_advance) * font_scale),
                y: round_ties_even_to_i32(-f64::from(position.y_advance) * font_scale),
            },
            offset: Vector26 {
                x: round_ties_even_to_i32(f64::from(position.x_offset) * font_scale),
                y: round_ties_even_to_i32(-f64::from(position.y_offset) * font_scale),
            },
            ..PlacedGlyph::unshaped(GlyphId::new(info.glyph_id), face)
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
    Ok(())
}
