//! Вес видео около позиции (`StreamBuffer::video_bitrate_near_position`, решение 277).
//!
//! Полки собираются синтетически (байты — нули, время — заданное); числа считаются
//! в голове: сегмент 1 с по 125 000 Б = 1 Мбит/с. Сквозная проверка на настоящем
//! потоке — `tests/video_bitrate_from_stream.rs`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use super::StreamBuffer;
use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::feed::EpisodeKey;
use crate::framing::{DecodeTiming, LaneContent, MediaTime, MediaTimeSpan, TimedSpan, TrackSpans};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::lane::{LaneId, LaneMailbox};
use crate::policy::{PushedSourcePolicy, PushedSourcePolicyError};
use crate::shelf::ShelfEntry;
use crate::stream::pushed_demux_registry;
use crate::time::TimestampOffset;

const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
const SECOND_VIDEO: LaneId = LaneId::new(2);
/// Байтов в секунде при 1 Мбит/с.
const ONE_MEGABIT_SEGMENT_BYTES: usize = 125_000;

fn at(milliseconds: i64) -> MediaTime {
    MediaTime::from_micros(milliseconds * 1_000)
}

/// Буфер с заданными полосами и политикой.
struct Bench {
    buffer: StreamBuffer,
    ledger: Arc<IntakeLedger>,
}

impl Bench {
    fn new(lanes: &[(LaneId, PushedContainer, LaneContent)]) -> Self {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        Self::with_policy(lanes, policy)
    }

    fn with_policy(
        lanes: &[(LaneId, PushedContainer, LaneContent)],
        policy: PushedSourcePolicy,
    ) -> Self {
        let ledger = Arc::new(IntakeLedger::new(64 * 1024 * 1024));
        let factory =
            EpisodeFactory::new(Arc::new(pushed_demux_registry().expect("registry")), policy);
        let buffer = StreamBuffer::new(Arc::clone(&ledger), factory, policy.stream_buffer_rules());
        for &(lane, container, content) in lanes {
            buffer.add_lane(lane, container, Arc::new(LaneMailbox::default()), 4);
            buffer.note_lane_content(lane, content).expect("lane");
        }
        Self { buffer, ledger }
    }

    fn video_only() -> Self {
        Self::new(&[(
            VIDEO,
            PushedContainer::FragmentedMp4,
            LaneContent::CarriesVideo,
        )])
    }

    /// Кладёт сегмент `[start_ms, end_ms)` в `bytes` байтов (без init: все такие — одно качество).
    fn shelve(&self, lane: LaneId, start_ms: i64, end_ms: i64, bytes: usize) {
        let key = EpisodeKey::new(PushedContainer::FragmentedMp4, None, TimestampOffset::ZERO);
        self.shelve_with_key(lane, start_ms, end_ms, bytes, key);
    }

    /// Init-сегмент качества: у сегментов разных init разный ключ эпизода (смена качества).
    fn initialization(&self, label: &'static [u8]) -> ChargedSegment {
        ChargedSegment::charge(&self.ledger, Bytes::from_static(label)).expect("room")
    }

    /// Секундные сегменты `[first, first + count)` с качества с init `init`.
    fn shelve_quality(&self, init: &ChargedSegment, first: i64, count: i64, bytes_each: usize) {
        for second in first..first + count {
            let key = EpisodeKey::new(
                PushedContainer::FragmentedMp4,
                Some(init.clone()),
                TimestampOffset::ZERO,
            );
            self.shelve_with_key(VIDEO, second * 1_000, (second + 1) * 1_000, bytes_each, key);
        }
    }

    fn shelve_with_key(
        &self,
        lane: LaneId,
        start_ms: i64,
        end_ms: i64,
        bytes: usize,
        key: EpisodeKey,
    ) {
        let media =
            ChargedSegment::charge(&self.ledger, Bytes::from(vec![0; bytes])).expect("room");
        let span = MediaTimeSpan::new(at(start_ms), at(end_ms)).expect("span");
        let mut state = self.buffer.lock();
        state
            .lanes
            .get_mut(&lane)
            .expect("lane")
            .shelf
            .insert(ShelfEntry::waiting(
                TimedSpan::exact(span),
                TrackSpans::default(),
                DecodeTiming::follows_presentation(TimedSpan::exact(span), TrackSpans::default()),
                media,
                key,
            ));
    }

    /// Секундные сегменты `[first, first + count)` с.
    fn shelve_seconds(&self, lane: LaneId, first: i64, count: i64, bytes_each: usize) {
        for second in first..first + count {
            self.shelve(lane, second * 1_000, (second + 1) * 1_000, bytes_each);
        }
    }

    fn play_at(&self, position_ms: i64) {
        self.buffer.lock().reported_position = Some(at(position_ms));
    }

    fn bits_per_second(&self) -> Option<u64> {
        self.buffer
            .video_bitrate_near_position()
            .map(crate::MediaBitrate::bits_per_second)
    }
}

#[test]
fn known_segments_give_the_exact_bitrate() {
    let bench = Bench::video_only();
    bench.shelve_seconds(VIDEO, 0, 4, ONE_MEGABIT_SEGMENT_BYTES);
    // Позиция 2 с: окно [-8; 2] берёт сегменты с началами 0, 1, 2 (под позицией — [2; 3)).
    bench.play_at(2_000);

    assert_eq!(bench.bits_per_second(), Some(1_000_000));
}

#[test]
fn window_follows_the_anchor_through_a_quality_switch() {
    let bench = Bench::video_only();
    // Смена качества — новый init в той же полосе: 0–12 с 200 кбит/с, 12–24 с 2 Мбит/с.
    let low = bench.initialization(b"init-360p");
    let high = bench.initialization(b"init-720p");
    bench.shelve_quality(&low, 0, 12, 25_000);
    bench.shelve_quality(&high, 12, 12, 250_000);

    bench.play_at(8_000);
    assert_eq!(bench.bits_per_second(), Some(200_000));

    // Последняя секунда старого качества: окно [1,5; 11,5] видит только его, новое — впереди.
    bench.play_at(11_500);
    assert_eq!(bench.bits_per_second(), Some(200_000));

    // Сразу после смены: в окне [4; 14] восемь секунд старого качества, но играет новое —
    // считаются только его сегменты [12; 15), и число сразу 2 Мбит/с, а не смесь (решение 277).
    bench.play_at(14_000);
    assert_eq!(bench.bits_per_second(), Some(2_000_000));

    bench.play_at(22_000);
    assert_eq!(bench.bits_per_second(), Some(2_000_000));
}

#[test]
fn segments_of_the_same_quality_are_averaged_across_the_window() {
    let bench = Bench::video_only();
    // Один init (одно качество), но тяжёлая сцена: 0–12 с по 25 000 Б, 12–24 с по 250 000 Б.
    bench.shelve_seconds(VIDEO, 0, 12, 25_000);
    bench.shelve_seconds(VIDEO, 12, 12, 250_000);

    // Окно [4; 14] = 8 лёгких сегментов (4..11) + 3 тяжёлых (12..14): среднее по окну.
    bench.play_at(14_000);
    let mixed = 8 * 25_000 + 3 * 250_000;
    let expected = u64::try_from(mixed * 8 / 11).expect("fits");
    assert_eq!(bench.bits_per_second(), Some(expected));
}

#[test]
fn bitrate_window_comes_from_the_policy() {
    let policy = PushedSourcePolicy::recommended()
        .expect("policy")
        .with_bitrate_window(Duration::from_secs(2))
        .expect("window");
    let bench = Bench::with_policy(
        &[(
            VIDEO,
            PushedContainer::FragmentedMp4,
            LaneContent::CarriesVideo,
        )],
        policy,
    );
    bench.shelve_seconds(VIDEO, 0, 12, 25_000);
    bench.shelve_seconds(VIDEO, 12, 12, 250_000);
    bench.play_at(14_000);

    // Окно [12; 14]: сегменты 12, 13, 14 — только высокое качество (при 10 с было бы смешанное).
    assert_eq!(bench.bits_per_second(), Some(2_000_000));
}

#[test]
fn zero_bitrate_window_is_rejected() {
    let result = PushedSourcePolicy::recommended()
        .expect("policy")
        .with_bitrate_window(Duration::ZERO);
    assert_eq!(
        result.err(),
        Some(PushedSourcePolicyError::ZeroBitrateWindow)
    );
}

#[test]
fn nothing_behind_the_anchor_falls_back_to_ahead() {
    let bench = Bench::video_only();
    bench.shelve_seconds(VIDEO, 5, 4, ONE_MEGABIT_SEGMENT_BYTES);
    bench.play_at(2_000);

    assert_eq!(bench.bits_per_second(), Some(1_000_000));
}

#[test]
fn nothing_in_either_direction_is_unknown() {
    let bench = Bench::video_only();
    // Единственный сегмент дальше окна впереди (позиция 0, окно 10 с, начало — 30 с).
    bench.shelve_seconds(VIDEO, 30, 1, ONE_MEGABIT_SEGMENT_BYTES);
    bench.play_at(0);

    assert_eq!(bench.bits_per_second(), None);
}

#[test]
fn without_any_position_the_bitrate_is_unknown() {
    let bench = Bench::video_only();
    bench.shelve_seconds(VIDEO, 0, 4, ONE_MEGABIT_SEGMENT_BYTES);

    assert_eq!(bench.bits_per_second(), None);
}

#[test]
fn audio_lane_is_ignored_next_to_a_video_lane() {
    let bench = Bench::new(&[
        (
            VIDEO,
            PushedContainer::FragmentedMp4,
            LaneContent::CarriesVideo,
        ),
        (
            AUDIO,
            PushedContainer::FragmentedMp4,
            LaneContent::WithoutVideo,
        ),
    ]);
    bench.shelve_seconds(VIDEO, 0, 4, ONE_MEGABIT_SEGMENT_BYTES);
    bench.shelve_seconds(AUDIO, 0, 4, 16_000);
    bench.play_at(2_000);

    assert_eq!(bench.bits_per_second(), Some(1_000_000));
}

#[test]
fn audio_only_stream_has_no_video_bitrate() {
    let bench = Bench::new(&[(
        AUDIO,
        PushedContainer::FragmentedMp4,
        LaneContent::WithoutVideo,
    )]);
    bench.shelve_seconds(AUDIO, 0, 4, 16_000);
    bench.play_at(2_000);

    assert_eq!(bench.bits_per_second(), None);
}

#[test]
fn untimed_lane_makes_the_bitrate_unknown() {
    let bench = Bench::new(&[
        (
            VIDEO,
            PushedContainer::FragmentedMp4,
            LaneContent::CarriesVideo,
        ),
        (AUDIO, PushedContainer::MpegTs, LaneContent::CarriesVideo),
    ]);
    bench.shelve_seconds(VIDEO, 0, 4, ONE_MEGABIT_SEGMENT_BYTES);
    bench.play_at(2_000);

    assert_eq!(bench.bits_per_second(), None);
}

#[test]
fn several_video_lanes_add_bytes_and_keep_the_longest_time() {
    let bench = Bench::new(&[
        (
            VIDEO,
            PushedContainer::FragmentedMp4,
            LaneContent::CarriesVideo,
        ),
        (
            SECOND_VIDEO,
            PushedContainer::FragmentedMp4,
            LaneContent::CarriesVideo,
        ),
    ]);
    bench.shelve_seconds(VIDEO, 0, 4, ONE_MEGABIT_SEGMENT_BYTES);
    // Вторая полоса покрывает только 2 с, но весит столько же за секунду.
    bench.shelve_seconds(SECOND_VIDEO, 0, 2, ONE_MEGABIT_SEGMENT_BYTES);
    bench.play_at(3_000);

    // Окно [-7; 3]: первая полоса — 4 сегмента за 4 с; вторая — 2 за 2 с.
    // Байты 6 × 125 000, время max(4; 2) = 4 с -> 1,5 Мбит/с.
    assert_eq!(bench.bits_per_second(), Some(1_500_000));
}

#[test]
fn touching_and_overlapping_segments_do_not_double_the_covered_time() {
    let bench = Bench::video_only();
    bench.shelve(VIDEO, 0, 1_000, ONE_MEGABIT_SEGMENT_BYTES);
    // Тот же отрезок ещё раз и наложение [0,5; 1,5): объединение = [0; 1,5) = 1,5 с.
    bench.shelve(VIDEO, 0, 1_000, ONE_MEGABIT_SEGMENT_BYTES);
    bench.shelve(VIDEO, 500, 1_500, ONE_MEGABIT_SEGMENT_BYTES);
    bench.play_at(1_200);

    // 3 × 125 000 Б × 8 / 1,5 с = 2 Мбит/с.
    assert_eq!(bench.bits_per_second(), Some(2_000_000));
}

#[test]
fn long_segment_reaching_the_anchor_is_found_despite_later_short_ones() {
    let bench = Bench::video_only();
    // Один сегмент на 30 с (3 000 000 Б) и короткие после него: поиск по началам
    // не должен потерять длинный, начало которого далеко позади окна.
    bench.shelve(VIDEO, 0, 30_000, 3_000_000);
    bench.shelve(VIDEO, 30_000, 31_000, ONE_MEGABIT_SEGMENT_BYTES);
    bench.play_at(25_000);

    assert_eq!(bench.bits_per_second(), Some(800_000));
}

#[test]
fn segment_without_frames_adds_no_bytes_to_the_weight() {
    let bench = Bench::video_only();
    bench.shelve_seconds(VIDEO, 0, 2, ONE_MEGABIT_SEGMENT_BYTES);
    bench.shelve(VIDEO, 1_000, 1_000, 999_999);
    bench.play_at(1_500);

    assert_eq!(bench.bits_per_second(), Some(1_000_000));
}

#[test]
fn the_query_changes_nothing() {
    let bench = Bench::video_only();
    bench.shelve_seconds(VIDEO, 0, 6, ONE_MEGABIT_SEGMENT_BYTES);
    bench.play_at(3_000);

    let snapshot = |bench: &Bench| {
        // Замок буфера берётся и отпускается до buffer_status (тот берёт его сам).
        let (layout, reported_position, first_fed_start) = {
            let state = bench.buffer.lock();
            (
                state.lanes[&VIDEO].shelf.layout(),
                state.reported_position,
                state.first_fed_start,
            )
        };
        (
            layout,
            reported_position,
            first_fed_start,
            bench.ledger.held_bytes(),
            bench.buffer.buffer_status(),
        )
    };
    let before = snapshot(&bench);
    let first = bench.bits_per_second();
    let second = bench.bits_per_second();
    let after = snapshot(&bench);

    assert_eq!(first, Some(1_000_000));
    assert_eq!(second, first);
    assert_eq!(before, after);
}
