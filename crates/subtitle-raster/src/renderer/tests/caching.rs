//! «Растеризуем только при изменении» и кэши.

use super::*;
use crate::overlay::OverlayChange;
use crate::{AttachedFontLimits, EMBEDDED_FONTS};

fn youtube() -> SubtitleAppearance {
    SubtitleAppearance::youtube_like()
}

#[test]
fn identical_request_is_unchanged_without_any_rasterization() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = cues(1, &["Привет, как дела?"]);
    let first = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(first.change(), OverlayChange::Replaced);
    let before = rasterizer.statistics();

    let second = render_once(&mut rasterizer, &test_cues, &youtube());
    let after = rasterizer.statistics();
    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(second.revision(), first.revision());
    assert_eq!(after.rasterized_cues, before.rasterized_cues);
    // Быстрый путь не заходит даже в кэш реплик и в кэш лиц.
    assert_eq!(after.cue_cache, before.cue_cache);
    assert_eq!(after.face_cache, before.face_cache);
    assert_eq!(after.unchanged_frames, before.unchanged_frames + 1);
    assert_eq!(after.rendered_frames, before.rendered_frames);
    // Картинки те же (общие маски).
    assert!(
        second
            .bitmaps()
            .iter()
            .zip(first.bitmaps())
            .all(|(a, b)| Arc::ptr_eq(a.shared_coverage(), b.shared_coverage()))
    );
}

#[test]
fn returning_to_an_earlier_cue_hits_the_cache_and_shares_masks() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let appearance = youtube();
    let a = cues(1, &["Привет, как дела?"]);
    let b = cues(2, &["Совсем другая реплика"]);

    let first = render_once(&mut rasterizer, &a, &appearance);
    let middle = render_once(&mut rasterizer, &b, &appearance);
    assert_eq!(middle.change(), OverlayChange::Replaced);
    let rasterized_before = rasterizer.statistics().rasterized_cues;
    let hits_before = rasterizer.statistics().cue_cache.hits;

    let again = render_once(&mut rasterizer, &a, &appearance);
    let statistics = rasterizer.statistics();
    assert_eq!(
        statistics.rasterized_cues, rasterized_before,
        "no new rasterization"
    );
    assert_eq!(statistics.cue_cache.hits, hits_before + 1);
    assert_eq!(again.bitmaps().len(), first.bitmaps().len());
    for (old, new) in first.bitmaps().iter().zip(again.bitmaps()) {
        assert!(Arc::ptr_eq(old.shared_coverage(), new.shared_coverage()));
        assert_eq!(old.coverage(), new.coverage());
        assert_eq!(old.origin(), new.origin());
    }
    // Для зрителя это новый кадр (между ними была другая реплика), ревизия растёт.
    assert_eq!(again.change(), OverlayChange::Replaced);
    assert!(again.revision() > middle.revision());
}

#[test]
fn changing_appearance_or_canvas_redraws_and_reports_replaced() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = cues(1, &["Привет"]);
    let first = render_once(&mut rasterizer, &test_cues, &youtube());

    let other_style = render_once(
        &mut rasterizer,
        &test_cues,
        &SubtitleAppearance::outline_and_shadow(),
    );
    assert_eq!(other_style.change(), OverlayChange::Replaced);
    assert!(other_style.revision() > first.revision());
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);

    let small = CanvasSize::new(640, 360).expect("canvas");
    let resized = render_on(
        &mut rasterizer,
        &test_cues,
        &SubtitleAppearance::outline_and_shadow(),
        small,
    );
    assert_eq!(resized.change(), OverlayChange::Replaced);
    assert_eq!(resized.canvas(), small);
    // Шрифт масштабируется вместе с холстом: на половинном холсте текст вдвое ниже.
    let big_ink = ink_bounds(of_kind(&other_style, SubtitleBitmapKind::Glyph)).expect("ink");
    let small_ink = ink_bounds(of_kind(&resized, SubtitleBitmapKind::Glyph)).expect("ink");
    let big_height = f64::from(big_ink.3 - big_ink.1);
    let small_height = f64::from(small_ink.3 - small_ink.1);
    assert!(
        (small_height / big_height - 0.5).abs() < 0.1,
        "{small_height} vs {big_height}"
    );
}

#[test]
fn cue_cache_never_exceeds_its_limit_and_evicts_the_old() {
    let limits = RasterCacheLimits {
        cue_bitmaps_bytes: 120_000,
        shaper_faces: 8,
    };
    let mut rasterizer = rasterizer(limits);
    let appearance = youtube();
    let texts: Vec<String> = (0..40)
        .map(|number| format!("Реплика номер {number}"))
        .collect();
    for (index, text) in texts.iter().enumerate() {
        let single = cues(index as u64, &[text.as_str()]);
        let overlay = render_once(&mut rasterizer, &single, &appearance);
        assert!(!overlay.is_empty());
        let statistics = rasterizer.statistics().cue_cache;
        assert!(
            statistics.current_bytes <= limits.cue_bitmaps_bytes,
            "step {index}: {} bytes",
            statistics.current_bytes
        );
    }
    let statistics = rasterizer.statistics().cue_cache;
    assert!(statistics.evictions > 0, "{statistics:?}");
    assert!(statistics.entries < 40 && statistics.entries > 0);
    // Вытесненная первая реплика растеризуется заново.
    let rasterized_before = rasterizer.statistics().rasterized_cues;
    render_once(&mut rasterizer, &cues(0, &[texts[0].as_str()]), &appearance);
    assert_eq!(
        rasterizer.statistics().rasterized_cues,
        rasterized_before + 1
    );
}

#[test]
fn cue_bigger_than_the_whole_cache_is_still_drawn_but_not_cached() {
    let limits = RasterCacheLimits {
        cue_bitmaps_bytes: 1_000,
        shaper_faces: 8,
    };
    let mut rasterizer = rasterizer(limits);
    let test_cues = cues(1, &["Привет, как дела?"]);
    let overlay = render_once(&mut rasterizer, &test_cues, &youtube());
    assert!(
        !overlay.is_empty(),
        "the cue is rendered despite the tiny cache"
    );
    let statistics = rasterizer.statistics().cue_cache;
    assert_eq!(statistics.rejected_oversized, 1);
    assert_eq!((statistics.entries, statistics.current_bytes), (0, 0));
}

#[test]
fn empty_cue_list_gives_an_empty_overlay_and_following_content_is_replaced() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let appearance = youtube();
    let empty = render_once(&mut rasterizer, &[], &appearance);
    assert!(empty.is_empty());
    assert_eq!(empty.canvas(), canvas());

    let again = render_once(&mut rasterizer, &[], &appearance);
    assert_eq!(again.change(), OverlayChange::Unchanged);
    assert_eq!(again.revision(), empty.revision());

    let shown = render_once(&mut rasterizer, &cues(1, &["Привет"]), &appearance);
    assert!(!shown.is_empty());
    assert_eq!(shown.change(), OverlayChange::Replaced);

    let hidden = render_once(&mut rasterizer, &[], &appearance);
    assert!(hidden.is_empty());
    assert_eq!(hidden.change(), OverlayChange::Replaced);
    assert!(hidden.revision() > shown.revision());
}

#[test]
fn blank_cue_draws_nothing_and_is_not_an_error() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_once(&mut rasterizer, &cues(1, &["   "]), &youtube());
    assert!(overlay.is_empty());
    assert_eq!(rasterizer.statistics().failed_cues, 0);
}

#[test]
fn moving_a_cue_with_the_same_masks_is_reported_as_moved() {
    // Через публичный запрос такой кадр недостижим (сдвиг зависит только от набора реплик, а
    // набор определяет и маски), поэтому проверяем сборку результата напрямую: те же маски
    // на других местах -> Moved, ревизия растёт, маски не копируются.
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_once(&mut rasterizer, &cues(1, &["Привет"]), &youtube());
    let shifted: Vec<SubtitleBitmap> = first
        .bitmaps()
        .iter()
        .map(|bitmap| bitmap.shifted_vertically(-30))
        .collect();
    let moved = rasterizer.assemble_overlay(canvas(), shifted);
    assert_eq!(moved.change(), OverlayChange::Moved);
    assert_eq!(moved.revision(), first.revision().next());
    for (old, new) in first.bitmaps().iter().zip(moved.bitmaps()) {
        assert!(Arc::ptr_eq(old.shared_coverage(), new.shared_coverage()));
        assert_eq!(new.origin().y, old.origin().y - 30);
    }
}

#[test]
fn registering_an_attached_font_drops_cached_cues_and_forces_a_redraw() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = cues(1, &["Привет"]);
    let first = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(rasterizer.statistics().cue_cache.entries, 1);

    let limits = AttachedFontLimits {
        max_font_bytes: 16 * 1024 * 1024,
        max_total_bytes: 64 * 1024 * 1024,
        max_faces: 64,
    };
    let bold = EMBEDDED_FONTS.first().expect("an embedded font");
    let registration = rasterizer
        .register_attached_font(Arc::from(bold.data), &limits)
        .expect("valid font");
    assert!(!registration.faces.is_empty());
    assert_eq!(rasterizer.statistics().cue_cache.entries, 0);
    assert_eq!(rasterizer.statistics().face_cache.cached_faces, 0);

    let rasterized_before = rasterizer.statistics().rasterized_cues;
    let second = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(
        rasterizer.statistics().rasterized_cues,
        rasterized_before + 1
    );
    assert_eq!(second.change(), OverlayChange::Replaced);
    assert!(second.revision() > first.revision());
}

#[test]
fn rejected_attached_font_changes_nothing() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = cues(1, &["Привет"]);
    render_once(&mut rasterizer, &test_cues, &youtube());
    let limits = AttachedFontLimits {
        max_font_bytes: 1024,
        max_total_bytes: 4096,
        max_faces: 4,
    };
    let result = rasterizer.register_attached_font(Arc::from(vec![0_u8; 16]), &limits);
    assert!(result.is_err());
    assert_eq!(rasterizer.statistics().cue_cache.entries, 1);
    let again = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(again.change(), OverlayChange::Unchanged);
}

#[test]
fn cue_larger_than_the_whole_cache_is_redrawn_each_time_but_its_masks_are_counted_for_the_frame() {
    let limits = RasterCacheLimits {
        cue_bitmaps_bytes: 1,
        shaper_faces: 16,
    };
    let mut rasterizer = rasterizer(limits);
    let test_cues = cues(1, &["Привет, как дела?"]);
    let first = render_once(&mut rasterizer, &test_cues, &youtube());
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.cue_cache.entries, 0, "nothing fits the cache");
    assert_eq!(statistics.cue_cache.rejected_oversized, 1);
    // Маски кадра вне учёта кэша, но видны в статистике кадра.
    let expected: usize = first.bitmaps().iter().map(|b| b.coverage().len()).sum();
    assert!(expected > 0);
    assert_eq!(statistics.current_frame_bytes, expected);
    assert_eq!(statistics.cue_cache.current_bytes, 0);

    // Новый кадр с тем же содержимым: реплика рисуется заново, буферы новые => Replaced.
    let other = render_once(
        &mut rasterizer,
        &cues(2, &["Привет, как дела?"]),
        &youtube(),
    );
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);
    assert_eq!(other.change(), OverlayChange::Replaced);
}
