//! Функциональные тесты окна decode-ahead с decoder-ом, у которого есть задержка выхода.
//!
//! Frame-threaded FFmpeg decoder (N потоков) выдаёт кадр k только после того, как получил
//! ещё ~N следующих packets: «забрал packet» и «выдал кадр» разнесены на N packets.
//! Окно decode-ahead обязано ограничивать именно выход decoder-а (готовые кадры впереди
//! звука), а не PTS последнего отправленного packet-а — иначе при N × длительность кадра
//! больше окна decoder голодает и кадры приходят поздно (потери «..2..2..2» на 30 fps).
//!
//! Тесты гоняют настоящий `PlayerSession::tick` (startup preroll → Playing, admission,
//! drain, scheduler показа) с fake-decoder-ом и audio clock-ом, который идёт в media time.
//! Проверяем наблюдаемый итог: какие кадры реально показаны, какие отброшены и насколько
//! готовые кадры опережают звук.

use std::collections::BTreeSet;

use super::*;
use crate::{PlayerEvent, PlayerVideoDropReason};

/// Шаг tick-а: чаще кадра, чтобы проверка зависела от admission, а не от редкого tick-а.
const TICK_STEP: Duration = Duration::from_millis(4);

/// Сколько media-времени длится воспроизведение в тесте.
const PLAYBACK_DURATION: Duration = Duration::from_secs(4);

/// Сколько packets «демуксер» держит в pending очереди (как `decoder_packet_channel_frames`).
const PENDING_PACKETS_TARGET: usize = 32;

/// Число потоков FFmpeg frame threading на 16-поточном CPU (`cores - 2`, `decode_threading`).
const FFMPEG_FRAME_THREADS: usize = 14;

/// Задержка переупорядочивания H.264 High с B-кадрами (`has_b_frames`), в кадрах.
const H264_HIGH_REORDER_DELAY_FRAMES: usize = 2;

/// Полная задержка выхода frame-threaded decoder-а в packets: потоки + reorder.
const FRAME_THREADED_OUTPUT_DELAY_PACKETS: usize =
    FFMPEG_FRAME_THREADS + H264_HIGH_REORDER_DELAY_FRAMES;

/// Шаг PTS 30 fps, мкс.
const FRAME_DURATION_30_FPS: Duration = Duration::from_micros(33_333);

/// Шаг PTS 24 fps, мкс.
const FRAME_DURATION_24_FPS: Duration = Duration::from_micros(41_667);

/// Audio clock, который тест двигает вручную (имитирует идущий звук в media time).
#[derive(Clone, Default)]
struct ManualAudioClock {
    /// Текущая слышимая позиция звука.
    position: Arc<Mutex<Duration>>,
}

impl ManualAudioClock {
    /// Ставит слышимую позицию звука.
    fn set(&self, position: Duration) {
        *self.position.lock().expect("manual audio clock lock") = position;
    }

    /// Текущая слышимая позиция звука.
    fn position(&self) -> Duration {
        *self.position.lock().expect("manual audio clock lock")
    }
}

impl PlayerAudioClock for ManualAudioClock {
    fn now(&self) -> Duration {
        self.position()
    }

    fn output_timing(&self) -> AudioOutputClockTiming {
        let position = self.position();
        AudioOutputClockTiming::new(position, position)
    }

    fn reset(&self) {}

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

/// Внутреннее состояние fake decoder-а с задержкой выхода.
#[derive(Default)]
struct DelayedOutputDecoderState {
    /// Packets, которые decoder принял, но кадр по ним ещё не выдал (PTS, generation).
    awaiting_output: VecDeque<(Duration, u64)>,

    /// Готовые кадры, которые worker ещё не забрал.
    ready_frames: VecDeque<video_core::DecodedFrame>,

    /// Сколько packets принято с прошлого drain-а completion counter-а.
    completed_packets: usize,

    /// Следующий handle кадра.
    next_handle: u64,

    /// Самый поздний PTS, который decoder уже выдал (включая ещё не забранные кадры).
    latest_emitted_pts: Option<Duration>,
}

/// Fake decoder: кадр по packet-у k появляется только после `output_delay_packets`
/// следующих packets (модель FFmpeg frame threading: `ff_thread_receive_frame` отдаёт кадр,
/// только когда все потоки получили работу). Packet считается «завершённым» сразу при
/// приёме — как `avcodec_send_packet` в `video-ffmpeg`, поэтому player-side in-flight ≈ 0.
#[derive(Clone)]
struct DelayedOutputVideoDecoderThread {
    /// Сколько следующих packets нужно decoder-у, чтобы выдать кадр.
    output_delay_packets: usize,

    /// Общее состояние между тестом и session.
    state: Arc<Mutex<DelayedOutputDecoderState>>,
}

impl DelayedOutputVideoDecoderThread {
    /// Создаёт decoder с заданной задержкой выхода в packets.
    fn new(output_delay_packets: usize) -> Self {
        Self {
            output_delay_packets,
            state: Arc::new(Mutex::new(DelayedOutputDecoderState::default())),
        }
    }

    /// Самый поздний PTS, который decoder уже выдал.
    fn latest_emitted_pts(&self) -> Option<Duration> {
        self.state
            .lock()
            .expect("delayed decoder state lock")
            .latest_emitted_pts
    }
}

impl video_core::VideoDecoderThreadHandle for DelayedOutputVideoDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn configure_stream(
        &self,
        _config: video_core::VideoStreamDecodeConfig,
    ) -> video_core::VideoStreamConfigResult {
        video_core::VideoStreamConfigResult::Configured
    }

    fn backend_name(&self) -> &'static str {
        "Delayed-output fake decoder"
    }

    fn send_packet(&self, packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
        let mut state = self.state.lock().expect("delayed decoder state lock");
        state
            .awaiting_output
            .push_back((packet.pts, packet.generation));
        state.completed_packets = state.completed_packets.saturating_add(1);
        while state.awaiting_output.len() > self.output_delay_packets {
            let Some((pts, generation)) = state.awaiting_output.pop_front() else {
                break;
            };
            state.next_handle = state.next_handle.saturating_add(1);
            let mut frame = decoded_frame(pts, state.next_handle);
            frame.generation = generation;
            state.ready_frames.push_back(frame);
            state.latest_emitted_pts = Some(state.latest_emitted_pts.map_or(pts, |p| p.max(pts)));
        }
        Ok(())
    }

    fn release_frame(&self, _handle: video_core::FrameResourceHandle) {}

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        self.state
            .lock()
            .expect("delayed decoder state lock")
            .ready_frames
            .pop_front()
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        panic!("delayed-output fake decoder does not provide renderer resources")
    }

    fn decoder_resource_snapshot(&self) -> Option<crate::DecoderResourceSnapshot> {
        None
    }

    fn host_upload_resource_snapshot(&self) -> video_core::HostUploadResourceSnapshotStatus {
        video_core::HostUploadResourceSnapshotStatus::UnsupportedBackend
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<crate::DecoderControlChannelPressureSnapshot> {
        None
    }

    fn decoder_activity_snapshot(&self) -> video_core::VideoDecoderActivitySnapshot {
        video_core::VideoDecoderActivitySnapshot::unsupported()
    }

    fn set_preroll_output_floor(
        &self,
        _floor: video_core::VideoPrerollOutputFloor,
    ) -> video_core::VideoPrerollOutputFloorResult {
        video_core::VideoPrerollOutputFloorResult::Applied
    }

    fn clear_preroll_output_floor(
        &self,
        _clear: video_core::VideoPrerollOutputFloorClear,
    ) -> video_core::VideoPrerollOutputFloorResult {
        video_core::VideoPrerollOutputFloorResult::Cleared
    }

    fn packet_queue_depth(&self) -> usize {
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        std::mem::take(
            &mut self
                .state
                .lock()
                .expect("delayed decoder state lock")
                .completed_packets,
        )
    }
}

/// Что наблюдал тест за прогон воспроизведения.
#[derive(Debug, Default)]
struct PlaybackObservation {
    /// PTS кадров, которые побывали на экране (present slot).
    presented_pts: BTreeSet<Duration>,

    /// Кадры, отброшенные из-за опоздания или голодания decoder-а.
    late_or_starved_drops: Vec<(Duration, PlayerVideoDropReason)>,

    /// Сколько раз детектор сообщил «decoder отстаёт от воспроизведения».
    falling_behind_events: usize,

    /// Максимальное опережение готового выхода decoder-а над звуком.
    max_decoded_lead: Duration,

    /// Максимальное опережение после того, как стартовый burst preroll-а показан.
    max_steady_decoded_lead: Duration,

    /// Насколько позже своего PTS кадр впервые попал на экран (худший случай).
    max_presentation_lateness: Duration,

    /// Перешла ли session из startup preroll в Playing.
    playing_started: bool,
}

/// Media time, к которому кадры стартового preroll-а (окно снято до первого кадра)
/// гарантированно показаны: дальше опережение задаёт только окно decode-ahead.
fn startup_burst_settled_at() -> Duration {
    PlayerTickConfig::default().max_video_decode_ahead
}

/// Готовит session в startup preroll с video + audio треками и fake decoder-ом.
fn startup_session(
    decoder_thread: DelayedOutputVideoDecoderThread,
    audio_clock: ManualAudioClock,
) -> PlayerSession {
    // Без demuxer-а: packets доливает сам тест, а ранний EOF не уводит session в Draining.
    let mut session = PlayerSession::new();
    session.pipeline.set_video_decoder_thread(decoder_thread);
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::H264),
    );
    session.pipeline.select_audio_track(TrackId::new(2));
    // Буфер звука полон: тест проверяет видео, а не голодание аудио.
    install_fixed_audio_output(&mut session, 500.0);
    session
        .pipeline
        .install_audio_clock(Arc::new(audio_clock) as Arc<dyn PlayerAudioClock>);
    // Startup как у настоящего открытия: Buffering + отложенный autoplay до первого кадра.
    session.set_playback_state(PlaybackState::Buffering);
    session.mark_startup_autoplay_preroll_pending();
    session
}

/// Завершает startup preroll, как только первый кадр готов (звук в тесте готов всегда).
///
/// Настоящий `finish_autoplay_preroll_if_ready` ждёт ещё и audio decoder, которого в
/// этом тесте нет; video gate тот же — `autoplay_video_preroll_ready`.
fn finish_startup_preroll_when_first_frame_is_ready(session: &mut PlayerSession) {
    if session.playback_state() == PlaybackState::Buffering
        && session.autoplay_video_preroll_ready()
    {
        session
            .begin_playback_without_media_for_test()
            .expect("startup playback must begin");
    }
}

/// Доливает pending video packets до `PENDING_PACKETS_TARGET`, как это делал бы demux.
fn top_up_pending_video_packets(
    session: &mut PlayerSession,
    next_packet_index: &mut u64,
    frame_duration: Duration,
) {
    while session.pipeline.pending_video_packet_len() < PENDING_PACKETS_TARGET {
        let keyframe = if *next_packet_index == 0 {
            PacketKeyframe::Keyframe
        } else {
            PacketKeyframe::NotKeyframe
        };
        let pts = frame_duration * u32::try_from(*next_packet_index).expect("packet index");
        enqueue_selected_video_packet(session, pts, Bytes::from_static(b"frame"), keyframe);
        *next_packet_index += 1;
    }
}

/// Прогоняет startup + `PLAYBACK_DURATION` воспроизведения и собирает наблюдения.
fn run_playback(output_delay_packets: usize, frame_duration: Duration) -> PlaybackObservation {
    let decoder_thread = DelayedOutputVideoDecoderThread::new(output_delay_packets);
    let audio_clock = ManualAudioClock::default();
    let mut session = startup_session(decoder_thread.clone(), audio_clock.clone());
    let mut observation = PlaybackObservation::default();
    let mut next_packet_index = 0u64;
    let start = Instant::now();
    let mut media_time = Duration::ZERO;
    // Startup не должен длиться вечно: ограничиваем общее число tick-ов.
    let max_ticks = (PLAYBACK_DURATION * 2).as_millis() / TICK_STEP.as_millis();

    for tick_index in 0..max_ticks {
        if media_time >= PLAYBACK_DURATION {
            break;
        }
        // Звук идёт только в Playing: в preroll он ждёт первый кадр, как в production.
        if session.playback_state() == PlaybackState::Playing {
            observation.playing_started = true;
            media_time += TICK_STEP;
        }
        audio_clock.set(media_time);
        top_up_pending_video_packets(&mut session, &mut next_packet_index, frame_duration);

        let now = start + TICK_STEP * u32::try_from(tick_index).expect("tick index");
        let tick_result = session.tick(PlayerTickContext::new(now));
        finish_startup_preroll_when_first_frame_is_ready(&mut session);

        for drop in &tick_result.dropped_video_frames {
            if matches!(
                drop.reason,
                PlayerVideoDropReason::Late | PlayerVideoDropReason::DecoderStarvation
            ) {
                observation
                    .late_or_starved_drops
                    .push((drop.pts, drop.reason));
            }
        }
        // Опоздание считаем в момент, когда кадр впервые оказался на экране.
        if let Some(frame) = session.pipeline.present_video_frame()
            && observation.presented_pts.insert(frame.pts)
        {
            let lateness = audio_clock.position().saturating_sub(frame.pts);
            observation.max_presentation_lateness =
                observation.max_presentation_lateness.max(lateness);
        }
        if let Some(latest_emitted) = decoder_thread.latest_emitted_pts() {
            let lead = latest_emitted.saturating_sub(audio_clock.position());
            observation.max_decoded_lead = observation.max_decoded_lead.max(lead);
            // Кадры, декодированные стартовым preroll-ом без окна, к этому моменту уже показаны.
            if media_time >= startup_burst_settled_at() {
                observation.max_steady_decoded_lead = observation.max_steady_decoded_lead.max(lead);
            }
        }
        observation.falling_behind_events += session
            .take_events()
            .iter()
            .filter(|event| matches!(event, PlayerEvent::VideoDecoderFallingBehind(_)))
            .count();
    }

    observation
}

/// Все кадры с PTS в `[0, until)`.
fn expected_frames(frame_duration: Duration, until: Duration) -> BTreeSet<Duration> {
    (0u32..)
        .map(|index| frame_duration * index)
        .take_while(|pts| *pts < until)
        .collect()
}

/// Проверяет, что за прогон каждый кадр показан вовремя и ни один не потерян.
///
/// «Вовремя» — кадр впервые попал на экран не позже своего PTS (с точностью до одного
/// tick-а). Опоздавший кадр — это рассинхрон с звуком; в настоящем decoder-е, где кадры
/// приходят пачками, опоздание превращается в потерю кадров (замер 780M: 463/600).
fn assert_every_frame_presented_on_time(
    observation: &PlaybackObservation,
    frame_duration: Duration,
) {
    assert!(
        observation.playing_started,
        "startup preroll never finished"
    );
    // Хвост прогона (последние ~окно) ещё может ждать показа: проверяем до него.
    let checked_until = PLAYBACK_DURATION - Duration::from_millis(300);
    let expected = expected_frames(frame_duration, checked_until);
    let missing: Vec<_> = expected
        .difference(&observation.presented_pts)
        .map(Duration::as_millis)
        .collect();

    assert!(
        missing.is_empty(),
        "{} of {} frames never reached the screen (ms): {missing:?}",
        missing.len(),
        expected.len()
    );
    assert!(
        observation.max_presentation_lateness <= TICK_STEP,
        "a frame reached the screen {:?} after its PTS (decoder starved by the window)",
        observation.max_presentation_lateness
    );
    assert!(
        observation.late_or_starved_drops.is_empty(),
        "frames dropped late/starved: {:?}",
        observation.late_or_starved_drops
    );
    assert_eq!(observation.falling_behind_events, 0);
}

/// Готовые кадры не опережают звук дальше окна decode-ahead (граница памяти).
///
/// Весь прогон — не дальше жёсткого максимума (`max_video_decode_ahead`: стартовый burst
/// preroll-а и bounded catch-up). Установившееся воспроизведение — не дальше
/// steady-state окна (`target_video_decode_ahead`), как и до исправления.
fn assert_decoded_output_inside_window(observation: &PlaybackObservation) {
    let config = PlayerTickConfig::default();
    assert!(
        observation.max_decoded_lead <= config.max_video_decode_ahead,
        "decoded output ran {:?} ahead of audio, max window {:?}",
        observation.max_decoded_lead,
        config.max_video_decode_ahead
    );
    assert!(
        observation.max_steady_decoded_lead <= config.target_video_decode_ahead,
        "steady decoded output ran {:?} ahead of audio, window {:?}",
        observation.max_steady_decoded_lead,
        config.target_video_decode_ahead
    );
}

/// 30 fps, 14 потоков + reorder 2: выход отстаёт на 16 × 33 мс ≈ 533 мс — больше окна
/// 250 мс и даже catch-up максимума 500 мс (замер 780M: 463/600 уникальных кадров).
#[test]
fn frame_threaded_decoder_at_30_fps_presents_every_frame_on_time() {
    let observation = run_playback(FRAME_THREADED_OUTPUT_DELAY_PACKETS, FRAME_DURATION_30_FPS);

    assert_every_frame_presented_on_time(&observation, FRAME_DURATION_30_FPS);
    assert_decoded_output_inside_window(&observation);
}

/// 24 fps (фильмы): та же задержка в packets ≈ 667 мс — ещё дальше за окном.
#[test]
fn frame_threaded_decoder_at_24_fps_presents_every_frame_on_time() {
    let observation = run_playback(FRAME_THREADED_OUTPUT_DELAY_PACKETS, FRAME_DURATION_24_FPS);

    assert_every_frame_presented_on_time(&observation, FRAME_DURATION_24_FPS);
    assert_decoded_output_inside_window(&observation);
}

/// Окно по-прежнему держит память: decoder без задержки выхода (HW-подобный) не
/// опережает звук дальше окна, как и до исправления.
///
/// 24 fps важен отдельно: окно 250 мс — это 6 кадров, и presentation queue (8) оставляет
/// место для нескольких packets за один send-проход — прогноз обязан сдвигаться на кадр
/// за каждый отправленный packet, иначе проход пропустит лишний кадр сверх окна.
#[test]
fn immediate_decoder_output_stays_inside_decode_ahead_window() {
    for frame_duration in [FRAME_DURATION_30_FPS, FRAME_DURATION_24_FPS] {
        let observation = run_playback(0, frame_duration);

        assert_every_frame_presented_on_time(&observation, frame_duration);
        assert_decoded_output_inside_window(&observation);
    }
}
