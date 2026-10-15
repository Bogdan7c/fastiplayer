use subtitle_core::{SpanStyle, StyledText, TextSpan};

use crate::fonts::FaceOrigin;
use crate::test_support::{embedded_fonts, plain_style, plain_text, shaped_clusters};

#[test]
fn latin_text_shapes_into_one_glyph_per_symbol_with_positive_advances() {
    let mut fonts = embedded_fonts();
    let clusters = shaped_clusters(&mut fonts, &plain_text("Hello"), &plain_style());
    assert_eq!(clusters.len(), 5);
    for cluster in &clusters {
        assert!(!cluster.skip);
        assert!(cluster.extra.is_empty());
        assert!(cluster.cluster_advance.x > 0, "{:?}", cluster.symbol);
        assert_ne!(cluster.root.glyph, skrifa::GlyphId::NOTDEF);
    }
    // Все буквы одного стиля и письменности образуют один участок шейпинга.
    assert!(clusters.iter().all(|c| c.shape_run_id == 0));
    // 'l' и 'l' одинаковы, 'H' шире 'l'.
    assert_eq!(clusters[2].cluster_advance, clusters[3].cluster_advance);
    assert!(clusters[0].cluster_advance.x > clusters[2].cluster_advance.x);
}

#[test]
fn hebrew_gets_embedded_fallback_face_and_rtl_level() {
    let mut fonts = embedded_fonts();
    let clusters = shaped_clusters(&mut fonts, &plain_text("abc אבג"), &plain_style());
    let latin_face = clusters[0].face;
    let hebrew = &clusters[4];
    assert_ne!(
        hebrew.face, latin_face,
        "Hebrew letters come from another face"
    );
    assert_eq!(
        fonts.describe_face(hebrew.face).map(|d| d.origin),
        Some(FaceOrigin::Embedded)
    );
    assert_ne!(hebrew.root.glyph, skrifa::GlyphId::NOTDEF);
    assert_eq!(hebrew.bidi_level, 1);
    assert_eq!(clusters[0].bidi_level, 0);
    assert!(
        clusters[3].shape_run_id != clusters[4].shape_run_id,
        "script change splits runs"
    );
}

#[test]
fn arabic_marks_produce_extra_glyphs_in_the_same_cluster() {
    let mut fonts = embedded_fonts();
    let mut style = plain_style();
    style.font_family = Some("Noto Sans Arabic".to_owned());
    let clusters = shaped_clusters(&mut fonts, &plain_text("مرحبا"), &style);
    let dotted = clusters.iter().find(|c| c.symbol == 'ب').expect("beh");
    assert_eq!(dotted.extra.len(), 1, "beh = base glyph + dot mark");
    assert_eq!(
        dotted.cluster_advance.x,
        dotted.root.advance.x + dotted.extra[0].advance.x
    );
}

#[test]
fn style_change_starts_a_new_run() {
    let mut fonts = embedded_fonts();
    let bold = SpanStyle {
        weight: subtitle_core::FontWeight::Bold,
        ..SpanStyle::default()
    };
    let text = StyledText::new(vec![
        TextSpan::text("ab", SpanStyle::default()),
        TextSpan::text("cd", bold),
    ]);
    let clusters = shaped_clusters(&mut fonts, &text, &plain_style());
    assert!(!clusters[1].starts_new_run);
    assert!(clusters[2].starts_new_run);
    assert_ne!(
        clusters[1].face, clusters[2].face,
        "bold uses the bold face"
    );
}

#[test]
fn unknown_symbol_is_notdef_and_counted_without_panic() {
    let mut fonts = embedded_fonts();
    let before = fonts.statistics().missing_glyph_lookups;
    let clusters = shaped_clusters(&mut fonts, &plain_text("a\u{E000}b"), &plain_style());
    assert_eq!(clusters[1].root.glyph, skrifa::GlyphId::NOTDEF);
    assert!(!clusters[1].skip, "notdef is drawn");
    assert_eq!(fonts.statistics().missing_glyph_lookups, before + 1);
}

#[test]
fn ligature_absorbed_symbols_stay_skipped() {
    let mut fonts = embedded_fonts();
    let clusters = shaped_clusters(&mut fonts, &plain_text("fi"), &plain_style());
    // Noto Sans имеет лигатуру fi: либо один глиф на два символа, либо два отдельных.
    let drawn = clusters.iter().filter(|c| !c.skip).count();
    let total_glyphs: usize = clusters
        .iter()
        .filter(|c| !c.skip)
        .map(|c| 1 + c.extra.len())
        .sum();
    assert_eq!(drawn, total_glyphs);
    assert!(drawn >= 1);
}
