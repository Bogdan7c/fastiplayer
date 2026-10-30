//! Общий бэклог п. 24: источник умер посреди файла — плеер доигрывает уже прочитанное
//! и только потом сообщает ошибку.
//!
//! Стенд: настоящий symphonia-демуксер маленькой MKV-фикстуры, обёрнутый в
//! [`BreaksMidFileDemuxer`] — он отдаёт пакеты до точки обрыва, а дальше на каждое
//! чтение возвращает ошибку (как демуксер обрезанного fMP4/WebM). Video decoder —
//! fake (кадр на каждый принятый пакет), звуковой выход — fake «звуковая карта»,
//! которая играет только то, что в неё записали. Время симулированное.

use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, TrackInfo};

use super::audio_track_switch::harness::{install_media, open_fixture};
use super::test_support::{
    ScriptedAudioOutputFactory, ScriptedAudioOutputFactoryHandle, SharedFakeVideoDecoderThread,
    decoded_frame_for_tests,
};
use super::*;
use crate::{PlaybackIntent, PlayerErrorKind, StartedVideoBackend};

/// Видео со звуком: VP8 64x64, 10 fps, 3 с + FLAC + Opus.
const AUDIO_VIDEO_FIXTURE: &str = "two-audio-tracks.mkv";

/// Видео без звука: VP8 64x64, 10 fps, 3 с.
const VIDEO_ONLY_FIXTURE: &str = "ass-with-font.mkv";

/// Длительность кадра фикстур (10 fps).
const FRAME: Duration = Duration::from_millis(100);

/// Где «обрывается файл»: пакеты с PTS не меньше этого демуксер уже не отдаёт.
const BREAK_AT: Duration = Duration::from_millis(2_000);

/// Пакетов за tick: файл читается постепенно, как при реальной игре.
const DEMUX_PACKETS_PER_TICK: usize = 6;

/// Сколько кадров fake decoder выдаёт за tick: как настоящий decoder, постепенно
/// (3 кадра за 50 мс — втрое быстрее 10 fps фикстур), а не всё отправленное разом.
const DECODED_FRAMES_PER_TICK: usize = 3;

/// Шаг симулированного времени между tick-ами.
const TICK_STEP: Duration = Duration::from_millis(50);

/// Предел tick-ов: 3 с media при шаге 50 мс — 60 tick-ов, остальное запас.
const TICK_BUDGET: usize = 400;

/// Допуск «показан почти последний прочитанный кадр»: один кадр фикстуры.
///
/// Fake decoder отдаёт кадр на каждый пакет, поэтому при доигрывании показывается
/// ровно последний прочитанный кадр (1,9 с при обрыве на 2,0 с); кадр допуска
/// покрывает округление часов на границе. Без доигрывания стенд встаёт на 1,7 с:
/// прочитанное впрок (звук ~0,2 с, очередь кадров) пропадает.
const SHOWN_FRAME_MARGIN: Duration = FRAME;

/// Допуск «прозвучал почти весь прочитанный звук».
///
/// С доигрыванием «звуковая карта» стенда играет до 1,997 с при прочитанных 2,007 с
/// (разница — один шаг стенда); без доигрывания — только до 1,75 с. 60 мс покрывают
/// шаг стенда с запасом и ловят потерю хвоста.
const AUDIO_MARGIN: Duration = Duration::from_millis(60);

/// Как именно ломается источник.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceBreak {
    /// Порча/обрыв данных: «isomp4: unexpected end of atom».
    CorruptedData,

    /// Пропала сеть после бюджета переподключения (метка `NetworkDown`).
    NetworkLost,
}

impl SourceBreak {
    fn error(self) -> anyhow::Error {
        match self {
            Self::CorruptedData => {
                anyhow::anyhow!("malformed stream: isomp4: unexpected end of atom")
            }
            Self::NetworkLost => anyhow::Error::new(io::Error::new(
                io::ErrorKind::NetworkDown,
                "connection lost after the reconnect budget",
            )),
        }
    }
}

/// Что увидел стенд со стороны демуксера.
#[derive(Debug, Default, Clone, Copy)]
struct DemuxObservation {
    /// Наибольший PTS отданного видеопакета.
    last_video_pts: Option<Duration>,
    /// Наибольший конец (PTS + длительность) отданного аудиопакета.
    last_audio_end: Option<Duration>,
    /// Сколько раз демуксер ответил ошибкой.
    failures: usize,
    /// Чтения, пришедшие уже после ошибки (умерший демуксер читать нельзя).
    reads_after_failure: usize,
}

/// Настоящий демуксер, который «обрывается» на [`BREAK_AT`].
struct BreaksMidFileDemuxer {
    inner: symphonia_demux::SymphoniaDemuxer,
    break_at: Duration,
    source_break: SourceBreak,
    failed: bool,
    observation: Arc<Mutex<DemuxObservation>>,
}

impl BreaksMidFileDemuxer {
    fn new(
        fixture_name: &str,
        break_at: Duration,
        source_break: SourceBreak,
    ) -> (Self, Arc<Mutex<DemuxObservation>>) {
        let observation = Arc::new(Mutex::new(DemuxObservation::default()));
        let demuxer = Self {
            inner: open_fixture(fixture_name),
            break_at,
            source_break,
            failed: false,
            observation: Arc::clone(&observation),
        };
        (demuxer, observation)
    }

    fn observe(&self) -> std::sync::MutexGuard<'_, DemuxObservation> {
        self.observation.lock().expect("observation mutex")
    }
}

impl Demuxer for BreaksMidFileDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        // Как у обрезанного файла: заголовок обещает полную длину.
        self.inner.duration()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.inner.seekability()
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self.failed {
            self.observe().reads_after_failure += 1;
            return Err(self.source_break.error());
        }
        let event = self.inner.next_event()?;
        if let DemuxReadEvent::Packet(packet) = &event {
            if packet.pts >= self.break_at {
                self.failed = true;
                self.observe().failures += 1;
                return Err(self.source_break.error());
            }
            let mut observation = self.observe();
            match packet.kind {
                TrackKind::Video => {
                    observation.last_video_pts = observation.last_video_pts.max(Some(packet.pts));
                }
                TrackKind::Audio => {
                    let end = packet.pts + packet.duration.unwrap_or_default();
                    observation.last_audio_end = observation.last_audio_end.max(Some(end));
                }
                _ => {}
            }
        }
        Ok(event)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        // Данные до обрыва целы: после перемотки назад их снова можно читать, а
        // перемотка в недостающую часть принимается, но первое же чтение падает.
        self.failed = timestamp >= self.break_at;
        self.inner.seek(timestamp)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.failed = request.timestamp >= self.break_at;
        self.inner.seek_with_request(request)
    }
}

/// Как идёт время стенда.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HarnessClock {
    /// Симулированное: tick-и опережают реальные часы (быстро, без перемоток).
    Simulated,

    /// Настоящее: между tick-ами стенд спит. Нужно для перемотки видео без звука —
    /// после seek-а часы без звука якорятся по настоящему `Instant::now()`, и
    /// симулированный сдвиг выглядел бы как прыжок вперёд.
    RealTime,
}

/// Стенд доигрывания: session, fake decoder, fake «звуковая карта», журнал событий.
struct PlayoutHarness {
    clock: HarnessClock,
    session: PlayerSession,
    video_decoder: SharedFakeVideoDecoderThread,
    output_factory: ScriptedAudioOutputFactoryHandle,
    observation: Arc<Mutex<DemuxObservation>>,
    decoded_packets: usize,
    acked_packets: usize,
    /// Сколько flush-ей decoder-а стенд уже учёл.
    seen_decoder_flushes: usize,
    /// Симулированное время, на которое tick-и опережают реальные часы (растёт, пока
    /// часы воспроизведения идут: якорь часов без звука берёт настоящий `Instant::now()`).
    simulated_offset: Duration,
    /// Сколько звука «звуковая карта» уже сыграла.
    audible: Duration,
    /// Показанные кадры по порядку (каждый — один раз) с номером tick-а.
    shown_frames: Vec<(usize, Duration)>,
    ticks: usize,
    events: Vec<PlayerEvent>,
    observed_states: Vec<PlaybackState>,
}

impl PlayoutHarness {
    fn new(fixture_name: &str, break_at: Duration, source_break: SourceBreak) -> Self {
        Self::with_clock(
            fixture_name,
            break_at,
            source_break,
            HarnessClock::Simulated,
        )
    }

    fn with_clock(
        fixture_name: &str,
        break_at: Duration,
        source_break: SourceBreak,
        clock: HarnessClock,
    ) -> Self {
        let (demuxer, observation) =
            BreaksMidFileDemuxer::new(fixture_name, break_at, source_break);
        // Уровень 0: «звуковая карта» сама сообщает, сколько записанного ещё не сыграно.
        let (output_factory, output_factory_handle) =
            ScriptedAudioOutputFactory::success(0.0, None);
        let mut session = PlayerSession::with_audio_factories(
            Arc::new(audio::decoder::ProductionAudioDecoderFactory::default()),
            output_factory,
        );
        let (_request_id, _media_instance_id) = install_media(
            &mut session,
            Box::new(demuxer),
            PlaybackIntent::StartPlaying,
        );
        let video_decoder = SharedFakeVideoDecoderThread::new();
        session.set_video_backend(StartedVideoBackend::from_decoder_thread(
            "demux-failure-playout-test-backend",
            video_decoder.clone(),
        ));
        let mut harness = Self {
            clock,
            session,
            video_decoder,
            output_factory: output_factory_handle,
            observation,
            decoded_packets: 0,
            acked_packets: 0,
            seen_decoder_flushes: 0,
            simulated_offset: Duration::ZERO,
            audible: Duration::ZERO,
            shown_frames: Vec::new(),
            ticks: 0,
            events: Vec::new(),
            observed_states: Vec::new(),
        };
        harness.collect_events();
        harness
    }

    /// Снимок наблюдений демуксера (копия: замок не держится между вызовами).
    fn observation(&self) -> DemuxObservation {
        *self.observation.lock().expect("observation mutex")
    }

    fn collect_events(&mut self) {
        for event in self.session.take_events() {
            if let PlayerEvent::PlaybackStateChanged(state) = event {
                self.observed_states.push(state);
            }
            self.events.push(event);
        }
    }

    /// Один tick: decoder отдаёт по кадру на каждый принятый пакет, «звуковая карта»
    /// играет записанное, session продвигает pipeline.
    fn tick(&mut self) {
        let sent_packets = self.video_decoder.sent_packets();
        // Flush (перемотка) выбрасывает всё, что decoder ещё не отдал: кадров старых
        // пакетов после него не бывает, как у настоящего decoder-а.
        let flush_count = self.video_decoder.flush_count();
        if flush_count != self.seen_decoder_flushes {
            self.seen_decoder_flushes = flush_count;
            self.decoded_packets = sent_packets.len();
            self.acked_packets = sent_packets.len();
        }
        let decode_until = sent_packets
            .len()
            .min(self.decoded_packets + DECODED_FRAMES_PER_TICK);
        for (packet_index, packet) in sent_packets
            .iter()
            .enumerate()
            .take(decode_until)
            .skip(self.decoded_packets)
        {
            let frame_handle = u64::try_from(packet_index).expect("small index") + 1;
            let mut frame = decoded_frame_for_tests(packet.pts, frame_handle);
            frame.generation = packet.generation;
            self.video_decoder.push_decoded_frame(frame);
        }
        self.decoded_packets = decode_until;
        self.video_decoder
            .add_completed_packet_count(decode_until - self.acked_packets);
        self.acked_packets = decode_until;

        self.play_sound_card();
        if self.clock == HarnessClock::RealTime {
            std::thread::sleep(TICK_STEP);
        }
        let tick_now = Instant::now() + self.simulated_offset;
        let config = PlayerTickConfig {
            max_demux_packets_per_tick: DEMUX_PACKETS_PER_TICK,
            ..PlayerTickConfig::default()
        };
        let _result = self
            .session
            .tick(PlayerTickContext::with_config(tick_now, config));
        if self.clock == HarnessClock::Simulated && self.playback_clock_runs() {
            self.simulated_offset += TICK_STEP;
        }
        if let Some(frame) = self.session.compose_snapshot().current_video_frame
            && self
                .shown_frames
                .last()
                .is_none_or(|(_, last_pts)| *last_pts != frame.pts)
        {
            self.shown_frames.push((self.ticks, frame.pts));
        }
        self.ticks += 1;
        self.collect_events();
    }

    /// «Звуковая карта»: пока поток запущен, играет записанное, но не больше него.
    fn play_sound_card(&mut self) {
        let Some(output) = self.output_factory.last_output_handle() else {
            return;
        };
        let written = self.written_audio();
        let stream_running =
            output.play_count.load(Ordering::Relaxed) > output.pause_count.load(Ordering::Relaxed);
        if stream_running {
            self.audible = (self.audible + TICK_STEP).min(written);
        }
        output.clock.set_output_timing(self.audible, self.audible);
        output.set_buffer_level_ms(written.saturating_sub(self.audible).as_secs_f64() * 1000.0);
    }

    /// Сколько звука записано в выход (по формату, с которым session создала выход).
    fn written_audio(&self) -> Duration {
        let Some(output) = self.output_factory.last_output_handle() else {
            return Duration::ZERO;
        };
        let Some(spec) = self.output_factory.created_specs().last().copied() else {
            return Duration::ZERO;
        };
        let channels = usize::try_from(spec.channels()).expect("channel count");
        let frames = output.written_samples().len() / channels.max(1);
        Duration::from_secs_f64(frames as f64 / f64::from(spec.sample_rate))
    }

    /// Идут ли часы воспроизведения (старт выполнен, пауза не нажата).
    fn playback_clock_runs(&self) -> bool {
        match self.session.playback_state() {
            PlaybackState::Playing => true,
            PlaybackState::Draining => !self.session.startup_autoplay_preroll_pending(),
            _ => false,
        }
    }

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

    fn play_until_terminal(&mut self) {
        self.tick_until("terminal state", |harness| {
            matches!(
                harness.session.playback_state(),
                PlaybackState::Ended | PlaybackState::Failed
            )
        });
    }

    fn command(&mut self, command: PlayerCommand) {
        self.session
            .dispatch_command(command)
            .expect("command accepted");
        self.collect_events();
    }

    fn fatal_errors(&self) -> Vec<PlayerError> {
        self.events
            .iter()
            .filter_map(|event| match event {
                PlayerEvent::FatalError(error) => Some(error.clone()),
                _ => None,
            })
            .collect()
    }

    fn last_shown_pts(&self) -> Duration {
        self.shown_frames
            .last()
            .map(|(_, pts)| *pts)
            .expect("at least one frame shown")
    }
}

/// Обрыв посреди файла со звуком: показаны кадры почти до последнего прочитанного,
/// звук прозвучал почти до конца прочитанного, затем `Failed` с исходной ошибкой ровно
/// один раз (не `Ended`), а умерший демуксер больше не читается.
#[test]
fn corrupted_source_mid_file_plays_out_read_ahead_then_fails_once() {
    let mut harness =
        PlayoutHarness::new(AUDIO_VIDEO_FIXTURE, BREAK_AT, SourceBreak::CorruptedData);

    harness.play_until_terminal();
    assert_eq!(harness.session.playback_state(), PlaybackState::Failed);
    assert!(!harness.observed_states.contains(&PlaybackState::Ended));
    let draining_index = harness
        .observed_states
        .iter()
        .position(|state| *state == PlaybackState::Draining)
        .expect("the read-ahead is played out in Draining");
    assert_eq!(
        harness.observed_states.last(),
        Some(&PlaybackState::Failed),
        "states: {:?}",
        harness.observed_states
    );
    assert!(draining_index < harness.observed_states.len() - 1);

    let fatal_errors = harness.fatal_errors();
    assert_eq!(
        fatal_errors.len(),
        1,
        "exactly one fatal error: {fatal_errors:?}"
    );
    assert_eq!(fatal_errors[0].kind, PlayerErrorKind::DemuxError);
    assert!(fatal_errors[0].message.contains("unexpected end of atom"));
    assert_eq!(
        harness
            .session
            .snapshot
            .last_error
            .as_ref()
            .map(|error| error.kind.clone()),
        Some(PlayerErrorKind::DemuxError)
    );

    let (last_read_video, last_read_audio_end, failures, reads_after_failure) = {
        let observation = harness.observation();
        (
            observation.last_video_pts.expect("video was read"),
            observation.last_audio_end.expect("audio was read"),
            observation.failures,
            observation.reads_after_failure,
        )
    };
    assert_eq!(failures, 1);
    assert_eq!(
        reads_after_failure, 0,
        "a dead demuxer must not be read again"
    );
    let last_shown = harness.last_shown_pts();
    assert!(
        last_shown + SHOWN_FRAME_MARGIN >= last_read_video,
        "shown up to {last_shown:?}, read up to {last_read_video:?}"
    );
    assert!(last_shown < BREAK_AT, "nothing past the break can be shown");
    let written_audio = harness.written_audio();
    assert!(
        written_audio + AUDIO_MARGIN >= last_read_audio_end,
        "audio written up to {written_audio:?}, read up to {last_read_audio_end:?}"
    );
    assert!(
        harness.audible + AUDIO_MARGIN >= last_read_audio_end,
        "audio played up to {:?}, read up to {last_read_audio_end:?}",
        harness.audible
    );
    // Позиция осталась там, где кончились данные, а не прыгнула к длительности файла.
    let position = harness
        .session
        .snapshot
        .timeline
        .current_position
        .as_duration();
    assert!(position < BREAK_AT + AUDIO_MARGIN, "position {position:?}");
}

/// Пропала сеть после бюджета переподключения: то же доигрывание, причина остаётся
/// сетевой (`NetworkError`) — по ней приложение останавливает очередь.
#[test]
fn network_loss_mid_file_plays_out_then_reports_network_error() {
    let mut harness = PlayoutHarness::new(AUDIO_VIDEO_FIXTURE, BREAK_AT, SourceBreak::NetworkLost);

    harness.play_until_terminal();

    assert_eq!(harness.session.playback_state(), PlaybackState::Failed);
    assert!(harness.observed_states.contains(&PlaybackState::Draining));
    let fatal_errors = harness.fatal_errors();
    assert_eq!(fatal_errors.len(), 1, "{fatal_errors:?}");
    assert_eq!(fatal_errors[0].kind, PlayerErrorKind::NetworkError);
    let last_read_video = harness
        .observation()
        .last_video_pts
        .expect("video was read");
    assert!(harness.last_shown_pts() + SHOWN_FRAME_MARGIN >= last_read_video);
}

/// Источник отказал на самом первом чтении: доигрывать нечего — `Failed` сразу, в том
/// же tick-е, без `Draining`, как до п. 24.
#[test]
fn failure_on_the_first_read_fails_immediately_without_draining() {
    let mut harness = PlayoutHarness::new(
        AUDIO_VIDEO_FIXTURE,
        Duration::ZERO,
        SourceBreak::CorruptedData,
    );

    harness.tick_until("source failure observed", |harness| {
        harness.observation().failures > 0
    });

    assert_eq!(harness.session.playback_state(), PlaybackState::Failed);
    assert!(!harness.observed_states.contains(&PlaybackState::Draining));
    assert!(harness.shown_frames.is_empty());
    assert_eq!(harness.fatal_errors().len(), 1);
    assert_eq!(harness.fatal_errors()[0].kind, PlayerErrorKind::DemuxError);
}

/// Пауза посреди доигрывания: ошибка не публикуется, пока стоим; Play продолжает хвост,
/// не читая умерший демуксер, и только в конце приходит одна ошибка.
#[test]
fn pause_during_playout_keeps_error_deferred_and_play_resumes_the_tail() {
    let mut harness = PlayoutHarness::new(VIDEO_ONLY_FIXTURE, BREAK_AT, SourceBreak::CorruptedData);
    harness.tick_until("play-out started", |harness| {
        harness.session.playback_state() == PlaybackState::Draining
    });

    harness.command(PlayerCommand::Pause);
    assert_eq!(harness.session.playback_state(), PlaybackState::Paused);
    let shown_before_pause = harness.shown_frames.len();
    for _ in 0..20 {
        harness.tick();
    }
    assert_eq!(harness.session.playback_state(), PlaybackState::Paused);
    assert!(harness.fatal_errors().is_empty(), "no error while paused");
    assert_eq!(harness.shown_frames.len(), shown_before_pause);

    harness.command(PlayerCommand::Play);
    harness.play_until_terminal();

    assert_eq!(harness.session.playback_state(), PlaybackState::Failed);
    assert_eq!(harness.fatal_errors().len(), 1);
    assert_eq!(harness.observation().reads_after_failure, 0);
    let last_read_video = harness
        .observation()
        .last_video_pts
        .expect("video was read");
    assert!(harness.last_shown_pts() + SHOWN_FRAME_MARGIN >= last_read_video);
}

/// Перемотка назад во время доигрывания: старый хвост не возвращается, воспроизведение
/// идёт от цели, а ошибка приходит один раз — когда файл снова дошёл до обрыва.
#[test]
fn seek_back_during_playout_plays_from_target_and_fails_once_at_the_break_again() {
    let mut harness = PlayoutHarness::with_clock(
        VIDEO_ONLY_FIXTURE,
        BREAK_AT,
        SourceBreak::CorruptedData,
        HarnessClock::RealTime,
    );
    harness.tick_until("play-out started", |harness| {
        harness.session.playback_state() == PlaybackState::Draining
    });
    let seek_target = Duration::from_millis(500);
    let seek_tick = harness.ticks;

    harness.command(PlayerCommand::Seek(SeekRequest::accurate(seek_target)));
    harness.play_until_terminal();

    assert_eq!(harness.session.playback_state(), PlaybackState::Failed);
    let fatal_errors = harness.fatal_errors();
    assert_eq!(fatal_errors.len(), 1, "{fatal_errors:?}");
    assert_eq!(
        harness.observation().failures,
        2,
        "the break was reached twice"
    );
    let after_seek: Vec<Duration> = harness
        .shown_frames
        .iter()
        .filter(|(tick, _)| *tick >= seek_tick)
        .map(|(_, pts)| *pts)
        .collect();
    let first_after_seek = *after_seek.first().expect("frames after the seek");
    assert!(
        first_after_seek >= seek_target && first_after_seek < seek_target + 2 * FRAME,
        "the first frame after the seek is near the target, not the old tail: {after_seek:?}"
    );
    assert!(
        after_seek.windows(2).all(|pair| pair[0] < pair[1]),
        "{after_seek:?}"
    );
    let last_read_video = harness
        .observation()
        .last_video_pts
        .expect("video was read");
    assert!(*after_seek.last().expect("frames") + SHOWN_FRAME_MARGIN >= last_read_video);
}

/// Stop во время доигрывания — штатная остановка: ошибки нет ни сейчас, ни потом.
#[test]
fn stop_during_playout_discards_the_deferred_error() {
    let mut harness = PlayoutHarness::new(VIDEO_ONLY_FIXTURE, BREAK_AT, SourceBreak::CorruptedData);
    harness.tick_until("play-out started", |harness| {
        harness.session.playback_state() == PlaybackState::Draining
    });

    harness.command(PlayerCommand::Stop);
    for _ in 0..40 {
        harness.tick();
    }

    assert_eq!(harness.session.playback_state(), PlaybackState::Stopped);
    assert!(harness.fatal_errors().is_empty());
    assert!(harness.session.snapshot.last_error.is_none());
}

/// Перемотка за точку обрыва: после перемотки из демуксера ничего не дошло — `Failed`
/// сразу, без доигрывания старого хвоста и без ожидания посадки, как до п. 24.
#[test]
fn failure_while_seeking_past_the_break_fails_immediately() {
    let mut harness =
        PlayoutHarness::new(AUDIO_VIDEO_FIXTURE, BREAK_AT, SourceBreak::CorruptedData);
    harness.tick_until("playing", |harness| {
        harness.session.playback_state() == PlaybackState::Playing
    });

    harness.command(PlayerCommand::Seek(SeekRequest::accurate(
        Duration::from_millis(2_500),
    )));
    let states_before_failure = harness.observed_states.len();
    harness.play_until_terminal();

    assert_eq!(harness.session.playback_state(), PlaybackState::Failed);
    assert!(
        !harness.observed_states[states_before_failure..].contains(&PlaybackState::Draining),
        "{:?}",
        harness.observed_states
    );
    assert_eq!(harness.fatal_errors().len(), 1);
}

/// Перемотка чуть раньше обрыва: демуксер доходит до обрыва раньше, чем seek успевает
/// сесть. Ошибка — тот же «конец данных», что и EOF: seek садится на прочитанном,
/// остаток до обрыва играется, затем одна ошибка.
#[test]
fn seek_just_before_the_break_lands_and_plays_the_rest_before_failing() {
    let mut harness = PlayoutHarness::with_clock(
        VIDEO_ONLY_FIXTURE,
        BREAK_AT,
        SourceBreak::CorruptedData,
        HarnessClock::RealTime,
    );
    harness.tick_until("play-out started", |harness| {
        harness.session.playback_state() == PlaybackState::Draining
    });
    let seek_target = Duration::from_millis(1_700);
    let seek_tick = harness.ticks;

    harness.command(PlayerCommand::Seek(SeekRequest::accurate(seek_target)));
    harness.play_until_terminal();

    assert_eq!(harness.session.playback_state(), PlaybackState::Failed);
    assert_eq!(harness.fatal_errors().len(), 1);
    let after_seek: Vec<Duration> = harness
        .shown_frames
        .iter()
        .filter(|(tick, _)| *tick >= seek_tick)
        .map(|(_, pts)| *pts)
        .collect();
    let first_after_seek = *after_seek.first().expect("the seek landed on a frame");
    assert!(
        first_after_seek >= seek_target && first_after_seek < BREAK_AT,
        "{after_seek:?}"
    );
    let last_read_video = harness
        .observation()
        .last_video_pts
        .expect("video was read");
    assert!(*after_seek.last().expect("frames") + SHOWN_FRAME_MARGIN >= last_read_video);
}
