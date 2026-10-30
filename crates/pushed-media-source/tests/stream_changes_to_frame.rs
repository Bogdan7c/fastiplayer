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

/// Сколько тишины посреди звука допускается: шаг «карты» и плеера — 5 мс, на
/// старте выход может разок опустеть на шаг. Прежний сброс по `TracksChanged`
/// ставил выход на паузу и пересоздавал его — это десятки миллисекунд тишины.
const AUDIO_SILENCE_TOLERANCE: Duration = Duration::from_millis(10);

/// Проверка смены init-сегмента посреди воспроизведения (качество или кодек).
///
/// Смена — эстафета на границе показа (сессия 03d): старый decoder доигрывает
/// кадры до границы, новый начинает с первого кадра нового эпизода. Поэтому:
/// - показаны все 20 кадров подряд, каждый ровно раз;
/// - кадры до 0,4 с — в старом размере, начиная ровно с 0,4 с — в новом;
/// - звук идёт без пауз, ошибок нет, поток доигран до конца.
fn assert_switch_without_stopping(
    report: &PlaybackReport,
    old_size: (u32, u32),
    new_size: (u32, u32),
) {
    assert_clean_end(report);
    assert_eq!(
        presented_times(report),
        frame_times(0..20),
        "every frame once, in order"
    );
    let switch_at = FRAME * 10;
    for frame in &report.frames {
        let expected_size = if frame.pts < switch_at {
            old_size
        } else {
            new_size
        };
        assert_eq!(
            (frame.width, frame.height),
            expected_size,
            "frame at {:?} has the size of the wrong episode",
            frame.pts
        );
    }
    assert_audio_without_pauses(report);
}

/// Звук целиком, без тишины посреди и без выброшенного буфера выхода.
fn assert_audio_without_pauses(report: &PlaybackReport) {
    assert_eq!(
        report.discarded_audio,
        Duration::ZERO,
        "audio output was destroyed with unplayed sound"
    );
    assert!(
        report.audio_duration() >= Duration::from_millis(780),
        "audio {:?}",
        report.audio_duration()
    );
    assert!(
        report.mid_stream_audio_silence <= AUDIO_SILENCE_TOLERANCE,
        "audio paused mid-stream for {:?}",
        report.mid_stream_audio_silence
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
    assert!(
        report.max_av_offset() <= FRAME,
        "A/V offset {:?}",
        report.max_av_offset()
    );
}

/// Звук разобран далеко вперёд видео (раздельные полосы YouTube допускают до 3 с).
///
/// Прежний сброс в момент разбора выбрасывал этот звук вместе с выходом: часы
/// вставали, и плеер навсегда висел в доигрывании (находка 03a). Теперь звук не
/// трогается, а поток доигрывается до `Ended`.
#[test]
fn quality_switch_with_audio_parsed_far_ahead_plays_to_the_end() {
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
    // Весь звук (0,8 с) сразу: разрыв со звуком больше всей первой половины видео.
    feed(
        &mut writer,
        AUDIO,
        &audio.stream_of(0..audio.fragments.len()),
    );

    let mut second_half = Some(high.stream_of(2..4));
    let report = play(open(&mut opener), |tick| {
        if tick == 40
            && let Some(video_bytes) = second_half.take()
        {
            feed(&mut writer, VIDEO, &video_bytes);
            writer.end_of_stream().expect("end of stream");
        }
    });

    assert_switch_without_stopping(&report, (64, 64), (96, 96));
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
