//! Посадка перемотки глазами полки: на какой сегмент она садится и что считается данными
//! страницы. Механика подачи — в `shelf_tests`, сквозь настоящий декодер —
//! `tests/b_frame_chunks_to_frame.rs`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use super::{IncomingMedia, LaneShelf, PlayedSoFar, ShelfEntry, ShelfRemoval, ShelfRules};
use crate::buffer_window::BufferWindow;
use crate::container::PushedContainer;
use crate::feed::EpisodeKey;
use crate::framing::{
    DecodeTiming, MediaTime, MediaTimeSpan, SegmentRandomAccess, TimedSpan, TrackSpans,
};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::page_removal::{PageRemovedRange, PageRemovedRangeEnd};
use crate::time::TimestampOffset;

/// Длительность кадра, мс.
const FRAME_MS: i64 = 40;

fn rules() -> ShelfRules {
    ShelfRules::new(
        BufferWindow {
            ahead: Duration::from_secs(120),
        },
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
}

fn at(ms: i64) -> MediaTime {
    MediaTime::from_micros(ms * 1_000)
}

/// Полка однокадровых сегментов.
struct Shelf {
    shelf: LaneShelf,
    ledger: Arc<IntakeLedger>,
    key: EpisodeKey,
}

impl Shelf {
    fn new() -> Self {
        let ledger = Arc::new(IntakeLedger::new(1 << 20));
        let init = ChargedSegment::charge(&ledger, Bytes::from_static(b"init")).expect("room");
        Self {
            shelf: LaneShelf::default(),
            key: EpisodeKey::new(
                PushedContainer::FragmentedMp4,
                Some(init),
                TimestampOffset::ZERO,
            ),
            ledger,
        }
    }

    fn put(&mut self, start_ms: i64, random_access: SegmentRandomAccess) {
        let span = MediaTimeSpan::new(at(start_ms), at(start_ms + FRAME_MS)).expect("span");
        let timing = TimedSpan::exact(span);
        let media = ChargedSegment::charge(&self.ledger, Bytes::from(vec![0; 8])).expect("room");
        self.shelf.insert(
            ShelfEntry::waiting(
                timing,
                TrackSpans::default(),
                DecodeTiming::follows_presentation(timing, TrackSpans::default()),
                media,
                self.key.clone(),
            )
            .with_random_access(random_access),
        );
    }

    /// Группа кадров от `start_ms`: ключевой и `dependent` зависимых.
    fn put_group(&mut self, start_ms: i64, dependent: i64) {
        self.put(start_ms, SegmentRandomAccess::LatestPoint(at(start_ms)));
        for frame in 1..=dependent {
            self.put(start_ms + frame * FRAME_MS, SegmentRandomAccess::NoPoints);
        }
    }

    /// Страница стёрла `[start_ms, end_ms)`, а плеер этого времени ещё не сыграл.
    fn erase_unplayed(&mut self, start_ms: u64, end_ms: u64) {
        let range = PageRemovedRange::new(
            Duration::from_millis(start_ms),
            PageRemovedRangeEnd::At(Duration::from_millis(end_ms)),
        );
        self.shelf.release_removed_by_page(
            ShelfRemoval::from_page(range),
            PlayedSoFar::Nothing,
            &rules(),
        );
    }

    /// Начало сегмента, с которого перемотка начнёт подачу (`None` — цели на полке нет).
    fn landing_ms(&mut self, target_ms: i64) -> Option<i64> {
        let choice =
            self.shelf
                .prepare_restart(at(target_ms), &rules(), IncomingMedia::MayArrive)?;
        let fed = self
            .shelf
            .take_for_feeding(choice, &rules())
            .expect("the chosen segment is fed");
        Some(fed.span.start().as_micros() / 1_000)
    }
}

#[test]
fn a_target_inside_a_group_of_pictures_lands_on_its_key_frame() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 7);
    shelf.put_group(320, 7);
    // Цель — четвёртый зависимый кадр второй группы; подача с него не дала бы ни одного кадра.
    assert_eq!(shelf.landing_ms(500), Some(320));
}

#[test]
fn a_target_on_the_key_frame_lands_right_there() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 7);
    shelf.put_group(320, 7);
    assert_eq!(shelf.landing_ms(330), Some(320));
}

#[test]
fn a_hole_before_the_target_keeps_the_landing_at_the_target() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 1);
    // Ключевой кадр этой группы не приходил: за дыру отступать нельзя.
    shelf.put(1_000, SegmentRandomAccess::NoPoints);
    shelf.put(1_040, SegmentRandomAccess::NoPoints);
    assert_eq!(shelf.landing_ms(1_050), Some(1_040));
}

#[test]
fn an_erased_key_frame_is_no_landing() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 7);
    // MSE вместе с ключевым кадром стирает и всю его группу.
    shelf.erase_unplayed(0, 1);
    assert!(
        shelf.landing_ms(10).is_none(),
        "the erased frame is not a landing for a seek"
    );
}

#[test]
fn without_recorded_points_the_landing_stays_on_the_target_segment() {
    let mut shelf = Shelf::new();
    shelf.put(0, SegmentRandomAccess::Unrecorded);
    shelf.put(40, SegmentRandomAccess::Unrecorded);
    assert_eq!(shelf.landing_ms(50), Some(40));
}

#[test]
fn what_the_page_erased_is_not_where_its_data_starts() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 1);
    shelf.put_group(10_000, 1);
    assert_eq!(shelf.shelf.earliest_held_start(), Some(at(0)));
    shelf.erase_unplayed(0, 1_000);
    assert_eq!(
        shelf.shelf.earliest_held_start(),
        Some(at(10_000)),
        "the erased early range stays for the player, but the page data starts later"
    );
}

#[test]
fn a_shelf_holding_only_erased_time_has_no_start_and_no_run() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 3);
    shelf.erase_unplayed(0, 1_000);
    assert_eq!(shelf.shelf.earliest_held_start(), None);
    assert_eq!(shelf.shelf.held_run(at(40), &rules()), None);
    // Плеер это время ещё доигрывает: для него кусок на полке остаётся.
    assert!(shelf.shelf.covered_run(at(40), &rules()).is_some());
}
