//! Тесты полки: подача в порядке ДЕКОДИРОВАНИЯ (однокадровые фрагменты с B-кадрами,
//! низкая задержка эфира Twitch, сессия 09b2 моста).
//!
//! GOP из пяти кадров по 40 мс, как у H.264 с пирамидой B: порядок декодирования
//! I, P, B, b, b; показ — I, b, B, b, P. Каждый кадр — свой фрагмент:
//!
//! | кадр | декодирование | показ     |
//! |------|---------------|-----------|
//! | I    | 0–40          | 80–120    |
//! | P    | 40–80         | 240–280   |
//! | B    | 80–120        | 160–200   |
//! | b1   | 120–160       | 120–160   |
//! | b2   | 160–200       | 200–240   |

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use super::{Delivery, IncomingMedia, LaneShelf, ShelfEntry, ShelfRules};
use crate::buffer_window::BufferWindow;
use crate::container::PushedContainer;
use crate::feed::{EpisodeContinuity, EpisodeKey, FeedCursor};
use crate::framing::{DecodeTiming, MediaTime, MediaTimeSpan, TimedSpan, TrackSpans};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::time::TimestampOffset;

/// Миллисекунда в микросекундах.
const MS: i64 = 1_000;

/// Кадры GOP: (имя, начало декодирования, начало показа), кадр — 40 мс.
const GOP: [(&str, i64, i64); 5] = [
    ("I", 0, 80),
    ("P", 40, 240),
    ("B", 80, 160),
    ("b1", 120, 120),
    ("b2", 160, 200),
];
/// Длительность кадра, мс.
const FRAME_MS: i64 = 40;

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

/// Отрезок в один кадр от `start_ms`.
fn frame_span(start_ms: i64) -> MediaTimeSpan {
    MediaTimeSpan::new(
        MediaTime::from_micros(start_ms * MS),
        MediaTime::from_micros((start_ms + FRAME_MS) * MS),
    )
    .expect("ordered span")
}

/// Полка однокадровых фрагментов с учётом байтов.
struct FrameShelf {
    shelf: LaneShelf,
    ledger: Arc<IntakeLedger>,
    key: EpisodeKey,
}

impl FrameShelf {
    fn new() -> Self {
        Self {
            shelf: LaneShelf::default(),
            ledger: Arc::new(IntakeLedger::new(1 << 20)),
            key: EpisodeKey::new(PushedContainer::FragmentedMp4, None, TimestampOffset::ZERO),
        }
    }

    /// Кладёт однокадровый фрагмент кадра `name` из [`GOP`], как буфер потока.
    fn put_frame(&mut self, name: &str) {
        let &(_, decode_ms, presentation_ms) = GOP
            .iter()
            .find(|(frame, _, _)| *frame == name)
            .expect("frame of the GOP");
        let presentation = TimedSpan::exact(frame_span(presentation_ms));
        let decode = DecodeTiming::follows_presentation(
            TimedSpan::exact(frame_span(decode_ms)),
            TrackSpans::default(),
        );
        let replaced =
            self.shelf
                .take_overlapped_by(presentation, &TrackSpans::default(), &rules());
        assert!(
            replaced.is_empty(),
            "frames of one GOP never replace each other"
        );
        let media =
            ChargedSegment::charge(&self.ledger, Bytes::from_static(b"frame")).expect("room");
        self.shelf.insert(ShelfEntry::waiting(
            presentation,
            TrackSpans::default(),
            decode,
            media,
            self.key.clone(),
        ));
    }

    /// Подаёт всё, что продолжает поданное, как `stream_buffer::feed_contiguous`;
    /// возвращает начала показа поданных (мс) и итоговый курсор.
    fn feed_contiguous(
        &mut self,
        mut cursor: Option<FeedCursor>,
    ) -> (Vec<i64>, Option<FeedCursor>) {
        let rules = rules();
        let mut fed_starts = Vec::new();
        while let Some(choice) = self.shelf.next_to_feed(cursor.as_ref(), &rules) {
            assert_eq!(
                choice.continuity(),
                EpisodeContinuity::MayContinue,
                "decode order continues the episode"
            );
            let fed = self.shelf.take_for_feeding(choice, &rules).expect("fed");
            fed_starts.push(fed.span.start().as_micros() / MS);
            cursor = Some(match cursor {
                // Курсор копится так же, как у `LaneFeed::advance_cursor`.
                Some(previous) if previous.certain_end() > fed.fed_to.certain_end() => previous,
                _ => fed.fed_to,
            });
        }
        (fed_starts, cursor)
    }

    /// Подан ли кадр с началом показа `presentation_ms`.
    fn delivery_at(&self, presentation_ms: i64) -> Delivery {
        self.shelf
            .layout()
            .into_iter()
            .find(|(start, _, _)| *start == presentation_ms * MS)
            .map(|(_, _, delivery)| delivery)
            .expect("frame on the shelf")
    }
}

#[test]
fn single_frame_fragments_are_fed_in_decode_order_not_presentation_order() {
    let mut shelf = FrameShelf::new();
    // Вся группа пришла одним `appendBuffer`.
    for (name, _, _) in GOP {
        shelf.put_frame(name);
    }

    let (fed, _) = shelf.feed_contiguous(None);

    // Начала показа в порядке декодирования: I, P, B, b1, b2.
    assert_eq!(fed, [80, 240, 160, 120, 200]);
    // Полка при этом лежит по времени показа (буферизовано, перемотка, замена).
    let presentation_starts: Vec<i64> = shelf
        .shelf
        .layout()
        .iter()
        .map(|(start, _, _)| start / MS)
        .collect();
    assert_eq!(presentation_starts, [80, 120, 160, 200, 240]);
}

#[test]
fn a_b_frame_arriving_after_its_p_continues_the_feed() {
    let mut shelf = FrameShelf::new();
    shelf.put_frame("I");
    shelf.put_frame("P");
    let (fed, cursor) = shelf.feed_contiguous(None);
    assert_eq!(fed, [80, 240]);

    // B показывается раньше уже поданного P, но декодируется после него: это
    // продолжение, а не «время назад» (раньше он навсегда оставался на полке).
    for name in ["B", "b1", "b2"] {
        shelf.put_frame(name);
    }
    let (fed, _) = shelf.feed_contiguous(cursor);

    assert_eq!(fed, [160, 120, 200]);
    assert!(
        shelf
            .shelf
            .layout()
            .iter()
            .all(|(_, _, delivery)| *delivery == Delivery::Fed),
        "every frame reaches the demuxer"
    );
}

#[test]
fn seek_restart_rewaits_frames_decoded_after_the_landing_frame() {
    let mut shelf = FrameShelf::new();
    for (name, _, _) in GOP {
        shelf.put_frame(name);
    }
    shelf.feed_contiguous(None);

    // Цель 170 мс — внутри показа B (160–200): с него подача начнётся заново.
    let restart = shelf
        .shelf
        .prepare_restart(
            MediaTime::from_micros(170 * MS),
            &rules(),
            IncomingMedia::MayArrive,
        )
        .expect("target on the shelf");
    let fed = shelf
        .shelf
        .take_for_feeding(restart, &rules())
        .expect("fed");
    assert_eq!(fed.span.start().as_micros(), 160 * MS);
    assert_eq!(fed.continuity, EpisodeContinuity::MustRestart);

    // b1 показывается раньше B, но декодируется после него — снова ждёт подачи;
    // I показывается и декодируется раньше B — остаётся поданным.
    assert_eq!(shelf.delivery_at(120), Delivery::Waiting, "b1");
    assert_eq!(shelf.delivery_at(200), Delivery::Waiting, "b2");
    assert_eq!(shelf.delivery_at(80), Delivery::Fed, "I");
    // После B подача идёт дальше по декодированию: b1, затем b2. P декодируется
    // раньше B — его время уже позади, повторно он не подаётся.
    let (fed, _) = shelf.feed_contiguous(Some(fed.fed_to));
    assert_eq!(fed, [120, 200]);
}
