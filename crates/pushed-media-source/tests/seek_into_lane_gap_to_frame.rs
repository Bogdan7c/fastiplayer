//! Перемотка туда, где у одной полосы ещё нет данных, — до кадра и до звука.
//!
//! Правило источника: после перемотки каждая полоса садится на цель, если у неё
//! есть данные у цели; полоса, чьи данные начинаются позже цели (звук начинается
//! позже видео, или страница прислала звук только со своего сегмента после цели),
//! вступает со своего первого сегмента после цели — как на обычном старте, — а не
//! ждёт вечно сегмент с целью, которого нет.
//!
//! Фикстуры: `video-h264-64-4s.mp4` (10 фрагментов по 0,4 с, каждый с ключевого
//! кадра, 25 к/с) и `audio-aac-4s.mp4` (синус; фрагмент — 19 AAC-кадров по 1024
//! sample при 48 кГц = 405,33 мс, поэтому звук фрагмента N начинается в
//! N × 405,33 мс).
//!
//! Время звука проверяется по записанному PCM: после очистки выхода перемоткой
//! первый записанный sample звучит в момент цели, и позиция первого слышимого
//! sample-а после неё — это время, когда зритель услышал звук, отсчитанное от цели.

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
    LaneId, PageSeekRequest, PushedContainer, PushedDuration, PushedSourcePolicy,
    PushedStreamMonitor, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, PresentedFrame, open, play_controlled};

/// Длительность кадра видео (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность фрагмента видео.
const VIDEO_FRAGMENT: Duration = Duration::from_millis(400);
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
/// Фрагментов в каждой фикстуре.
const FRAGMENT_COUNT: usize = 10;
/// Sample-ов звука в одном фрагменте (19 AAC-кадров по 1024).
const AUDIO_SAMPLES_PER_FRAGMENT: u64 = 19 * 1_024;
/// Частота звука фикстуры.
const AUDIO_SAMPLE_RATE: u64 = 48_000;
/// Порог «слышно»: синус фикстуры громче 0,5, вставленная тишина — ноль.
const AUDIBLE: f32 = 0.05;
/// Точность времени звука: кадр видео (первый AAC-кадр после сброса декодера
/// звучит тише и может сдвинуть порог на несколько миллисекунд).
const AUDIO_TIME_TOLERANCE: Duration = FRAME;
/// Предел ожидания после перемотки (шагов по 5 мс): посадка — десятки шагов,
/// вечное ожидание кончилось бы пределом прогона (~10 с), а не `Ended`.
const MAX_TICKS_FROM_SEEK_TO_END: usize = 1_200;
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Время начала фрагмента видео.
fn video_fragment_start(index: usize) -> Duration {
    VIDEO_FRAGMENT * u32::try_from(index).expect("small index")
}

/// Время первого sample-а фрагмента звука.
fn audio_fragment_start(index: usize) -> Duration {
    let samples = AUDIO_SAMPLES_PER_FRAGMENT * index as u64;
    Duration::from_nanos(samples * 1_000_000_000 / AUDIO_SAMPLE_RATE)
}

/// Страница: поток «видео + звук» и фрагменты обеих фикстур.
struct Page {
    writer: PushedStreamWriter,
    monitor: PushedStreamMonitor,
    video: FragmentedMp4,
    audio: FragmentedMp4,
}

impl Page {
    /// Поток с длительностью 4 с, init обеих полос уже отправлены.
    fn new() -> (Self, PushedStreamOpener) {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
        writer
            .add_lane(AUDIO, PushedContainer::FragmentedMp4)
            .expect("audio lane");
        writer.set_duration(PushedDuration::Finite(CLIP));
        let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
        let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
        assert_eq!(
            (video.fragments.len(), audio.fragments.len()),
            (FRAGMENT_COUNT, FRAGMENT_COUNT)
        );
        writer
            .append(VIDEO, &video.initialization)
            .expect("video init");
        writer
            .append(AUDIO, &audio.initialization)
            .expect("audio init");
        let page = Self {
            monitor: writer.monitor(),
            writer,
            video,
            audio,
        };
        (page, opener)
    }

    /// Шлёт фрагменты видео.
    fn push_video(&mut self, fragments: Range<usize>) {
        let bytes = self.video.fragments[fragments].concat();
        self.writer.append(VIDEO, &bytes).expect("video fragments");
    }

    /// Шлёт фрагменты звука.
    fn push_audio(&mut self, fragments: Range<usize>) {
        let bytes = self.audio.fragments[fragments].concat();
        self.writer.append(AUDIO, &bytes).expect("audio fragments");
    }

    /// Конец потока.
    fn end(&mut self) {
        self.writer.end_of_stream().expect("end of stream");
    }
}

fn prepared(page: &mut PushedStreamOpener) -> PreparedMedia {
    PreparedMedia::from_external_label("seek into a lane gap", Box::new(open(page)))
}

/// Кадры, впервые показанные после первой команды перемотки.
fn frames_after_seek(report: &PlaybackReport) -> Vec<PresentedFrame> {
    let (seek_tick, _) = report.seeks[0];
    report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .copied()
        .collect()
}

/// Плеер не завис: доиграл до `Ended` без ошибок, вскоре после перемотки.
fn assert_finished_without_hang(report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    let (seek_tick, _) = report.seeks[0];
    let (last_tick, last_state, _) = report.state_trace.last().copied().expect("trace");
    assert_eq!(
        last_state,
        PlaybackState::Ended,
        "the player hung after the seek (last state {last_state:?} at tick {last_tick})"
    );
    assert!(
        last_tick - seek_tick < MAX_TICKS_FROM_SEEK_TO_END,
        "took {} ticks from the seek to the end",
        last_tick - seek_tick
    );
}

/// Посадка: первый кадр после перемотки — цель ± кадр, дальше — каждый кадр по
/// порядку до конца ролика.
fn assert_landed_on_target_and_played_every_frame(report: &PlaybackReport, target: Duration) {
    let after = frames_after_seek(report);
    let first = after.first().expect("frames after the seek");
    assert!(
        first.pts.abs_diff(target) <= FRAME,
        "first frame after the seek at {:?}, target {target:?}",
        first.pts
    );
    let shown: Vec<Duration> = after.iter().map(|frame| frame.pts).collect();
    let expected: Vec<Duration> = (0..)
        .map(|index| first.pts + FRAME * index)
        .take_while(|pts| *pts < CLIP)
        .collect();
    assert_eq!(
        shown, expected,
        "every frame after the landing, in order, to the end"
    );
    assert_eq!(report.unattributed_presentations, 0);
}

/// Через сколько после цели зритель впервые услышал звук: первый слышимый
/// sample после последней очистки выхода перемоткой.
fn first_tone_after_seek(report: &PlaybackReport) -> Duration {
    let (sample_rate, channels) = report.audio_format.expect("audio output was created");
    let cleared_at = *report
        .audio_frames_at_seek_clears
        .last()
        .expect("the seek cleared the audio output");
    let after_seek = &report.audio_samples[cleared_at * channels..];
    let first_audible = after_seek
        .chunks_exact(channels)
        .position(|frame| frame.iter().any(|sample| sample.abs() > AUDIBLE))
        .expect("the tone must be heard after the seek");
    Duration::from_secs_f64(first_audible as f64 / f64::from(sample_rate))
}

/// Звук после перемотки слышен со своего времени (отсчёт — от цели) и до конца.
fn assert_audio_joined_at(report: &PlaybackReport, target: Duration, audio_start: Duration) {
    let heard_after = first_tone_after_seek(report);
    let expected = audio_start - target;
    assert!(
        heard_after.abs_diff(expected) <= AUDIO_TIME_TOLERANCE,
        "tone heard {heard_after:?} after the target, expected {expected:?} (audio data starts at \
         {audio_start:?})"
    );
}

/// (a) Перемотка назад по полке в начало, где звука ещё нет: звук начинается в
/// 0,405 с, видео — в 0, цель 0,2 с. Видео садится на цель, звук вступает в своё
/// время; страницу не просят (всё уже на полке).
#[test]
fn on_shelf_seek_into_the_audio_leading_gap_lands_on_video_and_joins_audio_later() {
    let (mut page, mut opener) = Page::new();
    page.push_video(0..FRAGMENT_COUNT);
    page.push_audio(1..FRAGMENT_COUNT);
    page.end();
    let target = Duration::from_millis(200);
    let monitor = page.monitor.clone();
    let mut requests: Vec<PageSeekRequest> = Vec::new();
    let mut seeked = false;
    let report = play_controlled(prepared(&mut opener), |control| {
        requests.extend(monitor.take_page_seek_request());
        if !seeked
            && control.state() == PlaybackState::Playing
            && control.observation().position >= video_fragment_start(3)
        {
            seeked = true;
            control.seek(target);
        }
    });

    assert_eq!(report.seeks.len(), 1, "exactly one seek was issued");
    assert_finished_without_hang(&report);
    assert_eq!(requests, [], "an on-shelf seek must not contact the page");
    assert_landed_on_target_and_played_every_frame(&report, target);
    assert_audio_joined_at(&report, target, audio_fragment_start(1));
}

/// (b) Перемотка вне полки: страница присылает видео с ключевого кадра перед
/// целью, а звук — только со своего сегмента через ~0,36 с после цели (больше
/// наибольшей дыры полки, 100 мс). Звук приходит РАНЬШЕ видео: вступает, когда
/// видео село. Одна просьба странице.
#[test]
fn off_shelf_seek_with_audio_starting_after_the_target_joins_audio_at_its_time() {
    let (mut page, mut opener) = Page::new();
    page.push_video(0..3);
    page.push_audio(0..3);
    let target = video_fragment_start(6) + FRAME * 2;
    let first_audio_fragment = 7;
    assert!(audio_fragment_start(first_audio_fragment) - target > Duration::from_millis(100));
    let monitor = page.monitor.clone();
    let mut requests: Vec<PageSeekRequest> = Vec::new();
    let mut seek_tick = None;
    let report = play_controlled(prepared(&mut opener), |control| {
        requests.extend(monitor.take_page_seek_request());
        let tick = control.observation().tick;
        match seek_tick {
            None if control.state() == PlaybackState::Playing
                && control.observation().position >= VIDEO_FRAGMENT =>
            {
                seek_tick = Some(tick);
                control.seek(target);
            }
            // Страница перемоталась: звук со своего сегмента после цели, затем
            // видео с ключевого кадра перед целью.
            Some(at) if tick == at + 20 => {
                page.push_audio(first_audio_fragment..FRAGMENT_COUNT);
                page.push_video(6..FRAGMENT_COUNT);
                page.end();
            }
            _ => {}
        }
    });

    assert_eq!(report.seeks.len(), 1, "exactly one seek was issued");
    assert_finished_without_hang(&report);
    assert_eq!(
        requests,
        [PageSeekRequest { target }],
        "exactly one request to the page"
    );
    assert_landed_on_target_and_played_every_frame(&report, target);
    assert_audio_joined_at(&report, target, audio_fragment_start(first_audio_fragment));
}

/// «Twitch»: страница уже прыгнула и прислала байты нового времени (видео с
/// ключевого кадра перед целью, звук — только со своего сегмента после цели) ДО
/// сообщения о перемотке; приложение отмечает цель страницы и перематывает плеер.
/// Всё на полке: посадка сразу, без просьбы странице.
#[test]
fn page_seek_whose_late_audio_arrived_before_the_seek_message_plays_from_the_shelf() {
    let (mut page, mut opener) = Page::new();
    page.push_video(0..3);
    page.push_audio(0..3);
    let target = video_fragment_start(6) + FRAME * 2;
    let first_audio_fragment = 7;
    let monitor = page.monitor.clone();
    let mut requests: Vec<PageSeekRequest> = Vec::new();
    let mut pushed_new_time = false;
    let mut seeked = false;
    let report = play_controlled(prepared(&mut opener), |control| {
        requests.extend(monitor.take_page_seek_request());
        let position = control.observation().position;
        if !pushed_new_time && position >= FRAME * 3 {
            pushed_new_time = true;
            page.push_video(6..FRAGMENT_COUNT);
            page.push_audio(first_audio_fragment..FRAGMENT_COUNT);
            page.end();
        }
        // Байты прислали с концом потока: плеер дочитал полку до дыры и доигрывает
        // (`Draining`), поэтому условие — только позиция.
        if !seeked && position >= VIDEO_FRAGMENT {
            seeked = true;
            page.writer.note_page_seek(target);
            control.seek(target);
        }
    });

    assert_eq!(report.seeks.len(), 1, "exactly one seek was issued");
    assert_finished_without_hang(&report);
    assert_eq!(
        requests,
        [],
        "the bytes were on the shelf and the page seeks itself"
    );
    assert_landed_on_target_and_played_every_frame(&report, target);
    assert_audio_joined_at(&report, target, audio_fragment_start(first_audio_fragment));
}
