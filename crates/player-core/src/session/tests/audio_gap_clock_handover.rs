//! Звук отсутствует на отрезке файла, видео продолжается (T410, сессия 13b).
//!
//! Живой случай: H.264, видео 20.08 с, звук обрезан до 5 с — картинка замирала на ~5 с
//! навсегда: audio-priority demux дочитал лимит video packets в ожидании звука,
//! decode-ahead держал видео по вставшим audio-часам, до EOF дело не доходило.
//!
//! Сценарии идут через настоящий tick: demux → audio decoder → output → часы → video
//! decoder → показ. Output моделирует звуковую карту: принятый PCM звучит в реальном
//! темпе, часы стоят, когда звука нет.

use audio_core::{
    AudioOutputClockTiming, AudioOutputInputFrameCount, AudioOutputStreamFrameCount,
    AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport,
};

use super::test_support::*;
use super::*;
use crate::PlaybackRate;
use crate::pipeline::PresentationClockSource;

/// Частота PCM тестового звука.
const SAMPLE_RATE: u32 = 48_000;

/// Обычная длительность одного audio packet-а (и его декодированного PCM).
const AUDIO_PACKET: Duration = Duration::from_millis(100);

/// Шаг wall-time между worker tick-ами; звуковая карта играет столько же.
const TICK_STEP: Duration = Duration::from_millis(25);

/// Предел симуляции: 20 с файла при шаге 25 мс плюс запас; дальше — зависание.
const MAX_TICKS: u32 = 2_000;

/// Переводит длительность в число PCM frames тестовой частоты.
fn frames_for(duration: Duration) -> u64 {
    let frames = duration.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000;
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// Переводит число PCM frames в длительность.
fn duration_for(frames: u64) -> Duration {
    Duration::from_nanos(frames.saturating_mul(1_000_000_000) / u64::from(SAMPLE_RATE))
}

/// Общее состояние модели звуковой карты.
#[derive(Debug, Default)]
struct PacedDeviceState {
    /// Сколько frames принято в output (ring buffer + уже сыгранное).
    submitted_frames: u64,

    /// Сколько frames уже прозвучало.
    audible_frames: u64,

    /// Запущен ли stream (`play`).
    playing: bool,
}

/// Модель звуковой карты: принятый PCM звучит в реальном темпе, без звука часы стоят.
#[derive(Clone, Default)]
struct PacedAudioDevice {
    /// Состояние, общее для output-а, clock-а и теста.
    state: Arc<Mutex<PacedDeviceState>>,
}

impl PacedAudioDevice {
    /// Блокирует состояние модели.
    fn lock(&self) -> std::sync::MutexGuard<'_, PacedDeviceState> {
        self.state
            .lock()
            .expect("paced device mutex must not be poisoned")
    }

    /// Звуковая карта играет `elapsed` wall-time (только запущенный stream и только PCM).
    fn advance(&self, elapsed: Duration) {
        let mut state = self.lock();
        if state.playing {
            state.audible_frames = state
                .audible_frames
                .saturating_add(frames_for(elapsed))
                .min(state.submitted_frames);
        }
    }

    /// Сколько frames output принял за всё время.
    fn submitted_frames(&self) -> u64 {
        self.lock().submitted_frames
    }

    /// Подкладывает уже декодированный звук (имитация нового отрезка после seek).
    fn submit_for_test(&self, duration: Duration) {
        let mut state = self.lock();
        state.submitted_frames = state.submitted_frames.saturating_add(frames_for(duration));
    }

    /// Текущий timing в контракте `PlayerAudioClock`.
    fn timing(&self) -> AudioOutputClockTiming {
        let state = self.lock();
        AudioOutputClockTiming::new(
            duration_for(state.audible_frames),
            duration_for(state.submitted_frames),
        )
    }
}

impl PlayerAudioClock for PacedAudioDevice {
    /// Слышимая позиция output-а.
    fn now(&self) -> Duration {
        self.timing().audible_output_position()
    }

    /// Слышимая позиция и конец принятого PCM.
    fn output_timing(&self) -> AudioOutputClockTiming {
        self.timing()
    }

    /// Reset начинает output-ось с нуля.
    fn reset(&self) {
        let mut state = self.lock();
        state.audible_frames = 0;
        state.submitted_frames = 0;
    }

    /// Модель не считает callback underrun-ы.
    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

/// Output поверх модели звуковой карты.
struct PacedAudioOutput {
    /// Общая модель устройства.
    device: PacedAudioDevice,
}

impl PlayerAudioOutput for PacedAudioOutput {
    /// Принимает весь stereo PCM в ring buffer.
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let frames = samples.len() / 2;
        self.device.submit_for_test(duration_for(frames as u64));
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
    }

    /// Запускает stream.
    fn play(&mut self) -> anyhow::Result<()> {
        self.device.lock().playing = true;
        Ok(())
    }

    /// Останавливает stream и возвращает замороженный timing.
    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        self.device.lock().playing = false;
        Ok(self.device.timing())
    }

    /// Seek выбрасывает ещё не сыгранный PCM.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        let mut state = self.device.lock();
        state.submitted_frames = state.audible_frames;
        Ok(generation)
    }

    /// Громкость модели не важна.
    fn set_volume(&mut self, _volume: f32) {}

    /// Уровень ring buffer-а в миллисекундах.
    fn buffer_level_ms(&self) -> f64 {
        let timing = self.device.timing();
        timing.submitted_output_tail().as_secs_f64() * 1_000.0
    }

    /// Clock той же модели.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::new(self.device.clone())
    }
}

/// Decoder, который превращает каждый audio packet в PCM заданной длительности.
struct FixedLengthPcmDecoder {
    /// Длительность PCM одного packet-а.
    pcm_duration: Duration,
}

impl audio_core::AudioDecoder for FixedLengthPcmDecoder {
    /// Возвращает PCM ровно на длительность packet-а.
    fn decode(&mut self, _packet: &audio_core::EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        let frames = usize::try_from(frames_for(self.pcm_duration)).unwrap_or(0);
        Ok(vec![0.0; frames * 2])
    }

    /// Stateless decoder.
    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Частота тестового PCM.
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    /// Stereo.
    fn channels(&self) -> u32 {
        2
    }

    /// Явный stereo layout.
    fn channel_layout(&self) -> Option<audio_core::AudioChannelLayout> {
        Some(audio_core::AudioChannelLayout::stereo())
    }
}

/// Раскладка тестового файла.
struct MediaLayout {
    /// Интервал кадров видео.
    video_frame_interval: Duration,

    /// PTS первого кадра видео. Не ноль — demux уже переставлен seek-ом: файл «читается»
    /// с этой позиции (fake demuxer не умеет репозиционироваться сам).
    video_start: Duration,

    /// Конец видео (PTS последнего кадра < этого значения).
    video_end: Duration,

    /// Отрезки, где в контейнере есть звук.
    audio_ranges: Vec<(Duration, Duration)>,

    /// Длительность одного audio packet-а внутри отрезков.
    audio_packet_duration: Duration,

    /// Сколько video packets audio-priority demux может дочитать ради звука.
    audio_catchup_video_packet_limit: usize,

    /// На сколько audio packets лежат в файле позже видео того же времени (interleave muxer-а).
    audio_file_order_lag: Duration,

    /// Сколько packets demux читает за один tick.
    demux_packets_per_tick: usize,

    /// Сколько первых video packets decoder принимает. `None` — принимает всё; `Some(n)` —
    /// после n-го packet-а его очередь навсегда полна (HW не тянет 4x): очередь pending
    /// video packets растёт до лимита audio-priority demux-а.
    video_decoder_frame_limit: Option<usize>,

    /// Скорость воспроизведения сценария.
    playback_rate: PlaybackRate,
}

impl MediaLayout {
    /// PTS всех кадров видео.
    fn video_pts(&self) -> Vec<Duration> {
        let mut video_pts = Vec::new();
        let mut pts = self.video_start;
        while pts < self.video_end {
            video_pts.push(pts);
            pts += self.video_frame_interval;
        }
        video_pts
    }

    /// PTS всех audio packets.
    fn audio_pts(&self) -> Vec<Duration> {
        let mut audio_pts = Vec::new();
        for (range_start, range_end) in &self.audio_ranges {
            let mut pts = *range_start;
            while pts < *range_end {
                audio_pts.push(pts);
                pts += self.audio_packet_duration;
            }
        }
        audio_pts
    }
}

/// Наблюдения одного прогона.
#[derive(Debug, Default)]
struct PlaybackObservations {
    /// PTS кадров в порядке показа и wall-момент показа.
    presented: Vec<(Duration, Instant)>,

    /// Позиция показа после каждого tick-а.
    positions: Vec<Duration>,

    /// Был ли хоть один tick с часами у monotonic clock во время звука/разрыва.
    handed_over: bool,

    /// Позиция, с которой звук вернул себе часы, и сколько PCM принято после этого.
    audio_resume: Option<(Duration, u64)>,

    /// Позиция показа, когда часы снова оказались у звука после разрыва.
    audio_drives_after_resume_at: Option<Duration>,
}

/// Session, проигрывающая [`MediaLayout`] через настоящий tick.
struct GapScenario {
    /// Session под тестом.
    session: PlayerSession,

    /// Модель звуковой карты.
    device: PacedAudioDevice,

    /// Конфиг tick-а сценария.
    tick_config: PlayerTickConfig,

    /// Кадры видео файла.
    video_pts: Vec<Duration>,
}

impl GapScenario {
    /// Открывает файл, ставит звук/видео и запускает Play.
    fn start(layout: &MediaLayout) -> Self {
        let video_track = fake_track(1, TrackKind::Video);
        let audio_track = fake_track(2, TrackKind::Audio);
        let video_pts = layout.video_pts();
        let mut packets: Vec<(Duration, u8, media_core::Packet)> = Vec::new();
        for (index, pts) in video_pts.iter().enumerate() {
            let keyframe = if index == 0 {
                PacketKeyframe::Keyframe
            } else {
                PacketKeyframe::NotKeyframe
            };
            let packet = fake_video_packet_with_keyframe(video_track.id, *pts, keyframe);
            packets.push((*pts, 1, packet));
        }
        for pts in layout.audio_pts() {
            let packet = fake_audio_packet(audio_track.id, pts, layout.audio_packet_duration);
            packets.push((pts + layout.audio_file_order_lag, 0, packet));
        }
        // Interleaving как у muxer-а: по времени (со сдвигом звука), звук раньше видео
        // на равных ключах.
        packets.sort_by_key(|(pts, kind_order, _)| (*pts, *kind_order));
        let demuxer = scripted_seek_demuxer(
            vec![video_track.clone(), audio_track.clone()],
            layout.video_start,
            layout.video_start,
            packets.into_iter().map(|(_, _, packet)| packet).collect(),
        );
        let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
        harness
            .session
            .set_snapshot_duration(Some(layout.video_end));
        for (handle, pts) in (10_000_u64..).zip(&video_pts) {
            harness.decoder.decode_next_packet_as_frame(*pts, handle);
        }

        if let Some(accepted_packets) = layout.video_decoder_frame_limit {
            // Decoder принимает первые packets, затем его очередь навсегда полна.
            for _ in 0..accepted_packets {
                harness.decoder.push_send_result(Ok(()));
            }
            for _ in 0..MAX_TICKS * 64 {
                harness
                    .decoder
                    .push_send_result(Err(DecodeSendError::Backpressure(
                        DecodeBackpressureReason::PacketQueueFull {
                            queued_packets: 4,
                            capacity: 4,
                        },
                    )));
            }
        }

        let device = PacedAudioDevice::default();
        harness
            .session
            .pipeline
            .install_audio_decoder(Box::new(FixedLengthPcmDecoder {
                pcm_duration: layout.audio_packet_duration,
            }));
        harness
            .session
            .pipeline
            .install_audio_output_for_tests(Box::new(PacedAudioOutput {
                device: device.clone(),
            }));
        harness
            .session
            .pipeline
            .install_audio_clock(Arc::new(device.clone()));
        harness
            .session
            .dispatch_command(PlayerCommand::Play)
            .expect("Play must reach the session");
        if layout.playback_rate != PlaybackRate::NORMAL {
            harness
                .session
                .dispatch_command(PlayerCommand::SetPlaybackRate(layout.playback_rate))
                .expect("SetPlaybackRate must reach the session");
        }
        let _start_events = harness.session.take_events();

        Self {
            session: harness.session,
            device,
            tick_config: PlayerTickConfig {
                max_pending_video_packets_during_audio_catchup: layout
                    .audio_catchup_video_packet_limit,
                max_demux_packets_per_tick: layout.demux_packets_per_tick,
                ..PlayerTickConfig::default()
            },
            video_pts,
        }
    }

    /// Текущий источник времени показа.
    fn clock_source(&self) -> PresentationClockSource {
        self.session.pipeline.presentation_clock_source()
    }

    /// Играет, пока `stop` не скажет хватит; падает, если предел tick-ов исчерпан.
    fn play_until(
        &mut self,
        started_at: Instant,
        observations: &mut PlaybackObservations,
        what: &str,
        mut stop: impl FnMut(&Self, &PlaybackObservations) -> bool,
    ) -> Instant {
        for tick_index in 1..=MAX_TICKS {
            let now = started_at + TICK_STEP * tick_index;
            self.device.advance(TICK_STEP);
            let source_before = self.clock_source();
            let submitted_before = self.device.submitted_frames();
            let _tick = self
                .session
                .tick(PlayerTickContext::with_config(now, self.tick_config));
            self.observe_tick(now, source_before, submitted_before, observations);
            if stop(self, observations) {
                return now;
            }
        }
        panic!(
            "never reached: {what}; state={:?} position={:?} presented={} source={:?} \
             pending_video={} pending_audio={}",
            self.session.playback_state(),
            self.session.snapshot().current_position,
            observations.presented.len(),
            self.clock_source(),
            self.session.pipeline.pending_video_packet_len(),
            self.session.pipeline.pending_audio_packet_len(),
        );
    }

    /// Записывает показ кадра, позицию и смену источника времени после tick-а.
    fn observe_tick(
        &self,
        now: Instant,
        source_before: PresentationClockSource,
        submitted_before: u64,
        observations: &mut PlaybackObservations,
    ) {
        if let Some(pts) = self
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts)
            && observations.presented.last().map(|(last, _)| *last) != Some(pts)
        {
            observations.presented.push((pts, now));
        }
        let position = self.session.snapshot().current_position;
        observations.positions.push(position);

        let source_after = self.clock_source();
        if source_after == PresentationClockSource::MonotonicAfterAudioEnd {
            observations.handed_over = true;
        }
        let audio_resumed = source_before == PresentationClockSource::MonotonicAfterAudioEnd
            && source_after == PresentationClockSource::AudioOutput;
        if audio_resumed {
            observations.audio_resume = Some((position, submitted_before));
        }
        if observations.audio_resume.is_some()
            && observations.audio_drives_after_resume_at.is_none()
            && source_after == PresentationClockSource::AudioOutput
            && position > observations.audio_resume.map_or(position, |(at, _)| at)
        {
            observations.audio_drives_after_resume_at = Some(position);
        }
    }

    /// Seek в `target`, где звука нет: дальше seek завершается обычными tick-ами.
    ///
    /// Fake demuxer читает пакеты только с `target` (см. `MediaLayout::video_start`),
    /// как реальный demux после seek-а; конец записанного звука сбрасывается seek-ом.
    fn start_seek_into_region_without_audio(&mut self, target: Duration) {
        self.session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_duration(target),
            )))
            .expect("Seek must reach the session");
    }

    /// Играет до `Ended`.
    fn play_until_ended(
        &mut self,
        started_at: Instant,
        observations: &mut PlaybackObservations,
    ) -> Instant {
        self.play_until(started_at, observations, "Ended", |scenario, _| {
            scenario.session.playback_state() == PlaybackState::Ended
        })
    }
}

/// Проверяет, что все кадры начиная с `from` показаны по порядку и без пропусков.
fn assert_frames_presented_in_order_from(
    observations: &PlaybackObservations,
    video_pts: &[Duration],
    from: Duration,
) {
    let presented_after: Vec<Duration> = observations
        .presented
        .iter()
        .map(|(pts, _)| *pts)
        .filter(|pts| *pts >= from)
        .collect();
    let expected_after: Vec<Duration> = video_pts
        .iter()
        .copied()
        .filter(|pts| *pts >= from)
        .collect();
    assert_eq!(
        presented_after, expected_after,
        "every video frame from {from:?} must be presented in order"
    );
}

/// Wall-time между показами двух кадров.
fn wall_time_between(
    observations: &PlaybackObservations,
    first: Duration,
    last: Duration,
) -> Duration {
    let shown_at = |wanted: Duration| {
        observations
            .presented
            .iter()
            .find(|(pts, _)| *pts == wanted)
            .map(|(_, at)| *at)
            .unwrap_or_else(|| panic!("frame {wanted:?} must be presented"))
    };
    shown_at(last).saturating_duration_since(shown_at(first))
}

/// Проверяет, что позиция показа ни разу не прыгнула назад.
fn assert_position_never_jumps_back(observations: &PlaybackObservations) {
    for pair in observations.positions.windows(2) {
        assert!(
            pair[1] >= pair[0],
            "presentation position must not jump backwards: {:?} -> {:?}",
            pair[0],
            pair[1]
        );
    }
}

/// Проверяет публикацию `Ended` (по нему playlist переходит к следующему файлу).
fn assert_ended_event_published(session: &mut PlayerSession) {
    let events = session.take_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            PlayerEvent::PlaybackStateChanged(PlaybackState::Ended)
        )),
        "Ended event is the terminal edge for playlist auto-advance"
    );
}

/// Файл из живого случая: звук только первые 5 с, видео 20 с.
fn audio_ends_at_five_seconds_layout() -> MediaLayout {
    MediaLayout {
        video_start: Duration::ZERO,
        video_frame_interval: Duration::from_millis(100),
        video_end: Duration::from_secs(20),
        audio_ranges: vec![(Duration::ZERO, Duration::from_secs(5))],
        audio_packet_duration: AUDIO_PACKET,
        // 4 с видео при 10 к/с: разрыв в 15 с гарантированно упирает demux в лимит.
        audio_catchup_video_packet_limit: 40,
        audio_file_order_lag: Duration::ZERO,
        demux_packets_per_tick: PlayerTickConfig::default().max_demux_packets_per_tick,
        video_decoder_frame_limit: None,
        playback_rate: PlaybackRate::NORMAL,
    }
}

/// Файл с разрывом звука посреди: звук 0–5 с и 12–20 с.
fn mid_file_audio_gap_layout() -> MediaLayout {
    MediaLayout {
        video_start: Duration::ZERO,
        video_frame_interval: Duration::from_millis(100),
        video_end: Duration::from_secs(20),
        audio_ranges: vec![
            (Duration::ZERO, Duration::from_secs(5)),
            (Duration::from_secs(12), Duration::from_secs(20)),
        ],
        audio_packet_duration: AUDIO_PACKET,
        // Обычный лимит: demux успевает дочитать звук 12 с, разрыв доказывает его PTS.
        audio_catchup_video_packet_limit: 240,
        audio_file_order_lag: Duration::ZERO,
        demux_packets_per_tick: PlayerTickConfig::default().max_demux_packets_per_tick,
        video_decoder_frame_limit: None,
        playback_rate: PlaybackRate::NORMAL,
    }
}

#[test]
fn audio_ending_long_before_video_plays_video_to_end_and_reaches_ended() {
    let layout = audio_ends_at_five_seconds_layout();
    let mut scenario = GapScenario::start(&layout);
    let mut observations = PlaybackObservations::default();

    let _ended_at = scenario.play_until_ended(Instant::now(), &mut observations);

    assert!(
        observations.handed_over,
        "video must continue on the monotonic clock"
    );
    assert_frames_presented_in_order_from(
        &observations,
        &scenario.video_pts,
        Duration::from_secs(5),
    );
    let tail_wall_time = wall_time_between(
        &observations,
        Duration::from_secs(5),
        Duration::from_millis(19_900),
    );
    assert!(
        tail_wall_time >= Duration::from_millis(14_500)
            && tail_wall_time <= Duration::from_millis(15_300),
        "14.9 s of video without audio must play at real rate, took {tail_wall_time:?}"
    );
    assert_position_never_jumps_back(&observations);
    assert_eq!(
        scenario.session.snapshot().current_position,
        layout.video_end
    );
    assert_ended_event_published(&mut scenario.session);
}

#[test]
fn mid_file_audio_gap_keeps_video_running_and_audio_retakes_clock_in_sync() {
    let layout = mid_file_audio_gap_layout();
    let mut scenario = GapScenario::start(&layout);
    let mut observations = PlaybackObservations::default();

    let _ended_at = scenario.play_until_ended(Instant::now(), &mut observations);

    assert!(
        observations.handed_over,
        "the gap must be bridged by the monotonic clock"
    );
    assert_frames_presented_in_order_from(&observations, &scenario.video_pts, Duration::ZERO);
    let gap_wall_time = wall_time_between(
        &observations,
        Duration::from_secs(5),
        Duration::from_secs(12),
    );
    assert!(
        gap_wall_time >= Duration::from_millis(6_700)
            && gap_wall_time <= Duration::from_millis(7_300),
        "7 s of video inside the audio gap must play at real rate, took {gap_wall_time:?}"
    );
    let (resumed_at, submitted_before_resume) = observations
        .audio_resume
        .expect("audio after the gap must take the clock back");
    assert!(
        resumed_at >= Duration::from_secs(12) && resumed_at < Duration::from_millis(12_100),
        "audio must return when presentation reaches its PTS, returned at {resumed_at:?}"
    );
    assert!(
        observations.audio_drives_after_resume_at.is_some(),
        "after the gap the audio clock must drive presentation again"
    );
    // Звук после разрыва обрезан ровно по позиции возврата: весь записанный PCM
    // покрывает [resumed_at, 20 с) — то есть звучит синхронно с видео.
    let audio_after_resume = duration_for(
        scenario
            .device
            .submitted_frames()
            .saturating_sub(submitted_before_resume),
    );
    let expected_audio_after_resume = Duration::from_secs(20) - resumed_at;
    let sync_error = audio_after_resume.abs_diff(expected_audio_after_resume);
    assert!(
        sync_error <= Duration::from_millis(5),
        "audio after the gap must start at the presentation position: \
         written {audio_after_resume:?}, expected {expected_audio_after_resume:?}"
    );
    assert_position_never_jumps_back(&observations);
    assert_eq!(
        scenario.session.snapshot().current_position,
        layout.video_end
    );
    assert_ended_event_published(&mut scenario.session);
}

#[test]
fn sparse_interleaved_audio_never_hands_over_the_clock() {
    // Один audio packet на секунду звука, между ними demux читает 30 video packets, а
    // muxer положил звук на 2 с позже видео: output успевает опустеть, пока медленный
    // demux (3 packets за tick) дочитывает до следующего звука. Звук редкий и запаздывает
    // в файле, но непрерывен — разрыва нет, и часы отдавать нельзя.
    let layout = MediaLayout {
        video_start: Duration::ZERO,
        video_frame_interval: Duration::from_millis(33),
        video_end: Duration::from_secs(10),
        audio_ranges: vec![(Duration::ZERO, Duration::from_secs(10))],
        audio_packet_duration: Duration::from_secs(1),
        audio_catchup_video_packet_limit: 240,
        audio_file_order_lag: Duration::from_secs(2),
        demux_packets_per_tick: 3,
        video_decoder_frame_limit: None,
        playback_rate: PlaybackRate::NORMAL,
    };
    let mut scenario = GapScenario::start(&layout);
    let mut observations = PlaybackObservations::default();

    let _ended_at = scenario.play_until_ended(Instant::now(), &mut observations);

    assert!(
        !observations.handed_over,
        "continuous audio must keep driving the clock (no false gap)"
    );
    assert_eq!(
        observations.presented.last().map(|(pts, _)| *pts),
        scenario.video_pts.last().copied()
    );
    assert_eq!(
        scenario.session.pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput
    );
    assert_ended_event_published(&mut scenario.session);
}

#[test]
fn lagging_video_decoder_with_continuous_audio_never_hands_the_clock_over() {
    // Живой T410: звук непрерывный, HW decoder не тянет 4x. Decoder «захлебнулся» после
    // 30 кадров, очередь pending video packets забита до лимита видео, которое лежит ДО
    // конца записанного звука, а следующий audio packet стоит сразу за ним в файле.
    // Насыщение demux-а здесь ничего не доказывает: часы звуку отнимать нельзя.
    let layout = MediaLayout {
        video_start: Duration::ZERO,
        video_frame_interval: Duration::from_millis(33),
        video_end: Duration::from_secs(20),
        audio_ranges: vec![(Duration::ZERO, Duration::from_secs(20))],
        audio_packet_duration: AUDIO_PACKET,
        audio_catchup_video_packet_limit: 240,
        audio_file_order_lag: Duration::ZERO,
        demux_packets_per_tick: PlayerTickConfig::default().max_demux_packets_per_tick,
        video_decoder_frame_limit: Some(30),
        playback_rate: PlaybackRate::new(4.0).expect("4x must be a valid playback rate"),
    };
    let mut scenario = GapScenario::start(&layout);
    let mut observations = PlaybackObservations::default();
    let started_at = Instant::now();

    // Ждём насыщения: очередь video packets дошла до лимита audio-priority demux-а.
    let saturated_at = scenario.play_until(
        started_at,
        &mut observations,
        "pending video queue saturated",
        |scenario, _| scenario.session.pipeline.pending_video_packet_len() >= 240,
    );
    // И даём ещё несколько секунд: ложное доказательство сработало бы на ближайших tick-ах.
    let _end = scenario.play_until(
        saturated_at,
        &mut observations,
        "several seconds after saturation",
        |_, observations| observations.positions.len() > 400,
    );

    assert!(
        !observations.handed_over,
        "saturated demux behind a lagging decoder must not prove audio absence"
    );
    assert_eq!(
        scenario.clock_source(),
        PresentationClockSource::AudioOutput
    );
}

#[test]
fn seek_into_region_without_audio_hands_the_clock_over_when_demux_saturates() {
    // Файл 13b: видео 0–20 с, звук только 0–5 с. Seek на 10 с при Playing: после seek-а
    // звук не пишется вовсе (конец записанного звука сброшен), decoder не успевает,
    // audio-priority demux упирается в лимит video packets. Это насыщение обязано
    // доказать отсутствие звука относительно позиции seek-а, иначе Playing стоит вечно.
    let seek_target = Duration::from_secs(10);
    let layout = MediaLayout {
        video_start: seek_target,
        video_end: Duration::from_secs(40),
        audio_ranges: Vec::new(),
        video_decoder_frame_limit: Some(20),
        ..audio_ends_at_five_seconds_layout()
    };
    let mut scenario = GapScenario::start(&layout);
    let mut observations = PlaybackObservations::default();
    scenario.start_seek_into_region_without_audio(seek_target);

    let handed_over_at = scenario.play_until(
        Instant::now(),
        &mut observations,
        "clock handed over after the seek into the silent region",
        |scenario, _| scenario.clock_source() == PresentationClockSource::MonotonicAfterAudioEnd,
    );

    assert!(
        scenario.session.pipeline.pending_video_packet_len() >= 40,
        "the handover must come from demux saturation, not from EOF"
    );
    assert_eq!(
        scenario
            .session
            .presentation_clock_position_at(handed_over_at),
        seek_target,
        "the monotonic clock starts at the seek position"
    );
}

#[test]
fn seek_at_4x_into_audio_region_with_lagging_decoder_keeps_audio_clock() {
    // Симметричный негатив: после seek-а звук есть (непрерывный), но 4x HW decoder
    // отстаёт и очередь video packets забита до лимита. Звук ещё не прочитан demux-ом
    // и не записан; насыщение не доказывает его отсутствия только если видео в очереди
    // не ушло дальше позиции seek-а на MIN_AUDIO_GAP.
    let seek_target = Duration::from_secs(10);
    let layout = MediaLayout {
        video_start: seek_target,
        video_end: Duration::from_secs(40),
        video_frame_interval: Duration::from_millis(33),
        audio_ranges: vec![(Duration::from_millis(9_900), Duration::from_secs(40))],
        audio_file_order_lag: Duration::from_secs(2),
        audio_catchup_video_packet_limit: 10,
        video_decoder_frame_limit: Some(5),
        playback_rate: PlaybackRate::new(4.0).expect("4x must be a valid playback rate"),
        ..audio_ends_at_five_seconds_layout()
    };
    let mut scenario = GapScenario::start(&layout);
    let mut observations = PlaybackObservations::default();
    scenario.start_seek_into_region_without_audio(seek_target);

    let _end = scenario.play_until(
        Instant::now(),
        &mut observations,
        "several seconds after the seek",
        |_, observations| observations.positions.len() > 200,
    );

    assert!(
        !observations.handed_over,
        "audio exists right after the seek: the clock must stay with the audio output"
    );
}

#[test]
fn seek_back_from_audio_gap_returns_presentation_to_audio_clock() {
    let layout = mid_file_audio_gap_layout();
    let mut scenario = GapScenario::start(&layout);
    let mut observations = PlaybackObservations::default();
    let started_at = Instant::now();
    let gap_at = scenario.play_until(
        started_at,
        &mut observations,
        "inside the gap",
        |scenario, _| {
            scenario.clock_source() == PresentationClockSource::MonotonicAfterAudioEnd
                && scenario.session.snapshot().current_position >= Duration::from_secs(8)
        },
    );
    let seek_target = Duration::from_secs(2);

    scenario
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_duration(seek_target),
        )))
        .unwrap();
    assert_eq!(
        scenario.clock_source(),
        PresentationClockSource::AudioOutput,
        "a seek must cancel the gap handover"
    );

    // Seek приземляется в звуковой отрезок: целевой кадр показан, звук буферизован.
    scenario.device.submit_for_test(Duration::from_millis(200));
    present_frame_for_current_seek_generation(
        &mut scenario.session,
        Duration::from_millis(2_000),
        990,
    );
    let seek_commit = scenario
        .session
        .seek_commit()
        .expect("seek must stay active until audio commits");
    scenario.session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(scenario.session.seek_commit().is_none());
    assert_eq!(scenario.session.playback_state(), PlaybackState::Playing);
    assert_eq!(scenario.session.snapshot().current_position, seek_target);
    assert_eq!(
        scenario.clock_source(),
        PresentationClockSource::AudioOutput
    );
    let far_future = gap_at + Duration::from_secs(30);
    assert_eq!(
        scenario.session.presentation_clock_position_at(far_future),
        seek_target,
        "without audio progress wall time alone must not move the position"
    );
    scenario.device.advance(Duration::from_millis(120));
    assert_eq!(
        scenario.session.presentation_clock_position_at(far_future),
        seek_target + Duration::from_millis(120),
        "audio progress drives the position again"
    );
    assert_ne!(scenario.session.playback_state(), PlaybackState::Ended);
}
