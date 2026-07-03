use media_core::{MediaDuration, MediaTime, TrackId, TrackTimestamp};

/// Ревизия media/source identity, которую внешний owner увеличивает при смене источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceRevision(u64);

impl SourceRevision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Ревизия playback backend/resource owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BackendRevision(u64);

impl BackendRevision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Поколение обычного playback seek/decode lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaybackGeneration(u64);

impl PlaybackGeneration {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Поколение scrub transaction внутри текущего playback generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScrubGeneration(u64);

impl ScrubGeneration {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Двойной generation guard: старым считается mismatch любого из двух полей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubGenerationToken {
    pub playback_generation: PlaybackGeneration,
    pub scrub_generation: ScrubGeneration,
}

impl ScrubGenerationToken {
    #[must_use]
    pub const fn new(
        playback_generation: PlaybackGeneration,
        scrub_generation: ScrubGeneration,
    ) -> Self {
        Self {
            playback_generation,
            scrub_generation,
        }
    }

    #[must_use]
    pub fn stale_reason_against(self, current: Self) -> Option<ScrubStaleReason> {
        if self.playback_generation != current.playback_generation {
            return Some(ScrubStaleReason::PlaybackGenerationMismatch {
                context_generation: self.playback_generation,
                current_generation: current.playback_generation,
            });
        }

        if self.scrub_generation != current.scrub_generation {
            return Some(ScrubStaleReason::ScrubGenerationMismatch {
                context_generation: self.scrub_generation,
                current_generation: current.scrub_generation,
            });
        }

        None
    }
}

/// Причина, по которой intent/outcome/frame больше не относится к текущему owner state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubStaleReason {
    SourceRevisionMismatch {
        context_revision: SourceRevision,
        current_revision: SourceRevision,
    },
    BackendRevisionMismatch {
        context_revision: BackendRevision,
        current_revision: BackendRevision,
    },
    PlaybackGenerationMismatch {
        context_generation: PlaybackGeneration,
        current_generation: PlaybackGeneration,
    },
    ScrubGenerationMismatch {
        context_generation: ScrubGeneration,
        current_generation: ScrubGeneration,
    },
}

/// Снимок текущих guards у внешнего owner-а, против которого проверяются DTO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubCurrentGuards {
    pub source_revision: SourceRevision,
    pub backend_revision: BackendRevision,
    pub generation: ScrubGenerationToken,
}

impl ScrubCurrentGuards {
    #[must_use]
    pub const fn new(
        source_revision: SourceRevision,
        backend_revision: BackendRevision,
        generation: ScrubGenerationToken,
    ) -> Self {
        Self {
            source_revision,
            backend_revision,
            generation,
        }
    }
}

/// Источник scrub request-а. Это policy signal, а не concrete UI command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubRequestKind {
    SeekLanding,
    LiveScrub,
}

/// Приоритет admission/scheduling. Порядок enum-а намеренно от низкого к высокому.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScrubPriority {
    LiveScrub,
    UserCommit,
}

impl ScrubPriority {
    #[must_use]
    pub const fn for_request_kind(request_kind: ScrubRequestKind) -> Self {
        match request_kind {
            ScrubRequestKind::SeekLanding => Self::UserCommit,
            ScrubRequestKind::LiveScrub => Self::LiveScrub,
        }
    }
}

/// Политика точности кадра, которую внешний driver обязан соблюдать при commit/preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubExactnessPolicy {
    ExactFrame,
    TargetOrAfter,
    NearestWithin { tolerance: MediaDuration },
}

/// Выбранные media tracks для scrub target-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubTrackSelection {
    pub video_track: TrackId,
    pub audio_track: Option<TrackId>,
}

impl ScrubTrackSelection {
    #[must_use]
    pub const fn video_only(video_track: TrackId) -> Self {
        Self {
            video_track,
            audio_track: None,
        }
    }

    #[must_use]
    pub const fn with_audio(video_track: TrackId, audio_track: TrackId) -> Self {
        Self {
            video_track,
            audio_track: Some(audio_track),
        }
    }
}

/// Цель scrub-а одновременно в media timeline и в timestamp выбранного video track-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubTarget {
    pub media_time: MediaTime,
    pub target_pts: TrackTimestamp,
}

impl ScrubTarget {
    #[must_use]
    pub const fn new(media_time: MediaTime, target_pts: TrackTimestamp) -> Self {
        Self {
            media_time,
            target_pts,
        }
    }
}

/// Общий строгий контекст, который несёт каждый scrub intent/outcome/event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubTargetContext {
    source_revision: SourceRevision,
    backend_revision: BackendRevision,
    track_selection: ScrubTrackSelection,
    target: ScrubTarget,
    exactness_policy: ScrubExactnessPolicy,
    request_kind: ScrubRequestKind,
    priority: ScrubPriority,
    generation: ScrubGenerationToken,
}

impl ScrubTargetContext {
    #[must_use]
    pub const fn new(
        source_revision: SourceRevision,
        backend_revision: BackendRevision,
        track_selection: ScrubTrackSelection,
        target: ScrubTarget,
        exactness_policy: ScrubExactnessPolicy,
        request_kind: ScrubRequestKind,
        generation: ScrubGenerationToken,
    ) -> Self {
        Self {
            source_revision,
            backend_revision,
            track_selection,
            target,
            exactness_policy,
            request_kind,
            priority: ScrubPriority::for_request_kind(request_kind),
            generation,
        }
    }

    #[must_use]
    pub const fn source_revision(&self) -> SourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn backend_revision(&self) -> BackendRevision {
        self.backend_revision
    }

    #[must_use]
    pub const fn track_selection(&self) -> ScrubTrackSelection {
        self.track_selection
    }

    #[must_use]
    pub const fn target(&self) -> ScrubTarget {
        self.target
    }

    #[must_use]
    pub const fn exactness_policy(&self) -> ScrubExactnessPolicy {
        self.exactness_policy
    }

    #[must_use]
    pub const fn request_kind(&self) -> ScrubRequestKind {
        self.request_kind
    }

    #[must_use]
    pub const fn priority(&self) -> ScrubPriority {
        self.priority
    }

    #[must_use]
    pub const fn generation(&self) -> ScrubGenerationToken {
        self.generation
    }

    #[must_use]
    pub fn stale_reason_against(&self, current: ScrubCurrentGuards) -> Option<ScrubStaleReason> {
        if self.source_revision != current.source_revision {
            return Some(ScrubStaleReason::SourceRevisionMismatch {
                context_revision: self.source_revision,
                current_revision: current.source_revision,
            });
        }

        if self.backend_revision != current.backend_revision {
            return Some(ScrubStaleReason::BackendRevisionMismatch {
                context_revision: self.backend_revision,
                current_revision: current.backend_revision,
            });
        }

        self.generation.stale_reason_against(current.generation)
    }
}

/// Единственный main-video entrypoint для реального preview scrub-а.
///
/// Тип документирует контракт для будущей интеграции: новый параллельный
/// `RealPreviewSeek`/`PreviewDecode` command рядом с `PreviewScrub` здесь не моделируется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainVideoRealPreviewEntrypoint {
    PreviewScrub,
}

impl MainVideoRealPreviewEntrypoint {
    #[must_use]
    pub const fn command_name(self) -> &'static str {
        match self {
            Self::PreviewScrub => "PreviewScrub",
        }
    }

    #[must_use]
    pub const fn accepted_entrypoints() -> &'static [Self] {
        &[Self::PreviewScrub]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubIntentKind {
    PrepareTarget,
    SeekDecodePointBefore,
    FeedAndDrain,
    Finish,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrepareTargetIntent {
    pub context: ScrubTargetContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeekDecodePointBeforeIntent {
    pub context: ScrubTargetContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeedAndDrainStopCondition {
    PreviewFrameReady,
    AudioResumeReady,
    DriverStepLimit { max_steps: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeedAndDrainIntent {
    pub context: ScrubTargetContext,
    pub stop_condition: FeedAndDrainStopCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FinishScrubPolicy {
    CommitVisiblePreview,
    MatchPlaybackPosition,
    ReleaseWithoutCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FinishScrubIntent {
    pub context: ScrubTargetContext,
    pub policy: FinishScrubPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CancelScrubReason {
    SupersededByNewTarget,
    UserCancelled,
    StaleContext,
    DriverFailed,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CancelScrubIntent {
    pub context: ScrubTargetContext,
    pub reason: CancelScrubReason,
}

/// Крупные intent-ы scrub state machine. Lifecycle шаги decoder/demux здесь не публичный API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubIntent {
    PrepareTarget(PrepareTargetIntent),
    SeekDecodePointBefore(SeekDecodePointBeforeIntent),
    FeedAndDrain(FeedAndDrainIntent),
    Finish(FinishScrubIntent),
    Cancel(CancelScrubIntent),
}

impl ScrubIntent {
    pub const ACCEPTED_KINDS: [ScrubIntentKind; 5] = [
        ScrubIntentKind::PrepareTarget,
        ScrubIntentKind::SeekDecodePointBefore,
        ScrubIntentKind::FeedAndDrain,
        ScrubIntentKind::Finish,
        ScrubIntentKind::Cancel,
    ];

    #[must_use]
    pub const fn accepted_kinds() -> &'static [ScrubIntentKind] {
        &Self::ACCEPTED_KINDS
    }

    #[must_use]
    pub const fn kind(&self) -> ScrubIntentKind {
        match self {
            Self::PrepareTarget(_) => ScrubIntentKind::PrepareTarget,
            Self::SeekDecodePointBefore(_) => ScrubIntentKind::SeekDecodePointBefore,
            Self::FeedAndDrain(_) => ScrubIntentKind::FeedAndDrain,
            Self::Finish(_) => ScrubIntentKind::Finish,
            Self::Cancel(_) => ScrubIntentKind::Cancel,
        }
    }

    #[must_use]
    pub const fn context(&self) -> &ScrubTargetContext {
        match self {
            Self::PrepareTarget(payload) => &payload.context,
            Self::SeekDecodePointBefore(payload) => &payload.context,
            Self::FeedAndDrain(payload) => &payload.context,
            Self::Finish(payload) => &payload.context,
            Self::Cancel(payload) => &payload.context,
        }
    }

    #[must_use]
    pub fn stale_reason_against(&self, current: ScrubCurrentGuards) -> Option<ScrubStaleReason> {
        self.context().stale_reason_against(current)
    }
}
