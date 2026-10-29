//! Смена качества, смена кодека, смещение времени, `abort()` и MPEG-TS — до кадра.
//!
//! Потоки собираются из фикстур crate-а так, как их кормит MSE плеер страницы:
//! init-сегменты и фрагменты, нарезанные кусками посреди боксов.

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
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamWriter,
    TimestampOffset, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, open, play};

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Размер куска: не кратен ни одному боксу, режет их посреди.
const CHUNK: usize = 777;
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

fn frame_times(range: std::ops::Range<u32>) -> Vec<Duration> {
    range.map(|index| FRAME * index).collect()
}

fn presented_times(report: &PlaybackReport) -> Vec<Duration> {
    report.frames.iter().map(|frame| frame.pts).collect()
}

/// Кормит полосу кусками фиксированного размера.
fn feed(writer: &mut PushedStreamWriter, lane: LaneId, bytes: &[u8]) {
    for chunk in bytes.chunks(CHUNK) {
        writer.append(lane, chunk).expect("chunk accepted");
    }
}

fn new_stream() -> (PushedStreamWriter, pushed_media_source::PushedStreamOpener) {
    pushed_stream(PushedSourcePolicy::recommended().expect("policy")).expect("stream")
}

/// Проверка смены init-сегмента посреди воспроизведения (качество или кодек).
///
/// Сегодня плеер при `TracksChanged` сбрасывает декодер сразу, в момент разбора, и
/// кадры старого эпизода, уже декодированные впрок, не показываются (решение 94:
/// плавная смена — сессия 03d). Поэтому проверяется честное нынешнее поведение:
/// - старый эпизод показан с начала, по порядку, в старом размере;
/// - новый эпизод показан целиком, по порядку, в новом размере;
/// - ошибок нет, поток доигран до конца.
///
/// После 03d проверка станет строгой: все 20 кадров подряд.
fn assert_switch_without_stopping(
    report: &PlaybackReport,
    old_size: (u32, u32),
    new_size: (u32, u32),
) {
    assert_clean_end(report);
    let switch_at = FRAME * 10;
    let (old_frames, new_frames): (Vec<&support::playback::PresentedFrame>, Vec<_>) = report
        .frames
        .iter()
        .partition(|frame| frame.pts < switch_at);
    let old_times: Vec<Duration> = old_frames.iter().map(|frame| frame.pts).collect();
    assert!(old_times.len() >= 5, "old episode shown {old_times:?}");
    assert_eq!(
        old_times,
        frame_times(0..u32::try_from(old_times.len()).expect("small"))
    );
    assert!(
        old_frames
            .iter()
            .all(|frame| (frame.width, frame.height) == old_size)
    );
    let new_times: Vec<Duration> = new_frames.iter().map(|frame| frame.pts).collect();
    assert_eq!(new_times, frame_times(10..20));
    assert!(
        new_frames
            .iter()
            .all(|frame| (frame.width, frame.height) == new_size)
    );
}

fn assert_clean_end(report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
}

#[test]
fn quality_switch_mid_stream_changes_resolution_without_stopping() {
    let low = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let high = FragmentedMp4::split(&own_fixture("video-h264-96.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac.mp4"));
    let (mut writer, mut opener) = new_stream();
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(Duration::from_millis(800)));
    feed(&mut writer, VIDEO, &low.stream_of(0..2));
    feed(&mut writer, AUDIO, &audio.stream_of(0..2));

    // Вторая половина приходит во время воспроизведения, звук и видео вровень —
    // так кормит MSE плеер страницы. Новый init (96×96) начинает новый эпизод.
    let mut second_half = Some((high.stream_of(2..4), audio.fragments[2..].concat()));
    let report = play(open(&mut opener), |tick| {
        if tick == 40
            && let Some((video_bytes, audio_bytes)) = second_half.take()
        {
            feed(&mut writer, VIDEO, &video_bytes);
            feed(&mut writer, AUDIO, &audio_bytes);
            writer.end_of_stream().expect("end of stream");
        }
    });

    assert_switch_without_stopping(&report, (64, 64), (96, 96));
    assert!(report.audio_duration() >= Duration::from_millis(780));
    assert!(
        report.max_av_offset() <= FRAME,
        "A/V offset {:?}",
        report.max_av_offset()
    );
}

#[test]
fn late_bytes_make_the_player_wait_instead_of_ending() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let (mut writer, mut opener) = new_stream();
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    feed(&mut writer, VIDEO, &video.stream_of(0..2));
    // Вторая половина приходит через ~0,75 с (150 шагов по 5 мс) — позже, чем
    // кончаются первые 0,4 с: плеер обязан ждать, а не заканчивать поток.
    let mut late_fragments = Some(video.fragments[2..].concat());
    let report = play(open(&mut opener), |tick| {
        if tick == 150
            && let Some(bytes) = late_fragments.take()
        {
            feed(&mut writer, VIDEO, &bytes);
            writer.end_of_stream().expect("end of stream");
        }
    });
    assert_clean_end(&report);
    assert!(
        report.buffered_after_start,
        "the player must wait for late bytes"
    );
    assert_eq!(presented_times(&report), frame_times(0..20));
}

#[test]
fn change_type_from_h264_to_vp9_keeps_playing() {
    let h264 = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let vp9 = FragmentedMp4::split(&own_fixture("video-vp9-96.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac.mp4"));
    let (mut writer, mut opener) = new_stream();
    // Звуковая полоса, как у YouTube: старт ведёт звук, и до смены кодека плеер
    // уже играет. (Поток только с видео ждёт следующих байтов до старта — см. 03b.)
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    feed(&mut writer, VIDEO, &h264.stream_of(0..2));
    feed(&mut writer, AUDIO, &audio.stream_of(0..2));

    // `changeType('video/mp4; codecs="vp09…"')` и VP9 приходят во время
    // воспроизведения, как у плеера страницы; следом за changeType обязан быть init.
    let mut second_half = Some((vp9.stream_of(2..4), audio.fragments[2..].concat()));
    let report = play(open(&mut opener), |tick| {
        if tick == 40
            && let Some((video_bytes, audio_bytes)) = second_half.take()
        {
            writer
                .change_container(VIDEO, PushedContainer::FragmentedMp4)
                .expect("change type");
            feed(&mut writer, VIDEO, &video_bytes);
            feed(&mut writer, AUDIO, &audio_bytes);
            writer.end_of_stream().expect("end of stream");
        }
    });
    assert_switch_without_stopping(&report, (64, 64), (96, 96));
}

#[test]
fn negative_timestamp_offset_moves_late_fragments_to_zero() {
    // Плеер страницы кладёт фрагменты с tfdt 0,4 с и сдвигает их к нулю, как это
    // делают MSE-плееры для склейки рекламы и роликов.
    let video = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let (mut writer, mut opener) = new_stream();
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .set_timestamp_offset(VIDEO, TimestampOffset::from_micros(-400_000))
        .expect("offset");
    feed(&mut writer, VIDEO, &video.stream_of(2..4));
    writer.end_of_stream().expect("end of stream");

    let report = play(open(&mut opener), |_| {});
    assert_clean_end(&report);
    assert_eq!(presented_times(&report), frame_times(0..10));
}

#[test]
fn abort_in_the_middle_of_a_fragment_drops_only_the_partial_bytes() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64.mp4"));
    let (mut writer, mut opener) = new_stream();
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    feed(&mut writer, VIDEO, &video.stream_of(0..1));
    // Половина второго фрагмента, затем `abort()` и повтор фрагмента целиком —
    // так плеер страницы переставляет скачивание.
    let second = &video.fragments[1];
    feed(&mut writer, VIDEO, &second[..second.len() / 2]);
    writer.abort(VIDEO).expect("abort");
    for fragment in &video.fragments[1..] {
        feed(&mut writer, VIDEO, fragment);
    }
    writer.end_of_stream().expect("end of stream");

    let report = play(open(&mut opener), |_| {});
    assert_clean_end(&report);
    assert_eq!(presented_times(&report), frame_times(0..20));
}

#[test]
fn mpeg_ts_lane_plays_video_and_audio() {
    let transport_stream = own_fixture("muxed-h264-aac.ts");
    let (mut writer, mut opener) = new_stream();
    writer
        .add_lane(VIDEO, PushedContainer::MpegTs)
        .expect("ts lane");
    feed(&mut writer, VIDEO, &transport_stream);
    writer.end_of_stream().expect("end of stream");

    let report = play(open(&mut opener), |_| {});
    assert_clean_end(&report);
    assert_eq!(
        report.frames.len(),
        10,
        "frames {:?}",
        presented_times(&report)
    );
    // Синус фикстуры (амплитуда 1/8, RMS ≈ 0,088), а не тишина.
    assert!(
        report.audio_rms() > 0.05,
        "audio RMS {}",
        report.audio_rms()
    );
    assert!(report.audio_duration() >= Duration::from_millis(350));
}
