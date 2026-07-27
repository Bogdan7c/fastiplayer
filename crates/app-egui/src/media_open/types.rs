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
    /// Stable normalized YtDlp locator + exact selected candidate token.
    YtDlpUrl {
        /// Reconstructible exact source identity; Debug/Display остаются redacted.
        source_locator: service_ytdlp::YtDlpMediaLocator,
        /// Exact selection для fresh semantic rematch при controlled reopen.
        candidate_selection: Box<service_ytdlp::YtDlpCandidateSelection>,
        /// Optional service-owned composed A/V semantic intent.
        composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
        /// UI-safe installed inventory без URL, headers/cookies и candidate IDs.
        stream_configuration: Box<crate::web_media_stream_model::WebMediaStreamConfiguration>,
        /// Runtime-only declared catalog; Debug и persistence не раскрывают opaque identities.
        catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
    },
    /// Exact functional direct locator с service-owned redacted formatting.
    DirectMediaUrl(service_direct_media::DirectMediaUrl),

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
            Self::LocalFile(_) | Self::YtDlpUrl { .. } | Self::DirectMediaUrl(_) => None,
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
            Self::YtDlpUrl { source_locator, .. } => formatter
                .debug_struct("YtDlpUrl")
                .field("source_locator", source_locator)
                .field("candidate_selection", &"<exact-candidate>")
                .field("catalog_attachment", &"<provider-private>")
                .finish(),
            Self::DirectMediaUrl(locator) => formatter
                .debug_tuple("DirectMediaUrl")
                .field(locator)
                .finish(),
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
    /// Direct media parity envelope без filesystem-only fields.
    Direct {
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        metadata: MediaTagMetadata,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    },
    /// YtDlp parity envelope с exact service-selected stream identity.
    YtDlp {
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        metadata: MediaTagMetadata,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    },
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
            Self::Direct {
                tracks,
                duration,
                metadata,
                ..
            }
            | Self::YtDlp {
                tracks,
                duration,
                metadata,
                ..
            } => Some(PreparedPlaylistCacheUpdate {
                media_kind: media_kind_from_tracks(tracks),
                duration: *duration,
                metadata: metadata.clone(),
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
            Self::Local { source, .. }
            | Self::Direct { source, .. }
            | Self::YtDlp { source, .. }
            | Self::CallerPrepared { source, .. } => source.clone(),
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
            Self::Direct {
                tracks,
                duration,
                metadata,
                source,
                safe_label,
            } => Self::Direct {
                tracks,
                duration,
                metadata,
                source: source.with_playback_window(semantic_identity),
                safe_label,
            },
            Self::YtDlp {
                tracks,
                duration,
                metadata,
                source,
                safe_label,
            } => Self::YtDlp {
                tracks,
                duration,
                metadata,
                source: source.with_playback_window(semantic_identity),
                safe_label,
            },
            Self::CallerPrepared { source, safe_label } => Self::CallerPrepared {
                source: source.with_playback_window(semantic_identity),
                safe_label,
            },
        }
    }
}

/// Подготовленный demuxer плюс immutable descriptor до ownership transfer.
pub(crate) struct PreparedMediaOpen {
    pub(super) prepared_media: player_core::PreparedMedia,
    pub(super) descriptor: PreparedMediaDescriptor,
}

impl PreparedMediaOpen {
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
}

/// Набор production source parameters; выбор момента запуска остаётся у caller-а.
#[derive(Clone)]
pub(crate) enum MediaOpenSourceRequest {
    Local {
        path: PathBuf,
        expected_fingerprint: Option<LocalMediaFingerprint>,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
    },
    Direct {
        locator: service_direct_media::DirectMediaUrl,
        network_config: rustiplayer_config::NetworkConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
    },
    YtDlp {
        locator: service_ytdlp::YtDlpMediaLocator,
        selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent,
        network_config: rustiplayer_config::NetworkConfig,
        yt_dlp_config: rustiplayer_config::YtDlpConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
        preferred_video_codec_order: Vec<rustiplayer_config::VideoCodec>,
        system_capabilities: Box<capability_core::SystemCapabilities>,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    },
    PlaybackWindow {
        source: Box<MediaOpenSourceRequest>,
        semantic_identity: player_core::MediaPlaybackWindow,
    },
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
    YtDlpOpen,
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
mod tests {
    use super::*;

    #[test]
    fn service_labels_and_source_debug_do_not_leak_url_secrets() {
        let direct_secret = "https://user:password@example.com/video.mp4?token=very-secret";
        let direct_locator = service_direct_media::parse_direct_media_url(direct_secret)
            .expect("direct locator parsed");
        let direct_source = ActiveMediaSource::DirectMediaUrl(direct_locator.clone());
        let direct_debug = format!("{direct_source:?}");
        let direct_label =
            SafeMediaLabel::from_service_safe_label(direct_locator.safe_label()).to_string();

        assert!(!direct_debug.contains("password"));
        assert!(!direct_debug.contains("very-secret"));
        assert!(!direct_label.contains("password"));
        assert!(!direct_label.contains("very-secret"));

        let yt_dlp_secret = "https://www.youtube.com/watch?v=dQw4w9WgXcQ&token=yt_dlp-secret";
        let yt_dlp_locator =
            service_ytdlp::parse_yt_dlp_media_locator(yt_dlp_secret).expect("YtDlp locator parsed");
        let yt_dlp_label =
            SafeMediaLabel::from_service_safe_label(yt_dlp_locator.safe_label()).to_string();
        assert!(!yt_dlp_label.contains("yt_dlp-secret"));
        assert!(!yt_dlp_label.contains('?'));
    }

    #[test]
    fn safe_label_is_bounded_by_named_unicode_limit() {
        let raw_label = "я".repeat(SAFE_MEDIA_LABEL_MAX_CHARS + 25);
        let label = SafeMediaLabel::from_service_safe_label(&raw_label);

        assert_eq!(label.as_str().chars().count(), SAFE_MEDIA_LABEL_MAX_CHARS);
    }

    #[test]
    fn playback_window_identity_wraps_reopen_request_without_source_specific_types() {
        let semantic_identity = player_core::MediaPlaybackWindow::new(
            media_core::MediaTime::from_secs(10),
            Some(media_core::MediaTime::from_secs(25)),
        )
        .expect("valid neutral window");
        let source = ActiveMediaSource::LocalFile(PathBuf::from("fixture.flac"))
            .with_playback_window(semantic_identity);
        let request = source.wrap_reopen_request(MediaOpenSourceRequest::Local {
            path: PathBuf::from("fixture.flac"),
            expected_fingerprint: None,
            demux_config: rustiplayer_config::PlayerDemuxConfig::default(),
        });

        assert_eq!(source.playback_window(), Some(semantic_identity));
        assert!(matches!(
            source.physical_source(),
            ActiveMediaSource::LocalFile(_)
        ));
        assert!(matches!(
            request,
            MediaOpenSourceRequest::PlaybackWindow {
                semantic_identity: actual,
                ..
            } if actual == semantic_identity
        ));
    }
}
