use skrifa::{FontRef, GlyphId, MetadataProvider};

use super::*;
use crate::face_cache::FaceCache;
use crate::fonts::FaceMetrics;
use crate::test_support::embedded_fonts;

const NOTO_SANS: &[u8] = include_bytes!("../../fonts/NotoSans-Regular.ttf");

fn noto_scale() -> (FontRef<'static>, FaceScale) {
    let font = FontRef::new(NOTO_SANS).expect("font");
    let fonts = embedded_fonts();
    let face = fonts
        .resolve_family(&crate::fonts::FontRequest {
            family: None,
            weight: crate::fonts::FontWeightClass::Regular,
            style: crate::fonts::FontStyle::Normal,
        })
        .face;
    let metrics = fonts.face_metrics(face).expect("metrics");
    (font, FaceScale::from_metrics(&metrics).expect("scale"))
}

#[test]
fn scale_matches_freetype_real_dim_request_for_noto_sans() {
    let (_, scale) = noto_scale();
    // Значения, замеренные у FreeType 2.14: ячейка 1124 + 395 единиц на 256 px.
    assert_eq!(
        scale.units_to_d6(1000),
        10_786,
        "1000 units at 706874/65536"
    );
    assert_eq!(scale.ascent(), mul_fix(1124, 706_874) as i32);
    assert_eq!(scale.descent(), mul_fix(395, 706_874) as i32);
    assert_eq!(scale.shaping_scale(), 10_786);
    // Ячейка равна 256 пикселям (в 26.6) с точностью до округления.
    assert!((scale.ascent() + scale.descent() - 256 * 64).abs() <= 1);
}

#[test]
fn degenerate_metrics_have_no_scale() {
    let metrics = FaceMetrics {
        units_per_em: 1000,
        ascender: 0,
        descender: 0,
        height: 0,
        source: crate::fonts::AscentDescentSource::Hhea,
    };
    assert!(FaceScale::from_metrics(&metrics).is_none());
}

#[test]
fn letter_h_outline_matches_freetype_box() {
    let (font, scale) = noto_scale();
    let glyph = font.charmap().map('H').expect("H");
    let outline = load_glyph_outline(&font, &scale, glyph).expect("outline");
    // FreeType (NO_HINTING, 256 px): horiBearingX 1046, width 5889 в 26.6.
    assert_eq!(outline.control_box.x_min, 1046);
    assert_eq!(outline.control_box.x_max, 1046 + 5889);
    assert!(
        outline.control_box.y_min < 0 && outline.control_box.y_max <= 0,
        "Y points down, glyph above baseline"
    );
}

#[test]
fn space_has_no_visible_outline_and_missing_glyph_is_reported() {
    let (font, scale) = noto_scale();
    let space = font.charmap().map(' ').expect("space");
    let outline = load_glyph_outline(&font, &scale, space).expect("space outline");
    assert!(outline.outline.is_empty());
    assert!(load_glyph_outline(&font, &scale, GlyphId::new(60_000)).is_err());
}

#[test]
fn hinted_advances_match_freetype_autohinter() {
    let fonts = embedded_fonts();
    let face = fonts
        .resolve_family(&crate::fonts::FontRequest {
            family: None,
            weight: crate::fonts::FontWeightClass::Regular,
            style: crate::fonts::FontStyle::Normal,
        })
        .face;
    let mut faces = FaceCache::new(crate::test_support::SHAPING_BUDGET_BYTES);
    faces.ensure_loaded(&fonts, face).expect("face");
    let entry = faces.get(face).expect("entry");
    let font = entry.font_ref().expect("font");
    let outlines = font.outline_glyphs();
    // Значения FT_Load_Glyph(FT_LOAD_DEFAULT) при размере 256 px, замеренные у FreeType.
    for (ch, expected) in [
        ('H', 8000),
        ('e', 6080),
        ('l', 2816),
        ('o', 6464),
        (',', 2880),
        (' ', 2816),
    ] {
        let glyph = font.charmap().map(ch).expect("glyph");
        assert_eq!(
            entry.advances().advance(&outlines, glyph),
            Some(expected),
            "{ch:?}"
        );
    }
}

#[test]
fn known_advance_is_absent_until_computed_and_then_matches_the_computed_value() {
    let fonts = embedded_fonts();
    let face = fonts
        .resolve_family(&crate::fonts::FontRequest {
            family: None,
            weight: crate::fonts::FontWeightClass::Regular,
            style: crate::fonts::FontStyle::Normal,
        })
        .face;
    let mut faces = FaceCache::new(crate::test_support::SHAPING_BUDGET_BYTES);
    faces.ensure_loaded(&fonts, face).expect("face");
    let entry = faces.get(face).expect("entry");
    let font = entry.font_ref().expect("font");
    let glyph = font.charmap().map('H').expect("glyph");
    assert_eq!(
        entry.advances().known_advance(glyph),
        None,
        "not computed yet"
    );
    let computed = entry.advances().advance(&font.outline_glyphs(), glyph);
    assert_eq!(computed, Some(8000));
    assert_eq!(entry.advances().known_advance(glyph), Some(computed));
}

#[test]
fn corrupted_glyph_data_is_a_load_error_not_a_missing_outline() {
    let mut bytes = crate::fonts::test_fonts::noto_regular_bytes();
    let font = FontRef::new(&bytes).expect("noto");
    let letter_b = font.charmap().map('B').expect("B");
    crate::fonts::test_fonts::corrupt_glyph_outline(&mut bytes, letter_b.to_u32() as u16);
    let font = FontRef::new(&bytes).expect("still parses");
    let scale = noto_scale().1;
    let error = load_glyph_outline(&font, &scale, letter_b).expect_err("corrupt outline");
    assert!(matches!(error, GlyphOutlineError::Load { .. }), "{error:?}");
}
