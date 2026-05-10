use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codec_core::VideoDecodeRequirement;
use media_core::{TrackId, TrackInfo};

/// Начальная оценка длительности video frame: 60 FPS.
pub const DEFAULT_VIDEO_FRAME_DURATION: Duration = Duration::from_micros(16_667);

/// Минимальная разумная длительность кадра для оценки FPS.
pub const MIN_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(5);

/// Максимальная разумная длительность кадра для оценки FPS.
pub const MAX_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(100);

/// Сырой audio packet, который ждёт decode из-за backpressure audio buffer.
pub struct PendingAudioPacket {
    /// Track ID нужен, чтобы не отправить packet неактивного audio track в decoder.
    pub track_id: TrackId,

    /// Encoded audio bytes хранятся отдельно от demuxer packet, потому что demuxer живёт независимо.
    pub encoded_bytes: Vec<u8>,
}

impl PendingAudioPacket {
    /// Создаёт ожидающий audio packet с явным track id и codec bytes.
    #[must_use]
    pub fn new(track_id: TrackId, encoded_bytes: Vec<u8>) -> Self {
        Self {
            track_id,
            encoded_bytes,
        }
    }
}

/// Сырой video packet, который ждёт отправки в decode thread.
pub struct PendingVideoPacket {
    /// Track ID нужен для фильтрации выбранного video track.
    pub track_id: TrackId,

    /// Presentation timestamp определяет A/V sync и decode-ahead лимит.
    pub pts: Duration,

    /// Encoded video bytes копируются из demuxer packet для владения внутри session.
    pub encoded_bytes: Vec<u8>,

    /// Keyframe flag пробрасывается в hardware decoder.
    pub keyframe: bool,
}

impl PendingVideoPacket {
    /// Создаёт ожидающий video packet без неименованных tuple-полей.
    #[must_use]
    pub fn new(track_id: TrackId, pts: Duration, encoded_bytes: Vec<u8>, keyframe: bool) -> Self {
        Self {
            track_id,
            pts,
            encoded_bytes,
            keyframe,
        }
    }
}

/// Внутреннее владение media pipeline для текущей player session.
///
/// Phase 4 переносит playback tick в `PlayerSession`. Часть полей пока остаётся публичной,
/// потому что renderer ещё забирает текущий кадр и backend-specific texture views напрямую.
pub struct PlaybackPipeline {
    /// Demuxer текущего WebM/Matroska media.
    pub demuxer: Option<Box<dyn webm_demux::Demuxer>>,

    /// Локальный путь, если media был открыт из файловой системы.
    pub file_path: Option<PathBuf>,

    /// Tracks текущего media без доступа UI к demuxer handle.
    pub tracks: Vec<TrackInfo>,

    /// User-facing label для streaming source без локального path.
    pub source_label: Option<String>,

    /// Audio decoder для Opus трека.
    pub audio_decoder: Option<audio::OpusDecoder>,

    /// Audio output: CPAL stream и ring buffer.
    pub audio_output: Option<audio::AudioOutput>,

    /// Track ID выбранного audio трека.
    pub audio_track_id: Option<TrackId>,

    /// Очередь сырых audio packets для throttle.
    pub pending_audio_packets: VecDeque<PendingAudioPacket>,

    /// Video decoder thread: VA-API VP9 decode в отдельном потоке.
    pub video_decoder_thread: Option<video_vaapi::VideoDecodeThread>,

    /// Очередь декодированных видеокадров перед presentation.
    pub video_frame_queue: VecDeque<video_core::DecodedFrame>,

    /// Текущая оценка длительности одного video frame.
    pub video_frame_duration_estimate: Duration,

    /// PTS последнего decoded frame для обновления оценки frame duration.
    pub last_decoded_video_pts: Option<Duration>,

    /// Текущий кадр для отображения, выбранный scheduler.
    pub present_video_frame: Option<video_core::DecodedFrame>,

    /// Track ID выбранного video трека.
    pub video_track_id: Option<TrackId>,

    /// Очередь сырых video packets для decode.
    pub pending_video_packets: VecDeque<PendingVideoPacket>,

    /// Audio clock для A/V sync.
    pub audio_clock: Option<Arc<audio::clock::AudioClock>>,

    /// Последнее значение audio clock для обнаружения stalled audio.
    pub last_audio_clock: Duration,

    /// Момент последнего изменения audio clock.
    pub last_audio_clock_change_at: Instant,

    /// Индикатор текущего видео backend для UI и диагностики.
    pub video_backend: &'static str,

    /// Требование активного video track, уточнённое container metadata или bitstream probe.
    pub active_video_requirement: Option<VideoDecodeRequirement>,
}

impl PlaybackPipeline {
    /// Создаёт пустой pipeline без открытого media.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
            video_frame_queue: VecDeque::new(),
            video_frame_duration_estimate: DEFAULT_VIDEO_FRAME_DURATION,
            last_decoded_video_pts: None,
            present_video_frame: None,
            video_track_id: None,
            pending_video_packets: VecDeque::new(),
            audio_clock: None,
            last_audio_clock: Duration::ZERO,
            last_audio_clock_change_at: Instant::now(),
            video_backend: "Synthetic (test)",
            active_video_requirement: None,
        }
    }
}
