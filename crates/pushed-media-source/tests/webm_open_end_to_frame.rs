//! WebM без длительностей кадров «до кадра»: малая частота кадров не встаёт на
//! «дыре» между кластерами, перемотка в последний кадр кластера садится.
//!
//! Фикстура `video-vp9-64-4fps.webm`: VP9 64×64, 4 к/с, 4 с, кластер на каждую
//! секунду (4 блока по 250 мс), `SimpleBlock` без `BlockDuration`. ffmpeg пишет
//! `DefaultDuration` — тест заменяет его на `Void` того же размера: так выглядят
//! WebM плееров страниц, и тогда конец кластера НЕ записан нигде. Раньше полка
//! считала концом кластера начало его последнего блока: дыра в кадр (250 мс)
//! больше наибольшей дыры полки (100 мс), и подача вставала после первого кластера.

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
    LaneId, PageSeekRequest, PushedContainer, PushedDuration, PushedSourcePolicy,
    PushedStreamWriter, pushed_stream,
};
use support::media::own_fixture;
use support::playback::{PlaybackReport, open, play_controlled};

/// Длительность кадра фикстуры (4 к/с).
const FRAME: Duration = Duration::from_millis(250);
/// Длительность ролика.
const CLIP: Duration = Duration::from_secs(4);
const VIDEO: LaneId = LaneId::new(0);
/// `TrackEntry/DefaultDuration`: ID и поле размера «4 байта» — как пишет ffmpeg.
const DEFAULT_DURATION_HEADER: [u8; 4] = [0x23, 0xE3, 0x83, 0x84];
/// Весь элемент `DefaultDuration`: заголовок и 4 байта значения.
const DEFAULT_DURATION_ELEMENT_LEN: usize = 8;
/// `Void` того же размера: ID `0xEC`, размер 6, шесть нулей.
const VOID_OF_DEFAULT_DURATION: [u8; 8] = [0xEC, 0x86, 0, 0, 0, 0, 0, 0];

/// Фикстура без `DefaultDuration`: длительностей кадров в файле нет вовсе.
fn webm_without_frame_durations() -> Vec<u8> {
    let mut file = own_fixture("video-vp9-64-4fps.webm");
    let positions: Vec<usize> = file
        .windows(DEFAULT_DURATION_HEADER.len())
        .enumerate()
        .filter(|(_, window)| *window == DEFAULT_DURATION_HEADER)
        .map(|(position, _)| position)
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "exactly one DefaultDuration in the fixture"
    );
    let element = positions[0]..positions[0] + DEFAULT_DURATION_ELEMENT_LEN;
    file[element].copy_from_slice(&VOID_OF_DEFAULT_DURATION);
    file
}

/// Поток с одной полосой WebM, все байты и конец потока уже пришли.
///
/// Писателя тест держит до конца прогона (его `Drop` — тоже конец потока).
fn prepared_stream() -> (PreparedMedia, PushedStreamWriter) {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer.add_lane(VIDEO, PushedContainer::WebM).expect("lane");
    writer.set_duration(PushedDuration::Finite(CLIP));
    writer
        .append(VIDEO, &webm_without_frame_durations())
        .expect("whole file");
    writer.end_of_stream().expect("end");
    let prepared =
        PreparedMedia::from_external_label("pushed webm test", Box::new(open(&mut opener)));
    (prepared, writer)
}

fn presented(report: &PlaybackReport) -> Vec<Duration> {
    report.frames.iter().map(|frame| frame.pts).collect()
}

#[test]
fn low_frame_rate_webm_without_frame_durations_plays_through() {
    let (prepared, _writer) = prepared_stream();
    let report = play_controlled(prepared, |_| {});
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    let expected: Vec<Duration> = (0..16).map(|index| FRAME * index).collect();
    assert_eq!(presented(&report), expected);
}

#[test]
fn seek_into_the_last_frame_of_a_cluster_lands_from_the_shelf() {
    let (prepared, writer) = prepared_stream();
    let monitor = writer.monitor();
    // Последний кадр второго кластера: 1750–2000 мс, его конец не записан.
    let target = Duration::from_millis(1_875);
    let mut requests: Vec<PageSeekRequest> = Vec::new();
    let mut seeked = false;
    let report = play_controlled(prepared, |control| {
        requests.extend(monitor.take_page_seek_request());
        if !seeked && control.observation().position >= FRAME {
            seeked = true;
            control.seek(target);
        }
    });
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(requests, [], "the target is on the shelf");
    let (seek_tick, _) = report.seeks[0];
    let after: Vec<Duration> = report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .map(|frame| frame.pts)
        .collect();
    let first = *after.first().expect("frames after the seek");
    assert!(
        first.abs_diff(target) <= FRAME,
        "first frame after the seek at {first:?}"
    );
    assert!(after.windows(2).all(|pair| pair[0] < pair[1]), "{after:?}");
    assert_eq!(after.last(), Some(&(CLIP - FRAME)));
}
