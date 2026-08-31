//! Reusable policy-neutral media-open mechanism (Session 10C).
//!
//! Модуль знает source preparation и neutral player install protocol, но намеренно
//! не знает playlist Item ID, navigation, repeat/shuffle, confirmation или priority policy.

#[allow(
    dead_code,
    reason = "Session 10C precedes production callsite migration in 10D"
)]
mod coordinator;
#[allow(
    dead_code,
    reason = "Session 10C precedes production callsite migration in 10D"
)]
mod executor;
#[allow(
    dead_code,
    reason = "Session 10C precedes production callsite migration in 10D"
)]
pub(crate) mod local;
mod native_hls;
#[allow(
    dead_code,
    reason = "Session 10C precedes production callsite migration in 10D"
)]
mod player_port;
mod preload_budget;
#[allow(
    dead_code,
    reason = "Session 10C precedes production callsite migration in 10D"
)]
mod preparation;
mod speculative;
#[allow(
    dead_code,
    reason = "Session 10C precedes production callsite migration in 10D"
)]
mod types;
mod web;

#[allow(unused_imports)] // Public mechanism inventory becomes consumed by Session 10D/11A.
pub(crate) use coordinator::MediaOpenCoordinator;
#[allow(unused_imports)] // Named D38 budget is part of the Session 10C mechanism contract.
pub(crate) use executor::MAX_NON_CANCELLABLE_STALE_PREPARATIONS;
#[allow(unused_imports)]
// Prepared envelope is intentionally introduced before callsite migration.
pub(crate) use local::{LocalFingerprintValidation, PreparedLocalOpenResult, prepare_local_open};
pub(crate) use native_hls::{NativeHlsOpenIntent, NativeHlsUrl};
// Все app ingress-ы собирают provider-neutral `PreparedMedia` через один boundary.
pub(crate) use preload_budget::QueuePreloadResourceBudget;
pub(crate) use preparation::{merge_yt_dlp_playlist_metadata, service_duration_for_timeline};
pub(crate) use speculative::{SpeculativeMediaPreparation, SpeculativeMediaPreparationPoll};
#[allow(
    unused_imports,
    reason = "cache snapshot is consumed through descriptor intent method"
)]
pub(crate) use types::{
    ActiveMediaSource, AuthorizationDispatchResolution, CancellationDispatchOutcome,
    MediaOpenClientKey, MediaOpenCommandError, MediaOpenCompletionDriveError,
    MediaOpenInstallIntent, MediaOpenInvariantViolation, MediaOpenPhase,
    MediaOpenPositionPreparation, MediaOpenRequestId, MediaOpenSnapshot, MediaOpenSourceRequest,
    MediaOpenStartError, MediaOpenStartMode, MediaOpenStartOutcome, MediaOpenTerminalOutcome,
    MediaPreparationFailureKind, PlayerDispatchRejection, PreparedMediaDescriptor,
    PreparedMediaOpen, PreparedPlaylistCacheUpdate, SafeMediaLabel,
    SameLineagePositionPreparationPhase,
};
#[cfg(test)]
pub(crate) use web::PreparedWebMediaCompositionError;
pub(crate) use web::{
    PreparedWebMediaAttachments, PreparedWebMediaEnvelope, PreparedWebMediaSeekAttachment,
    WebMediaOpenRequest, WebMediaOpenSettings, WebMediaSourceAdapterBridge, WebMediaSourceIntent,
    compose_prepared_web_media,
};
