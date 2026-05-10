use std::time::Duration;

use media_core::TrackKind;

use crate::{PlaybackState, PlayerError, QualityId, TrackId};

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

    /// Состояние audio buffer.
    pub audio_buffer: AudioBufferSnapshot,

    /// Состояние очередей player pipeline.
    pub queues: QueueSnapshot,

    /// Счётчики frame pacing.
    pub frame_counters: FrameCounters,

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
            volume: 1.0,
            muted: false,
            selected_tracks: TrackSelectionSnapshot::default(),
            tracks: Vec::new(),
            available_qualities: Vec::new(),
            active_backend: BackendSnapshot::default(),
            current_video_frame: None,
            audio_buffer: AudioBufferSnapshot::default(),
            queues: QueueSnapshot::default(),
            frame_counters: FrameCounters::default(),
            last_error: None,
            capability_summary: None,
        }
    }
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

    /// Container codec id, например `V_VP9` или `A_OPUS`.
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
    /// Имя backend, например `VA-API VP9`.
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
