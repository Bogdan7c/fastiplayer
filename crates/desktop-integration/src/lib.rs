//! Desktop integration boundary для media controls и критических уведомлений.
//!
//! Crate владеет только neutral desktop vocabulary, Linux MPRIS codec, доставкой
//! критического уведомления рабочего стола (`notification`) и транспортом
//! пересылки запроса второго запуска в первый экземпляр (`instance_forwarding`). Player,
//! playlist/controller и process lease остаются за app composition root.

#![forbid(unsafe_code)]

mod command;
mod error;
mod event;
mod instance_forwarding;
mod notification;
mod platform;
mod runtime;
mod shutdown;
mod snapshot;

pub use command::{
    DesktopCommand, DesktopCommandRequestId, DesktopCommandSink, DesktopLoopStatus,
    DesktopTimelineSeekOutcome, DesktopTrackKey, DesktopTransportAction, EffectiveVolume,
    EffectiveVolumeError, TimelineSeekRequestId,
};
pub use error::{DesktopIntegrationError, DesktopIntegrationResult};
pub use event::{DesktopBackendKind, DesktopIntegrationEvent};
pub use instance_forwarding::{
    FASTIPLAYER_APPLICATION_ID, ForwardedInstanceAction, ForwardedInstanceRequest,
    ForwardedRequestAcknowledgement, ForwardedRequestDelivery, ForwardedRequestRejection,
    ForwardedRequestSink, ForwardedRequestSinkError, ForwardedUri, InstanceForwardingClientConfig,
    InstanceForwardingError, InstanceForwardingService, InstanceForwardingServiceConfig,
    InstanceForwardingServiceError, MAX_ACTIVATION_TOKEN_BYTES, MAX_FORWARDED_URI_BYTES,
    MAX_FORWARDED_URIS, MAX_TOTAL_FORWARDED_URI_BYTES, WindowActivationToken,
    forward_to_running_instance, start_instance_forwarding_service,
};
pub use notification::{
    CriticalDesktopNotification, DesktopNotificationError, send_critical_desktop_notification,
};
pub use runtime::{DesktopIntegration, LatestSnapshotHandle, LatestSnapshotSource};
pub use shutdown::{DesktopIntegrationShutdownOutcome, DesktopIntegrationShutdownTransportFailure};
pub use snapshot::{
    DesktopCapabilities, DesktopControlRevision, DesktopMetadata, DesktopPlaybackStatus,
    DesktopSeeked, DesktopSnapshotChange, DesktopSnapshotRevision, DesktopSnapshotView,
};
