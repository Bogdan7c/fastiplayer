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

// --- Замена и время декодирования (сессия 16b2 моста, ролики TikTok) ---
//
// Видео с B-кадрами, кадр 40 мс, показ отстаёт от декодирования на два кадра. Фрагменты
// разрезаны посреди группы кадров: P остался в предыдущем, его B — в следующем, поэтому
// отрезки показа соседей накладываются на 160 мс, а декодирование идёт встык.

/// Фрагмент видео с записанным временем декодирования: (показ, декодирование), мс.
struct ReorderedVideo {
    presentation: (i64, i64),
    decode: (i64, i64),
}

/// Первый фрагмент ролика: 13 кадров, последний по декодированию — кадр P.
const FIRST: ReorderedVideo = ReorderedVideo {
    presentation: (80, 680),
    decode: (0, 520),
};
/// Его продолжение: начинается с B-кадров, показываемых раньше того кадра P.
const CONTINUATION: ReorderedVideo = ReorderedVideo {
    presentation: (520, 1_240),
    decode: (520, 1_040),
};

impl Shelf {
    /// Кладёт фрагмент видео с записанным временем декодирования и ключом `key`;
    /// возвращает число снятых.
    fn put_reordered(&mut self, fragment: &ReorderedVideo, key: EpisodeKey) -> usize {
        let (timing, tracks) = video_only(fragment.presentation.0, fragment.presentation.1);
        let (decode_timing, decode_tracks) = video_only(fragment.decode.0, fragment.decode.1);
        let decode = DecodeTiming::follows_presentation(decode_timing, decode_tracks);
        self.put_segment(timing, tracks, decode, key, 1)
    }

    /// Ключ другого кодирования: свой init (страница сменила качество).
    fn another_rendition(&self) -> EpisodeKey {
        let init = ChargedSegment::charge(&self.ledger, Bytes::from_static(b"next")).expect("room");
        EpisodeKey::new(
            PushedContainer::FragmentedMp4,
            Some(init),
            TimestampOffset::ZERO,
        )
    }
}

#[test]
fn a_fragment_cut_inside_a_group_of_pictures_does_not_replace_its_predecessor() {
    let mut shelf = Shelf::new();
    let key = shelf.key.clone();
    assert_eq!(shelf.put_reordered(&FIRST, key.clone()), 0);

    // Показ накладывается на 160 мс (> допуска 10 мс), но общих кадров нет: декодирование
    // продолжения начинается ровно там, где кончилось у первого. Оба остаются.
    assert_eq!(shelf.put_reordered(&CONTINUATION, key), 0);

    assert_eq!(shelf.starts_ms(), [80, 520]);
    assert_eq!(shelf.media_bytes(), 2);
}

#[test]
fn a_continuation_that_arrived_first_is_not_replaced_by_its_predecessor() {
    let mut shelf = Shelf::new();
    let key = shelf.key.clone();
    // Порядок прихода обратный (страница докачала начало позже): правило симметрично.
    assert_eq!(shelf.put_reordered(&CONTINUATION, key.clone()), 0);
    assert_eq!(shelf.put_reordered(&FIRST, key), 0);
    assert_eq!(shelf.starts_ms(), [80, 520]);
}

#[test]
fn a_repeat_of_a_fragment_with_reordered_frames_still_replaces_it() {
    let mut shelf = Shelf::new();
    let key = shelf.key.clone();
    shelf.put_reordered(&FIRST, key.clone());
    shelf.put_reordered(&CONTINUATION, key.clone());

    // Страница прислала продолжение ещё раз: то же время показа И декодирования — повтор.
    // Снят только он сам: первый фрагмент делит с ним показ, но не кадры.
    assert_eq!(shelf.put_reordered(&CONTINUATION, key), 1);

    assert_eq!(shelf.starts_ms(), [80, 520]);
    assert_eq!(shelf.media_bytes(), 2, "the repeat replaced, not doubled");
}

#[test]
fn another_cut_of_the_same_frames_still_replaces_what_it_covers() {
    let mut shelf = Shelf::new();
    let key = shelf.key.clone();
    shelf.put_reordered(&FIRST, key.clone());
    shelf.put_reordered(&CONTINUATION, key.clone());

    // Те же кадры другой нарезкой: фрагмент начинается посреди первого и кончается посреди
    // продолжения — общее время декодирования есть с обоими, оба сняты.
    let recut = ReorderedVideo {
        presentation: (240, 960),
        decode: (240, 800),
    };
    assert_eq!(shelf.put_reordered(&recut, key), 2);
    assert_eq!(shelf.starts_ms(), [240]);
}

#[test]
fn another_rendition_of_the_same_time_replaces_whatever_its_decode_time() {
    let mut shelf = Shelf::new();
    let key = shelf.key.clone();
    // Однокадровый фрагмент (низкая задержка): показ 240–280 мс, декодирование 160–200 мс.
    let frame = ReorderedVideo {
        presentation: (240, 280),
        decode: (160, 200),
    };
    shelf.put_reordered(&frame, key);

    // Тот же кадр в другом качестве: у этого кодирования задержка показа на кадр меньше,
    // и время декодирования с лежащим не пересекается. Оси декодирования разных
    // кодирований несравнимы — решает показ: замена.
    let other_rendition = ReorderedVideo {
        presentation: (240, 280),
        decode: (200, 240),
    };
    let other_key = shelf.another_rendition();
    assert_eq!(shelf.put_reordered(&other_rendition, other_key), 1);
    assert_eq!(shelf.starts_ms(), [240]);
}

#[test]
fn another_rendition_cut_inside_a_group_of_pictures_replaces_by_presentation_time() {
    let mut shelf = Shelf::new();
    let key = shelf.key.clone();
    shelf.put_reordered(&FIRST, key);

    // Те же отрезки, что у продолжения, но init другой: это не продолжение лежащего
    // (его кадры от того кодирования не декодируются), а новое качество поверх старого.
    let other_key = shelf.another_rendition();
    assert_eq!(shelf.put_reordered(&CONTINUATION, other_key), 1);
    assert_eq!(shelf.starts_ms(), [520]);
}

#[test]
fn without_recorded_decode_time_overlapping_presentation_still_replaces() {
    let mut shelf = Shelf::new();
    // WebM: времени декодирования нет, им служит время показа — правило прежнее.
    assert_eq!(shelf.put_fragment(video_only(80, 680)), 0);
    assert_eq!(shelf.put_fragment(video_only(520, 1_240)), 1);
    assert_eq!(shelf.starts_ms(), [520]);
}

#[test]
fn a_kept_continuation_feeds_after_its_predecessor_in_the_same_episode() {
    let mut shelf = Shelf::new();
    let rules = rules();
    let key = shelf.key.clone();
    shelf.put_reordered(&FIRST, key.clone());
    shelf.put_reordered(&CONTINUATION, key);
    let first = shelf.shelf.next_to_feed(None, &rules).expect("first");
    let fed = shelf.shelf.take_for_feeding(first, &rules).expect("fed");
    assert_eq!(fed.span.start(), at(80));

    // Продолжение идёт тому же демуксеру: новый эпизод ждал бы ключевого кадра, а его
    // в продолжении нет.
    let next = shelf
        .shelf
        .next_to_feed(Some(&fed.fed_to), &rules)
        .expect("the continuation is fed");
    assert_eq!(next.continuity(), EpisodeContinuity::MayContinue);
    let fed = shelf.shelf.take_for_feeding(next, &rules).expect("fed");
    assert_eq!(fed.span.start(), at(520));
}
