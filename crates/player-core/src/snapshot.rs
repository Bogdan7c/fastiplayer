use std::time::Duration;

use frame_server_core::{
    BackendRevision, FrameExactnessPolicy, ScrubGenerationToken, ScrubTrackSelection,
    SourceRevision, TimelineHoverFrameBucket,
};
use media_core::{
    MediaDuration, MediaTime, TimeBase, TimelineNotSeekableReason, TimelineRange, TimelineSnapshot,
    TrackKind, TrackTimestamp,
};
use video_core::{DecodedPixelFormat, FrameMemoryPath};

use crate::{PlaybackDiagnosticsSnapshot, PlaybackState, PlayerError, QualityId, TrackId};

/// Read-only snapshot player state для UI, renderer и desktop integration.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    /// Текущее состояние воспроизведения.
    pub playback_state: PlaybackState,

    /// Метка текущего источника: путь, URL или сервисное имя.
    pub source_label: Option<String>,

    /// Заголовок media, если он известен после demux/probe.
    pub media_title: Option<String>,

    /// Полная длительность media, если контейнер её сообщил.
    pub duration: Option<Duration>,

    /// Текущая media-позиция.
    pub current_position: Duration,

    /// Typed timeline/seek состояние для UI, renderer и desktop integration.
    pub timeline: TimelineSnapshot,

    /// Текущая громкость в диапазоне `0.0..=1.0`.
    pub volume: f32,

    /// Флаг mute, отделённый от числовой громкости.
    pub muted: bool,

    /// Выбранные треки.
    pub selected_tracks: TrackSelectionSnapshot,

    /// Доступные media tracks без доступа к demuxer handle.
    pub tracks: Vec<TrackSummarySnapshot>,

    /// Доступные варианты качества.
    pub available_qualities: Vec<QualitySummary>,

    /// Активный decode/render backend.
    pub active_backend: BackendSnapshot,

    /// Кадр, который renderer может показать сейчас.
    pub current_video_frame: Option<VideoFrameSnapshot>,

    /// Player-owned guarded context для hover prepare/preview shared working set-а.
    pub timeline_hover_prepare: Option<TimelineHoverPrepareSnapshot>,

    /// Поколение render resources; меняется при полной смене media pipeline.
    pub render_generation: u64,

    /// Текущая оценка длительности video frame для diagnostics UI.
    pub video_frame_duration_estimate: Duration,

    /// Состояние audio buffer.
    pub audio_buffer: AudioBufferSnapshot,

    /// Состояние очередей player pipeline.
    pub queues: QueueSnapshot,

    /// Счётчики frame pacing.
    pub frame_counters: FrameCounters,

    /// Typed playback diagnostics без UI/render handles.
    pub diagnostics: PlaybackDiagnosticsSnapshot,

    /// Последняя user-facing ошибка.
    pub last_error: Option<PlayerError>,

    /// Краткая сводка capability probing для UI.
    pub capability_summary: Option<String>,
}

impl PlayerSnapshot {
    /// Возвращает пустой snapshot без выбранного media.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Обновляет typed и legacy-представление текущей позиции одновременно.
    pub fn set_timeline_position(&mut self, position: MediaTime) {
        self.current_position = position.as_duration();
        self.timeline.current_position = position;
    }

    /// Обновляет typed и legacy-представление длительности одновременно.
    pub fn set_timeline_duration(&mut self, duration: Option<MediaDuration>) {
        self.duration = duration.map(MediaDuration::as_duration);
        let current_position = self.timeline.current_position;
        self.timeline = match duration {
            Some(duration) => {
                let mut timeline = TimelineSnapshot::seekable_vod(duration);
                timeline.current_position = current_position;
                timeline
            }
            None => TimelineSnapshot {
                current_position,
                not_seekable_reason: Some(TimelineNotSeekableReason::UnknownTimeline),
                ..TimelineSnapshot::default()
            },
        };
    }

    /// Сбрасывает timeline в состояние без открытого media.
    pub fn clear_timeline(&mut self) {
        self.duration = None;
        self.current_position = Duration::ZERO;
        self.timeline = TimelineSnapshot::default();
    }
}

impl Default for PlayerSnapshot {
    /// Создаёт безопасные default-значения для первого кадра UI.
    fn default() -> Self {
        Self {
            playback_state: PlaybackState::default(),
            source_label: None,
            media_title: None,
            duration: None,
            current_position: Duration::ZERO,
            timeline: TimelineSnapshot::default(),
            volume: 1.0,
            muted: false,
            selected_tracks: TrackSelectionSnapshot::default(),
            tracks: Vec::new(),
            available_qualities: Vec::new(),
            active_backend: BackendSnapshot::default(),
            current_video_frame: None,
            timeline_hover_prepare: None,
            render_generation: 0,
            video_frame_duration_estimate: Duration::ZERO,
            audio_buffer: AudioBufferSnapshot::default(),
            queues: QueueSnapshot::default(),
            frame_counters: FrameCounters::default(),
            diagnostics: PlaybackDiagnosticsSnapshot::default(),
            last_error: None,
            capability_summary: None,
        }
    }
}

/// Шаг смыслового hover target-а: 250 мс достаточно мелкий для отзывчивого preview,
/// но крупнее субпиксельного дрожания pointer-а и поэтому стабилизирует prepared cache key.
pub const HOVER_TARGET_QUANTIZATION_STEP: Duration = Duration::from_millis(250);

const MICROS_PER_SECOND: u128 = 1_000_000;
const NANOS_PER_MICRO: u32 = 1_000;

/// Player-owned context для S25 hover prepare/preview shared working set-а.
///
/// App получает только guard-значения и timebase для построения того же lookup key,
/// который `player-core` позже использует при S17 SeekLanding promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineHoverPrepareSnapshot {
    /// Активный video track, в timebase которого строится target PTS.
    video_track: TrackId,

    /// Активный audio track; нужен для совпадения prepared key с SeekLanding.
    audio_track: Option<TrackId>,

    /// Timebase video track-а для детерминированного MediaTime -> TrackTimestamp.
    video_track_time_base: TimeBase,

    /// Source guard из player-owned prepared seek context-а.
    source_revision: SourceRevision,

    /// Backend guard из player-owned prepared seek context-а.
    backend_revision: BackendRevision,

    /// Поколение будущего SeekLanding/scrub target-а.
    hover_generation: ScrubGenerationToken,

    /// Exactness policy prepared working set-а.
    exactness_policy: FrameExactnessPolicy,

    /// Player-owned режим взаимодействия hover prepare с текущим SeekLanding/live scrub.
    interaction: TimelineHoverPrepareInteraction,
}

/// Read-only режим, который говорит app-side hover prepare, кто сейчас владеет scrub ресурсами.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPrepareInteraction {
    /// Нет активного S17/live-scrub владельца, поэтому app выбирает обычный режим по playback state.
    Ordinary,

    /// One-shot SeekLanding уже удерживает promoted/active branch, поэтому hover может идти только при запасе.
    OneShotSeekLandingResumePending {
        /// Есть ли хотя бы один hover-owned slot после reservation активного SeekLanding.
        spare_capacity_available: bool,
    },

    /// Timeline live scrub gesture активен и полностью приостанавливает hover prepare/preview.
    LiveScrubActive,
}

/// Именованные поля для сборки `TimelineHoverPrepareSnapshot` внутри `player-core`.
pub(crate) struct TimelineHoverPrepareSnapshotParts {
    pub(crate) video_track: TrackId,
    pub(crate) audio_track: Option<TrackId>,
    pub(crate) video_track_time_base: TimeBase,
    pub(crate) source_revision: SourceRevision,
    pub(crate) backend_revision: BackendRevision,
    pub(crate) hover_generation: ScrubGenerationToken,
    pub(crate) exactness_policy: FrameExactnessPolicy,
    pub(crate) interaction: TimelineHoverPrepareInteraction,
}

impl TimelineHoverPrepareSnapshot {
    /// Создаёт snapshot только из player-owned session/snapshot builder-а.
    #[must_use]
    pub(crate) const fn from_parts(parts: TimelineHoverPrepareSnapshotParts) -> Self {
        Self {
            video_track: parts.video_track,
            audio_track: parts.audio_track,
            video_track_time_base: parts.video_track_time_base,
            source_revision: parts.source_revision,
            backend_revision: parts.backend_revision,
            hover_generation: parts.hover_generation,
            exactness_policy: parts.exactness_policy,
            interaction: parts.interaction,
        }
    }

    /// Возвращает source guard без раскрытия способа хранения snapshot-а.
    #[must_use]
    pub const fn source_revision(self) -> SourceRevision {
        self.source_revision
    }

    /// Возвращает backend guard без доступа к player session internals.
    #[must_use]
    pub const fn backend_revision(self) -> BackendRevision {
        self.backend_revision
    }

    /// Возвращает generation token, который совпадает с будущим SeekLanding key.
    #[must_use]
    pub const fn hover_generation(self) -> ScrubGenerationToken {
        self.hover_generation
    }

    /// Возвращает exactness policy для shared prepared working set lookup-а.
    #[must_use]
    pub const fn exactness_policy(self) -> FrameExactnessPolicy {
        self.exactness_policy
    }

    /// Возвращает player-owned режим hover prepare без доступа app к seek transaction internals.
    #[must_use]
    pub const fn interaction(self) -> TimelineHoverPrepareInteraction {
        self.interaction
    }

    /// Собирает track selection точно в форме, ожидаемой prepared SeekLanding.
    #[must_use]
    pub fn track_selection(self) -> ScrubTrackSelection {
        match self.audio_track {
            Some(audio_track) => ScrubTrackSelection::with_audio(self.video_track, audio_track),
            None => ScrubTrackSelection::video_only(self.video_track),
        }
    }

    /// Переводит media position в PTS выбранного video track-а.
    #[must_use]
    pub fn target_pts(self, target_position: MediaTime) -> TrackTimestamp {
        let units = self
            .video_track_time_base
            .duration_to_track_units_saturating(MediaDuration::from_duration(
                target_position.as_duration(),
            ));
        TrackTimestamp::new(self.video_track, units.get(), self.video_track_time_base)
    }

    /// Квантует смысловой hover target вверх к общей сетке и не выпускает его за seekable range.
    #[must_use]
    pub fn quantize_hover_target_position(
        target_position: MediaTime,
        seekable_range: TimelineRange,
    ) -> MediaTime {
        let clamped_position = seekable_range.clamp(target_position);
        let quantized_duration = duration_from_micros_saturating(ceil_micros_to_hover_grid(
            clamped_position.as_duration().as_micros(),
        ));

        seekable_range.clamp(MediaTime::from_duration(quantized_duration))
    }

    /// Строит bucket как индекс ceil-кванта, а не как raw timestamp.
    ///
    /// Bucket остаётся только быстрым индексом working set-а: точность кадра дальше
    /// проверяется отдельным `target_pts` в lookup request-е.
    #[must_use]
    pub fn target_bucket(target_pts: TrackTimestamp) -> TimelineHoverFrameBucket {
        TimelineHoverFrameBucket::new(hover_grid_bucket_index(target_pts.to_media_time()))
    }
}

fn hover_grid_bucket_index(position: MediaTime) -> i64 {
    let quantized_micros = ceil_micros_to_hover_grid(position.as_duration().as_micros());
    let bucket_index = quantized_micros / hover_target_quantization_step_micros();

    i64::try_from(bucket_index).unwrap_or(i64::MAX)
}

fn ceil_micros_to_hover_grid(position_micros: u128) -> u128 {
    let step_micros = hover_target_quantization_step_micros();
    let remainder = position_micros % step_micros;

    if remainder == 0 {
        position_micros
    } else {
        position_micros.saturating_add(step_micros - remainder)
    }
}

fn hover_target_quantization_step_micros() -> u128 {
    HOVER_TARGET_QUANTIZATION_STEP.as_micros()
}

fn duration_from_micros_saturating(micros: u128) -> Duration {
    let seconds = micros / MICROS_PER_SECOND;
    if seconds > u128::from(u64::MAX) {
        return Duration::MAX;
    }

    let subsecond_micros = micros % MICROS_PER_SECOND;
    Duration::new(
        u64::try_from(seconds).expect("checked seconds must fit into u64"),
        u32::try_from(subsecond_micros).expect("subsecond micros must stay below one second")
            * NANOS_PER_MICRO,
    )
}

/// Snapshot выбранных tracks без владения demuxer state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackSelectionSnapshot {
    /// Активный video track.
    pub video_track: Option<TrackId>,

    /// Активный audio track.
    pub audio_track: Option<TrackId>,

    /// Активный subtitle track или `None`, если субтитры отключены.
    pub subtitle_track: Option<TrackId>,
}

/// Snapshot media track для UI и диагностики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSummarySnapshot {
    /// Уникальный ID трека внутри текущего media.
    pub id: TrackId,

    /// Тип трека: video или audio.
    pub kind: TrackKind,

    /// Container codec id без привязки к конкретному codec backend.
    pub codec_id: String,

    /// Sample rate audio-трека, если он известен.
    pub sample_rate: Option<u32>,

    /// Количество audio-каналов, если оно известно.
    pub channels: Option<u32>,

    /// Compact summary video color metadata для diagnostics panel.
    pub video_color_summary: Option<String>,
}

/// Описание качества, которое UI может показать в списке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualitySummary {
    /// Стабильный ID качества.
    pub id: QualityId,

    /// Человекочитаемая подпись качества.
    pub label: String,

    /// Выбрано ли это качество сейчас.
    pub selected: bool,
}

/// Snapshot активного backend без backend-specific handles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendSnapshot {
    /// Имя активного backend для diagnostics.
    pub name: Option<String>,

    /// Состояние texture pool, если backend его предоставляет.
    pub texture_pool: Option<TexturePoolSnapshot>,
}

/// Snapshot texture pool для backpressure и диагностики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TexturePoolSnapshot {
    /// Максимальное количество texture slots.
    pub capacity: usize,

    /// Количество созданных slots.
    pub slots: usize,

    /// Количество slots, занятых текущими кадрами.
    pub in_use: usize,

    /// Количество persistent imports, готовых к reuse.
    pub free_surfaces: usize,

    /// Количество releases, ожидающих GPU completion.
    pub waiting_gpu_completion: usize,

    /// Количество releases, ожидающих возврата surface decoder-у.
    pub waiting_decoder_reuse: usize,

    /// Количество failed external imports.
    pub import_failures: u64,

    /// Количество созданных external imports.
    pub imports_created: u64,

    /// Количество reuse hits без нового external import-а.
    pub imports_reused: u64,

    /// Количество replacements free import-а после смены descriptor/object identity.
    pub imports_replaced: u64,
}

impl TexturePoolSnapshot {
    /// Возвращает количество свободных slots без underflow.
    #[must_use]
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Opaque snapshot кадра для renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameSnapshot {
    /// Поколение render resources, которому принадлежит handle кадра.
    pub render_generation: u64,

    /// Opaque handle текущего frame resource.
    pub handle: u64,

    /// Presentation timestamp кадра.
    pub pts: Duration,

    /// Декодированная ширина кадра.
    pub width: u32,

    /// Декодированная высота кадра.
    pub height: u32,

    /// Ширина отображения после crop/aspect обработки.
    pub render_width: u32,

    /// Высота отображения после crop/aspect обработки.
    pub render_height: u32,

    /// Фактический decoded pixel format renderer boundary.
    pub format: DecodedPixelFormat,

    /// Путь памяти кадра на renderer boundary.
    pub memory_path: FrameMemoryPath,
}

/// Snapshot audio buffer для UI и scheduler diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioBufferSnapshot {
    /// Текущий уровень буфера, если audio output активен.
    pub level: Option<Duration>,

    /// Количество underrun callbacks от audio backend.
    pub underruns: u64,
}

/// Snapshot очередей pipeline без доступа к их mutable данным.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    /// Количество audio packets, ожидающих decode.
    pub pending_audio_packets: usize,

    /// Количество video packets, ожидающих decode.
    pub pending_video_packets: usize,

    /// Количество decoded frames, ожидающих presentation.
    pub decoded_video_frames: usize,
}

/// Счётчики кадров для frame pacing diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameCounters {
    /// Общее количество представленных кадров.
    pub presented: u64,

    /// Общее количество отброшенных кадров.
    pub dropped: u64,

    /// Количество повторных показов предыдущего кадра.
    pub repeated: u64,
}

#[cfg(test)]
mod tests {
    use media_core::{TimeBase, TimelineRange, TrackId, TrackTimestamp};

    use super::*;

    fn seekable_range(end_millis: u64) -> TimelineRange {
        TimelineRange::from_bounds_saturating(MediaTime::ZERO, MediaTime::from_millis(end_millis))
    }

    fn timestamp_millis(millis: u64) -> TrackTimestamp {
        let time_base = TimeBase::new(1, 1_000).expect("millisecond timebase must be valid");

        TrackTimestamp::new(
            TrackId::new(1),
            i64::try_from(millis).expect("test timestamp must fit into i64"),
            time_base,
        )
    }

    #[test]
    fn hover_target_quantization_ceil_rounds_to_next_grid_boundary() {
        let range = seekable_range(2_000);

        assert_eq!(
            TimelineHoverPrepareSnapshot::quantize_hover_target_position(
                MediaTime::from_millis(1_000),
                range,
            ),
            MediaTime::from_millis(1_000)
        );
        assert_eq!(
            TimelineHoverPrepareSnapshot::quantize_hover_target_position(
                MediaTime::from_millis(1_001),
                range,
            ),
            MediaTime::from_millis(1_250)
        );
    }

    #[test]
    fn hover_target_quantization_clamps_last_cell_to_seekable_end() {
        let range = seekable_range(1_120);

        assert_eq!(
            TimelineHoverPrepareSnapshot::quantize_hover_target_position(
                MediaTime::from_millis(1_001),
                range,
            ),
            MediaTime::from_millis(1_120)
        );
        assert_eq!(
            TimelineHoverPrepareSnapshot::quantize_hover_target_position(
                MediaTime::from_millis(1_120),
                range,
            ),
            MediaTime::from_millis(1_120)
        );
    }

    #[test]
    fn hover_target_quantization_and_bucket_are_stable_inside_one_cell() {
        let range = seekable_range(2_000);
        let first_position = MediaTime::from_millis(1_001);
        let second_position = MediaTime::from_millis(1_249);

        assert_eq!(
            TimelineHoverPrepareSnapshot::quantize_hover_target_position(first_position, range),
            MediaTime::from_millis(1_250)
        );
        assert_eq!(
            TimelineHoverPrepareSnapshot::quantize_hover_target_position(second_position, range),
            MediaTime::from_millis(1_250)
        );
        assert_eq!(
            TimelineHoverPrepareSnapshot::target_bucket(timestamp_millis(1_001)),
            TimelineHoverPrepareSnapshot::target_bucket(timestamp_millis(1_249))
        );
        assert_eq!(
            TimelineHoverPrepareSnapshot::target_bucket(timestamp_millis(1_001)).get(),
            5
        );
    }
}
