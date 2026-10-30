//! Звук начинается позже видео больше чем на секунду — до кадра и до звука в выходе.
//!
//! Разрыв до 1 с плеер закрывает тишиной (`audio_leading_gap_to_frame.rs`). Больше
//! тишиной не закрыть, поэтому звук ждёт своего времени сжатым в очереди, видео идёт
//! само по себе (часы без звука), а звук входит ровно в свой PTS и забирает часы.
//!
//! Проверяется то, что увидит и услышит зритель, а не `max_av_offset()`: тот сравнивает
//! кадр с часами и сдвиг звука относительно видео не видит. Поэтому прогон здесь свой:
//! на каждом шаге записываются настоящее время, позиция плеера, кадр на экране и сколько
//! PCM уже ушло в выход. Шаг, на котором в выход ушёл первый PCM, даёт позицию, в которой
//! этот PCM зазвучал: выход только что создан (или пуст после перемотки), и его первый
//! sample звучит в позиции привязки часов — это и есть позиция плеера на этом шаге.
//!
//! MPEG-TS от ffmpeg (`muxed-h264-aac.ts`) — другой случай того же разрыва: оба потока
//! начинаются с ~1,4 с при шкале с нуля. Это не отложенный звук, а поздно начавшаяся
//! запись, и показ стартует с её начала, без 1,4 с чёрного экрана.
//!
//! Фикстуры: `video-h264-64-4s.mp4` и `audio-aac-4s.mp4` (синус), по десять фрагментов
//! по 0,4 с; фрагмент звука N начинается в N × 19 × 1024 / 48000 с (≈ N × 0,405 с).
//!
//! Перемотка в место, где звук цели начинается позже: видео идёт с цели, звук входит в
//! свой PTS. Первый PCM после перемотки ложится в пустой (очищенный) выход и звучит в
//! позиции плеера на шаге записи — так же, как на старте.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::ops::Range;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use player_core::{
    PlaybackState, PlayerCommand, PlayerEvent, PlayerSession, PlayerTickContext, PreparedMedia,
    SeekRequest,
};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamDemuxer,
    TimestampOffset, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::open;
use support::simulated_audio::SimulatedAudioDevice;

/// Длительность кадра фикстуры (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Фрагментов в фикстурах (4 с).
const FRAGMENT_COUNT: usize = 10;
/// Длительность фрагмента обеих фикстур.
const FRAGMENT: Duration = Duration::from_millis(400);
/// Через сколько шагов после перемотки «страница» присылает данные цели.
const PAGE_RESPONSE_TICKS: usize = 20;
/// Кадров в ролике.
const FRAME_COUNT: u64 = 100;
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
/// AAC-кадров в одном фрагменте звука фикстуры.
const AAC_FRAMES_PER_FRAGMENT: u64 = 19;
/// Sample-ов в одном AAC-кадре.
const AAC_FRAME_SAMPLES: u64 = 1_024;
/// Частота звука фикстуры.
const AUDIO_SAMPLE_RATE: u64 = 48_000;
/// Порог «слышно»: синус фикстуры громче 0,5, тишина — ровно ноль.
const AUDIBLE: f32 = 0.05;
/// Точность времени звука относительно видео: один кадр видео.
const AUDIO_TIME_TOLERANCE: Duration = FRAME;
/// Точность длины записанного звука: AAC-кадр с запасом.
const AUDIO_LENGTH_TOLERANCE: Duration = Duration::from_millis(100);
/// Точность темпа видео без звука (часы по настоящему времени): два кадра — шаг
/// плеера плюс задержки потока теста.
const PACE_TOLERANCE: Duration = Duration::from_millis(80);
/// Шаг «звуковой карты» и плеера — 5 мс; на старте выход может разок опустеть.
const AUDIO_SILENCE_TOLERANCE: Duration = Duration::from_millis(10);
/// Пауза между шагами плеера (настоящее время, как в общей обвязке).
const TICK: Duration = Duration::from_millis(5);
/// Предел шагов: ~10 с на ролик 4 с.
const MAX_TICKS: usize = 2_000;
/// Потоков программного декодера (как в общей обвязке: одинаково на любой машине).
const DECODER_THREADS: std::num::NonZeroUsize = std::num::NonZeroUsize::new(2).expect("non-zero");
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Время первого sample-а фрагмента звука `index` фикстуры.
fn audio_fragment_start(index: usize) -> Duration {
    let samples = AAC_FRAMES_PER_FRAGMENT * AAC_FRAME_SAMPLES * index as u64;
    Duration::from_nanos(samples * 1_000_000_000 / AUDIO_SAMPLE_RATE)
}

/// Поток: звук из фрагментов `audio` (со смещением меток), видео из `video`.
fn stream(
    audio: Range<usize>,
    audio_offset: TimestampOffset,
    video: Option<Range<usize>>,
) -> PushedStreamDemuxer {
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
    writer
        .set_timestamp_offset(AUDIO, audio_offset)
        .expect("audio offset");
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

/// Что было после одного шага плеера.
#[derive(Debug, Clone, Copy)]
struct TickRecord {
    /// Настоящее время шага.
    wall: Instant,
    /// Позиция плеера после шага.
    position: Duration,
    /// Состояние после шага.
    state: PlaybackState,
    /// Сколько PCM-кадров ушло в выход к концу шага (всего за прогон).
    written_frames: usize,
    /// Кадр на экране после шага.
    on_screen: Option<Duration>,
}

/// Итог прогона.
struct Run {
    /// Записи шагов по порядку.
    ticks: Vec<TickRecord>,
    /// Сумма показов кадров (`video_frames_presented`).
    presented_count: u64,
    /// Записанный PCM.
    pcm: Vec<f32>,
    /// Частота и каналы выхода.
    format: Option<(u32, usize)>,
    /// Ошибки плеера.
    errors: Vec<String>,
    /// Тишина посреди звука.
    mid_stream_silence: Duration,
    /// Шаг, на котором была команда перемотки.
    seek_tick: Option<usize>,
}

/// Возможности, как у приложения с программным декодом.
fn software_only_capabilities() -> capability_core::SystemCapabilities {
    let mut scanner = capability_core::CapabilityScanner::new();
    scanner.register_provider(Box::new(
        video_ffmpeg::FfmpegSoftwareCapabilityProvider::new(),
    ));
    let mut render = render_core::RenderCapabilities::wgpu_nv12(Some(4096));
    render.supported_frame_contracts =
        vec![video_frame_contract::VideoFrameContract::host_yuv420_planar8()];
    scanner.register_render_capabilities(render);
    scanner.scan()
}

/// Настоящий программный декодер видео.
fn start_software_video_decoder() -> player_core::StartedVideoBackend {
    let thread_config = video_core::VideoDecoderThreadConfig {
        software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::Fixed(
            DECODER_THREADS,
        ),
        ..video_core::VideoDecoderThreadConfig::default()
    };
    video_ffmpeg::decoder_thread::start_decoder_thread(
        video_ffmpeg::FfmpegDecoderThreadConfig::from_thread_config(thread_config),
    )
    .expect("software FFmpeg decoder")
}

/// Перемотка по ходу прогона: когда позиция дошла до `at`, «зритель» жмёт перемотку.
#[derive(Debug, Clone, Copy)]
struct SeekPlan {
    /// Позиция, на которой нажата перемотка.
    at: Duration,
    /// Цель перемотки.
    target: Duration,
}

/// Играет поток до конца в безоконной сессии с настоящими декодерами.
fn play(demuxer: PushedStreamDemuxer) -> Run {
    play_with_seek(demuxer, None, |_| {})
}

/// Как [`play`], но с перемоткой по ходу прогона; `after_seek` зовётся после каждого
/// шага, начиная с перемотки, с числом шагов после неё (так «страница» шлёт байты).
fn play_with_seek(
    demuxer: PushedStreamDemuxer,
    seek: Option<SeekPlan>,
    mut after_seek: impl FnMut(usize),
) -> Run {
    let device = SimulatedAudioDevice::default();
    let mut session = PlayerSession::with_audio_factories(
        Arc::new(audio::ProductionAudioDecoderFactory::default()),
        device.factory(),
    );
    session.set_system_capabilities(software_only_capabilities());
    let prepared = PreparedMedia::from_external_label("large audio gap", Box::new(demuxer));
    session.load_prepared_media_with_autoplay(prepared, true);
    for event in session.take_events() {
        if let PlayerEvent::VideoBackendSelectionRequested(_) = event {
            session.set_video_backend(start_software_video_decoder());
        }
    }

    let mut ticks = Vec::new();
    let mut errors = Vec::new();
    let mut presented_count = 0;
    let mut seek_tick = None;
    let mut previous_tick = Instant::now();
    for tick in 0..MAX_TICKS {
        let now = Instant::now();
        // Как общая обвязка: «карта» играет не больше шага за шаг.
        device.advance(now.duration_since(previous_tick).min(TICK));
        previous_tick = now;
        let result = session.tick(PlayerTickContext::new(now));
        presented_count += result.video_frames_presented;
        let snapshot = session.compose_snapshot();
        for event in session.take_events() {
            if let PlayerEvent::RecoverableError(error) | PlayerEvent::FatalError(error) = event {
                errors.push(format!("{error:?}"));
            }
        }
        ticks.push(TickRecord {
            wall: now,
            position: snapshot.current_position,
            state: snapshot.playback_state,
            written_frames: device
                .format()
                .map_or(0, |(_, channels)| device.written_samples().len() / channels),
            on_screen: snapshot.current_video_frame.as_ref().map(|frame| frame.pts),
        });
        if let Some(plan) = seek
            && seek_tick.is_none()
            && snapshot.playback_state == PlaybackState::Playing
            && snapshot.current_position >= plan.at
        {
            session
                .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                    media_core::MediaTime::from_duration(plan.target),
                )))
                .expect("seek command accepted");
            seek_tick = Some(tick);
        }
        if let Some(seek_tick) = seek_tick {
            after_seek(tick - seek_tick);
        }
        if matches!(
            snapshot.playback_state,
            PlaybackState::Ended | PlaybackState::Failed
        ) {
            break;
        }
        thread::sleep(TICK);
    }
    Run {
        ticks,
        presented_count,
        pcm: device.written_samples(),
        format: device.format(),
        errors,
        mid_stream_silence: device.mid_stream_silence(),
        seek_tick,
    }
}

impl Run {
    /// Состояние в конце прогона.
    fn final_state(&self) -> PlaybackState {
        self.ticks
            .last()
            .map_or(PlaybackState::Idle, |tick| tick.state)
    }

    /// Частота и каналы выхода.
    fn audio_format(&self) -> (u32, usize) {
        self.format.expect("audio output was created")
    }

    /// Время PCM-кадров в выходе.
    fn pcm_duration(&self, frames: usize) -> Duration {
        let (sample_rate, _) = self.audio_format();
        Duration::from_secs_f64(frames as f64 / f64::from(sample_rate))
    }

    /// Номер первого слышимого PCM-кадра в выходе.
    fn first_audible_frame(&self) -> Option<usize> {
        self.first_audible_frame_from(0)
    }

    /// Номер первого слышимого PCM-кадра, начиная с кадра `from_frame`.
    fn first_audible_frame_from(&self, from_frame: usize) -> Option<usize> {
        let (_, channels) = self.audio_format();
        self.pcm
            .chunks_exact(channels)
            .enumerate()
            .skip(from_frame)
            .find(|(_, frame)| frame.iter().any(|sample| sample.abs() > AUDIBLE))
            .map(|(index, _)| index)
    }

    /// Всего записано PCM-кадров.
    fn written_frames(&self) -> usize {
        let (_, channels) = self.audio_format();
        self.pcm.len() / channels
    }

    /// Медиа-время, в которое зазвучал первый слышимый sample.
    fn media_time_of_first_audible(&self) -> Duration {
        self.media_time_of_first_audible_after(None)
    }

    /// Медиа-время, в которое зазвучал первый слышимый sample, записанный после шага
    /// `after_tick` (`None` — с начала прогона).
    ///
    /// Первый PCM ложится в пустой выход (только что созданный или очищенный перемоткой)
    /// и звучит в позиции привязки часов — позиции плеера на шаге записи. Слышимый
    /// sample дальше него на `k` PCM-кадров звучит на `k` кадров позже.
    fn media_time_of_first_audible_after(&self, after_tick: Option<usize>) -> Duration {
        let frames_before = after_tick.map_or(0, |tick| self.ticks[tick].written_frames);
        let first_tick = after_tick.map_or(0, |tick| tick + 1);
        let write_tick = self.ticks[first_tick..]
            .iter()
            .find(|tick| tick.written_frames > frames_before)
            .expect("audio was written");
        let audible = self
            .first_audible_frame_from(frames_before)
            .expect("the tone must be heard");
        write_tick.position + self.pcm_duration(audible - frames_before)
    }

    /// Кадры на экране по порядку смены и шаг, на котором каждый появился.
    fn on_screen_frames(&self) -> Vec<(Duration, &TickRecord)> {
        let mut frames: Vec<(Duration, &TickRecord)> = Vec::new();
        for tick in &self.ticks {
            if let Some(pts) = tick.on_screen
                && frames.last().is_none_or(|(last, _)| *last != pts)
            {
                frames.push((pts, tick));
            }
        }
        frames
    }
}

/// `actual` совпадает с `expected` с точностью `tolerance`.
fn assert_near(actual: Duration, expected: Duration, tolerance: Duration, what: &str) {
    assert!(
        actual.abs_diff(expected) <= tolerance,
        "{what}: {actual:?}, expected {expected:?} ± {tolerance:?}"
    );
}

/// Доиграно без ошибок до `Ended`, последний кадр показан.
fn assert_played_to_the_end(run: &Run) {
    assert!(run.errors.is_empty(), "player errors: {:?}", run.errors);
    assert_eq!(run.final_state(), PlaybackState::Ended);
}

/// Кадры на экране идут по возрастанию, без возвратов, до последнего кадра ролика.
fn assert_frames_in_order(frames: &[(Duration, &TickRecord)], first: Duration) {
    assert_frames_in_order_until(frames, first, FRAME * 99);
}

/// Кадры на экране идут по возрастанию, без возвратов, от `first` до `last`.
fn assert_frames_in_order_until(
    frames: &[(Duration, &TickRecord)],
    first: Duration,
    last: Duration,
) {
    let presented: Vec<Duration> = frames.iter().map(|(pts, _)| *pts).collect();
    assert_near(presented[0], first, FRAME, "first frame");
    assert!(
        presented.windows(2).all(|pair| pair[0] < pair[1]),
        "frames go forward: {presented:?}"
    );
    assert_eq!(presented.last().copied(), Some(last), "the last frame");
}

/// Видео до `until` идёт в темпе настоящего времени (часы без звука).
fn assert_video_paced_by_wall_clock(frames: &[(Duration, &TickRecord)], until: Duration) {
    let (first_pts, first_tick) = frames[0];
    for (pts, tick) in frames.iter().filter(|(pts, _)| *pts < until) {
        let wall = tick.wall.duration_since(first_tick.wall);
        assert_near(
            wall,
            *pts - first_pts,
            PACE_TOLERANCE,
            &format!("frame {pts:?} shown after"),
        );
    }
}

/// Звук с ~2,03 с, видео с нуля: кадры с нуля идут сами, звук звучит в свой PTS.
#[test]
fn audio_two_seconds_after_video_joins_at_its_pts() {
    let first_audio_fragment = 5;
    let audio_start = audio_fragment_start(first_audio_fragment);
    let run = play(stream(
        first_audio_fragment..FRAGMENT_COUNT,
        TimestampOffset::ZERO,
        Some(0..FRAGMENT_COUNT),
    ));

    assert_played_to_the_end(&run);
    assert_near(
        run.media_time_of_first_audible(),
        audio_start,
        AUDIO_TIME_TOLERANCE,
        "the first tone sample sounds at its own time relative to video",
    );
    let frames = run.on_screen_frames();
    assert_frames_in_order(&frames, Duration::ZERO);
    assert_eq!(run.presented_count, FRAME_COUNT, "every frame shown once");
    assert_video_paced_by_wall_clock(&frames, audio_start);
    for (pts, tick) in &frames {
        assert_near(tick.position, *pts, FRAME, "frame shown on the clock");
    }
    assert!(
        run.pcm_duration(run.first_audible_frame().expect("tone")) <= AUDIO_TIME_TOLERANCE,
        "no leading silence: the audio waits compressed, not as silence in the output"
    );
    assert_near(
        run.pcm_duration(run.written_frames()),
        CLIP - audio_start,
        AUDIO_LENGTH_TOLERANCE,
        "the whole tone after its start, nothing more",
    );
    assert!(
        run.mid_stream_silence <= AUDIO_SILENCE_TOLERANCE,
        "audio output ran dry after the audio started: {:?}",
        run.mid_stream_silence
    );
}

/// Только звук, с ~2,03 с: без тишины, позиция с времени первого sample-а.
#[test]
fn audio_only_two_seconds_late_starts_at_its_first_sample() {
    let first_audio_fragment = 5;
    let audio_start = audio_fragment_start(first_audio_fragment);
    let run = play(stream(
        first_audio_fragment..FRAGMENT_COUNT,
        TimestampOffset::ZERO,
        None,
    ));

    assert_played_to_the_end(&run);
    assert!(
        run.pcm_duration(run.first_audible_frame().expect("tone")) <= AUDIO_TIME_TOLERANCE,
        "audio-only playback starts with the tone, not with silence"
    );
    assert_near(
        run.pcm_duration(run.written_frames()),
        CLIP - audio_start,
        AUDIO_LENGTH_TOLERANCE,
        "only the tone is written",
    );
    let playing_positions: Vec<Duration> = run
        .ticks
        .iter()
        .filter(|tick| matches!(tick.state, PlaybackState::Playing | PlaybackState::Draining))
        .map(|tick| tick.position)
        .collect();
    assert_near(
        playing_positions.iter().min().copied().expect("played"),
        audio_start,
        AUDIO_TIME_TOLERANCE,
        "the position starts at the time of the sound being heard",
    );
    assert_near(
        playing_positions.iter().max().copied().expect("played"),
        CLIP,
        AUDIO_LENGTH_TOLERANCE,
        "the position reaches the end of the audio",
    );
    assert!(
        run.mid_stream_silence <= AUDIO_SILENCE_TOLERANCE,
        "audio output ran dry: {:?}",
        run.mid_stream_silence
    );
}

/// Битые метки: звук «через три часа» (за концом media). Ждать его — не услышать вовсе,
/// поэтому прежнее поведение: звук сразу в базу, плеер не виснет и доигрывает видео.
#[test]
fn broken_hours_gap_does_not_hang_and_keeps_the_old_behavior() {
    let three_hours_micros = 3 * 60 * 60 * 1_000_000;
    let run = play(stream(
        0..FRAGMENT_COUNT,
        TimestampOffset::from_micros(three_hours_micros),
        Some(0..FRAGMENT_COUNT),
    ));

    assert_played_to_the_end(&run);
    let frames = run.on_screen_frames();
    assert_frames_in_order(&frames, Duration::ZERO);
    assert_near(
        run.media_time_of_first_audible(),
        Duration::ZERO,
        AUDIO_TIME_TOLERANCE,
        "the audio is written at the clock base, not held for hours",
    );
}

/// Сколько настоящего времени допустимо до первого кадра на старте: шаги открытия и
/// декода, но не 1,4 с ожидания начала записи.
const FIRST_FRAME_WALL_LIMIT: Duration = Duration::from_millis(400);

/// MPEG-TS от ffmpeg: видео и звук с ~1,4 с, шкала с нуля. Раньше звук звучал в нуле,
/// а первый кадр появлялся через 1,4 с (звук опережал картинку на 1,4 с); удержание
/// звука без переноса старта дало бы 1,4 с чёрного экрана. Показ стартует с содержимого.
#[test]
fn mpeg_ts_starting_after_the_timeline_start_plays_from_its_content() {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::MpegTs)
        .expect("ts lane");
    writer
        .append(VIDEO, &own_fixture("muxed-h264-aac.ts"))
        .expect("transport stream bytes");
    writer.end_of_stream().expect("end of stream");

    let run = play(open(&mut opener));

    assert_played_to_the_end(&run);
    let frames = run.on_screen_frames();
    let (first_frame, first_frame_tick) = *frames.first().expect("video frames");
    assert!(
        first_frame > Duration::from_secs(1),
        "the fixture starts late: first frame {first_frame:?}"
    );
    let first_tick = run.ticks.first().expect("ticks");
    let wall_until_first_frame = first_frame_tick.wall.duration_since(first_tick.wall);
    assert!(
        wall_until_first_frame <= FIRST_FRAME_WALL_LIMIT,
        "the first frame must not wait for the timeline to reach the content: \
         {wall_until_first_frame:?}"
    );
    for (pts, tick) in &frames {
        assert_near(tick.position, *pts, FRAME, "frame shown on the clock");
    }
    assert_near(
        run.media_time_of_first_audible(),
        first_frame,
        AUDIO_TIME_TOLERANCE,
        "audio and video of the recording start together",
    );
}

/// Перемотка в место, где звук цели начинается на ~1,6 с позже: страница после
/// перемотки шлёт видео с ключевого кадра цели (1,6 с), а звук — со своего сегмента
/// ~3,24 с. Видео идёт с цели, звук входит в свой PTS. Раньше звук цели писался в
/// очищенный выход прямо на commit-е и звучал в цели — на весь разрыв раньше видео.
#[test]
fn seek_into_a_late_audio_region_keeps_video_and_joins_audio_at_its_pts() {
    let audio_return_fragment = 8;
    let audio_return = audio_fragment_start(audio_return_fragment);
    let target_fragment = 4;
    let target = FRAGMENT * target_fragment as u32;
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

    let run = play_with_seek(
        open(&mut opener),
        Some(SeekPlan {
            at: FRAGMENT,
            target,
        }),
        |ticks_after_seek| {
            // Страница перемоталась и шлёт с цели: видео с ключевого кадра цели, звук —
            // со своего сегмента после неё.
            if ticks_after_seek == PAGE_RESPONSE_TICKS {
                writer
                    .append(VIDEO, &video.fragments[target_fragment..].concat())
                    .expect("video after the seek");
                writer
                    .append(AUDIO, &audio.fragments[audio_return_fragment..].concat())
                    .expect("audio after the seek");
                writer.end_of_stream().expect("end of stream");
            }
        },
    );

    assert_played_to_the_end(&run);
    let seek_tick = run.seek_tick.expect("the seek was issued");
    assert_near(
        run.media_time_of_first_audible_after(Some(seek_tick)),
        audio_return,
        AUDIO_TIME_TOLERANCE,
        "the first tone sample after the seek sounds at its own time",
    );
    let frames_after: Vec<(Duration, &TickRecord)> = run
        .on_screen_frames()
        .into_iter()
        .filter(|(_, tick)| tick.wall > run.ticks[seek_tick].wall)
        .collect();
    assert_frames_in_order(&frames_after, target);
    assert_video_paced_by_wall_clock(&frames_after, audio_return);
    for (pts, tick) in &frames_after {
        assert_near(tick.position, *pts, FRAME, "frame shown on the clock");
    }
}

/// Видео кончается раньше, чем начинается звук (видео 0–2 с, звук с ~3,24 с, длительность
/// 4 с): часы без звука доходят до звука, звук входит в свой PTS и доигрывает до конца
/// без обрезки, `Ended` — только после него.
#[test]
fn audio_starting_after_the_video_end_still_plays_whole() {
    let first_audio_fragment = 8;
    let audio_start = audio_fragment_start(first_audio_fragment);
    let last_video_fragment = 5;
    let run = play(stream(
        first_audio_fragment..FRAGMENT_COUNT,
        TimestampOffset::ZERO,
        Some(0..last_video_fragment),
    ));

    assert_played_to_the_end(&run);
    assert_near(
        run.media_time_of_first_audible(),
        audio_start,
        AUDIO_TIME_TOLERANCE,
        "the tone sounds at its own time after the last frame",
    );
    assert_near(
        run.pcm_duration(run.written_frames()),
        CLIP - audio_start,
        AUDIO_LENGTH_TOLERANCE,
        "the whole tone is played, nothing cut at the join",
    );
    assert_frames_in_order_until(
        &run.on_screen_frames(),
        Duration::ZERO,
        FRAME * (last_video_fragment as u32 * 10 - 1),
    );
}
