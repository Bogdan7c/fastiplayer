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
    DecodeSendError, DecodeThreadError, DecoderControlChannelPressureSnapshot,
    DecoderResourceSnapshot, PlayerAudioClock, PlayerAudioOutput, PlayerDecodePacket,
    PlayerVideoDecoderThreadHandle, PresentFrameResourceProviderHandle,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState, VideoPrerollOutputFloor,
    VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult, VideoStreamConfigResult,
    VideoStreamDecodeConfig,
};
#[cfg(test)]
use video_core::VideoDecoderThreadHandle;

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
    #[allow(dead_code)]
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
}

impl MonotonicMediaClockAnchor {
    /// Создаёт anchor без привязки к video FPS или worker tick cadence.
    #[must_use]
    const fn new(media_position: Duration, anchored_at: Instant) -> Self {
        Self {
            media_position,
            anchored_at,
        }
    }

    /// Возвращает media position на заданный monotonic момент.
    #[must_use]
    fn position_at(self, now: Instant) -> Duration {
        self.media_position
            .checked_add(now.saturating_duration_since(self.anchored_at))
            .unwrap_or(Duration::MAX)
    }
}

/// Сырой audio packet, который ждёт decode из-за backpressure audio buffer.
pub(crate) struct PendingAudioPacket {
    /// Track ID нужен, чтобы не отправить packet неактивного audio track в decoder.
    pub(crate) track_id: TrackId,

    /// Presentation timestamp packet-а на абсолютной media timeline.
    pub(crate) pts: Duration,

    /// Decode timestamp, если container сообщил DTS отдельно от PTS.
    pub(crate) dts: Option<Duration>,

    /// Packet duration, если demuxer смог сохранить container duration.
    pub(crate) duration: Option<Duration>,

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
        dts: Option<Duration>,
        duration: Option<Duration>,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
            dts,
            duration,
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
        dts: Option<Duration>,
        duration: Option<Duration>,
        timing: audio_core::AudioPacketTiming,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
            dts,
            duration,
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

impl PlaybackPipeline {
    /// Устанавливает codec-neutral audio decoder, созданный session policy слоем.
    pub(crate) fn install_audio_decoder(&mut self, decoder: audio_core::AudioDecoderHandle) {
        self.audio_decoder = Some(decoder);
        self.deferred_audio_decoder_config = None;
    }

    /// Удаляет audio decoder runtime slot без side effects на output/track selection.
    pub(crate) fn clear_audio_decoder(&mut self) {
        self.audio_decoder = None;
    }

    /// Проверяет наличие audio decoder-а без раскрытия `Option` storage.
    #[must_use]
    pub(crate) fn has_audio_decoder(&self) -> bool {
        self.audio_decoder.is_some()
    }

    /// Сохраняет decoder config до первого selected audio packet-а.
    pub(crate) fn install_deferred_audio_decoder_config(
        &mut self,
        config: audio_core::AudioDecoderConfig,
    ) {
        self.deferred_audio_decoder_config = Some(config);
    }

    /// Проверяет наличие deferred decoder config без раскрытия `Option` storage.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_deferred_audio_decoder_config(&self) -> bool {
        self.deferred_audio_decoder_config.is_some()
    }

    /// Забирает deferred decoder config только для выбранного track-а.
    pub(crate) fn take_deferred_audio_decoder_config(
        &mut self,
        track_id: TrackId,
    ) -> Option<audio_core::AudioDecoderConfig> {
        let config = self.deferred_audio_decoder_config.as_ref()?;
        if config.track_id() != track_id.get() {
            return None;
        }

        self.deferred_audio_decoder_config.take()
    }

    /// Удаляет deferred decoder config при сбросе media или отключении audio path.
    pub(crate) fn clear_deferred_audio_decoder_config(&mut self) {
        self.deferred_audio_decoder_config = None;
    }

    /// Декодирует audio packet через установленный decoder.
    ///
    /// `None` означает absent decoder и сохраняет прежний no-op path. `Err`
    /// остаётся отдельным состоянием, чтобы session могла выставить runtime error.
    pub(crate) fn decode_audio_packet(
        &mut self,
        encoded_audio_packet: &audio_core::EncodedAudioPacket<'_>,
    ) -> Option<anyhow::Result<DecodedAudioPacket>> {
        let decoder = self.audio_decoder.as_mut()?;
        let decode_result = decoder.decode(encoded_audio_packet);
        Some(decode_result.map(|samples| DecodedAudioPacket {
            samples,
            sample_rate: decoder.sample_rate(),
            channels: decoder.channels(),
        }))
    }

    /// Сбрасывает codec state установленного audio decoder-а после seek/discontinuity.
    ///
    /// `None` означает absent decoder, а `Some(Err(_))` сохраняет reset error для session.
    pub(crate) fn reset_audio_decoder(&mut self) -> Option<anyhow::Result<()>> {
        self.audio_decoder.as_mut().map(|decoder| decoder.reset())
    }

    /// Устанавливает audio output, созданный composition/session boundary.
    pub(crate) fn install_audio_output(&mut self, output: Box<dyn PlayerAudioOutput>) {
        self.audio_output = Some(output);
        self.audio_output_play_requested = false;
    }

    /// Устанавливает fake/stub output для unit-тестов без CPAL device side effects.
    #[cfg(test)]
    pub(crate) fn install_audio_output_for_tests(&mut self, output: Box<dyn PlayerAudioOutput>) {
        self.audio_output = Some(output);
        self.audio_output_play_requested = false;
    }

    /// Удаляет audio output runtime slot без изменения decoder/track selection.
    pub(crate) fn clear_audio_output(&mut self) {
        self.audio_output = None;
        self.audio_output_play_requested = false;
    }

    /// Проверяет наличие audio output-а без доступа к concrete CPAL handle.
    #[must_use]
    pub(crate) fn has_audio_output(&self) -> bool {
        self.audio_output.is_some()
    }

    /// Записывает samples в output ring buffer, если output установлен.
    ///
    /// Возвращает `None` для absent output и `Some(written_samples)` для активного output.
    pub(crate) fn write_audio_output_samples(&mut self, samples: &[f32]) -> Option<u64> {
        self.audio_output
            .as_mut()
            .map(|output| output.write_samples(samples))
    }

    /// Запускает audio output stream без смешивания absent output и CPAL error.
    pub(crate) fn play_audio_output(&mut self) -> Option<anyhow::Result<()>> {
        let output = self.audio_output.as_mut()?;
        let play_result = output.play();
        if play_result.is_ok() {
            self.audio_output_play_requested = true;
        }
        Some(play_result)
    }

    /// Ставит audio output stream на паузу без изменения high-level playback state.
    pub(crate) fn pause_audio_output(&mut self) -> Option<anyhow::Result<()>> {
        let output = self.audio_output.as_mut()?;
        let pause_result = output.pause();
        if pause_result.is_ok() {
            self.audio_output_play_requested = false;
        }
        Some(pause_result)
    }

    /// Очищает audio output buffer для seek и возвращает sync ack generation.
    pub(crate) fn clear_audio_output_for_seek(
        &mut self,
        generation: u64,
    ) -> Option<anyhow::Result<u64>> {
        self.audio_output
            .as_mut()
            .map(|output| output.clear_buffer_for_seek(generation))
    }

    /// Устанавливает громкость output-а, если runtime output уже существует.
    pub(crate) fn set_audio_output_volume(&mut self, volume: f32) -> bool {
        let Some(output) = self.audio_output.as_mut() else {
            return false;
        };

        output.set_volume(volume);
        true
    }

    /// Возвращает уровень audio buffer через output boundary.
    #[must_use]
    pub(crate) fn audio_output_buffer_level_ms(&self) -> Option<f64> {
        self.audio_output
            .as_ref()
            .map(|output| output.buffer_level_ms())
    }

    /// Возвращает EOF-drain состояние audio tail-а без доступа к queue/output storage.
    #[must_use]
    pub(crate) fn audio_eof_drain_state(&self) -> AudioEofDrainState {
        if self.audio_track_id.is_none() {
            return AudioEofDrainState::NoSelectedAudio;
        }

        let queued_packets = self.pending_audio_packets.len();
        if queued_packets > 0 {
            return AudioEofDrainState::PendingPackets { queued_packets };
        }

        let Some(output) = self.audio_output.as_ref() else {
            return AudioEofDrainState::NoOutput;
        };

        let buffer_level_ms = output.buffer_level_ms();
        if buffer_level_ms.is_finite() && buffer_level_ms > 0.0 {
            return AudioEofDrainState::DrainingOutput {
                buffer_level_ms,
                playback_requested: self.audio_output_play_requested,
            };
        }

        AudioEofDrainState::DrainedOutput {
            playback_requested: self.audio_output_play_requested,
        }
    }

    /// Возвращает clock handle output-а без раскрытия самого output slot-а.
    #[must_use]
    pub(crate) fn audio_output_clock(&self) -> Option<Arc<dyn PlayerAudioClock>> {
        self.audio_output.as_ref().map(|output| output.clock())
    }

    /// Проверяет наличие audio clock без раскрытия `Option` storage.
    #[must_use]
    pub(crate) fn has_audio_clock(&self) -> bool {
        self.audio_clock.is_some()
    }

    /// Устанавливает audio clock handle и отключает no-audio monotonic fallback.
    pub(crate) fn install_audio_clock(&mut self, clock: Arc<dyn PlayerAudioClock>) {
        self.audio_clock = Some(clock);
        self.clear_monotonic_media_clock();
    }

    /// Удаляет audio clock handle без изменения decoder/output slots.
    pub(crate) fn clear_audio_clock(&mut self) {
        self.audio_clock = None;
    }

    /// Сбрасывает установленный audio clock; absent clock остаётся явным no-op.
    pub(crate) fn reset_audio_clock(&self) -> bool {
        let Some(clock) = self.audio_clock.as_ref() else {
            return false;
        };

        clock.reset();
        true
    }

    /// Возвращает текущее audio clock time или `Duration::ZERO`, если clock отсутствует.
    #[must_use]
    pub(crate) fn audio_clock_now(&self) -> Duration {
        self.audio_clock
            .as_ref()
            .map(|clock| clock.now())
            .unwrap_or(Duration::ZERO)
    }

    /// Возвращает число audio underrun callbacks для snapshot diagnostics.
    #[must_use]
    pub(crate) fn audio_clock_underrun_callbacks(&self) -> u64 {
        self.audio_clock
            .as_ref()
            .map(|clock| clock.underrun_callbacks())
            .unwrap_or(0)
    }

    /// Возвращает media base, соответствующий нулю текущего audio clock.
    #[must_use]
    pub(crate) const fn media_clock_base(&self) -> Duration {
        self.media_clock_base
    }

    /// Устанавливает media base без изменения audio clock handle или sample accounting.
    pub(crate) fn set_media_clock_base(&mut self, position: Duration) {
        self.media_clock_base = position;
    }

    /// Возвращает абсолютную media position, вычисленную от audio clock и media base.
    #[must_use]
    pub(crate) fn media_position_from_audio_clock(&self) -> Duration {
        self.media_clock_base
            .checked_add(self.audio_clock_now())
            .unwrap_or(Duration::MAX)
    }

    /// Запускает monotonic fallback clock от заданной media position.
    pub(crate) fn start_monotonic_media_clock(&mut self, position: Duration, now: Instant) {
        self.monotonic_media_clock_anchor = Some(MonotonicMediaClockAnchor::new(position, now));
    }

    /// Очищает monotonic fallback clock для pause/seek/audio-clock paths.
    pub(crate) fn clear_monotonic_media_clock(&mut self) {
        self.monotonic_media_clock_anchor = None;
    }

    /// Возвращает no-audio fallback media position, если fallback anchor активен.
    #[must_use]
    pub(crate) fn monotonic_media_position(&self, now: Instant) -> Option<Duration> {
        self.monotonic_media_clock_anchor
            .map(|anchor| anchor.position_at(now))
    }

    /// Обновляет sample для stall detection только при реальном движении audio clock.
    pub(crate) fn note_audio_clock_sample(&mut self, audio_now: Duration, observed_at: Instant) {
        if audio_now == self.last_audio_clock {
            return;
        }

        self.last_audio_clock = audio_now;
        self.last_audio_clock_change_at = observed_at;
    }

    /// Переставляет baseline stall detection после play/autoplay/seek resume.
    pub(crate) fn reset_audio_clock_sample(&mut self, audio_now: Duration, observed_at: Instant) {
        self.last_audio_clock = audio_now;
        self.last_audio_clock_change_at = observed_at;
    }

    /// Возвращает длительность, в течение которой audio clock не менял значение.
    #[must_use]
    pub(crate) fn audio_clock_stalled_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_audio_clock_change_at)
    }

    /// Возвращает последнее поколение, для которого audio buffer clear был подтверждён.
    #[must_use]
    pub(crate) const fn audio_buffer_clear_generation(&self) -> u64 {
        self.audio_buffer_clear_generation
    }

    /// Отмечает синхронное подтверждение очистки audio buffer для seek generation.
    pub(crate) fn mark_audio_buffer_clear_ack(&mut self, generation: u64) {
        self.audio_buffer_clear_generation = generation;
    }

    /// Возвращает выбранный video track id без раскрытия storage поля.
    #[must_use]
    pub(crate) fn selected_video_track_id(&self) -> Option<TrackId> {
        self.video_track_id
    }

    /// Возвращает выбранный audio track id без раскрытия storage поля.
    #[must_use]
    pub(crate) fn selected_audio_track_id(&self) -> Option<TrackId> {
        self.audio_track_id
    }

    /// Проверяет, выбран ли video track в текущем pipeline.
    #[must_use]
    pub(crate) fn has_selected_video_track(&self) -> bool {
        self.video_track_id.is_some()
    }

    /// Проверяет, выбран ли audio track в текущем pipeline.
    #[must_use]
    pub(crate) fn has_selected_audio_track(&self) -> bool {
        self.audio_track_id.is_some()
    }

    /// Классифицирует audio runtime slots для session gate-ов без раскрытия storage полей.
    #[must_use]
    pub(crate) fn audio_seek_runtime_state(&self) -> AudioSeekRuntimeState {
        audio_seek_runtime_state_from_slots(
            self.audio_track_id.is_some(),
            self.audio_decoder.is_some(),
            self.audio_output.is_some(),
        )
    }

    /// Проверяет, относится ли video packet к активному video track.
    #[must_use]
    pub(crate) fn video_packet_belongs_to_selected_track(&self, track_id: TrackId) -> bool {
        self.video_track_id == Some(track_id)
    }

    /// Выбирает video track вместе с уже принятым session policy decode requirement.
    #[cfg(test)]
    pub(crate) fn select_video_track(
        &mut self,
        track_id: TrackId,
        requirement: VideoDecodeRequirement,
    ) {
        let frame_contract = fallback_frame_contract_for_unprobed_requirement(&requirement);
        self.select_video_track_with_frame_contract(track_id, requirement, frame_contract);
    }

    /// Выбирает video track вместе с frame contract из capability output.
    pub(crate) fn select_video_track_with_frame_contract(
        &mut self,
        track_id: TrackId,
        requirement: VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) {
        self.video_track_id = Some(track_id);
        self.active_video_requirement = Some(requirement);
        self.active_video_frame_contract = Some(frame_contract);
    }

    /// Выбирает audio track id без side effects для decoder/output ownership.
    pub(crate) fn select_audio_track(&mut self, track_id: TrackId) {
        self.audio_track_id = Some(track_id);
    }

    /// Очищает только выбранный audio track и связанный deferred decoder plan.
    pub(crate) fn clear_selected_audio_track(&mut self) {
        self.audio_track_id = None;
        self.clear_deferred_audio_decoder_config();
    }

    /// Очищает выбранные tracks и requirement, не трогая queues/decoder/source slots.
    pub(crate) fn clear_selected_tracks(&mut self) {
        self.audio_track_id = None;
        self.video_track_id = None;
        self.active_video_requirement = None;
        self.active_video_frame_contract = None;
    }

    /// Возвращает active video requirement без передачи владения наружу pipeline.
    #[must_use]
    pub(crate) fn active_video_requirement(&self) -> Option<&VideoDecodeRequirement> {
        self.active_video_requirement.as_ref()
    }

    /// Возвращает expected runtime frame contract active video stream-а.
    #[must_use]
    pub(crate) fn active_video_frame_contract(&self) -> Option<VideoFrameContract> {
        self.active_video_frame_contract
    }

    /// Сохраняет requirement и frame contract, которые session уже провалидировала.
    pub(crate) fn set_active_video_selection(
        &mut self,
        requirement: VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) {
        self.active_video_requirement = Some(requirement);
        self.active_video_frame_contract = Some(frame_contract);
    }

    /// Legacy/test helper: обновляет requirement с explicit fallback contract.
    #[cfg(test)]
    pub(crate) fn set_active_video_requirement(&mut self, requirement: VideoDecodeRequirement) {
        let frame_contract = fallback_frame_contract_for_unprobed_requirement(&requirement);
        self.set_active_video_selection(requirement, frame_contract);
    }

    /// Возвращает текущее поколение packets без раскрытия storage поля.
    #[must_use]
    pub(crate) const fn seek_generation(&self) -> u64 {
        self.seek_generation
    }

    /// Начинает новое поколение packets для seek transaction.
    ///
    /// Saturating increment оставляет поведение прежним: после переполнения
    /// generation фиксируется на `u64::MAX`, а не делает wrap в старые packets.
    pub(crate) fn begin_seek_generation(&mut self) -> u64 {
        self.seek_generation = self.seek_generation.saturating_add(1);
        self.seek_generation
    }

    /// Проверяет, относится ли packet к актуальному поколению после последнего seek.
    #[must_use]
    pub(crate) const fn packet_generation_is_current(&self, generation: u64) -> bool {
        generation == self.seek_generation
    }

    /// Возвращает текущую оценку длительности video frame без раскрытия estimator state.
    #[must_use]
    pub(crate) const fn video_frame_duration_estimate(&self) -> Duration {
        self.video_frame_duration_estimate
    }

    /// Обновляет estimator по очередному decoded PTS, сохраняя прежнюю smoothing formula.
    pub(crate) fn observe_decoded_video_frame_pts(&mut self, pts: Duration) {
        if let Some(previous_pts) = self.last_decoded_video_pts {
            let observed_duration = pts.saturating_sub(previous_pts);
            if (MIN_OBSERVED_VIDEO_FRAME_DURATION..=MAX_OBSERVED_VIDEO_FRAME_DURATION)
                .contains(&observed_duration)
            {
                let old_micros = self.video_frame_duration_estimate.as_micros() as u64;
                let new_micros = observed_duration.as_micros() as u64;
                let smoothed_micros = (old_micros.saturating_mul(7) + new_micros) / 8;
                self.video_frame_duration_estimate = Duration::from_micros(smoothed_micros.max(1));
            }
        }

        self.last_decoded_video_pts = Some(pts);
    }

    /// Возвращает estimator к bootstrap duration и забывает предыдущий decoded PTS.
    pub(crate) fn reset_video_frame_timing_estimator(&mut self) {
        self.video_frame_duration_estimate = DEFAULT_VIDEO_FRAME_DURATION;
        self.last_decoded_video_pts = None;
    }

    /// Устанавливает generation для edge-case тестов saturation без обхода boundary чтения.
    #[cfg(test)]
    pub(crate) fn set_seek_generation_for_tests(&mut self, generation: u64) {
        self.seek_generation = generation;
    }

    /// Очищает pending audio/video packets, которые относятся к старой seek generation.
    pub(crate) fn clear_pending_packets_for_seek(&mut self) {
        self.clear_pending_audio_packets();
        self.clear_pending_video_packets();
    }

    /// Сбрасывает decoder-side состояние, которое становится невалидным после seek.
    pub(crate) fn reset_decoder_state_for_seek(&mut self, has_video: bool) {
        if has_video {
            self.require_video_decoder_keyframe();
        } else {
            self.mark_video_decoder_bootstrapped();
        }
        self.reset_video_decode_in_flight();
        self.last_decoded_video_pts = None;
    }

    /// Переставляет media clocks на целевую позицию seek.
    pub(crate) fn reset_clocks_for_seek(&mut self, target: Duration) {
        self.reanchor_media_clock_for_seek(target, Instant::now());
    }

    /// Перепривязывает seek clock base без доступа к внутренним clock полям.
    pub(crate) fn reanchor_media_clock_for_seek(
        &mut self,
        position: Duration,
        observed_at: Instant,
    ) {
        self.set_media_clock_base(position);
        self.clear_monotonic_media_clock();
        self.reset_audio_clock_sample(Duration::ZERO, observed_at);
    }

    /// Очищает очередь будущих video frames и возвращает texture handles для release.
    #[must_use]
    pub(crate) fn clear_video_queues(&mut self) -> Vec<video_core::FrameResourceHandle> {
        self.video_frame_queue
            .drain(..)
            .map(|frame| frame.resource_handle)
            .collect()
    }

    /// Возвращает текущий present frame без передачи владения наружу pipeline.
    #[must_use]
    pub(crate) fn present_video_frame(&self) -> Option<&video_core::DecodedFrame> {
        self.present_video_frame.as_ref()
    }

    /// Возвращает PTS текущего present frame-а для diagnostics и seek gates.
    #[must_use]
    pub(crate) fn present_video_frame_pts(&self) -> Option<Duration> {
        self.present_video_frame().map(|frame| frame.pts)
    }

    /// Проверяет, что текущий present frame покрывает целевую media-позицию.
    #[must_use]
    pub(crate) fn present_video_frame_covers(&self, target: Duration) -> bool {
        self.present_video_frame()
            .is_some_and(|frame| frame.pts >= target)
    }

    /// Проверяет, что текущий present frame ровно совпадает с media-позицией.
    #[must_use]
    pub(crate) fn present_video_frame_matches(&self, position: Duration) -> bool {
        self.present_video_frame()
            .is_some_and(|frame| frame.pts == position)
    }

    /// Проверяет наличие текущего present frame-а без раскрытия внутреннего `Option`.
    #[must_use]
    pub(crate) fn has_present_video_frame(&self) -> bool {
        self.present_video_frame.is_some()
    }

    /// Делает decoded frame текущим кадром presentation.
    pub(crate) fn set_present_video_frame(&mut self, frame: video_core::DecodedFrame) {
        self.present_video_frame = Some(frame);
    }

    /// Забирает текущий present frame, чтобы вызывающий слой мог освободить texture.
    pub(crate) fn take_present_video_frame(&mut self) -> Option<video_core::DecodedFrame> {
        self.present_video_frame.take()
    }

    /// Заменяет текущий present frame и возвращает старый frame для явного release.
    pub(crate) fn replace_present_video_frame(
        &mut self,
        frame: video_core::DecodedFrame,
    ) -> Option<video_core::DecodedFrame> {
        let old_frame = self.take_present_video_frame();
        self.set_present_video_frame(frame);
        old_frame
    }

    /// Проверяет наличие EOF fallback frame-а для final seek near EOF.
    #[must_use]
    pub(crate) fn has_seek_preroll_fallback_video_frame(&self) -> bool {
        self.seek_preroll_fallback_video_frame.is_some()
    }

    /// Забирает EOF fallback frame, когда scheduler решил показать его после EOF.
    pub(crate) fn take_seek_preroll_fallback_video_frame(
        &mut self,
    ) -> Option<video_core::DecodedFrame> {
        self.seek_preroll_fallback_video_frame.take()
    }

    /// Заменяет EOF fallback frame и возвращает прежний frame для явного release.
    pub(crate) fn replace_seek_preroll_fallback_video_frame(
        &mut self,
        frame: video_core::DecodedFrame,
    ) -> Option<video_core::DecodedFrame> {
        self.seek_preroll_fallback_video_frame.replace(frame)
    }

    /// Очищает EOF fallback frame и возвращает его владельцу release path-а.
    pub(crate) fn clear_seek_preroll_fallback_video_frame(
        &mut self,
    ) -> Option<video_core::DecodedFrame> {
        self.seek_preroll_fallback_video_frame.take()
    }

    /// Добавляет audio packet в pending queue текущего pipeline.
    pub(crate) fn enqueue_pending_audio_packet(&mut self, packet: PendingAudioPacket) {
        self.pending_audio_packets.push_back(packet);
    }

    /// Добавляет video packet в pending queue текущего pipeline.
    pub(crate) fn enqueue_pending_video_packet(&mut self, packet: PendingVideoPacket) {
        self.pending_video_packets.push_back(packet);
    }

    /// Забирает первый pending video packet для drop или отправки в decoder.
    pub(crate) fn pop_pending_video_packet_front(&mut self) -> Option<PendingVideoPacket> {
        self.pending_video_packets.pop_front()
    }

    /// Возвращает первый pending video packet без снятия его с очереди.
    #[must_use]
    pub(crate) fn front_pending_video_packet(&self) -> Option<&PendingVideoPacket> {
        self.pending_video_packets.front()
    }

    /// Проверяет, пуста ли очередь pending video packets.
    #[must_use]
    pub(crate) fn pending_video_packet_is_empty(&self) -> bool {
        self.pending_video_packets.is_empty()
    }

    /// Очищает очередь pending video packets через единый pipeline boundary.
    pub(crate) fn clear_pending_video_packets(&mut self) {
        self.pending_video_packets.clear();
    }

    /// Забирает первый pending audio packet для декодирования.
    pub(crate) fn pop_pending_audio_packet_front(&mut self) -> Option<PendingAudioPacket> {
        self.pending_audio_packets.pop_front()
    }

    /// Возвращает audio packet обратно в начало очереди после throttle.
    pub(crate) fn push_pending_audio_packet_front(&mut self, packet: PendingAudioPacket) {
        self.pending_audio_packets.push_front(packet);
    }

    /// Проверяет, пуста ли очередь pending audio packets.
    #[must_use]
    pub(crate) fn pending_audio_packet_is_empty(&self) -> bool {
        self.pending_audio_packets.is_empty()
    }

    /// Возвращает глубину pending audio queue без раскрытия поля очереди.
    #[must_use]
    pub(crate) fn pending_audio_packet_len(&self) -> usize {
        self.pending_audio_packets.len()
    }

    /// Очищает очередь pending audio packets через единый pipeline boundary.
    pub(crate) fn clear_pending_audio_packets(&mut self) {
        self.pending_audio_packets.clear();
    }

    /// Возвращает первый decoded frame из presentation queue без мутации.
    #[must_use]
    pub(crate) fn front_queued_video_frame(&self) -> Option<&video_core::DecodedFrame> {
        self.video_frame_queue.front()
    }

    /// Даёт read-only проход по presentation queue без доступа к самой структуре очереди.
    pub(crate) fn queued_video_frames(&self) -> impl Iterator<Item = &video_core::DecodedFrame> {
        self.video_frame_queue.iter()
    }

    /// Возвращает первый и следующий за ним decoded frames без раскрытия очереди.
    #[must_use]
    pub(crate) fn front_and_next_queued_video_frames(
        &self,
    ) -> Option<(&video_core::DecodedFrame, &video_core::DecodedFrame)> {
        let front_frame = self.video_frame_queue.front()?;
        let next_frame = self.video_frame_queue.get(1)?;

        Some((front_frame, next_frame))
    }

    /// Забирает первый decoded frame из presentation queue.
    pub(crate) fn pop_queued_video_frame_front(&mut self) -> Option<video_core::DecodedFrame> {
        self.video_frame_queue.pop_front()
    }

    /// Добавляет decoded frame в конец presentation queue.
    pub(crate) fn enqueue_queued_video_frame(&mut self, frame: video_core::DecodedFrame) {
        self.video_frame_queue.push_back(frame);
    }

    /// Проверяет, пуста ли presentation queue.
    #[must_use]
    pub(crate) fn video_present_queue_is_empty(&self) -> bool {
        self.video_frame_queue.is_empty()
    }

    /// Проверяет, есть ли queued frame текущего seek generation-а для final target.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn queued_video_frame_covers_target_for_generation(
        &self,
        target: Duration,
        generation: u64,
    ) -> bool {
        self.video_frame_queue
            .iter()
            .any(|frame| frame.generation == generation && frame.pts >= target)
    }

    /// Возвращает глубину presentation queue без раскрытия поля очереди.
    #[must_use]
    pub(crate) fn video_present_queue_len(&self) -> usize {
        self.video_frame_queue.len()
    }

    /// Возвращает глубину pending video queue без раскрытия поля очереди.
    #[must_use]
    pub(crate) fn pending_video_packet_len(&self) -> usize {
        self.pending_video_packets.len()
    }

    /// Проверяет, ждёт ли video decoder первый keyframe после bootstrap/flush.
    #[must_use]
    pub(crate) const fn video_decoder_needs_keyframe(&self) -> bool {
        self.video_decoder_needs_keyframe
    }

    /// Отмечает, что decoder получил keyframe и может принимать inter-frames.
    pub(crate) fn mark_video_decoder_bootstrapped(&mut self) {
        self.video_decoder_needs_keyframe = false;
    }

    /// Требует новый keyframe перед следующей отправкой packets в decoder.
    pub(crate) fn require_video_decoder_keyframe(&mut self) {
        self.video_decoder_needs_keyframe = true;
    }

    /// Переводит renderer resource ids в новое поколение после полной смены media.
    pub(crate) fn advance_render_generation(&mut self) {
        self.rendered_video_texture_release_providers.clear();
        self.render_generation = self.render_generation.wrapping_add(1);
    }

    /// Возвращает текущее поколение render resources без доступа к полю accounting-а.
    #[must_use]
    pub(crate) const fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Сбрасывает все media-specific поля после того, как session освободила video frames.
    pub(crate) fn reset_media_slots(&mut self) {
        self.clear_media_source_slots();
        self.clear_audio_decoder();
        self.clear_deferred_audio_decoder_config();
        self.clear_audio_output();
        self.clear_selected_tracks();
        self.clear_pending_audio_packets();
        self.clear_pending_video_packets();
        self.rendered_video_texture_release_providers.clear();
        self.require_video_decoder_keyframe();
        self.reset_video_decode_in_flight();
        debug_assert!(
            self.seek_preroll_fallback_video_frame.is_none(),
            "reset_media_slots вызывается только после release всех video frames"
        );
        self.seek_preroll_fallback_video_frame = None;
        self.clear_audio_clock();
        self.set_media_clock_base(Duration::ZERO);
        self.clear_monotonic_media_clock();
        self.seek_generation = 0;
        self.mark_audio_buffer_clear_ack(0);
        self.reset_video_frame_timing_estimator();
        self.reset_audio_clock_sample(Duration::ZERO, Instant::now());
    }

    /// Очищает только source identity и demux handle без изменения остальных lifecycle slots.
    fn clear_media_source_slots(&mut self) {
        self.demuxer = None;
        self.file_path = None;
        self.tracks.clear();
        self.source_label = None;
    }

    /// Подключает уже открытый demuxer и source identity к текущему pipeline.
    pub(crate) fn install_opened_media(
        &mut self,
        demuxer: Box<dyn Demuxer + Send>,
        file_path: Option<PathBuf>,
        source_label: Option<String>,
        tracks: Vec<TrackInfo>,
    ) {
        self.demuxer = Some(demuxer);
        self.file_path = file_path;
        self.source_label = source_label;
        self.tracks = tracks;
    }

    /// Применяет новый track list после demux lifecycle reset.
    ///
    /// Demuxer остаётся тем же владельцем source-а, но decoder-dependent state
    /// должен быть пересоздан, потому что старые configs могли ссылаться на уже
    /// неактуальные track ids, codec params или audio spec.
    pub(crate) fn apply_demux_track_list_update(&mut self, tracks: Vec<TrackInfo>) {
        let has_video_track = tracks
            .iter()
            .any(|track| track.kind == media_core::TrackKind::Video);

        self.tracks = tracks;
        self.clear_audio_decoder();
        self.clear_deferred_audio_decoder_config();
        self.clear_audio_output();
        self.clear_audio_clock();
        self.clear_selected_tracks();
        self.clear_pending_audio_packets();
        self.clear_pending_video_packets();
        self.mark_audio_buffer_clear_ack(self.seek_generation);
        self.reset_decoder_state_for_seek(has_video_track);
    }

    /// Проверяет, установлен ли demuxer текущего media source.
    #[must_use]
    pub(crate) fn has_demuxer(&self) -> bool {
        self.demuxer.is_some()
    }

    /// Возвращает путь локального source без передачи владения path storage.
    #[must_use]
    pub(crate) fn source_file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    /// Возвращает streaming/source label без раскрытия внутреннего `Option<String>`.
    #[must_use]
    pub(crate) fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }

    /// Возвращает immutable tracks snapshot текущего media.
    #[must_use]
    pub(crate) fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Возвращает количество tracks, когда вызывающему коду не нужны сами metadata.
    #[must_use]
    pub(crate) fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Читает следующий packet через demux boundary, сохраняя absent-demuxer как no-op.
    #[cfg(test)]
    pub(crate) fn demux_next_packet(
        &mut self,
    ) -> Option<anyhow::Result<Option<media_core::Packet>>> {
        self.demuxer.as_mut().map(|demuxer| demuxer.next_packet())
    }

    /// Читает следующий demux event, сохраняя absent-demuxer как no-op.
    pub(crate) fn demux_next_event(&mut self) -> Option<anyhow::Result<DemuxReadEvent>> {
        self.demuxer.as_mut().map(|demuxer| demuxer.next_event())
    }

    /// Выполняет seek текущего demuxer-а, не раскрывая место хранения demux handle.
    pub(crate) fn seek_demuxer(
        &mut self,
        request: DemuxSeekRequest,
    ) -> Option<anyhow::Result<DemuxSeekResult>> {
        self.demuxer
            .as_mut()
            .map(|demuxer| demuxer.seek_with_request(request))
    }

    /// Сохраняет запущенный video backend без раскрытия backend-specific init в session.
    #[cfg(test)]
    pub(crate) fn set_video_decoder_thread(
        &mut self,
        decoder_thread: impl VideoDecoderThreadHandle<
            ResourceProvider = PresentFrameResourceProviderHandle,
        > + 'static,
    ) {
        self.set_video_decoder_thread_handle(Box::new(decoder_thread));
    }

    /// Сохраняет decoder handle, который уже прошёл backend startup boundary.
    pub(crate) fn set_video_decoder_thread_handle(
        &mut self,
        decoder_thread: Box<PlayerVideoDecoderThreadHandle>,
    ) {
        self.video_backend = decoder_thread.backend_name();
        self.video_decoder_thread = Some(decoder_thread);
        self.reset_video_decode_in_flight();
    }

    /// Проверяет, есть ли active decoder для операций presentation/render handoff.
    #[must_use]
    pub(crate) fn has_active_video_decoder(&self) -> bool {
        self.video_decoder_thread.is_some()
    }

    /// Возвращает имя текущего video backend-а без раскрытия runtime slot-а.
    #[must_use]
    pub(crate) const fn video_backend_name(&self) -> &'static str {
        self.video_backend
    }

    /// Проверяет, можно ли отправлять encoded packets через decoder I/O boundary.
    ///
    /// Tick-код использует этот метод как send-side readiness и не зависит от
    /// того, каким полем pipeline владеет активным decoder backend-ом.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn can_send_video_decode_packets(&self) -> bool {
        self.video_decoder_send_backpressure(usize::MAX).is_none()
    }

    /// Возвращает typed причину send-side backpressure без доступа к backend internals.
    ///
    /// `UnsupportedBackend` у host-upload snapshot-а означает hardware/old backend и
    /// оставляет старую VA-API surface accounting ветку активной. `AbsentResource`
    /// остаётся отличимым через `host_upload_resource_snapshot()` и не маскируется
    /// под свободные upload slots.
    #[must_use]
    pub(crate) fn video_decoder_send_backpressure(
        &self,
        host_upload_ready_queue_capacity: usize,
    ) -> Option<VideoDecoderSendBackpressure> {
        if self.video_decoder_thread.is_none() {
            return Some(VideoDecoderSendBackpressure::AbsentDecoder);
        }

        match self.host_upload_resource_snapshot() {
            HostUploadResourceSnapshotStatus::Available(snapshot) => {
                if let Some(reason) = self.decoder_control_backpressure_reason() {
                    return Some(VideoDecoderSendBackpressure::DecoderControl(reason));
                }

                snapshot
                    .backpressure_reason(host_upload_ready_queue_capacity)
                    .map(VideoDecoderSendBackpressure::HostUpload)
            }
            HostUploadResourceSnapshotStatus::AbsentDecoder => {
                Some(VideoDecoderSendBackpressure::AbsentDecoder)
            }
            HostUploadResourceSnapshotStatus::AbsentResource
            | HostUploadResourceSnapshotStatus::UnsupportedBackend => None,
        }
    }

    /// Проверяет, можно ли принимать decoded frames через decoder I/O boundary.
    ///
    /// Receive-side readiness сейчас совпадает с наличием active backend-а, но
    /// call sites больше не читают внутреннее устройство decoder thread-а.
    #[must_use]
    pub(crate) fn can_receive_decoded_video_frames(&self) -> bool {
        self.has_active_video_decoder()
    }

    /// Возвращает глубину send queue decoder thread-а, если backend запущен.
    #[must_use]
    pub(crate) fn video_decoder_packet_queue_depth(&self) -> Option<usize> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.packet_queue_depth())
    }

    /// Возвращает snapshot texture pool-а, не раскрывая decoder thread наружу.
    #[must_use]
    pub(crate) fn video_decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.decoder_resource_snapshot())
    }

    /// Возвращает typed snapshot software host-upload ресурсов.
    // S20 подключит этот boundary к scheduler/tick; сейчас его фиксируют focused tests.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn host_upload_resource_snapshot(&self) -> HostUploadResourceSnapshotStatus {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return HostUploadResourceSnapshotStatus::AbsentDecoder;
        };

        decoder_thread.host_upload_resource_snapshot()
    }

    /// Возвращает pressure snapshot decoder control channel-а, если backend его поддерживает.
    pub(crate) fn video_decoder_control_channel_pressure(
        &self,
    ) -> Option<DecoderControlChannelPressureSnapshot> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.decoder_control_channel_pressure())
    }

    /// Возвращает typed control-channel backpressure, если neutral snapshot показывает full queue.
    #[must_use]
    pub(crate) fn decoder_control_backpressure_reason(
        &self,
    ) -> Option<VideoDecoderControlBackpressureReason> {
        let pressure = self.video_decoder_control_channel_pressure()?;
        if pressure.control_channel_capacity == 0
            || pressure.control_channel_len < pressure.control_channel_capacity
        {
            return None;
        }

        Some(VideoDecoderControlBackpressureReason::ControlChannelFull {
            queued_messages: pressure.control_channel_len,
            capacity: pressure.control_channel_capacity,
        })
    }

    /// Возвращает typed status neutral decoder activity boundary-а.
    ///
    /// Planner/worker видят только намерение и typed unavailable reason, а не
    /// concrete decoder thread storage или backend-specific channels.
    #[must_use]
    pub(crate) fn video_decoder_activity_status(&self) -> VideoDecoderActivityStatus {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoDecoderActivityStatus::AbsentDecoder;
        };

        match decoder_thread.decoder_activity_snapshot() {
            VideoDecoderActivitySnapshot::Available {
                captured_epoch,
                subscription,
            } => VideoDecoderActivityStatus::Available {
                snapshot: VideoDecoderActivitySnapshot::Available {
                    captured_epoch,
                    subscription,
                },
            },
            VideoDecoderActivitySnapshot::Unavailable {
                reason: VideoDecoderActivityUnavailableReason::UnsupportedNotifier,
            } => VideoDecoderActivityStatus::Unsupported,
            VideoDecoderActivitySnapshot::Unavailable { reason } => {
                VideoDecoderActivityStatus::Unavailable(reason)
            }
        }
    }

    /// Возвращает renderer-neutral provider активного decoder thread-а.
    #[must_use]
    pub(crate) fn video_decoder_resource_provider(
        &self,
    ) -> Option<PresentFrameResourceProviderHandle> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.resource_provider())
    }

    /// Немедленно отдаёт texture slot активному decoder thread-у.
    ///
    /// Метод намеренно не знает про deferred render leases: это решение остаётся
    /// в `PlayerSession`, потому что только session видит поколение renderer-а.
    pub(crate) fn release_frame_to_video_decoder(
        &self,
        resource_handle: video_core::FrameResourceHandle,
    ) -> bool {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return false;
        };

        decoder_thread.release_frame(resource_handle);
        true
    }

    /// Конфигурирует active decoder stream без seek flush/generation side effects.
    pub(crate) fn configure_video_decoder_stream(
        &self,
        config: VideoStreamDecodeConfig,
    ) -> VideoStreamConfigResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoStreamConfigResult::AbsentDecoder;
        };

        decoder_thread.configure_stream(config)
    }

    /// Очищает stream config активного decoder-а как отдельный media lifecycle step.
    pub(crate) fn clear_video_decoder_stream(&self) -> VideoStreamConfigResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoStreamConfigResult::AbsentDecoder;
        };

        decoder_thread.clear_stream()
    }

    /// Устанавливает decoder-side floor для Accurate preroll без знания concrete backend-а.
    pub(crate) fn set_video_decoder_preroll_output_floor(
        &self,
        floor: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoPrerollOutputFloorResult::AbsentDecoder;
        };

        decoder_thread.set_preroll_output_floor(floor)
    }

    /// Очищает decoder-side Accurate preroll floor через нейтральный decoder boundary.
    pub(crate) fn clear_video_decoder_preroll_output_floor(
        &self,
        clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoPrerollOutputFloorResult::AbsentDecoder;
        };

        decoder_thread.clear_preroll_output_floor(clear)
    }

    /// Запускает decoder EOF/DPB drain без превращения его в seek flush.
    pub(crate) fn begin_video_decoder_end_of_stream_drain(
        &self,
        generation: u64,
    ) -> VideoDecoderEndOfStreamDrainResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoDecoderEndOfStreamDrainResult::AbsentDecoder;
        };

        decoder_thread.begin_end_of_stream_drain(generation)
    }

    /// Возвращает explicit decoder EOF/DPB drain state без чтения decoder storage.
    pub(crate) fn video_decoder_end_of_stream_drain_state(
        &self,
    ) -> VideoDecoderEndOfStreamDrainState {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.end_of_stream_drain_state())
            .unwrap_or(VideoDecoderEndOfStreamDrainState::Idle)
    }

    /// Сбрасывает decoder thread перед seek/media reset.
    ///
    /// Отсутствующий decoder thread остаётся успешным no-op, как и прежний
    /// прямой вызов из session.
    pub(crate) fn flush_video_decoder_thread(&self) -> anyhow::Result<()> {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return Ok(());
        };

        decoder_thread.flush()
    }

    /// Забирает один decoded frame без блокировки worker-а.
    pub(crate) fn try_recv_decoded_video_frame(&self) -> Option<video_core::DecodedFrame> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_frame())
    }

    /// Забирает один diagnostics event от decoder/backend boundary.
    pub(crate) fn try_recv_video_decoder_diagnostic_event(
        &self,
    ) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_diagnostic_event())
    }

    /// Забирает один fatal decoder-thread error, если backend уже остановился.
    pub(crate) fn try_recv_video_decoder_error(&self) -> Option<DecodeThreadError> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_error())
    }

    /// Забирает packet ack-и decoder thread-а без изменения player-side accounting.
    #[must_use]
    pub(crate) fn drain_completed_video_decode_packet_count(&self) -> usize {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.drain_completed_packet_count())
            .unwrap_or(0)
    }

    /// Отправляет encoded packet в активный decoder thread.
    ///
    /// `None` означает, что decoder thread отсутствует. `Some(Ok(()))`
    /// означает принятую отправку, а `Some(Err(_))` сохраняет различие между
    /// backpressure и fatal send failure.
    pub(crate) fn send_video_decode_packet(
        &self,
        packet: PlayerDecodePacket,
    ) -> Option<Result<(), DecodeSendError>> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.send_packet(packet))
    }

    /// Сбрасывает счётчик packets, которые могли остаться внутри decoder после flush/seek.
    pub(crate) fn reset_video_decode_in_flight(&mut self) {
        self.video_decode_in_flight_packets = 0;
    }

    /// Отмечает packet, успешно переданный через worker -> decoder boundary.
    pub(crate) fn note_video_packet_sent_to_decoder(&mut self) {
        self.video_decode_in_flight_packets = self.video_decode_in_flight_packets.saturating_add(1);
    }

    /// Отмечает packets, которые decoder thread обработал без привязки к числу output frames.
    pub(crate) fn note_video_packets_completed_by_decoder(&mut self, packet_count: usize) {
        self.video_decode_in_flight_packets = self
            .video_decode_in_flight_packets
            .saturating_sub(packet_count);
    }

    /// Возвращает приблизительное число packets, которые decoder уже забрал, но ещё не ack-нул.
    #[must_use]
    pub(crate) const fn video_decode_in_flight_packets(&self) -> usize {
        self.video_decode_in_flight_packets
    }

    /// Регистрирует render lease только для актуального поколения renderer resources.
    pub(crate) fn try_register_render_lease(
        &mut self,
        render_generation: u64,
        resource_handle: video_core::FrameResourceHandle,
    ) -> bool {
        if render_generation != self.render_generation {
            return false;
        }

        let lease_key = (render_generation, resource_handle.0);
        let lease_count = self.leased_video_textures.entry(lease_key).or_insert(0);
        *lease_count = lease_count.saturating_add(1);
        true
    }

    /// Снимает один render lease и возвращает точный accounting outcome.
    pub(crate) fn release_render_lease_accounting(
        &mut self,
        render_generation: u64,
        resource_handle: video_core::FrameResourceHandle,
    ) -> RenderLeaseReleaseEffect {
        let lease_key = (render_generation, resource_handle.0);

        let Some(lease_count) = self.leased_video_textures.get_mut(&lease_key) else {
            return RenderLeaseReleaseEffect::UnknownLease;
        };

        if *lease_count > 1 {
            *lease_count -= 1;
            return RenderLeaseReleaseEffect::LeaseStillActive;
        }

        self.leased_video_textures.remove(&lease_key);
        if self.deferred_video_texture_releases.remove(&lease_key) {
            self.rendered_video_texture_release_providers
                .remove(&lease_key);
            RenderLeaseReleaseEffect::DeferredTextureReady
        } else {
            RenderLeaseReleaseEffect::ReleasedWithoutDeferredTexture
        }
    }

    /// Запоминает release provider кадра, который renderer уже использовал и отпустил.
    pub(crate) fn remember_rendered_texture_release_provider(
        &mut self,
        render_generation: u64,
        resource_handle: video_core::FrameResourceHandle,
        resource_provider: &PresentFrameResourceProviderHandle,
    ) {
        if render_generation != self.render_generation {
            return;
        }

        let lease_key = (render_generation, resource_handle.0);
        if self.leased_video_textures.contains_key(&lease_key)
            || self.deferred_video_texture_releases.contains(&lease_key)
        {
            return;
        }

        self.rendered_video_texture_release_providers
            .insert(lease_key, resource_provider.clone());
    }

    /// Помечает texture handle текущего поколения как deferred, если renderer держит lease.
    pub(crate) fn request_video_texture_release(
        &mut self,
        resource_handle: video_core::FrameResourceHandle,
    ) -> VideoTextureReleaseEffect {
        let lease_key = (self.render_generation, resource_handle.0);
        if self.leased_video_textures.contains_key(&lease_key) {
            self.deferred_video_texture_releases.insert(lease_key);
            return VideoTextureReleaseEffect::DeferredUntilRenderLeaseDrop;
        }

        if let Some(resource_provider) = self
            .rendered_video_texture_release_providers
            .remove(&lease_key)
        {
            return VideoTextureReleaseEffect::ReleaseViaRenderProvider(resource_provider);
        }

        VideoTextureReleaseEffect::ReleaseNow
    }

    /// Возвращает количество texture handles, удерживаемых render leases.
    #[must_use]
    pub(crate) fn active_render_lease_count(&self) -> usize {
        self.leased_video_textures.len()
    }

    /// Проверяет, отложен ли release конкретного texture handle текущего поколения.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_deferred_video_texture_release(
        &self,
        resource_handle: video_core::FrameResourceHandle,
    ) -> bool {
        self.deferred_video_texture_releases
            .contains(&(self.render_generation, resource_handle.0))
    }

    /// Возвращает количество отложенных texture releases.
    #[must_use]
    pub(crate) fn deferred_render_release_count(&self) -> usize {
        self.deferred_video_texture_releases.len()
    }
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };

    use super::*;
    use codec_core::VideoCodec;
    use media_core::{MediaTime, TrackKind};

    /// Создаёт decoded frame без реальных GPU resources для проверки pipeline storage.
    fn decoded_frame_for_tests(pts: Duration, resource_handle: u64) -> video_core::DecodedFrame {
        video_core::DecodedFrame {
            generation: 0,
            pts,
            frame_contract: video_frame_contract::VideoFrameContract::dma_buf_nv12(
                video_frame_contract::DmaBufImageLayout::SeparateLayers,
            ),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: video_core::FrameResourceHandle(resource_handle),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    /// Управляемый fake decoder для проверки audio boundary без CPAL и codec side effects.
    struct FakeAudioDecoder {
        /// Результат, который fake вернёт из `decode`.
        decode_outcome: FakeAudioDecodeOutcome,

        /// Ошибка, которую fake вернёт из `reset`, если она задана.
        reset_error: Option<&'static str>,

        /// Sample rate, который boundary должен вернуть вместе с decoded samples.
        sample_rate: u32,

        /// Channel count, который boundary должен вернуть вместе с decoded samples.
        channels: u32,
    }

    impl FakeAudioDecoder {
        /// Создаёт fake decoder, который успешно возвращает заданные PCM samples.
        fn with_samples(samples: Vec<f32>, sample_rate: u32, channels: u32) -> Self {
            Self {
                decode_outcome: FakeAudioDecodeOutcome::Samples(samples),
                reset_error: None,
                sample_rate,
                channels,
            }
        }

        /// Создаёт fake decoder, который падает на decode и успешно reset-ится.
        fn with_decode_error(error: &'static str) -> Self {
            Self {
                decode_outcome: FakeAudioDecodeOutcome::Error(error),
                reset_error: None,
                sample_rate: 48_000,
                channels: 2,
            }
        }

        /// Создаёт fake decoder, который decode-ит пустой packet и падает на reset.
        fn with_reset_error(error: &'static str) -> Self {
            Self {
                decode_outcome: FakeAudioDecodeOutcome::Samples(Vec::new()),
                reset_error: Some(error),
                sample_rate: 48_000,
                channels: 2,
            }
        }
    }

    /// Явный сценарий fake decode, чтобы тесты не полагались на magic flags.
    enum FakeAudioDecodeOutcome {
        /// Успешный decode с предсказуемыми samples.
        Samples(Vec<f32>),

        /// Ошибка decode с предсказуемым текстом.
        Error(&'static str),
    }

    impl audio_core::AudioDecoder for FakeAudioDecoder {
        /// Возвращает заранее заданный результат decode.
        fn decode(
            &mut self,
            _packet: &audio_core::EncodedAudioPacket<'_>,
        ) -> anyhow::Result<Vec<f32>> {
            match &self.decode_outcome {
                FakeAudioDecodeOutcome::Samples(samples) => Ok(samples.clone()),
                FakeAudioDecodeOutcome::Error(error) => Err(anyhow::anyhow!(*error)),
            }
        }

        /// Возвращает reset error только если тест явно его сконфигурировал.
        fn reset(&mut self) -> anyhow::Result<()> {
            match self.reset_error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => Ok(()),
            }
        }

        /// Возвращает sample rate fake decoder-а.
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        /// Возвращает channel count fake decoder-а.
        fn channels(&self) -> u32 {
            self.channels
        }
    }

    /// Fake clock для проверки нейтрального audio clock boundary без concrete audio crate clock.
    struct FixedAudioClock {
        /// Текущее значение, которое clock отдаёт pipeline.
        now: Mutex<Duration>,

        /// Счётчик reset-вызовов, чтобы тест видел side effect boundary.
        reset_count: AtomicUsize,

        /// Scripted счётчик underrun callbacks.
        underrun_callbacks: AtomicU64,
    }

    impl FixedAudioClock {
        /// Создаёт clock с заданными observable значениями.
        fn new(now: Duration, underrun_callbacks: u64) -> Self {
            Self {
                now: Mutex::new(now),
                reset_count: AtomicUsize::new(0),
                underrun_callbacks: AtomicU64::new(underrun_callbacks),
            }
        }

        /// Возвращает количество reset-вызовов.
        fn reset_count(&self) -> usize {
            self.reset_count.load(Ordering::Relaxed)
        }
    }

    impl PlayerAudioClock for FixedAudioClock {
        /// Возвращает scripted playback позицию.
        fn now(&self) -> Duration {
            *self
                .now
                .lock()
                .expect("fake clock mutex не должен ломаться")
        }

        /// Сбрасывает позицию и отмечает reset-вызов.
        fn reset(&self) {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
            *self
                .now
                .lock()
                .expect("fake clock mutex не должен ломаться") = Duration::ZERO;
        }

        /// Возвращает scripted underrun count.
        fn underrun_callbacks(&self) -> u64 {
            self.underrun_callbacks.load(Ordering::Relaxed)
        }
    }

    /// Fake output с управляемым buffer level для проверки EOF-drain boundary.
    struct FixedAudioOutput {
        /// Нейтральный fake clock output-а.
        clock: Arc<FixedAudioClock>,

        /// Уровень buffer-а, который вернёт output boundary.
        buffer_level_ms: f64,

        /// Ошибка play, если сценарий проверяет propagation.
        play_error: Option<&'static str>,

        /// Ошибка pause, если сценарий проверяет propagation.
        pause_error: Option<&'static str>,

        /// Последний volume, который pipeline передал output boundary.
        last_volume: Arc<Mutex<Option<f32>>>,
    }

    impl FixedAudioOutput {
        /// Создаёт fake output с заданным уровнем buffer-а.
        fn new(buffer_level_ms: f64) -> Self {
            Self {
                clock: Arc::new(FixedAudioClock::new(Duration::ZERO, 0)),
                buffer_level_ms,
                play_error: None,
                pause_error: None,
                last_volume: Arc::new(Mutex::new(None)),
            }
        }

        /// Создаёт fake output с scripted play/pause errors.
        fn with_errors(
            play_error: Option<&'static str>,
            pause_error: Option<&'static str>,
        ) -> Self {
            Self {
                play_error,
                pause_error,
                ..Self::new(0.0)
            }
        }

        /// Возвращает clock handle до передачи output-а в pipeline.
        fn clock_handle(&self) -> Arc<FixedAudioClock> {
            Arc::clone(&self.clock)
        }

        /// Возвращает volume log handle до передачи output-а в pipeline.
        fn volume_handle(&self) -> Arc<Mutex<Option<f32>>> {
            Arc::clone(&self.last_volume)
        }
    }

    impl PlayerAudioOutput for FixedAudioOutput {
        /// Записывает все samples как успешно принятые.
        fn write_samples(&mut self, samples: &[f32]) -> u64 {
            samples.len() as u64
        }

        /// Fake stream всегда успешно стартует.
        fn play(&mut self) -> anyhow::Result<()> {
            match self.play_error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => Ok(()),
            }
        }

        /// Fake stream всегда успешно ставится на паузу.
        fn pause(&mut self) -> anyhow::Result<()> {
            match self.pause_error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => Ok(()),
            }
        }

        /// Возвращает тот же generation, который запросил caller.
        fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
            Ok(generation)
        }

        /// Volume не влияет на EOF-drain state.
        fn set_volume(&mut self, volume: f32) {
            *self
                .last_volume
                .lock()
                .expect("fake volume mutex не должен ломаться") = Some(volume);
        }

        /// Возвращает scripted buffer level.
        fn buffer_level_ms(&self) -> f64 {
            self.buffer_level_ms
        }

        /// Возвращает fake clock для соблюдения output contract-а.
        fn clock(&self) -> Arc<dyn PlayerAudioClock> {
            let clock: Arc<dyn PlayerAudioClock> = self.clock.clone();
            clock
        }
    }

    #[test]
    fn queued_video_frame_methods_preserve_fifo_order_and_len() {
        let mut pipeline = PlaybackPipeline::default();

        assert!(pipeline.video_present_queue_is_empty());
        assert_eq!(pipeline.video_present_queue_len(), 0);

        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));

        assert_eq!(pipeline.video_present_queue_len(), 2);
        assert_eq!(
            pipeline.front_queued_video_frame().map(|frame| frame.pts),
            Some(Duration::from_millis(16))
        );
        assert_eq!(
            pipeline
                .front_and_next_queued_video_frames()
                .map(|(front_frame, next_frame)| (front_frame.pts, next_frame.pts)),
            Some((Duration::from_millis(16), Duration::from_millis(33)))
        );

        assert_eq!(
            pipeline
                .pop_queued_video_frame_front()
                .map(|frame| frame.resource_handle),
            Some(video_core::FrameResourceHandle(1))
        );
        assert_eq!(
            pipeline
                .pop_queued_video_frame_front()
                .map(|frame| frame.resource_handle),
            Some(video_core::FrameResourceHandle(2))
        );
        assert!(pipeline.pop_queued_video_frame_front().is_none());
        assert!(pipeline.video_present_queue_is_empty());
    }

    #[test]
    fn queued_video_frame_covers_target_for_generation_reports_only_matching_frames() {
        let mut pipeline = PlaybackPipeline::default();
        let target_position = Duration::from_millis(100);
        let active_generation = 7;
        let stale_generation = 6;

        assert!(
            !pipeline.queued_video_frame_covers_target_for_generation(
                target_position,
                active_generation
            )
        );

        let mut pretarget_frame = decoded_frame_for_tests(Duration::from_millis(90), 1);
        pretarget_frame.generation = active_generation;
        pipeline.enqueue_queued_video_frame(pretarget_frame);

        assert!(
            !pipeline.queued_video_frame_covers_target_for_generation(
                target_position,
                active_generation
            )
        );

        let mut stale_target_frame = decoded_frame_for_tests(Duration::from_millis(100), 2);
        stale_target_frame.generation = stale_generation;
        pipeline.enqueue_queued_video_frame(stale_target_frame);

        assert!(
            !pipeline.queued_video_frame_covers_target_for_generation(
                target_position,
                active_generation
            )
        );

        let mut active_target_frame = decoded_frame_for_tests(Duration::from_millis(100), 3);
        active_target_frame.generation = active_generation;
        pipeline.enqueue_queued_video_frame(active_target_frame);

        assert!(
            pipeline.queued_video_frame_covers_target_for_generation(
                target_position,
                active_generation
            )
        );
    }

    #[test]
    fn present_video_frame_methods_keep_replacement_ownership_explicit() {
        let mut pipeline = PlaybackPipeline::default();

        assert!(!pipeline.has_present_video_frame());
        assert!(pipeline.present_video_frame().is_none());
        assert!(pipeline.take_present_video_frame().is_none());

        pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(10), 10));

        assert!(pipeline.has_present_video_frame());
        assert_eq!(
            pipeline.present_video_frame().map(|frame| frame.pts),
            Some(Duration::from_millis(10))
        );

        let replaced_frame = pipeline
            .replace_present_video_frame(decoded_frame_for_tests(Duration::from_millis(20), 20));

        assert_eq!(
            replaced_frame.map(|frame| frame.resource_handle),
            Some(video_core::FrameResourceHandle(10))
        );
        assert_eq!(
            pipeline
                .take_present_video_frame()
                .map(|frame| frame.resource_handle),
            Some(video_core::FrameResourceHandle(20))
        );
        assert!(!pipeline.has_present_video_frame());
    }

    #[test]
    fn opened_media_boundary_methods_expose_installed_source_slots() {
        let mut pipeline = PlaybackPipeline::default();
        let track_infos = vec![
            source_slot_track(TrackId::new(1), TrackKind::Video, "V_VP9"),
            source_slot_track(TrackId::new(2), TrackKind::Audio, "A_OPUS"),
        ];

        assert!(!pipeline.has_demuxer());
        assert_eq!(pipeline.track_count(), 0);

        pipeline.install_opened_media(
            Box::new(SourceSlotFakeDemuxer::new(track_infos.clone())),
            Some(PathBuf::from("/tmp/source.webm")),
            Some("external source".to_owned()),
            track_infos,
        );

        assert!(pipeline.has_demuxer());
        assert_eq!(
            pipeline.source_file_path(),
            Some(Path::new("/tmp/source.webm"))
        );
        assert_eq!(pipeline.source_label(), Some("external source"));
        assert_eq!(pipeline.track_count(), 2);
        assert_eq!(pipeline.tracks()[0].id, TrackId::new(1));
        assert_eq!(pipeline.tracks()[1].kind, TrackKind::Audio);
    }

    #[test]
    fn demux_track_list_update_invalidates_decoder_dependent_state() {
        let mut pipeline = PlaybackPipeline::default();
        let old_video_track = TrackId::new(1);
        let old_audio_track = TrackId::new(2);
        let new_audio_track = TrackId::new(3);
        let initial_tracks = vec![
            source_slot_track(old_video_track, TrackKind::Video, "V_VP9"),
            source_slot_track(old_audio_track, TrackKind::Audio, "A_OPUS"),
        ];

        pipeline.install_opened_media(
            Box::new(SourceSlotFakeDemuxer::new(initial_tracks.clone())),
            None,
            None,
            initial_tracks,
        );
        pipeline.select_video_track(
            old_video_track,
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        pipeline.select_audio_track(old_audio_track);
        pipeline.install_deferred_audio_decoder_config(
            audio_core::AudioDecoderConfig::from_track_metadata(
                old_audio_track.get(),
                "A_OPUS",
                Some(48_000),
                Some(2),
            ),
        );
        pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
            vec![0.0],
            48_000,
            2,
        )));
        pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
            old_audio_track,
            Duration::ZERO,
            None,
            None,
            pipeline.seek_generation(),
            Bytes::from_static(b"old audio"),
        ));
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            old_video_track,
            Duration::ZERO,
            pipeline.seek_generation(),
            Bytes::from_static(b"old video"),
            true,
        ));
        pipeline.mark_video_decoder_bootstrapped();
        pipeline.note_video_packet_sent_to_decoder();

        pipeline.apply_demux_track_list_update(vec![source_slot_track(
            new_audio_track,
            TrackKind::Audio,
            "A_AAC",
        )]);

        assert_eq!(pipeline.tracks()[0].id, new_audio_track);
        assert!(pipeline.selected_audio_track_id().is_none());
        assert!(pipeline.selected_video_track_id().is_none());
        assert!(!pipeline.has_audio_decoder());
        assert!(!pipeline.has_deferred_audio_decoder_config());
        assert_eq!(pipeline.pending_audio_packet_len(), 0);
        assert_eq!(pipeline.pending_video_packet_len(), 0);
        assert!(!pipeline.video_decoder_needs_keyframe());
        assert_eq!(pipeline.video_decode_in_flight_packets(), 0);
    }

    #[test]
    fn audio_decoder_boundaries_preserve_absent_success_and_error_states() {
        let mut pipeline = PlaybackPipeline::default();
        let packet =
            audio_core::EncodedAudioPacket::without_timing(TrackId::new(2).get(), b"packet");
        let bad_packet =
            audio_core::EncodedAudioPacket::without_timing(TrackId::new(2).get(), b"bad packet");

        assert!(!pipeline.has_audio_decoder());
        assert!(pipeline.decode_audio_packet(&packet).is_none());
        assert!(pipeline.reset_audio_decoder().is_none());

        pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
            vec![0.25, -0.25],
            44_100,
            2,
        )));

        let decoded_audio = pipeline
            .decode_audio_packet(&packet)
            .expect("installed decoder должен вернуть decode result")
            .expect("successful fake decoder не должен падать");
        assert_eq!(decoded_audio.samples, vec![0.25, -0.25]);
        assert_eq!(decoded_audio.sample_rate, 44_100);
        assert_eq!(decoded_audio.channels, 2);

        pipeline.clear_audio_decoder();
        assert!(!pipeline.has_audio_decoder());

        pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_decode_error(
            "decode failed",
        )));

        let decode_error = pipeline
            .decode_audio_packet(&bad_packet)
            .expect("installed decoder должен сохранить decode error")
            .expect_err("decode error должен дойти до session boundary");
        assert_eq!(decode_error.to_string(), "decode failed");

        pipeline.clear_audio_decoder();
        pipeline
            .install_audio_decoder(Box::new(FakeAudioDecoder::with_reset_error("reset failed")));

        let reset_error = pipeline
            .reset_audio_decoder()
            .expect("installed decoder должен вернуть reset result")
            .expect_err("reset error должен дойти до session boundary");
        assert_eq!(reset_error.to_string(), "reset failed");
    }

    #[test]
    fn deferred_audio_decoder_config_boundary_preserves_absent_match_and_mismatch() {
        let mut pipeline = PlaybackPipeline::default();
        let config = audio_core::AudioDecoderConfig::from_track_metadata(
            TrackId::new(2).get(),
            "A_AAC",
            None,
            None,
        );

        assert!(!pipeline.has_deferred_audio_decoder_config());
        assert!(
            pipeline
                .take_deferred_audio_decoder_config(TrackId::new(2))
                .is_none()
        );

        pipeline.install_deferred_audio_decoder_config(config.clone());
        assert!(pipeline.has_deferred_audio_decoder_config());
        assert!(
            pipeline
                .take_deferred_audio_decoder_config(TrackId::new(3))
                .is_none()
        );
        assert!(pipeline.has_deferred_audio_decoder_config());

        let taken_config = pipeline
            .take_deferred_audio_decoder_config(TrackId::new(2))
            .expect("matching track should consume deferred decoder config");
        assert_eq!(taken_config, config);
        assert!(!pipeline.has_deferred_audio_decoder_config());
    }

    #[test]
    fn audio_seek_runtime_state_classifies_slots_without_cpal_output() {
        assert_eq!(
            audio_seek_runtime_state_from_slots(false, false, false),
            AudioSeekRuntimeState::NoSelectedAudio
        );
        assert_eq!(
            audio_seek_runtime_state_from_slots(true, false, false),
            AudioSeekRuntimeState::WaitingForDecoder
        );
        assert_eq!(
            audio_seek_runtime_state_from_slots(true, true, false),
            AudioSeekRuntimeState::WaitingForOutput
        );
        assert_eq!(
            audio_seek_runtime_state_from_slots(true, true, true),
            AudioSeekRuntimeState::Ready
        );
    }

    #[test]
    fn audio_seek_runtime_state_boundary_keeps_selection_ownership() {
        let mut pipeline = PlaybackPipeline::default();
        let track_id = TrackId::new(2);
        let decoder_config = audio_core::AudioDecoderConfig::from_track_metadata(
            track_id.get(),
            "A_OPUS",
            None,
            None,
        );

        assert_eq!(
            pipeline.audio_seek_runtime_state(),
            AudioSeekRuntimeState::NoSelectedAudio
        );

        pipeline.select_audio_track(track_id);
        assert_eq!(
            pipeline.audio_seek_runtime_state(),
            AudioSeekRuntimeState::WaitingForDecoder
        );

        pipeline.install_deferred_audio_decoder_config(decoder_config);
        assert_eq!(
            pipeline.audio_seek_runtime_state(),
            AudioSeekRuntimeState::WaitingForDecoder
        );
        assert_eq!(pipeline.selected_audio_track_id(), Some(track_id));

        pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
            vec![0.0, 0.0],
            48_000,
            2,
        )));
        assert_eq!(
            pipeline.audio_seek_runtime_state(),
            AudioSeekRuntimeState::WaitingForOutput
        );
        assert_eq!(pipeline.selected_audio_track_id(), Some(track_id));
    }

    #[test]
    fn absent_audio_output_boundaries_are_noop_without_losing_absent_state() {
        let mut pipeline = PlaybackPipeline::default();

        assert!(!pipeline.has_audio_output());
        assert_eq!(pipeline.write_audio_output_samples(&[0.0, 0.1]), None);
        assert!(pipeline.play_audio_output().is_none());
        assert!(pipeline.pause_audio_output().is_none());
        assert!(pipeline.clear_audio_output_for_seek(1).is_none());
        assert!(pipeline.audio_output_buffer_level_ms().is_none());
        assert!(pipeline.audio_output_clock().is_none());
        assert_eq!(
            pipeline.audio_eof_drain_state(),
            AudioEofDrainState::NoSelectedAudio
        );

        pipeline.clear_audio_output();
        assert!(!pipeline.has_audio_output());
    }

    #[test]
    fn installed_audio_output_boundary_forwards_calls_and_neutral_clock() {
        let mut pipeline = PlaybackPipeline::default();
        let output = FixedAudioOutput::new(42.0);
        let clock = output.clock_handle();
        let volume = output.volume_handle();

        pipeline.install_audio_output_for_tests(Box::new(output));

        assert!(pipeline.has_audio_output());
        assert!(pipeline.audio_output_clock().is_some());
        assert_eq!(pipeline.write_audio_output_samples(&[0.1, -0.1]), Some(2));
        assert_eq!(pipeline.audio_output_buffer_level_ms(), Some(42.0));
        assert_eq!(
            pipeline
                .clear_audio_output_for_seek(7)
                .expect("installed output должен вернуть clear result")
                .expect("fake clear должен быть успешным"),
            7
        );
        assert!(pipeline.set_audio_output_volume(0.25));
        assert_eq!(
            *volume.lock().expect("fake volume mutex не должен ломаться"),
            Some(0.25)
        );

        pipeline.install_audio_clock(clock);
        assert!(pipeline.reset_audio_clock());
    }

    #[test]
    fn audio_output_boundary_preserves_play_pause_errors() {
        let mut pipeline = PlaybackPipeline::default();

        pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::with_errors(
            Some("play failed"),
            Some("pause failed"),
        )));

        let play_error = pipeline
            .play_audio_output()
            .expect("installed output должен вернуть play result")
            .expect_err("play error должен пройти через boundary");
        assert_eq!(play_error.to_string(), "play failed");

        let pause_error = pipeline
            .pause_audio_output()
            .expect("installed output должен вернуть pause result")
            .expect_err("pause error должен пройти через boundary");
        assert_eq!(pause_error.to_string(), "pause failed");
    }

    #[test]
    fn audio_eof_drain_state_preserves_queue_output_and_playback_distinctions() {
        let mut pipeline = PlaybackPipeline::default();
        let audio_track_id = TrackId::new(2);

        pipeline.select_audio_track(audio_track_id);
        assert_eq!(
            pipeline.audio_eof_drain_state(),
            AudioEofDrainState::NoOutput
        );

        pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
            audio_track_id,
            Duration::ZERO,
            None,
            Some(Duration::from_millis(20)),
            pipeline.seek_generation(),
            Bytes::from_static(b"encoded-audio"),
        ));
        assert_eq!(
            pipeline.audio_eof_drain_state(),
            AudioEofDrainState::PendingPackets { queued_packets: 1 }
        );

        let _pending_packet = pipeline.pop_pending_audio_packet_front();
        pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(24.0)));
        assert_eq!(
            pipeline.audio_eof_drain_state(),
            AudioEofDrainState::DrainingOutput {
                buffer_level_ms: 24.0,
                playback_requested: false,
            }
        );

        pipeline
            .play_audio_output()
            .expect("installed output должен вернуть play result")
            .expect("fake output play должен быть успешным");
        assert_eq!(
            pipeline.audio_eof_drain_state(),
            AudioEofDrainState::DrainingOutput {
                buffer_level_ms: 24.0,
                playback_requested: true,
            }
        );

        pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(0.0)));
        assert_eq!(
            pipeline.audio_eof_drain_state(),
            AudioEofDrainState::DrainedOutput {
                playback_requested: false,
            }
        );
    }

    #[test]
    fn audio_buffer_clear_generation_boundary_records_ack_generation() {
        let mut pipeline = PlaybackPipeline::default();

        assert_eq!(pipeline.audio_buffer_clear_generation(), 0);

        pipeline.mark_audio_buffer_clear_ack(7);

        assert_eq!(pipeline.audio_buffer_clear_generation(), 7);
    }

    #[test]
    fn no_audio_monotonic_fallback_counts_position_from_anchor() {
        let mut pipeline = PlaybackPipeline::default();
        let anchored_at = Instant::now();
        let initial_position = Duration::from_millis(100);

        pipeline.start_monotonic_media_clock(initial_position, anchored_at);

        assert_eq!(
            pipeline.monotonic_media_position(anchored_at + Duration::from_millis(40)),
            Some(Duration::from_millis(140))
        );
    }

    #[test]
    fn installing_audio_clock_clears_monotonic_fallback_anchor() {
        let mut pipeline = PlaybackPipeline::default();
        let anchored_at = Instant::now();
        let clock = Arc::new(FixedAudioClock::new(Duration::from_millis(12), 3));

        pipeline.start_monotonic_media_clock(Duration::from_secs(3), anchored_at);
        assert!(pipeline.monotonic_media_position(anchored_at).is_some());

        pipeline.install_audio_clock(Arc::clone(&clock) as Arc<dyn PlayerAudioClock>);

        assert!(pipeline.has_audio_clock());
        assert_eq!(pipeline.audio_clock_now(), Duration::from_millis(12));
        assert_eq!(pipeline.audio_clock_underrun_callbacks(), 3);
        assert!(pipeline.monotonic_media_position(anchored_at).is_none());
        assert!(pipeline.reset_audio_clock());
        assert_eq!(clock.reset_count(), 1);
        assert_eq!(pipeline.audio_clock_now(), Duration::ZERO);
    }

    #[test]
    fn seek_clock_reset_restores_base_and_clears_fallback_sample_window() {
        let mut pipeline = PlaybackPipeline::default();
        let anchored_at = Instant::now();
        let target_position = Duration::from_secs(9);

        pipeline.set_media_clock_base(Duration::from_secs(2));
        pipeline.start_monotonic_media_clock(Duration::from_secs(4), anchored_at);
        pipeline.reset_audio_clock_sample(Duration::from_secs(3), anchored_at);

        pipeline.reset_clocks_for_seek(target_position);

        assert_eq!(pipeline.media_clock_base(), target_position);
        assert!(pipeline.monotonic_media_position(anchored_at).is_none());
        assert!(pipeline.audio_clock_stalled_for(Instant::now()) < Duration::from_secs(1));
    }

    #[test]
    fn stalled_audio_duration_is_measured_from_last_changed_sample() {
        let mut pipeline = PlaybackPipeline::default();
        let first_observed_at = Instant::now();
        let unchanged_observed_at = first_observed_at + Duration::from_millis(20);
        let changed_observed_at = first_observed_at + Duration::from_millis(40);

        pipeline.reset_audio_clock_sample(Duration::ZERO, first_observed_at);
        pipeline.note_audio_clock_sample(Duration::ZERO, unchanged_observed_at);

        assert_eq!(
            pipeline.audio_clock_stalled_for(first_observed_at + Duration::from_millis(30)),
            Duration::from_millis(30)
        );

        pipeline.note_audio_clock_sample(Duration::from_millis(5), changed_observed_at);

        assert_eq!(
            pipeline.audio_clock_stalled_for(changed_observed_at + Duration::from_millis(15)),
            Duration::from_millis(15)
        );
    }

    #[test]
    fn demux_boundaries_preserve_eof_and_seek_results() {
        let mut pipeline = PlaybackPipeline::default();
        pipeline.install_opened_media(
            Box::new(SourceSlotFakeDemuxer::new(Vec::new())),
            None,
            None,
            Vec::new(),
        );

        let packet_result = pipeline
            .demux_next_packet()
            .expect("installed demuxer должен быть видим через boundary")
            .expect("fake demuxer не должен возвращать ошибку");
        assert!(packet_result.is_none());

        let seek_result = pipeline
            .seek_demuxer(DemuxSeekRequest::accurate(Duration::from_secs(3)))
            .expect("installed demuxer должен принять seek через boundary")
            .expect("fake demuxer должен принять accurate seek");
        assert_eq!(seek_result.actual_position, MediaTime::from_secs(3));
    }

    #[test]
    fn selected_track_boundaries_manage_ids_requirement_and_clear_only_selection() {
        let mut pipeline = PlaybackPipeline::default();
        let video_track_id = TrackId::new(10);
        let audio_track_id = TrackId::new(20);
        let source_tracks = vec![
            source_slot_track(video_track_id, TrackKind::Video, "V_VP9"),
            source_slot_track(audio_track_id, TrackKind::Audio, "A_OPUS"),
        ];
        let initial_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
        let refined_requirement = initial_requirement.clone().with_resolution(1920, 1080);

        pipeline.install_opened_media(
            Box::new(SourceSlotFakeDemuxer::new(source_tracks.clone())),
            None,
            Some("selected-track-test".to_owned()),
            source_tracks,
        );
        pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
            audio_track_id,
            Duration::from_millis(1),
            None,
            None,
            pipeline.seek_generation(),
            Bytes::from_static(b"audio"),
        ));
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            video_track_id,
            Duration::from_millis(2),
            pipeline.seek_generation(),
            Bytes::from_static(b"video"),
            true,
        ));

        pipeline.select_audio_track(audio_track_id);
        pipeline.select_video_track(video_track_id, initial_requirement.clone());

        assert_eq!(pipeline.selected_audio_track_id(), Some(audio_track_id));
        assert_eq!(pipeline.selected_video_track_id(), Some(video_track_id));
        assert!(pipeline.has_selected_audio_track());
        assert!(pipeline.has_selected_video_track());
        assert!(pipeline.video_packet_belongs_to_selected_track(video_track_id));
        assert!(!pipeline.video_packet_belongs_to_selected_track(TrackId::new(99)));
        assert_eq!(
            pipeline.active_video_requirement(),
            Some(&initial_requirement)
        );

        pipeline.set_active_video_requirement(refined_requirement.clone());

        assert_eq!(
            pipeline.active_video_requirement(),
            Some(&refined_requirement)
        );

        pipeline.clear_selected_tracks();

        assert!(pipeline.selected_audio_track_id().is_none());
        assert!(pipeline.selected_video_track_id().is_none());
        assert!(!pipeline.has_selected_audio_track());
        assert!(!pipeline.has_selected_video_track());
        assert!(pipeline.active_video_requirement().is_none());
        assert_eq!(pipeline.pending_audio_packet_len(), 1);
        assert_eq!(pipeline.pending_video_packet_len(), 1);
        assert!(pipeline.has_demuxer());
        assert_eq!(pipeline.track_count(), 2);
    }

    #[test]
    fn video_frame_timing_first_observation_only_records_pts() {
        let mut pipeline = PlaybackPipeline::default();

        pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));

        assert_eq!(
            pipeline.video_frame_duration_estimate(),
            DEFAULT_VIDEO_FRAME_DURATION
        );
    }

    #[test]
    fn video_frame_timing_valid_delta_updates_estimate_with_legacy_smoothing() {
        let mut pipeline = PlaybackPipeline::default();
        let observed_frame_duration = Duration::from_millis(20);

        pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));
        pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration);

        let old_micros = DEFAULT_VIDEO_FRAME_DURATION.as_micros() as u64;
        let observed_micros = observed_frame_duration.as_micros() as u64;
        let expected_micros = (old_micros.saturating_mul(7) + observed_micros) / 8;

        assert_eq!(
            pipeline.video_frame_duration_estimate(),
            Duration::from_micros(expected_micros.max(1))
        );
    }

    #[test]
    fn video_frame_timing_ignores_out_of_range_deltas() {
        let mut pipeline = PlaybackPipeline::default();
        let first_pts = Duration::from_secs(10);
        let too_small_pts = first_pts + MIN_OBSERVED_VIDEO_FRAME_DURATION / 2;
        let too_large_pts = too_small_pts + MAX_OBSERVED_VIDEO_FRAME_DURATION * 2;

        pipeline.observe_decoded_video_frame_pts(first_pts);
        pipeline.observe_decoded_video_frame_pts(too_small_pts);
        pipeline.observe_decoded_video_frame_pts(too_large_pts);

        assert_eq!(
            pipeline.video_frame_duration_estimate(),
            DEFAULT_VIDEO_FRAME_DURATION
        );
    }

    #[test]
    fn video_frame_timing_reset_restores_default_and_clears_previous_pts() {
        let mut pipeline = PlaybackPipeline::default();
        let observed_frame_duration = Duration::from_millis(20);

        pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));
        pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration);
        assert_ne!(
            pipeline.video_frame_duration_estimate(),
            DEFAULT_VIDEO_FRAME_DURATION
        );

        pipeline.reset_video_frame_timing_estimator();
        pipeline
            .observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration * 2);

        assert_eq!(
            pipeline.video_frame_duration_estimate(),
            DEFAULT_VIDEO_FRAME_DURATION
        );
    }

    #[test]
    fn pending_packet_queue_boundaries_preserve_fifo_order_and_lengths() {
        let mut pipeline = PlaybackPipeline::default();
        let audio_track_id = TrackId::new(20);
        let video_track_id = TrackId::new(10);
        let generation = pipeline.seek_generation();

        assert!(pipeline.pending_audio_packet_is_empty());
        assert!(pipeline.pending_video_packet_is_empty());

        pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
            audio_track_id,
            Duration::from_millis(10),
            None,
            None,
            generation,
            Bytes::from_static(b"audio-10"),
        ));
        pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
            audio_track_id,
            Duration::from_millis(20),
            None,
            None,
            generation,
            Bytes::from_static(b"audio-20"),
        ));
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            video_track_id,
            Duration::from_millis(30),
            generation,
            Bytes::from_static(b"video-30"),
            true,
        ));
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            video_track_id,
            Duration::from_millis(40),
            generation,
            Bytes::from_static(b"video-40"),
            false,
        ));

        assert_eq!(pipeline.pending_audio_packet_len(), 2);
        assert_eq!(pipeline.pending_video_packet_len(), 2);
        assert_eq!(
            pipeline
                .front_pending_video_packet()
                .map(|packet| packet.pts),
            Some(Duration::from_millis(30))
        );
        assert_eq!(
            pipeline
                .pop_pending_audio_packet_front()
                .map(|packet| packet.pts),
            Some(Duration::from_millis(10))
        );
        assert_eq!(
            pipeline
                .pop_pending_audio_packet_front()
                .map(|packet| packet.pts),
            Some(Duration::from_millis(20))
        );
        assert_eq!(
            pipeline
                .pop_pending_video_packet_front()
                .map(|packet| packet.pts),
            Some(Duration::from_millis(30))
        );
        assert_eq!(
            pipeline
                .pop_pending_video_packet_front()
                .map(|packet| packet.pts),
            Some(Duration::from_millis(40))
        );
        assert!(pipeline.pending_audio_packet_is_empty());
        assert!(pipeline.pending_video_packet_is_empty());
    }

    #[test]
    fn begin_seek_generation_saturates_without_wrapping() {
        let mut pipeline = PlaybackPipeline::default();

        pipeline.set_seek_generation_for_tests(u64::MAX - 1);

        assert_eq!(pipeline.begin_seek_generation(), u64::MAX);
        assert_eq!(pipeline.seek_generation(), u64::MAX);
        assert_eq!(pipeline.begin_seek_generation(), u64::MAX);
        assert_eq!(pipeline.seek_generation(), u64::MAX);
        assert!(pipeline.packet_generation_is_current(u64::MAX));
        assert!(!pipeline.packet_generation_is_current(u64::MAX - 1));
    }

    #[test]
    fn clear_pending_packets_for_seek_does_not_touch_selection_or_decoder_state() {
        let mut pipeline = PlaybackPipeline::default();
        let video_track_id = TrackId::new(10);
        let audio_track_id = TrackId::new(20);
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
        let generation = pipeline.seek_generation();

        pipeline.select_audio_track(audio_track_id);
        pipeline.select_video_track(video_track_id, requirement.clone());
        pipeline.mark_video_decoder_bootstrapped();
        pipeline.note_video_packet_sent_to_decoder();
        pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
            audio_track_id,
            Duration::from_millis(10),
            None,
            None,
            generation,
            Bytes::from_static(b"audio"),
        ));
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            video_track_id,
            Duration::from_millis(20),
            generation,
            Bytes::from_static(b"video"),
            true,
        ));

        pipeline.clear_pending_packets_for_seek();

        assert!(pipeline.pending_audio_packet_is_empty());
        assert!(pipeline.pending_video_packet_is_empty());
        assert_eq!(pipeline.selected_audio_track_id(), Some(audio_track_id));
        assert_eq!(pipeline.selected_video_track_id(), Some(video_track_id));
        assert_eq!(pipeline.active_video_requirement(), Some(&requirement));
        assert!(!pipeline.video_decoder_needs_keyframe());
        assert_eq!(pipeline.video_decode_in_flight_packets(), 1);
    }

    #[test]
    fn clear_video_queues_returns_only_queued_resource_handles() {
        let mut pipeline = PlaybackPipeline::default();

        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));
        pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(50), 3));
        pipeline.replace_seek_preroll_fallback_video_frame(decoded_frame_for_tests(
            Duration::from_millis(40),
            4,
        ));

        let released_resource_handles = pipeline.clear_video_queues();

        assert_eq!(
            released_resource_handles,
            vec![
                video_core::FrameResourceHandle(1),
                video_core::FrameResourceHandle(2)
            ]
        );
        assert!(pipeline.video_present_queue_is_empty());
        assert_eq!(
            pipeline
                .present_video_frame()
                .map(|frame| frame.resource_handle),
            Some(video_core::FrameResourceHandle(3))
        );
        assert_eq!(
            pipeline
                .take_seek_preroll_fallback_video_frame()
                .map(|frame| frame.resource_handle),
            Some(video_core::FrameResourceHandle(4))
        );
    }

    /// Fake demuxer для проверки source-slot boundaries без реального container backend-а.
    struct SourceSlotFakeDemuxer {
        /// Metadata tracks, которые demuxer отдаёт по neutral contract.
        track_infos: Vec<TrackInfo>,
    }

    impl SourceSlotFakeDemuxer {
        /// Создаёт fake demuxer с фиксированным набором tracks.
        fn new(track_infos: Vec<TrackInfo>) -> Self {
            Self { track_infos }
        }
    }

    impl Demuxer for SourceSlotFakeDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.track_infos
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(30))
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
            Ok(None)
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    /// Создаёт минимальный track metadata для проверки source-slot getters.
    fn source_slot_track(track_id: TrackId, kind: TrackKind, codec_id: &str) -> TrackInfo {
        TrackInfo {
            id: track_id,
            kind,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: media_core::TimeBase::new(1, 1_000),
            duration: Some(Duration::from_secs(30)),
            sample_rate: (kind == TrackKind::Audio).then_some(48_000),
            channels: (kind == TrackKind::Audio).then_some(2),
            video: None,
        }
    }
}
