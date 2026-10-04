//! Reusable policy-neutral media-open mechanism (Session 10C).
//!
//! Модуль знает source preparation и neutral player install protocol, но намеренно
//! не знает playlist Item ID, navigation, repeat/shuffle, confirmation или priority policy.

mod coordinator;
mod executor;
pub(crate) mod local;
mod native_hds_preparation;
mod player_port;
mod preload_budget;
mod preparation;
mod speculative;
mod types;
mod web;

#[allow(unused_imports)] // Public mechanism inventory becomes consumed by Session 10D/11A.
pub(crate) use coordinator::MediaOpenCoordinator;
#[allow(unused_imports)] // Named D38 budget is part of the Session 10C mechanism contract.
pub(crate) use executor::MAX_NON_CANCELLABLE_STALE_PREPARATIONS;
#[allow(unused_imports)]
// Prepared envelope is intentionally introduced before callsite migration.
pub(crate) use local::{
    LocalFingerprintValidation, LocalOpenFailureOutcome, LocalOpenFailureReason,
    PreparedLocalOpenResult, prepare_local_open,
};
// Native-типы источников и owner отката переехали в `media-source-open`
// (`native_web_source`); прежние пути `crate::media_open::*` сохранены re-export-ами.
pub(crate) use media_source_open::native_web_source::dash::{
    NativeDashOpenIntent, NativeDashSourceState, NativeDashUrl,
};
pub(crate) use media_source_open::native_web_source::fallback as native_fallback;
pub(crate) use media_source_open::native_web_source::hds::{
    NativeHdsOpenIntent, NativeHdsSourceState, NativeHdsUrl,
};
pub(crate) use media_source_open::native_web_source::hls::{
    NativeHlsOpenIntent, NativeHlsSourceState, NativeHlsUrl,
};
pub(crate) use media_source_open::native_web_source::smooth::{
    NativeSmoothOpenIntent, NativeSmoothSourceState, NativeSmoothUrl,
};
// Все app ingress-ы собирают provider-neutral `PreparedMedia` через один boundary.
pub(crate) use media_source_open::prepared_web_media::{
    PreparedWebMediaAttachments, PreparedWebMediaSeekAttachment, compose_prepared_web_media,
};
pub(crate) use preload_budget::QueuePreloadResourceBudget;
pub(crate) use preparation::{
    merge_yt_dlp_playlist_metadata, prepare_source_synchronously, service_duration_for_timeline,
};
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
pub(crate) use web::{
    DirectResourceSettingsAction, PreparedWebMediaEnvelope, WebMediaOpenRequest,
    WebMediaOpenSettings, WebMediaSelectionSwitchIntent, WebMediaSelectionSwitchResolution,
    WebMediaSettingsReconfigureDecision, WebMediaSettingsReconfigurePolicy,
    WebMediaSettingsSelectionPolicy, WebMediaSourceIntent,
};
