//! Тесты очереди готовых картинок: настоящий растеризатор делает картинки, проверяется, ЧТО
//! именно выбрано для кадра.

use std::sync::Arc;

use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};
use subtitle_raster::{
    CueKey, FontLookupCacheLimits, RasterCacheLimits, RasterInputLimits, RasterLimits,
    RenderRequest, RenderableCue, SubtitleAppearance, SubtitleFontLibrary, SubtitleRasterizer,
    TextLanguagePreference,
};

use super::*;

const SOURCE: u64 = 7;

fn ms(value: i64) -> SubtitleTime {
    SubtitleTime::from_millis(value)
}

fn canvas(width: u32) -> CanvasSize {
    CanvasSize::new(width, 720).expect("холст")
}

fn identity(source: u64, canvas_width: u32, inputs: RenderInputsRevision) -> OverlayIdentity {
    OverlayIdentity {
        source: CueSourceId::new(source),
        canvas: canvas(canvas_width),
        inputs,
    }
}

fn base_identity() -> OverlayIdentity {
    identity(SOURCE, 1280, RenderInputsRevision::INITIAL)
}

/// Тестовая «дорожка»: реплики с настоящими идентификаторами и растеризатор.
struct Scene {
    cues: Vec<(SubtitleEventId, SubtitleTextCue)>,
    rasterizer: SubtitleRasterizer,
}

impl Scene {
    fn new(texts: &[&str]) -> Self {
        let mut timeline = SubtitleTimeline::new();
        let cues = texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let cue = SubtitleTextCue {
                    text: StyledText::new(vec![TextSpan::text(*text, SpanStyle::default())]),
                    placement: CuePlacement::default(),
                };
                let start = SubtitleTime::from_millis(index as i64);
                let end = SubtitleTime::from_millis(index as i64 + 1);
                let event = SubtitleEvent::new(
                    start,
                    end,
                    SubtitleLayer::DEFAULT,
                    SubtitleContent::Text(cue.clone()),
                )
                .expect("интервал");
                let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
                    panic!("событие должно вставиться");
                };
                (id, cue)
            })
            .collect();
        let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
            max_cached_chars: 1024,
        })
        .expect("встроенные шрифты");
        let rasterizer = SubtitleRasterizer::new(
            fonts,
            RasterLimits {
                cache: RasterCacheLimits {
                    cue_bitmaps_bytes: 32 * 1024 * 1024,
                    shaper_faces: 8,
                    blur_scratch_retained_bytes: 32 * 1024 * 1024,
                },
                input: RasterInputLimits {
                    max_chars_per_cue: 1_000,
                    max_cues_per_frame: 16,
                    max_ass_events_per_frame: 16,
                    max_canvas_side: 8_192,
                },
            },
        );
        Self { cues, rasterizer }
    }

    fn event(&self, index: usize) -> SubtitleEventId {
        self.cues[index].0
    }

    fn key(&self, index: usize) -> CueKey {
        CueKey::new(CueSourceId::new(SOURCE), self.event(index))
    }

    /// Рисует реплики с индексами `which` как одну готовую картинку со сроком `interval`.
    fn prepare(
        &mut self,
        which: &[usize],
        identity: OverlayIdentity,
        interval: ValidityInterval,
        contains_ass: bool,
    ) -> PreparedOverlay {
        let renderables: Vec<RenderableCue<'_>> = which
            .iter()
            .map(|&index| {
                RenderableCue::text(
                    CueKey::new(identity.source, self.cues[index].0),
                    &self.cues[index].1,
                )
            })
            .collect();
        let appearance = SubtitleAppearance::youtube_like();
        let overlay = self
            .rasterizer
            .render(&RenderRequest {
                moment: subtitle_core::SubtitleTime::ZERO,
                cues: &renderables,
                canvas: identity.canvas,
                appearance: &appearance,
                text_language: TextLanguagePreference::unknown(),
                ass: subtitle_raster::AssRenderOptions::default(),
            })
            .clone();
        let events = which.iter().map(|&index| self.cues[index].0).collect();
        PreparedOverlay::new(identity, interval, events, contains_ass, overlay)
    }
}

fn queue(capacity: usize) -> PreparedQueue {
    PreparedQueue::new(NonZeroUsize::new(capacity).expect("ёмкость"))
}

fn span(from_ms: i64, until_ms: Option<i64>) -> ValidityInterval {
    ValidityInterval::new(ms(from_ms), until_ms.map(ms))
}

/// Ключи владельцев картинок без повторов, по порядку.
fn owners(overlay: &SubtitleOverlay) -> Vec<CueKey> {
    let mut keys: Vec<CueKey> = Vec::new();
    for bitmap in overlay.bitmaps() {
        let key = bitmap.cue_key().expect("владелец");
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

#[test]
fn exact_selection_returns_the_overlay_covering_the_time() {
    let mut scene = Scene::new(&["Alpha", "Beta"]);
    let early = scene.prepare(&[0], base_identity(), span(0, Some(100)), false);
    let late = scene.prepare(&[1], base_identity(), span(100, Some(200)), false);
    let late_masks: Vec<_> = late
        .overlay()
        .bitmaps()
        .iter()
        .map(|bitmap| Arc::clone(bitmap.shared_coverage()))
        .collect();
    let mut queue = queue(4);
    queue.push(early);
    queue.push(late);

    let Selection::Exact(overlay) = queue.select(base_identity(), ms(150), &[scene.event(1)])
    else {
        panic!("в 150 мс нужна точная картинка");
    };
    assert_eq!(overlay.bitmaps().len(), late_masks.len());
    for (bitmap, mask) in overlay.bitmaps().iter().zip(&late_masks) {
        assert!(Arc::ptr_eq(bitmap.shared_coverage(), mask));
    }
    assert_eq!(queue.counters().exact_hits, 1);
    assert_eq!(queue.counters().stale_fallbacks, 0);
}

#[test]
fn interval_end_is_exclusive_in_selection() {
    let mut scene = Scene::new(&["Alpha"]);
    let prepared = scene.prepare(&[0], base_identity(), span(0, Some(100)), false);
    let mut queue = queue(4);
    queue.push(prepared);

    assert!(matches!(
        queue.select(base_identity(), ms(99), &[scene.event(0)]),
        Selection::Exact(_)
    ));
    // На 100 картинка уже не годна, а показанная того же источника даёт фоллбэк.
    assert!(matches!(
        queue.select(base_identity(), ms(100), &[scene.event(0)]),
        Selection::StaleWithoutEnded(_)
    ));
}

#[test]
fn miss_with_same_cues_shows_the_last_shown_overlay_with_shared_masks() {
    let mut scene = Scene::new(&["Alpha"]);
    let shown = scene.prepare(&[0], base_identity(), span(0, Some(100)), false);
    let shown_overlay = shown.overlay().clone();
    let mut queue = queue(4);
    queue.push(shown);
    assert!(matches!(
        queue.select(base_identity(), ms(10), &[scene.event(0)]),
        Selection::Exact(_)
    ));

    // Новая картинка ещё не готова; реплика та же.
    let Selection::StaleWithoutEnded(fallback) =
        queue.select(base_identity(), ms(150), &[scene.event(0)])
    else {
        panic!("ожидался фоллбэк");
    };
    assert_eq!(fallback.bitmaps().len(), shown_overlay.bitmaps().len());
    for (bitmap, original) in fallback.bitmaps().iter().zip(shown_overlay.bitmaps()) {
        assert!(Arc::ptr_eq(
            bitmap.shared_coverage(),
            original.shared_coverage()
        ));
    }
    assert_eq!(queue.counters().stale_fallbacks, 1);
}

#[test]
fn miss_with_ended_cue_drops_only_that_cue() {
    let mut scene = Scene::new(&["Alpha", "Beta"]);
    let shown = scene.prepare(&[0, 1], base_identity(), span(0, Some(100)), false);
    let mut queue = queue(4);
    queue.push(shown);
    assert!(matches!(
        queue.select(base_identity(), ms(10), &[scene.event(0), scene.event(1)]),
        Selection::Exact(_)
    ));

    let Selection::StaleWithoutEnded(fallback) =
        queue.select(base_identity(), ms(150), &[scene.event(1)])
    else {
        panic!("ожидался фоллбэк без закончившейся реплики");
    };
    assert_eq!(owners(&fallback), [scene.key(1)]);
}

#[test]
fn miss_when_every_cue_has_ended_shows_nothing() {
    let mut scene = Scene::new(&["Alpha"]);
    let shown = scene.prepare(&[0], base_identity(), span(0, Some(100)), false);
    let mut queue = queue(4);
    queue.push(shown);
    let _ = queue.select(base_identity(), ms(10), &[scene.event(0)]);

    assert!(matches!(
        queue.select(base_identity(), ms(150), &[]),
        Selection::Nothing
    ));
    assert_eq!(queue.counters().misses_without_picture, 1);
    assert!(!queue.shown_contains_ass());
}

#[test]
fn fallback_keeps_the_original_so_a_later_frame_can_show_more_cues() {
    let mut scene = Scene::new(&["Alpha", "Beta"]);
    let shown = scene.prepare(&[0, 1], base_identity(), span(0, Some(100)), false);
    let mut queue = queue(4);
    queue.push(shown);
    let _ = queue.select(base_identity(), ms(10), &[scene.event(0), scene.event(1)]);

    let Selection::StaleWithoutEnded(only_second) =
        queue.select(base_identity(), ms(150), &[scene.event(1)])
    else {
        panic!("фоллбэк");
    };
    assert_eq!(owners(&only_second), [scene.key(1)]);
    // Скраб назад: обе реплики снова активны, оригинал не испорчен прошлым фоллбэком.
    let Selection::StaleWithoutEnded(both) =
        queue.select(base_identity(), ms(160), &[scene.event(0), scene.event(1)])
    else {
        panic!("фоллбэк");
    };
    assert_eq!(owners(&both), [scene.key(0), scene.key(1)]);
}

#[test]
fn nothing_is_shown_before_any_picture_was_shown() {
    let mut queue = queue(4);
    assert!(matches!(
        queue.select(base_identity(), ms(10), &[]),
        Selection::Nothing
    ));
    assert_eq!(queue.counters().misses_without_picture, 1);
}

#[test]
fn fallback_never_crosses_source_ids() {
    let mut scene = Scene::new(&["Alpha"]);
    let shown = scene.prepare(&[0], base_identity(), span(0, Some(100)), false);
    let mut queue = queue(4);
    queue.push(shown);
    let _ = queue.select(base_identity(), ms(10), &[scene.event(0)]);

    // Дорожку сменили: другой источник, те же идентификаторы событий.
    let other_source = identity(SOURCE + 1, 1280, RenderInputsRevision::INITIAL);
    assert!(matches!(
        queue.select(other_source, ms(150), &[scene.event(0)]),
        Selection::Nothing
    ));
    // Старая показанная картинка забыта и не вернётся, даже если вернуть прежний источник.
    assert!(matches!(
        queue.select(base_identity(), ms(150), &[scene.event(0)]),
        Selection::Nothing
    ));
}

#[test]
fn other_revision_is_not_exact_but_old_shown_picture_bridges_the_gap() {
    let mut scene = Scene::new(&["Alpha"]);
    let old_inputs = base_identity();
    let new_inputs = identity(SOURCE, 1280, RenderInputsRevision::INITIAL.next());
    let old = scene.prepare(&[0], old_inputs, span(0, None), false);
    let mut queue = queue(4);
    queue.push(old);
    assert!(matches!(
        queue.select(old_inputs, ms(10), &[scene.event(0)]),
        Selection::Exact(_)
    ));

    // Стиль сменился: картинки старой ревизии не точные, пока нет новой.
    assert!(matches!(
        queue.select(new_inputs, ms(20), &[scene.event(0)]),
        Selection::StaleWithoutEnded(_)
    ));
    assert_eq!(
        queue.prepared_count(),
        0,
        "старая ревизия вытеснена из очереди"
    );

    let fresh = scene.prepare(&[0], new_inputs, span(0, None), false);
    queue.push(fresh);
    assert!(matches!(
        queue.select(new_inputs, ms(30), &[scene.event(0)]),
        Selection::Exact(_)
    ));
}

#[test]
fn other_canvas_is_not_exact_but_keeps_fallback() {
    let mut scene = Scene::new(&["Alpha"]);
    let small = identity(SOURCE, 1280, RenderInputsRevision::INITIAL);
    let large = identity(SOURCE, 1920, RenderInputsRevision::INITIAL);
    let picture = scene.prepare(&[0], small, span(0, None), false);
    let mut queue = queue(4);
    queue.push(picture);
    let _ = queue.select(small, ms(10), &[scene.event(0)]);

    let Selection::StaleWithoutEnded(fallback) = queue.select(large, ms(20), &[scene.event(0)])
    else {
        panic!("фоллбэк при смене холста");
    };
    assert_eq!(fallback.canvas(), canvas(1280));
}

#[test]
fn capacity_overflow_evicts_oldest_first() {
    let mut scene = Scene::new(&["Alpha", "Beta", "Gamma"]);
    let mut queue = queue(2);
    for (index, from) in [(0_usize, 0_i64), (1, 100), (2, 200)] {
        let prepared = scene.prepare(
            &[index],
            base_identity(),
            span(from, Some(from + 100)),
            false,
        );
        queue.push(prepared);
    }
    assert_eq!(queue.prepared_count(), 2);

    // Самая старая (0..100) вытеснена: на 50 мс точной картинки нет.
    assert!(!matches!(
        queue.select(base_identity(), ms(50), &[scene.event(0)]),
        Selection::Exact(_)
    ));
}

#[test]
fn entries_whose_time_has_passed_are_evicted() {
    let mut scene = Scene::new(&["Alpha", "Beta"]);
    let mut queue = queue(4);
    queue.push(scene.prepare(&[0], base_identity(), span(0, Some(100)), false));
    queue.push(scene.prepare(&[1], base_identity(), span(100, Some(200)), false));
    let _ = queue.select(base_identity(), ms(150), &[scene.event(1)]);
    assert_eq!(queue.prepared_count(), 1, "картинка 0..100 осталась позади");
}

#[test]
fn newest_rendered_picture_wins_among_overlapping_ones() {
    let mut scene = Scene::new(&["Alpha", "Beta"]);
    let broad = scene.prepare(&[0], base_identity(), span(0, None), false);
    let narrow = scene.prepare(&[1], base_identity(), span(50, None), false);
    let mut queue = queue(4);
    queue.push(broad);
    queue.push(narrow);

    let Selection::Exact(overlay) = queue.select(base_identity(), ms(60), &[scene.event(1)]) else {
        panic!("точная");
    };
    assert_eq!(owners(overlay), [scene.key(1)]);
}

#[test]
fn shown_contains_ass_follows_the_selected_picture() {
    let mut scene = Scene::new(&["Alpha", "Beta"]);
    let mut queue = queue(4);
    queue.push(scene.prepare(&[0], base_identity(), span(0, Some(100)), true));
    queue.push(scene.prepare(&[1], base_identity(), span(100, Some(200)), false));

    let _ = queue.select(base_identity(), ms(10), &[scene.event(0)]);
    assert!(queue.shown_contains_ass());
    let _ = queue.select(base_identity(), ms(150), &[scene.event(1)]);
    assert!(!queue.shown_contains_ass());
}

#[test]
fn prepared_overlay_remembers_its_rendered_events() {
    let mut scene = Scene::new(&["Alpha", "Beta"]);
    let prepared = scene.prepare(&[0, 1], base_identity(), span(0, None), false);
    assert_eq!(prepared.rendered_events(), [scene.event(0), scene.event(1)]);
    assert_eq!(prepared.interval(), span(0, None));
}

#[test]
fn clear_forgets_queue_and_shown_picture() {
    let mut scene = Scene::new(&["Alpha"]);
    let mut queue = queue(4);
    queue.push(scene.prepare(&[0], base_identity(), span(0, None), false));
    let _ = queue.select(base_identity(), ms(10), &[scene.event(0)]);
    queue.clear();
    assert_eq!(queue.prepared_count(), 0);
    assert!(matches!(
        queue.select(base_identity(), ms(10), &[scene.event(0)]),
        Selection::Nothing
    ));
}

#[test]
fn backward_seek_keeps_the_target_picture_when_the_displayed_frame_is_far_ahead() {
    let mut scene = Scene::new(&["Alpha"]);
    let target_picture = scene.prepare(&[0], base_identity(), span(500, Some(1_000)), false);
    let mut queue = queue(4);
    queue.push(target_picture);

    // Идёт seek назад к 500 мс, а на экране ещё кадр из 6000 мс: время «ушло» считается от
    // цели, поэтому свежая картинка цели не выбрасывается.
    queue.note_planning_anchor(ms(500));
    let during_seek = queue.select(base_identity(), ms(6_000), &[]);
    assert!(!matches!(during_seek, Selection::Exact(_)));
    assert_eq!(
        queue.prepared_count(),
        1,
        "the target picture must survive the seek"
    );

    // Первый кадр после seek попадает в неё точно.
    queue.note_planning_anchor(ms(540));
    assert!(matches!(
        queue.select(base_identity(), ms(540), &[scene.event(0)]),
        Selection::Exact(_)
    ));
}

#[test]
fn without_an_anchor_the_lookup_time_decides_what_is_over() {
    let mut scene = Scene::new(&["Alpha"]);
    let picture = scene.prepare(&[0], base_identity(), span(500, Some(1_000)), false);
    let mut queue = queue(4);
    queue.push(picture);

    let _ = queue.select(base_identity(), ms(6_000), &[]);

    assert_eq!(
        queue.prepared_count(),
        0,
        "legacy behaviour: expired at the lookup time"
    );
}
