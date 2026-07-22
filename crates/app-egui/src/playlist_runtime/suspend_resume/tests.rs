use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use media_core::MediaTime;
use player_core::{
    MediaInstallRequestId, MediaInstanceId, MediaPlaybackWindow, PlaybackState, PlayerSnapshot,
};
use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};

use super::*;
use crate::app_wake::{AppWakeEvent, AppWakeOwner, AppWakePort, WakeEmitter};
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, EndedSnapshotKind,
};
use crate::playlist_runtime::removal_undo::{RemovalUndoOutcome, RuntimeRemovalOutcome};

struct NoopEmitter;

impl WakeEmitter for NoopEmitter {
    fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
        Ok(())
    }
}

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
}

fn runtime() -> PlaylistRuntime {
    let wake = AppWakePort::new(AppWakeOwner::PlaylistRuntime, Arc::new(NoopEmitter));
    let mut runtime = PlaylistRuntime::new(wake);
    runtime.resolve_missing_state_for_test();
    runtime
}

fn install_external(
    runtime: &mut PlaylistRuntime,
    binding: PlaylistRuntimeBinding,
    value: u64,
) -> ActiveMediaIdentity {
    runtime
        .register_successful_strong_install(
            MediaOpenRequestId::from_non_zero(non_zero(value)),
            MediaInstallRequestId::from_non_zero(non_zero(value)),
            MediaInstanceId::from_non_zero(non_zero(value)),
            binding,
            ActiveMediaSource::LocalFile("fixture.mp4".into()),
            player_core::PlaybackIntent::StartPaused,
        )
        .expect("external strong install must register lineage")
}

fn snapshot(
    active: ActiveMediaIdentity,
    state: PlaybackState,
    position: Duration,
) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.media_instance_id = Some(active.media_instance_id());
    snapshot.playback_state = state;
    snapshot.current_position = position;
    snapshot.duration = Some(Duration::from_secs(120));
    snapshot
}

fn playlist_draft(label: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(label)),
        None,
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
    )
}

#[test]
fn paused_resume_preserves_position_and_lineage_with_new_instance() {
    let mut runtime = runtime();
    let first_binding = runtime.bind_resumed_app_state().expect("first binding");
    let active = install_external(&mut runtime, first_binding, 11);
    let captured = runtime
        .capture_suspended_media_checkpoint(
            first_binding,
            &snapshot(active, PlaybackState::Paused, Duration::from_secs(37)),
        )
        .expect("checkpoint capture");
    assert_eq!(captured, SuspendCheckpointOutcome::Captured);

    runtime.suspend_app_state_binding();
    let second_binding = runtime.bind_resumed_app_state().expect("second binding");
    let attempt = runtime
        .begin_suspended_media_resume(false)
        .expect("automatic paused resume");
    assert_eq!(attempt.position, Duration::from_secs(37));
    assert_eq!(attempt.intent, ResumePlaybackIntent::Paused);
    assert_eq!(attempt.expected_active.lineage_id(), active.lineage_id());

    let new_instance = MediaInstanceId::from_non_zero(non_zero(12));
    let rebound = runtime
        .complete_suspended_media_resume(
            active,
            new_instance,
            second_binding.binding_generation(),
            None,
        )
        .expect("same-lineage rebound");
    assert_eq!(rebound.lineage_id(), active.lineage_id());
    assert_eq!(rebound.media_instance_id(), new_instance);
    assert_eq!(
        runtime.suspended_media_status(),
        ResumeCheckpointStatus::Empty
    );
    assert!(runtime.begin_suspended_media_resume(false).is_none());

    runtime
        .capture_suspended_media_checkpoint(
            second_binding,
            &snapshot(rebound, PlaybackState::Paused, Duration::from_secs(44)),
        )
        .expect("second lifecycle checkpoint");
    runtime.suspend_app_state_binding();
    let third_binding = runtime.bind_resumed_app_state().expect("third binding");
    let second_attempt = runtime
        .begin_suspended_media_resume(false)
        .expect("second lifecycle attempt");
    assert_eq!(second_attempt.expected_active, rebound);
    assert_eq!(second_attempt.position, Duration::from_secs(44));
    runtime
        .complete_suspended_media_resume(
            rebound,
            MediaInstanceId::from_non_zero(non_zero(13)),
            third_binding.binding_generation(),
            None,
        )
        .expect("second lifecycle rebound");
    assert!(runtime.begin_suspended_media_resume(false).is_none());
}

#[test]
fn detached_windowed_source_reopens_without_queue_row_or_cue_identity() {
    let mut runtime = runtime();
    let first_binding = runtime.bind_resumed_app_state().expect("first binding");
    let playback_window =
        MediaPlaybackWindow::new(MediaTime::from_secs(10), Some(MediaTime::from_secs(25)))
            .expect("valid neutral window");
    let source = ActiveMediaSource::LocalFile("detached-source.flac".into())
        .with_playback_window(playback_window);
    let active = runtime
        .register_successful_strong_install(
            MediaOpenRequestId::from_non_zero(non_zero(71)),
            MediaInstallRequestId::from_non_zero(non_zero(71)),
            MediaInstanceId::from_non_zero(non_zero(71)),
            first_binding,
            source,
            player_core::PlaybackIntent::StartPaused,
        )
        .expect("detached install");
    assert_eq!(active.item_id(), None);
    let mut relative_snapshot = snapshot(active, PlaybackState::Paused, Duration::from_secs(4));
    relative_snapshot.duration = Some(Duration::from_secs(15));
    runtime
        .capture_suspended_media_checkpoint(first_binding, &relative_snapshot)
        .expect("windowed checkpoint");

    runtime.suspend_app_state_binding();
    let _second_binding = runtime.bind_resumed_app_state().expect("second binding");
    let attempt = runtime
        .begin_suspended_media_resume(false)
        .expect("detached windowed resume attempt");

    assert_eq!(attempt.position, Duration::from_secs(4));
    assert_eq!(attempt.source.playback_window(), Some(playback_window));
    assert!(matches!(
        attempt.source.physical_source(),
        ActiveMediaSource::LocalFile(_)
    ));
}

#[test]
fn detached_same_item_switch_rebinds_instance_without_inventing_queue_current() {
    let mut runtime = runtime();
    let binding = runtime.bind_resumed_app_state().expect("active binding");
    let active_before = install_external(&mut runtime, binding, 72);
    let revisions_before = runtime
        .controller
        .as_ref()
        .expect("controller")
        .queue()
        .revision_snapshot();
    let replacement_source = ActiveMediaSource::LocalFile("replacement.flac".into());

    let active_after = runtime
        .complete_same_item_candidate_switch(
            active_before,
            MediaInstanceId::from_non_zero(non_zero(73)),
            binding,
            replacement_source.clone(),
        )
        .expect("same-lineage detached rebind");

    assert_eq!(active_after.item_id(), None);
    assert_eq!(active_after.lineage_id(), active_before.lineage_id());
    assert_eq!(runtime.playlist_view_snapshot().traversal_current(), None);
    assert_eq!(
        runtime
            .controller
            .as_ref()
            .expect("controller")
            .queue()
            .revision_snapshot(),
        revisions_before
    );
    assert_eq!(
        runtime.suspended_media.active_source.as_ref(),
        Some(&replacement_source)
    );
}

#[test]
fn tombstone_undo_survives_same_lineage_new_instance() {
    let mut runtime = runtime();
    let first_binding = runtime.bind_resumed_app_state().expect("first binding");
    let item_id = match runtime
        .controller
        .append(vec![playlist_draft("active-tombstone.mp4")])
        .expect("append active row")
    {
        super::super::controller::ControllerAppendOutcome::Added { item_ids, .. } => item_ids[0],
        super::super::controller::ControllerAppendOutcome::NoItemsProvided => {
            panic!("fixture must append one row")
        }
    };
    runtime
        .controller
        .queue
        .set_traversal_current(item_id)
        .expect("fixture current row");
    let active = ActiveMediaIdentity::installed(
        Some(item_id),
        super::super::identity::ActiveMediaLineageId::from_non_zero(non_zero(71)),
        MediaInstanceId::from_non_zero(non_zero(72)),
        first_binding.binding_generation(),
    );
    runtime.controller.active_media = Some(active);
    runtime.suspended_media.active_source =
        Some(ActiveMediaSource::LocalFile("active-tombstone.mp4".into()));
    let now = Instant::now();
    assert!(matches!(
        runtime.remove_playlist_item(item_id, now),
        RuntimeRemovalOutcome::Removed { .. }
    ));
    let detached = runtime.controller.active_media().expect("detached active");
    assert_eq!(detached.item_id(), None);

    runtime
        .capture_suspended_media_checkpoint(
            first_binding,
            &snapshot(detached, PlaybackState::Paused, Duration::from_secs(19)),
        )
        .expect("tombstone checkpoint");
    runtime.suspend_app_state_binding();
    let second_binding = runtime.bind_resumed_app_state().expect("second binding");
    runtime
        .begin_suspended_media_resume(false)
        .expect("same-lineage tombstone resume");
    let rebound = runtime
        .complete_suspended_media_resume(
            detached,
            MediaInstanceId::from_non_zero(non_zero(73)),
            second_binding.binding_generation(),
            None,
        )
        .expect("tombstone rebound");

    assert_eq!(rebound.lineage_id(), active.lineage_id());
    assert!(runtime.removal_undo_status(now).is_some());
    assert!(matches!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Restored { .. }
    ));
    assert_eq!(
        runtime
            .controller
            .active_media()
            .expect("undo reattaches active")
            .item_id(),
        Some(item_id)
    );
}

#[test]
fn playing_intent_is_restored_only_as_post_seek_attempt_intent() {
    let mut runtime = runtime();
    let binding = runtime.bind_resumed_app_state().expect("binding");
    let active = runtime
        .register_successful_strong_install(
            MediaOpenRequestId::from_non_zero(non_zero(21)),
            MediaInstallRequestId::from_non_zero(non_zero(21)),
            MediaInstanceId::from_non_zero(non_zero(21)),
            binding,
            ActiveMediaSource::LocalFile("fixture.mp4".into()),
            player_core::PlaybackIntent::StartPlaying,
        )
        .expect("playing strong install must register stable intent");
    runtime
        .capture_suspended_media_checkpoint(
            binding,
            &snapshot(active, PlaybackState::Playing, Duration::from_secs(9)),
        )
        .expect("playing checkpoint");
    let attempt = runtime
        .begin_suspended_media_resume(false)
        .expect("playing resume attempt");
    assert_eq!(attempt.intent, ResumePlaybackIntent::Playing);
    assert_eq!(attempt.position, Duration::from_secs(9));
}

#[test]
fn ended_checkpoint_becomes_paused_at_end_and_carries_consumed_eof_edge() {
    let mut runtime = runtime();
    let first_binding = runtime.bind_resumed_app_state().expect("first binding");
    let active = install_external(&mut runtime, first_binding, 31);
    let mut ended = snapshot(active, PlaybackState::Ended, Duration::from_secs(119));
    ended.duration = Some(Duration::from_secs(120));
    runtime
        .capture_suspended_media_checkpoint(first_binding, &ended)
        .expect("ended checkpoint");
    runtime.suspend_app_state_binding();
    let second_binding = runtime.bind_resumed_app_state().expect("second binding");
    let attempt = runtime
        .begin_suspended_media_resume(false)
        .expect("ended resume attempt");
    assert_eq!(attempt.intent, ResumePlaybackIntent::Paused);
    assert_eq!(attempt.position, Duration::from_secs(120));

    let rebound_instance = MediaInstanceId::from_non_zero(non_zero(32));
    let rebound = runtime
        .complete_suspended_media_resume(
            active,
            rebound_instance,
            second_binding.binding_generation(),
            None,
        )
        .expect("ended rebound");
    let repeated = runtime
        .controller
        .as_mut()
        .expect("controller")
        .observe_automatic_snapshot(
            second_binding.binding_generation(),
            Some(rebound.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        );
    assert!(matches!(repeated, AutomaticLifecycleOutcome::NoAction));
}

#[test]
fn terminal_failure_requires_explicit_retry_and_recoverable_failure_keeps_checkpoint() {
    let mut runtime = runtime();
    let binding = runtime.bind_resumed_app_state().expect("binding");
    let active = install_external(&mut runtime, binding, 41);
    runtime
        .capture_suspended_media_checkpoint(
            binding,
            &snapshot(active, PlaybackState::Failed, Duration::from_secs(4)),
        )
        .expect("failed checkpoint");
    assert_eq!(
        runtime.suspended_media_status(),
        ResumeCheckpointStatus::TerminalFailureNeedsExplicitRetry
    );
    assert!(runtime.begin_suspended_media_resume(false).is_none());
    assert!(runtime.begin_suspended_media_resume(true).is_some());
    runtime.fail_suspended_media_resume(ResumeCheckpointError::PreparationFailed);
    assert!(matches!(
        runtime.suspended_media_status(),
        ResumeCheckpointStatus::RecoverableFailure(ResumeCheckpointError::PreparationFailed)
    ));
    assert!(runtime.begin_suspended_media_resume(true).is_some());
}

#[test]
fn non_seekable_warning_and_explicit_open_supersede_do_not_mutate_lineage_early() {
    let mut runtime = runtime();
    let first_binding = runtime.bind_resumed_app_state().expect("first binding");
    let active = install_external(&mut runtime, first_binding, 51);
    runtime
        .capture_suspended_media_checkpoint(
            first_binding,
            &snapshot(active, PlaybackState::Paused, Duration::from_secs(50)),
        )
        .expect("checkpoint");
    runtime.suspend_app_state_binding();
    let second_binding = runtime.bind_resumed_app_state().expect("second binding");
    runtime
        .begin_suspended_media_resume(false)
        .expect("resume attempt");
    let warning = ResumePositionWarning {
        requested_position: Duration::from_secs(50),
        available_position: Duration::from_secs(57),
    };
    runtime
        .complete_suspended_media_resume(
            active,
            MediaInstanceId::from_non_zero(non_zero(52)),
            second_binding.binding_generation(),
            Some(warning),
        )
        .expect("warning rebound");
    assert_eq!(
        runtime.suspended_media_status(),
        ResumeCheckpointStatus::ResumedWithPositionWarning(warning)
    );

    let rebound = runtime
        .controller
        .as_ref()
        .expect("controller")
        .active_media()
        .expect("rebound active");
    runtime
        .capture_suspended_media_checkpoint(
            second_binding,
            &snapshot(rebound, PlaybackState::Paused, Duration::from_secs(57)),
        )
        .expect("second checkpoint");
    runtime.supersede_suspended_media_checkpoint();
    assert!(runtime.begin_suspended_media_resume(true).is_none());
    assert_eq!(
        runtime
            .controller
            .as_ref()
            .expect("controller")
            .active_media(),
        Some(rebound)
    );
    let explicitly_opened = install_external(&mut runtime, second_binding, 53);
    assert_ne!(explicitly_opened.lineage_id(), rebound.lineage_id());
}

#[test]
fn stale_old_binding_snapshot_cannot_replace_checkpoint() {
    let mut runtime = runtime();
    let first_binding = runtime.bind_resumed_app_state().expect("first binding");
    let active = install_external(&mut runtime, first_binding, 61);
    runtime.suspend_app_state_binding();
    let _second_binding = runtime.bind_resumed_app_state().expect("second binding");
    assert_eq!(
        runtime.capture_suspended_media_checkpoint(
            first_binding,
            &snapshot(active, PlaybackState::Paused, Duration::from_secs(1)),
        ),
        Err(ResumeCheckpointError::StalePlayerBinding)
    );
}
