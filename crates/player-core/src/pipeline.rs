use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::VideoDecodeRequirement;
use media_core::{TrackId, TrackInfo};
use webm_demux::Demuxer;

/// Минимальный session-level контракт decoder thread-а, который нужен player-core.
///
/// Production backend остаётся `video_vaapi::VideoDecodeThread`, но session tests
/// могут подставить fake handle и проверить boundary без WGPU/VA-API ресурсов.
pub(crate) trait VideoDecoderThreadHandle: Send {
    /// Возвращает человекочитаемое имя backend-а для snapshot/diagnostics.
    fn backend_name(&self) -> &'static str;

    /// Отправляет encoded packet в decoder thread.
    fn send_packet(
        &self,
        packet: video_vaapi::DecodePacket,
    ) -> Result<(), video_vaapi::DecodeThreadSendError>;

    /// Освобождает texture/surface handle после presentation/drop.
    fn release_frame(&self, handle: video_core::FrameTextureHandle);

    /// Забирает следующий decoded frame без блокировки worker-а.
    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame>;

    /// Забирает backend diagnostics event без блокировки worker-а.
    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent>;

    /// Забирает fatal decoder-thread error, если backend остановился.
    fn try_recv_error(&self) -> Option<video_vaapi::DecodeThreadError>;

    /// Сбрасывает decoder state перед seek transaction.
    fn flush(&self) -> anyhow::Result<()>;

    /// Возвращает provider для renderer-side texture views/release path.
    fn texture_view_provider(&self) -> video_vaapi::VideoTextureViewProvider;

    /// Возвращает snapshot texture pool-а для UI/backpressure diagnostics.
    fn texture_pool_stats(&self) -> Option<video_vaapi::texture_cache::TexturePoolStats>;

    /// Возвращает глубину packet channel-а внутри decoder thread.
    fn packet_queue_depth(&self) -> usize;

    /// Забирает количество packets, обработанных decoder thread-ом.
    fn drain_completed_packet_count(&self) -> usize;
}

impl VideoDecoderThreadHandle for video_vaapi::VideoDecodeThread {
    fn backend_name(&self) -> &'static str {
        video_vaapi::VideoDecodeThread::backend_name(self)
    }

    fn send_packet(
        &self,
        packet: video_vaapi::DecodePacket,
    ) -> Result<(), video_vaapi::DecodeThreadSendError> {
        video_vaapi::VideoDecodeThread::send_packet(self, packet)
    }

    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        video_vaapi::VideoDecodeThread::release_frame(self, handle);
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        video_vaapi::VideoDecodeThread::try_recv_frame(self)
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        video_vaapi::VideoDecodeThread::try_recv_diagnostic_event(self)
    }

    fn try_recv_error(&self) -> Option<video_vaapi::DecodeThreadError> {
        video_vaapi::VideoDecodeThread::try_recv_error(self)
    }

    fn flush(&self) -> anyhow::Result<()> {
        video_vaapi::VideoDecodeThread::flush(self)
    }

    fn texture_view_provider(&self) -> video_vaapi::VideoTextureViewProvider {
        video_vaapi::VideoDecodeThread::texture_view_provider(self)
    }

    fn texture_pool_stats(&self) -> Option<video_vaapi::texture_cache::TexturePoolStats> {
        video_vaapi::VideoDecodeThread::texture_pool_stats(self)
    }

    fn packet_queue_depth(&self) -> usize {
        video_vaapi::VideoDecodeThread::packet_queue_depth(self)
    }

    fn drain_completed_packet_count(&self) -> usize {
        video_vaapi::VideoDecodeThread::drain_completed_packet_count(self)
    }
}

/// Bootstrap-оценка длительности frame до первых PTS observations; не worker cadence.
pub(crate) const DEFAULT_VIDEO_FRAME_DURATION: Duration = Duration::from_micros(16_667);

/// Минимальная разумная длительность кадра для оценки FPS.
pub(crate) const MIN_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(5);

/// Максимальная разумная длительность кадра для оценки FPS.
pub(crate) const MAX_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(100);

/// Anchor внутреннего monotonic media clock для media без audio clock.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MonotonicMediaClockAnchor {
    /// Media position, которая соответствовала `anchored_at`.
    media_position: Duration,

    /// Монотонный момент, от которого считается fallback media time.
    anchored_at: Instant,
}

impl MonotonicMediaClockAnchor {
    /// Создаёт anchor без привязки к video FPS или worker tick cadence.
    #[must_use]
    pub(crate) const fn new(media_position: Duration, anchored_at: Instant) -> Self {
        Self {
            media_position,
            anchored_at,
        }
    }

    /// Возвращает media position на заданный monotonic момент.
    #[must_use]
    pub(crate) fn position_at(self, now: Instant) -> Duration {
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

    /// Seek generation, в котором packet был прочитан из demuxer.
    pub(crate) generation: u64,

    /// Encoded audio bytes владеют shared payload-ом без копии между demuxer и player queue.
    pub(crate) encoded_bytes: Bytes,
}

impl PendingAudioPacket {
    /// Создаёт ожидающий audio packet с явным track id и codec bytes.
    #[must_use]
    pub(crate) fn new(
        track_id: TrackId,
        pts: Duration,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
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

    /// Seek generation, в котором packet был прочитан из demuxer.
    pub(crate) generation: u64,

    /// Encoded video bytes владеют shared payload-ом без копии до decoder thread.
    pub(crate) encoded_bytes: Bytes,

    /// Keyframe flag пробрасывается в hardware decoder.
    pub(crate) keyframe: bool,
}

impl PendingVideoPacket {
    /// Создаёт ожидающий video packet без неименованных tuple-полей.
    #[must_use]
    pub(crate) fn new(
        track_id: TrackId,
        pts: Duration,
        generation: u64,
        encoded_bytes: Bytes,
        keyframe: bool,
    ) -> Self {
        Self {
            track_id,
            pts,
            generation,
            encoded_bytes,
            keyframe,
        }
    }
}

/// Внутреннее владение media pipeline для текущей player session.
///
/// Поля остаются видимыми только внутри `player-core`, пока tick/scheduler
/// живут отдельным модулем. Наружный API работает через методы `PlayerSession`.
pub(crate) struct PlaybackPipeline {
    /// Demuxer текущего WebM/Matroska media.
    pub(crate) demuxer: Option<Box<dyn webm_demux::Demuxer + Send>>,

    /// Локальный путь, если media был открыт из файловой системы.
    pub(crate) file_path: Option<PathBuf>,

    /// Tracks текущего media без доступа UI к demuxer handle.
    pub(crate) tracks: Vec<TrackInfo>,

    /// User-facing label для streaming source без локального path.
    pub(crate) source_label: Option<String>,

    /// Audio decoder для Opus трека.
    pub(crate) audio_decoder: Option<audio::OpusDecoder>,

    /// Audio output: CPAL stream и ring buffer.
    pub(crate) audio_output: Option<audio::AudioOutput>,

    /// Track ID выбранного audio трека.
    pub(crate) audio_track_id: Option<TrackId>,

    /// Очередь сырых audio packets для throttle.
    pub(crate) pending_audio_packets: VecDeque<PendingAudioPacket>,

    /// Video decoder thread: backend decode в отдельном потоке за узким session contract.
    pub(crate) video_decoder_thread: Option<Box<dyn VideoDecoderThreadHandle>>,

    /// Требует ли decoder следующий video packet быть keyframe-ом.
    ///
    /// Stateless hardware decode после flush теряет reference frames. Если сразу
    /// отправить inter-frame, decoder может показать зелёные артефакты до
    /// следующего keyframe. Флаг держит этот codec contract рядом с очередью
    /// packets, а не в UI/render слое.
    pub(crate) video_decoder_needs_keyframe: bool,

    /// Сколько video packets уже ушло в decoder thread и ещё не получило packet ack.
    ///
    /// `VideoDecodeThread::packet_queue_depth()` не видит packet, который decoder
    /// уже забрал из channel и прямо сейчас обрабатывает. Decoder ack приходит
    /// по отдельному channel независимо от того, дал packet output frame или нет.
    pub(crate) video_decode_in_flight_packets: usize,

    /// Очередь декодированных видеокадров перед presentation.
    pub(crate) video_frame_queue: VecDeque<video_core::DecodedFrame>,

    /// Последний свежедекодированный frame до final seek target для EOF fallback.
    ///
    /// При seek в самый конец файла requested target может оказаться позже
    /// последнего реального video PTS. Такой frame нельзя показывать сразу как
    /// точный target, но его нужно сохранить до EOF, чтобы не зависнуть в seek.
    pub(crate) seek_preroll_fallback_video_frame: Option<video_core::DecodedFrame>,

    /// Текущая оценка длительности одного video frame.
    pub(crate) video_frame_duration_estimate: Duration,

    /// PTS последнего decoded frame для обновления оценки frame duration.
    pub(crate) last_decoded_video_pts: Option<Duration>,

    /// Текущий кадр для отображения, выбранный scheduler.
    pub(crate) present_video_frame: Option<video_core::DecodedFrame>,

    /// Поколение render resources текущего media pipeline.
    pub(crate) render_generation: u64,

    /// Texture handles, которые сейчас удерживает render/UI thread.
    pub(crate) leased_video_textures: HashMap<(u64, u64), usize>,

    /// Texture handles, release которых отложен до drop-ack от render/UI thread.
    pub(crate) deferred_video_texture_releases: HashSet<(u64, u64)>,

    /// Track ID выбранного video трека.
    pub(crate) video_track_id: Option<TrackId>,

    /// Очередь сырых video packets для decode.
    pub(crate) pending_video_packets: VecDeque<PendingVideoPacket>,

    /// Audio clock для A/V sync.
    pub(crate) audio_clock: Option<Arc<audio::clock::AudioClock>>,

    /// Абсолютная media-позиция, соответствующая нулю текущего audio clock.
    pub(crate) media_clock_base: Duration,

    /// Внутренний monotonic clock для playback без доступного audio clock.
    pub(crate) monotonic_media_clock_anchor: Option<MonotonicMediaClockAnchor>,

    /// Поколение packets после последнего seek transaction.
    pub(crate) seek_generation: u64,

    /// Последнее поколение, для которого audio output подтвердил очистку buffer.
    pub(crate) audio_buffer_clear_generation: u64,

    /// Последнее значение audio clock для обнаружения stalled audio.
    pub(crate) last_audio_clock: Duration,

    /// Момент последнего изменения audio clock.
    pub(crate) last_audio_clock_change_at: Instant,

    /// Индикатор текущего видео backend для UI и диагностики.
    pub(crate) video_backend: &'static str,

    /// Требование активного video track, уточнённое container metadata или bitstream probe.
    pub(crate) active_video_requirement: Option<VideoDecodeRequirement>,
}

impl PlaybackPipeline {
    /// Переводит renderer resource ids в новое поколение после полной смены media.
    pub(crate) fn advance_render_generation(&mut self) {
        self.render_generation = self.render_generation.wrapping_add(1);
    }

    /// Сбрасывает все media-specific поля после того, как session освободила video frames.
    pub(crate) fn reset_media_slots(&mut self) {
        self.demuxer = None;
        self.file_path = None;
        self.tracks.clear();
        self.source_label = None;
        self.audio_decoder = None;
        self.audio_output = None;
        self.audio_track_id = None;
        self.pending_audio_packets.clear();
        self.video_track_id = None;
        self.pending_video_packets.clear();
        self.video_decoder_needs_keyframe = true;
        self.reset_video_decode_in_flight();
        self.seek_preroll_fallback_video_frame = None;
        self.audio_clock = None;
        self.media_clock_base = Duration::ZERO;
        self.monotonic_media_clock_anchor = None;
        self.seek_generation = 0;
        self.audio_buffer_clear_generation = 0;
        self.video_frame_duration_estimate = DEFAULT_VIDEO_FRAME_DURATION;
        self.last_decoded_video_pts = None;
        self.last_audio_clock = Duration::ZERO;
        self.last_audio_clock_change_at = Instant::now();
        self.active_video_requirement = None;
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

    /// Сохраняет запущенный video backend без раскрытия backend-specific init в session.
    pub(crate) fn set_video_decoder_thread(
        &mut self,
        decoder_thread: impl VideoDecoderThreadHandle + 'static,
    ) {
        self.video_backend = decoder_thread.backend_name();
        self.video_decoder_thread = Some(Box::new(decoder_thread));
        self.reset_video_decode_in_flight();
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

    /// Возвращает количество активных render leases для тестов lease/release контракта.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn render_lease_count(&self) -> usize {
        self.leased_video_textures.len()
    }

    /// Проверяет, отложен ли release конкретного texture handle текущего поколения.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_deferred_video_texture_release(
        &self,
        texture_handle: video_core::FrameTextureHandle,
    ) -> bool {
        self.deferred_video_texture_releases
            .contains(&(self.render_generation, texture_handle.0))
    }

    /// Возвращает количество отложенных texture releases для тестов render boundary.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn deferred_video_texture_release_count(&self) -> usize {
        self.deferred_video_texture_releases.len()
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
            audio_output: None,
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
        }
    }
}
