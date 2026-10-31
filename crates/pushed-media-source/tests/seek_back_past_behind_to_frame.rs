//! «Помним, пока помнит страница» до кадра: перемотка зрителя назад дальше окна
//! «назад» у страницы, которая уже отданное повторно НЕ шлёт (как YouTube: у её
//! настоящего MSE `SourceBuffer` этот кусок ещё лежит, качать его заново ей
//! незачем).
//!
//! Живая находка 04a: полка держала прошлое только `behind` (30 с), перемотка
//! 5:07 → 3:30 уходила к странице, страница перематывалась и не присылала ни
//! байта — картинка висела до срока перемотки плеера. Здесь то же в масштабе
//! фикстуры: окно «назад» 0,4 с, сыграно 2,8 с, перемотка на 0,52 с.
//!
//! Настоящие декодеры видео и звука, настоящее время (обвязка `support/playback`).
//! Фикстуры: `video-h264-64-4s.mp4` (4 с, 25 к/с, десять фрагментов по 0,4 с,
//! каждый с ключевого кадра) и `audio-aac-4s.mp4`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use player_core::{PlaybackState, PreparedMedia};
use pushed_media_source::{
    BufferWindow, LaneId, PageSeekRequest, PushedContainer, PushedDuration, PushedSourcePolicy,
    pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, open, play_controlled};

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Первый кадр после перемотки — у цели, раньше цели ничего, дальше — по порядку
/// и до конца ролика, звук вровень с видео.
fn assert_landed_and_played_through(report: &PlaybackReport, target: Duration) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let (seek_tick, _) = report.seeks[0];
    let after: Vec<_> = report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .collect();
    let first = after.first().expect("frames after the seek");
    assert!(
        first.pts.abs_diff(target) <= FRAME,
        "first frame after the seek at {:?}, target {target:?}",
        first.pts
    );
    assert!(
        after.iter().all(|frame| frame.pts + FRAME >= target),
        "a frame from before the target was shown after the seek"
    );
    assert!(
        after
            .windows(2)
            .all(|pair| pair[0].pts < pair[1].pts && pair[1].pts - pair[0].pts <= FRAME * 2),
        "frames after the seek skip time or go backwards"
    );
    assert_eq!(after.last().map(|frame| frame.pts), Some(CLIP - FRAME));
    let worst = after
        .iter()
        .skip(1)
        .map(|frame| frame.pts.abs_diff(frame.position))
        .max()
        .unwrap_or_default();
    assert!(worst <= FRAME, "A/V offset after the seek {worst:?}");
}

#[test]
fn seek_back_further_than_behind_lands_from_the_shelf_when_the_page_never_resends() {
    let window = BufferWindow {
        ahead: Duration::from_secs(120),
        behind: Duration::from_millis(400),
    };
    let policy = PushedSourcePolicy::recommended()
        .expect("policy")
        .with_buffer_window(window)
        .expect("window");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(CLIP));
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    // Страница отдаёт всё один раз и больше не шлёт ничего: просьбу перемотаться
    // она бы исполнила, но байты у неё уже есть.
    writer
        .append(VIDEO, &video.initialization)
        .expect("video init");
    writer
        .append(AUDIO, &audio.initialization)
        .expect("audio init");
    for (video_fragment, audio_fragment) in video.fragments.iter().zip(&audio.fragments) {
        writer.append(VIDEO, video_fragment).expect("video");
        writer.append(AUDIO, audio_fragment).expect("audio");
    }
    writer.end_of_stream().expect("end of stream");

    let monitor = writer.monitor();
    // Назад дальше окна «назад» (0,4 с) с большим запасом: от 2,8 с к 0,52 с.
    let seek_from = Duration::from_millis(2_800);
    let target = Duration::from_millis(520);
    let mut requests: Vec<PageSeekRequest> = Vec::new();
    let mut seeked = false;
    let prepared = PreparedMedia::from_external_label(
        "pushed media seek back past behind",
        Box::new(open(&mut opener)),
    );
    let report = play_controlled(prepared, |control| {
        // Как приложение: позиция плеера → монитор источника каждый шаг.
        let position = control.observation().position;
        monitor.note_playback_position(position);
        requests.extend(monitor.take_page_seek_request());
        if !seeked && position >= seek_from {
            seeked = true;
            control.seek(target);
        }
    });

    assert!(seeked, "playback reached {seek_from:?}");
    assert_eq!(
        requests,
        [],
        "the shelf still holds what the page holds: no request to the page"
    );
    assert_landed_and_played_through(&report, target);
}
