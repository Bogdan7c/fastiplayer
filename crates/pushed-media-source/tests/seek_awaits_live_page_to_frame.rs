//! Перемотка ждёт живую страницу дольше срока перемотки плеера (решение владельца
//! 150) — до кадра И до звука, с настоящими декодерами.
//!
//! Стенды 04e/04f: YouTube 4K отдавал данные нового места через 6–12 с, а плеер
//! сдавался через 10 с («Seeking timed out») и вставал на паузу на СТАРОМ месте
//! навсегда, хотя данные приходили следом. В 04e видео цели уже было, а звук после
//! цели ещё нет: плеер читал звук только до цели (всё до цели выбрасывается),
//! сборка «видео + звук» упиралась в предел опережения видео (3 с) и отвечала
//! «данных нет», а плеер ждал звук (`audio_preroll`) до срока. Здесь срок перемотки
//! укорочен до [`SHORT_COMMIT_TIMEOUT`], и страница несёт данные дольше него.
//!
//! Фикстуры: `video-h264-64-4s.mp4` (10 фрагментов по 0,4 с, каждый с ключевого
//! кадра, 25 к/с) и `audio-aac-4s.mp4` (синус; фрагмент — 19 AAC-кадров по 1024
//! sample при 48 кГц = 405,33 мс).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::ops::Range;
use std::time::Duration;

use player_core::{PlaybackState, PlayerTickConfig, PreparedMedia};
use pushed_media_source::{
    LaneId, PageSeekRequest, PushedContainer, PushedDuration, PushedSourcePolicy,
    PushedStreamMonitor, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, PlayerControl, TICK, open, play_controlled_with_config};

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
/// Порог «слышно»: синус фикстуры громче 0,5, тишина — ноль.
const AUDIBLE: f32 = 0.05;
/// Срок перемотки плеера в этих тестах (по умолчанию 10 с).
const SHORT_COMMIT_TIMEOUT: Duration = Duration::from_millis(250);
/// Через сколько шагов (по 5 мс и дольше) страница приносит недостающее: заметно
/// дольше [`SHORT_COMMIT_TIMEOUT`] даже без нагрузки (≥ 0,6 с).
const PAGE_DELAY_TICKS: usize = 120;
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

/// Время, округлённое вниз до целых микросекунд (шкала полки источника).
fn whole_micros(time: Duration) -> Duration {
    Duration::from_micros(u64::try_from(time.as_micros()).expect("small time"))
}

/// Настройки шага плеера: короткий срок перемотки, остальное — как в продукте.
fn short_timeout_config() -> PlayerTickConfig {
    PlayerTickConfig {
        seek_commit_timeout: SHORT_COMMIT_TIMEOUT,
        ..PlayerTickConfig::default()
    }
}

/// Страница: поток «видео + звук» и фрагменты обеих фикстур.
struct Page {
    writer: PushedStreamWriter,
    monitor: PushedStreamMonitor,
    video: FragmentedMp4,
    audio: FragmentedMp4,
}

impl Page {
    /// Поток с длительностью 4 с, init обеих полос и первые фрагменты уже отправлены.
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
        writer
            .append(VIDEO, &video.initialization)
            .expect("video init");
        writer
            .append(AUDIO, &audio.initialization)
            .expect("audio init");
        let mut page = Self {
            monitor: writer.monitor(),
            writer,
            video,
            audio,
        };
        page.push_video(0..3);
        page.push_audio(0..3);
        (page, opener)
    }

    fn push_video(&mut self, fragments: Range<usize>) {
        let bytes = self.video.fragments[fragments].concat();
        self.writer.append(VIDEO, &bytes).expect("video fragments");
    }

    fn push_audio(&mut self, fragments: Range<usize>) {
        let bytes = self.audio.fragments[fragments].concat();
        self.writer.append(AUDIO, &bytes).expect("audio fragments");
    }

    fn end(&mut self) {
        self.writer.end_of_stream().expect("end of stream");
    }
}

fn prepared(opener: &mut PushedStreamOpener) -> PreparedMedia {
    PreparedMedia::from_external_label("seek awaits a live page", Box::new(open(opener)))
}

/// Играет поток: на ~0,4 с перемотка к `target` вне полки, дальше страница по
/// `script` (что страница шлёт на каком шаге от перемотки), позиция плеера — монитору
/// (как приложение).
fn play_with_slow_page(
    target: Duration,
    mut script: impl FnMut(&mut Page, usize),
) -> (PlaybackReport, Vec<PageSeekRequest>) {
    play_with_page_script(target, short_timeout_config(), |page, _control, ticks| {
        script(page, ticks);
    })
}

/// Как [`play_with_slow_page`], но с заданными сроками плеера и пультом в сценарии
/// (повторная перемотка зрителя после отказа).
fn play_with_page_script(
    target: Duration,
    tick_config: PlayerTickConfig,
    mut script: impl FnMut(&mut Page, &mut PlayerControl<'_>, usize),
) -> (PlaybackReport, Vec<PageSeekRequest>) {
    let (mut page, mut opener) = Page::new();
    let monitor = page.monitor.clone();
    let mut requests = Vec::new();
    let mut seek_tick = None;
    let report = play_controlled_with_config(prepared(&mut opener), tick_config, |control| {
        requests.extend(monitor.take_page_seek_request());
        monitor.note_playback_position(control.observation().position);
        let tick = control.observation().tick;
        match seek_tick {
            None if control.state() == PlaybackState::Playing
                && control.observation().position >= VIDEO_FRAGMENT =>
            {
                seek_tick = Some(tick);
                control.seek(target);
            }
            Some(at) if tick > at => script(&mut page, control, tick - at),
            _ => {}
        }
    });
    (report, requests)
}

/// Перемотка дождалась страницы: без ошибок (никакого «Seeking timed out»), плеер
/// доиграл до конца, ни разу не вставал на паузу и не возвращался на старое место.
fn assert_waited_and_played_through(report: &PlaybackReport, target: Duration) {
    assert!(
        report.errors.is_empty(),
        "player errors (a seek timeout would be here): {:?}",
        report.errors
    );
    let (seek_tick, _) = report.seeks[0];
    assert!(
        report
            .state_trace
            .iter()
            .filter(|(tick, _, _)| *tick > seek_tick)
            .all(|(_, state, _)| *state != PlaybackState::Paused),
        "the player paused while the page was bringing data: {:?}",
        report.state_trace
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let after: Vec<Duration> = report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .map(|frame| frame.pts)
        .collect();
    let first = *after.first().expect("frames after the seek");
    assert!(
        first.abs_diff(target) <= FRAME,
        "first frame after the seek at {first:?}, target {target:?}"
    );
    assert!(
        after.iter().all(|pts| *pts + FRAME >= target),
        "a frame of the old place came back after the seek: {after:?}"
    );
    let expected: Vec<Duration> = (0..)
        .map(|index| first + FRAME * index)
        .take_while(|pts| *pts < CLIP)
        .collect();
    assert_eq!(after, expected, "every frame from the target to the end");
}

/// Звук после перемотки слышен (отсчёт от цели) не позже `latest_start` и звучит
/// до конца ролика.
fn assert_audio_after_seek(report: &PlaybackReport, target: Duration, latest_start: Duration) {
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
    let heard_after = Duration::from_secs_f64(first_audible as f64 / f64::from(sample_rate));
    assert!(
        heard_after <= latest_start - target + FRAME,
        "tone heard {heard_after:?} after the target, expected by {:?}",
        latest_start - target
    );
    let audible_after = after_seek.len() / channels - first_audible;
    let heard_span = Duration::from_secs_f64(audible_after as f64 / f64::from(sample_rate));
    assert!(
        heard_span + FRAME * 3 >= CLIP - latest_start,
        "the tone plays to the end of the clip: {heard_span:?}"
    );
}

/// Страница молчит дольше срока перемотки, потом присылает видео и звук с цели:
/// плеер ждёт (спиннер), садится на цель и играет сам.
#[test]
fn a_seek_waits_for_a_page_slower_than_the_commit_timeout() {
    let target = video_fragment_start(6) + FRAME * 2;
    let (report, requests) = play_with_slow_page(target, |page, ticks_since_seek| {
        if ticks_since_seek == PAGE_DELAY_TICKS {
            page.push_video(6..FRAGMENT_COUNT);
            page.push_audio(5..FRAGMENT_COUNT);
            page.end();
        }
    });

    assert_eq!(requests, [PageSeekRequest { target }]);
    assert!(report.waiting_ticks >= PAGE_DELAY_TICKS);
    assert_waited_and_played_through(&report, target);
    assert_audio_after_seek(&report, target, target);
}

/// Форма стенда 04e: видео цели пришло быстро, звук — только до цели (последний
/// кадр сегмента звука едва заходит за цель), звук после цели — дольше срока
/// перемотки. Плеер ждёт звук (`audio_preroll`), не сдаётся, садится и играет со
/// звуком, как только страница его принесла.
#[test]
fn a_seek_waits_for_audio_after_the_target_that_the_page_brings_late() {
    // Цель за 5 мс до конца сегмента звука 6: сегмент звука с целью несёт меньше
    // 50 мс звука после цели — набрать звук плеер не может, пока не придёт сегмент 7.
    // Шкала источника — целые микросекунды: цель берём на ней.
    let target = whole_micros(audio_fragment_start(7) - Duration::from_millis(5));
    let (report, requests) = play_with_slow_page(target, |page, ticks_since_seek| {
        if ticks_since_seek == 20 {
            page.push_video(6..FRAGMENT_COUNT);
            page.push_audio(6..7);
        }
        if ticks_since_seek == PAGE_DELAY_TICKS {
            page.push_audio(7..FRAGMENT_COUNT);
            page.end();
        }
    });

    assert_eq!(requests, [PageSeekRequest { target }]);
    assert_waited_and_played_through(&report, target);
    assert_audio_after_seek(&report, target, target);
}

/// Звук страница присылает только со своего сегмента ПОСЛЕ цели (правило 5, позднее
/// вступление) и дольше срока перемотки после видео: плеер ждёт, садится на цель
/// по видео и подхватывает звук в его время.
#[test]
fn a_seek_waits_for_late_joining_audio_that_arrives_after_the_timeout() {
    let target = video_fragment_start(6) + FRAME * 2;
    let first_audio_fragment = 7;
    let (report, requests) = play_with_slow_page(target, |page, ticks_since_seek| {
        if ticks_since_seek == 20 {
            page.push_video(6..FRAGMENT_COUNT);
        }
        if ticks_since_seek == PAGE_DELAY_TICKS {
            page.push_audio(first_audio_fragment..FRAGMENT_COUNT);
            page.end();
        }
    });

    assert_eq!(requests, [PageSeekRequest { target }]);
    assert_waited_and_played_through(&report, target);
    assert_audio_after_seek(&report, target, audio_fragment_start(first_audio_fragment));
}

// ---- решение 151: предел молчания живой страницы -------------------------------------

/// Короткий предел молчания страницы для тестов (по умолчанию 60 с): дольше срока
/// перемотки, чтобы «ждём дольше срока» и «сдаёмся по молчанию» различались.
const SHORT_STALL_TIMEOUT: Duration = Duration::from_millis(500);

/// Короткие сроки перемотки и молчания.
fn short_stall_config() -> PlayerTickConfig {
    PlayerTickConfig {
        seek_live_source_stall_timeout: SHORT_STALL_TIMEOUT,
        ..short_timeout_config()
    }
}

/// Страница несёт сегмент цели кусками, между кусками — меньше предела молчания, а всё
/// вместе — во много раз дольше и срока перемотки, и предела молчания: перемотка ждёт
/// без предела, пока байты идут, и садится, когда сегмент собран.
#[test]
fn a_seek_waits_while_the_page_trickles_the_target_segment_longer_than_the_stall_limit() {
    let target = video_fragment_start(6) + FRAME * 2;
    // Перерыв между кусками (шаги ≥ 5 мс): ≥ 0,25 с при пределе молчания 0,5 с — даже
    // под нагрузкой шаги растягивают перерыв, а не укорачивают.
    const TRICKLE_EVERY_TICKS: usize = 50;
    const PIECES: usize = 6;
    let mut target_segment: Option<Vec<u8>> = None;
    let (report, requests) =
        play_with_page_script(target, short_stall_config(), |page, _control, ticks| {
            let segment = target_segment.get_or_insert_with(|| page.video.fragments[6].clone());
            if ticks == 1 {
                page.push_audio(5..FRAGMENT_COUNT);
            }
            if ticks % TRICKLE_EVERY_TICKS == 0 && ticks / TRICKLE_EVERY_TICKS <= PIECES {
                let piece = ticks / TRICKLE_EVERY_TICKS - 1;
                let piece_len = segment.len().div_ceil(PIECES);
                let start = (piece * piece_len).min(segment.len());
                let end = ((piece + 1) * piece_len).min(segment.len());
                page.writer
                    .append(VIDEO, &segment[start..end])
                    .expect("a piece of the target segment");
                if piece + 1 == PIECES {
                    page.push_video(7..FRAGMENT_COUNT);
                    page.end();
                }
            }
        });

    assert_eq!(requests, [PageSeekRequest { target }]);
    // Ожидание шло дольше предела молчания (шаг — не меньше `TICK`).
    assert!(
        TICK * u32::try_from(report.waiting_ticks).expect("ticks") > SHORT_STALL_TIMEOUT,
        "waited {} ticks",
        report.waiting_ticks
    );
    assert_waited_and_played_through(&report, target);
    assert_audio_after_seek(&report, target, target);
}

/// Страница на связи, но ни байта для цели дольше предела молчания: честный отказ
/// «Seeking timed out» (плеер встаёт на паузу), а новая перемотка зрителя, когда
/// страница всё же принесла данные, садится и играет до конца.
#[test]
fn a_silent_page_ends_the_seek_with_a_timeout_and_a_new_click_retries() {
    let target = video_fragment_start(6) + FRAME * 2;
    let mut retried = false;
    let (report, requests) =
        play_with_page_script(target, short_stall_config(), |page, control, _ticks| {
            if !retried && control.state() == PlaybackState::Paused {
                retried = true;
                page.push_video(6..FRAGMENT_COUNT);
                page.push_audio(5..FRAGMENT_COUNT);
                page.end();
                // Зритель кликает снова и жмёт «играть» (отказ оставил паузу).
                control.seek(target);
                control.play();
            }
        });

    assert!(retried, "the seek never gave up on the silent page");
    assert_eq!(
        report.errors.len(),
        1,
        "exactly one seek timeout: {:?}",
        report.errors
    );
    assert!(
        report.errors[0].contains("SeekTimeout"),
        "{:?}",
        report.errors
    );
    // Перед отказом перемотка ждала дольше срока перемотки (страница жива).
    let (seek_tick, _) = report.seeks[0];
    let (retry_tick, _) = report.seeks[1];
    let waited_ticks = u32::try_from(retry_tick - seek_tick).expect("ticks");
    assert!(
        TICK * waited_ticks > SHORT_COMMIT_TIMEOUT,
        "gave up after {waited_ticks} ticks"
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let after_retry: Vec<Duration> = report
        .frames
        .iter()
        .filter(|frame| frame.tick > retry_tick)
        .map(|frame| frame.pts)
        .collect();
    assert!(
        after_retry
            .first()
            .is_some_and(|first| first.abs_diff(target) <= FRAME),
        "the retried seek landed on the target: {after_retry:?}"
    );
    assert_eq!(requests.first(), Some(&PageSeekRequest { target }));
}
