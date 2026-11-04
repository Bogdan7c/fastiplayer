//! Тесты окрестности замены: срез полки даёт тот же ответ, что перебор всей полки, —
//! в том числе для длинного сегмента далеко слева и для дорожки, выходящей за отрезок
//! показа, — и остаётся срезом, а не всей полкой.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::super::{LaneShelf, ShelfEntry, ShelfRules};
use crate::buffer_window::BufferWindow;
use crate::container::PushedContainer;
use crate::feed::EpisodeKey;
use crate::framing::{
    DecodeTiming, MediaTime, MediaTimeSpan, SegmentEnd, TimedSpan, TrackMedia, TrackSlotAssigner,
    TrackSpans,
};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::time::TimestampOffset;

/// Миллисекунда в микросекундах.
const MS: i64 = 1_000;

/// Правила по умолчанию: допуск 10 мс, дыра 100 мс, окно 120 с, открытый хвост 2 с.
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

fn span(start_ms: i64, end_ms: i64) -> MediaTimeSpan {
    MediaTimeSpan::new(
        MediaTime::from_micros(start_ms * MS),
        MediaTime::from_micros(end_ms * MS),
    )
    .expect("ordered span")
}

/// Видеодорожка с отрезком.
fn video(start_ms: i64, end_ms: i64) -> TrackSpans {
    let mut tracks = TrackSpans::default();
    tracks.include(
        TrackSlotAssigner::default().next(TrackMedia::Video),
        span(start_ms, end_ms),
    );
    tracks
}

/// Полка с учётом байтов.
struct Shelf {
    shelf: LaneShelf,
    ledger: Arc<IntakeLedger>,
    key: EpisodeKey,
}

impl Shelf {
    fn new() -> Self {
        Self {
            shelf: LaneShelf::default(),
            ledger: Arc::new(IntakeLedger::new(1 << 24)),
            key: EpisodeKey::new(PushedContainer::FragmentedMp4, None, TimestampOffset::ZERO),
        }
    }

    /// Кладёт сегмент; возвращает начала снятых им (мс).
    fn put(&mut self, timing: TimedSpan, tracks: TrackSpans) -> Vec<i64> {
        let replaced: Vec<i64> = self
            .shelf
            .take_overlapped_by(timing, &tracks, &rules())
            .iter()
            .map(|entry| entry.span().start().as_micros() / MS)
            .collect();
        let media =
            ChargedSegment::charge(&self.ledger, Bytes::from_static(&[0; 8])).expect("room");
        let decode = DecodeTiming::follows_presentation(timing, tracks.clone());
        self.shelf.insert(ShelfEntry::waiting(
            timing,
            tracks,
            decode,
            media,
            self.key.clone(),
        ));
        replaced
    }

    fn put_exact(&mut self, start_ms: i64, end_ms: i64) -> Vec<i64> {
        self.put(
            TimedSpan::exact(span(start_ms, end_ms)),
            TrackSpans::default(),
        )
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
fn a_long_segment_far_to_the_left_is_still_replaced() {
    let mut shelf = Shelf::new();
    // Длинный сегмент, затем много коротких далеко справа от его начала.
    shelf.put_exact(0, 10_000);
    for start in (12_000..13_000).step_by(40) {
        shelf.put_exact(start, start + 40);
    }
    // Короткий сегмент внутри длинного: по началу он далеко от него на полке, но
    // накрыт им — длинный снимается (повтор времени).
    assert_eq!(shelf.put_exact(9_000, 9_100), [0]);
    assert!(!shelf.starts_ms().contains(&0));
    assert_eq!(shelf.starts_ms()[0], 9_000);
}

#[test]
fn an_estimated_end_counts_only_up_to_the_certain_end() {
    let mut shelf = Shelf::new();
    // Конец оценён до 10 с, а наверняка известен до 1 с: сосед на 5 с — не повтор.
    shelf.put(
        TimedSpan::new(
            span(0, 10_000),
            SegmentEnd::Estimated {
                certain_end: MediaTime::from_micros(1_000 * MS),
            },
        ),
        TrackSpans::default(),
    );
    assert_eq!(shelf.put_exact(5_000, 5_200), Vec::<i64>::new());
    // А сегмент внутри точно известной части — повтор.
    assert_eq!(shelf.put_exact(500, 600), [0]);
}

#[test]
fn a_track_reaching_outside_the_presentation_span_still_overlaps() {
    let mut shelf = Shelf::new();
    // Отрезок показа 0–200 мс, а дорожка (точно известное время) тянется до 400 мс.
    shelf.put(TimedSpan::exact(span(0, 200)), video(0, 400));
    for start in (1_000..2_000).step_by(40) {
        shelf.put(
            TimedSpan::exact(span(start, start + 40)),
            video(start, start + 40),
        );
    }
    // Новый сегмент 300–380 мс пересекает дорожку, хотя отрезок показа кончился в 200.
    assert_eq!(
        shelf.put(TimedSpan::exact(span(300, 380)), video(300, 380)),
        [0]
    );
}

#[test]
fn the_neighbourhood_is_a_slice_of_the_shelf_not_the_whole_shelf() {
    let mut shelf = Shelf::new();
    for start in (0..40_000).step_by(40) {
        shelf.put_exact(start, start + 40);
    }
    assert_eq!(shelf.starts_ms().len(), 1_000);
    // Окрестность одного кадра посреди полки — единицы сегментов, не тысяча.
    let looked_at = shelf
        .shelf
        .neighbourhood_len_for_tests(span(20_000, 20_040));
    assert!(
        looked_at <= 4,
        "neighbourhood of one frame spans {looked_at} entries"
    );
}

/// Замена на журнале однокадровых фрагментов (находка 09b2): раньше каждый приход
/// проверял всю полку и разбор был квадратичным.
#[test]
fn placing_thousands_of_one_frame_fragments_takes_linear_time() {
    const FRAMES: i64 = 10_000;
    let mut shelf = Shelf::new();
    let started = Instant::now();
    for frame in 0..FRAMES {
        shelf.put_exact(frame * 40, (frame + 1) * 40);
    }
    let elapsed = started.elapsed();
    assert_eq!(
        shelf.starts_ms().len(),
        usize::try_from(FRAMES).expect("count")
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "placing {FRAMES} one-frame fragments took {elapsed:?}"
    );
}
