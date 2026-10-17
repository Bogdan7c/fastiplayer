//! Тесты кэша результатов шейпинга: бюджет и вытеснение по давности, раздельные записи для
//! разных лиц и свойств шейпинга, совпадение шейпинга по кэшированному плану с собственным
//! планом `harfrust`.

use harfrust::{Direction, script};

use super::plan_cache::PlanSelector;
use super::run_cache::{ShapedGlyph, ShapedRunCache};
use super::run_shaper::{PlanSource, shape_glyphs_with};
use crate::face_cache::FaceCache;
use crate::fonts::FaceId;
use crate::test_support::{
    SHAPING_BUDGET_BYTES, embedded_fonts, plain_style, plain_text, shaped_clusters,
};

const GLYPH: ShapedGlyph = ShapedGlyph {
    cluster: 0,
    glyph_id: 7,
    x_advance: 100,
    y_advance: 0,
    x_offset: 0,
    y_offset: 0,
};

fn latin_selector() -> PlanSelector {
    PlanSelector {
        direction: Direction::LeftToRight,
        script: script::LATIN,
        kerning: false,
        ligatures: true,
    }
}

/// Два разных лица из встроенных шрифтов: латинское и иврит.
fn two_faces() -> (FaceId, FaceId) {
    let mut fonts = embedded_fonts();
    let clusters = shaped_clusters(&mut fonts, &plain_text("abc אבג"), &plain_style());
    (clusters[0].face, clusters[4].face)
}

/// Учтённый размер записи с одним глифом и четырьмя символами текста: проверяется через бюджет
/// (три таких записи помещаются, четвёртая вытесняет самую давнюю).
fn budget_for_three_small_runs() -> usize {
    // Мерим на реальном кэше: бюджет 1 МБ, одна запись -> `current_bytes` и есть её размер.
    let probe = ShapedRunCache::new(1 << 20);
    let (face, _) = two_faces();
    probe
        .glyphs_for::<()>(face, latin_selector(), "run0".chars(), |_| Ok(vec![GLYPH]))
        .expect("shaped");
    probe.statistics().current_bytes * 3
}

#[test]
fn full_cache_drops_the_least_recently_used_runs_only() {
    let (face, _) = two_faces();
    let cache = ShapedRunCache::new(budget_for_three_small_runs());
    let shape_calls = std::cell::Cell::new(0_u32);
    let lookup = |text: &str| {
        cache
            .glyphs_for::<()>(face, latin_selector(), text.chars(), |_| {
                shape_calls.set(shape_calls.get() + 1);
                Ok(vec![GLYPH])
            })
            .expect("shaped")
    };
    for text in ["run0", "run1", "run2"] {
        lookup(text);
    }
    assert_eq!(shape_calls.get(), 3);
    // run0 снова на виду: самым давним стал run1.
    lookup("run0");
    assert_eq!(shape_calls.get(), 3, "a hit does not shape again");
    lookup("run3");
    let after_overflow = cache.statistics();
    assert_eq!(after_overflow.evictions, 1, "exactly one run is dropped");
    assert_eq!(
        after_overflow.entries, 3,
        "the cache is not cleared as a whole"
    );

    lookup("run0");
    lookup("run2");
    lookup("run3");
    assert_eq!(shape_calls.get(), 4, "recently used runs survive");
    lookup("run1");
    assert_eq!(
        shape_calls.get(),
        5,
        "the stale run was dropped and shapes again"
    );
}

#[test]
fn failures_are_not_remembered_and_oversized_runs_are_returned_uncached() {
    let (face, _) = two_faces();
    let cache = ShapedRunCache::new(budget_for_three_small_runs());
    let failed = cache.glyphs_for(face, latin_selector(), "x".chars(), |_| Err("boom"));
    assert_eq!(failed.err(), Some("boom"));
    assert_eq!(cache.statistics().entries, 0, "a failure is not cached");

    // Участок крупнее всего бюджета: результат отдаётся, но не запоминается.
    let long_text = "x".repeat(10_000);
    let result = cache
        .glyphs_for::<&str>(face, latin_selector(), long_text.chars(), |text| {
            assert_eq!(text, long_text);
            Ok(vec![GLYPH])
        })
        .expect("shaped");
    assert_eq!(&*result, &[GLYPH]);
    let stats = cache.statistics();
    assert_eq!((stats.entries, stats.rejected_oversized), (0, 1));
}

#[test]
fn runs_with_different_face_direction_script_kerning_or_ligatures_do_not_share_entries() {
    let (latin_face, hebrew_face) = two_faces();
    let cache = ShapedRunCache::new(SHAPING_BUDGET_BYTES);
    let base = latin_selector();
    let variants: [(FaceId, PlanSelector); 6] = [
        (latin_face, base),
        (hebrew_face, base),
        (
            latin_face,
            PlanSelector {
                direction: Direction::RightToLeft,
                ..base
            },
        ),
        (
            latin_face,
            PlanSelector {
                script: script::ARABIC,
                ..base
            },
        ),
        (
            latin_face,
            PlanSelector {
                kerning: true,
                ..base
            },
        ),
        (
            latin_face,
            PlanSelector {
                ligatures: false,
                ..base
            },
        ),
    ];
    for (index, (face, selector)) in variants.iter().enumerate() {
        // Каждый вариант отдаёт СВОЙ результат: глиф с номером варианта в `glyph_id`.
        let tagged = ShapedGlyph {
            glyph_id: index as u32,
            ..GLYPH
        };
        let result = cache
            .glyphs_for::<()>(*face, *selector, "AV fi".chars(), |_| Ok(vec![tagged]))
            .expect("shaped");
        assert_eq!(result[0].glyph_id, index as u32);
    }
    assert_eq!(cache.statistics().entries, variants.len());
    assert_eq!(cache.statistics().hits, 0);
    // Повторные запросы находят каждый свой результат, а не чужой.
    for (index, (face, selector)) in variants.iter().enumerate() {
        let result = cache
            .glyphs_for::<()>(*face, *selector, "AV fi".chars(), |_| {
                panic!("a cached variant must not be shaped again")
            })
            .expect("cached");
        assert_eq!(result[0].glyph_id, index as u32);
    }
    assert_eq!(cache.statistics().hits, variants.len() as u64);
}

/// Лицо с загруженными данными для текста `text` стилем `font_family`.
fn face_for(text: &str, font_family: &str) -> (FaceCache, FaceId) {
    let mut fonts = embedded_fonts();
    let mut style = plain_style();
    style.font_family = Some(font_family.to_owned());
    let source = crate::source_text::SourceText::from_styled(&plain_text(text), &style);
    let mut faces = FaceCache::new(SHAPING_BUDGET_BYTES);
    let clusters = super::shape_text(
        &mut fonts,
        &mut faces,
        &source,
        &style,
        crate::fonts::TextLanguagePreference::unknown(),
    )
    .expect("shaping");
    let face = clusters[0].face;
    (faces, face)
}

#[test]
fn arabic_text_is_cached_per_direction_and_matches_harfrust_own_plan() {
    let text = "مرحبا بالعالم";
    let (faces, face) = face_for(text, "Noto Sans Arabic");
    let entry = faces.get(face).expect("face");
    let cache = ShapedRunCache::new(SHAPING_BUDGET_BYTES);
    let selector = |direction| PlanSelector {
        direction,
        script: script::ARABIC,
        kerning: false,
        ligatures: true,
    };
    let shaped_through_cache = |direction| {
        let selector = selector(direction);
        cache
            .glyphs_for(face, selector, text.chars(), |symbols| {
                shape_glyphs_with(entry, selector, symbols, PlanSource::FaceCache)
            })
            .expect("shaped")
    };
    let rtl = shaped_through_cache(Direction::RightToLeft);
    let ltr = shaped_through_cache(Direction::LeftToRight);
    assert_eq!(
        cache.statistics().entries,
        2,
        "directions are separate entries"
    );
    assert_ne!(*rtl, *ltr, "RTL and LTR shape Arabic differently");

    // Кэшированный план даёт ровно тот же результат, что собственный план `harfrust`.
    for direction in [Direction::RightToLeft, Direction::LeftToRight] {
        let own = shape_glyphs_with(entry, selector(direction), text, PlanSource::HarfrustOwn)
            .expect("own plan");
        let cached = shaped_through_cache(direction);
        assert_eq!(*cached, own, "{direction:?}");
    }
}

#[test]
fn planned_latin_shaping_equals_own_plan_for_kerning_and_ligature_settings() {
    let (faces, face) = face_for("AV fi", "Noto Sans");
    let entry = faces.get(face).expect("face");
    for text in ["AV To fi ffl", "Hello, world", "fi"] {
        for (kerning, ligatures) in [(false, true), (true, true), (false, false), (true, false)] {
            let selector = PlanSelector {
                kerning,
                ligatures,
                ..latin_selector()
            };
            let planned =
                shape_glyphs_with(entry, selector, text, PlanSource::FaceCache).expect("planned");
            let own = shape_glyphs_with(entry, selector, text, PlanSource::HarfrustOwn)
                .expect("own plan");
            assert_eq!(
                planned, own,
                "{text:?} kerning={kerning} ligatures={ligatures}"
            );
        }
    }
    // Настройки действительно влияют на результат: кернинг сужает пару, лигатура сливает fi.
    let advance_of_first = |kerning| {
        let selector = PlanSelector {
            kerning,
            ..latin_selector()
        };
        shape_glyphs_with(entry, selector, "AV", PlanSource::FaceCache).expect("shaped")[0]
            .x_advance
    };
    assert!(advance_of_first(true) < advance_of_first(false));
    let glyph_count = |ligatures| {
        let selector = PlanSelector {
            ligatures,
            ..latin_selector()
        };
        shape_glyphs_with(entry, selector, "fi", PlanSource::FaceCache)
            .expect("shaped")
            .len()
    };
    assert!(glyph_count(true) < glyph_count(false));
}
