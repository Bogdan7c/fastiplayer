//! Общий стенд тестов переключения аудиодорожки: настоящий MKV + настоящий демуксер и
//! декодеры, fake output, который запоминает записанные сэмплы.

use super::super::*;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, TrackInfo};

use super::super::test_support::{
    ScriptedAudioOutputFactory, ScriptedAudioOutputFactoryHandle, ScriptedAudioOutputHandle,
    SharedFakeVideoDecoderThread,
};
use crate::{
    AuthorizeInstallCommit, MediaInstallCompletion, MediaInstallControl,
    MediaInstallControlOutcome, MediaInstallPhase, MediaInstallReceipt, MediaInstallRequestId,
    MediaInstanceId, PlaybackIntent, PlaybackIntentRevision,
};

/// Фиксированные параметры дорожек фикстуры (см. `fixtures/subtitles/README.md`).
pub(super) const FLAC_SAMPLE_RATE: u32 = 22_050;
pub(super) const FLAC_CHANNELS: usize = 2;
pub(super) const FLAC_FREQUENCY_HZ: f64 = 440.0;
pub(super) const OPUS_SAMPLE_RATE: u32 = 48_000;
pub(super) const OPUS_CHANNELS: usize = 1;
pub(super) const OPUS_FREQUENCY_HZ: f64 = 880.0;
/// Длина фикстуры, секунды.
pub(super) const FIXTURE_SECONDS: f64 = 3.0;

pub(super) fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../symphonia-demux/tests/fixtures/subtitles")
        .join(name)
}

pub(super) fn open_fixture(name: &str) -> symphonia_demux::SymphoniaDemuxer {
    symphonia_demux::SymphoniaDemuxer::from_file(&fixture_path(name))
        .expect("audio-track fixture must open")
}

/// Обёртка над настоящим демуксером: прячет видео (дорожку и пакеты), чтобы session
/// проходила audio-only путь без декодера кадров, и может объявить источник не seekable.
pub(super) struct AudioOnlyDemuxer {
    inner: symphonia_demux::SymphoniaDemuxer,
    tracks: Vec<TrackInfo>,
    seekability: DemuxSeekability,
}

impl AudioOnlyDemuxer {
    pub(super) fn new(inner: symphonia_demux::SymphoniaDemuxer) -> Self {
        let tracks = inner
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .cloned()
            .collect();
        Self {
            inner,
            tracks,
            seekability: DemuxSeekability::Seekable,
        }
    }

    pub(super) fn not_seekable(mut self) -> Self {
        self.seekability = DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::UnknownTimeline,
        };
        self
    }
}

impl Demuxer for AudioOnlyDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        loop {
            match self.inner.next_event()? {
                DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {}
                event => return Ok(event),
            }
        }
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek(timestamp)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek_with_request(request)
    }
}

/// Production-декодеры с журналом: какие кодеки session реально запросила.
pub(super) struct RecordingDecoderFactory {
    inner: audio::decoder::ProductionAudioDecoderFactory,
    requested_codecs: Mutex<Vec<String>>,
}

impl RecordingDecoderFactory {
    /// Сколько decoder-ов session реально запросила у factory.
    pub(super) fn requested_decoder_count(&self) -> usize {
        self.requested_codecs.lock().expect("codec log lock").len()
    }
}

impl audio_core::AudioDecoderFactory for RecordingDecoderFactory {
    fn create_decoder(
        &self,
        config: audio_core::AudioDecoderConfig,
    ) -> anyhow::Result<audio_core::AudioDecoderHandle> {
        self.requested_codecs
            .lock()
            .expect("codec log")
            .push(config.codec_id().to_string());
        self.inner.create_decoder(config)
    }
}

pub(super) struct SwitchHarness {
    pub(super) session: PlayerSession,
    pub(super) output_factory: ScriptedAudioOutputFactoryHandle,
    pub(super) decoder_factory: Arc<RecordingDecoderFactory>,
    pub(super) media_instance_id: MediaInstanceId,
    pub(super) request_id: MediaInstallRequestId,
    pub(super) audio_track_ids: Vec<TrackId>,
}

impl SwitchHarness {
    /// Audio-only media (видео спрятано), старт по `intent`.
    pub(super) fn audio_only(intent: PlaybackIntent) -> Self {
        Self::with_demuxer("two-audio-tracks.mkv", intent, |inner| {
            Box::new(AudioOnlyDemuxer::new(inner))
        })
    }

    /// Audio-only media, но источник объявлен не seekable (live-подобный).
    pub(super) fn audio_only_not_seekable(intent: PlaybackIntent) -> Self {
        Self::with_demuxer("two-audio-tracks.mkv", intent, |inner| {
            Box::new(AudioOnlyDemuxer::new(inner).not_seekable())
        })
    }

    /// Полный файл (с видео) и fake video backend: для проверок валидации без воспроизведения.
    pub(super) fn with_video(fixture_name: &str) -> (Self, SharedFakeVideoDecoderThread) {
        let mut harness = Self::with_demuxer(fixture_name, PlaybackIntent::StartPaused, |inner| {
            Box::new(inner)
        });
        let decoder = SharedFakeVideoDecoderThread::new();
        harness
            .session
            .set_video_backend(StartedVideoBackend::from_decoder_thread(
                "audio-track-switch-test-backend",
                decoder.clone(),
            ));
        (harness, decoder)
    }

    pub(super) fn with_demuxer(
        fixture_name: &str,
        intent: PlaybackIntent,
        wrap: impl FnOnce(symphonia_demux::SymphoniaDemuxer) -> Box<dyn Demuxer + Send>,
    ) -> Self {
        let inner = open_fixture(fixture_name);
        let demuxer = wrap(inner);
        let audio_track_ids = demuxer
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id)
            .collect();
        let decoder_factory = Arc::new(RecordingDecoderFactory {
            inner: audio::decoder::ProductionAudioDecoderFactory::default(),
            requested_codecs: Mutex::new(Vec::new()),
        });
        // Буфер 50 мс: выше порога готовности preroll, ниже high-water — пакеты не тормозятся.
        let (output_factory, output_factory_handle) =
            ScriptedAudioOutputFactory::success(50.0, None);
        let mut session = PlayerSession::with_audio_factories(
            Arc::clone(&decoder_factory) as Arc<dyn audio_core::AudioDecoderFactory>,
            output_factory,
        );
        let (request_id, media_instance_id) = install_media(&mut session, demuxer, intent);
        Self {
            session,
            output_factory: output_factory_handle,
            decoder_factory,
            media_instance_id,
            request_id,
            audio_track_ids,
        }
    }

    /// Один tick с маленьким demux-окном: media читается постепенно, как при реальной игре.
    pub(super) fn tick(&mut self) {
        let config = PlayerTickConfig {
            max_demux_packets_per_tick: 6,
            ..PlayerTickConfig::default()
        };
        let _result = self
            .session
            .tick(PlayerTickContext::with_config(Instant::now(), config));
    }

    pub(super) fn ticks(&mut self, count: usize) {
        for _ in 0..count {
            self.tick();
        }
    }

    /// Тикает, пока условие не выполнится (или падает, если оно недостижимо).
    pub(super) fn tick_until(&mut self, what: &str, mut condition: impl FnMut(&Self) -> bool) {
        for _ in 0..400 {
            if condition(self) {
                return;
            }
            self.tick();
        }
        panic!("condition never reached: {what}");
    }

    pub(super) fn latest_output(&self) -> ScriptedAudioOutputHandle {
        self.output_factory
            .last_output_handle()
            .expect("audio output must exist")
    }

    /// Имитирует звуковую карту: output «проиграл» `audible` своего времени.
    pub(super) fn play_out_latest(&self, audible: Duration) {
        let output = self.latest_output();
        output.clock.set_output_timing(audible, audible);
    }

    pub(super) fn selected_audio(&self) -> Option<TrackId> {
        self.session.snapshot.selected_tracks.audio_track
    }

    pub(super) fn position(&self) -> Duration {
        self.session
            .snapshot
            .timeline
            .current_position
            .as_duration()
    }
}

/// Ставит media настоящим staged-протоколом без video backend (audio-only).
pub(super) fn install_media(
    session: &mut PlayerSession,
    demuxer: Box<dyn Demuxer + Send>,
    intent: PlaybackIntent,
) -> (MediaInstallRequestId, MediaInstanceId) {
    let request_id =
        MediaInstallRequestId::from_non_zero(NonZeroU64::new(8_001).expect("non-zero id"));
    let (install_receipt, install_port) = MediaInstallReceipt::new(request_id);
    session.stage_prepared_media_install_compatibility(
        request_id,
        PreparedMedia::from_external_label("audio-track-switch", demuxer),
        intent,
        PlaybackIntentRevision::INITIAL,
        install_port,
    );
    assert_eq!(
        install_receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id })
    );
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = install_receipt
        .try_take_completion()
        .expect("authorized install completes")
    else {
        panic!("install must succeed");
    };
    (request_id, media_instance_id)
}

/// Частота сигнала по положительным переходам через ноль первого канала.
pub(super) fn estimate_frequency_hz(samples: &[f32], channels: usize, sample_rate: u32) -> f64 {
    let first_channel: Vec<f32> = samples.iter().step_by(channels).copied().collect();
    let rising_crossings = first_channel
        .windows(2)
        .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
        .count();
    rising_crossings as f64 / (first_channel.len() as f64 / f64::from(sample_rate))
}

pub(super) fn seconds_of(samples: &[f32], channels: usize, sample_rate: u32) -> f64 {
    (samples.len() / channels) as f64 / f64::from(sample_rate)
}

pub(super) fn seek_requested_count(events: &[PlayerEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, PlayerEvent::SeekRequested(_)))
        .count()
}

pub(super) fn audio_selected_events(events: &[PlayerEvent]) -> Vec<TrackId> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::AudioTrackSelected(id) => Some(*id),
            _ => None,
        })
        .collect()
}
