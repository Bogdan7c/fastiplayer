//! Стирание страницей глазами полки: что считает стёртым сам MSE и что полка держит ради
//! плеера. Общие правила отпускания (сыграно, труба) — в `shelf_tests`.

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
use crate::page_removal::{PageRemovalReport, PageRemovedRange, PageRemovedRangeEnd};
use crate::time::TimestampOffset;

/// Миллисекунда в микросекундах.
const MS: i64 = 1_000;

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
    MediaTime::from_micros(ms * MS)
}

/// Кадр с точкой доступа в своём начале.
fn point_at(ms: i64) -> SegmentRandomAccess {
    SegmentRandomAccess::LatestPoint(at(ms))
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

    fn put(&mut self, start_ms: i64, end_ms: i64, random_access: SegmentRandomAccess) {
        let span = MediaTimeSpan::new(at(start_ms), at(end_ms)).expect("span");
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

    /// Группа кадров по 40 мс от `start_ms`: ключевой и `dependent` зависимых.
    fn put_group(&mut self, start_ms: i64, dependent: i64) {
        self.put(start_ms, start_ms + 40, point_at(start_ms));
        for frame in 1..=dependent {
            let frame_start = start_ms + frame * 40;
            self.put(frame_start, frame_start + 40, SegmentRandomAccess::NoPoints);
        }
    }

    fn remove(&mut self, start_ms: u64, end_ms: u64, played: PlayedSoFar) -> PageRemovalReport {
        let range = PageRemovedRange::new(
            Duration::from_millis(start_ms),
            PageRemovedRangeEnd::At(Duration::from_millis(end_ms)),
        );
        self.shelf
            .release_removed_by_page(ShelfRemoval::from_page(range), played, &rules())
            .report
    }

    fn starts_ms(&self) -> Vec<i64> {
        self.shelf
            .layout()
            .iter()
            .map(|(start, _, _)| start / MS)
            .collect()
    }
}

#[test]
fn a_removal_takes_the_rest_of_the_group_of_pictures_with_it() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 3);
    shelf.put_group(160, 3);
    // Страница назвала только первые два кадра; MSE стёр и их зависимых до ключевого 160.
    let report = shelf.remove(0, 60, PlayedSoFar::Until(at(1_000)));
    assert_eq!(report.released_segments, 4);
    assert_eq!(shelf.starts_ms(), [160, 200, 240, 280]);
}

#[test]
fn a_removal_starting_at_a_later_frame_leaves_what_comes_before_it() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 3);
    shelf.put_group(160, 1);
    let report = shelf.remove(60, 100, PlayedSoFar::Until(at(1_000)));
    assert_eq!(report.released_segments, 2);
    assert_eq!(shelf.starts_ms(), [0, 40, 160, 200]);
}

#[test]
fn a_random_access_point_on_the_end_of_the_range_stays_despite_rounding() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 3);
    shelf.put_group(160, 0);
    let range = PageRemovedRange::new(
        Duration::ZERO,
        // Секунды страницы при переводе разошлись с началом кадра на 1 мкс.
        PageRemovedRangeEnd::At(Duration::from_micros(160_001)),
    );
    let released = shelf.shelf.release_removed_by_page(
        ShelfRemoval::from_page(range),
        PlayedSoFar::Until(at(1_000)),
        &rules(),
    );
    assert_eq!(released.report.released_segments, 4);
    assert_eq!(shelf.starts_ms(), [160]);
}

#[test]
fn a_frame_that_starts_inside_the_range_is_erased_even_if_it_ends_past_it() {
    let mut shelf = Shelf::new();
    // Кадры звука: каждый — точка доступа. Граница страницы попала внутрь второго кадра.
    shelf.put(0, 23, point_at(0));
    shelf.put(23, 46, point_at(23));
    shelf.put(46, 69, point_at(46));
    let report = shelf.remove(0, 30, PlayedSoFar::Until(at(1_000)));
    assert_eq!(report.released_segments, 2);
    assert_eq!(shelf.starts_ms(), [46]);
}

#[test]
fn a_segment_with_a_random_access_point_past_the_range_keeps_the_rest_of_the_timeline() {
    let mut shelf = Shelf::new();
    // Целый фрагмент с точкой доступа в середине: хвост у страницы остался.
    shelf.put(0, 400, point_at(200));
    shelf.put(400, 440, SegmentRandomAccess::NoPoints);
    let report = shelf.remove(0, 100, PlayedSoFar::Until(at(1_000)));
    assert_eq!(
        (report.released_segments, report.kept_partially_removed),
        (0, 1)
    );
    assert_eq!(shelf.starts_ms(), [0, 400]);
}

#[test]
fn without_recorded_points_only_segments_inside_the_range_are_erased() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, SegmentRandomAccess::Unrecorded);
    shelf.put(200, 400, SegmentRandomAccess::Unrecorded);
    shelf.put(400, 600, SegmentRandomAccess::Unrecorded);
    let report = shelf.remove(0, 300, PlayedSoFar::Until(at(1_000)));
    assert_eq!(
        (report.released_segments, report.kept_partially_removed),
        (1, 1)
    );
    assert_eq!(shelf.starts_ms(), [200, 400]);
}

#[test]
fn erased_but_unplayed_stays_for_the_player_until_the_next_seek() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 3);
    shelf.put_group(160, 3);
    let held = shelf.ledger.held_bytes();
    // Плеер на 100 мс: всё после него ещё нужно ему, но страницы этого больше нет.
    let report = shelf.remove(0, 1_000, PlayedSoFar::Until(at(100)));
    assert_eq!((report.released_segments, report.kept_not_played), (2, 6));
    // Плеер доигрывает их с полки, но перемотка туда уже не сядет: снимет их первым шагом.
    assert_eq!(shelf.starts_ms(), [80, 120, 160, 200, 240, 280]);
    assert!(
        !shelf
            .shelf
            .holds(at(200), &rules(), IncomingMedia::MayArrive),
        "the seek question answers like the seek"
    );

    let erased = shelf.shelf.take_erased_by_page();

    assert_eq!(erased.len(), 6);
    drop(erased);
    assert_eq!(shelf.starts_ms(), Vec::<i64>::new());
    assert_eq!(
        shelf.ledger.held_bytes(),
        held - 8 * 8,
        "the bytes are free"
    );
}

#[test]
fn a_seek_keeps_what_the_page_still_holds() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 3);
    shelf.put_group(160, 3);
    let report = shelf.remove(200, 1_000, PlayedSoFar::Until(at(100)));
    assert_eq!(report.kept_not_played, 3);
    let erased = shelf.shelf.take_erased_by_page();
    assert_eq!(erased.len(), 3);
    assert_eq!(shelf.starts_ms(), [0, 40, 80, 120, 160]);
}

#[test]
fn when_the_player_has_left_erased_segments_are_released_even_from_an_abandoned_pipe() {
    let mut shelf = Shelf::new();
    shelf.put_group(0, 1);
    let choice = shelf.shelf.next_to_feed(None, &rules()).expect("first");
    let in_pipe = shelf.shelf.take_for_feeding(choice, &rules()).expect("fed");
    let report = shelf.remove(0, 1_000, PlayedSoFar::PlayerLeft);
    assert_eq!(
        (
            report.released_segments,
            report.kept_in_pipe,
            report.kept_not_played
        ),
        (2, 0, 0)
    );
    assert_eq!(shelf.starts_ms(), Vec::<i64>::new());
    drop(in_pipe);
}
