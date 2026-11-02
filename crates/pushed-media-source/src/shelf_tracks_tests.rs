//! Полка по дорожкам: полоса «видео + звук» в одном SourceBuffer (сессия 09a моста).
//!
//! Числа — как у фрагментов ffmpeg с AAC: граница звука расходится с границей видео
//! на 5–16 мс (кадр AAC — 21 мс), у стенда `mse-muxed` — 5,3 / 10,6 / 15,9 мс.

use super::*;
use crate::framing::{TrackMedia, TrackSlotAssigner};

/// Отрезки видео и звука одного фрагмента: (объединённый отрезок, по дорожкам).
fn muxed(video: (i64, i64), audio: (i64, i64)) -> (TimedSpan, TrackSpans) {
    let mut slots = TrackSlotAssigner::default();
    let video_slot = slots.next(TrackMedia::Video);
    let audio_slot = slots.next(TrackMedia::NotVideo);
    let mut tracks = TrackSpans::default();
    tracks.include(video_slot, span(video.0, video.1));
    tracks.include(audio_slot, span(audio.0, audio.1));
    let union = span(video.0, video.1).union(span(audio.0, audio.1));
    (TimedSpan::exact(union), tracks)
}

/// Фрагмент только с видео.
fn video_only(start_ms: i64, end_ms: i64) -> (TimedSpan, TrackSpans) {
    let mut slots = TrackSlotAssigner::default();
    let mut tracks = TrackSpans::default();
    tracks.include(slots.next(TrackMedia::Video), span(start_ms, end_ms));
    (TimedSpan::exact(span(start_ms, end_ms)), tracks)
}

/// Фрагмент только со звуком (место звука — как в init «видео + звук»).
fn audio_only(start_ms: i64, end_ms: i64) -> (TimedSpan, TrackSpans) {
    let mut slots = TrackSlotAssigner::default();
    let _video = slots.next(TrackMedia::Video);
    let mut tracks = TrackSpans::default();
    tracks.include(slots.next(TrackMedia::NotVideo), span(start_ms, end_ms));
    (TimedSpan::exact(span(start_ms, end_ms)), tracks)
}

impl Shelf {
    /// Кладёт фрагмент с временем дорожек; возвращает число снятых.
    fn put_fragment(&mut self, (timing, tracks): (TimedSpan, TrackSpans)) -> usize {
        self.put_with_tracks(timing, tracks, 1)
    }
}

#[test]
fn muxed_neighbours_with_overhanging_audio_both_stay_on_the_shelf() {
    let mut shelf = Shelf::new();
    assert_eq!(shelf.put_fragment(muxed((0, 400), (0, 416))), 0);
    // Объединённые отрезки накрывают друг друга на 16 мс (> допуска 10 мс), а у
    // каждой дорожки по отдельности пересечения нет — это соседи, не повтор.
    assert_eq!(shelf.put_fragment(muxed((400, 800), (416, 810))), 0);
    assert_eq!(shelf.starts_ms(), [0, 400]);
}

#[test]
fn without_track_times_the_union_rule_still_applies() {
    let mut shelf = Shelf::new();
    // Тот же случай без сведений о дорожках (сегмент без кадров, старое поведение):
    // объединённые отрезки накрывают друг друга больше допуска — замена.
    shelf.put_timed(TimedSpan::exact(span(0, 416)), 1);
    assert_eq!(shelf.put_timed(TimedSpan::exact(span(400, 810)), 1), 1);
    assert_eq!(shelf.starts_ms(), [400]);
}

#[test]
fn a_real_repeat_of_a_muxed_fragment_still_replaces_it() {
    let mut shelf = Shelf::new();
    shelf.put_fragment(muxed((0, 400), (0, 416)));
    shelf.put_fragment(muxed((400, 800), (416, 810)));

    // Страница прислала первый фрагмент ещё раз (повтор после `abort`).
    assert_eq!(shelf.put_fragment(muxed((0, 400), (0, 416))), 1);
    assert_eq!(shelf.starts_ms(), [0, 400]);
    assert_eq!(shelf.media_bytes(), 2, "the repeat replaced, not doubled");
}

#[test]
fn video_only_and_audio_only_fragments_of_the_same_time_share_the_shelf() {
    let mut shelf = Shelf::new();
    assert_eq!(shelf.put_fragment(video_only(0, 400)), 0);
    // Звук того же времени — другая дорожка: не повтор видео.
    assert_eq!(shelf.put_fragment(audio_only(0, 416)), 0);
    assert_eq!(shelf.starts_ms(), [0, 0]);

    // Новое видео того же времени заменяет только видео.
    assert_eq!(shelf.put_fragment(video_only(0, 400)), 1);
    assert_eq!(shelf.starts_ms(), [0, 0]);
    assert_eq!(shelf.media_bytes(), 2);
}

#[test]
fn a_muxed_neighbour_continues_the_running_episode() {
    let mut shelf = Shelf::new();
    let rules = rules();
    shelf.put_fragment(muxed((0, 400), (0, 416)));
    shelf.put_fragment(muxed((400, 800), (416, 810)));
    let first = shelf.shelf.next_to_feed(None, &rules).expect("first");
    let fed = shelf.shelf.take_for_feeding(first, &rules).expect("fed");

    // Видео следующего начинается на 16 мс раньше конца звука поданного, но у каждой
    // дорожки время идёт вперёд: тот же эпизод, без нового демуксера.
    let next = shelf
        .shelf
        .next_to_feed(Some(&fed.fed_to), &rules)
        .expect("the neighbour continues");
    assert_eq!(next.continuity(), EpisodeContinuity::MayContinue);
}

#[test]
fn a_track_going_back_still_starts_a_new_episode() {
    let mut shelf = Shelf::new();
    let rules = rules();
    shelf.put_fragment(muxed((0, 400), (0, 416)));
    let first = shelf.shelf.next_to_feed(None, &rules).expect("first");
    let fed = shelf.shelf.take_for_feeding(first, &rules).expect("fed");
    // Другая нарезка: звук начинается на 36 мс раньше конца поданного звука — это
    // повтор звука (поданный сегмент снят с полки, его дочитает труба).
    assert_eq!(shelf.put_fragment(muxed((400, 800), (380, 810))), 1);

    let next = shelf
        .shelf
        .next_to_feed(Some(&fed.fed_to), &rules)
        .expect("the straddling fragment is fed");
    assert_eq!(next.continuity(), EpisodeContinuity::MustRestart);
}

#[test]
fn audio_after_video_of_the_same_time_continues_the_episode() {
    let mut shelf = Shelf::new();
    let rules = rules();
    shelf.put_fragment(video_only(0, 400));
    let first = shelf.shelf.next_to_feed(None, &rules).expect("video");
    let fed = shelf.shelf.take_for_feeding(first, &rules).expect("fed");
    shelf.put_fragment(audio_only(0, 416));

    // Звук ничего не уводит назад: у звука поданного ещё нет.
    let next = shelf
        .shelf
        .next_to_feed(Some(&fed.fed_to), &rules)
        .expect("the audio of the same time is fed");
    assert_eq!(next.continuity(), EpisodeContinuity::MayContinue);
}
