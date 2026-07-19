use std::num::NonZeroU64;

use player_core::{
    ExactMediaTransportAction, ExactMediaTransportOutcome, ExactMediaTransportRequest,
    MediaInstanceId, PlayerWorkerSendError,
};

use super::{PlaylistMediaResetOwner, PlaylistMediaResetReceiptDisposition};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::controller::AppTransportDisposition;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};
use crate::playlist_runtime::{PlaylistBindingGeneration, PlaylistRuntime};

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(value).expect("test ID is non-zero"))
}

fn reset_request(value: u64) -> ExactMediaTransportRequest {
    ExactMediaTransportRequest {
        media_instance_id: media_instance_id(value),
        action: ExactMediaTransportAction::ResetMedia,
    }
}

#[test]
fn latest_pending_reset_retries_full_and_disconnect_is_terminal() {
    let mut owner = PlaylistMediaResetOwner::default();
    let first = reset_request(1);
    let latest = reset_request(2);
    owner.schedule(Some(first));
    owner.schedule(Some(latest));
    assert_eq!(owner.pending_request(), Some(latest));

    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    runtime.media_reset = owner;
    runtime.report_media_reset_send_error(latest, PlayerWorkerSendError::Full);
    assert_eq!(runtime.pending_media_reset_request(), Some(latest));

    runtime.report_media_reset_send_error(latest, PlayerWorkerSendError::Disconnected);
    assert_eq!(runtime.pending_media_reset_request(), None);
    assert_eq!(
        runtime
            .playlist_interaction_model()
            .safe_feedback
            .as_deref(),
        Some("Очередь очищена, но воспроизведение не удалось сбросить")
    );
}

#[test]
fn stale_reset_with_new_current_media_is_superseded() {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let requested = media_instance_id(10);

    assert_eq!(
        runtime.apply_media_reset_receipt(
            requested,
            Ok(ExactMediaTransportOutcome::StaleInstance {
                requested_media_instance_id: requested,
                current_media_instance_id: Some(media_instance_id(11)),
            }),
        ),
        PlaylistMediaResetReceiptDisposition::SupersededByNewMedia
    );
    assert!(runtime.playlist_interaction_model().safe_feedback.is_none());
}

#[test]
fn matching_reset_commits_stopped_but_late_applied_cannot_override_new_installed() {
    let requested = media_instance_id(20);
    let mut matching_runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    matching_runtime.resolve_missing_state_for_test();
    assert_eq!(
        matching_runtime.apply_media_reset_receipt(
            requested,
            Ok(ExactMediaTransportOutcome::Applied {
                media_instance_id: requested,
            }),
        ),
        PlaylistMediaResetReceiptDisposition::ClearAppMediaState
    );
    assert_eq!(
        matching_runtime.controller.transport_disposition,
        AppTransportDisposition::Stopped
    );

    let mut superseded_runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    superseded_runtime.resolve_missing_state_for_test();
    let new_instance = media_instance_id(21);
    superseded_runtime.controller.active_media = Some(ActiveMediaIdentity::installed(
        None,
        ActiveMediaLineageId::from_non_zero(NonZeroU64::new(22).expect("test lineage is non-zero")),
        new_instance,
        PlaylistBindingGeneration(23),
    ));
    assert_eq!(
        superseded_runtime.apply_media_reset_receipt(
            requested,
            Ok(ExactMediaTransportOutcome::Applied {
                media_instance_id: requested,
            }),
        ),
        PlaylistMediaResetReceiptDisposition::SupersededByNewMedia
    );
    assert_ne!(
        superseded_runtime.controller.transport_disposition,
        AppTransportDisposition::Stopped
    );
    assert_eq!(
        superseded_runtime
            .controller
            .active_media()
            .map(ActiveMediaIdentity::media_instance_id),
        Some(new_instance)
    );
}

#[test]
fn terminal_reset_failure_keeps_cleared_queue_and_reports_exact_safe_message() {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let requested = media_instance_id(30);

    assert_eq!(
        runtime.apply_media_reset_receipt(
            requested,
            Err(player_core::ExactMediaTransportReceiptError::MissingOwnerOutcome),
        ),
        PlaylistMediaResetReceiptDisposition::Failed
    );
    assert!(runtime.controller.queue.is_empty());
    assert_eq!(
        runtime
            .playlist_interaction_model()
            .safe_feedback
            .as_deref(),
        Some("Очередь очищена, но воспроизведение не удалось сбросить")
    );
}
