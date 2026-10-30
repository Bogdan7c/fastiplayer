//! Короткий поток со звуком, где видео так и не дало ни одного кадра, — до конца.
//!
//! Так выглядит битый или недокачанный поток: пакеты видео есть, декодер их принимает,
//! но картинки не выдаёт, а звук короче запаса ожидания позднего видео (10 с). Раньше старт ждал первый кадр: поток кончался раньше, чем запас звука
//! исчерпывался, защёлка «звук первым» в `Draining` не ставилась, и плеер навсегда
//! оставался в `Draining` с позицией 0 и без звука.
//!
//! Решение владельца: если картинка не пришла — играет звук. Проверяем то, что слышит
//! зритель: звук записан целиком, без пауз, поток доигран до `Ended`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use player_core::PlaybackState;
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{open, play};

/// Длительность фрагмента фикстур: 10 фрагментов по 0,4 с.
const FRAGMENT: Duration = Duration::from_millis(400);
/// Фрагментов в фикстурах (4 с).
const FRAGMENT_COUNT: usize = 10;
/// Звук не короче записанного в поток минус один AAC-фрейм с запасом.
const AUDIO_LENGTH_TOLERANCE: Duration = Duration::from_millis(100);
/// Шаг «звуковой карты» и плеера — 5 мс; на старте выход может разок опустеть.
const AUDIO_SILENCE_TOLERANCE: Duration = Duration::from_millis(10);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Видеодорожка, из которой декодер не получит ни одной картинки.
///
/// Заголовок (`moov`) и разметка фрагментов (`moof`) настоящие, а полезная нагрузка
/// кадров (`mdat`) обнулена: демуксер честно отдаёт пакеты видео, декодер их
/// принимает и молчит, ошибки нет. Так выглядит битый или недокачанный поток.
fn video_lane_without_decodable_frames() -> Vec<u8> {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let mut lane_bytes = video.initialization.clone();
    for fragment in &video.fragments {
        let moof_size = u32::from_be_bytes(fragment[0..4].try_into().expect("moof size"));
        let moof_size = usize::try_from(moof_size).expect("moof size fits usize");
        // Заголовок `mdat` (8 байт) оставляем, обнуляем только сжатые кадры.
        let payload_start = moof_size + 8;
        let mut broken_fragment = fragment.clone();
        broken_fragment[payload_start..].fill(0);
        lane_bytes.extend_from_slice(&broken_fragment);
    }
    lane_bytes
}

/// Звук 4 с (меньше запаса ожидания видео 10 с), видео без единого декодируемого
/// кадра: звук играет до конца, поток доходит до `Ended`.
#[test]
fn audio_plays_to_the_end_when_the_video_never_yields_a_frame() {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    let stream_length = FRAGMENT * FRAGMENT_COUNT as u32;
    writer.set_duration(PushedDuration::Finite(stream_length));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    writer
        .append(VIDEO, &video_lane_without_decodable_frames())
        .expect("video bytes");
    writer
        .append(AUDIO, &audio.stream_of(0..FRAGMENT_COUNT))
        .expect("audio bytes");
    writer.end_of_stream().expect("end of stream");

    let report = play(open(&mut opener), |_| {});

    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(
        report.final_state,
        PlaybackState::Ended,
        "without a picture the audio must play to the end, not hang in Draining"
    );
    assert!(report.frames.is_empty(), "there is no frame to show");
    assert!(
        report.audio_duration() + AUDIO_LENGTH_TOLERANCE >= stream_length,
        "all audio must be heard: {:?} of {stream_length:?}",
        report.audio_duration()
    );
    assert!(
        report.mid_stream_audio_silence <= AUDIO_SILENCE_TOLERANCE,
        "audio paused mid-stream for {:?}",
        report.mid_stream_audio_silence
    );
}
