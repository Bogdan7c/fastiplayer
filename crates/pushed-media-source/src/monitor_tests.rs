//! Тесты монитора: «с какого момента есть данные всех полос» (сессия 03e).
//!
//! Фикстура протокола `video.mp4`: init — первые 733 байта, затем два фрагмента
//! (0 и 0,2 с); `audio.mp4` — звук с нуля.

use std::time::Duration;

use crate::container::PushedContainer;
use crate::lane::LaneId;
use crate::policy::PushedSourcePolicy;
use crate::stream::{PushedStreamWriter, pushed_stream};
use crate::time::TimestampOffset;

const VIDEO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/video.mp4");
const AUDIO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/audio.mp4");
const MUXED_TS: &[u8] = include_bytes!("../fixtures/muxed-h264-aac.ts");
/// Конец init-сегмента `VIDEO_MP4`.
const VIDEO_INIT_END: usize = 733;
/// Конец первого фрагмента `VIDEO_MP4`: дальше — фрагмент с 0,2 с.
const VIDEO_FIRST_FRAGMENT_END: usize = 5714;
/// Начало второго фрагмента видео.
const SECOND_VIDEO_FRAGMENT_START: Duration = Duration::from_millis(200);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Поток с двумя полосами fMP4 (видео и звук), байтов ещё нет.
fn two_lane_stream() -> PushedStreamWriter {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer
}

/// Видео только со второго фрагмента: init + фрагмент с 0,2 с.
fn append_video_from_second_fragment(writer: &mut PushedStreamWriter) {
    writer
        .append(VIDEO, &VIDEO_MP4[..VIDEO_INIT_END])
        .expect("video init");
    writer
        .append(VIDEO, &VIDEO_MP4[VIDEO_FIRST_FRAGMENT_END..])
        .expect("second video fragment");
}

#[test]
fn without_bytes_there_is_no_common_start() {
    let writer = two_lane_stream();
    assert_eq!(writer.monitor().earliest_common_start(), None);
}

#[test]
fn one_empty_lane_means_no_common_start_yet() {
    let mut writer = two_lane_stream();
    writer.append(VIDEO, VIDEO_MP4).expect("video");
    // Видео лежит с нуля, но звука нет: плеер всё равно ждал бы звук.
    assert_eq!(writer.monitor().earliest_common_start(), None);
}

#[test]
fn common_start_is_the_latest_lane_start() {
    let mut writer = two_lane_stream();
    // Звук с нуля, видео с 0,2 с: общий старт — там, где есть и то и другое.
    writer.append(AUDIO, AUDIO_MP4).expect("audio");
    append_video_from_second_fragment(&mut writer);
    assert_eq!(
        writer.monitor().earliest_common_start(),
        Some(SECOND_VIDEO_FRAGMENT_START)
    );
}

#[test]
fn timestamp_offset_moves_the_common_start() {
    let mut writer = two_lane_stream();
    let offset = Duration::from_secs(40);
    for lane in [VIDEO, AUDIO] {
        writer
            .set_timestamp_offset(lane, TimestampOffset::from_micros(40_000_000))
            .expect("offset");
    }
    writer.append(AUDIO, AUDIO_MP4).expect("audio");
    writer.append(VIDEO, VIDEO_MP4).expect("video");
    // Как новое поколение YouTube: байты лежат с 40 с, а не с нуля.
    assert_eq!(writer.monitor().earliest_common_start(), Some(offset));
}

#[test]
fn time_before_zero_is_reported_as_zero() {
    let mut writer = two_lane_stream();
    for lane in [VIDEO, AUDIO] {
        writer
            .set_timestamp_offset(lane, TimestampOffset::from_micros(-100_000))
            .expect("offset");
    }
    writer.append(AUDIO, AUDIO_MP4).expect("audio");
    writer.append(VIDEO, VIDEO_MP4).expect("video");
    assert_eq!(
        writer.monitor().earliest_common_start(),
        Some(Duration::ZERO)
    );
}

#[test]
fn transport_stream_without_box_times_has_no_common_start() {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::MpegTs)
        .expect("ts lane");
    writer.append(VIDEO, MUXED_TS).expect("ts bytes");
    assert_eq!(writer.monitor().earliest_common_start(), None);
}

#[test]
fn asking_for_the_common_start_changes_nothing_in_the_buffer() {
    let mut writer = two_lane_stream();
    writer.append(AUDIO, AUDIO_MP4).expect("audio");
    append_video_from_second_fragment(&mut writer);
    let monitor = writer.monitor();
    let (status_before, held_before) = (monitor.buffer_status(), monitor.held_segment_bytes());
    for _ in 0..3 {
        assert!(monitor.earliest_common_start().is_some());
    }
    assert_eq!(monitor.buffer_status(), status_before);
    assert_eq!(monitor.held_segment_bytes(), held_before);
    // И никакой просьбы к странице: монитор только смотрит на полки.
    assert_eq!(monitor.take_page_seek_request(), None);
}
