use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use player_core::{MediaInstanceId, PlaybackState};
use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::{ControllerAppendOutcome, StablePlaybackIntent};
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, ActiveMediaLineageId, TransportActionOrigin,
};
use crate::process_shutdown::ShutdownDeadline;
use crate::url_service_adapter::{StartupUrlClassification, classify_startup_url};

fn runtime_with_queue(item_count: usize) -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let drafts = (0..item_count)
        .map(|index| {
            let filename = format!("queued-{index}.mkv");
            PlaylistItemDraft::local(
                LocalLocator::Native(PathBuf::from(&filename)),
                None,
                CachedPlaylistMetadata::new(filename, PlaylistMediaKind::Video),
            )
        })
        .collect();
    if item_count > 0 {
        assert!(matches!(
            runtime.controller.append(drafts).expect("queue fixture"),
            ControllerAppendOutcome::Added { .. }
        ));
    }
    runtime
}

fn classified_url(raw_url: &str) -> crate::url_service_adapter::StartupUrlLocator {
    let StartupUrlClassification::Supported(locator) = classify_startup_url(raw_url) else {
        panic!("test URL должен поддерживаться service registry");
    };
    locator
}

fn confirmation_action(
    model: &PendingQueueReplacementConfirmation,
    decision: QueueReplacementConfirmationDecision,
) -> QueueReplacementConfirmationAction {
    QueueReplacementConfirmationAction {
        intent_id: model.intent_id(),
        decision,
    }
}

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity must be non-zero")
}

fn pending_model(
    runtime: &PlaylistRuntime,
    admission: InAppQueueReplacementAdmission,
) -> PendingQueueReplacementConfirmation {
    match admission {
        InAppQueueReplacementAdmission::AwaitingConfirmation => runtime
            .pending_queue_replacement_confirmation()
            .expect("awaiting admission must publish one model"),
        InAppQueueReplacementAdmission::StartNow(_) => panic!("confirmation expected"),
    }
}

#[test]
fn empty_queue_gates_sensitive_direct_but_admits_local_and_yt_dlp() {
    let mut local_runtime = runtime_with_queue(0);
    assert!(matches!(
        local_runtime
            .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
                "movie.mkv"
            )))
            .expect("empty local admission"),
        InAppQueueReplacementAdmission::StartNow(AdmittedQueueReplacementIntent::LocalFile(_))
    ));

    let mut direct_runtime = runtime_with_queue(0);
    let direct = classified_url("https://media.example.test/movie.mp4?token=secret");
    assert!(matches!(
        direct_runtime
            .admit_in_app_queue_replacement(InAppQueueReplacementIntent::service_url(direct))
            .expect("empty direct admission"),
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    let direct_model = direct_runtime
        .pending_playlist_confirmation()
        .expect("D15 sensitive-only confirmation");
    assert!(!direct_model.reasons().queue_replacement());
    assert!(direct_model.reasons().sensitive_url_persistence());
    assert!(
        direct_runtime
            .pending_queue_replacement_confirmation()
            .is_none()
    );

    let mut yt_dlp_runtime = runtime_with_queue(0);
    let yt_dlp = classified_url("https://www.youtube.com/watch?v=abcdefghijk");
    assert!(matches!(
        yt_dlp_runtime
            .admit_in_app_queue_replacement(InAppQueueReplacementIntent::service_url(yt_dlp))
            .expect("empty YtDlp admission"),
        InAppQueueReplacementAdmission::StartNow(AdmittedQueueReplacementIntent::ServiceUrl(_))
    ));
}

#[test]
fn pre_load_sensitive_direct_open_is_gated_after_superseding_restore_apply() {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    let direct = classified_url("https://media.example.test/movie.mp4?token=secret");
    assert!(matches!(
        runtime
            .admit_in_app_queue_replacement(InAppQueueReplacementIntent::service_url(direct))
            .expect("pre-load sensitive direct admission"),
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    let model = runtime
        .pending_sensitive_url_persistence_decision()
        .expect("process-lifetime D15 decision");
    assert!(!model.reasons().queue_replacement());
    assert!(model.reasons().sensitive_url_persistence());
}

#[test]
fn pre_load_in_app_open_supersedes_restore_apply_without_starting_confirmation() {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    assert!(matches!(
        runtime
            .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
                "early.mkv"
            )))
            .expect("D65 startup draft admission"),
        InAppQueueReplacementAdmission::StartNow(AdmittedQueueReplacementIntent::LocalFile(_))
    ));
    assert!(runtime.pending_queue_replacement_confirmation().is_none());
}

#[test]
fn nonempty_local_and_url_do_not_reach_lower_layer_before_matching_confirm() {
    let mut runtime = runtime_with_queue(1);
    let dirty_before = runtime.controller.dirty_revision();
    let local_admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "/private/folder/selected.mkv",
        )))
        .expect("local admission");
    let local_model = pending_model(&runtime, local_admission);
    assert_eq!(runtime.controller.dirty_revision(), dirty_before);
    assert_eq!(local_model.safe_label(), "локальный media-файл");

    let secret_url = classified_url(
        "https://user:password@media.example.test/private/movie.mp4?token=secret#fragment",
    );
    let url_admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::service_url(secret_url))
        .expect("URL admission");
    assert!(matches!(
        url_admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    assert!(runtime.pending_queue_replacement_confirmation().is_none());
    let url_model = runtime
        .pending_playlist_confirmation()
        .expect("composed confirmation must use generalized model");
    assert_ne!(local_model.intent_id(), url_model.intent_id());
    assert!(url_model.reasons().queue_replacement());
    assert!(url_model.reasons().sensitive_url_persistence());
    assert_eq!(runtime.controller.dirty_revision(), dirty_before);
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirmation_action(
            &local_model,
            QueueReplacementConfirmationDecision::Confirm,
        )),
        QueueReplacementConfirmationOutcome::Stale
    ));
}

#[test]
fn confirm_consumes_exact_intent_once_and_cancel_is_complete_noop() {
    let mut runtime = runtime_with_queue(2);
    let dirty_before = runtime.controller.dirty_revision();
    let view_before = runtime.playlist_view_snapshot();
    let admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "once.mkv",
        )))
        .expect("confirmation");
    let model = pending_model(&runtime, admission);
    let confirm = confirmation_action(&model, QueueReplacementConfirmationDecision::Confirm);
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirm),
        QueueReplacementConfirmationOutcome::Confirmed(AdmittedQueueReplacementIntent::LocalFile(
            _
        ))
    ));
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirm),
        QueueReplacementConfirmationOutcome::Stale
    ));
    assert_eq!(runtime.controller.dirty_revision(), dirty_before);
    assert_eq!(
        runtime.playlist_view_snapshot().revision(),
        view_before.revision()
    );

    let cancel_admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "cancel.mkv",
        )))
        .expect("second confirmation");
    let cancel_model = pending_model(&runtime, cancel_admission);
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirmation_action(
            &cancel_model,
            QueueReplacementConfirmationDecision::Cancel,
        )),
        QueueReplacementConfirmationOutcome::Cancelled
    ));
    assert_eq!(runtime.controller.dirty_revision(), dirty_before);
}

#[test]
fn replacement_row_play_and_clear_supersede_but_selection_and_removal_preserve_prompt() {
    let mut runtime = runtime_with_queue(3);
    let first_admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "first.mkv",
        )))
        .expect("first confirmation");
    let first = pending_model(&runtime, first_admission);
    let selected_id = runtime.controller.queue().items()[1].item_id();
    assert!(runtime.controller.select_row(Some(selected_id)));
    assert_eq!(
        runtime
            .pending_queue_replacement_confirmation()
            .expect("selection preserves prompt")
            .intent_id(),
        first.intent_id()
    );
    let removed_id = runtime.controller.queue().items()[2].item_id();
    let _removal = runtime.remove_playlist_item(removed_id, Instant::now());
    assert_eq!(
        runtime
            .pending_queue_replacement_confirmation()
            .expect("compatible row removal preserves prompt")
            .intent_id(),
        first.intent_id()
    );

    let second_admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "second.mkv",
        )))
        .expect("replacement confirmation");
    let second = pending_model(&runtime, second_admission);
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirmation_action(
            &first,
            QueueReplacementConfirmationDecision::Confirm,
        )),
        QueueReplacementConfirmationOutcome::Stale
    ));
    runtime.supersede_queue_replacement_confirmation_for_row_play();
    assert!(runtime.pending_queue_replacement_confirmation().is_none());
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirmation_action(
            &second,
            QueueReplacementConfirmationDecision::Confirm,
        )),
        QueueReplacementConfirmationOutcome::Stale
    ));

    let clear_admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "clear.mkv",
        )))
        .expect("clear confirmation");
    let clear_model = pending_model(&runtime, clear_admission);
    let _clear = runtime.clear_playlist(Instant::now());
    assert!(runtime.pending_queue_replacement_confirmation().is_none());
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirmation_action(
            &clear_model,
            QueueReplacementConfirmationDecision::Confirm,
        )),
        QueueReplacementConfirmationOutcome::Stale
    ));
}

#[test]
fn current_playback_transport_and_selection_preserve_active_prompt_and_dirty_revision() {
    let mut runtime = runtime_with_queue(2);
    let active_item_id = runtime.controller.queue().items()[0].item_id();
    runtime
        .controller
        .queue
        .set_traversal_current(active_item_id)
        .expect("fixture current");
    let active = ActiveMediaIdentity::installed(
        Some(active_item_id),
        ActiveMediaLineageId::from_non_zero(non_zero(41)),
        MediaInstanceId::from_non_zero(non_zero(42)),
        PlaylistBindingGeneration(43),
    );
    runtime.controller.active_media = Some(active);
    let dirty_before = runtime.controller.dirty_revision();
    let admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "pending.mkv",
        )))
        .expect("confirmation");
    let model = pending_model(&runtime, admission);

    let _pause_dispatch = runtime
        .controller
        .record_stable_transport_intent(StablePlaybackIntent::Paused, TransportActionOrigin::Ui);
    assert!(
        !runtime
            .controller
            .observe_player_snapshot_state(PlaybackState::Seeking)
    );
    let selected_item_id = runtime.controller.queue().items()[1].item_id();
    assert!(runtime.controller.select_row(Some(selected_item_id)));

    assert_eq!(runtime.controller.active_media(), Some(active));
    assert_eq!(runtime.controller.dirty_revision(), dirty_before);
    assert_eq!(
        runtime
            .pending_queue_replacement_confirmation()
            .expect("current transport and selection preserve prompt")
            .intent_id(),
        model.intent_id()
    );
}

#[test]
fn suspend_recreation_preserves_prompt_and_shutdown_cancels_it() {
    let mut runtime = runtime_with_queue(1);
    let admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_file(PathBuf::from(
            "survive.mkv",
        )))
        .expect("confirmation");
    let model = pending_model(&runtime, admission);
    let first_binding = runtime.bind_resumed_app_state().expect("first binding");
    runtime.suspend_app_state_binding();
    let second_binding = runtime.bind_resumed_app_state().expect("second binding");
    assert_ne!(first_binding, second_binding);
    assert_eq!(
        runtime
            .pending_queue_replacement_confirmation()
            .expect("prompt survives AppState recreation")
            .intent_id(),
        model.intent_id()
    );

    assert!(matches!(
        runtime.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        crate::playlist_runtime::PlaylistTerminalShutdownOutcome::Completed(_)
    ));
    assert!(runtime.pending_queue_replacement_confirmation().is_none());
    assert!(matches!(
        runtime.respond_to_queue_replacement_confirmation(confirmation_action(
            &model,
            QueueReplacementConfirmationDecision::Confirm,
        )),
        QueueReplacementConfirmationOutcome::Stale
    ));
}

#[test]
fn trusted_cli_origin_bypasses_dialog_by_type_and_redaction_never_exposes_locator() {
    let mut runtime = runtime_with_queue(1);
    let trusted = TrustedStartupQueueReplacementIntent::local_file(PathBuf::from(
        "/foreign/private/startup.mkv",
    ));
    assert!(matches!(
        runtime
            .admit_trusted_startup_queue_replacement(trusted)
            .expect("trusted startup admission"),
        AdmittedQueueReplacementIntent::LocalFile(_)
    ));
    assert!(runtime.pending_queue_replacement_confirmation().is_none());

    let local_intent =
        InAppQueueReplacementIntent::local_file(PathBuf::from("/foreign/private/visible-name.mkv"));
    let local_debug = format!("{local_intent:?}");
    assert!(!local_debug.contains("visible-name.mkv"));
    assert!(!local_debug.contains("/foreign/private"));

    let raw_url =
        "https://user:password@media.example.test/private/movie.mp4?token=secret#fragment";
    let intent = InAppQueueReplacementIntent::service_url(classified_url(raw_url));
    let debug = format!("{intent:?}");
    for secret in ["password", "private/movie", "token=secret", "fragment"] {
        assert!(!debug.contains(secret));
    }
    let url_admission = runtime
        .admit_in_app_queue_replacement(intent)
        .expect("URL confirmation");
    assert!(matches!(
        url_admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    assert!(runtime.pending_queue_replacement_confirmation().is_none());
    let model = runtime
        .pending_playlist_confirmation()
        .expect("sensitive replacement uses generalized model");
    let model_debug = format!("{model:?}");
    for secret in ["password", "private/movie", "token=secret", "fragment"] {
        assert!(!model_debug.contains(secret));
    }
}
