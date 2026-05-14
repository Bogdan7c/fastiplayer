use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::VideoDecodeRequirement;
use media_core::{TrackId, TrackInfo};
use webm_demux::Demuxer;

/// Начальная оценка длительности video frame: 60 FPS.
pub(crate) const DEFAULT_VIDEO_FRAME_DURATION: Duration = Duration::from_micros(16_667);

/// Минимальная разумная длительность кадра для оценки FPS.
pub(crate) const MIN_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(5);

/// Максимальная разумная длительность кадра для оценки FPS.
pub(crate) const MAX_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(100);

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

    /// Video decoder thread: текущий VA-API backend decode в отдельном потоке.
    pub(crate) video_decoder_thread: Option<video_vaapi::VideoDecodeThread>,

    /// Требует ли decoder следующий video packet быть keyframe-ом.
    ///
    /// Stateless hardware decode после flush теряет reference frames. Если сразу
    /// отправить inter-frame, decoder может показать зелёные артефакты до
    /// следующего keyframe. Флаг держит этот codec contract рядом с очередью
    /// packets, а не в UI/render слое.
    pub(crate) video_decoder_needs_keyframe: bool,

    /// Очередь декодированных видеокадров перед presentation.
    pub(crate) video_frame_queue: VecDeque<video_core::DecodedFrame>,

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
        self.audio_clock = None;
        self.media_clock_base = Duration::ZERO;
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
        decoder_thread: video_vaapi::VideoDecodeThread,
    ) {
        self.video_backend = decoder_thread.backend_name();
        self.video_decoder_thread = Some(decoder_thread);
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
            video_frame_queue: VecDeque::new(),
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
            seek_generation: 0,
            audio_buffer_clear_generation: 0,
            last_audio_clock: Duration::ZERO,
            last_audio_clock_change_at: Instant::now(),
            video_backend: "Synthetic (test)",
            active_video_requirement: None,
        }
    }
}
