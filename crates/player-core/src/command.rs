use std::path::PathBuf;
use std::time::Duration;

use frame_server_core::LiveScrubDiagnostics;
use media_core::{MediaTime, TrackId};

use crate::{PlaybackRate, PlaybackState};

/// Идентификатор качества или варианта потока.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualityId(String);

impl QualityId {
    /// Создаёт opaque ID качества без привязки к конкретному сервису.
    #[must_use]
    pub fn new(raw_quality_id: impl Into<String>) -> Self {
        Self(raw_quality_id.into())
    }

    /// Возвращает строковое представление для UI и сервисных адаптеров.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Источник media, который пользователь просит открыть.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// Локальный файл, выбранный пользователем или переданный через CLI.
    LocalFile(PathBuf),

    /// Сетевой URL, который позже обработает `source-core`.
    Url(String),

    /// Уже подготовленный внешний источник с человекочитаемой меткой.
    ExternalLabel(String),
}

impl MediaSource {
    /// Возвращает безопасную метку источника для snapshot'а и событий.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::LocalFile(path) => path.display().to_string(),
            Self::Url(url) => url.clone(),
            Self::ExternalLabel(label) => label.clone(),
        }
    }
}

/// Запрос на открытие media без владения demuxer или decoder handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaOpenRequest {
    /// Источник bytes или stream manifest.
    pub source: MediaSource,

    /// Нужно ли автоматически начать воспроизведение после успешного открытия.
    pub autoplay: bool,
}

impl MediaOpenRequest {
    /// Создаёт запрос открытия media с явным autoplay-флагом.
    #[must_use]
    pub const fn new(source: MediaSource, autoplay: bool) -> Self {
        Self { source, autoplay }
    }
}

/// Политика seek-операции, которую должен выбрать scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    /// Точный пользовательский seek: video decoder может стартовать раньше target-а,
    /// но playback/audio gate открываются только на target-е или позже.
    Accurate,

    /// Seek к ближайшему ключевому кадру до указанной позиции.
    KeyframeBefore,

    /// Seek к ближайшему ключевому кадру после target; пока отклоняется typed ошибкой.
    KeyframeAfter,
}

impl Default for SeekMode {
    /// По умолчанию выбираем точный seek как наиболее ожидаемое UI-поведение.
    fn default() -> Self {
        Self::Accurate
    }
}

/// Цель seek-запроса без привязки к UI control или container timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekTarget {
    /// Абсолютная позиция на нормализованной media timeline.
    Absolute(MediaTime),

    /// Относительный шаг от текущей позиции playback.
    Relative(Duration),
}

impl SeekTarget {
    /// Разрешает цель в абсолютную позицию для текущей timeline-позиции.
    #[must_use]
    pub fn resolve(self, current_position: MediaTime) -> MediaTime {
        match self {
            Self::Absolute(position) => position,
            Self::Relative(delta) => current_position.saturating_add(delta.into()),
        }
    }
}

/// Запрос перемотки внутри текущего media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekRequest {
    /// Целевая позиция: абсолютная media time или относительный шаг.
    pub target: SeekTarget,

    /// Политика точности перемотки.
    pub mode: SeekMode,
}

impl SeekRequest {
    /// Создаёт seek-запрос с точной перемоткой из legacy `Duration` позиции.
    #[must_use]
    pub const fn accurate(position: Duration) -> Self {
        Self {
            target: SeekTarget::Absolute(MediaTime::from_duration(position)),
            mode: SeekMode::Accurate,
        }
    }

    /// Создаёт seek-запрос к абсолютной media-позиции.
    #[must_use]
    pub const fn absolute(position: MediaTime) -> Self {
        Self {
            target: SeekTarget::Absolute(position),
            mode: SeekMode::Accurate,
        }
    }

    /// Создаёт seek-запрос как положительный шаг от текущей позиции.
    #[must_use]
    pub const fn relative(delta: Duration) -> Self {
        Self {
            target: SeekTarget::Relative(delta),
            mode: SeekMode::Accurate,
        }
    }
}

/// Policy завершения interactive scrub gesture-а.
///
/// Timeline live scrub использует этот intent как UI/UX boundary: release может
/// коммитить реально видимый preview, тогда как обычные exact seek-команды идут
/// через `PlayerCommand::Seek` и не наследуют timeline-specific release policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubCommitPolicy {
    /// Exact final target: всегда выполнить final seek в последнюю цель `UpdateScrub`.
    CommitLatestTarget,

    /// UX smooth pointer release: зафиксировать последний реально видимый preview.
    CommitVisiblePreview,
}

impl ScrubCommitPolicy {
    /// UX policy по умолчанию для отпускания pointer-а на timeline.
    ///
    /// Значение сохранено только для source/binary compatibility существующих callers.
    /// До будущей переписи preview-пайплайна session трактует оба enum-варианта одинаково.
    pub const DEFAULT_TIMELINE_RELEASE: Self = Self::CommitVisiblePreview;
}

/// Выбор качества потока для локального файла или сетевого сервиса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualitySelection {
    /// Автоматический выбор на основе capability matrix и bandwidth.
    Auto,

    /// Конкретный вариант, выбранный пользователем или сервисным слоем.
    Specific(QualityId),
}

/// Итог применения команды внутри `PlayerSession`.
///
/// Это boundary между нормальными semantic reject-ами и fatal/runtime ошибками:
/// `Rejected` не должен проходить через worker fatal path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCommandOutcome {
    /// Команда применена к session state.
    Applied,

    /// Команда корректна по типам, но недоступна в текущем состоянии session.
    Rejected(PlayerCommandReject),
}

/// Typed semantic reject для public player command-ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCommandReject {
    /// Playback rate V1 можно менять только во время `Playing` или `Paused`.
    PlaybackRateUnavailableForState {
        /// Public state, из-за которого команда не была применена.
        state: PlaybackState,
    },

    /// Выбранный audio path не смог атомарно подготовить новый tempo segment.
    PlaybackRateAudioTempoUnavailable {
        /// Typed причина, по которой старый rate был сохранён без мутации session.
        reason: PlaybackRateAudioTempoRejectReason,
    },
}

/// Причина атомарного отказа rate-команды на neutral player boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackRateAudioTempoRejectReason {
    /// Audio track выбран, но его output/clock boundary ещё не готов или уже потерян.
    AudioOutputUnavailable,

    /// Decoder ещё не сообщил надёжный PCM format для создания processor-а.
    PcmFormatNotReady,

    /// Factory или активный processor отклонили новый tempo segment.
    BackendRejected,
}

/// Команда, которую UI или внешняя интеграция отправляет player state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCommand {
    /// Открыть новый media-источник.
    OpenMedia(MediaOpenRequest),

    /// Начать или продолжить воспроизведение.
    Play,

    /// Приостановить воспроизведение.
    Pause,

    /// Переключить состояние между play и pause.
    TogglePlayback,

    /// Выполнить exact/accurate seek текущего media через единый SeekLanding route.
    ///
    /// Эта команда не использует `ScrubCommitPolicy` и остаётся route-ом для keyboard seek,
    /// external/MPRIS seek, будущего chapter seek и любого отделённого exact click-to-seek.
    Seek(SeekRequest),

    /// Начать timeline scrub gesture без немедленного commit-а.
    BeginScrub {
        /// Diagnostics live drag-а, если command пришла из real live-scrub UI route.
        live_scrub: Option<LiveScrubDiagnostics>,
    },

    /// Запомнить latest target лёгкого scrub-а без запуска demux seek-а.
    UpdateScrub(SeekRequest),

    /// Запустить/обновить live scrub preview target через reused-decoder route.
    ///
    /// Во время active live timeline drag это не ordinary seek event: session
    /// декодит exact preview target, но final commit остаётся заблокирован до
    /// `EndScrub`.
    PreviewScrub {
        /// Target, который должен стать latest live preview.
        request: SeekRequest,

        /// Snapshot/deferred diagnostics текущего live drag-а.
        live_scrub: Option<LiveScrubDiagnostics>,
    },

    /// Завершить active scrub gesture.
    ///
    /// Для live scrub release это разрешает commit уже активного preview route-а;
    /// для lightweight scrub fallback — запускает единый SeekLanding в latest target.
    EndScrub {
        /// UX policy release/cancel для текущего scrub gesture-а.
        policy: ScrubCommitPolicy,

        /// Последний bounded diagnostics state live drag-а на момент release/cancel.
        live_scrub: Option<LiveScrubDiagnostics>,
    },

    /// Остановить текущий media без завершения всей session.
    Stop,

    /// Установить runtime-only playback rate.
    ///
    /// S34 подключает no-audio clock/scheduler groundwork. Audio tempo и внешние
    /// control surfaces ещё не подключены, поэтому release всё ещё gated.
    SetPlaybackRate(PlaybackRate),

    /// Установить громкость в диапазоне `0.0..=1.0`.
    SetVolume(f32),

    /// Переключить mute с восстановлением последней слышимой громкости.
    ///
    /// `fallback_volume` приходит от app/config boundary и используется только тогда,
    /// когда session ещё не знает предыдущую ненулевую громкость.
    ToggleMute { fallback_volume: f32 },

    /// Выбрать активный video track.
    SelectVideoTrack(TrackId),

    /// Выбрать активный audio track.
    SelectAudioTrack(TrackId),

    /// Выбрать subtitle track или отключить субтитры через `None`.
    SelectSubtitleTrack(Option<TrackId>),

    /// Выбрать качество потока.
    SelectQuality(QualitySelection),

    /// Перечитать runtime config.
    ReloadConfig,

    /// Завершить player session.
    Shutdown,
}

impl PlayerCommand {
    /// Создаёт compatibility scrub begin без live-scrub diagnostics.
    #[must_use]
    pub const fn begin_scrub() -> Self {
        Self::BeginScrub { live_scrub: None }
    }

    /// Создаёт live-scrub begin с per-drag diagnostics snapshot.
    #[must_use]
    pub const fn begin_live_scrub(live_scrub: LiveScrubDiagnostics) -> Self {
        Self::BeginScrub {
            live_scrub: Some(live_scrub),
        }
    }

    /// Создаёт compatibility/live preview command без additional diagnostics.
    #[must_use]
    pub const fn preview_scrub(request: SeekRequest) -> Self {
        Self::PreviewScrub {
            request,
            live_scrub: None,
        }
    }

    /// Создаёт live preview command с текущим bounded diagnostics state.
    #[must_use]
    pub const fn preview_live_scrub(
        request: SeekRequest,
        live_scrub: LiveScrubDiagnostics,
    ) -> Self {
        Self::PreviewScrub {
            request,
            live_scrub: Some(live_scrub),
        }
    }

    /// Создаёт compatibility scrub end без live-scrub diagnostics.
    #[must_use]
    pub const fn end_scrub(policy: ScrubCommitPolicy) -> Self {
        Self::EndScrub {
            policy,
            live_scrub: None,
        }
    }

    /// Создаёт live-scrub release/cancel с последним diagnostics state.
    #[must_use]
    pub const fn end_live_scrub(
        policy: ScrubCommitPolicy,
        live_scrub: LiveScrubDiagnostics,
    ) -> Self {
        Self::EndScrub {
            policy,
            live_scrub: Some(live_scrub),
        }
    }
}
