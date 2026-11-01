//! Тесты полки без потоков: место во времени, наложение, выбор следующего,
//! стирание страницей и непрерывный кусок вокруг позиции.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use super::{
    Delivery, IncomingMedia, LaneShelf, LateJoinCandidates, LateJoinWindow, PlayedSoFar,
    ShelfEntry, ShelfRemoval, ShelfRules,
};
use crate::buffer_window::BufferWindow;
use crate::container::PushedContainer;
use crate::feed::{EpisodeContinuity, EpisodeKey, FeedCursor};
use crate::framing::{MediaTime, MediaTimeSpan, SegmentEnd, TimedSpan};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::page_removal::{PageRemovalReport, PageRemovedRange, PageRemovedRangeEnd};
use crate::time::TimestampOffset;

/// Миллисекунда в микросекундах.
const MS: i64 = 1_000;

/// Правила по умолчанию: допуск 10 мс, дыра 100 мс, окно 120/30 с, открытый хвост 2 с.
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

/// Курсор точного конца на `ms`.
fn cursor(ms: i64) -> Option<FeedCursor> {
    Some(FeedCursor::exact(at(ms)))
}

/// Полка с учётом: сегменты кладутся так же, как их кладёт буфер потока.
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

    /// Снимает накрытое и кладёт сегмент из `size` байтов; возвращает число снятых.
    fn put(&mut self, start_ms: i64, end_ms: i64, size: usize) -> usize {
        self.put_timed(TimedSpan::exact(span(start_ms, end_ms)), size)
    }

    /// Как `put`, но с точностью конца из разметки.
    fn put_timed(&mut self, timing: TimedSpan, size: usize) -> usize {
        let replaced = self.shelf.take_overlapped_by(timing, &rules()).len();
        let media = ChargedSegment::charge(&self.ledger, Bytes::from(vec![0; size])).expect("room");
        self.shelf
            .insert(ShelfEntry::waiting(timing, media, self.key.clone()));
        replaced
    }

    fn starts_ms(&self) -> Vec<i64> {
        self.shelf
            .layout()
            .iter()
            .map(|(start, _, _)| start / MS)
            .collect()
    }

    /// Учёт без init (4 байта ключа).
    fn media_bytes(&self) -> usize {
        self.ledger.held_bytes() - 4
    }
}

#[test]
fn segments_lie_in_time_order_not_arrival_order() {
    let mut shelf = Shelf::new();
    shelf.put(400, 600, 1);
    shelf.put(0, 200, 1);
    shelf.put(200, 400, 1);
    assert_eq!(shelf.starts_ms(), [0, 200, 400]);
}

#[test]
fn overlapping_segment_replaces_the_old_one_and_frees_its_bytes() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, 100);
    shelf.put(200, 400, 30);
    assert_eq!(shelf.media_bytes(), 130);
    // Повтор того же времени другой нарезкой (50 байтов) заменяет старый сегмент.
    assert_eq!(shelf.put(0, 200, 50), 1);
    assert_eq!(shelf.starts_ms(), [0, 200]);
    // На полке новые 50 байтов вместо старых 100 — учёт точный.
    assert_eq!(shelf.media_bytes(), 80);
    // Один длинный сегмент накрывает оба.
    assert_eq!(shelf.put(0, 400, 7), 2);
    assert_eq!(shelf.media_bytes(), 7);
}

#[test]
fn adjacent_and_rounding_overlaps_are_not_replacements() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, 1);
    // Точно соседний (fMP4) и наложение округления 5 мс (WebM Opus) — не повтор.
    assert_eq!(shelf.put(200, 400, 1), 0);
    assert_eq!(shelf.put(395, 600, 1), 0);
    assert_eq!(shelf.starts_ms(), [0, 200, 395]);
    // Наложение 50 мс — уже повтор.
    assert_eq!(shelf.put(550, 800, 1), 1);
    // Короткий сегмент целиком внутри другого — повтор при любой длине.
    assert_eq!(shelf.put(100, 105, 1), 1);
    assert_eq!(shelf.starts_ms(), [100, 200, 550]);
}

#[test]
fn next_to_feed_continues_the_cursor_and_skips_past_and_gaps() {
    let mut shelf = Shelf::new();
    let rules = rules();
    shelf.put(400, 600, 1);
    // Курсора ещё нет — первый ждущий задаёт его.
    let first = shelf.shelf.next_to_feed(None, &rules).expect("first");
    let fed = shelf.shelf.take_for_feeding(first, &rules).expect("fed");
    assert_eq!(fed.span, span(400, 600));
    assert_eq!(fed.continuity, EpisodeContinuity::MayContinue);
    // Поданный второй раз не выбирается.
    assert_eq!(shelf.shelf.next_to_feed(cursor(600), &rules), None);

    // Из прошлого (кончился до курсора) и за дырой 200 мс — ждут.
    shelf.put(0, 200, 1);
    shelf.put(800, 1_000, 1);
    assert_eq!(shelf.shelf.next_to_feed(cursor(600), &rules), None);
    // Дыра 50 мс — ещё продолжение.
    shelf.put(650, 800, 1);
    let next = shelf
        .shelf
        .next_to_feed(cursor(600), &rules)
        .expect("continuation");
    let fed = shelf.shelf.take_for_feeding(next, &rules).expect("fed");
    assert_eq!(fed.span, span(650, 800));
    assert_eq!(
        shelf.shelf.layout(),
        [
            (0, 200 * MS, Delivery::Waiting),
            (400 * MS, 600 * MS, Delivery::Fed),
            (650 * MS, 800 * MS, Delivery::Fed),
            (800 * MS, 1_000 * MS, Delivery::Waiting),
        ]
    );
}

#[test]
fn segment_straddling_the_cursor_is_fed_in_a_new_episode() {
    let mut shelf = Shelf::new();
    shelf.put(300, 700, 1);
    // Курсор 500: сегмент начался раньше, а кончается заметно позже.
    let choice = shelf
        .shelf
        .next_to_feed(cursor(500), &rules())
        .expect("straddling");
    let fed = shelf.shelf.take_for_feeding(choice, &rules()).expect("fed");
    assert_eq!(fed.continuity, EpisodeContinuity::MustRestart);
    // Подаётся весь сегмент (300–700), курсор уходит к его концу. Что плеер при
    // этом НЕ получит повтор 300–500, проверяет демуксер полосы («шов»):
    // `stream_buffer::limits_tests::straddling_replacement_never_sends_time_backwards`.
    assert_eq!(fed.span, span(300, 700));
    assert_eq!(fed.fed_to, FeedCursor::exact(at(700)));
    // Сегмент из прошлого (кончился до курсора) — не «перешагивание».
    shelf.put(100, 290, 1);
    assert_eq!(shelf.shelf.next_to_feed(cursor(500), &rules()), None);
}

#[test]
fn oldest_unshared_segment_is_found_by_its_end_and_removal_frees_it() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, 10);
    shelf.put(200, 400, 20);
    shelf.put(400, 600, 40);
    assert_eq!(
        shelf.shelf.oldest_unshared_ended_by(at(400)),
        Some((0, at(200)))
    );
    assert_eq!(shelf.shelf.oldest_unshared_ended_by(at(199)), None);
    let removed = shelf.shelf.evict(0).expect("first segment");
    assert_eq!(removed.media_len(), 10);
    drop(removed);
    assert_eq!(shelf.starts_ms(), [200, 400]);
    assert_eq!(shelf.media_bytes(), 60);
    assert!(shelf.shelf.evict(2).is_none(), "no such place");
}

/// Стирание страницей `[start_ms, end)` при «сыграно до `played_ms`».
fn page_removal(
    shelf: &mut Shelf,
    start_ms: u64,
    end: PageRemovedRangeEnd,
    played: PlayedSoFar,
) -> PageRemovalReport {
    let range = PageRemovedRange::new(Duration::from_millis(start_ms), end);
    let result =
        shelf
            .shelf
            .release_removed_by_page(ShelfRemoval::from_page(range), played, &rules());
    // Отчёт сходится с тем, что действительно снято.
    assert_eq!(result.released.len(), result.report.released_segments);
    result.report
}

fn until_ms(value: u64) -> PageRemovedRangeEnd {
    PageRemovedRangeEnd::At(Duration::from_millis(value))
}

#[test]
fn page_removal_releases_only_played_segments_entirely_inside_the_range() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, 10);
    shelf.put(200, 400, 20);
    shelf.put(400, 600, 40);
    shelf.put(600, 800, 80);
    let played = PlayedSoFar::Until(at(500));
    // [0, 700): 0–200 и 200–400 сыграны и целиком внутри; 400–600 внутри, но не
    // сыгран (позиция 500); 600–800 задет лишь частично.
    let report = page_removal(&mut shelf, 0, until_ms(700), played);
    assert_eq!(
        report,
        PageRemovalReport {
            released_segments: 2,
            released_bytes: 30,
            kept_not_played: 1,
            kept_in_pipe: 0,
            kept_partially_removed: 1,
        }
    );
    assert_eq!(shelf.starts_ms(), [400, 600]);
    assert_eq!(
        shelf.media_bytes(),
        120,
        "the ledger dropped exactly 30 bytes"
    );
    // До конца шкалы, но ничего не сыграно: ничего не уходит.
    let report = page_removal(
        &mut shelf,
        0,
        PageRemovedRangeEnd::EndOfTimeline,
        PlayedSoFar::Nothing,
    );
    assert_eq!(report.released_segments, 0);
    assert_eq!(report.kept_not_played, 2);
    assert_eq!(shelf.media_bytes(), 120);
}

#[test]
fn page_removal_tolerates_rounding_of_the_range_edges_only() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, 10);
    shelf.put(200, 400, 20);
    let played = PlayedSoFar::Until(at(400));
    // Края диапазона страницы разошлись с краями сегментов на 5 мс округления:
    // сегмент 0–200 считается стёртым целиком.
    let report = page_removal(&mut shelf, 5, until_ms(195), played);
    assert_eq!(report.released_segments, 1);
    // 20 мс — уже не округление: часть сегмента 200–400 у страницы осталась.
    let report = page_removal(&mut shelf, 220, PageRemovedRangeEnd::EndOfTimeline, played);
    assert_eq!(
        (report.released_segments, report.kept_partially_removed),
        (0, 1)
    );
    // Пустой и перевёрнутый диапазоны ничего не трогают.
    assert_eq!(
        page_removal(&mut shelf, 300, until_ms(300), played),
        PageRemovalReport::default()
    );
    assert_eq!(
        page_removal(&mut shelf, 400, until_ms(200), played),
        PageRemovalReport::default()
    );
    assert_eq!(shelf.starts_ms(), [200]);
}

#[test]
fn page_removal_keeps_a_played_segment_whose_bytes_a_pipe_still_holds() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, 10);
    shelf.put(200, 400, 20);
    // Сегмент 0–200 отдан в трубу эпизода, демуксер его ещё не дочитал.
    let choice = shelf.shelf.next_to_feed(None, &rules()).expect("first");
    let in_pipe = shelf.shelf.take_for_feeding(choice, &rules()).expect("fed");
    let report = page_removal(&mut shelf, 0, until_ms(400), PlayedSoFar::Until(at(400)));
    assert_eq!(
        (report.released_segments, report.kept_in_pipe),
        (1, 1),
        "only the segment without a pipe copy is released"
    );
    assert_eq!(shelf.starts_ms(), [0]);
    drop(in_pipe);
}

#[test]
fn covered_run_bridges_small_gaps_only() {
    let mut shelf = Shelf::new();
    let rules = rules();
    shelf.put(0, 200, 1);
    shelf.put(200, 400, 1);
    // Дыра 50 мс внутри куска; дыра 400 мс разделяет куски.
    shelf.put(450, 600, 1);
    shelf.put(1_000, 1_200, 1);
    assert_eq!(shelf.shelf.covered_run_end(at(100), &rules), Some(at(600)));
    assert_eq!(shelf.shelf.covered_run_end(at(420), &rules), Some(at(600)));
    // Позиция в дыре 600–1000 не покрыта.
    assert_eq!(shelf.shelf.covered_run_end(at(700), &rules), None);
    // В пределах наибольшей дыры перед куском — покрыта (плеер её перешагнёт).
    assert_eq!(
        shelf.shelf.covered_run_end(at(950), &rules),
        Some(at(1_200))
    );
    assert_eq!(shelf.shelf.covered_run_end(at(1_200), &rules), None);
    assert_eq!(LaneShelf::default().covered_run_end(at(0), &rules), None);
}

#[test]
fn seek_restart_is_the_segment_holding_the_target_or_just_after_a_small_gap() {
    let mut shelf = Shelf::new();
    shelf.put(0, 200, 1);
    shelf.put(250, 400, 1);
    shelf.put(800, 1_000, 1);
    // Внутри сегмента, на его начале и в маленькой дыре (≤ 100 мс) перед ним.
    for target in [300, 250, 210] {
        assert!(
            shelf
                .shelf
                .holds(at(target), &rules(), IncomingMedia::MayArrive),
            "{target}"
        );
    }
    // В большой дыре и после конца полки цели нет.
    for target in [500, 1_000] {
        assert!(
            !shelf
                .shelf
                .holds(at(target), &rules(), IncomingMedia::MayArrive),
            "{target}"
        );
    }
}

#[test]
fn seek_restart_makes_the_target_segment_and_everything_after_it_wait_again() {
    let mut shelf = Shelf::new();
    for start in [0, 200, 400] {
        shelf.put(start, start + 200, 1);
    }
    // Всё подано (без курсора полка отдаёт первый ждущий).
    while let Some(choice) = shelf.shelf.next_to_feed(None, &rules()) {
        shelf.shelf.take_for_feeding(choice, &rules()).expect("fed");
    }
    assert!(
        shelf
            .shelf
            .prepare_restart(at(1_000), &rules(), IncomingMedia::MayArrive)
            .is_none()
    );
    let choice = shelf
        .shelf
        .prepare_restart(at(300), &rules(), IncomingMedia::MayArrive)
        .expect("on shelf");
    let fed = shelf
        .shelf
        .take_for_feeding(choice, &rules())
        .expect("restart segment");
    // С начала сегмента с целью и обязательно новым эпизодом.
    assert_eq!(fed.span, span(200, 400));
    assert_eq!(fed.continuity, EpisodeContinuity::MustRestart);
    let deliveries: Vec<Delivery> = shelf
        .shelf
        .layout()
        .iter()
        .map(|(_, _, delivery)| *delivery)
        .collect();
    assert_eq!(
        deliveries,
        [Delivery::Fed, Delivery::Fed, Delivery::Waiting]
    );
}

/// Позднее вступление с любым сегментом полки (перемотка по полке).
const ANY: LateJoinCandidates = LateJoinCandidates::AnyOnShelf;

/// Окно позднего вступления: цель `target_ms`, другие полосы непрерывно
/// покрывают `[from_ms, until_ms)`.
fn late_window(target_ms: i64, from_ms: i64, until_ms: i64) -> LateJoinWindow {
    LateJoinWindow::new(at(target_ms), span(from_ms, until_ms)).expect("covered after the target")
}

#[test]
fn late_join_window_needs_other_lanes_covering_time_after_the_target() {
    // Другие полосы кончаются у цели или раньше — вступать некуда.
    assert_eq!(LateJoinWindow::new(at(200), span(0, 200)), None);
    assert_eq!(LateJoinWindow::new(at(200), span(0, 100)), None);
    assert!(LateJoinWindow::new(at(200), span(0, 201)).is_some());
}

#[test]
fn late_join_starts_at_the_first_segment_after_the_target_inside_the_window() {
    let mut shelf = Shelf::new();
    // Звук: до цели (кончился раньше), затем с 405 мс, дальше подряд.
    shelf.put(0, 100, 1);
    for start in [405, 810, 1_215] {
        shelf.put(start, start + 405, 1);
    }
    // Всё уже подано — позднее вступление снова сделает ждущими только сегменты
    // с точки вступления.
    while let Some(choice) = shelf.shelf.next_to_feed(None, &rules()) {
        shelf.shelf.take_for_feeding(choice, &rules()).expect("fed");
    }
    // Начало первого сегмента после цели (405) — за окном (другие полосы
    // покрывают только до 405): вступать рано, ничего не меняется.
    assert!(!shelf.shelf.can_join_late(late_window(200, 150, 405), ANY));
    assert!(
        shelf
            .shelf
            .prepare_late_join(late_window(200, 150, 405), ANY)
            .is_none()
    );
    // Цель внутри сегмента 405–810: данные у цели есть, это не позднее
    // вступление (следующий сегмент, 810, не точка вступления).
    assert!(!shelf.shelf.can_join_late(late_window(500, 150, 4_000), ANY));

    // Другие полосы непрерывны с 150 (звук 0–100 лежит вне их куска).
    let window = late_window(200, 150, 4_000);
    assert!(shelf.shelf.can_join_late(window, ANY));
    let choice = shelf
        .shelf
        .prepare_late_join(window, ANY)
        .expect("joins at its first segment after the target");
    let fed = shelf
        .shelf
        .take_for_feeding(choice, &rules())
        .expect("join segment");
    assert_eq!(fed.span, span(405, 810));
    assert_eq!(fed.continuity, EpisodeContinuity::MustRestart);
    let deliveries: Vec<Delivery> = shelf
        .shelf
        .layout()
        .iter()
        .map(|(_, _, delivery)| *delivery)
        .collect();
    // Сегмент до цели не тронут, точка вступления подана, дальнейшие снова ждут.
    assert_eq!(
        deliveries,
        [
            Delivery::Fed,
            Delivery::Fed,
            Delivery::Waiting,
            Delivery::Waiting
        ]
    );
    // Пустая полка или полка только до цели вступать не может.
    assert!(!Shelf::new().shelf.can_join_late(window, ANY));
    let mut past_only = Shelf::new();
    past_only.put(0, 150, 1);
    assert!(!past_only.shelf.can_join_late(window, ANY));
}

#[test]
fn data_of_the_lane_inside_the_window_before_the_join_point_is_a_hole_not_a_late_start() {
    // Звук 0–400 и с 1 200: дыра 400–1 200 посреди полосы, а другие полосы
    // покрывают 0–4 000 подряд. Цель 800 в дыре — это не позднее начало звука, а
    // недокачанное: вступать с 1 200 нельзя (байты с цели пришлёт страница).
    let mut shelf = Shelf::new();
    shelf.put(0, 400, 1);
    shelf.put(1_200, 1_600, 1);
    assert!(!shelf.shelf.can_join_late(late_window(800, 0, 4_000), ANY));
    assert!(
        shelf
            .shelf
            .prepare_late_join(late_window(800, 0, 4_000), ANY)
            .is_none()
    );
    assert_eq!(
        shelf
            .shelf
            .layout()
            .iter()
            .map(|(_, _, delivery)| *delivery)
            .collect::<Vec<_>>(),
        [Delivery::Waiting, Delivery::Waiting],
        "a refused join changes nothing"
    );
    // Те же данные, но другие полосы непрерывны только с 600: данные звука до
    // 400 лежат вне их куска — это начало звука внутри куска, вступление можно.
    assert!(shelf.shelf.can_join_late(late_window(800, 600, 4_000), ANY));
}

#[test]
fn after_an_off_shelf_seek_only_segments_arrived_since_the_seek_can_be_join_points() {
    // Звук с 405 лежал на полке до перемотки (время прежнего места). Цель
    // попросили у страницы: вступать с лежавшего нельзя — страница ещё несёт
    // данные цели; сегмент, пришедший после перемотки, — можно.
    let since_seek = LateJoinCandidates::ArrivedSinceSeek;
    let window = late_window(200, 150, 4_000);
    let mut shelf = Shelf::new();
    shelf.put(405, 810, 1);
    assert!(
        shelf.shelf.can_join_late(window, since_seek),
        "accepted before any seek: counts as arrived since the last seek"
    );
    shelf.shelf.note_seek();
    assert!(!shelf.shelf.can_join_late(window, since_seek));
    assert!(shelf.shelf.prepare_late_join(window, since_seek).is_none());
    assert!(
        shelf.shelf.can_join_late(window, ANY),
        "by the shelf it is a join point"
    );
    // Страница прислала тот же сегмент заново (замена) — он из её ответа.
    assert_eq!(
        shelf.put(405, 810, 1),
        1,
        "the resent segment replaces the old one"
    );
    let choice = shelf
        .shelf
        .prepare_late_join(window, since_seek)
        .expect("the page's own segment is a join point");
    let fed = shelf
        .shelf
        .take_for_feeding(choice, &rules())
        .expect("join segment");
    assert_eq!(fed.span, span(405, 810));
}

/// Отрезок с оценённым концом: точно известно до `certain_ms`.
fn estimated(start_ms: i64, certain_ms: i64, end_ms: i64) -> TimedSpan {
    TimedSpan::new(
        span(start_ms, end_ms),
        SegmentEnd::Estimated {
            certain_end: at(certain_ms),
        },
    )
}

/// Отрезок с открытым концом: известно только до `certain_ms`.
fn open(start_ms: i64, certain_ms: i64) -> TimedSpan {
    TimedSpan::new(
        span(start_ms, certain_ms),
        SegmentEnd::Open {
            certain_end: at(certain_ms),
        },
    )
}

#[test]
fn estimated_end_continues_into_the_next_cluster_and_never_replaces_it() {
    let mut shelf = Shelf::new();
    let rules = rules();
    // Кластер 4 к/с: кадры 0–750 мс, конец оценён в 1000 мс.
    shelf.put_timed(estimated(0, 750, 1_000), 1);
    // Следующий начался на 980 мс (переменная частота кадров): оценка накрывает
    // его на 20 мс, но точно известное — нет, поэтому это не повтор.
    assert_eq!(shelf.put(980, 1_500, 1), 0);
    assert_eq!(shelf.starts_ms(), [0, 980]);
    let first = shelf.shelf.next_to_feed(None, &rules).expect("first");
    let fed = shelf.shelf.take_for_feeding(first, &rules).expect("fed");
    assert_eq!(fed.fed_to, FeedCursor::new(at(750), at(1_000)));
    let next = shelf
        .shelf
        .next_to_feed(Some(fed.fed_to), &rules)
        .expect("the next cluster continues the estimated end");
    assert_eq!(next.continuity(), EpisodeContinuity::MayContinue);
    // Цель в последнем кадре кластера (875 мс) лежит на полке.
    assert!(shelf.shelf.holds(at(875), &rules, IncomingMedia::MayArrive));
    assert_eq!(shelf.shelf.covered_run_end(at(0), &rules), Some(at(1_500)));
}

#[test]
fn open_end_reaches_the_next_segment_for_feeding_and_seeking() {
    let mut shelf = Shelf::new();
    let rules = rules();
    // Один кадр в 0 мс без длительности, следующий сегмент — через секунду.
    shelf.put_timed(open(0, 0), 1);
    shelf.put(1_000, 2_000, 1);
    let first = shelf.shelf.next_to_feed(None, &rules).expect("first");
    let fed = shelf.shelf.take_for_feeding(first, &rules).expect("fed");
    // Открытый хвост тянется на 2 с за начало кадра.
    assert_eq!(fed.fed_to, FeedCursor::new(at(0), at(2_000)));
    assert!(shelf.shelf.next_to_feed(Some(fed.fed_to), &rules).is_some());
    // Цель внутри открытого кадра (до начала следующего сегмента) — на полке.
    assert!(shelf.shelf.holds(at(500), &rules, IncomingMedia::MayArrive));
    // Повтор того же одиночного кадра — замена, а не второй сегмент.
    assert_eq!(shelf.put_timed(open(0, 0), 1), 1);
    assert_eq!(shelf.starts_ms(), [0, 1_000]);
}

#[test]
fn after_the_end_of_input_a_target_past_the_shelf_lands_on_the_last_segment() {
    let mut shelf = Shelf::new();
    let rules = rules();
    shelf.put(0, 200, 1);
    shelf.put(200, 400, 1);
    // Вход ещё открыт: за концом полки — просить страницу.
    assert!(!shelf.shelf.holds(at(400), &rules, IncomingMedia::MayArrive));
    // Вход окончен: на длительность и дальше — последний сегмент.
    for target in [400, 450, 10_000] {
        assert!(
            shelf.shelf.holds(at(target), &rules, IncomingMedia::Ended),
            "{target}"
        );
    }
    let choice = shelf
        .shelf
        .prepare_restart(at(400), &rules, IncomingMedia::Ended)
        .expect("lands");
    let fed = shelf.shelf.take_for_feeding(choice, &rules).expect("fed");
    assert_eq!(fed.span, span(200, 400));
    // Большая дыра посреди полки — не «за концом»: там цели нет и после конца.
    shelf.put(1_000, 1_200, 1);
    assert!(!shelf.shelf.holds(at(700), &rules, IncomingMedia::Ended));
}

#[test]
fn ceiling_candidates_skip_segments_still_held_by_a_pipe() {
    let mut shelf = Shelf::new();
    let rules = rules();
    shelf.put(0, 200, 10);
    shelf.put(200, 400, 20);
    let first = shelf.shelf.next_to_feed(None, &rules).expect("first");
    // Копия первого сегмента «в трубе»: демуксер её ещё не прочитал.
    let in_pipe = shelf.shelf.take_for_feeding(first, &rules).expect("fed");
    assert_eq!(
        shelf.shelf.oldest_unshared_ended_by(at(600)),
        Some((1, at(400)))
    );
    drop(in_pipe);
    assert_eq!(
        shelf.shelf.oldest_unshared_ended_by(at(600)),
        Some((0, at(200)))
    );
}
