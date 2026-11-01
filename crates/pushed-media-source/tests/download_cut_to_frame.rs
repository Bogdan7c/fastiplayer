//! Досмотр после ухода страницы кончается на конце первой кончившейся дорожки
//! (решение владельца 133) — на настоящем плеере, до кадра в рендере и звука в выходе.
//!
//! Живой показ 04b: YouTube скачал видео до 0:48, звук до 1:00; вкладку закрыли —
//! картинка встала на 0:48, а звук и ползунок шли ещё 12 с, и на «доиграно» позиция
//! прыгнула в длительность фильма. Теперь приложение говорит источнику «скачивание
//! оборвано» (`end_download_cut_short`): картинка и звук встают вместе на конце видео,
//! а позиция остаётся там, где встал показ.
//!
//! Модель в масштабе фикстур: видео скачано на 1,2 с (3 фрагмента по 0,4 с), звук — на
//! 4,05 с (весь), длительность фильма — 10 с. Честный конец потока (`end_of_stream`)
//! с теми же байтами доигрывает весь звук и ставит позицию в длительность — как было.

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
    DownloadCutOutcome, LaneId, PlayableEnd, PushedContainer, PushedDuration, PushedSourcePolicy,
    PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, open, play_observed};

const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Скачанных фрагментов видео (по 0,4 с).
const DOWNLOADED_VIDEO_FRAGMENTS: usize = 3;
/// Конец скачанного видео.
const VIDEO_END: Duration = Duration::from_millis(1_200);
/// Конец скачанного звука (весь звук фикстуры).
const AUDIO_END: Duration = Duration::from_micros(4_053_333);
/// Длительность фильма, которую объявила страница: больше любой дорожки.
const MOVIE_DURATION: Duration = Duration::from_secs(10);
/// Длительность кадра фикстуры (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Допуск конца звука: AAC-кадр (21,3 мс) с запасом на шаг плеера.
const AUDIO_END_TOLERANCE: Duration = Duration::from_millis(60);
/// Допуск позиции на `Ended`: показ встаёт на конце доступного ± шаг обвязки.
const POSITION_TOLERANCE: Duration = Duration::from_millis(50);
/// Когда «уходит вкладка» в сценарии обрыва: посреди просмотра скачанного видео.
const TAB_LEAVES_AT: Duration = Duration::from_millis(300);

/// Поток, где видео скачано меньше звука. Вход кончает `end_input`, когда позиция
/// плеера дошла до `end_input_at` (как уход вкладки посреди просмотра): к этому
/// моменту плеер уже прочитал звук впрок дальше конца видео — сборка «видео + звук»
/// пускает его на 3 с вперёд, пока видео ждёт байтов. Позиция, как у приложения,
/// идёт монитору источника.
fn shorter_video_than_audio(
    end_input_at: Duration,
    end_input: impl FnOnce(&mut PushedStreamWriter),
) -> PlaybackReport {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(MOVIE_DURATION));
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    writer
        .append(VIDEO, &video.stream_of(0..DOWNLOADED_VIDEO_FRAGMENTS))
        .expect("video bytes");
    writer
        .append(AUDIO, &audio.stream_of(0..audio.fragments.len()))
        .expect("audio bytes");
    let monitor = writer.monitor();
    let demuxer = open(&mut opener);
    let mut end_input = Some(end_input);
    let report = play_observed(demuxer, |observation| {
        monitor.note_playback_position(observation.position);
        if observation.position >= end_input_at
            && let Some(end_input) = end_input.take()
        {
            end_input(&mut writer);
        }
    });
    assert!(end_input.is_none(), "the input was ended during playback");
    // Писатель живёт до конца прогона, как у приложения до конца доигрывания.
    drop(writer);
    report
}

/// Время последнего показанного кадра.
fn last_presented_frame(report: &PlaybackReport) -> Duration {
    report
        .frames
        .iter()
        .map(|frame| frame.pts)
        .max()
        .expect("frames reached the render")
}

#[test]
fn a_cut_short_download_stops_picture_and_sound_together_without_jumping_to_the_duration() {
    let report = shorter_video_than_audio(TAB_LEAVES_AT, |writer| {
        let outcome = writer.end_download_cut_short().expect("download cut short");
        let DownloadCutOutcome::CutShort(report) = outcome else {
            panic!("the stream input was still open: {outcome:?}");
        };
        assert_eq!(report.playable_end, PlayableEnd::At(VIDEO_END));
    });

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    // Картинка дошла до конца скачанного видео: последний кадр — в рендере.
    assert_eq!(
        last_presented_frame(&report),
        VIDEO_END - FRAME,
        "the last downloaded frame reaches the render"
    );
    // Звук кончился вместе с картинкой, а не на 4 с.
    let audio = report.heard_audio;
    assert!(
        audio <= VIDEO_END + AUDIO_END_TOLERANCE,
        "audio must stop at the end of the video, played {audio:?}"
    );
    assert!(
        audio >= VIDEO_END - AUDIO_END_TOLERANCE,
        "audio plays up to the end of the video, played {audio:?}"
    );
    // Ползунок остался там, где встал показ, а не прыгнул в длительность фильма.
    assert!(
        report.final_position.abs_diff(VIDEO_END) <= POSITION_TOLERANCE,
        "the position stays where playback stopped: {:?}",
        report.final_position
    );
}

#[test]
fn an_honest_end_of_stream_still_plays_all_audio_and_ends_at_the_duration() {
    let report = shorter_video_than_audio(Duration::ZERO, |writer| {
        writer.end_of_stream().expect("honest end of stream");
    });

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(last_presented_frame(&report), VIDEO_END - FRAME);
    let audio = report.heard_audio;
    assert!(
        audio >= AUDIO_END - AUDIO_END_TOLERANCE,
        "the whole downloaded audio plays after an honest end, played {audio:?}"
    );
    assert_eq!(
        report.final_position, MOVIE_DURATION,
        "an honest end keeps today's position: the media duration"
    );
}
