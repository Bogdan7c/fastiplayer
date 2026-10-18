//! Тесты хранилища композиций: интервалы, seek, повторное чтение, политика памяти.

use std::num::NonZeroU32;
use std::sync::Arc;

use subtitle_core::{
    IndexedPicture, PictureEncoding, PictureForcedFlag, PicturePalette, PicturePosition,
    PictureReferenceSize, SubtitleContent, SubtitlePictureCue, SubtitleTime,
};

use super::*;
use crate::decoder::PgsComposition;

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero")
}

fn picture(rle_len: usize) -> Arc<IndexedPicture> {
    Arc::new(IndexedPicture::new(
        nz(2),
        nz(2),
        PictureEncoding::HdmvRle(Arc::from(vec![1u8; rle_len])),
    ))
}

fn cue_of(picture: &Arc<IndexedPicture>, palette: &Arc<PicturePalette>) -> SubtitlePictureCue {
    SubtitlePictureCue::new(
        PictureReferenceSize::new(nz(100), nz(100)),
        PicturePosition { x: 1, y: 2 },
        picture.full_rect(),
        picture.clone(),
        palette.clone(),
        PictureForcedFlag::Regular,
    )
    .expect("valid cue")
}

fn at(millis: i64) -> SubtitleTime {
    SubtitleTime::from_millis(millis)
}

fn shown(millis: i64, rle_len: usize) -> PgsComposition {
    let pic = picture(rle_len);
    let palette = Arc::new(PicturePalette::transparent());
    PgsComposition {
        start: at(millis),
        cues: vec![cue_of(&pic, &palette)],
    }
}

fn clear(millis: i64) -> PgsComposition {
    PgsComposition {
        start: at(millis),
        cues: Vec::new(),
    }
}

fn spans(store: &PgsCompositionStore) -> Vec<(i64, i64)> {
    store
        .events()
        .iter()
        .map(|event| {
            (
                event.start().as_micros() / 1_000,
                event.end().as_micros() / 1_000,
            )
        })
        .collect()
}

const OPEN_MS: i64 = PgsCompositionStore::OPEN_END.as_micros() / 1_000;

#[test]
fn end_is_the_start_of_the_contiguous_successor_and_last_one_is_open() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(500, OPEN_MS)]);
    store.insert(shown(2_000, 4), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(500, 2_000), (2_000, OPEN_MS)]);
}

#[test]
fn clear_composition_ends_previous_and_produces_no_event() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(500, 1_500)]);
    assert_eq!(store.len(), 2);
}

#[test]
fn explicit_end_applies_but_earlier_successor_wins() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::ExplicitAt(at(900)));
    assert_eq!(spans(&store), [(500, 900)]);
    store.insert(shown(700, 4), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(500, 700), (700, OPEN_MS)]);
    // Явный конец не позже начала игнорируется (композиция остаётся открытой).
    store.insert(shown(5_000, 4), CompositionEnd::ExplicitAt(at(5_000)));
    assert_eq!(spans(&store).last(), Some(&(5_000, OPEN_MS)));
}

#[test]
fn layers_follow_object_order_inside_a_composition() {
    let palette = Arc::new(PicturePalette::transparent());
    let first = picture(3);
    let second = picture(5);
    let mut store = PgsCompositionStore::new();
    store.insert(
        PgsComposition {
            start: at(0),
            cues: vec![cue_of(&first, &palette), cue_of(&second, &palette)],
        },
        CompositionEnd::UntilNextDisplaySet,
    );
    let events = store.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].layer().get(), 0);
    assert_eq!(events[1].layer().get(), 1);
    assert!(matches!(events[0].content(), SubtitleContent::Picture(_)));
}

#[test]
fn discontinuity_drops_open_composition_but_keeps_closed_ones() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet);
    store.insert(shown(3_000, 4), CompositionEnd::UntilNextDisplaySet);
    let before = store.revision();
    store.note_discontinuity();
    // 500..1500 закрыта «очисткой», «очистка» закрыта показом на 3 с; открыта (и выброшена)
    // только последняя композиция 3000.
    assert_eq!(spans(&store), [(500, 1_500)]);
    assert_eq!(store.len(), 2);
    assert_eq!(store.statistics().open_dropped_on_discontinuity, 1);
    assert!(store.revision() > before);
}

#[test]
fn composition_after_discontinuity_never_closes_one_read_before_it() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(1_000), CompositionEnd::UntilNextDisplaySet);
    store.insert(shown(2_000, 4), CompositionEnd::UntilNextDisplaySet);
    store.note_discontinuity();
    // Seek вперёд: следующая прочитанная композиция начинается на 9 с. 2000 уже выброшена,
    // а закрытая 500..1000 остаётся нетронутой.
    store.insert(shown(9_000, 4), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(500, 1_000), (9_000, OPEN_MS)]);
    store.note_discontinuity();
    // Новая композиция после очередного разрыва не закрывает открытую из прошлого прогона.
    store.insert(shown(12_000, 4), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(500, 1_000), (12_000, OPEN_MS)]);
}

#[test]
fn rereading_identical_composition_does_not_duplicate_or_bump_revision() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet);
    let revision = store.revision();
    let bytes = store.estimated_bytes();
    // Seek назад: те же композиции читаются заново, но уже другими Arc.
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(store.len(), 2);
    assert_eq!(spans(&store), [(500, 1_500)]);
    assert_eq!(store.revision(), revision);
    assert_eq!(store.estimated_bytes(), bytes);
    assert_eq!(store.statistics().duplicate_compositions_ignored, 2);
}

#[test]
fn different_content_at_same_start_replaces_and_bumps_revision() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    let revision = store.revision();
    store.insert(shown(500, 40), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(store.len(), 1);
    assert!(store.revision() > revision);
    assert_eq!(
        store.estimated_bytes(),
        40 + PicturePalette::BYTE_LEN + COMPOSITION_OVERHEAD_BYTES + CUE_OVERHEAD_BYTES
    );
}

#[test]
fn reading_between_known_compositions_clamps_the_earlier_end() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(0, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(10_000), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(0, 10_000)]);
    // Поздно прочитанный набор внутри интервала побеждает.
    store.insert(clear(4_000), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(0, 4_000)]);
}

#[test]
fn background_retention_keeps_trailing_window_and_open_composition() {
    let mut store = PgsCompositionStore::new();
    for start in [0, 10_000, 20_000, 30_000] {
        store.insert(shown(start, 4), CompositionEnd::UntilNextDisplaySet);
        store.insert(clear(start + 2_000), CompositionEnd::UntilNextDisplaySet);
    }
    store.insert(shown(40_000, 4), CompositionEnd::UntilNextDisplaySet);
    let revision = store.revision();
    // Позиция 35 с, окно 10 с -> порог 25 с: удаляются все, что закончились до 25 с.
    let outcome = store.enforce_retention(
        at(35_000),
        PgsRetention::Background {
            trailing_window: at(10_000),
        },
    );
    assert_eq!(spans(&store), [(30_000, 32_000), (40_000, OPEN_MS)]);
    // Удалены shown 0/10/20 и clear 2/12; clear 22 с длится до 30 с (> порога) и остаётся.
    assert_eq!(outcome.evicted_compositions, 5);
    assert_eq!(store.statistics().evicted_by_trailing_window, 5);
    assert!(store.revision() > revision);
    // Повторный вызов ничего не меняет.
    let revision = store.revision();
    let again = store.enforce_retention(
        at(35_000),
        PgsRetention::Background {
            trailing_window: at(10_000),
        },
    );
    assert_eq!(again.evicted_compositions, 0);
    assert_eq!(store.revision(), revision);
}

#[test]
fn shown_budget_evicts_farthest_first_and_never_the_active_one() {
    let mut store = PgsCompositionStore::new();
    // Пять композиций по ~1 КБ RLE, каждая с собственной палитрой, интервалы по 1 с.
    for start in [0, 1_000, 2_000, 3_000, 4_000] {
        store.insert(shown(start, 1_000), CompositionEnd::UntilNextDisplaySet);
    }
    store.insert(clear(5_000), CompositionEnd::UntilNextDisplaySet);
    let one = 1_000 + PicturePalette::BYTE_LEN + COMPOSITION_OVERHEAD_BYTES + CUE_OVERHEAD_BYTES;
    let clear_cost = COMPOSITION_OVERHEAD_BYTES;
    assert_eq!(store.estimated_bytes(), 5 * one + clear_cost);

    // Позиция 2.5 с (активна композиция 2000); бюджет на три композиции + clear.
    let outcome = store.enforce_retention(
        at(2_500),
        PgsRetention::Shown {
            budget_bytes: 3 * one + clear_cost,
        },
    );
    // Порядок вытеснения: открытая «очистка» на 5 с (дальше всех, 2.5 с), затем 4000 и 0
    // (оба в 1.5 с от позиции; ничья решается более поздним началом).
    assert_eq!(outcome.evicted_compositions, 3);
    assert_eq!(
        spans(&store),
        [(1_000, 2_000), (2_000, 3_000), (3_000, 4_000)]
    );
    assert_eq!(store.statistics().evicted_by_budget, 3);

    // Бюджет 0: уходит всё, кроме активной композиции.
    let outcome = store.enforce_retention(at(2_500), PgsRetention::Shown { budget_bytes: 0 });
    assert_eq!(outcome.evicted_compositions, 2);
    assert_eq!(spans(&store), [(2_000, 3_000)]);
}

#[test]
fn shared_picture_and_palette_are_counted_once() {
    let shared_picture = picture(500);
    let shared_palette = Arc::new(PicturePalette::transparent());
    let mut store = PgsCompositionStore::new();
    for start in [0, 1_000, 2_000] {
        store.insert(
            PgsComposition {
                start: at(start),
                cues: vec![cue_of(&shared_picture, &shared_palette)],
            },
            CompositionEnd::UntilNextDisplaySet,
        );
    }
    let overhead = 3 * (COMPOSITION_OVERHEAD_BYTES + CUE_OVERHEAD_BYTES);
    assert_eq!(
        store.estimated_bytes(),
        500 + PicturePalette::BYTE_LEN + overhead
    );
    // Вытеснение двух из трёх не освобождает общую картинку: она нужна оставшейся.
    store.enforce_retention(at(2_500), PgsRetention::Shown { budget_bytes: 0 });
    assert_eq!(store.len(), 1);
    assert_eq!(
        store.estimated_bytes(),
        500 + PicturePalette::BYTE_LEN + COMPOSITION_OVERHEAD_BYTES + CUE_OVERHEAD_BYTES
    );
}

#[test]
fn revision_grows_only_on_changes() {
    let mut store = PgsCompositionStore::new();
    assert_eq!(store.revision(), 0);
    store.insert(shown(0, 4), CompositionEnd::UntilNextDisplaySet);
    let after_insert = store.revision();
    assert!(after_insert > 0);
    store.note_discontinuity();
    // Открытая выброшена -> изменение.
    assert!(store.revision() > after_insert);
    let after_drop = store.revision();
    store.note_discontinuity();
    assert_eq!(store.revision(), after_drop);
    let outcome = store.enforce_retention(
        at(0),
        PgsRetention::Background {
            trailing_window: at(1_000),
        },
    );
    assert_eq!(outcome.evicted_compositions, 0);
    assert_eq!(store.revision(), after_drop);
}

// ---- Инкрементальные итоги (`StoreChange`) ----

fn tail_append(change: StoreChange) -> TailAppend {
    match change {
        StoreChange::AppendedAtTail(append) => append,
        other => panic!("expected AppendedAtTail, got {other:?}"),
    }
}

fn event_spans(events: &[subtitle_core::SubtitleEvent]) -> Vec<(i64, i64)> {
    events
        .iter()
        .map(|event| {
            (
                event.start().as_micros() / 1_000,
                event.end().as_micros() / 1_000,
            )
        })
        .collect()
}

#[test]
fn first_composition_is_a_tail_append_with_open_end_and_no_predecessor() {
    let mut store = PgsCompositionStore::new();
    let append = tail_append(store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet));
    assert_eq!(event_spans(&append.events), [(500, OPEN_MS)]);
    assert_eq!(append.predecessor_closed, None);
}

#[test]
fn successor_reports_the_predecessor_closed_at_its_start() {
    let mut store = PgsCompositionStore::new();
    let first = tail_append(store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet));
    let second = tail_append(store.insert(shown(2_000, 4), CompositionEnd::UntilNextDisplaySet));
    assert_eq!(
        second.predecessor_closed,
        Some(PredecessorClosed {
            composition: first.composition,
            new_end: at(2_000)
        })
    );
    assert_eq!(event_spans(&second.events), [(2_000, OPEN_MS)]);
    assert_ne!(first.composition, second.composition);
}

#[test]
fn clear_composition_is_a_tail_append_without_events_that_closes_the_picture() {
    let mut store = PgsCompositionStore::new();
    let picture_append =
        tail_append(store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet));
    let clear_append = tail_append(store.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet));
    assert!(clear_append.events.is_empty());
    assert_eq!(
        clear_append.predecessor_closed,
        Some(PredecessorClosed {
            composition: picture_append.composition,
            new_end: at(1_500)
        })
    );
    // Картинку, показ которой закрыла «очистка», больше ничто не закрывает: у «очистки» события нет.
    let after_clear =
        tail_append(store.insert(shown(3_000, 4), CompositionEnd::UntilNextDisplaySet));
    assert_eq!(
        after_clear
            .predecessor_closed
            .map(|closed| closed.composition),
        Some(clear_append.composition)
    );
}

#[test]
fn explicit_end_before_the_successor_is_not_reported_as_a_change() {
    let mut store = PgsCompositionStore::new();
    let first = tail_append(store.insert(shown(500, 4), CompositionEnd::ExplicitAt(at(900))));
    assert_eq!(event_spans(&first.events), [(500, 900)]);
    // Преемник после явного конца: показ предшественника не меняется.
    let later = tail_append(store.insert(shown(1_500, 4), CompositionEnd::UntilNextDisplaySet));
    assert_eq!(later.predecessor_closed, None);
    // Преемник раньше явного конца укорачивает его.
    let mut other = PgsCompositionStore::new();
    let long = tail_append(other.insert(shown(500, 4), CompositionEnd::ExplicitAt(at(900))));
    let early = tail_append(other.insert(shown(700, 4), CompositionEnd::UntilNextDisplaySet));
    assert_eq!(
        early.predecessor_closed,
        Some(PredecessorClosed {
            composition: long.composition,
            new_end: at(700)
        })
    );
}

#[test]
fn rereading_a_known_composition_reports_duplicate() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(
        store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet),
        StoreChange::Duplicate
    );
    assert_eq!(
        store.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet),
        StoreChange::Duplicate
    );
}

#[test]
fn insert_between_stored_compositions_requires_rebuild() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(0, 4), CompositionEnd::UntilNextDisplaySet);
    store.insert(clear(10_000), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(
        store.insert(clear(4_000), CompositionEnd::UntilNextDisplaySet),
        StoreChange::RequiresRebuild
    );
    // Состояние хранилища при этом корректно: пересборка даст актуальную картину.
    assert_eq!(spans(&store), [(0, 4_000)]);
}

#[test]
fn replacing_content_at_the_same_start_requires_rebuild_and_issues_a_new_handle() {
    let mut store = PgsCompositionStore::new();
    let original = tail_append(store.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet));
    assert_eq!(
        store.insert(shown(500, 40), CompositionEnd::UntilNextDisplaySet),
        StoreChange::RequiresRebuild
    );
    let handles: Vec<CompositionHandle> = store
        .events_by_composition()
        .into_iter()
        .map(|(handle, _)| handle)
        .collect();
    assert_eq!(handles.len(), 1);
    assert_ne!(handles[0], original.composition);
}

#[test]
fn handles_are_monotonic_and_never_reused_after_eviction() {
    let mut store = PgsCompositionStore::new();
    let first = tail_append(store.insert(shown(0, 4), CompositionEnd::UntilNextDisplaySet));
    store.insert(clear(1_000), CompositionEnd::UntilNextDisplaySet);
    let outcome = store.enforce_retention(at(100_000), PgsRetention::Shown { budget_bytes: 0 });
    assert!(outcome.requires_rebuild());
    assert_eq!(store.len(), 1); // осталась только активная «очистка»
    let next = tail_append(store.insert(shown(200_000, 4), CompositionEnd::UntilNextDisplaySet));
    assert!(next.composition > first.composition);
    assert!(next.composition.sequence() >= 2);
}

#[test]
fn retention_without_eviction_does_not_require_rebuild() {
    let mut store = PgsCompositionStore::new();
    store.insert(shown(0, 4), CompositionEnd::UntilNextDisplaySet);
    let outcome = store.enforce_retention(
        at(0),
        PgsRetention::Background {
            trailing_window: at(1_000),
        },
    );
    assert!(!outcome.requires_rebuild());
}

#[test]
fn discontinuity_requires_rebuild_only_when_an_open_picture_was_dropped() {
    // Ничего не открыто.
    let mut empty = PgsCompositionStore::new();
    assert_eq!(empty.note_discontinuity(), DiscontinuityOutcome::default());
    assert!(!DiscontinuityOutcome::default().requires_rebuild());

    // Открыта картинка: её событие с далёким концом лежит на шкале потребителя.
    let mut with_picture = PgsCompositionStore::new();
    with_picture.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    let outcome = with_picture.note_discontinuity();
    assert_eq!(outcome.dropped_open_compositions, 1);
    assert_eq!(outcome.dropped_with_pictures, 1);
    assert!(outcome.requires_rebuild());

    // Открыта только «очистка»: событий у неё нет, шкала не затронута.
    let mut with_clear = PgsCompositionStore::new();
    with_clear.insert(shown(500, 4), CompositionEnd::UntilNextDisplaySet);
    with_clear.insert(clear(1_500), CompositionEnd::UntilNextDisplaySet);
    let outcome = with_clear.note_discontinuity();
    assert_eq!(outcome.dropped_open_compositions, 1);
    assert_eq!(outcome.dropped_with_pictures, 0);
    assert!(!outcome.requires_rebuild());
}

#[test]
fn events_by_composition_groups_events_and_keeps_clear_compositions_empty() {
    let palette = Arc::new(PicturePalette::transparent());
    let mut store = PgsCompositionStore::new();
    store.insert(
        PgsComposition {
            start: at(0),
            cues: vec![cue_of(&picture(3), &palette), cue_of(&picture(5), &palette)],
        },
        CompositionEnd::UntilNextDisplaySet,
    );
    store.insert(clear(1_000), CompositionEnd::UntilNextDisplaySet);
    let grouped = store.events_by_composition();
    assert_eq!(grouped.len(), 2);
    assert_eq!(event_spans(&grouped[0].1), [(0, 1_000), (0, 1_000)]);
    assert!(grouped[1].1.is_empty());
    assert_eq!(store.events().len(), 2);
}

/// Приложение строит шкалу только инкрементально (дописать + укоротить); результат обязан
/// совпасть с шкалой, пересобранной из хранилища, — то есть исходы описывают изменение полно.
#[test]
fn applying_changes_incrementally_reproduces_the_rebuilt_timeline() {
    use std::collections::HashMap;
    use subtitle_core::{SubtitleEventId, SubtitleTimeline, TimelineInsertOutcome};

    let mut store = PgsCompositionStore::new();
    let mut timeline = SubtitleTimeline::new();
    let mut ids_by_composition: HashMap<CompositionHandle, Vec<SubtitleEventId>> = HashMap::new();
    let sequence = [
        (shown(500, 4), CompositionEnd::UntilNextDisplaySet),
        (clear(1_500), CompositionEnd::UntilNextDisplaySet),
        (shown(2_000, 5), CompositionEnd::ExplicitAt(at(9_000))),
        (shown(3_000, 6), CompositionEnd::UntilNextDisplaySet),
        (shown(7_000, 7), CompositionEnd::UntilNextDisplaySet),
    ];
    for (composition, end) in sequence {
        let append = tail_append(store.insert(composition, end));
        if let Some(closed) = append.predecessor_closed {
            for id in ids_by_composition
                .get(&closed.composition)
                .into_iter()
                .flatten()
            {
                timeline
                    .shorten_event_end(*id, closed.new_end)
                    .expect("shortening an inserted event");
            }
        }
        let mut ids = Vec::new();
        for event in append.events {
            match timeline.insert(event) {
                TimelineInsertOutcome::Inserted(id) => ids.push(id),
                other => panic!("unexpected insert outcome {other:?}"),
            }
        }
        ids_by_composition.insert(append.composition, ids);
    }
    let (rebuilt, _) = SubtitleTimeline::from_events(store.events());
    let incremental: Vec<_> = timeline.iter().map(|(_, e)| e.clone()).collect();
    let expected: Vec<_> = rebuilt.iter().map(|(_, e)| e.clone()).collect();
    assert_eq!(incremental, expected);
    // Картинка 500 закрыта «очисткой» 1500, а не осталась открытой.
    assert_eq!(event_spans(&incremental)[0], (500, 1_500));
}

#[test]
fn evicting_the_end_boundary_of_an_open_predecessor_keeps_its_display_end() {
    let mut store = PgsCompositionStore::new();
    // Прогон 1: закрытая композиция 8-9 с (явный конец).
    store.insert(shown(8_000, 100), CompositionEnd::ExplicitAt(at(9_000)));
    store.note_discontinuity();
    // Прогон 2: открытая композиция в 5 с; её показ заканчивается на начале следующей (8 с).
    store.insert(shown(5_000, 100), CompositionEnd::UntilNextDisplaySet);
    assert_eq!(spans(&store), [(5_000, 8_000), (8_000, 9_000)]);

    // Позиция 1 с: обе далеко, дальше всех 8-9 с. Бюджет на одну композицию.
    let one = 100 + PicturePalette::BYTE_LEN + COMPOSITION_OVERHEAD_BYTES + CUE_OVERHEAD_BYTES;
    let outcome = store.enforce_retention(at(1_000), PgsRetention::Shown { budget_bytes: one });
    assert_eq!(outcome.evicted_compositions, 1);
    // Раньше тут получалось (5000, OPEN_MS): композиция растягивалась на всё будущее.
    assert_eq!(spans(&store), [(5_000, 8_000)]);
}

#[test]
fn budget_eviction_of_many_compositions_removes_farthest_first() {
    let mut store = PgsCompositionStore::new();
    let count = 2_000;
    for index in 0..count {
        store.insert(
            shown(index * 1_000, 10),
            CompositionEnd::UntilNextDisplaySet,
        );
    }
    let one = 10 + PicturePalette::BYTE_LEN + COMPOSITION_OVERHEAD_BYTES + CUE_OVERHEAD_BYTES;
    // Позиция 1000.5 с (активна композиция 1000 с); оставляем бюджет на 11 композиций.
    let outcome = store.enforce_retention(
        at(1_000_500),
        PgsRetention::Shown {
            budget_bytes: 11 * one,
        },
    );
    assert_eq!(outcome.evicted_compositions, (count - 11) as usize);
    let remaining: Vec<i64> = spans(&store)
        .iter()
        .map(|(start, _)| start / 1_000)
        .collect();
    // Остаются самые близкие к позиции (с учётом ничьей в пользу более ранней).
    assert_eq!(
        remaining,
        [
            995, 996, 997, 998, 999, 1_000, 1_001, 1_002, 1_003, 1_004, 1_005
        ]
    );
}
