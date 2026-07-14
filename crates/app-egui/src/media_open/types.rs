//! Policy-neutral vocabulary reusable media-open coordinator-а.

use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{MediaTagMetadata, TrackInfo};
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
    pub(super) const fn from_non_zero(value: NonZeroU64) -> Self {
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
    /// Stable normalized YouTube locator + exact selected stream pair.
    YouTubeUrl {
        source_locator: service_youtube::YoutubeMediaLocator,
        selected_stream_identity: service_youtube::YoutubeSelectedStreamIdentity,
    },
    /// Exact functional direct locator с service-owned redacted formatting.
    DirectMediaUrl(service_direct_media::DirectMediaUrl),
}

impl fmt::Debug for ActiveMediaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalFile(_) => formatter.write_str("LocalFile(<redacted-path>)"),
            Self::YouTubeUrl { source_locator, .. } => formatter
                .debug_struct("YouTubeUrl")
                .field("source_locator", source_locator)
                .field("selected_stream_identity", &"<selected-stream>")
                .finish(),
            Self::DirectMediaUrl(locator) => formatter
                .debug_tuple("DirectMediaUrl")
                .field(locator)
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
    /// YouTube parity envelope с exact service-selected stream identity.
    YouTube {
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        metadata: MediaTagMetadata,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    },
}

/// Подготовленный demuxer плюс immutable descriptor до ownership transfer.
pub(crate) struct PreparedMediaOpen {
    pub(super) prepared_media: player_core::PreparedMedia,
    pub(super) descriptor: PreparedMediaDescriptor,
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
    YouTube {
        locator: service_youtube::YoutubeMediaLocator,
        network_config: rustiplayer_config::NetworkConfig,
        youtube_config: rustiplayer_config::YoutubeConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
        preferred_video_codec_order: Vec<rustiplayer_config::VideoCodec>,
        system_capabilities: capability_core::SystemCapabilities,
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

/// Почему media preparation завершилась без secret-bearing context-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreparationFailureKind {
    LocalOpen,
    LocalSourceChanged,
    DirectOpen,
    YouTubeOpen,
    Cancelled,
    StaleResult,
    WorkerPanicked,
}

/// Fatal protocol failures не маскируются под recoverable media error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenInvariantViolation {
    PreparationStateLost,
    LifecycleCancellationDispatchFailed,
    MissingPlayerControlResolution,
    MissingTerminalAfterPlayerControl,
    UnexpectedAuthorizationOutcome,
    MissingInstalledAfterPlayerEnqueue,
    MismatchedPlayerRequest,
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
}

/// Exact typed initial intent без coordinator-owned autoplay policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaOpenInstallIntent {
    pub(crate) intent: PlaybackIntent,
    pub(crate) revision: PlaybackIntentRevision,
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

        let youtube_secret = "https://www.youtube.com/watch?v=dQw4w9WgXcQ&token=youtube-secret";
        let youtube_locator = service_youtube::parse_youtube_media_locator(youtube_secret)
            .expect("YouTube locator parsed");
        let youtube_label =
            SafeMediaLabel::from_service_safe_label(youtube_locator.safe_label()).to_string();
        assert!(!youtube_label.contains("youtube-secret"));
        assert!(!youtube_label.contains('?'));
    }

    #[test]
    fn safe_label_is_bounded_by_named_unicode_limit() {
        let raw_label = "я".repeat(SAFE_MEDIA_LABEL_MAX_CHARS + 25);
        let label = SafeMediaLabel::from_service_safe_label(&raw_label);

        assert_eq!(label.as_str().chars().count(), SAFE_MEDIA_LABEL_MAX_CHARS);
    }
}
