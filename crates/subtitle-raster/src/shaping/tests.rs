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

/// Шейпит текст, у которого после разворота спанов поправлен единственный вид символов.
fn shape_with_look(
    text: &str,
    adjust: impl FnOnce(&mut crate::source_text::SourceText),
) -> Vec<crate::glyph_info::TextCluster> {
    let mut fonts = embedded_fonts();
    let style = plain_style();
    let mut source = crate::source_text::SourceText::from_styled(&plain_text(text), &style);
    adjust(&mut source);
    let mut faces = crate::face_cache::FaceCache::new(crate::test_support::SHAPING_BUDGET_BYTES);
    super::shape_text(
        &mut fonts,
        &mut faces,
        &source,
        &style,
        crate::fonts::TextLanguagePreference::unknown(),
    )
    .expect("shaping")
}

#[test]
fn letter_spacing_disables_ligatures_and_isolates_each_letter() {
    let with_ligature = shape_with_look("fi", |_| {});
    assert!(
        with_ligature.iter().any(|cluster| cluster.skip),
        "Noto Sans merges f and i into one glyph"
    );
    let spaced = shape_with_look("fi", |source| {
        for look in &mut source.looks {
            look.letter_spacing = 0.5;
        }
    });
    assert!(
        spaced.iter().all(|cluster| !cluster.skip),
        "no ligature with \\fsp"
    );
    assert!(spaced.iter().all(|cluster| cluster.has_letter_spacing));
    assert_ne!(
        spaced[0].shape_run_id, spaced[1].shape_run_id,
        "each spaced letter is shaped on its own (libass: !whole_text_layout && hspacing)"
    );
}

#[test]
fn glyph_scale_follows_font_size_and_axis_scale_and_spacing_is_scaled_with_x() {
    let clusters = shape_with_look("H", |source| {
        for look in &mut source.looks {
            look.scale = crate::event_style::AxisPair {
                horizontal: 2.0,
                vertical: 0.5,
            };
            look.letter_spacing = 3.0;
        }
    });
    let base = shape_with_look("H", |_| {});
    let (scaled, plain) = (&clusters[0], &base[0]);
    assert!((scaled.glyph_scale.horizontal - 2.0 * plain.glyph_scale.horizontal).abs() < 1e-12);
    assert!((scaled.glyph_scale.vertical - 0.5 * plain.glyph_scale.vertical).abs() < 1e-12);
    // 3 px * масштаб по X (2.0) = 6 px = 384 в 26.6; продвижение буквы выросло ровно вдвое.
    assert_eq!(scaled.letter_spacing_d6, 384);
    assert_eq!(plain.letter_spacing_d6, 0);
    assert!((scaled.root.advance.x - 2 * plain.root.advance.x).abs() <= 1);
}

#[test]
fn kerning_mode_tightens_the_kerned_pair() {
    let advance_of_first = |mode| {
        let clusters = shape_with_look("AV", |source| source.kerning = mode);
        clusters[0].cluster_advance.x
    };
    let off = advance_of_first(crate::source_text::KerningMode::Disabled);
    let on = advance_of_first(crate::source_text::KerningMode::Enabled);
    assert!(on < off, "A before V is kerned: {on} < {off}");
}

/// Шейпит `text` в общем кэше лиц и возвращает кластеры.
fn shape_in_shared_faces(
    fonts: &mut crate::fonts::SubtitleFontLibrary,
    faces: &mut crate::face_cache::FaceCache,
    text: &str,
    adjust: impl FnOnce(&mut crate::source_text::SourceText),
) -> Vec<crate::glyph_info::TextCluster> {
    let style = plain_style();
    let mut source = crate::source_text::SourceText::from_styled(&plain_text(text), &style);
    adjust(&mut source);
    super::shape_text(
        fonts,
        faces,
        &source,
        &style,
        crate::fonts::TextLanguagePreference::unknown(),
    )
    .expect("shaping")
}

#[test]
fn shape_plan_is_built_once_per_property_set_and_reused_with_identical_results() {
    let mut fonts = embedded_fonts();
    let mut faces = crate::face_cache::FaceCache::new(crate::test_support::SHAPING_BUDGET_BYTES);
    let plans_of = |faces: &crate::face_cache::FaceCache, face| {
        faces.get(face).expect("face").shape_plans().len()
    };

    let first = shape_in_shared_faces(&mut fonts, &mut faces, "Hello fi", |_| {});
    let face = first[0].face;
    assert_eq!(plans_of(&faces, face), 1);
    let second = shape_in_shared_faces(&mut fonts, &mut faces, "Hello fi", |_| {});
    assert_eq!(plans_of(&faces, face), 1, "same properties reuse the plan");
    let advances = |clusters: &[crate::glyph_info::TextCluster]| -> Vec<_> {
        clusters
            .iter()
            .map(|cluster| (cluster.skip, cluster.root.glyph, cluster.cluster_advance))
            .collect()
    };
    assert_eq!(advances(&first), advances(&second));

    // Кернинг и межбуквенный интервал меняют набор возможностей: нужны отдельные планы,
    // и они действительно действуют (кернинг сужает пару, интервал разрывает лигатуру).
    let kerned = shape_in_shared_faces(&mut fonts, &mut faces, "AV", |source| {
        source.kerning = crate::source_text::KerningMode::Enabled;
    });
    let plain = shape_in_shared_faces(&mut fonts, &mut faces, "AV", |_| {});
    assert!(kerned[0].cluster_advance.x < plain[0].cluster_advance.x);
    let spaced = shape_in_shared_faces(&mut fonts, &mut faces, "fi", |source| {
        for look in &mut source.looks {
            look.letter_spacing = 1.0;
        }
    });
    assert!(
        spaced.iter().all(|cluster| !cluster.skip),
        "no ligature with spacing"
    );
    assert_eq!(plans_of(&faces, face), 3, "default, kerning, no ligatures");
}

#[test]
fn plan_cache_builds_only_missing_plans() {
    use super::plan_cache::{PlanSelector, ShapePlanCache};
    let mut fonts = embedded_fonts();
    let mut faces = crate::face_cache::FaceCache::new(crate::test_support::SHAPING_BUDGET_BYTES);
    let clusters = shape_in_shared_faces(&mut fonts, &mut faces, "a", |_| {});
    let entry = faces.get(clusters[0].face).expect("face");
    let font = entry.font_ref().expect("font");
    let shaper = entry.shaper_data().shaper(&font).build();
    let cache = ShapePlanCache::default();
    let selector = PlanSelector {
        direction: harfrust::Direction::LeftToRight,
        script: harfrust::script::LATIN,
        kerning: false,
        ligatures: true,
    };
    let builds = std::cell::Cell::new(0);
    let build = || {
        builds.set(builds.get() + 1);
        harfrust::ShapePlan::new(
            &shaper,
            selector.direction,
            Some(selector.script),
            None,
            &[],
        )
    };
    let first = cache.plan_for(selector, build);
    let second = cache.plan_for(selector, build);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(builds.get(), 1);
    let other = PlanSelector {
        direction: harfrust::Direction::RightToLeft,
        ..selector
    };
    let third = cache.plan_for(other, build);
    assert!(!std::sync::Arc::ptr_eq(&first, &third));
    assert_eq!((builds.get(), cache.len()), (2, 2));
}

#[test]
fn shaped_run_is_cached_per_text_and_cached_result_equals_a_fresh_shaping() {
    let mut fonts = embedded_fonts();
    let mut faces = crate::face_cache::FaceCache::new(crate::test_support::SHAPING_BUDGET_BYTES);
    let runs_of =
        |faces: &crate::face_cache::FaceCache, _face| faces.shaped_runs().statistics().entries;
    let signature = |clusters: &[crate::glyph_info::TextCluster]| -> Vec<_> {
        clusters
            .iter()
            .map(|cluster| {
                (
                    cluster.skip,
                    cluster.root.glyph,
                    cluster.root.advance,
                    cluster.root.offset,
                    cluster.extra.len(),
                    cluster.cluster_advance,
                )
            })
            .collect()
    };

    let first = shape_in_shared_faces(&mut fonts, &mut faces, "Hello fi", |_| {});
    let face = first[0].face;
    assert_eq!(runs_of(&faces, face), 1);
    let cached = shape_in_shared_faces(&mut fonts, &mut faces, "Hello fi", |_| {});
    assert_eq!(runs_of(&faces, face), 1, "the same run is not shaped again");
    // Результат из кэша совпадает с шейпингом в пустом кэше лиц.
    let fresh = shape_with_look("Hello fi", |_| {});
    assert_eq!(signature(&first), signature(&fresh));
    assert_eq!(signature(&cached), signature(&fresh));

    // Другой текст и другие свойства - отдельные записи.
    shape_in_shared_faces(&mut fonts, &mut faces, "Hello", |_| {});
    shape_in_shared_faces(&mut fonts, &mut faces, "Hello fi", |source| {
        source.kerning = crate::source_text::KerningMode::Enabled;
    });
    assert_eq!(runs_of(&faces, face), 3);

    // Размер шрифта не часть ключа: масштаб применяется после кэша, продвижения пропорциональны.
    let doubled = shape_in_shared_faces(&mut fonts, &mut faces, "Hello fi", |source| {
        for look in &mut source.looks {
            look.font_size *= 2.0;
        }
    });
    assert_eq!(runs_of(&faces, face), 3, "size reuses the cached shaping");
    assert!((doubled[0].root.advance.x - 2 * first[0].root.advance.x).abs() <= 1);
}
