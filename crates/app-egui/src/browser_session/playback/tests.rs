//! Сквозные проверки воспроизведения браузерного режима: настоящий `PlayerWorker`,
//! настоящий программный FFmpeg, настоящий декодер звука, тихая карта реального
//! времени и безоконный вывод. Проверяется то, что увидел и услышал бы зритель.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use fastiplayer_config::AppConfig;

use super::metered_audio::AudioOutputStatus;
use super::video_output::{HeadlessVideoOutput, PresentTally, software_only_capabilities};
use super::*;

/// Шаг главного цикла в тестах (настоящее время, как у процесса).
const PUMP_STEP: Duration = Duration::from_millis(5);
/// Предел ожидания одного сценария.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(20);
/// Допуск позиции после точной перемотки: кадр фикстуры 30 к/с ≈ 33 мс, плюс шаг цикла.
const SEEK_LANDING_TOLERANCE: Duration = Duration::from_millis(150);

/// Сколько ждать, пока доиграет звук, уже записанный в выход до смены скорости.
///
/// Плеер не сбрасывает очередь выхода при смене скорости: в начале воспроизведения в ней
/// лежит запас старта (`audio.startup_read_ahead_cap_ms`, по умолчанию 1,5 с), и он
/// доигрывается на прежней скорости (замерено: 2× наступает через ~1,25 с). 2 с — с запасом.
const AUDIO_QUEUE_DRAIN: Duration = Duration::from_secs(2);

/// Считает пробуждения главного цикла.
#[derive(Default)]
struct CountingWake(AtomicUsize);

impl BrowserSessionWake for CountingWake {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Фикстура репозитория (относительно crate-а app-egui).
fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

/// 1 с, H.264 64×64 10 к/с (ровно 10 кадров) + AC-3 5.1.
fn one_second_clip() -> PathBuf {
    fixture("symphonia-demux/tests/fixtures/ac3/h264-ac3-5p1.mp4")
}

/// 20 с, H.264 30 к/с с B-кадрами, IDR каждые 2 с, AAC — перемотка и пауза.
fn twenty_second_clip() -> PathBuf {
    fixture("symphonia-demux/tests/fixtures/seek-gop/gop2s-bframes.mp4")
}

/// Настройки с фиксированным числом потоков FFmpeg: на любой машине одинаково.
///
/// По умолчанию FFmpeg берёт почти все ядра и держит столько же кадров в конвейере —
/// у 10-кадрового ролика первый кадр появился бы только в конце потока.
fn test_settings() -> BrowserSessionSettings {
    let mut config = AppConfig::default();
    config.video.sw_decode_threads = 2;
    BrowserSessionSettings::from_app_config_for_test(config)
}

/// Открывает локальный файл существующим реестром демуксеров (symphonia и др.).
fn prepared(path: &Path, settings: &BrowserSessionSettings) -> PreparedMedia {
    let config = settings.app_config();
    media_source_open::local_media::prepare_local_file(
        path,
        &config.player.demux,
        media_source_open::local_media::attachment_read_limits_from_config(&config.subtitles),
    )
    .expect("fixture opens")
}

/// Плеер с безоконным выводом и тихой картой.
fn start_headless(settings: &BrowserSessionSettings) -> (BrowserPlayback, Arc<CountingWake>) {
    let video_output = HeadlessVideoOutput::new(decoder_thread_config_from_settings(settings));
    start_with_output(settings, Box::new(video_output))
}

fn start_with_output(
    settings: &BrowserSessionSettings,
    video_output: Box<dyn BrowserVideoOutput>,
) -> (BrowserPlayback, Arc<CountingWake>) {
    let wake = Arc::new(CountingWake::default());
    let playback = BrowserPlayback::start(
        settings,
        video_output,
        BrowserAudioOutputChoice::SimulatedRealTime,
        wake.clone(),
        Instant::now(),
    )
    .expect("browser playback starts");
    (playback, wake)
}

/// Крутит главный цикл, пока условие не выполнится; возвращает последнее наблюдение.
fn pump_until(
    playback: &mut BrowserPlayback,
    what: &str,
    mut done: impl FnMut(&PlaybackObservation, &PlaybackSummary) -> bool,
) -> PlaybackObservation {
    let deadline = Instant::now() + SCENARIO_DEADLINE;
    loop {
        let observation = playback.pump(Instant::now());
        if done(&observation, &playback.summary()) {
            return observation;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {observation:?} {}",
            playback.summary()
        );
        thread::sleep(PUMP_STEP);
    }
}

/// Крутит главный цикл заданное время.
fn pump_for(playback: &mut BrowserPlayback, duration: Duration) -> PlaybackObservation {
    let until = Instant::now() + duration;
    loop {
        let observation = playback.pump(Instant::now());
        if Instant::now() >= until {
            return observation;
        }
        thread::sleep(PUMP_STEP);
    }
}

/// Ролик проходит весь путь: каждый кадр забран выводом, звук прозвучал, конец достигнут.
#[test]
fn local_clip_plays_to_the_end_through_the_real_player() {
    let settings = test_settings();
    let (mut playback, wake) = start_headless(&settings);
    playback
        .load(prepared(&one_second_clip(), &settings), LoadStart::Playing)
        .expect("load");

    let last = pump_until(&mut playback, "the end of the clip", |observation, _| {
        observation.ended || observation.fault.is_some()
    });
    let summary = playback.finish();

    assert_eq!(last.fault, None);
    assert!(last.ended);
    assert_eq!(summary.state, PlaybackState::Ended);
    let counters = summary.counters;
    assert_eq!(counters.presented_frames, 10, "{summary}");
    assert_eq!(
        counters.taken_frames, 10,
        "every presented frame is taken: {summary}"
    );
    assert_eq!(counters.position_regressions, 0, "{summary}");
    assert_eq!(counters.loads, 1);
    assert_eq!(summary.audio.output, AudioOutputStatus::Simulated);
    assert!(
        summary.audio.written >= Duration::from_millis(900),
        "almost the whole second of sound is written: {summary}"
    );
    assert!(
        summary.audio.played >= Duration::from_millis(800),
        "the sound actually played in real time: {summary}"
    );
    assert!(
        wake.0.load(Ordering::SeqCst) > 0,
        "the player wakes the main loop on new frames"
    );
}

/// Пауза останавливает показ и позицию; play продолжает с того же места.
#[test]
fn pause_stops_presentation_and_play_resumes_it() {
    let settings = test_settings();
    let (mut playback, _wake) = start_headless(&settings);
    playback
        .load(
            prepared(&twenty_second_clip(), &settings),
            LoadStart::Playing,
        )
        .expect("load");
    pump_until(&mut playback, "the first frames", |_, summary| {
        summary.counters.taken_frames >= 10
    });

    playback.pause().expect("pause");
    let paused = pump_until(&mut playback, "the paused state", |observation, _| {
        observation.state == PlaybackState::Paused
    });
    // Ещё один шаг: кадр, уже стоявший в очереди показа в момент паузы, забирается здесь.
    pump_for(&mut playback, Duration::from_millis(50));
    let taken_at_pause = playback.summary().counters.taken_frames;
    let after_wait = pump_for(&mut playback, Duration::from_millis(500));

    assert_eq!(
        playback.summary().counters.taken_frames,
        taken_at_pause,
        "no frame is shown while paused"
    );
    assert_eq!(after_wait.position, paused.position, "time stands still");

    playback.play().expect("play");
    pump_until(&mut playback, "frames after resume", |_, summary| {
        summary.counters.taken_frames >= taken_at_pause + 5
    });
    let summary = playback.finish();
    assert_eq!(summary.counters.position_regressions, 0, "{summary}");
}

/// Перемотка вперёд и назад: позиция у цели, кадры идут дальше, откатом не считается.
#[test]
fn seek_lands_near_the_target_without_counting_a_regression() {
    let settings = test_settings();
    let (mut playback, _wake) = start_headless(&settings);
    playback
        .load(
            prepared(&twenty_second_clip(), &settings),
            LoadStart::Playing,
        )
        .expect("load");
    pump_until(&mut playback, "playback start", |observation, _| {
        observation.state == PlaybackState::Playing && observation.position > Duration::ZERO
    });

    for target in [Duration::from_secs(15), Duration::from_secs(3)] {
        playback.seek(target).expect("seek");
        // Перемотка, которая не дошла до цели, упрётся в предел ожидания.
        let landed = pump_until(&mut playback, "the seek landing", |observation, _| {
            observation.state == PlaybackState::Playing
                && observation.position >= target
                && observation.position - target <= SEEK_LANDING_TOLERANCE
        });
        let taken_after_landing = playback.summary().counters.taken_frames;
        let moving = pump_for(&mut playback, Duration::from_millis(300));
        assert!(
            moving.position > landed.position,
            "time moves on after the seek"
        );
        assert!(
            playback.summary().counters.taken_frames > taken_after_landing,
            "frames keep coming after the seek"
        );
    }

    let summary = playback.finish();
    assert_eq!(
        summary.counters.position_regressions, 0,
        "a seek back is not a regression: {summary}"
    );
    assert_eq!(summary.counters.loads, 1);
}

/// Скорость 2× от страницы: время медиа идёт вдвое быстрее настоящего.
#[test]
fn doubled_rate_moves_media_time_twice_as_fast() {
    let settings = test_settings();
    let (mut playback, _wake) = start_headless(&settings);
    playback
        .load(
            prepared(&twenty_second_clip(), &settings),
            LoadStart::Playing,
        )
        .expect("load");
    pump_until(&mut playback, "playback start", |observation, _| {
        observation.state == PlaybackState::Playing
            && observation.position >= Duration::from_millis(200)
    });

    let double = PlaybackRate::new(2.0).expect("valid rate");
    playback.set_rate(double).expect("set rate");
    let at_rate = pump_until(&mut playback, "the rate change", |observation, _| {
        observation.rate == double
    });
    // Звук, уже стоявший в очереди выхода на 1×, доигрывается на 1×: замер — после него.
    let settled = pump_for(&mut playback, AUDIO_QUEUE_DRAIN);
    let wall_start = Instant::now();
    let after = pump_for(&mut playback, Duration::from_secs(1));
    let wall = wall_start.elapsed();
    let summary = playback.finish();

    assert!(at_rate.rate == double);
    let media_advance = after.position - settled.position;
    assert!(
        media_advance.as_secs_f64() >= 1.6 * wall.as_secs_f64(),
        "media advanced {media_advance:?} in {wall:?} of real time at 2x"
    );
    assert_eq!(summary.counters.position_regressions, 0, "{summary}");
}

/// Вывод, который не умеет показывать ни один формат кадров FFmpeg: H.264 не играет.
struct NoFrameFormatOutput {
    inner: HeadlessVideoOutput,
}

impl BrowserVideoOutput for NoFrameFormatOutput {
    fn capabilities(&self) -> SystemCapabilities {
        let mut render = render_core::RenderCapabilities::wgpu_nv12(None);
        render.supported_frame_contracts.clear();
        software_only_capabilities(render)
    }

    fn start_backend(
        &mut self,
        request: VideoBackendStartRequest<'_>,
    ) -> Result<VideoBackendStart, VideoBackendStartError> {
        match request {
            VideoBackendStartRequest::Startup => self.inner.start_backend(request),
            VideoBackendStartRequest::Stream(_) => Ok(VideoBackendStart::NoCompatibleBackend),
        }
    }

    fn present(&mut self, player: &PlayerWorker) -> PresentTally {
        self.inner.present(player)
    }
}

/// Видео, которое вывод не может показать, даёт наблюдаемый отказ «неподдерживаемое видео».
#[test]
fn undecodable_video_surfaces_as_unsupported_video() {
    let settings = test_settings();
    let video_output = NoFrameFormatOutput {
        inner: HeadlessVideoOutput::new(decoder_thread_config_from_settings(&settings)),
    };
    let (mut playback, _wake) = start_with_output(&settings, Box::new(video_output));
    playback
        .load(prepared(&one_second_clip(), &settings), LoadStart::Playing)
        .expect("load");

    let observation = pump_until(
        &mut playback,
        "the unsupported verdict",
        |observation, _| observation.fault.is_some() || observation.ended,
    );
    let summary = playback.finish();

    assert_eq!(
        observation.fault.map(|load_fault| load_fault.fault),
        Some(BrowserPlaybackFault::UnsupportedVideo)
    );
    assert_eq!(
        summary.counters.taken_frames, 0,
        "no frame of it is shown: {summary}"
    );
}

/// Новая загрузка сбрасывает отказ и считается новым поколением.
#[test]
fn second_load_is_a_new_generation_and_plays() {
    let settings = test_settings();
    let (mut playback, _wake) = start_headless(&settings);
    let first_load = playback
        .load(
            prepared(&twenty_second_clip(), &settings),
            LoadStart::Playing,
        )
        .expect("first load");
    let first_playing = pump_until(
        &mut playback,
        "the first generation playing",
        |observation, _| observation.position >= Duration::from_millis(500),
    );
    assert!(first_playing.shows(first_load), "{first_playing:?}");

    let second_load = playback
        .load(prepared(&one_second_clip(), &settings), LoadStart::Playing)
        .expect("second load");
    assert_ne!(first_load, second_load);
    // Сразу после команды плеер ещё показывает первое медиа: наблюдение не может
    // быть про вторую загрузку, пока плеер её не поставил.
    let right_after_load = playback.pump(Instant::now());
    assert!(
        !right_after_load.shows(second_load)
            || right_after_load.position < Duration::from_millis(500),
        "{right_after_load:?}"
    );
    let last = pump_until(
        &mut playback,
        "the second generation end",
        |observation, _| observation.ended || observation.fault.is_some(),
    );
    let summary = playback.finish();

    assert!(last.ended, "{last:?}");
    assert!(
        last.shows(second_load) && !last.shows(first_load),
        "the end belongs to the second load: {last:?}"
    );
    assert_eq!(summary.counters.loads, 2);
    assert_eq!(
        summary.counters.position_regressions, 0,
        "a new generation starting from zero is not a regression: {summary}"
    );
}

/// Демуксер, который после `packets_left` событий ломается ошибкой чтения —
/// как поток, чьи байты посреди просмотра перестали разбираться.
struct BreakingDemuxer {
    inner: symphonia_demux::SymphoniaDemuxer,
    events_left: usize,
}

impl media_core::Demuxer for BreakingDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        media_core::Demuxer::tracks(&self.inner)
    }

    fn duration(&self) -> Option<Duration> {
        media_core::Demuxer::duration(&self.inner)
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        let Some(left) = self.events_left.checked_sub(1) else {
            anyhow::bail!("the stream broke in the middle");
        };
        self.events_left = left;
        media_core::Demuxer::next_event(&mut self.inner)
    }

    fn seek(&mut self, target: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        media_core::Demuxer::seek(&mut self.inner, target)
    }
}

/// Медиа, которое сломается после первых пакетов.
fn breaking_media(path: &Path) -> PreparedMedia {
    let inner = symphonia_demux::SymphoniaDemuxer::from_file(path).expect("fixture opens");
    PreparedMedia::from_external_label(
        "breaking stream",
        Box::new(BreakingDemuxer {
            inner,
            events_left: 80,
        }),
    )
}

/// Прежнее медиа упало (`Failed` в снимке и ошибка в событиях), следом загружено
/// исправное: его отказом прежнее не считается ни на одном шаге — оно играет до
/// конца без отказа (ревью 03b: YouTube — нечитаемая реклама, потом ролик).
#[test]
fn a_failed_previous_media_is_never_the_fault_of_the_next_load() {
    let settings = test_settings();
    let (mut playback, _wake) = start_headless(&settings);
    let broken_load = playback
        .load(breaking_media(&twenty_second_clip()), LoadStart::Playing)
        .expect("first load");
    let failed = pump_until(
        &mut playback,
        "the first media failure",
        |observation, _| observation.fault.is_some() || observation.ended,
    );
    assert!(
        matches!(
            failed.fault_of(broken_load),
            Some(BrowserPlaybackFault::PlaybackFailed(_))
        ),
        "{failed:?}"
    );

    let good_load = playback
        .load(prepared(&one_second_clip(), &settings), LoadStart::Playing)
        .expect("second load");
    let mut foreign_faults = Vec::new();
    let last = pump_until(&mut playback, "the second media end", |observation, _| {
        if let Some(fault) = observation.fault_of(good_load) {
            foreign_faults.push(fault.clone());
        }
        observation.ended && observation.shows(good_load)
    });
    let summary = playback.finish();

    assert_eq!(foreign_faults, Vec::new(), "{summary}");
    assert_eq!(last.fault_of(good_load), None);
    assert_eq!(summary.counters.loads, 2);
    assert!(summary.counters.presented_frames >= 10, "{summary}");
}

/// Загрузка «на паузе» (страница на паузе): ни звука, ни движения времени —
/// а play запускает то же медиа (решение 99 не нарушено: старт по слову страницы).
#[test]
fn a_paused_load_makes_no_sound_until_play() {
    let settings = test_settings();
    let (mut playback, _wake) = start_headless(&settings);
    let load = playback
        .load(
            prepared(&twenty_second_clip(), &settings),
            LoadStart::Paused,
        )
        .expect("load");
    pump_until(&mut playback, "the paused install", |observation, _| {
        observation.shows(load)
    });
    let paused = pump_for(&mut playback, Duration::from_millis(700));
    assert_eq!(paused.state, PlaybackState::Paused, "{paused:?}");
    assert_eq!(paused.position, Duration::ZERO);
    assert_eq!(
        playback.summary().audio.played,
        Duration::ZERO,
        "no sound slipped out: {}",
        playback.summary()
    );

    playback.play().expect("play");
    pump_until(&mut playback, "sound after play", |observation, summary| {
        observation.state == PlaybackState::Playing
            && summary.audio.played >= Duration::from_millis(200)
    });
    playback.finish();
}

/// Ядро принимает play во время перемотки: перемотка после конца (она кончается
/// паузой) и сразу play — медиа снова играет и показывает кадры (ревью 03b, п. 10).
#[test]
fn play_right_after_a_seek_from_the_end_plays_again() {
    let settings = test_settings();
    let (mut playback, _wake) = start_headless(&settings);
    playback
        .load(prepared(&one_second_clip(), &settings), LoadStart::Playing)
        .expect("load");
    pump_until(&mut playback, "the end", |observation, _| observation.ended);
    let taken_at_end = playback.summary().counters.taken_frames;

    playback.seek(Duration::ZERO).expect("seek");
    playback.play().expect("play");
    pump_until(
        &mut playback,
        "frames after the replay",
        |observation, summary| {
            observation.state != PlaybackState::Ended
                && summary.counters.taken_frames >= taken_at_end + 5
        },
    );
    let replayed = pump_until(&mut playback, "the second end", |observation, _| {
        observation.ended
    });
    let summary = playback.finish();
    assert!(replayed.ended);
    assert!(
        summary.counters.taken_frames >= taken_at_end + 5,
        "{summary}"
    );
}
