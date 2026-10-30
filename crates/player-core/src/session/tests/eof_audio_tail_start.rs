//! Вечный `Draining`: звуковой хвост после EOF обязан запускаться, даже когда в очереди
//! ещё лежит сжатый звук.
//!
//! Три зависания с одним корнем (2026-10-30): хвост звука запускался только когда
//! все считанные audio packets уже декодированы (`DrainingOutput`), а декод ждал, пока
//! уровень output-а опустится ниже предела, — без запущенного звука он не опускается
//! никогда. Плюс для короткого файла без единого кадра: старт ждал кадр, а защёлка
//! «звук первым» в `Draining` была недостижима.
//!
//! Стенд: настоящий MKV (VP8 + FLAC + Opus, 3 с), настоящий демуксер и аудио-декодеры,
//! fake video decoder и fake «звуковая карта», которая расходует записанный звук только
//! после `play` (как настоящее устройство), с симулированными часами.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio_core::{
    AudioOutputClockTiming, AudioOutputDeviceRequest, AudioOutputDeviceRoute,
    AudioOutputStreamHealth, AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport,
    CreatedAudioOutput,
};

use super::audio_track_switch::harness::{install_media, open_fixture};
use super::test_support::{
    FakeDemuxer, ScriptedAudioOutput, ScriptedAudioOutputHandle, SharedFakeVideoDecoderThread,
    counting_audio_decoder_handle, decoded_frame_for_current_seek_generation,
    decoded_frame_for_tests, fake_audio_packet, fake_track,
};
use super::*;
use crate::{MediaInstanceId, PlaybackIntent, StartedVideoBackend};

/// Видео со звуком: VP8 64x64, 10 fps, 3 с + FLAC + Opus.
const AUDIO_VIDEO_FIXTURE: &str = "two-audio-tracks.mkv";

/// Видео без звука: VP8 64x64, 10 fps, 3 с (+ ASS-дорожка, которая плееру не мешает).
const VIDEO_ONLY_FIXTURE: &str = "ass-with-font.mkv";

/// Длина фикстуры (см. `fixtures/subtitles/README.md`).
const FIXTURE_LENGTH: Duration = Duration::from_secs(3);

/// Шаг симулированного времени между tick-ами.
const SIMULATED_TICK_STEP: Duration = Duration::from_millis(50);

/// Конечный бюджет tick-ов: 3 с media при шаге 50 мс — 60 tick-ов, остальное запас.
const TICK_BUDGET: usize = 400;

/// Сколько звука фикстуры обязано прозвучать: вся длина минус последний packet.
const MIN_HEARD_AUDIO: Duration = Duration::from_millis(2_800);

/// Что «звуковая карта» успела принять и проиграть (общая для output-а и стенда).
#[derive(Debug, Default)]
struct SoundCardTimeline {
    /// Сколько звука записано в output.
    written: Duration,
    /// Сколько из записанного уже прозвучало.
    played: Duration,
    /// Запущен ли поток (`play` без последующей паузы).
    running: bool,
    /// Сколько звука было записано в момент первого `play` (на чём стартовал звук).
    written_at_first_play: Option<Duration>,
}

/// Output, похожий на настоящее устройство: уровень растёт с записью и убывает, только
/// пока поток запущен. `ScriptedAudioOutput` держит уровень константой.
struct SoundCardOutput {
    inner: Box<dyn PlayerAudioOutput>,
    handle: ScriptedAudioOutputHandle,
    timeline: Arc<Mutex<SoundCardTimeline>>,
    sample_rate: u32,
    channels: usize,
}

impl PlayerAudioOutput for SoundCardOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let report = self.inner.write_samples(samples, intent)?;
        let frames = samples.len() / self.channels;
        let mut timeline = self.timeline.lock().expect("sound card timeline lock");
        timeline.written += Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate));
        self.handle
            .set_buffer_level_ms((timeline.written - timeline.played).as_secs_f64() * 1000.0);
        Ok(report)
    }

    fn play(&mut self) -> anyhow::Result<()> {
        self.inner.play()?;
        let mut timeline = self.timeline.lock().expect("sound card timeline lock");
        timeline.running = true;
        let written = timeline.written;
        timeline.written_at_first_play.get_or_insert(written);
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        self.timeline
            .lock()
            .expect("sound card timeline lock")
            .running = false;
        self.inner.pause_and_freeze_clock()
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.inner.clear_buffer_for_seek(generation)
    }

    fn set_volume(&mut self, volume: f32) {
        self.inner.set_volume(volume);
    }

    fn buffer_level_ms(&self) -> f64 {
        self.inner.buffer_level_ms()
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.inner.clock()
    }

    fn stream_health(&self) -> AudioOutputStreamHealth {
        self.inner.stream_health()
    }
}

/// Последний созданный output: handle для проверок и его «звуковая карта».
#[derive(Clone)]
struct SoundCard {
    handle: ScriptedAudioOutputHandle,
    timeline: Arc<Mutex<SoundCardTimeline>>,
}

impl SoundCard {
    /// Проигрывает `step` записанного звука, если поток запущен, и двигает часы карты.
    fn advance(&self, step: Duration) {
        let mut timeline = self.timeline.lock().expect("sound card timeline lock");
        if !timeline.running {
            return;
        }
        timeline.played = (timeline.played + step).min(timeline.written);
        self.handle
            .set_buffer_level_ms((timeline.written - timeline.played).as_secs_f64() * 1000.0);
        self.handle
            .clock
            .set_output_timing(timeline.played, timeline.played);
    }

    /// Сколько звука было в output-е, когда его впервые запустили.
    fn written_at_first_play(&self) -> Option<Duration> {
        self.timeline
            .lock()
            .expect("sound card timeline lock")
            .written_at_first_play
    }

    /// Сколько звука прозвучало.
    fn played(&self) -> Duration {
        self.timeline
            .lock()
            .expect("sound card timeline lock")
            .played
    }
}

/// Factory, отдающая `SoundCardOutput` и запоминающая его карту.
#[derive(Default)]
struct SoundCardFactory {
    last_card: Mutex<Option<SoundCard>>,
}

impl AudioOutputFactory for SoundCardFactory {
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        _device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        let channels = spec.channels() as usize;
        let (inner, handle) = ScriptedAudioOutput::new(0.0, None)
            .with_input_channels(channels)
            .into_parts();
        let timeline = Arc::new(Mutex::new(SoundCardTimeline::default()));
        *self.last_card.lock().expect("sound card lock") = Some(SoundCard {
            handle: handle.clone(),
            timeline: Arc::clone(&timeline),
        });
        let output = SoundCardOutput {
            inner,
            handle,
            timeline,
            sample_rate: spec.sample_rate,
            channels,
        };
        Ok(CreatedAudioOutput::new(
            Box::new(output),
            AudioOutputDeviceRoute::SystemDefault,
        ))
    }
}

/// Когда fake video decoder отдаёт кадры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FramePolicy {
    /// Никогда: decoder принимает packets и молчит (битый поток).
    Never,
    /// Только после входа session в EOF drain: кадр опоздал к концу файла.
    AfterEofDrainEntered,
}

/// Стенд короткого A/V файла с «настоящей» звуковой картой.
struct EofTailHarness {
    session: PlayerSession,
    video_decoder: SharedFakeVideoDecoderThread,
    output_factory: Arc<SoundCardFactory>,
    frame_policy: FramePolicy,
    acked_packets: usize,
    decoded_packets: usize,
    frames_presented: u64,
    /// Симулированное время, на которое tick-и опережают реальные часы после старта.
    simulated_offset: Duration,
    events: Vec<PlayerEvent>,
}

impl EofTailHarness {
    fn new(frame_policy: FramePolicy) -> Self {
        Self::with_fixture(AUDIO_VIDEO_FIXTURE, frame_policy)
    }

    fn with_fixture(fixture_name: &str, frame_policy: FramePolicy) -> Self {
        let output_factory = Arc::new(SoundCardFactory::default());
        let mut session = PlayerSession::with_audio_factories(
            Arc::new(audio::decoder::ProductionAudioDecoderFactory::default()),
            Arc::clone(&output_factory) as Arc<dyn AudioOutputFactory>,
        );
        install_media(
            &mut session,
            Box::new(open_fixture(fixture_name)),
            PlaybackIntent::StartPlaying,
        );
        let video_decoder = SharedFakeVideoDecoderThread::new();
        session.set_video_backend(StartedVideoBackend::from_decoder_thread(
            "eof-audio-tail-test-backend",
            video_decoder.clone(),
        ));
        Self {
            session,
            video_decoder,
            output_factory,
            frame_policy,
            acked_packets: 0,
            decoded_packets: 0,
            frames_presented: 0,
            simulated_offset: Duration::ZERO,
            events: Vec::new(),
        }
    }

    fn sound_card(&self) -> Option<SoundCard> {
        self.output_factory
            .last_card
            .lock()
            .expect("sound card lock")
            .clone()
    }

    /// Один tick: decoder ack-ает packets и (по политике) отдаёт кадры; карта играет шаг.
    fn tick(&mut self) {
        let sent_packets = self.video_decoder.sent_packets();
        self.video_decoder
            .add_completed_packet_count(sent_packets.len() - self.acked_packets);
        self.acked_packets = sent_packets.len();
        let frames_are_due = match self.frame_policy {
            FramePolicy::Never => false,
            FramePolicy::AfterEofDrainEntered => self.session.is_eof_draining(),
        };
        if frames_are_due {
            for (packet_index, packet) in sent_packets.iter().enumerate().skip(self.decoded_packets)
            {
                let frame_handle = u64::try_from(packet_index).expect("small index") + 1;
                self.video_decoder
                    .push_decoded_frame(decoded_frame_for_tests(packet.pts, frame_handle));
            }
            self.decoded_packets = sent_packets.len();
        }

        let tick_now = Instant::now() + self.simulated_offset;
        let result = self.session.tick(PlayerTickContext::with_config(
            tick_now,
            PlayerTickConfig::default(),
        ));
        self.frames_presented += result.video_frames_presented;
        if let Some(sound_card) = self.sound_card() {
            sound_card.advance(SIMULATED_TICK_STEP);
        }
        if self
            .sound_card()
            .is_some_and(|card| card.played() > Duration::ZERO)
        {
            self.simulated_offset += SIMULATED_TICK_STEP;
        }
        self.events.extend(self.session.take_events());
    }

    /// Тикает до `Ended`; возвращает `false`, если бюджет кончился раньше.
    fn play_until_ended(&mut self) -> bool {
        for _ in 0..TICK_BUDGET {
            if self.session.playback_state() == PlaybackState::Ended {
                return true;
            }
            self.tick();
        }
        self.session.playback_state() == PlaybackState::Ended
    }

    fn audio_resumed_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, PlayerEvent::AudioPlaybackResumed))
            .count()
    }

    /// Диагностика зависания для сообщения теста.
    fn describe(&self) -> String {
        format!(
            "state={:?} position={:?} pending_audio={} level_ms={:?} played={:?} frames={}",
            self.session.playback_state(),
            self.session.snapshot().current_position,
            self.session.pipeline.pending_audio_packet_len(),
            self.session.audio_buffer_level_ms(),
            self.sound_card().map(|card| card.played()),
            self.frames_presented,
        )
    }

    /// Общие проверки «зритель услышал весь файл, и плеер закончил».
    fn assert_heard_whole_file_and_ended(&mut self) {
        let ended = self.play_until_ended();
        assert!(ended, "media must reach Ended: {}", self.describe());
        let sound_card = self.sound_card().expect("audio output created");
        assert_eq!(
            sound_card.handle.play_count(),
            1,
            "audio stream starts exactly once"
        );
        assert_eq!(self.audio_resumed_count(), 1);
        assert!(
            sound_card.played() >= MIN_HEARD_AUDIO,
            "the whole file must be heard: {:?}",
            sound_card.played()
        );
        let media_duration = self
            .session
            .snapshot()
            .duration
            .expect("static media duration");
        assert!(
            media_duration >= FIXTURE_LENGTH,
            "fixture is {FIXTURE_LENGTH:?} long: {media_duration:?}"
        );
        assert_eq!(
            self.session.snapshot().current_position,
            media_duration,
            "Ended publishes the end of the media"
        );
    }
}

/// Случай 1: короткий файл (3 с, меньше запаса ожидания видео 10 с), decoder не выдаёт
/// ни одного кадра. Раньше: вечный `Draining`, позиция 0, звука нет. Решение владельца:
/// картинка не пришла — играет звук.
#[test]
fn short_media_without_any_frame_plays_audio_to_ended() {
    let mut harness = EofTailHarness::new(FramePolicy::Never);

    harness.assert_heard_whole_file_and_ended();
    assert_eq!(harness.frames_presented, 0, "there is no frame to show");
}

/// Защёлка «видео больше не придёт» не срабатывает раньше времени: пока decoder ещё
/// дожимает EOF (кадр из DPB может прийти), звук ждёт; после `Drained` — играет.
#[test]
fn audio_first_in_eof_drain_waits_until_the_video_decoder_is_drained() {
    let mut harness = EofTailHarness::new(FramePolicy::Never);
    let generation = harness.session.pipeline.seek_generation();
    // Decoder начал EOF drain, но ещё не закончил: DPB может отдать кадр.
    harness.video_decoder.push_eof_drain_result(
        video_core::VideoDecoderEndOfStreamDrainResult::Started(
            video_core::VideoDecoderEndOfStreamDrainState::Draining { generation },
        ),
    );

    for _ in 0..TICK_BUDGET / 4 {
        harness.tick();
    }
    assert_eq!(
        harness.session.playback_state(),
        PlaybackState::Draining,
        "{}",
        harness.describe()
    );
    assert_eq!(
        harness.video_decoder.eof_drain_requests(),
        vec![generation],
        "decoder EOF drain must have been requested"
    );
    let sound_card = harness.sound_card().expect("audio output created");
    assert_eq!(
        sound_card.handle.play_count(),
        0,
        "audio must keep waiting while the decoder may still yield a frame"
    );

    harness
        .video_decoder
        .set_eof_drain_state(video_core::VideoDecoderEndOfStreamDrainState::Drained { generation });
    harness.assert_heard_whole_file_and_ended();
    assert_eq!(harness.frames_presented, 0);
}

/// Видео без звука и без единого кадра: ждать нечего, файл доходит до `Ended`, а
/// отказ от ожидания кадра не создаёт звук из ничего.
#[test]
fn short_video_only_media_without_any_frame_ends() {
    let mut harness = EofTailHarness::with_fixture(VIDEO_ONLY_FIXTURE, FramePolicy::Never);

    assert!(harness.play_until_ended(), "{}", harness.describe());
    assert!(harness.sound_card().is_none(), "no audio track, no output");
    assert_eq!(harness.frames_presented, 0);
}

/// Случай 3: кадр пришёл уже в `Draining`, а стартовый read-ahead успел записать в
/// output потолок (1,5 с) и оставить остаток сжатым. Раньше старт «выполнялся», но звук
/// не запускался (очередь не пуста) — дедлок, позиция 0 навсегда.
#[test]
fn late_frame_in_eof_drain_with_audio_over_startup_cap_plays_to_end() {
    let mut harness = EofTailHarness::new(FramePolicy::AfterEofDrainEntered);

    harness.assert_heard_whole_file_and_ended();
    assert!(
        harness.frames_presented > 0,
        "the late frames must be presented"
    );
}

/// Уровень output-а, при котором playing A/V законно замораживается ожиданием источника:
/// ниже high-water (demux читает) и в пределах «ожидание + запас 100 мс».
const LEVEL_AT_FREEZE_MS: f64 = 50.0;

/// Уровень paused output-а к моменту EOF: выше high-water (200 мс), поэтому обычный
/// декод следующего сжатого packet-а ждёт, пока звук не начнёт убывать.
const LEVEL_AT_EOF_MS: f64 = 250.0;

/// Позиция, на которой playback заморожен.
const FROZEN_POSITION: Duration = Duration::from_secs(2);

/// Playing A/V с источником, который отвечает «подождите», а потом кончается.
fn playing_av_session_with_source_that_waits_then_ends()
-> (PlayerSession, ScriptedAudioOutputHandle) {
    let audio_track = fake_track(1, TrackKind::Audio);
    let video_track = fake_track(2, TrackKind::Video);
    let tracks = vec![audio_track.clone(), video_track];
    let mut demuxer = FakeDemuxer::new(
        tracks.clone(),
        Some(Duration::from_secs(30)),
        Arc::new(Mutex::new(Vec::new())),
    );
    let retry_hint = media_core::DemuxRetryHint::new(media_core::DemuxRetryHint::MIN_RETRY_AFTER)
        .expect("minimum retry hint");
    demuxer.push_event(DemuxReadEvent::TemporarilyUnavailable(retry_hint));
    demuxer.push_event(DemuxReadEvent::EndOfStream);

    let mut session = PlayerSession::new();
    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    session
        .select_default_video_track(&tracks, "fixture must contain video")
        .expect("video track selected");
    session.pipeline.select_audio_track(audio_track.id);
    session.snapshot.media_instance_id = Some(MediaInstanceId::new_unique());
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(Arc::default()));
    let (audio_output, output) = ScriptedAudioOutput::new(LEVEL_AT_FREEZE_MS, None).into_parts();
    session
        .pipeline
        .install_audio_output_for_tests(audio_output);
    session.pipeline.install_audio_clock(output.clock());
    output.set_underlying_output_timing(FROZEN_POSITION, FROZEN_POSITION);
    let frame = decoded_frame_for_current_seek_generation(&session, FROZEN_POSITION, 91);
    session.pipeline.enqueue_queued_video_frame(frame);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("fixture enters Playing");
    (session, output)
}

/// Tick config стенда заморозки: одно событие источника за tick.
fn frozen_eof_tick_config() -> PlayerTickConfig {
    PlayerTickConfig {
        max_demux_packets_per_tick: 1,
        adaptive_catch_up_time_budget: Duration::ZERO,
        audio_preroll_target_ms: 50.0,
        ..PlayerTickConfig::default()
    }
}

/// Случай 2: playing A/V заморожен ожиданием источника (`Buffering`, output на паузе).
/// Пока плеер ждал, звук дописался в output выше high-water, а следующий сжатый packet
/// остался в очереди; затем источник кончился. Раньше хвост не запускался
/// (`PendingPackets`), декод стоял — вечный `Draining` с застывшей позицией.
///
/// Заполнение output-а и pending packet задаются напрямую: это состояние «во время
/// ожидания» (декод в paused output, read-ы ограничены high-water), а переходы
/// freeze → EOF → хвост → `Ended` идут обычными tick-ами.
#[test]
fn eof_after_freeze_with_pending_audio_over_high_water_plays_the_tail() {
    let (mut session, output) = playing_av_session_with_source_that_waits_then_ends();
    let tick_config = frozen_eof_tick_config();
    let tick = |session: &mut PlayerSession| {
        let _tick_result =
            session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));
    };

    tick(&mut session);
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output.pause_count(), 1, "freeze pauses the output");
    let play_count_before_eof = output.play_count();

    output.set_buffer_level_ms(LEVEL_AT_EOF_MS);
    let generation = session.pipeline.seek_generation();
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            TrackId::new(1),
            FROZEN_POSITION,
            None,
            Some(Duration::from_millis(20)),
            generation,
            Bytes::from_static(b"audio-packet"),
        ));
    std::thread::sleep(media_core::DemuxRetryHint::MIN_RETRY_AFTER + Duration::from_millis(1));
    tick(&mut session);
    assert_eq!(session.playback_state(), PlaybackState::Draining);
    assert_eq!(
        output.play_count(),
        play_count_before_eof + 1,
        "EOF must resume the frozen output to play the tail"
    );

    // Звуковая карта играет хвост: уровень падает, часы идут, packet декодируется.
    output.set_buffer_level_ms(0.0);
    output.set_underlying_output_timing(
        FROZEN_POSITION + Duration::from_millis(300),
        FROZEN_POSITION + Duration::from_millis(300),
    );
    for _ in 0..10 {
        if session.playback_state() == PlaybackState::Ended {
            break;
        }
        tick(&mut session);
    }
    assert_eq!(session.pipeline.pending_audio_packet_len(), 0);
    assert_eq!(
        session.playback_state(),
        PlaybackState::Ended,
        "the tail must finish the media"
    );
}

/// Частота и каналы тонового «декодера».
const TONE_SAMPLE_RATE: u32 = 48_000;
const TONE_CHANNELS: u32 = 2;
/// Один packet тона — 1024 кадра (~21,3 мс), как AAC.
const TONE_PACKET_FRAMES: usize = 1_024;

/// Длительность одного packet-а тона.
fn tone_packet_duration() -> Duration {
    Duration::from_secs_f64(TONE_PACKET_FRAMES as f64 / f64::from(TONE_SAMPLE_RATE))
}

/// Decoder, отдающий на каждый packet ровно один блок тона (наблюдаемый PCM).
struct ToneDecoder;

impl audio_core::AudioDecoder for ToneDecoder {
    fn decode(&mut self, _packet: &audio_core::EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.5; TONE_PACKET_FRAMES * TONE_CHANNELS as usize])
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        TONE_SAMPLE_RATE
    }

    fn channels(&self) -> u32 {
        TONE_CHANNELS
    }

    fn channel_layout(&self) -> Option<audio_core::AudioChannelLayout> {
        Some(audio_core::AudioChannelLayout::stereo())
    }
}

/// Factory тонового decoder-а.
struct ToneDecoderFactory;

impl audio_core::AudioDecoderFactory for ToneDecoderFactory {
    fn create_decoder(
        &self,
        _config: audio_core::AudioDecoderConfig,
    ) -> anyhow::Result<audio_core::AudioDecoderHandle> {
        Ok(Box::new(ToneDecoder))
    }
}

/// Audio-only короткий файл: EOF приходит, когда в output-е меньше preroll target-а
/// (два packet-а ≈ 43 мс < 50 мс), а третий packet ещё сжат (прочитан тем же tick-ом). Хвост первого просмотра не
/// стартует сам в обход ворот старта: звук запускается, только когда ворота готовы
/// (весь считанный звук уже в output-е), а не на тонком буфере с pending.
#[test]
fn audio_only_startup_in_eof_drain_does_not_start_on_thin_output_with_pending_audio() {
    let audio_track = fake_track(1, TrackKind::Audio);
    let mut demuxer = FakeDemuxer::new(
        vec![audio_track.clone()],
        Some(Duration::from_secs(30)),
        Arc::new(Mutex::new(Vec::new())),
    );
    let tone_packet = |packet_index: u32| {
        DemuxReadEvent::Packet(fake_audio_packet(
            audio_track.id,
            tone_packet_duration() * packet_index,
            tone_packet_duration(),
        ))
    };
    let retry_hint = media_core::DemuxRetryHint::new(media_core::DemuxRetryHint::MIN_RETRY_AFTER)
        .expect("minimum retry hint");
    // Первый tick: два packet-а и пауза источника (output ≈ 43 мс, старт ждёт 50 мс).
    // Следующий tick: третий packet и EOF в одном чтении — он ещё сжат при входе в drain.
    for event in [
        tone_packet(0),
        tone_packet(1),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint),
        tone_packet(2),
        DemuxReadEvent::EndOfStream,
    ] {
        demuxer.push_event(event);
    }
    let output_factory = Arc::new(SoundCardFactory::default());
    let mut session = PlayerSession::with_audio_factories(
        Arc::new(ToneDecoderFactory),
        Arc::clone(&output_factory) as Arc<dyn AudioOutputFactory>,
    );
    install_media(
        &mut session,
        Box::new(demuxer),
        PlaybackIntent::StartPlaying,
    );
    for _ in 0..4 {
        let _tick_result = session.tick(PlayerTickContext::new(Instant::now()));
        std::thread::sleep(media_core::DemuxRetryHint::MIN_RETRY_AFTER + Duration::from_millis(1));
    }

    assert!(session.is_eof_draining(), "{:?}", session.playback_state());
    let sound_card = output_factory
        .last_card
        .lock()
        .expect("sound card lock")
        .clone()
        .expect("audio output created");
    assert_eq!(
        sound_card.handle.play_count(),
        1,
        "audio starts exactly once"
    );
    let all_audio = tone_packet_duration() * 3;
    let started_with = sound_card
        .written_at_first_play()
        .expect("audio was started");
    assert!(
        started_with + Duration::from_micros(10) >= all_audio,
        "audio must start through the startup gate with all read audio, \
         not on a thin buffer: started with {started_with:?} of {all_audio:?}"
    );
}

/// Пауза во время `Draining`: EOF-хвост в output-е есть, но звук сам не возобновляется.
#[test]
fn pause_during_eof_drain_keeps_the_tail_silent() {
    let (mut session, output) = playing_av_session_with_source_that_waits_then_ends();
    let tick_config = frozen_eof_tick_config();
    let tick = |session: &mut PlayerSession| {
        let _tick_result =
            session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));
    };
    tick(&mut session);
    output.set_buffer_level_ms(LEVEL_AT_EOF_MS);
    std::thread::sleep(media_core::DemuxRetryHint::MIN_RETRY_AFTER + Duration::from_millis(1));
    tick(&mut session);
    assert_eq!(session.playback_state(), PlaybackState::Draining);
    let play_count_while_draining = output.play_count();
    let pause_count_while_draining = output.pause_count();

    session
        .dispatch_command(PlayerCommand::Pause)
        .expect("pause accepted");
    for _ in 0..5 {
        tick(&mut session);
    }

    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert!(output.pause_count() > pause_count_while_draining);
    assert_eq!(
        output.play_count(),
        play_count_while_draining,
        "paused tail must not be restarted by the EOF drain"
    );
}

/// Звук после EOF удержан до своего PTS (разрыв), а output пуст: запускать нечего —
/// EOF не стартует output раньше, вход звука ведёт путь разрыва.
#[test]
fn eof_with_empty_output_and_held_audio_does_not_start_the_output() {
    let (mut session, output) = playing_av_session_with_source_that_waits_then_ends();
    let tick_config = frozen_eof_tick_config();
    let tick = |session: &mut PlayerSession| {
        let _tick_result =
            session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));
    };
    tick(&mut session);
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    let play_count_before_eof = output.play_count();

    // Output доигран и пуст; следующий звук начнётся через 5 с — после разрыва.
    output.set_buffer_level_ms(0.0);
    let generation = session.pipeline.seek_generation();
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            TrackId::new(1),
            FROZEN_POSITION + Duration::from_secs(5),
            None,
            Some(Duration::from_millis(20)),
            generation,
            Bytes::from_static(b"audio-packet"),
        ));
    std::thread::sleep(media_core::DemuxRetryHint::MIN_RETRY_AFTER + Duration::from_millis(1));
    tick(&mut session);

    assert_eq!(session.playback_state(), PlaybackState::Draining);
    assert!(
        session.pending_audio_is_held_until_its_pts(),
        "precondition: the next audio waits for its PTS"
    );
    assert_eq!(
        output.play_count(),
        play_count_before_eof,
        "an empty output must not be started ahead of the held audio"
    );
}
