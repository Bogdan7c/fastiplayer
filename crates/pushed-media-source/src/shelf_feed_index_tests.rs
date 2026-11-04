//! Тесты указателя подачи: выбор следующего сегмента по дереву ждущих даёт тот же
//! ответ, что перебор всей полки, отсев не теряет сегменты, а журнал однокадровых
//! фрагментов разбирается за линейное время (находка 09b2).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::super::{Delivery, LaneShelf, ShelfEntry, ShelfRules};
use crate::buffer_window::BufferWindow;
use crate::container::PushedContainer;
use crate::feed::{EpisodeContinuity, EpisodeKey, FeedCursor};
use crate::framing::{
    DecodeTiming, MediaTime, MediaTimeSpan, TimedSpan, TrackMedia, TrackSlotAssigner, TrackSpans,
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

fn at(ms: i64) -> MediaTime {
    MediaTime::from_micros(ms * MS)
}

/// Дорожка с отрезком (место — первое видео или первый звук).
fn track(media: TrackMedia, start_ms: i64, end_ms: i64) -> TrackSpans {
    let mut tracks = TrackSpans::default();
    tracks.include(
        TrackSlotAssigner::default().next(media),
        span(start_ms, end_ms),
    );
    tracks
}

/// Полка с учётом байтов: сегменты кладутся так же, как их кладёт буфер потока.
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

    /// Кладёт сегмент без сведений о дорожках; возвращает число снятых им.
    fn put(&mut self, start_ms: i64, end_ms: i64) -> usize {
        self.put_with_tracks(start_ms, end_ms, TrackSpans::default())
    }

    /// Кладёт сегмент с дорожками `tracks` (время декодирования = время показа).
    fn put_with_tracks(&mut self, start_ms: i64, end_ms: i64, tracks: TrackSpans) -> usize {
        let timing = TimedSpan::exact(span(start_ms, end_ms));
        let replaced = self
            .shelf
            .take_overlapped_by(timing, &tracks, &rules())
            .len();
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

    /// Подаёт всё, что продолжает курсор; возвращает начала поданных (мс) и курсор.
    fn feed_from(&mut self, mut cursor: Option<FeedCursor>) -> (Vec<i64>, Option<FeedCursor>) {
        let mut fed_starts = Vec::new();
        while let Some(choice) = self.shelf.next_to_feed(cursor.as_ref(), &rules()) {
            let fed = self
                .shelf
                .take_for_feeding(choice, &rules())
                .expect("chosen segment is on the shelf");
            fed_starts.push(fed.span.start().as_micros() / MS);
            cursor = Some(fed.fed_to);
        }
        (fed_starts, cursor)
    }

    fn waiting_starts_ms(&self) -> Vec<i64> {
        self.shelf
            .layout()
            .iter()
            .filter(|(_, _, delivery)| *delivery == Delivery::Waiting)
            .map(|(start, _, _)| start / MS)
            .collect()
    }
}

#[test]
fn feeding_follows_decode_order_and_arrival_order_for_equal_starts() {
    let mut shelf = Shelf::new();
    // Приход не по времени: полка кладёт по началу, подаёт по времени декодирования.
    shelf.put(400, 600);
    shelf.put(0, 200);
    shelf.put(200, 400);
    let (fed, cursor) = shelf.feed_from(None);
    assert_eq!(fed, [0, 200, 400]);
    assert_eq!(cursor.expect("cursor").certain_end(), at(600));
}

#[test]
fn a_segment_from_the_past_is_pruned_and_a_later_arrival_is_still_fed() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200);
    shelf.put(200, 400);
    // Курсор уже за обоими: они из прошлого и не подаются (отсев).
    let cursor = FeedCursor::exact(at(400));
    assert_eq!(shelf.shelf.next_to_feed(Some(&cursor), &rules()), None);
    assert_eq!(
        shelf.waiting_starts_ms(),
        [0, 200],
        "отсев не снимает с полки"
    );
    // Пришло продолжение: оно подаётся, а отсеянные — нет.
    shelf.put(400, 600);
    let (fed, _) = shelf.feed_from(Some(cursor));
    assert_eq!(fed, [400]);
    assert_eq!(shelf.waiting_starts_ms(), [0, 200]);
}

#[test]
fn pruned_segments_come_back_when_the_cursor_is_reset_by_a_seek() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200);
    shelf.put(200, 400);
    assert_eq!(
        shelf
            .shelf
            .next_to_feed(Some(&FeedCursor::exact(at(400))), &rules()),
        None
    );
    // Перемотка переставляет курсор: подача без курсора снова видит самый ранний.
    shelf.shelf.note_seek();
    let (fed, _) = shelf.feed_from(None);
    assert_eq!(fed, [0, 200]);
}

#[test]
fn pruned_segments_come_back_for_a_request_without_a_cursor() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200);
    assert_eq!(
        shelf
            .shelf
            .next_to_feed(Some(&FeedCursor::exact(at(400))), &rules()),
        None
    );
    // Полоса потеряла курсор (новый эпизод с нуля): отсеянный сегмент снова в счёте.
    let (fed, _) = shelf.feed_from(None);
    assert_eq!(fed, [0]);
}

#[test]
fn a_tracked_segment_is_not_pruned_while_the_cursor_knows_no_tracks() {
    let mut shelf = Shelf::new();
    shelf.put_with_tracks(0, 200, track(TrackMedia::NotVideo, 0, 200));
    // Курсор без дорожек: по объединённому отрезку сегмент из прошлого — не подаётся…
    let untracked = FeedCursor::exact(at(400));
    assert_eq!(shelf.shelf.next_to_feed(Some(&untracked), &rules()), None);
    // …но курсор с дорожками (только видео) общих дорожек с ним не имеет: назад он не
    // уходит и подаётся как продолжение. Отсев при курсоре без дорожек его потерял бы.
    let video_cursor = FeedCursor::new(at(400), at(400), track(TrackMedia::Video, 200, 400));
    let choice = shelf
        .shelf
        .next_to_feed(Some(&video_cursor), &rules())
        .expect("audio-only segment continues a video cursor");
    assert_eq!(choice.continuity(), EpisodeContinuity::MayContinue);
}

#[test]
fn replaced_and_evicted_segments_are_never_chosen() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200);
    shelf.put(200, 400);
    shelf.put(400, 600);
    // Повтор середины другой нарезкой снимает старый сегмент; выселение — третий.
    assert_eq!(shelf.put(200, 400), 1);
    assert!(shelf.shelf.evict(2).is_some());
    let (fed, _) = shelf.feed_from(None);
    assert_eq!(fed, [0, 200]);
    assert_eq!(shelf.shelf.layout().len(), 2);
}

#[test]
fn a_seek_restart_feeds_the_target_and_everything_after_it_again() {
    let mut shelf = Shelf::new();
    for start in [0, 200, 400, 600] {
        shelf.put(start, start + 200);
    }
    let (fed, _) = shelf.feed_from(None);
    assert_eq!(fed, [0, 200, 400, 600]);
    let choice = shelf
        .shelf
        .prepare_restart(at(300), &rules(), super::super::IncomingMedia::MayArrive)
        .expect("target on the shelf");
    let fed = shelf
        .shelf
        .take_for_feeding(choice, &rules())
        .expect("restart segment");
    assert_eq!(fed.span, span(200, 400));
    let (fed, _) = shelf.feed_from(Some(fed.fed_to));
    assert_eq!(fed, [400, 600]);
}

/// Журнал страницы (решение 176) из однокадровых фрагментов: история, которую плеер
/// не играет (курсор уже за ней), и тысячи фрагментов, подаваемых по мере прихода.
/// До находки 09b2 каждый приход стоил перебор всей полки, и разбор был квадратичным.
#[test]
fn a_journal_of_one_frame_fragments_is_fed_in_linear_time() {
    const FRAME_MS: i64 = 40;
    const HISTORY_FRAMES: i64 = 2_000;
    const LIVE_FRAMES: i64 = 6_000;
    let mut shelf = Shelf::new();
    for frame in 0..HISTORY_FRAMES {
        shelf.put(frame * FRAME_MS, (frame + 1) * FRAME_MS);
    }
    // Курсор стоит за историей: она — прошлое и ждёт на полке (как у Twitch после
    // перемотки страницы вперёд).
    let mut cursor = Some(FeedCursor::exact(at(HISTORY_FRAMES * FRAME_MS)));
    let started = Instant::now();
    let mut fed_total = 0;
    for frame in HISTORY_FRAMES..HISTORY_FRAMES + LIVE_FRAMES {
        shelf.put(frame * FRAME_MS, (frame + 1) * FRAME_MS);
        let (fed, next) = shelf.feed_from(cursor);
        fed_total += fed.len();
        cursor = next;
    }
    let elapsed = started.elapsed();
    assert_eq!(fed_total, usize::try_from(LIVE_FRAMES).expect("count"));
    assert_eq!(
        shelf.waiting_starts_ms().len(),
        usize::try_from(HISTORY_FRAMES).expect("count"),
        "история остаётся на полке нетронутой"
    );
    // Линейный разбор — доли секунды даже в отладочной сборке; квадратичный — десятки.
    assert!(
        elapsed < Duration::from_secs(3),
        "feeding {LIVE_FRAMES} one-frame fragments over a {HISTORY_FRAMES}-frame history took {elapsed:?}"
    );
}
