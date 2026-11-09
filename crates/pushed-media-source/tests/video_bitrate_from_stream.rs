//! «Вес» видео потока (`PushedStreamMonitor::video_bitrate_near_position`, решение 277)
//! на настоящем fMP4, прошедшем через `PushedStreamWriter`: смена качества посреди
//! потока меняет число, и оно равно независимому расчёту «байты фрагментов ÷ время».
//!
//! Независимый расчёт делается здесь, из самих фрагментов фикстур, а не через
//! внутренности crate-а. Время фрагмента известно из фикстур: 25 к/с, по 5 кадров
//! во фрагменте = 0,2 с (20 кадров = 0,8 с на 4 фрагмента; ту же разметку берёт
//! `stream_changes_to_frame.rs`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};

/// Размер куска: не кратен ни одному боксу, режет их посреди.
const CHUNK: usize = 777;
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Длительность одного фрагмента фикстур: 5 кадров по 40 мс.
const FRAGMENT_SECONDS: f64 = 5.0 * 0.04;
/// Допустимое расхождение с независимым расчётом.
const TOLERANCE: f64 = 0.01;

fn feed(writer: &mut PushedStreamWriter, lane: LaneId, bytes: &[u8]) {
    for chunk in bytes.chunks(CHUNK) {
        writer.append(lane, chunk).expect("chunk accepted");
    }
}

/// Бит/с по независимому расчёту: байты фрагментов ÷ их время.
fn expected_bits_per_second(fragments: &[Vec<u8>]) -> f64 {
    let bytes: usize = fragments.iter().map(Vec::len).sum();
    let seconds = FRAGMENT_SECONDS * f64::from(u32::try_from(fragments.len()).expect("few"));
    f64::from(u32::try_from(bytes).expect("small")) * 8.0 / seconds
}

#[allow(
    clippy::cast_precision_loss,
    reason = "бит/с фикстуры далеко ниже 2^52"
)]
fn assert_close(actual: u64, expected: f64, what: &str) {
    let actual = actual as f64;
    assert!(
        (actual - expected).abs() <= expected * TOLERANCE,
        "{what}: got {actual} bit/s, expected {expected} bit/s (±1 %)"
    );
}

#[test]
fn video_bitrate_follows_a_quality_switch_and_matches_segment_bytes_over_time() {
    let low = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let high = FragmentedMp4::split(&own_fixture("video-h264-96.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac.mp4"));
    // Окно 0,3 с: на позиции 0,3 с в него попадают фрагменты 0–1, на 0,7 с — 2–3.
    let policy = PushedSourcePolicy::recommended()
        .expect("policy")
        .with_bitrate_window(Duration::from_millis(300))
        .expect("window");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(Duration::from_millis(800)));
    let monitor = writer.monitor();

    // До данных и позиции число неизвестно.
    assert_eq!(monitor.video_bitrate_near_position(), None);

    feed(&mut writer, VIDEO, &low.stream_of(0..2));
    feed(&mut writer, AUDIO, &audio.stream_of(0..2));
    monitor.note_playback_position(Duration::from_millis(300));
    let before_switch = monitor
        .video_bitrate_near_position()
        .expect("bitrate before the switch")
        .bits_per_second();
    assert_close(
        before_switch,
        expected_bits_per_second(&low.fragments[0..2]),
        "before the quality switch",
    );

    // Страница переключила качество: новый init (96×96) и фрагменты 2–3.
    feed(&mut writer, VIDEO, &high.stream_of(2..4));
    feed(&mut writer, AUDIO, &audio.fragments[2..].concat());
    monitor.note_playback_position(Duration::from_millis(700));
    let after_switch = monitor
        .video_bitrate_near_position()
        .expect("bitrate after the switch")
        .bits_per_second();
    assert_close(
        after_switch,
        expected_bits_per_second(&high.fragments[2..4]),
        "after the quality switch",
    );

    assert_ne!(
        before_switch, after_switch,
        "64x64 and 96x96 fixtures weigh differently"
    );
    // Опрос ничего не сдвинул: то же число при повторе.
    assert_eq!(
        monitor
            .video_bitrate_near_position()
            .map(pushed_media_source::MediaBitrate::bits_per_second),
        Some(after_switch)
    );
}
