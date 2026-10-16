//! Регрессия «короткий файл не стартует»: весь файл демуксится до EOF раньше, чем decoder
//! отдаёт первый кадр.
//!
//! Раньше session входила в EOF drain (`Draining`) ещё во время стартового autoplay preroll,
//! preroll молча переставал завершаться (он ждал только `Buffering`), часы не запускались,
//! а пришедший следом D52 intent `StartPlaying` трактовался как «повтор после конца» и
//! запускал seek в 0 — app-сторона видела `UnexpectedSeek` вместо первого кадра.
//!
//! Сценарии используют настоящий symphonia-демуксер и маленькие фикстуры (VP8 10 fps, 3 с),
//! fake video decoder и fake audio output. Гонка «EOF раньше первого кадра» воспроизводится
//! детерминированно: decoder отдаёт первый кадр только после того, как session вошла в drain.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::audio_track_switch::harness::{AudioOnlyDemuxer, install_media, open_fixture};
use super::test_support::{
    ScriptedAudioOutputFactory, ScriptedAudioOutputFactoryHandle, ScriptedAudioOutputHandle,
    SharedFakeVideoDecoderThread, decoded_frame_for_tests,
};
use super::*;
use crate::{MediaInstanceId, PlaybackIntent, StartedVideoBackend};

/// Видео без звука: VP8 64x64, 10 fps, 3 с (+ ASS-дорожка, которая плееру не мешает).
const VIDEO_ONLY_FIXTURE: &str = "ass-with-font.mkv";

/// Видео со звуком: VP8 64x64, 10 fps, 3 с + FLAC + Opus.
const AUDIO_VIDEO_FIXTURE: &str = "two-audio-tracks.mkv";

/// Длина обеих фикстур (см. `fixtures/subtitles/README.md`).
const FIXTURE_LENGTH: Duration = Duration::from_secs(3);

/// Обычное окно demux-а на tick: файл читается постепенно, как при реальной игре.
const GRADUAL_DEMUX_PACKETS_PER_TICK: usize = 6;

/// Окно, в которое весь короткий файл влезает за один tick (EOF в первом же tick-е).
const WHOLE_FILE_DEMUX_PACKETS_PER_TICK: usize = 10_000;

/// Шаг симулированного времени между tick-ами.
const SIMULATED_TICK_STEP: Duration = Duration::from_millis(50);

/// Конечный бюджет tick-ов: 3 с media при шаге 50 мс — 60 tick-ов, остальное запас.
const TICK_BUDGET: usize = 400;

/// Когда fake decoder начинает отдавать кадры (по одному на каждый принятый packet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstFramePolicy {
    /// Только после входа session в EOF drain — та самая гонка короткого файла.
    AfterEofDrainEntered,
    /// Сразу после первого принятого packet-а — обычный (длинный) старт.
    AfterFirstPacket,
}

/// Стенд короткого файла: настоящий демуксер, fake decoder, fake output, симулированные часы.
struct ShortMediaHarness {
    session: PlayerSession,
    media_instance_id: MediaInstanceId,
    video_decoder: SharedFakeVideoDecoderThread,
    output_factory: ScriptedAudioOutputFactoryHandle,
    first_frame_policy: FirstFramePolicy,
    /// Сколько принятых packets уже превращено в decoded frames.
    decoded_packets: usize,
    acked_packets: usize,
    frames_presented: u64,
    /// Симулированное время, на которое tick-и опережают реальные часы после старта.
    ///
    /// До старта часы стоят, поэтому offset не растёт: иначе якорь no-audio часов (он
    /// берёт реальный `Instant::now()`) сразу «прыгнул» бы на накопленный разрыв.
    simulated_offset: Duration,
    /// Симулированное время, прошедшее с запуска audio stream-а (часы звуковой карты).
    audible_elapsed: Duration,
    events: Vec<PlayerEvent>,
    observed_states: Vec<PlaybackState>,
    demux_packets_per_tick: usize,
}

impl ShortMediaHarness {
    fn new(fixture_name: &str, first_frame_policy: FirstFramePolicy) -> Self {
        Self::with_demuxer(
            Box::new(open_fixture(fixture_name)),
            first_frame_policy,
            GRADUAL_DEMUX_PACKETS_PER_TICK,
        )
    }

    /// Audio-only короткий файл (видео спрятано), который демуксится целиком за один tick.
    fn audio_only_read_in_one_tick() -> Self {
        Self::with_demuxer(
            Box::new(AudioOnlyDemuxer::new(open_fixture(AUDIO_VIDEO_FIXTURE))),
            FirstFramePolicy::AfterFirstPacket,
            WHOLE_FILE_DEMUX_PACKETS_PER_TICK,
        )
    }

    fn with_demuxer(
        demuxer: Box<dyn Demuxer + Send>,
        first_frame_policy: FirstFramePolicy,
        demux_packets_per_tick: usize,
    ) -> Self {
        // Уровень 50 мс: не ниже порога готовности preroll и ниже high-water.
        let (output_factory, output_factory_handle) =
            ScriptedAudioOutputFactory::success(50.0, None);
        let mut session = PlayerSession::with_audio_factories(
            Arc::new(audio::decoder::ProductionAudioDecoderFactory::default()),
            output_factory,
        );
        let (_request_id, media_instance_id) =
            install_media(&mut session, demuxer, PlaybackIntent::StartPlaying);
        let video_decoder = SharedFakeVideoDecoderThread::new();
        session.set_video_backend(StartedVideoBackend::from_decoder_thread(
            "short-media-startup-test-backend",
            video_decoder.clone(),
        ));
        let mut harness = Self {
            session,
            media_instance_id,
            video_decoder,
            output_factory: output_factory_handle,
            first_frame_policy,
            decoded_packets: 0,
            acked_packets: 0,
            frames_presented: 0,
            simulated_offset: Duration::ZERO,
            audible_elapsed: Duration::ZERO,
            events: Vec::new(),
            observed_states: Vec::new(),
            demux_packets_per_tick,
        };
        harness.collect_events();
        harness
    }

    fn output(&self) -> Option<ScriptedAudioOutputHandle> {
        self.output_factory.last_output_handle()
    }

    /// Забирает события session и запоминает каждую смену playback state.
    fn collect_events(&mut self) {
        for event in self.session.take_events() {
            if let PlayerEvent::PlaybackStateChanged(state) = event {
                self.observed_states.push(state);
            }
            self.events.push(event);
        }
    }

    /// Один tick: decoder ack-ает принятые packets и (по политике) отдаёт по кадру на packet.
    fn tick(&mut self) {
        let sent_packets = self.video_decoder.sent_packets();
        self.video_decoder
            .add_completed_packet_count(sent_packets.len() - self.acked_packets);
        self.acked_packets = sent_packets.len();
        let frames_are_due = match self.first_frame_policy {
            FirstFramePolicy::AfterEofDrainEntered => self.session.is_eof_draining(),
            FirstFramePolicy::AfterFirstPacket => true,
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
        let config = PlayerTickConfig {
            max_demux_packets_per_tick: self.demux_packets_per_tick,
            ..PlayerTickConfig::default()
        };
        let result = self
            .session
            .tick(PlayerTickContext::with_config(tick_now, config));
        self.frames_presented += result.video_frames_presented;
        if self.playback_clock_runs() {
            self.simulated_offset += SIMULATED_TICK_STEP;
        }

        // Звуковая карта «играет» только после запуска stream-а.
        if let Some(output) = self.output()
            && output.play_count() > 0
        {
            self.audible_elapsed += SIMULATED_TICK_STEP;
            output
                .clock
                .set_output_timing(self.audible_elapsed, self.audible_elapsed);
            // Звуковая карта доиграла весь файл: buffer пуст, EOF drain может закончиться.
            if self.audible_elapsed >= FIXTURE_LENGTH {
                output.set_buffer_level_ms(0.0);
            }
        }
        self.collect_events();
    }

    /// Идут ли часы воспроизведения (старт выполнен, media ещё не закончился).
    fn playback_clock_runs(&self) -> bool {
        match self.session.playback_state() {
            PlaybackState::Playing => true,
            PlaybackState::Draining => !self.session.startup_autoplay_preroll_pending(),
            _ => false,
        }
    }

    /// Тикает, пока условие не выполнится; падает, если условие недостижимо.
    fn tick_until(&mut self, what: &str, mut condition: impl FnMut(&Self) -> bool) {
        for _ in 0..TICK_BUDGET {
            if condition(self) {
                return;
            }
            self.tick();
        }
        panic!(
            "condition never reached: {what}; state={:?}, states={:?}",
            self.session.playback_state(),
            self.observed_states
        );
    }

    /// Доставляет запоздавший D52 intent так же, как app после install completion.
    fn deliver_start_playing_intent(&mut self) {
        assert!(
            self.session
                .apply_playback_intent_to_exact_installed_instance(
                    self.media_instance_id,
                    PlaybackIntent::StartPlaying,
                )
        );
        self.collect_events();
    }

    fn seek_requested_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, PlayerEvent::SeekRequested(_)))
            .count()
    }

    fn audio_resumed_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, PlayerEvent::AudioPlaybackResumed))
            .count()
    }

    fn position(&self) -> Duration {
        self.session
            .snapshot
            .timeline
            .current_position
            .as_duration()
    }

    /// Доводит короткий файл до EOF drain раньше первого кадра (предусловие гонки).
    fn reach_eof_drain_before_first_frame(&mut self) {
        self.tick_until("EOF drain entered", |harness| {
            harness.session.is_eof_draining()
        });
        assert_eq!(
            self.frames_presented, 0,
            "race precondition: EOF must come before the first frame"
        );
        assert!(
            self.session.startup_autoplay_preroll_pending(),
            "startup autoplay must still be pending when EOF arrives first"
        );
    }

    /// Тикает до `Ended` (падает, если media так и не закончился).
    fn play_until_ended(&mut self) {
        self.tick_until("playback ended", |harness| {
            harness.session.playback_state() == PlaybackState::Ended
        });
    }
}

/// Видео без звука: EOF раньше первого кадра → старт всё равно происходит (часы идут,
/// кадр показан), запоздавший `StartPlaying` не делает replay-seek, файл доигрывается до Ended.
#[test]
fn short_video_only_media_starts_when_eof_arrives_before_first_frame() {
    let mut harness =
        ShortMediaHarness::new(VIDEO_ONLY_FIXTURE, FirstFramePolicy::AfterEofDrainEntered);
    harness.reach_eof_drain_before_first_frame();
    harness.deliver_start_playing_intent();

    harness.tick_until("first frame presented", |harness| {
        harness.frames_presented > 0
    });
    assert!(
        !harness.session.startup_autoplay_preroll_pending(),
        "startup must complete once the first frame is ready"
    );
    // Часы запущены: за несколько tick-ов позиция уходит от нуля, пока идёт drain.
    harness.tick_until("clock advances during drain", |harness| {
        harness.position() > Duration::ZERO
    });
    assert_eq!(harness.session.playback_state(), PlaybackState::Draining);

    harness.play_until_ended();
    assert_eq!(harness.seek_requested_count(), 0, "no replay seek allowed");
    assert_eq!(
        harness.observed_states,
        vec![
            PlaybackState::Buffering,
            PlaybackState::Draining,
            PlaybackState::Ended,
        ]
    );
    assert_eq!(Some(harness.position()), harness.session.snapshot.duration);
}

/// Видео со звуком: audio stream стартует ровно один раз и только вместе со стартом
/// (после первого кадра), звук доходит до output, replay-seek не происходит.
#[test]
fn short_audio_video_media_starts_audio_once_when_eof_arrives_before_first_frame() {
    let mut harness =
        ShortMediaHarness::new(AUDIO_VIDEO_FIXTURE, FirstFramePolicy::AfterEofDrainEntered);
    harness.reach_eof_drain_before_first_frame();
    let output = harness.output().expect("audio output created before EOF");
    assert_eq!(
        output.play_count(),
        0,
        "audio must wait for the startup gate, not start as a bare EOF tail"
    );
    harness.deliver_start_playing_intent();

    harness.play_until_ended();
    let output = harness.output().expect("audio output exists");
    assert_eq!(output.play_count(), 1, "audio stream starts exactly once");
    assert_eq!(harness.audio_resumed_count(), 1);
    assert!(
        !output.written_samples().is_empty(),
        "decoded audio must reach the output"
    );
    assert!(
        harness.frames_presented > 0,
        "first frame must be presented"
    );
    assert_eq!(harness.seek_requested_count(), 0, "no replay seek allowed");
    assert_eq!(
        harness.observed_states,
        vec![
            PlaybackState::Buffering,
            PlaybackState::Draining,
            PlaybackState::Ended,
        ]
    );
}

/// Обычный (длинный) путь не изменился: кадр пришёл раньше EOF → Buffering → Playing →
/// Draining → Ended, без seek-ов.
#[test]
fn media_with_first_frame_before_eof_keeps_playing_then_draining_sequence() {
    let mut harness =
        ShortMediaHarness::new(AUDIO_VIDEO_FIXTURE, FirstFramePolicy::AfterFirstPacket);
    harness.play_until_ended();

    assert_eq!(
        harness.observed_states,
        vec![
            PlaybackState::Buffering,
            PlaybackState::Playing,
            PlaybackState::Draining,
            PlaybackState::Ended,
        ]
    );
    assert_eq!(harness.seek_requested_count(), 0);
    let output = harness.output().expect("audio output exists");
    assert_eq!(output.play_count(), 1);
    assert_eq!(harness.audio_resumed_count(), 1);
}

/// Настоящий повтор после настоящего `Ended` по-прежнему перезапускает файл с нуля.
#[test]
fn play_after_real_end_of_short_media_still_replays_from_beginning() {
    let mut harness =
        ShortMediaHarness::new(VIDEO_ONLY_FIXTURE, FirstFramePolicy::AfterEofDrainEntered);
    harness.reach_eof_drain_before_first_frame();
    harness.play_until_ended();
    assert_eq!(harness.seek_requested_count(), 0);

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .expect("replay command");
    harness.collect_events();

    // Replay — внутренний seek-to-zero: state `Seeking` с целью 0.
    assert_eq!(harness.session.playback_state(), PlaybackState::Seeking);
    assert_eq!(
        harness.session.snapshot.timeline.target_position,
        Some(MediaTime::ZERO),
        "replay seeks to zero"
    );
    assert_eq!(
        harness.observed_states.last(),
        Some(&PlaybackState::Seeking)
    );
}

/// Play во время ещё не завершённого старта в drain — не replay: ни seek-а, ни смены state.
#[test]
fn play_command_while_startup_pending_in_eof_drain_does_not_replay() {
    let mut harness =
        ShortMediaHarness::new(VIDEO_ONLY_FIXTURE, FirstFramePolicy::AfterEofDrainEntered);
    harness.reach_eof_drain_before_first_frame();
    let states_before = harness.observed_states.clone();

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .expect("play command");
    harness.collect_events();

    assert_eq!(harness.seek_requested_count(), 0);
    assert_eq!(harness.session.playback_state(), PlaybackState::Draining);
    assert_eq!(harness.observed_states, states_before);
    assert!(harness.session.startup_autoplay_preroll_pending());
}

/// Seek во время отложенного старта забирает старт себе: флаг сброшен, seek доходит до
/// цели, а дальнейший Play после конца снова работает как replay.
#[test]
fn seek_while_startup_pending_clears_pending_startup() {
    let mut harness =
        ShortMediaHarness::new(VIDEO_ONLY_FIXTURE, FirstFramePolicy::AfterEofDrainEntered);
    harness.reach_eof_drain_before_first_frame();

    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::accurate(
            Duration::from_secs(1),
        )))
        .expect("seek command");
    harness.collect_events();

    assert_eq!(harness.seek_requested_count(), 1);
    assert!(!harness.session.startup_autoplay_preroll_pending());
}

/// Установка нового media поверх отложенного старта не переносит флаг на новый media.
#[test]
fn new_media_install_clears_pending_startup() {
    let mut harness =
        ShortMediaHarness::new(VIDEO_ONLY_FIXTURE, FirstFramePolicy::AfterEofDrainEntered);
    harness.reach_eof_drain_before_first_frame();

    let _ids = install_media(
        &mut harness.session,
        Box::new(open_fixture(VIDEO_ONLY_FIXTURE)),
        PlaybackIntent::StartPaused,
    );

    assert!(!harness.session.startup_autoplay_preroll_pending());
    assert_eq!(harness.session.playback_state(), PlaybackState::Paused);
}

/// Audio-only короткий файл, прочитанный до EOF в первом же tick-е: старт звука выполняет
/// тот же autoplay-старт (один `play`, один `AudioPlaybackResumed`), запоздавший
/// `StartPlaying` не делает replay, файл штатно доходит до Ended.
#[test]
fn short_audio_only_media_read_to_eof_in_first_tick_starts_once_without_replay() {
    let mut harness = ShortMediaHarness::audio_only_read_in_one_tick();
    harness.tick();
    assert!(
        harness.session.is_eof_draining(),
        "whole file must be demuxed in the first tick"
    );
    harness.deliver_start_playing_intent();

    harness.play_until_ended();
    let output = harness.output().expect("audio output exists");
    assert_eq!(output.play_count(), 1, "audio stream starts exactly once");
    assert_eq!(harness.audio_resumed_count(), 1);
    assert!(!output.written_samples().is_empty());
    assert_eq!(
        harness.observed_states,
        vec![
            PlaybackState::Buffering,
            PlaybackState::Draining,
            PlaybackState::Ended,
        ]
    );
}
