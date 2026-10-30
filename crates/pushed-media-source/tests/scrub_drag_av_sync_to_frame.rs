//! Drag по таймлайну и отпускание мыши — до кадра и до звука в выходе (дефект D2).
//!
//! Живая приёмка (MP4): после drag-а вперёд и отпускания на кадре звук начинался с
//! дырой или раньше картинки на 0,3–2,6 с и так оставался до следующей перемотки.
//! Причина — forward extension живого прохода: цель уезжает вперёд без нового demux
//! seek-а, output очищается, и уже декодированный звук новой цели выбрасывался.
//!
//! Проверяется то, что услышит зритель: PCM, записанный в выход после последней
//! очистки перемоткой, звучит от позиции commit-а (= кадра, на котором отпустили мышь).
//! Звук фикстуры — непрерывный синус на весь ролик, поэтому:
//! - тишина в начале отрезка — дыра (звук цели потерян);
//! - отрезок короче «конец ролика − цель» — звук начался с более позднего места
//!   файла (звучит раньше картинки).
//!
//! Настоящие декодеры видео (программный FFmpeg) и звука, выход — симулятор
//! (`support/simulated_audio.rs`). Обвязка `support/playback.rs` умеет только
//! seek/play, поэтому шаги плеера и команды скраба здесь свои, по тем же правилам.
//! Фикстуры: `video-h264-64-4s.mp4` (4 с, 25 к/с, ключевой кадр каждые 0,4 с) и
//! `audio-aac-4s.mp4` (синус 440 Гц, 4 с).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use player_core::{
    PlaybackState, PlayerCommand, PlayerEvent, PlayerSession, PlayerTickContext, PreparedMedia,
    ScrubCommitPolicy, SeekRequest,
};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamDemuxer, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{AUDIO_ADVANCE_PER_TICK_CAP, TICK, open};
use support::simulated_audio::SimulatedAudioDevice;

/// Длительность кадра фикстуры (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Длительность роликов.
const CLIP: Duration = Duration::from_secs(4);
/// Фрагментов в фикстурах.
const FRAGMENT_COUNT: usize = 10;
/// Порог «слышно»: синус фикстуры громче 0,5, тишина — ровно ноль.
const AUDIBLE: f32 = 0.05;
/// Допуск A/V «до кадра»: первый звук после отпускания — в пределах кадра.
const AV_TOLERANCE: Duration = FRAME;
/// Допуск длины звука после отпускания: AAC-кадр (~21 мс) с запасом на хвост.
const AUDIO_LENGTH_TOLERANCE: Duration = Duration::from_millis(100);
/// Наибольшая допустимая дыра в звуке после отпускания.
const MAX_AUDIO_HOLE: Duration = Duration::from_millis(50);
/// Сколько мышь стоит на первом кадре drag-а: за это время проход читает дальше цели.
const DRAG_PAUSE: Duration = Duration::from_millis(150);
/// Предел шагов одной фазы сценария (~5 с настоящего времени).
const MAX_PHASE_TICKS: usize = 1_000;
/// Полоса видео потока.
const VIDEO: LaneId = LaneId::new(0);
/// Полоса звука потока.
const AUDIO: LaneId = LaneId::new(1);

/// Потоков программного декодера (как в `support/playback.rs`).
const DECODER_THREADS: std::num::NonZeroUsize = std::num::NonZeroUsize::new(2).expect("non-zero");

/// Весь ролик «видео + звук», как его отдала бы страница.
fn whole_clip_stream() -> PushedStreamDemuxer {
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
        .append(VIDEO, &video.stream_of(0..FRAGMENT_COUNT))
        .expect("video bytes");
    writer
        .append(AUDIO, &audio.stream_of(0..FRAGMENT_COUNT))
        .expect("audio bytes");
    writer.end_of_stream().expect("end of stream");
    open(&mut opener)
}

/// Возможности приложения с программным декодом (как в `support/playback.rs`).
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

/// Безоконный плеер: сессия, «звуковая карта» и шаги по настоящему времени.
struct Player {
    /// Сессия с настоящими декодерами.
    session: PlayerSession,
    /// Симулятор выхода звука.
    device: SimulatedAudioDevice,
    /// Время прошлого шага: «карта» играет прошедшее время.
    previous_tick: Instant,
    /// Ошибки плеера за прогон.
    errors: Vec<String>,
}

impl Player {
    /// Открывает весь ролик с автозапуском.
    fn start() -> Self {
        let device = SimulatedAudioDevice::default();
        let mut session = PlayerSession::with_audio_factories(
            Arc::new(audio::ProductionAudioDecoderFactory::default()),
            device.factory(),
        );
        session.set_system_capabilities(software_only_capabilities());
        let prepared =
            PreparedMedia::from_external_label("scrub drag test", Box::new(whole_clip_stream()));
        session.load_prepared_media_with_autoplay(prepared, true);
        let mut player = Self {
            session,
            device,
            previous_tick: Instant::now(),
            errors: Vec::new(),
        };
        player.handle_events();
        player
    }

    /// Один шаг плеера: «карта» играет прошедшее время, сессия делает tick.
    fn step(&mut self) {
        thread::sleep(TICK);
        let now = Instant::now();
        let elapsed = now.duration_since(self.previous_tick);
        self.device.advance(elapsed.min(AUDIO_ADVANCE_PER_TICK_CAP));
        self.previous_tick = now;
        let _ = self.session.tick(PlayerTickContext::new(now));
        self.handle_events();
    }

    /// Шагает, пока не выполнится условие; падает, если фаза не закончилась.
    fn step_until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        for _ in 0..MAX_PHASE_TICKS {
            if done(self) {
                return;
            }
            self.step();
        }
        panic!(
            "{what} did not happen; state {:?}, errors {:?}",
            self.session.playback_state(),
            self.errors
        );
    }

    /// Как приложение: ставит декодер видео по запросу и собирает ошибки.
    fn handle_events(&mut self) {
        for event in self.session.take_events() {
            match event {
                PlayerEvent::VideoBackendSelectionRequested(_) => {
                    self.session
                        .set_video_backend(start_software_video_decoder());
                }
                PlayerEvent::RecoverableError(error) | PlayerEvent::FatalError(error) => {
                    self.errors.push(format!("{error:?}"));
                }
                _ => {}
            }
        }
    }

    /// Время кадра на экране.
    fn shown_frame(&self) -> Option<Duration> {
        self.session
            .compose_snapshot()
            .current_video_frame
            .map(|frame| frame.pts)
    }

    /// Позиция плеера.
    fn position(&self) -> Duration {
        self.session.compose_snapshot().current_position
    }

    /// Команда скраба (как timeline приложения).
    fn command(&mut self, command: PlayerCommand) {
        self.session
            .dispatch_command(command)
            .expect("scrub command accepted");
    }

    /// Мышь стоит на месте: плеер шагает, команд нет.
    fn hold_mouse_still(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.step();
        }
    }

    /// Drag-preview в `target` и ожидание его кадра на экране.
    fn drag_to(&mut self, target: Duration) {
        self.command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            media_core::MediaTime::from_duration(target),
        )));
        self.step_until("drag preview frame", |player| {
            player.shown_frame() == Some(target)
        });
    }
}

/// PCM, записанный после последней очистки выхода перемоткой: звучит от цели commit-а.
struct AudioAfterRelease {
    /// Interleaved PCM отрезка.
    samples: Vec<f32>,
    /// Частота выхода.
    sample_rate: u32,
    /// Каналов в выходе.
    channels: usize,
}

impl AudioAfterRelease {
    /// Отрезок звука после последней очистки выхода.
    fn of(device: &SimulatedAudioDevice) -> Self {
        let (sample_rate, channels) = device.format().expect("audio output was created");
        let first_frame = device
            .written_frames_at_seek_clears()
            .last()
            .copied()
            .expect("the scrub cleared the output");
        let samples = device.written_samples()[first_frame * channels..].to_vec();
        Self {
            samples,
            sample_rate,
            channels,
        }
    }

    /// Время PCM-кадра от начала отрезка (= от позиции commit-а).
    fn time_of_frame(&self, frame: usize) -> Duration {
        Duration::from_secs_f64(frame as f64 / f64::from(self.sample_rate))
    }

    /// Слышен ли PCM-кадр.
    fn audible(frame: &[f32]) -> bool {
        frame.iter().any(|sample| sample.abs() > AUDIBLE)
    }

    /// Время первого слышимого PCM-кадра от начала отрезка.
    fn first_audible(&self) -> Option<Duration> {
        self.samples
            .chunks_exact(self.channels)
            .position(Self::audible)
            .map(|frame| self.time_of_frame(frame))
    }

    /// Длительность слышимого звука отрезка (от первого до последнего слышимого кадра).
    fn audible_span(&self) -> Duration {
        let frames: Vec<&[f32]> = self.samples.chunks_exact(self.channels).collect();
        let first = frames.iter().position(|frame| Self::audible(frame));
        let last = frames.iter().rposition(|frame| Self::audible(frame));
        match (first, last) {
            (Some(first), Some(last)) => self.time_of_frame(last + 1 - first),
            _ => Duration::ZERO,
        }
    }

    /// Самая длинная тишина внутри слышимого звука (дыра посреди отрезка).
    fn longest_inner_silence(&self) -> Duration {
        let mut longest = 0;
        let mut current = 0;
        let mut heard_any = false;
        for frame in self.samples.chunks_exact(self.channels) {
            if Self::audible(frame) {
                heard_any = true;
                longest = longest.max(current);
                current = 0;
            } else if heard_any {
                current += 1;
            }
        }
        self.time_of_frame(longest)
    }
}

/// Drag вперёд в два preview (второй — продолжение прохода без перемотки demux-а),
/// отпускание на кадре: звук после отпускания — от кадра, без дыры и не раньше него.
#[test]
fn audio_after_drag_release_starts_at_the_released_frame() {
    let mut player = Player::start();
    // Ролик прочитан целиком сразу, поэтому игра идёт как `Draining` (доигрывание).
    player.step_until("playback start", |player| {
        matches!(
            player.session.playback_state(),
            PlaybackState::Playing | PlaybackState::Draining
        ) && player.position() >= Duration::from_millis(600)
    });

    // Первый preview между ключевыми кадрами (2,0 и 2,4 с): проход с 2,0 с.
    let first_preview = Duration::from_millis(2_120);
    // Второй — на два кадра дальше: меньше декодированного запаса звука (200 мс),
    // поэтому продолжение прохода раньше выбрасывало звук этого места.
    let released_frame = first_preview + FRAME * 2;
    player.command(PlayerCommand::begin_scrub());
    player.drag_to(first_preview);
    // Мышь чуть задержалась на кадре: проход успевает прочитать дальше цели.
    player.hold_mouse_still(DRAG_PAUSE);
    player.drag_to(released_frame);
    let output_clears_before_release = player.device.written_frames_at_seek_clears().len();
    player.command(PlayerCommand::end_scrub(
        ScrubCommitPolicy::CommitVisiblePreview,
    ));
    // Весь ролик уже прочитан, поэтому после commit-а плеер доигрывает его (Draining).
    player.step_until("release commit and playback to the end", |player| {
        player.session.playback_state() == PlaybackState::Ended
    });

    assert!(
        player.errors.is_empty(),
        "player errors: {:?}",
        player.errors
    );
    // Отпускание на кадре прохода переиспользует проход (без новой перемотки и очистки
    // выхода): ровно тот путь, где звук терялся.
    assert_eq!(
        player.device.written_frames_at_seek_clears().len(),
        output_clears_before_release,
        "release on the drag frame must reuse the live pass"
    );
    let audio = AudioAfterRelease::of(&player.device);
    let first_audible = audio
        .first_audible()
        .expect("the tone must be heard after release");
    assert!(
        first_audible <= AV_TOLERANCE,
        "audio after release starts {first_audible:?} after the released frame (hole)"
    );
    // Звук от кадра до конца ролика целиком: короче — значит, звучит более позднее
    // место файла, т. е. звук раньше картинки.
    let expected_span = CLIP - released_frame;
    assert!(
        audio.audible_span().abs_diff(expected_span) <= AUDIO_LENGTH_TOLERANCE,
        "tone after release lasts {:?}, expected {expected_span:?} (shorter = audio ahead of video)",
        audio.audible_span()
    );
    assert!(
        audio.longest_inner_silence() <= MAX_AUDIO_HOLE,
        "hole of {:?} inside the audio after release",
        audio.longest_inner_silence()
    );
}
