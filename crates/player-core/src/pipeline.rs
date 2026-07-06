use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::VideoDecodeRequirement;
#[cfg(test)]
use codec_core::video_frame_pixel_layout_from_decode_requirement;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, Demuxer, PacketKeyframe, TrackId, TrackInfo,
    TrackTimestamp,
};
use video_core::{
    HostUploadBackpressureReason, HostUploadResourceSnapshotStatus, VideoDecoderActivitySnapshot,
    VideoDecoderActivityUnavailableReason, VideoDecoderControlBackpressureReason,
};
use video_frame_contract::VideoFrameContract;
#[cfg(test)]
use video_frame_contract::{DmaBufImageLayout, VideoFramePixelLayout};

use crate::{
    AudioTempoDecodedMedia, AudioTempoPcmFormat, AudioTempoProcessReport,
    AudioTempoProcessorHandle, AudioTempoRatio, AudioTempoSegment, AudioTempoSegmentId,
    AudioTempoStretchedOutput, DecodeSendError, DecodeThreadError,
    DecoderControlChannelPressureSnapshot, DecoderResourceSnapshot, PlaybackRate, PlayerAudioClock,
    PlayerAudioOutput, PlayerDecodePacket, PlayerVideoDecoderThreadHandle,
    PresentFrameResourceProviderHandle, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoPrerollOutputFloor, VideoPrerollOutputFloorClear,
    VideoPrerollOutputFloorResult, VideoStreamConfigResult, VideoStreamDecodeConfig,
};
#[cfg(test)]
use video_core::VideoDecoderThreadHandle;
mod audio;
mod media_slots;
mod render_resources;
#[cfg(test)]
mod tests;
mod video_decoder;

/// Bootstrap-оценка длительности frame до первых PTS observations; не worker cadence.
pub(crate) const DEFAULT_VIDEO_FRAME_DURATION: Duration = Duration::from_micros(16_667);

/// Минимальная разумная длительность кадра для оценки FPS.
pub(crate) const MIN_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(5);

/// Максимальная разумная длительность кадра для оценки FPS.
pub(crate) const MAX_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(100);

/// Результат снятия render lease-а из accounting map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderLeaseReleaseEffect {
    /// Drop-ack пришёл для lease-а, которого pipeline уже не учитывает.
    UnknownLease,

    /// Lease count уменьшен, но другие clone-ы всё ещё держат texture handle.
    LeaseStillActive,

    /// Последний lease снят, deferred texture release для handle не был запрошен.
    ReleasedWithoutDeferredTexture,

    /// Последний lease снят, и ранее отложенный texture release можно выполнить.
    DeferredTextureReady,
}

/// Решение pipeline accounting при запросе release texture handle-а.
#[derive(Clone)]
pub(crate) enum VideoTextureReleaseEffect {
    /// Texture handle удерживается renderer-ом и должен быть освобождён после drop-ack.
    DeferredUntilRenderLeaseDrop,

    /// Render lease уже dropped, но frame успел побывать в renderer-е.
    ReleaseViaRenderProvider(PresentFrameResourceProviderHandle),

    /// Активных render leases нет, texture handle можно сразу вернуть decoder-у.
    ReleaseNow,
}

/// Typed причина, по которой tick не должен отправлять новый packet в decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoDecoderSendBackpressure {
    /// Decoder ещё не установлен в playback pipeline.
    AbsentDecoder,

    /// Software host-upload backend временно не может принять ещё один decoded host frame.
    HostUpload(HostUploadBackpressureReason),

    /// Control/release channel decoder-а заполнен и должен быть обработан перед новым input.
    DecoderControl(VideoDecoderControlBackpressureReason),
}

/// Status neutral decoder-activity boundary-а, который скрывает decoder handle storage.
#[derive(Debug, Clone)]
pub(crate) enum VideoDecoderActivityStatus {
    /// Decoder backend ещё не установлен, поэтому ждать activity не у кого.
    AbsentDecoder,

    /// Backend установлен, но пока не поддерживает neutral activity notifier.
    Unsupported,

    /// Backend сообщил typed unavailable state, который нельзя превращать в busy loop.
    ///
    /// Session 3 только планирует intent; Session 4 будет читать reason в worker wait policy.
    Unavailable(VideoDecoderActivityUnavailableReason),

    /// Snapshot доступен; worker сможет ждать activity через video-core contract.
    Available {
        /// Captured snapshot содержит epoch и subscription без backend-specific channels.
        snapshot: VideoDecoderActivitySnapshot,
    },
}

impl VideoDecoderActivityStatus {
    /// Проверяет, можно ли планировать event-driven ожидание decoder activity.
    #[must_use]
    pub(crate) fn can_wait_for_activity(&self) -> bool {
        match self {
            Self::Available { snapshot } => snapshot.captured_epoch().is_some(),
            Self::AbsentDecoder | Self::Unsupported | Self::Unavailable(_) => false,
        }
    }
}

/// Runtime-готовность выбранного audio path-а для session-level audio gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioSeekRuntimeState {
    /// Audio track не выбран: media video-only или audio path уже явно отключён policy-слоем.
    NoSelectedAudio,

    /// Track выбран, но decoder ещё не установлен или deferred config ждёт первого packet-а.
    WaitingForDecoder,

    /// Decoder уже есть, но output ещё не создан из первого decoded AudioSpec.
    WaitingForOutput,

    /// Decoder и output готовы; session policy может проверять buffer preroll.
    Ready,
}

/// Состояние audio tail-а после EOF без раскрытия очередей и concrete output-а.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AudioEofDrainState {
    /// Audio track не выбран, поэтому audio tail отсутствует.
    NoSelectedAudio,

    /// Есть encoded audio packets, которые session ещё должна декодировать/записать в output.
    PendingPackets {
        /// Количество packets в audio pending queue.
        queued_packets: usize,
    },

    /// Audio track выбран, но output так и не был создан до EOF.
    NoOutput,

    /// Output существует и его buffer ещё содержит слышимый tail.
    DrainingOutput {
        /// Текущий уровень output buffer-а в миллисекундах.
        buffer_level_ms: f64,

        /// Был ли уже успешно запрошен запуск stream-а.
        playback_requested: bool,
    },

    /// Output существует, но его buffer уже пуст.
    DrainedOutput {
        /// Был ли уже успешно запрошен запуск stream-а.
        playback_requested: bool,
    },
}

/// Успешный результат audio decode вместе с параметрами decoder-а для clock trimming.
#[derive(Debug)]
pub(crate) struct DecodedAudioPacket {
    /// Interleaved PCM samples, которые вернул codec-neutral decoder.
    pub(crate) samples: Vec<f32>,

    /// Sample rate decoded PCM на момент decode.
    pub(crate) sample_rate: u32,

    /// Количество interleaved audio channels на момент decode.
    pub(crate) channels: u32,
}

/// Anchor внутреннего monotonic media clock для media без audio clock.
#[derive(Debug, Clone, Copy)]
struct MonotonicMediaClockAnchor {
    /// Media position, которая соответствовала `anchored_at`.
    media_position: Duration,

    /// Монотонный момент, от которого считается fallback media time.
    anchored_at: Instant,

    /// Playback rate, с которым wall-time превращается в media-time.
    playback_rate: PlaybackRate,
}

impl MonotonicMediaClockAnchor {
    /// Создаёт anchor без привязки к video FPS или worker tick cadence.
    #[must_use]
    const fn new(
        media_position: Duration,
        anchored_at: Instant,
        playback_rate: PlaybackRate,
    ) -> Self {
        Self {
            media_position,
            anchored_at,
            playback_rate,
        }
    }

    /// Возвращает media position на заданный monotonic момент.
    #[must_use]
    fn position_at(self, now: Instant) -> Duration {
        let elapsed_wall_time = now.saturating_duration_since(self.anchored_at);
        let elapsed_media_time = self
            .playback_rate
            .scale_wall_delta_to_media_delta(elapsed_wall_time);

        self.media_position
            .checked_add(elapsed_media_time)
            .unwrap_or(Duration::MAX)
    }
}

/// Anchor перевода audio output clock progress в media progress.
#[derive(Debug, Clone, Copy)]
struct AudioClockMediaMappingAnchor {
    /// Media position, которая соответствовала `output_clock_position`.
    media_position: Duration,

    /// Значение `PlayerAudioClock::now()` в момент re-anchor.
    output_clock_position: Duration,

    /// Media-progress per output-clock-progress для текущего tempo segment-а.
    playback_rate: PlaybackRate,
}

impl AudioClockMediaMappingAnchor {
    /// Создаёт mapping anchor без доступа к concrete audio output/backend state.
    #[must_use]
    const fn new(
        media_position: Duration,
        output_clock_position: Duration,
        playback_rate: PlaybackRate,
    ) -> Self {
        Self {
            media_position,
            output_clock_position,
            playback_rate,
        }
    }

    /// Возвращает media position для текущего output clock.
    #[must_use]
    fn media_position_at_output_clock(self, output_clock_position: Duration) -> Duration {
        let output_delta = output_clock_position.saturating_sub(self.output_clock_position);
        let media_delta = self
            .playback_rate
            .scale_wall_delta_to_media_delta(output_delta);

        self.media_position
            .checked_add(media_delta)
            .unwrap_or(Duration::MAX)
    }
}

/// Сырой audio packet, который ждёт decode из-за backpressure audio buffer.
pub(crate) struct PendingAudioPacket {
    /// Track ID нужен, чтобы не отправить packet неактивного audio track в decoder.
    pub(crate) track_id: TrackId,

    /// Presentation timestamp packet-а на абсолютной media timeline.
    pub(crate) pts: Duration,

    /// Raw packet timing в container units для decoder boundary.
    pub(crate) timing: audio_core::AudioPacketTiming,

    /// Seek generation, в котором packet был прочитан из demuxer.
    pub(crate) generation: u64,

    /// Encoded audio bytes владеют shared payload-ом без копии между demuxer и player queue.
    pub(crate) encoded_bytes: Bytes,
}

impl PendingAudioPacket {
    /// Создаёт ожидающий audio packet с явным track id и codec bytes.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new(
        track_id: TrackId,
        pts: Duration,
        _dts: Option<Duration>,
        _duration: Option<Duration>,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
            timing: audio_core::AudioPacketTiming::unknown(),
            generation,
            encoded_bytes,
        }
    }

    /// Создаёт ожидающий audio packet с raw container timing для decoder-а.
    #[must_use]
    pub(crate) fn with_timing(
        track_id: TrackId,
        pts: Duration,
        _dts: Option<Duration>,
        _duration: Option<Duration>,
        timing: audio_core::AudioPacketTiming,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
            timing,
            generation,
            encoded_bytes,
        }
    }
}

/// Сырой video packet, который ждёт отправки в decode thread.
pub(crate) struct PendingVideoPacket {
    /// Track ID нужен для фильтрации выбранного video track.
    pub(crate) track_id: TrackId,

    /// Presentation timestamp определяет A/V sync и decode-ahead лимит.
    pub(crate) pts: Duration,

    /// Decode timestamp нужен codec backends с decode-order семантикой, например H.264 B-frames.
    pub(crate) dts: Option<Duration>,

    /// Raw DTS в track time base сохраняет container ordering metadata до decoder boundary.
    pub(crate) track_dts: Option<TrackTimestamp>,

    /// Seek generation, в котором packet был прочитан из demuxer.
    pub(crate) generation: u64,

    /// Encoded video bytes владеют shared payload-ом без копии до decoder thread.
    pub(crate) encoded_bytes: Bytes,

    /// Keyframe-классификация, полученная на demux boundary.
    pub(crate) keyframe: PacketKeyframe,
}

impl PendingVideoPacket {
    /// Создаёт ожидающий video packet без неименованных tuple-полей.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new(
        track_id: TrackId,
        pts: Duration,
        generation: u64,
        encoded_bytes: Bytes,
        keyframe: impl Into<PacketKeyframe>,
    ) -> Self {
        Self::new_with_decode_timestamps(
            track_id,
            pts,
            None,
            None,
            generation,
            encoded_bytes,
            keyframe,
        )
    }

    /// Создаёт ожидающий video packet с decode-order timestamp metadata.
    #[must_use]
    pub(crate) fn new_with_decode_timestamps(
        track_id: TrackId,
        pts: Duration,
        dts: Option<Duration>,
        track_dts: Option<TrackTimestamp>,
        generation: u64,
        encoded_bytes: Bytes,
        keyframe: impl Into<PacketKeyframe>,
    ) -> Self {
        Self {
            track_id,
            pts,
            dts,
            track_dts,
            generation,
            encoded_bytes,
            keyframe: keyframe.into(),
        }
    }
}

/// Внутреннее владение media pipeline для текущей player session.
///
/// Поля намеренно закрыты: `session`, `tick`, `worker` и render bridge обращаются
/// к pipeline через intent methods, которые сохраняют lifecycle, generation,
/// queue accounting и release-инварианты.
pub(crate) struct PlaybackPipeline {
    /// Demuxer текущего media source через нейтральный media-core contract.
    demuxer: Option<Box<dyn media_core::Demuxer + Send>>,

    /// Локальный путь, если media был открыт из файловой системы.
    file_path: Option<PathBuf>,

    /// Tracks текущего media без доступа UI к demuxer handle.
    tracks: Vec<TrackInfo>,

    /// User-facing label для streaming source без локального path.
    source_label: Option<String>,

    /// Codec-neutral audio decoder для выбранного audio трека.
    audio_decoder: Option<audio_core::AudioDecoderHandle>,

    /// Deferred config для audio decoder-а, пока первый packet не потребовал decode.
    deferred_audio_decoder_config: Option<audio_core::AudioDecoderConfig>,

    /// Audio output за нейтральным boundary trait-ом: production adapter или test fake.
    audio_output: Option<Box<dyn PlayerAudioOutput>>,

    /// Optional tempo processor для non-1x decoded PCM; `1.0x` остаётся passthrough.
    audio_tempo_processor: Option<AudioTempoProcessorHandle>,

    /// PCM format, под который создан текущий tempo processor.
    audio_tempo_pcm_format: Option<AudioTempoPcmFormat>,

    /// Следующий monotonic tempo segment id внутри текущего media pipeline.
    next_audio_tempo_segment_id: u64,

    /// Был ли текущий audio output успешно запущен через boundary `play`.
    audio_output_play_requested: bool,

    /// Track ID выбранного audio трека.
    audio_track_id: Option<TrackId>,

    /// Очередь сырых audio packets для throttle.
    pending_audio_packets: VecDeque<PendingAudioPacket>,

    /// Video decoder thread: backend decode в отдельном потоке за узким session contract.
    video_decoder_thread: Option<Box<PlayerVideoDecoderThreadHandle>>,

    /// Требует ли decoder следующий video packet быть keyframe-ом.
    ///
    /// Stateless hardware decode после flush теряет reference frames. Если сразу
    /// отправить inter-frame, decoder может показать зелёные артефакты до
    /// следующего keyframe. Флаг держит этот codec contract рядом с очередью
    /// packets, а не в UI/render слое.
    video_decoder_needs_keyframe: bool,

    /// Сколько video packets уже ушло в decoder thread и ещё не получило packet ack.
    ///
    /// `VideoDecodeThread::packet_queue_depth()` не видит packet, который decoder
    /// уже забрал из channel и прямо сейчас обрабатывает. Decoder ack приходит
    /// по отдельному channel независимо от того, дал packet output frame или нет.
    video_decode_in_flight_packets: usize,

    /// Очередь декодированных видеокадров перед presentation.
    video_frame_queue: VecDeque<video_core::DecodedFrame>,

    /// Последний свежедекодированный frame до final seek target для EOF fallback.
    ///
    /// При seek в самый конец файла requested target может оказаться позже
    /// последнего реального video PTS. Такой frame нельзя показывать сразу как
    /// точный target, но его нужно сохранить до EOF, чтобы не зависнуть в seek.
    seek_preroll_fallback_video_frame: Option<video_core::DecodedFrame>,

    /// Текущая оценка длительности одного video frame.
    video_frame_duration_estimate: Duration,

    /// PTS последнего decoded frame для обновления оценки frame duration.
    last_decoded_video_pts: Option<Duration>,

    /// Текущий кадр для отображения, выбранный scheduler.
    present_video_frame: Option<video_core::DecodedFrame>,

    /// Поколение render resources текущего media pipeline.
    render_generation: u64,

    /// Texture handles, которые сейчас удерживает render/UI thread.
    leased_video_textures: HashMap<(u64, u64), usize>,

    /// Texture handles, release которых отложен до drop-ack от render/UI thread.
    deferred_video_texture_releases: HashSet<(u64, u64)>,

    /// Provider-ы кадров, у которых render lease уже dropped до player-owned release.
    rendered_video_texture_release_providers:
        HashMap<(u64, u64), PresentFrameResourceProviderHandle>,

    /// Track ID выбранного video трека.
    video_track_id: Option<TrackId>,

    /// Очередь сырых video packets для decode.
    pending_video_packets: VecDeque<PendingVideoPacket>,

    /// Нейтральный audio clock для A/V sync.
    audio_clock: Option<Arc<dyn PlayerAudioClock>>,

    /// Абсолютная media-позиция, соответствующая нулю текущего audio clock.
    media_clock_base: Duration,

    /// Rate-aware mapping от output clock к media timeline.
    audio_clock_media_mapping_anchor: AudioClockMediaMappingAnchor,

    /// Внутренний monotonic clock для playback без доступного audio clock.
    monotonic_media_clock_anchor: Option<MonotonicMediaClockAnchor>,

    /// Поколение packets после последнего seek transaction.
    seek_generation: u64,

    /// Последнее поколение, для которого audio output подтвердил очистку buffer.
    audio_buffer_clear_generation: u64,

    /// Последнее значение audio clock для обнаружения stalled audio.
    last_audio_clock: Duration,

    /// Момент последнего изменения audio clock.
    last_audio_clock_change_at: Instant,

    /// Индикатор текущего видео backend для UI и диагностики.
    video_backend: &'static str,

    /// Требование активного video track, уточнённое container metadata или bitstream probe.
    active_video_requirement: Option<VideoDecodeRequirement>,

    /// Runtime frame contract, который decoder stream должен публиковать для active track.
    active_video_frame_contract: Option<VideoFrameContract>,
}

/// Чистая часть классификации audio slots для тестов без CPAL output.
#[must_use]
fn audio_seek_runtime_state_from_slots(
    audio_track_selected: bool,
    audio_decoder_installed: bool,
    audio_output_installed: bool,
) -> AudioSeekRuntimeState {
    if !audio_track_selected {
        return AudioSeekRuntimeState::NoSelectedAudio;
    }

    if !audio_decoder_installed {
        return AudioSeekRuntimeState::WaitingForDecoder;
    }

    if !audio_output_installed {
        return AudioSeekRuntimeState::WaitingForOutput;
    }

    AudioSeekRuntimeState::Ready
}

/// Explicit fallback contract for tests/no-capability paths.
#[cfg(test)]
fn fallback_frame_contract_for_unprobed_requirement(
    requirement: &VideoDecodeRequirement,
) -> VideoFrameContract {
    match video_frame_pixel_layout_from_decode_requirement(requirement) {
        Some(VideoFramePixelLayout::P010) => {
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers)
        }
        _ => VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
    }
}

impl Default for PlaybackPipeline {
    /// Возвращает начальные значения pipeline без decoder/demuxer/audio ресурсов.
    fn default() -> Self {
        Self {
            demuxer: None,
            file_path: None,
            tracks: Vec::new(),
            source_label: None,
            audio_decoder: None,
            deferred_audio_decoder_config: None,
            audio_output: None,
            audio_tempo_processor: None,
            audio_tempo_pcm_format: None,
            next_audio_tempo_segment_id: 1,
            audio_output_play_requested: false,
            audio_track_id: None,
            pending_audio_packets: VecDeque::new(),
            video_decoder_thread: None,
            video_decoder_needs_keyframe: true,
            video_decode_in_flight_packets: 0,
            video_frame_queue: VecDeque::new(),
            seek_preroll_fallback_video_frame: None,
            video_frame_duration_estimate: DEFAULT_VIDEO_FRAME_DURATION,
            last_decoded_video_pts: None,
            present_video_frame: None,
            render_generation: 0,
            leased_video_textures: HashMap::new(),
            deferred_video_texture_releases: HashSet::new(),
            rendered_video_texture_release_providers: HashMap::new(),
            video_track_id: None,
            pending_video_packets: VecDeque::new(),
            audio_clock: None,
            media_clock_base: Duration::ZERO,
            audio_clock_media_mapping_anchor: AudioClockMediaMappingAnchor::new(
                Duration::ZERO,
                Duration::ZERO,
                PlaybackRate::NORMAL,
            ),
            monotonic_media_clock_anchor: None,
            seek_generation: 0,
            audio_buffer_clear_generation: 0,
            last_audio_clock: Duration::ZERO,
            last_audio_clock_change_at: Instant::now(),
            video_backend: "Synthetic (test)",
            active_video_requirement: None,
            active_video_frame_contract: None,
        }
    }
}
