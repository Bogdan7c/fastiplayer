//! Desktop integration boundary для media controls и критических уведомлений.
//!
//! Crate владеет только neutral desktop vocabulary, Linux MPRIS codec, доставкой
//! критического уведомления рабочего стола (`notification`) и транспортом
//! пересылки запроса второго запуска в первый экземпляр (`instance_forwarding`). Player,
//! playlist/controller и process lease остаются за app composition root.

#![forbid(unsafe_code)]

mod browser_session_entry;
mod command;
mod error;
mod event;
mod instance_forwarding;
#[cfg(target_os = "linux")]
mod mpris_names;
mod notification;
mod peer_admission;
mod platform;
mod player_pause;
mod runtime;
mod shutdown;
mod snapshot;
#[cfg(test)]
mod test_support;

pub use browser_session_entry::{
    ActivationServiceFileError, BROWSER_SESSION_ARGUMENT, BROWSER_SESSION_BUS_NAME,
    BROWSER_SESSION_INTERFACE, BROWSER_SESSION_OBJECT_PATH, BrowserEntryBus, BrowserEntryCallError,
    BrowserEntryCaller, BrowserEntryClient, BrowserEntryClientConfig, BrowserEntryDelivery,
    BrowserEntryReply, BrowserEntryRequest, BrowserEntryService, BrowserEntryServiceConfig,
    BrowserEntryServiceError, BrowserEntrySink, BrowserEntrySinkError,
    MAX_BROWSER_ENTRY_MESSAGE_BYTES, browser_session_activation_service_file,
    start_browser_entry_service,
};
pub use command::{
    DesktopCommand, DesktopCommandRequestId, DesktopCommandSink, DesktopLoopStatus,
    DesktopTimelineSeekOutcome, DesktopTrackKey, DesktopTransportAction, EffectiveVolume,
    EffectiveVolumeError, TimelineSeekRequestId,
};
pub use error::{DesktopIntegrationError, DesktopIntegrationResult};
pub use event::{DesktopBackendKind, DesktopIntegrationEvent};
pub use instance_forwarding::{
    FASTIPLAYER_APPLICATION_ID, ForwardedInstanceAction, ForwardedInstanceRequest,
    ForwardedInstanceRequestParts, ForwardedRequestAcknowledgement, ForwardedRequestDelivery,
    ForwardedRequestRejection, ForwardedRequestSink, ForwardedRequestSinkError, ForwardedUri,
    FullscreenIntent, InstanceForwardingClientConfig, InstanceForwardingError,
    InstanceForwardingService, InstanceForwardingServiceConfig, InstanceForwardingServiceError,
    MAX_ACTIVATION_TOKEN_BYTES, MAX_FORWARDED_URI_BYTES, MAX_FORWARDED_URIS,
    MAX_TOTAL_FORWARDED_URI_BYTES, WindowActivationToken, forward_to_running_instance,
    start_instance_forwarding_service,
};
pub use notification::{
    CriticalDesktopNotification, DesktopNotificationError, send_critical_desktop_notification,
};
pub use player_pause::{
    PlayerPauseConfig, PlayerPauseError, PlayerPauseOutcome, pause_running_player,
};
pub use runtime::{
    DesktopIntegration, DesktopPositionOutcome, LatestSnapshotHandle, LatestSnapshotSource,
};
pub use shutdown::{DesktopIntegrationShutdownOutcome, DesktopIntegrationShutdownTransportFailure};
pub use snapshot::{
    DesktopCapabilities, DesktopControlRevision, DesktopMetadata, DesktopPlaybackStatus,
    DesktopSeeked, DesktopSnapshotChange, DesktopSnapshotRevision, DesktopSnapshotView,
};
