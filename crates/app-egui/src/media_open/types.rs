//! Policy-neutral vocabulary reusable media-open coordinator-а.

use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{MediaTagMetadata, TrackInfo, TrackKind};
use player_core::{
    MediaInstallCancellationCause, MediaInstallCompletion, MediaInstallRequestId, PlaybackIntent,
    PlaybackIntentRevision,
};
use playlist_discovery::{LocalMediaFingerprint, LocalMediaKind};

use super::local::LocalFingerprintValidation;

/// Максимальная длина display-only label в Unicode scalar values.
pub(crate) const SAFE_MEDIA_LABEL_MAX_CHARS: usize = 160;

/// Opaque identity клиента coordinator-а; Item ID и queue semantics сюда не входят.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MediaOpenClientKey(NonZeroU64);

impl MediaOpenClientKey {
    /// Создаёт opaque key из caller-owned ненулевой identity.
    #[must_use]
    pub(crate) const fn from_non_zero(value: NonZeroU64) -> Self {
        Self(value)
    }
}

/// Process-local identity конкретного media-open request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MediaOpenRequestId(NonZeroU64);

impl MediaOpenRequestId {
    /// Crate-internal constructor также нужен controller protocol fixtures;
    /// production coordinator по-прежнему остаётся единственным allocator owner-ом.
    pub(crate) const fn from_non_zero(value: NonZeroU64) -> Self {
        Self(value)
    }
}

/// Bounded/redacted label, безопасный для UI, diagnostics и `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SafeMediaLabel(String);

impl SafeMediaLabel {
    /// Принимает только уже redacted service-owned label и дополнительно ограничивает длину.
    #[must_use]
    pub(crate) fn from_service_safe_label(label: &str) -> Self {
        Self(label.chars().take(SAFE_MEDIA_LABEL_MAX_CHARS).collect())
    }

    /// Строит display label только из filename, не раскрывая parent path.
    #[must_use]
    pub(crate) fn from_local_path(path: &Path) -> Self {
        let filename = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        Self::from_service_safe_label(&filename)
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SafeMediaLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SafeMediaLabel")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for SafeMediaLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Восстановимый пользовательский source intent для controlled rebuild после suspend.
///
/// Тип живёт у reusable media-open owner-а и переиспользуется текущими `AppState`
/// callsites через re-export, поэтому второй расходящийся source vocabulary не возникает.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ActiveMediaSource {
    /// Exact native path; `Debug` ниже не раскрывает его.
    LocalFile(PathBuf),
    /// Единый reconstructible web intent независимо от ingress adapter-а.
    Web(super::web::WebMediaSourceIntent),

    /// Source плюс neutral semantic identity ограниченного playback window.
    ///
    /// Wrapper не знает CUE/Group/playlist vocabulary и остаётся reconstructible,
    /// даже когда active media уже detached от исходной queue row.
    PlaybackWindow {
        source: Box<ActiveMediaSource>,
        semantic_identity: player_core::MediaPlaybackWindow,
    },
}

impl ActiveMediaSource {
    /// Возвращает neutral web intent под optional playback-window wrapper-ом.
    pub(crate) fn web_intent(&self) -> Option<&super::web::WebMediaSourceIntent> {
        match self.physical_source() {
            Self::Web(intent) => Some(intent),
            Self::LocalFile(_) => None,
            Self::PlaybackWindow { .. } => {
                unreachable!("physical_source removes playback-window wrappers")
            }
        }
    }

    /// Добавляет или заменяет единственную neutral playback-window identity.
    #[must_use]
    pub(crate) fn with_playback_window(
        self,
        semantic_identity: player_core::MediaPlaybackWindow,
    ) -> Self {
        let source = match self {
            Self::PlaybackWindow { source, .. } => source,
            source => Box::new(source),
        };
        Self::PlaybackWindow {
            source,
            semantic_identity,
        }
    }

    /// Возвращает optional semantic identity без знания playlist/CUE origin-а.
    #[must_use]
    pub(crate) const fn playback_window(&self) -> Option<player_core::MediaPlaybackWindow> {
        match self {
            Self::PlaybackWindow {
                semantic_identity, ..
            } => Some(*semantic_identity),
            Self::LocalFile(_) | Self::Web(_) => None,
        }
    }

    /// Возвращает физический source под optional semantic wrapper-ом.
    #[must_use]
    pub(crate) fn physical_source(&self) -> &Self {
        match self {
            Self::PlaybackWindow { source, .. } => source.physical_source(),
            source => source,
        }
    }

    /// Снимает optional wrapper для source-specific reopen dispatch-а.
    #[must_use]
    pub(crate) fn into_physical_source(self) -> Self {
        match self {
            Self::PlaybackWindow { source, .. } => source.into_physical_source(),
            source => source,
        }
    }

    /// Повторно применяет identity к freshly reopened prepared media.
    #[must_use]
    pub(crate) fn apply_to_prepared_media(
        &self,
        prepared_media: player_core::PreparedMedia,
    ) -> player_core::PreparedMedia {
        match self.playback_window() {
            Some(window) => prepared_media
                .with_playback_window(window)
                .expect("active static source cannot contain a dynamic live timeline"),
            None => prepared_media,
        }
    }

    /// Создаёт source request wrapper для suspend reopen через общий coordinator.
    #[must_use]
    pub(crate) fn wrap_reopen_request(
        &self,
        request: MediaOpenSourceRequest,
    ) -> MediaOpenSourceRequest {
        match self.playback_window() {
            Some(semantic_identity) => MediaOpenSourceRequest::PlaybackWindow {
                source: Box::new(request),
                semantic_identity,
            },
            None => request,
        }
    }
}

impl fmt::Debug for ActiveMediaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalFile(_) => formatter.write_str("LocalFile(<redacted-path>)"),
            Self::Web(intent) => formatter.debug_tuple("Web").field(intent).finish(),
            Self::PlaybackWindow {
                source,
                semantic_identity,
            } => formatter
                .debug_struct("PlaybackWindow")
                .field("source", source)
                .field("semantic_identity", semantic_identity)
                .finish(),
        }
    }
}

/// Immutable envelope descriptor, сохраняемый после передачи demuxer player owner-у.
#[derive(Debug, Clone)]
pub(crate) enum PreparedMediaDescriptor {
    /// Local D64/D75 envelope с полным cache snapshot-ом.
    Local {
        media_kind: LocalMediaKind,
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        metadata: MediaTagMetadata,
        fingerprint: LocalMediaFingerprint,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
        fingerprint_validation: LocalFingerprintValidation,
    },
    /// Единая web envelope для direct/native-manifest/extractor ingress-ов.
    Web(super::web::PreparedWebMediaEnvelope),
    /// Prepared-by-caller ingress сохраняет single-open ownership без повторного I/O.
    CallerPrepared {
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    },
}

/// Cache-only snapshot, который app action применяет только после exact Installed.
#[derive(Debug, Clone)]
pub(crate) struct PreparedPlaylistCacheUpdate {
    pub(crate) media_kind: LocalMediaKind,
    pub(crate) duration: Option<Duration>,
    pub(crate) metadata: MediaTagMetadata,
    pub(crate) fingerprint: Option<LocalMediaFingerprint>,
}

impl PreparedMediaDescriptor {
    /// Извлекает last-known metadata без demuxer ownership и без нового I/O.
    pub(crate) fn playlist_cache_update(&self) -> Option<PreparedPlaylistCacheUpdate> {
        match self {
            Self::Local {
                media_kind,
                duration,
                metadata,
                fingerprint,
                ..
            } => Some(PreparedPlaylistCacheUpdate {
                media_kind: *media_kind,
                duration: *duration,
                metadata: metadata.clone(),
                fingerprint: Some(*fingerprint),
            }),
            Self::Web(envelope) => Some(PreparedPlaylistCacheUpdate {
                media_kind: media_kind_from_tracks(envelope.tracks()),
                duration: envelope.duration(),
                metadata: envelope.metadata().clone(),
                fingerprint: None,
            }),
            Self::CallerPrepared { .. } => None,
        }
    }
}

fn media_kind_from_tracks(tracks: &[TrackInfo]) -> LocalMediaKind {
    if tracks.iter().any(|track| track.kind == TrackKind::Video) {
        LocalMediaKind::VideoContaining
    } else {
        LocalMediaKind::AudioOnly
    }
}

impl PreparedMediaDescriptor {
    /// Возвращает reconstructible source без раскрытия variant-specific storage.
    pub(crate) fn active_source(&self) -> ActiveMediaSource {
        match self {
            Self::Local { source, .. } | Self::CallerPrepared { source, .. } => source.clone(),
            Self::Web(envelope) => envelope.active_source(),
        }
    }

    /// Сохраняет neutral window identity рядом с physical reopen source.
    #[must_use]
    fn with_playback_window(self, semantic_identity: player_core::MediaPlaybackWindow) -> Self {
        match self {
            Self::Local {
                media_kind,
                tracks,
                duration,
                metadata,
                fingerprint,
                source,
                safe_label,
                fingerprint_validation,
            } => Self::Local {
                media_kind,
                tracks,
                duration,
                metadata,
                fingerprint,
                source: source.with_playback_window(semantic_identity),
                safe_label,
                fingerprint_validation,
            },
            Self::Web(envelope) => Self::Web(envelope.with_playback_window(semantic_identity)),
            Self::CallerPrepared { source, safe_label } => Self::CallerPrepared {
                source: source.with_playback_window(semantic_identity),
                safe_label,
            },
        }
    }

    /// Возвращает runtime-only VOD recovery attachment только для fresh yt-dlp candidate-а.
    pub(crate) fn vod_endpoint_recovery(
        &self,
    ) -> Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment> {
        match self {
            Self::Web(envelope) => envelope.vod_endpoint_recovery(),
            Self::Local { .. } | Self::CallerPrepared { .. } => None,
        }
    }
}

/// Подготовленный demuxer плюс immutable descriptor до ownership transfer.
pub(crate) struct PreparedMediaOpen {
    pub(super) prepared_media: player_core::PreparedMedia,
    pub(super) descriptor: PreparedMediaDescriptor,
}

impl PreparedMediaOpen {
    /// Передаёт settings strong-install owner-у готовый player payload и descriptor.
    pub(crate) fn into_parts(self) -> (player_core::PreparedMedia, PreparedMediaDescriptor) {
        (self.prepared_media, self.descriptor)
    }

    /// Применяет window одновременно к player payload и reconstructible descriptor.
    #[must_use]
    pub(super) fn with_playback_window(
        self,
        semantic_identity: player_core::MediaPlaybackWindow,
    ) -> Self {
        Self {
            prepared_media: self
                .prepared_media
                .with_playback_window(semantic_identity)
                .expect("prepared static descriptor cannot contain a dynamic live timeline"),
            descriptor: self.descriptor.with_playback_window(semantic_identity),
        }
    }

    /// Принимает demuxer, который уже открыл правильный startup/settings source owner.
    pub(crate) fn from_caller_prepared(
        prepared_media: player_core::PreparedMedia,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    ) -> Self {
        let source = match prepared_media.playback_window() {
            Some(window) => source.with_playback_window(window),
            None => source,
        };
        Self {
            prepared_media,
            descriptor: PreparedMediaDescriptor::CallerPrepared { source, safe_label },
        }
    }

    /// Background owner может сохранить rich descriptor без повторного media I/O.
    pub(crate) fn from_caller_prepared_with_descriptor(
        prepared_media: player_core::PreparedMedia,
        descriptor: PreparedMediaDescriptor,
    ) -> Self {
        Self {
            prepared_media,
            descriptor,
        }
    }
}

/// Набор production source parameters; выбор момента запуска остаётся у caller-а.
#[derive(Clone)]
pub(crate) enum MediaOpenSourceRequest {
    Local {
        path: PathBuf,
        expected_fingerprint: Option<LocalMediaFingerprint>,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
    },
    Web(super::web::WebMediaOpenRequest),
    PlaybackWindow {
        source: Box<MediaOpenSourceRequest>,
        semantic_identity: player_core::MediaPlaybackWindow,
    },
}

impl MediaOpenSourceRequest {
    /// Возвращает только безопасную bounded label без раскрытия URL credentials.
    pub(crate) fn safe_label(&self) -> SafeMediaLabel {
        match self {
            Self::Local { path, .. } => SafeMediaLabel::from_local_path(path),
            Self::Web(request) => request.safe_label(),
            Self::PlaybackWindow { source, .. } => source.safe_label(),
        }
    }
}

/// Caller явно выбирает coalesce либо требует новый request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenStartMode {
    /// Совпадающий client key возвращает существующий request без нового I/O.
    CoalesceMatchingClient,
    /// Любой existing request делает вызов typed busy; supersede выполняется отдельным API.
    RequireIdle,
}

/// Результат admission нового source request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenStartOutcome {
    Accepted { request_id: MediaOpenRequestId },
    Coalesced { request_id: MediaOpenRequestId },
}

/// Admission rejection не смешивается с terminal preparation/player failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MediaOpenStartError {
    #[error("media-open coordinator уже владеет другим request-ом")]
    Busy,
    #[error("media-open coordinator закрыт")]
    ShuttingDown,
    #[error("не удалось запустить media-open worker")]
    WorkerStartup,
    #[error("media-open executor потерял доверенный internal state")]
    ExecutorInvariant,
}

/// Typed внешняя фаза; ни одна acceptance не выдаётся за следующий barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenPhase {
    Accepted,
    Preparing,
    Prepared,
    PlayerStaging,
    ReadyToCommit,
    AuthorizationDispatchPending,
    EnqueuedAtPlayerOwner,
    Installed,
    Failed,
}

/// Same-lineage subphase не меняет ordinary coordinator protocol consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SameLineagePositionPreparationPhase {
    NotRequired,
    WaitingForPlayerReady,
    ReadyForPositionPreparation,
    PreparationDispatched,
    ReadyToCommit,
}

/// Authoritative resolution гонки cancel/dispatch до player enqueue barrier-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorizationDispatchResolution {
    CancelWonBeforePlayerEnqueue {
        cause: MediaInstallCancellationCause,
    },
    DownstreamRejectedBeforeEnqueue {
        rejection: PlayerDispatchRejection,
    },
    EnqueuedAtPlayerOwner,
}

/// Acceptance cancel command-а отдельно от authoritative race resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancellationDispatchOutcome {
    /// Player staging ещё не начался, поэтому cooperative cancellation уже authoritative.
    CancelledBeforePlayerStaging,
    /// Cancel помещён в ordered stream, но caller обязан ждать request-owned resolution.
    DispatchPending,
    /// Authorization уже была enqueued: token нельзя abort-ить, commit обязан завершиться.
    CommitMustFinish,
}

/// Transport rejection exact ordered player stream-а до enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayerDispatchRejection {
    Backpressure,
    Disconnected,
}

/// Coordinator command rejection сохраняет различие stale request, неверной фазы
/// и фактической transport-проблемы downstream player owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MediaOpenCommandError {
    #[error("media-open coordinator не владеет active request-ом")]
    NoCurrentRequest,
    #[error("media-open command адресована stale request-у")]
    StaleRequest,
    #[error("media-open command недопустима в фазе {actual:?}")]
    InvalidPhase { actual: MediaOpenPhase },
    #[error("renderer-bound player owner сейчас не привязан")]
    MissingPlayerBinding,
    #[error("player owner отклонил command до enqueue: {0:?}")]
    PlayerDispatch(PlayerDispatchRejection),
}

/// Synchronous completion driver failure; timeout никогда не считается success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MediaOpenCompletionDriveError {
    /// Exact request lookup/phase rejection.
    #[error(transparent)]
    Command(#[from] MediaOpenCommandError),
    /// Request-owned preparation result был потерян.
    #[error("request-owned media preparation result потерян")]
    MissingPreparationResolution,
    /// Player staging/control owner исчез без required outcome-а.
    #[error("request-owned player completion resolution потерян")]
    MissingPlayerResolution,
}

/// Почему media preparation завершилась без secret-bearing context-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreparationFailureKind {
    LocalOpen,
    LocalSourceChanged,
    DirectOpen,
    NativeHlsOpen,
    NativeDashOpen,
    NativeHdsOpen,
    NativeSmoothOpen,
    ExtractorOpen,
    /// Dynamic DASH валиден, но использует намеренно исключённый timing/profile contract.
    DashLiveProfileExcluded,
    /// Dynamic DASH нарушает поддерживаемую schema/model форму.
    DashLiveSchemaRejected,
    /// Fresh component catalog отсутствует либо не прошёл typed rematch/install.
    ComponentCatalogUnavailable,
    Cancelled,
    StaleResult,
    WorkerPanicked,
}

/// Fatal protocol failures не маскируются под recoverable media error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenInvariantViolation {
    PreparationStateLost,
    LifecycleCancellationDispatchFailed,
    LosslessCancellationDispatchFailed,
    MissingPlayerInstallResolution,
    MissingPlayerControlResolution,
    MissingTerminalAfterPlayerControl,
    UnexpectedAuthorizationOutcome,
    MissingInstalledAfterPlayerEnqueue,
    MismatchedPlayerRequest,
    UnexpectedPlayerInstallPhase,
}

/// Exactly-once terminal coordinator result.
#[derive(Debug, Clone)]
pub(crate) enum MediaOpenTerminalOutcome {
    Installed {
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        descriptor: Box<PreparedMediaDescriptor>,
        completion: MediaInstallCompletion,
    },
    Cancelled {
        request_id: MediaOpenRequestId,
        cause: MediaInstallCancellationCause,
    },
    PreparationFailed {
        request_id: MediaOpenRequestId,
        safe_label: SafeMediaLabel,
        kind: MediaPreparationFailureKind,
    },
    PlayerRejected {
        request_id: MediaOpenRequestId,
        rejection: PlayerDispatchRejection,
    },
    PlayerFailed {
        request_id: MediaOpenRequestId,
        completion: MediaInstallCompletion,
    },
    FatalInvariant {
        request_id: MediaOpenRequestId,
        violation: MediaOpenInvariantViolation,
    },
}

/// Snapshot current request-а для caller event-loop drain.
#[derive(Debug, Clone)]
pub(crate) struct MediaOpenSnapshot {
    pub(crate) client_key: MediaOpenClientKey,
    pub(crate) request_id: MediaOpenRequestId,
    pub(crate) phase: MediaOpenPhase,
    pub(crate) descriptor: Option<PreparedMediaDescriptor>,
    pub(crate) authorization_resolution: Option<AuthorizationDispatchResolution>,
    pub(crate) same_lineage_position: SameLineagePositionPreparationPhase,
}

/// Exact typed initial intent без coordinator-owned autoplay policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaOpenInstallIntent {
    pub(crate) intent: PlaybackIntent,
    pub(crate) revision: PlaybackIntentRevision,
}

/// Staging policy передаёт player-у exact old instance, не timeline position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenPositionPreparation {
    NotRequired,
    SameLineage {
        expected_old_media_instance_id: player_core::MediaInstanceId,
    },
}

#[cfg(test)]
#[path = "types/tests.rs"]
mod tests;
