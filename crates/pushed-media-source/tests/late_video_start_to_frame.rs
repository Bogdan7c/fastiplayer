//! Видео начинается позже звука, старт без перемотки — до кадра и до конца.
//!
//! Так выглядят файлы с отложенной видеодорожкой и вход в живой поток: demux отдаёт
//! пакеты по времени, поэтому секунды звука идут раньше первого видеопакета. Раньше
//! стартовый preroll ждал кадр, звук без старта не убывал, а чтение вставало на
//! потолке звука (1,5 с): при разрыве больше потолка плеер навсегда оставался в
//! `Buffering` без единого кадра (сессия 03e моста).
//!
//! Проверяем то, что видит и слышит зритель: показан каждый видеокадр в свой момент,
//! звук записан целиком и без пауз, поток доигран до `Ended`.

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
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture, protocol_fixtures_dir};
use support::playback::{PlaybackReport, open, play, play_observed};

/// Длительность кадра фикстуры (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность фрагмента обеих фикстур: 10 фрагментов по 0,4 с.
const FRAGMENT: Duration = Duration::from_millis(400);
/// Кадров в одном фрагменте.
const FRAMES_PER_FRAGMENT: u32 = 10;
/// Фрагментов в фикстурах (4 с).
const FRAGMENT_COUNT: usize = 10;
/// Шаг «звуковой карты» и плеера — 5 мс; на старте выход может разок опустеть.
const AUDIO_SILENCE_TOLERANCE: Duration = Duration::from_millis(10);
/// Звук не короче записанного в поток минус один AAC-фрейм с запасом.
const AUDIO_LENGTH_TOLERANCE: Duration = Duration::from_millis(100);
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Собирает поток: звук с фрагмента `first_audio_fragment`, видео с `first_video_fragment`.
///
/// Каждый фрагмент видео начинается с ключевого кадра, поэтому видео с любого
/// фрагмента декодируемо. Время фрагментов исходное (без смещения), как у живого
/// потока, к которому подключились посреди.
fn play_stream_with_late_video(
    first_audio_fragment: usize,
    first_video_fragment: usize,
) -> PlaybackReport {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(FRAGMENT * FRAGMENT_COUNT as u32));
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    writer
        .append(
            VIDEO,
            &video.stream_of(first_video_fragment..FRAGMENT_COUNT),
        )
        .expect("video bytes");
    writer
        .append(
            AUDIO,
            &audio.stream_of(first_audio_fragment..FRAGMENT_COUNT),
        )
        .expect("audio bytes");
    writer.end_of_stream().expect("end of stream");
    play(open(&mut opener), |_| {})
}

/// Времена всех кадров, начиная с фрагмента `first_video_fragment` до конца.
fn expected_frame_times(first_video_fragment: usize) -> Vec<Duration> {
    let first_frame = first_video_fragment as u32 * FRAMES_PER_FRAGMENT;
    let frame_count = FRAGMENT_COUNT as u32 * FRAMES_PER_FRAGMENT;
    (first_frame..frame_count)
        .map(|index| FRAME * index)
        .collect()
}

/// Зритель увидел все кадры позднего видео вовремя и услышал весь звук.
fn assert_late_video_played_through(
    report: &PlaybackReport,
    first_audio_fragment: usize,
    first_video_fragment: usize,
) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(
        report.final_state,
        PlaybackState::Ended,
        "the stream must play to the end, not hang in preroll"
    );
    let presented: Vec<Duration> = report.frames.iter().map(|frame| frame.pts).collect();
    assert_eq!(
        presented,
        expected_frame_times(first_video_fragment),
        "every late video frame once, in order"
    );
    // Кадр показан в свой момент: ни раньше (картинка не опережает часы звука),
    // ни заметно позже (preroll не съел начало видео).
    assert!(
        report.max_av_offset() <= FRAME,
        "frame shown {:?} away from the player clock",
        report.max_av_offset()
    );
    let written_audio = FRAGMENT * (FRAGMENT_COUNT - first_audio_fragment) as u32;
    assert!(
        report.audio_duration() + AUDIO_LENGTH_TOLERANCE >= written_audio,
        "all audio before and with the video must be heard: {:?} of {written_audio:?}",
        report.audio_duration()
    );
    assert!(
        report.mid_stream_audio_silence <= AUDIO_SILENCE_TOLERANCE,
        "audio paused mid-stream for {:?}",
        report.mid_stream_audio_silence
    );
    assert_eq!(
        report.discarded_audio,
        Duration::ZERO,
        "audio output was destroyed with unplayed sound"
    );
}

/// Звук с нуля, видео с 2,0 с: разрыв больше стартового потолка звука (1,5 с).
#[test]
fn video_two_seconds_after_audio_plays_every_frame_and_all_audio() {
    let report = play_stream_with_late_video(0, 5);
    assert_late_video_played_through(&report, 0, 5);
}

/// Подключение посреди потока: звук с 0,4 с, видео с 2,0 с (разрыв 1,6 с).
#[test]
fn joined_stream_with_video_1_6_seconds_behind_audio_plays_to_the_end() {
    let report = play_stream_with_late_video(1, 5);
    assert_late_video_played_through(&report, 1, 5);
}

/// Видео с 3,2 с — дальше даже максимального потолка звука (1,8 с): фикс не
/// сдвигает порог, а убирает его для чтения, пока preroll ждёт кадр.
#[test]
fn video_beyond_the_largest_startup_audio_cap_still_plays() {
    let report = play_stream_with_late_video(0, 8);
    assert_late_video_played_through(&report, 0, 8);
}

/// Разрыв меньше потолка (звук с 0,4 с, видео с 1,6 с) работал и раньше — не ломаем.
#[test]
fn video_within_the_startup_audio_cap_keeps_playing_as_before() {
    let report = play_stream_with_late_video(1, 4);
    assert_late_video_played_through(&report, 1, 4);
}

/// Первое поколение записанного потока моста `quality-switch-new-media-source`: звук
/// 0–3 с, видео 0–2,4 с (GOP 1,2 с), конца потока нет — страница шлёт дальше, только
/// когда часы плеера дошли до 1,8 с. Голодный preroll теперь читает звук дальше
/// стартового потолка и может дочитать такой источник досуха; старт при этом не должен
/// ждать новых байтов (сессия 03e моста).
///
/// Честно: здесь decoder с двумя потоками отдаёт кадр раньше, чем источник пересохнет,
/// поэтому сам дефект «fence ждёт байтов до старта» этот тест не ловит — его ловит
/// `player-core` `session::tests::demux_retry::startup_preroll_starts_when_source_is_dry_but_audio_and_frame_are_ready`,
/// а вживую — e2e моста `generation_switch` (frame threading на всех ядрах).
#[test]
fn open_stream_with_long_gop_plays_until_the_page_sends_more() {
    let media_dir = protocol_fixtures_dir().join("media");
    let read_media = |name: &str| std::fs::read(media_dir.join(name)).expect("protocol fixture");
    let video = FragmentedMp4::split(&read_media("video-64-6s.mp4"));
    let audio = FragmentedMp4::split(&read_media("audio-6s.mp4"));
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(6)));
    writer
        .append(AUDIO, &audio.stream_of(0..3))
        .expect("audio bytes");
    writer
        .append(VIDEO, &video.stream_of(0..2))
        .expect("video bytes");

    let page_waits_for_clock = Duration::from_millis(1_800);
    let mut rest_of_stream = Some((video.fragments[2..].concat(), audio.fragments[3..].concat()));
    let report = play_observed(open(&mut opener), |observation| {
        if observation.position >= page_waits_for_clock
            && let Some((video_rest, audio_rest)) = rest_of_stream.take()
        {
            writer.append(VIDEO, &video_rest).expect("video rest");
            writer.append(AUDIO, &audio_rest).expect("audio rest");
            writer.end_of_stream().expect("end of stream");
        }
    });

    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(report.frames.len(), 150, "every frame of 6 s at 25 fps");
}

/// Наблюдение собственного прогона с явным tick config-ом.
struct AudioFirstRun {
    /// Показанные кадры: (PTS, позиция плеера в момент показа).
    frames: Vec<(Duration, Duration)>,
    /// Сколько звука было записано в выход в момент перехода в `Playing`.
    audio_at_start: Option<Duration>,
    /// Показан ли хоть один кадр к моменту перехода в `Playing`.
    frame_before_start: bool,
    /// Наибольшая очередь сжатого звука до старта (packets): это и есть запас,
    /// который ограничивает фикс. После старта очередь ведут обычные механизмы.
    max_pending_audio_packets_before_start: usize,
    /// Состояние в конце.
    final_state: PlaybackState,
    /// Записанный звук за весь прогон.
    audio_total: Duration,
    /// Тишина посреди звука.
    mid_stream_silence: Duration,
}

/// Длительность записанного PCM симулятора.
fn written_audio(device: &support::simulated_audio::SimulatedAudioDevice) -> Duration {
    let Some((sample_rate, channels)) = device.format() else {
        return Duration::ZERO;
    };
    let frames = device.written_samples().len() / channels;
    Duration::from_secs_f64(frames as f64 / f64::from(sample_rate))
}

/// Безоконный прогон, как `support::playback::play`, но с явным `PlayerTickConfig`:
/// стенд из `support` всегда берёт config по умолчанию, а запас 10 с не уложить в
/// фикстуру 4 с. Декодеры настоящие (FFmpeg с 2 потоками, AAC), выход — симулятор.
fn play_with_tick_config(
    demuxer: pushed_media_source::PushedStreamDemuxer,
    tick_config: player_core::PlayerTickConfig,
) -> AudioFirstRun {
    use player_core::{PlayerEvent, PlayerSession, PlayerTickContext, PreparedMedia};
    use std::sync::Arc;
    use std::time::Instant;

    const TICK: Duration = Duration::from_millis(5);
    const MAX_TICKS: usize = 3_000;
    let device = support::simulated_audio::SimulatedAudioDevice::default();
    let mut session = PlayerSession::with_audio_factories(
        Arc::new(audio::ProductionAudioDecoderFactory::default()),
        device.factory(),
    );
    let mut scanner = capability_core::CapabilityScanner::new();
    scanner.register_provider(Box::new(
        video_ffmpeg::FfmpegSoftwareCapabilityProvider::new(),
    ));
    let mut render = render_core::RenderCapabilities::wgpu_nv12(Some(4096));
    render.supported_frame_contracts =
        vec![video_frame_contract::VideoFrameContract::host_yuv420_planar8()];
    scanner.register_render_capabilities(render);
    session.set_system_capabilities(scanner.scan());
    session.load_prepared_media_with_autoplay(
        PreparedMedia::from_external_label("audio-first test", Box::new(demuxer)),
        true,
    );
    for event in session.take_events() {
        if let PlayerEvent::VideoBackendSelectionRequested(_) = event {
            let thread_config = video_core::VideoDecoderThreadConfig {
                software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::Fixed(
                    std::num::NonZeroUsize::new(2).expect("non-zero"),
                ),
                ..video_core::VideoDecoderThreadConfig::default()
            };
            session.set_video_backend(
                video_ffmpeg::decoder_thread::start_decoder_thread(
                    video_ffmpeg::FfmpegDecoderThreadConfig::from_thread_config(thread_config),
                )
                .expect("software FFmpeg decoder"),
            );
        }
    }

    let mut run = AudioFirstRun {
        frames: Vec::new(),
        audio_at_start: None,
        frame_before_start: false,
        max_pending_audio_packets_before_start: 0,
        final_state: PlaybackState::Idle,
        audio_total: Duration::ZERO,
        mid_stream_silence: Duration::ZERO,
    };
    let mut previous_tick = Instant::now();
    for _ in 0..MAX_TICKS {
        let now = Instant::now();
        device.advance(now.duration_since(previous_tick));
        previous_tick = now;
        let _tick_result = session.tick(PlayerTickContext::with_config(now, tick_config));
        let snapshot = session.compose_snapshot();
        if let Some(frame) = &snapshot.current_video_frame
            && run.frames.last().is_none_or(|(pts, _)| *pts != frame.pts)
        {
            run.frames.push((frame.pts, snapshot.current_position));
        }
        if snapshot.playback_state == PlaybackState::Buffering {
            run.max_pending_audio_packets_before_start = run
                .max_pending_audio_packets_before_start
                .max(snapshot.queues.pending_audio_packets);
        }
        if run.audio_at_start.is_none() && snapshot.playback_state == PlaybackState::Playing {
            run.audio_at_start = Some(written_audio(&device));
            run.frame_before_start = !run.frames.is_empty();
        }
        run.final_state = snapshot.playback_state;
        if run.final_state == PlaybackState::Ended {
            break;
        }
        std::thread::sleep(TICK);
    }
    run.audio_total = written_audio(&device);
    run.mid_stream_silence = device.mid_stream_silence();
    run
}

/// Запас исчерпан раньше первого видеопакета (видео с 2,4 с, запас 0,5 с сверх потолка
/// 1,5 с): звук стартует первым без картинки, видео подключается в свой PTS, поток
/// доигран до `Ended`, очередь сжатого звука ограничена (ревью 03e, путь защёлки).
#[test]
fn exhausted_backlog_starts_audio_first_and_video_joins_at_its_pts() {
    let first_video_fragment = 6;
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(FRAGMENT * FRAGMENT_COUNT as u32));
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    writer
        .append(
            VIDEO,
            &video.stream_of(first_video_fragment..FRAGMENT_COUNT),
        )
        .expect("video bytes");
    writer
        .append(AUDIO, &audio.stream_of(0..FRAGMENT_COUNT))
        .expect("audio bytes");
    writer.end_of_stream().expect("end of stream");
    let backlog_limit = Duration::from_millis(500);
    let tick_config = player_core::PlayerTickConfig {
        startup_video_wait_max_audio_backlog: backlog_limit,
        ..player_core::PlayerTickConfig::default()
    };

    let run = play_with_tick_config(open(&mut opener), tick_config);

    assert_eq!(run.final_state, PlaybackState::Ended);
    let audio_at_start = run.audio_at_start.expect("playback must start");
    assert!(
        !run.frame_before_start,
        "audio must start first, before any video frame exists"
    );
    assert!(
        run.audio_total > audio_at_start + Duration::from_secs(1),
        "audio keeps being written after the start: {audio_at_start:?} -> {:?}",
        run.audio_total
    );
    assert!(
        run.audio_total + AUDIO_LENGTH_TOLERANCE >= FRAGMENT * FRAGMENT_COUNT as u32,
        "all audio heard: {:?}",
        run.audio_total
    );
    assert!(
        run.mid_stream_silence <= AUDIO_SILENCE_TOLERANCE,
        "audio paused mid-stream for {:?}",
        run.mid_stream_silence
    );
    let presented: Vec<Duration> = run.frames.iter().map(|(pts, _)| *pts).collect();
    assert_eq!(
        presented,
        expected_frame_times(first_video_fragment),
        "every late video frame once, in order"
    );
    for (pts, position) in &run.frames {
        assert!(
            pts.abs_diff(*position) <= FRAME,
            "frame {pts:?} shown at player position {position:?}"
        );
    }
    // Запас 0,5 с ≈ 24 AAC-пакета по 21 мс; предел мягкий — плюс read-ы одного tick-а.
    assert!(
        run.max_pending_audio_packets_before_start <= 32,
        "compressed audio backlog before the start must stay bounded: {} packets",
        run.max_pending_audio_packets_before_start
    );
}
