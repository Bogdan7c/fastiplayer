//! Звук начинается позже видео — до кадра и до звука в выходе.
//!
//! Часы показа ведёт выход звука: первый PCM, записанный в пустой выход, звучит в базе
//! часов (старт, цель перемотки). Если данные звука начинаются позже базы, а плеер
//! пишет их сразу, звук опережает видео на весь разрыв до конца файла (сессия 03e
//! моста). Поэтому проверяем сам PCM в выходе, а не `max_av_offset()`: тот сравнивает
//! кадр с часами звука и сдвиг звука относительно видео не видит.
//!
//! Позиция sample-а в записанном PCM — это и есть время, когда его услышит зритель:
//! начало выхода соответствует базе часов, а кадры показываются по тем же часам.
//!
//! Фикстуры: `video-h264-64-4s.mp4` и `audio-aac-4s.mp4` (синус), по десять
//! фрагментов по 0,4 с; фрагмент N начинается в N × 0,4 с.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::ops::Range;
use std::time::Duration;

use player_core::{PlaybackState, PreparedMedia};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamDemuxer, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, open, play, play_controlled};

/// Длительность кадра фикстуры (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность фрагмента обеих фикстур.
const FRAGMENT: Duration = Duration::from_millis(400);
/// Фрагментов в фикстурах (4 с).
const FRAGMENT_COUNT: usize = 10;
/// Кадров в одном фрагменте.
const FRAMES_PER_FRAGMENT: u32 = 10;
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
/// AAC-кадров в одном фрагменте звука фикстуры: фрагмент звука — 19 кадров по 1024
/// sample при 48 кГц = 405,33 мс, поэтому звук фрагмента N начинается чуть позже
/// видео того же фрагмента (N × 0,4 с).
const AAC_FRAMES_PER_FRAGMENT: u32 = 19;
/// Sample-ов в одном AAC-кадре.
const AAC_FRAME_SAMPLES: u32 = 1_024;
/// Частота звука фикстуры.
const AUDIO_SAMPLE_RATE: u32 = 48_000;
/// Порог «слышно»: синус фикстуры громче 0,5, тишина — ровно ноль.
const AUDIBLE: f32 = 0.05;
/// Точность времени звука: кадр видео (первый AAC-кадр после сброса декодера
/// может прозвучать тише и сдвинуть порог на несколько миллисекунд).
const AUDIO_TIME_TOLERANCE: Duration = FRAME;
/// Точность длины записанного звука: AAC-кадр с запасом.
const AUDIO_LENGTH_TOLERANCE: Duration = Duration::from_millis(100);
/// Шаг «звуковой карты» и плеера — 5 мс; на старте выход может разок опустеть.
const AUDIO_SILENCE_TOLERANCE: Duration = Duration::from_millis(10);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Поток: звук из фрагментов `audio`, видео из `video` (или без видео).
///
/// Время фрагментов исходное: поток «звук с 0,4 с» — это фрагменты звука с первого.
fn stream(audio: Range<usize>, video: Option<Range<usize>>) -> PushedStreamDemuxer {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    if video.is_some() {
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
    }
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(CLIP));
    if let Some(video) = video {
        let video_fixture = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
        writer
            .append(VIDEO, &video_fixture.stream_of(video))
            .expect("video bytes");
    }
    let audio_fixture = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    writer
        .append(AUDIO, &audio_fixture.stream_of(audio))
        .expect("audio bytes");
    writer.end_of_stream().expect("end of stream");
    open(&mut opener)
}

/// PCM выхода с форматом: кадры (frames) переводятся во время.
struct WrittenAudio<'report> {
    /// Записанный interleaved PCM.
    samples: &'report [f32],
    /// Частота выхода.
    sample_rate: u32,
    /// Каналов в выходе.
    channels: usize,
}

impl<'report> WrittenAudio<'report> {
    /// PCM отчёта.
    fn of(report: &'report PlaybackReport) -> Self {
        let (sample_rate, channels) = report.audio_format.expect("audio output was created");
        Self {
            samples: &report.audio_samples,
            sample_rate,
            channels,
        }
    }

    /// Время PCM-кадра с номером `frame` от начала выхода.
    fn time_of_frame(&self, frame: usize) -> Duration {
        Duration::from_secs_f64(frame as f64 / f64::from(self.sample_rate))
    }

    /// Время первого слышимого PCM-кадра от начала выхода.
    fn first_audible(&self) -> Option<Duration> {
        self.samples
            .chunks_exact(self.channels)
            .position(|frame| frame.iter().any(|sample| sample.abs() > AUDIBLE))
            .map(|frame_index| self.time_of_frame(frame_index))
    }

    /// Длительность всего записанного PCM.
    fn duration(&self) -> Duration {
        self.time_of_frame(self.samples.len() / self.channels)
    }
}

/// Время первого sample-а фрагмента звука `index` фикстуры.
fn audio_fragment_start(index: usize) -> Duration {
    let samples = u64::from(AAC_FRAMES_PER_FRAGMENT * AAC_FRAME_SAMPLES) * index as u64;
    Duration::from_nanos(samples * 1_000_000_000 / u64::from(AUDIO_SAMPLE_RATE))
}

/// Времена кадров с фрагмента `first_video_fragment` до конца.
fn expected_frame_times(first_video_fragment: usize) -> Vec<Duration> {
    let first_frame = first_video_fragment as u32 * FRAMES_PER_FRAGMENT;
    let frame_count = FRAGMENT_COUNT as u32 * FRAMES_PER_FRAGMENT;
    (first_frame..frame_count)
        .map(|index| FRAME * index)
        .collect()
}

/// `actual` совпадает с `expected` с точностью `tolerance`.
fn assert_near(actual: Duration, expected: Duration, tolerance: Duration, what: &str) {
    assert!(
        actual.abs_diff(expected) <= tolerance,
        "{what}: {actual:?}, expected {expected:?} ± {tolerance:?}"
    );
}

/// Поток доигран без ошибок, каждый кадр показан по порядку и вовремя по часам.
fn assert_played_every_frame(report: &PlaybackReport, first_video_fragment: usize) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let presented: Vec<Duration> = report.frames.iter().map(|frame| frame.pts).collect();
    assert_eq!(
        presented,
        expected_frame_times(first_video_fragment),
        "every frame once, in order"
    );
    assert!(
        report.max_av_offset() <= FRAME,
        "frame shown {:?} away from the player clock",
        report.max_av_offset()
    );
    assert!(
        report.mid_stream_audio_silence <= AUDIO_SILENCE_TOLERANCE,
        "audio output ran dry mid-stream for {:?}",
        report.mid_stream_audio_silence
    );
}

/// Звук с 0,4 с, видео с нуля: первый звук слышен в 0,4 с, а не в начале.
#[test]
fn audio_starting_after_video_is_heard_at_its_own_time() {
    let report = play(stream(1..FRAGMENT_COUNT, Some(0..FRAGMENT_COUNT)), |_| {});

    assert_played_every_frame(&report, 0);
    let written = WrittenAudio::of(&report);
    assert_near(
        written.first_audible().expect("the tone must be heard"),
        audio_fragment_start(1),
        AUDIO_TIME_TOLERANCE,
        "first audible sample (output time = media time)",
    );
    assert_near(
        written.duration(),
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "0,4 s of leading silence plus 3,6 s of tone",
    );
}

/// Длинная тишина почти до потолка (звук с ~0,81 с): тишина занимает буфер вывода, и
/// подпитка настоящим звуком после неё идёт обычным порядком — выход не пустеет
/// посреди звука (`assert_played_every_frame` проверяет `mid_stream_audio_silence`).
#[test]
fn long_leading_silence_does_not_starve_the_output_afterwards() {
    let first_audio_fragment = 2;
    let report = play(
        stream(
            first_audio_fragment..FRAGMENT_COUNT,
            Some(0..FRAGMENT_COUNT),
        ),
        |_| {},
    );

    assert_played_every_frame(&report, 0);
    let written = WrittenAudio::of(&report);
    assert_near(
        written.first_audible().expect("the tone must be heard"),
        audio_fragment_start(first_audio_fragment),
        AUDIO_TIME_TOLERANCE,
        "first audible sample",
    );
    assert_near(
        written.duration(),
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "leading silence plus tone",
    );
}

/// Контроль: звук и видео с нуля — тишина не вставляется.
#[test]
fn audio_and_video_from_zero_get_no_leading_silence() {
    let report = play(stream(0..FRAGMENT_COUNT, Some(0..FRAGMENT_COUNT)), |_| {});

    assert_played_every_frame(&report, 0);
    let written = WrittenAudio::of(&report);
    assert_near(
        written.first_audible().expect("the tone must be heard"),
        Duration::ZERO,
        AUDIO_TIME_TOLERANCE,
        "first audible sample",
    );
    assert_near(
        written.duration(),
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "the whole tone and nothing else",
    );
}

/// Звук с 0,4 с, видео с 1,6 с (поздние оба, вместе с фиксом зависания старта):
/// звук слышен в 0,4 с, кадры с 1,6 с показаны по тем же часам.
#[test]
fn late_audio_and_later_video_stay_in_sync() {
    let first_video_fragment = 4;
    let report = play(
        stream(
            1..FRAGMENT_COUNT,
            Some(first_video_fragment..FRAGMENT_COUNT),
        ),
        |_| {},
    );

    assert_played_every_frame(&report, first_video_fragment);
    let written = WrittenAudio::of(&report);
    assert_near(
        written.first_audible().expect("the tone must be heard"),
        audio_fragment_start(1),
        AUDIO_TIME_TOLERANCE,
        "first audible sample",
    );
    assert_near(
        written.duration(),
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "leading silence plus tone",
    );
    // Кадр в момент показа и звук в выходе — на одной оси: позиция кадра на часах
    // равна его PTS, значит и звук этого PTS звучит в тот же момент.
    let first_frame = report.frames.first().expect("video frames");
    assert_near(
        first_frame.position,
        first_frame.pts,
        FRAME,
        "first late frame on the audio clock",
    );
}

/// Перемотка за полку: страница шлёт видео с ключевого кадра 2,4 с, а звук — со
/// своего сегмента ~2,84 с (границы сегментов звука и видео на сайтах разные). Цель —
/// за 80 мс до звука: после перемотки 80 мс тишины, затем тон; кадры — с цели.
///
/// Разрыв взят меньше «наибольшей дыры» полки источника (100 мс): сегмент, начавшийся
/// дальше от цели, источник сейчас не подаёт вовсе — после перемотки звука не было
/// бы совсем (так же он ждёт байтов страницы, если на полке звук начинается дальше
/// цели). Это поведение источника, а не плеера; здесь проверяется плеер.
#[test]
fn seek_landing_before_the_first_audio_packet_pads_the_gap_with_silence() {
    let first_audio_after_seek = 7;
    let target = audio_fragment_start(first_audio_after_seek) - Duration::from_millis(80);
    let policy = PushedSourcePolicy::recommended().expect("policy");
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
    writer
        .append(VIDEO, &video.stream_of(0..3))
        .expect("video bytes");
    writer
        .append(AUDIO, &audio.stream_of(0..3))
        .expect("audio bytes");
    let mut seek_tick = None;
    let report = play_controlled(
        PreparedMedia::from_external_label(
            "leading gap after a page seek",
            Box::new(open(&mut opener)),
        ),
        |control| {
            let tick = control.observation().tick;
            match seek_tick {
                None if control.state() == PlaybackState::Playing
                    && control.observation().position >= FRAGMENT =>
                {
                    seek_tick = Some(tick);
                    control.seek(target);
                }
                // Страница перемоталась и шлёт с цели: видео с ключевого кадра
                // перед целью, звук — со своего сегмента после цели.
                Some(at) if tick == at + 20 => {
                    writer
                        .append(VIDEO, &video.fragments[6..].concat())
                        .expect("video after the seek");
                    writer
                        .append(AUDIO, &audio.fragments[first_audio_after_seek..].concat())
                        .expect("audio after the seek");
                    writer.end_of_stream().expect("end of stream");
                }
                _ => {}
            }
        },
    );

    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(report.seeks.len(), 1, "exactly one seek was issued");
    let (seek_tick, _) = report.seeks[0];
    let frames_after: Vec<Duration> = report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .map(|frame| frame.pts)
        .collect();
    assert_near(
        *frames_after.first().expect("frames after the seek"),
        target,
        FRAME,
        "first frame after the seek",
    );
    assert_eq!(
        frames_after.last().copied(),
        expected_frame_times(0).last().copied(),
        "played to the last frame"
    );

    // До перемотки тон непрерывен (звук и видео с нуля), поэтому единственный
    // неслышимый отрезок посреди записанного PCM — тишина после перемотки: от цели
    // до первого sample-а звука страницы.
    let written = WrittenAudio::of(&report);
    assert_near(
        longest_inner_silence(&written),
        audio_fragment_start(first_audio_after_seek) - target,
        AUDIO_TIME_TOLERANCE,
        "silence between the seek target and the first tone sample",
    );
}

/// Самый длинный неслышимый отрезок PCM между первым и последним слышимым кадром.
fn longest_inner_silence(written: &WrittenAudio<'_>) -> Duration {
    let audible: Vec<bool> = written
        .samples
        .chunks_exact(written.channels)
        .map(|frame| frame.iter().any(|sample| sample.abs() > AUDIBLE))
        .collect();
    let first = audible.iter().position(|is_audible| *is_audible);
    let last = audible.iter().rposition(|is_audible| *is_audible);
    let (Some(first), Some(last)) = (first, last) else {
        return Duration::ZERO;
    };
    let mut longest = 0_usize;
    let mut current = 0_usize;
    for is_audible in &audible[first..=last] {
        if *is_audible {
            current = 0;
        } else {
            current += 1;
            longest = longest.max(current);
        }
    }
    written.time_of_frame(longest)
}

/// Только звук, с 0,4 с: тишина не вставляется (смотреть нечего), воспроизведение
/// начинается с первого sample-а, а позиция показывает время звука (0,4 → 4,0 с).
#[test]
fn audio_only_stream_starts_at_its_first_sample_and_shows_its_time() {
    let mut playing_positions = Vec::new();
    let report = play_controlled(
        PreparedMedia::from_external_label(
            "audio-only late start",
            Box::new(stream(1..FRAGMENT_COUNT, None)),
        ),
        |control| {
            // `Draining` — источник кончился, выход доигрывает звук: позиция идёт дальше.
            if matches!(
                control.state(),
                PlaybackState::Playing | PlaybackState::Draining
            ) {
                playing_positions.push(control.observation().position);
            }
        },
    );

    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let written = WrittenAudio::of(&report);
    assert_near(
        written.first_audible().expect("the tone must be heard"),
        Duration::ZERO,
        AUDIO_TIME_TOLERANCE,
        "audio-only playback starts with the tone, not with silence",
    );
    assert_near(
        written.duration(),
        CLIP - FRAGMENT,
        AUDIO_LENGTH_TOLERANCE,
        "only the tone is written",
    );
    let earliest = playing_positions
        .iter()
        .min()
        .copied()
        .expect("playback started");
    let latest = playing_positions.iter().max().copied().expect("played");
    assert_near(
        earliest,
        audio_fragment_start(1),
        AUDIO_TIME_TOLERANCE,
        "position when the first sample plays is its own time",
    );
    assert_near(
        latest,
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "position reaches the end of the audio",
    );
}

/// Звук с 2,0 с, видео с нуля: разрыв больше потолка тишины (1 с).
///
/// Фиксирует ТЕКУЩЕЕ поведение (решение владельца, 03e): тишина не вставляется,
/// звук пишется сразу в базу часов и играет на 2 с раньше видео; плеер не виснет,
/// показывает все кадры и доходит до конца. Следующая задача («звук ждёт своего
/// времени, часы идут») должна поменять этот тест: первый звук — в 2,0 с.
#[test]
fn audio_gap_above_the_silence_cap_keeps_the_current_early_audio() {
    let first_audio_fragment = 5;
    let report = play(
        stream(
            first_audio_fragment..FRAGMENT_COUNT,
            Some(0..FRAGMENT_COUNT),
        ),
        |_| {},
    );

    assert_played_every_frame(&report, 0);
    let written = WrittenAudio::of(&report);
    assert_near(
        written.first_audible().expect("the tone must be heard"),
        Duration::ZERO,
        AUDIO_TIME_TOLERANCE,
        "current behavior: audio is written at the clock base without silence",
    );
    assert_near(
        written.duration(),
        FRAGMENT * (FRAGMENT_COUNT - first_audio_fragment) as u32,
        AUDIO_LENGTH_TOLERANCE,
        "only the tone is written",
    );
}
