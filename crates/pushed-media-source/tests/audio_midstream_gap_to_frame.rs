//! Дыра в звуке посреди файла короче порога разрыва — до кадра и до звука в выходе.
//!
//! Дефект D3 (живая приёмка 03e): дыра 321–340 мс в дорожке звука посреди файла
//! склеивалась — звук после дыры писался сразу за звуком до неё и опережал видео на
//! длину дыры до следующей перемотки. Разрывы от 500 мс ведёт передача часов
//! (`audio_gap.rs`), а дыры короче теперь закрывает тишина той же длины
//! (`session/audio_runtime/mid_stream_gap.rs`).
//!
//! Время звука проверяется по записанному PCM: часы показа ведёт выход звука, поэтому
//! позиция sample-а в выходе (от начала или от очистки перемоткой) — это время, когда его
//! услышит зритель; кадры показываются по тем же часам.
//!
//! Фикстуры: `video-h264-64-4s.mp4` и `audio-aac-4s.mp4` (синус), по десять фрагментов;
//! фрагмент звука — 19 AAC-кадров по 1024 sample при 48 кГц = 405,33 мс, поэтому
//! пропущенный фрагмент звука — дыра 405 мс (меньше порога разрыва 500 мс).

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
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamDemuxer, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture, with_last_sample_extended};
use support::playback::{PlaybackReport, open, play, play_controlled};

/// Длительность кадра фикстуры (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность фрагмента видео.
const VIDEO_FRAGMENT: Duration = Duration::from_millis(400);
/// Фрагментов в фикстурах.
const FRAGMENT_COUNT: usize = 10;
/// Кадров в одном фрагменте видео.
const FRAMES_PER_FRAGMENT: u32 = 10;
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
/// Sample-ов звука в одном фрагменте (19 AAC-кадров по 1024).
const AUDIO_SAMPLES_PER_FRAGMENT: u64 = 19 * 1_024;
/// Частота звука фикстуры.
const AUDIO_SAMPLE_RATE: u64 = 48_000;
/// Пропускаемый фрагмент звука: дыра 2,03–2,43 с.
const MISSING_AUDIO_FRAGMENT: usize = 5;
/// Порог «слышно»: синус фикстуры громче 0,5, вставленная тишина — ровно ноль.
const AUDIBLE: f32 = 0.05;
/// Неслышимые отрезки короче этого — переход синуса через ноль и тихий первый AAC-кадр
/// после сброса декодера, а не тишина.
const MIN_COUNTED_SILENCE: Duration = Duration::from_millis(15);
/// Точность времени звука: кадр видео.
const AUDIO_TIME_TOLERANCE: Duration = FRAME;
/// Точность длины записанного звука: AAC-кадр с запасом.
const AUDIO_LENGTH_TOLERANCE: Duration = Duration::from_millis(100);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Время первого sample-а фрагмента звука `index` фикстуры.
fn audio_fragment_start(index: usize) -> Duration {
    Duration::from_nanos(
        AUDIO_SAMPLES_PER_FRAGMENT * index as u64 * 1_000_000_000 / AUDIO_SAMPLE_RATE,
    )
}

/// Длина дыры из `missing` подряд пропущенных фрагментов звука.
fn hole_of(missing: usize) -> Duration {
    audio_fragment_start(missing)
}

/// Поток: всё видео и звук без `missing_fragments` фрагментов, начиная с
/// [`MISSING_AUDIO_FRAGMENT`] (0 — звук без дыры), конец потока объявлен.
///
/// Дыра сделана так, как она выглядит в настоящем MP4: последний AAC-кадр перед ней
/// по контейнеру длится до начала звука после дыры, а декодер отдаёт обычные 1024
/// sample-а (`with_last_sample_extended`).
fn stream(missing_fragments: usize) -> PushedStreamDemuxer {
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
        .append(VIDEO, &video.stream_of(0..FRAGMENT_COUNT))
        .expect("video bytes");
    let mut audio_bytes = audio.initialization.clone();
    for (index, fragment) in audio.fragments.iter().enumerate() {
        let before_hole = MISSING_AUDIO_FRAGMENT - 1;
        if missing_fragments > 0 && index == before_hole {
            let hole_ticks = u32::try_from(AUDIO_SAMPLES_PER_FRAGMENT).expect("ticks")
                * u32::try_from(missing_fragments).expect("fragment count");
            audio_bytes.extend_from_slice(&with_last_sample_extended(fragment, hole_ticks));
        } else if !(MISSING_AUDIO_FRAGMENT..MISSING_AUDIO_FRAGMENT + missing_fragments)
            .contains(&index)
        {
            audio_bytes.extend_from_slice(fragment);
        }
    }
    writer.append(AUDIO, &audio_bytes).expect("audio bytes");
    writer.end_of_stream().expect("end of stream");
    open(&mut opener)
}

/// Слышимые отрезки записанного PCM (время от начала выбранного куска).
#[derive(Debug)]
struct AudibleRuns {
    /// Отрезки `[начало, конец)`; соседние, разделённые меньше [`MIN_COUNTED_SILENCE`],
    /// слиты в один.
    runs: Vec<(Duration, Duration)>,
}

impl AudibleRuns {
    /// Разбирает PCM отчёта, начиная с PCM-кадра `first_frame`.
    fn of(report: &PlaybackReport, first_frame: usize) -> Self {
        let (sample_rate, channels) = report.audio_format.expect("audio output was created");
        let frame_time =
            |frame: usize| Duration::from_secs_f64(frame as f64 / f64::from(sample_rate));
        let audible: Vec<bool> = report.audio_samples[first_frame * channels..]
            .chunks_exact(channels)
            .map(|frame| frame.iter().any(|sample| sample.abs() > AUDIBLE))
            .collect();
        let mut runs: Vec<(Duration, Duration)> = Vec::new();
        let mut run_start = None;
        for (frame_index, is_audible) in audible.iter().copied().chain([false]).enumerate() {
            match (is_audible, run_start) {
                (true, None) => run_start = Some(frame_index),
                (false, Some(start)) => {
                    let (start, end) = (frame_time(start), frame_time(frame_index));
                    match runs.last_mut() {
                        Some(previous) if start - previous.1 < MIN_COUNTED_SILENCE => {
                            previous.1 = end;
                        }
                        _ => runs.push((start, end)),
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        Self { runs }
    }

    /// Тишины между слышимыми отрезками: (начало тишины, длина).
    fn inner_silences(&self) -> Vec<(Duration, Duration)> {
        self.runs
            .windows(2)
            .map(|pair| (pair[0].1, pair[1].0 - pair[0].1))
            .collect()
    }

    /// Начало последнего слышимого отрезка.
    fn last_run_start(&self) -> Duration {
        self.runs.last().expect("the tone must be heard").0
    }

    /// Конец последнего слышимого отрезка.
    fn end(&self) -> Duration {
        self.runs.last().expect("the tone must be heard").1
    }
}

/// `actual` совпадает с `expected` с точностью `tolerance`.
fn assert_near(actual: Duration, expected: Duration, tolerance: Duration, what: &str) {
    assert!(
        actual.abs_diff(expected) <= tolerance,
        "{what}: {actual:?}, expected {expected:?} ± {tolerance:?}"
    );
}

/// Поток доигран без ошибок, все кадры показаны по порядку и вовремя по часам.
fn assert_played_every_frame(report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let presented: Vec<Duration> = report.frames.iter().map(|frame| frame.pts).collect();
    let expected: Vec<Duration> = (0..FRAGMENT_COUNT as u32 * FRAMES_PER_FRAGMENT)
        .map(|index| FRAME * index)
        .collect();
    assert_eq!(presented, expected, "every frame once, in order");
    assert!(
        report.max_av_offset() <= FRAME,
        "frame shown {:?} away from the player clock",
        report.max_av_offset()
    );
}

/// Дыра 405 мс посреди звука: зритель слышит тишину длины дыры, а звук после неё —
/// в своё время, рядом с кадрами того же времени (до фикса — склейка, звук на 405 мс
/// раньше картинки и записанный звук короче ролика на длину дыры).
#[test]
fn audio_hole_shorter_than_the_gap_threshold_is_heard_as_silence_in_sync() {
    let report = play(stream(1), |_| {});

    assert_played_every_frame(&report);
    let audible = AudibleRuns::of(&report, 0);
    let silences = audible.inner_silences();
    assert_eq!(silences.len(), 1, "exactly one hole: {audible:?}");
    let (hole_start, hole_length) = silences[0];
    assert_near(
        hole_start,
        audio_fragment_start(MISSING_AUDIO_FRAGMENT),
        AUDIO_TIME_TOLERANCE,
        "the hole begins where the audio before it ends",
    );
    assert_near(
        hole_length,
        hole_of(1),
        AUDIO_TIME_TOLERANCE,
        "silence of the hole length",
    );
    assert_near(
        audible.last_run_start(),
        audio_fragment_start(MISSING_AUDIO_FRAGMENT + 1),
        AUDIO_TIME_TOLERANCE,
        "the tone after the hole plays at its own time (output time = media time)",
    );
    assert_near(
        audible.end(),
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "all of the tone plus the silence fills the clip",
    );
}

/// Контроль ложных срабатываний: звук без дыры — ни одной вставленной тишины.
#[test]
fn continuous_audio_gets_no_inserted_silence() {
    let report = play(stream(0), |_| {});

    assert_played_every_frame(&report);
    let audible = AudibleRuns::of(&report, 0);
    assert!(
        audible.inner_silences().is_empty(),
        "no silence inside continuous audio: {audible:?}"
    );
    assert_near(
        audible.end(),
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "the whole tone",
    );
}

/// Контроль границы: дыра 810 мс (два фрагмента) — не тишина, а передача часов
/// (`audio_gap.rs`): в выход тишина не пишется, выход доигрывает звук до дыры и молчит,
/// пока часы видео не дойдут до звука после неё.
#[test]
fn audio_hole_above_the_gap_threshold_is_held_not_padded() {
    let missing_fragments = 2;
    let report = play(stream(missing_fragments), |_| {});

    assert_played_every_frame(&report);
    let audible = AudibleRuns::of(&report, 0);
    assert!(
        audible.inner_silences().is_empty(),
        "the hold path writes no silence: {audible:?}"
    );
    assert_near(
        audible.end(),
        CLIP - hole_of(missing_fragments),
        AUDIO_LENGTH_TOLERANCE,
        "only the tone is written",
    );
    assert_near(
        report.mid_stream_audio_silence,
        hole_of(missing_fragments),
        AUDIO_LENGTH_TOLERANCE,
        "the output stays silent for the hole while video plays on",
    );
}

/// Перемотка в `target` (данные уже в источнике): после очистки выхода тон после дыры
/// звучит в своё время относительно цели, до него — тишина.
fn play_with_seek_into_the_hole(target: Duration) -> (PlaybackReport, AudibleRuns) {
    let mut seek_issued = false;
    let report = play_controlled(
        PreparedMedia::from_external_label(
            "mid-stream audio hole after a seek",
            Box::new(stream(1)),
        ),
        |control| {
            if !seek_issued
                && control.state() == PlaybackState::Playing
                && control.observation().position >= VIDEO_FRAGMENT
            {
                seek_issued = true;
                control.seek(target);
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
    let clear_frame = *report
        .audio_frames_at_seek_clears
        .last()
        .expect("the seek clears the audio output");
    let audible = AudibleRuns::of(&report, clear_frame);
    (report, audible)
}

/// Цель — начало дыры по видео (2,0 с): последний AAC-кадр перед дырой пересекает цель
/// и становится первым PCM отрезка, а звук после дыры раньше склеивался с ним.
#[test]
fn seek_to_the_start_of_the_hole_keeps_the_tone_after_it_in_sync() {
    let target = VIDEO_FRAGMENT * MISSING_AUDIO_FRAGMENT as u32;

    let (_report, audible) = play_with_seek_into_the_hole(target);

    assert_near(
        audible.last_run_start(),
        audio_fragment_start(MISSING_AUDIO_FRAGMENT + 1) - target,
        AUDIO_TIME_TOLERANCE,
        "tone after the hole, measured from the seek target",
    );
    let silence_before_tone: Duration = audible
        .inner_silences()
        .iter()
        .map(|(_, length)| *length)
        .sum::<Duration>()
        + audible.runs[0].0;
    assert_near(
        silence_before_tone,
        hole_of(1),
        AUDIO_TIME_TOLERANCE,
        "silence of the hole length before the tone",
    );
}

/// Цель посреди дыры: до звука после дыры — тишина, затем тон в своё время.
#[test]
fn seek_into_the_middle_of_the_hole_waits_with_silence_for_the_tone() {
    let target = audio_fragment_start(MISSING_AUDIO_FRAGMENT) + Duration::from_millis(200);

    let (_report, audible) = play_with_seek_into_the_hole(target);

    assert!(
        audible.inner_silences().is_empty(),
        "one tone run after the seek: {audible:?}"
    );
    assert_near(
        audible.last_run_start(),
        audio_fragment_start(MISSING_AUDIO_FRAGMENT + 1) - target,
        AUDIO_TIME_TOLERANCE,
        "silence from the target to the tone after the hole",
    );
}
