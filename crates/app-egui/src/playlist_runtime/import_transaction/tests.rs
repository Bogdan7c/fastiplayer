use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use player_core::{MediaInstanceId, PlaybackState};
use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistCompoundImportDraft,
    PlaylistImportAvailability, PlaylistImportEntryDraft, PlaylistImportProvenance,
    PlaylistImportSourceKind, PlaylistItemDraft, PlaylistMediaKind, PlaylistSingleImportDraft,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, ControllerManualNavigationOutcome,
    DiscoveryManualWaitAvailability, ManualNavigationFailureOutcome, PlaylistController,
    PreviousRestartThreshold,
};
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};
use crate::playlist_runtime::replacement_confirmation::{
    PlaylistConfirmationAction, PlaylistConfirmationReason, QueueReplacementConfirmationDecision,
};
use crate::playlist_runtime::{PlaylistBindingGeneration, TransportActionOrigin};
use crate::process_shutdown::ShutdownDeadline;

fn metadata(label: &str) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(label, PlaylistMediaKind::Unknown)
}

fn provenance(root: DurableReopenLocator) -> PlaylistImportProvenance {
    PlaylistImportProvenance::new(root, PlaylistImportSourceKind::M3u, None)
}

fn single(label: &str) -> PlaylistSingleImportDraft {
    let locator =
        DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(format!("/{label}.mkv"))));
    PlaylistSingleImportDraft::new(
        locator.clone(),
        metadata(label),
        None,
        Vec::new(),
        provenance(locator),
        PlaylistImportAvailability::Available,
    )
    .expect("focused single import draft")
}

fn compound(label: &str, part_count: usize) -> PlaylistImportEntryDraft {
    let root = DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(format!(
        "/{label}.collection"
    ))));
    let parts = (0..part_count)
        .map(|index| single(&format!("{label}-{index}")))
        .collect();
    PlaylistImportEntryDraft::Compound(
        PlaylistCompoundImportDraft::new(root.clone(), metadata(label), provenance(root), parts)
            .expect("focused compound import draft"),
    )
}

fn runtime() -> PlaylistRuntime {
    let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime =
        PlaylistRuntime::new_with_config(wake, rustiplayer_config::PlaylistConfig::default());
    runtime.controller.install(PlaylistController::new());
    runtime
}

fn runtime_with_active_old_queue() -> (PlaylistRuntime, ActiveMediaIdentity) {
    let mut runtime = runtime();
    let old_item_id = match runtime
        .controller
        .append(vec![PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from("/old.mkv")),
            None,
            metadata("old"),
        )])
        .expect("old row")
    {
        crate::playlist_runtime::controller::ControllerAppendOutcome::Added {
            item_ids, ..
        } => item_ids[0],
        crate::playlist_runtime::controller::ControllerAppendOutcome::NoItemsProvided => {
            panic!("focused fixture must append one row")
        }
    };
    runtime
        .controller
        .queue
        .set_traversal_current(old_item_id)
        .expect("old current");
    let active = ActiveMediaIdentity::installed(
        Some(old_item_id),
        ActiveMediaLineageId::from_non_zero(NonZeroU64::new(31).expect("non-zero")),
        MediaInstanceId::from_non_zero(NonZeroU64::new(41).expect("non-zero")),
        PlaylistBindingGeneration(51),
    );
    runtime.controller.active_media = Some(active);
    (runtime, active)
}

fn confirm_interactive_replacement(
    runtime: &mut PlaylistRuntime,
    entries: Vec<PlaylistImportEntryDraft>,
) {
    let preview = runtime
        .stage_playlist_import(
            PlaylistImportIntent::ReplaceQueue,
            PlaylistImportDraft::new(entries, Vec::new(), None, 0),
        )
        .expect("replace preview");
    assert_eq!(
        runtime.continue_playlist_import(preview.preview_id()),
        PlaylistImportContinueOutcome::AwaitingConfirmation
    );
    assert!(runtime.pending_queue_replacement_confirmation().is_none());
    let model = runtime
        .pending_playlist_confirmation()
        .expect("replacement confirmation");
    let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Confirm,
    });
    assert!(matches!(
        outcome,
        crate::playlist_runtime::actions::PlaylistConfirmationApplyOutcome::Import(
            PlaylistImportContinueOutcome::Committed(_)
        )
    ));
}

#[test]
fn append_commits_singles_and_groups_only_after_explicit_continue() {
    let mut runtime = runtime();
    let preview = runtime
        .stage_playlist_import(
            PlaylistImportIntent::AppendToQueue,
            PlaylistImportDraft::new(
                vec![single("single").into(), compound("group", 2)],
                Vec::new(),
                None,
                0,
            ),
        )
        .expect("preview");

    assert_eq!(preview.accepted().singles(), 1);
    assert_eq!(preview.accepted().groups(), 1);
    assert_eq!(preview.accepted().retained_items(), 3);
    assert_eq!(runtime.controller.queue().retained_item_count(), 0);
    assert!(matches!(
        runtime.continue_playlist_import(preview.preview_id()),
        PlaylistImportContinueOutcome::Committed(ControllerImportCommitOutcome::Committed { .. })
    ));
    let controller = runtime
        .playlist_controller()
        .expect("focused runtime owns controller");
    assert_eq!(controller.queue().top_level_entry_count(), 2);
    assert_eq!(controller.queue().retained_item_count(), 3);
    assert_eq!(
        controller
            .queue()
            .next_item_id_snapshot()
            .expose_value_for_persistence(),
        4
    );
    assert_eq!(
        controller
            .queue()
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        2
    );
}

#[test]
fn composed_sensitive_and_replacement_reasons_use_one_ordered_slot() {
    let mut runtime = runtime();
    runtime
        .controller
        .append(vec![PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from("/old.mkv")),
            None,
            metadata("old"),
        )])
        .expect("old row");
    let preview = runtime
        .stage_playlist_import(
            PlaylistImportIntent::ReplaceQueue,
            PlaylistImportDraft::new(vec![single("new").into()], Vec::new(), None, 1),
        )
        .expect("preview");

    assert_eq!(
        runtime.continue_playlist_import(preview.preview_id()),
        PlaylistImportContinueOutcome::AwaitingConfirmation
    );
    let model = runtime
        .pending_playlist_confirmation()
        .expect("one generalized slot");
    assert_eq!(
        model.reasons().ordered().collect::<Vec<_>>(),
        vec![
            PlaylistConfirmationReason::SensitiveDurableLocatorPersistence,
            PlaylistConfirmationReason::QueueReplacement,
        ]
    );
    assert_eq!(
        runtime
            .pending_playlist_import_preview()
            .map(|preview| preview.preview_id()),
        Some(preview.preview_id())
    );

    let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Confirm,
    });
    assert!(matches!(
        outcome,
        crate::playlist_runtime::actions::PlaylistConfirmationApplyOutcome::Import(
            PlaylistImportContinueOutcome::Committed(_)
        )
    ));
    assert!(runtime.pending_playlist_confirmation().is_none());
    assert!(runtime.pending_playlist_import_preview().is_none());
    assert_eq!(runtime.controller.queue().retained_item_count(), 1);
}

#[test]
fn partial_preview_is_bounded_and_cancelled_confirmation_is_mutation_free() {
    let (mut runtime, old_active) = runtime_with_active_old_queue();
    let issues = (0..130)
        .map(|_| PlaylistImportIssue::new(PlaylistImportIssueKind::SourceRejectedEntry))
        .collect();
    let preview = runtime
        .stage_playlist_import(
            PlaylistImportIntent::ReplaceQueue,
            PlaylistImportDraft::new(
                vec![single("partial").into()],
                issues,
                Some(PlaylistImportSourceTruncation::new(
                    PlaylistImportRejectedCount::AtLeast(2),
                )),
                0,
            ),
        )
        .expect("partial preview");

    assert!(preview.requires_partial_decision());
    assert_eq!(preview.issues().len(), MAX_PLAYLIST_IMPORT_PREVIEW_ISSUES);
    assert_eq!(preview.omitted_issue_count(), 2);
    assert_eq!(
        preview
            .source_truncation()
            .map(PlaylistImportSourceTruncation::rejected_entries),
        Some(PlaylistImportRejectedCount::AtLeast(2))
    );
    assert_eq!(
        runtime.continue_playlist_import(preview.preview_id()),
        PlaylistImportContinueOutcome::AwaitingConfirmation
    );
    let model = runtime
        .pending_playlist_confirmation()
        .expect("replacement confirmation");
    let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Cancel,
    });

    assert!(matches!(
        outcome,
        crate::playlist_runtime::actions::PlaylistConfirmationApplyOutcome::Cancelled
    ));
    assert_eq!(runtime.controller.queue.retained_item_count(), 1);
    assert_eq!(runtime.controller.active_media, Some(old_active));
    assert!(runtime.pending_playlist_import_preview().is_none());
}

#[test]
fn capped_prefix_never_splits_compound_or_admits_following_tail() {
    let entries = vec![
        single("accepted").into(),
        compound("group", 2),
        single("tail").into(),
    ];
    let (accepted, truncation) = capped_import_prefix(entries, 2);

    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].retained_item_count(), 1);
    assert_eq!(
        truncation,
        Some(PlaylistImportCapacityTruncation {
            rejected_entries: 2,
            rejected_items: 3,
        })
    );
}

#[test]
fn zero_and_full_capacity_prefixes_are_explicit() {
    let (zero, zero_truncation) = capped_import_prefix(vec![single("zero").into()], 0);
    assert!(zero.is_empty());
    assert_eq!(
        zero_truncation,
        Some(PlaylistImportCapacityTruncation {
            rejected_entries: 1,
            rejected_items: 1,
        })
    );

    let (full, full_truncation) = capped_import_prefix(
        vec![single("one").into(), compound("two", 2)],
        MAX_PLAYLIST_ITEMS,
    );
    assert_eq!(full.len(), 2);
    assert_eq!(count_entries(&full).retained_items(), 3);
    assert_eq!(full_truncation, None);
}

#[test]
fn startup_replace_bypasses_prompt_and_keeps_special_detached_disposition_off() {
    let (mut runtime, old_active) = runtime_with_active_old_queue();
    let preview = runtime
        .stage_playlist_import(
            PlaylistImportIntent::StartupReplace,
            PlaylistImportDraft::new(vec![single("startup").into()], Vec::new(), None, 1),
        )
        .expect("startup preview");

    assert!(matches!(
        runtime.continue_playlist_import(preview.preview_id()),
        PlaylistImportContinueOutcome::Committed(_)
    ));
    assert!(runtime.pending_playlist_confirmation().is_none());
    assert_eq!(runtime.controller.active_media, Some(old_active.detached()));
    assert!(
        runtime
            .controller
            .replacement_detached_disposition
            .is_none()
    );
    assert_eq!(runtime.controller.queue.retained_item_count(), 1);
}

#[test]
fn interactive_replace_detaches_without_clear_reset_or_removal_continuation() {
    let (mut runtime, old_active) = runtime_with_active_old_queue();
    confirm_interactive_replacement(
        &mut runtime,
        vec![single("first").into(), single("last").into()],
    );

    assert_eq!(runtime.controller.active_media, Some(old_active.detached()));
    assert!(runtime.controller.detached_active_tombstone.is_none());
    assert!(
        runtime
            .controller
            .replacement_detached_disposition
            .is_some()
    );
    assert_eq!(runtime.controller.queue.traversal_current(), None);
    assert!(!runtime.has_pending_media_reset());
}

#[test]
fn detached_clean_ended_stops_exactly_once() {
    let (mut runtime, old_active) = runtime_with_active_old_queue();
    confirm_interactive_replacement(&mut runtime, vec![single("new").into()]);

    let first = runtime.controller.observe_automatic_snapshot(
        old_active.player_binding_generation(),
        Some(old_active.media_instance_id()),
        PlaybackState::Ended,
        crate::playlist_runtime::controller::EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    assert!(matches!(
        first,
        AutomaticLifecycleOutcome::Stop {
            item_id: None,
            media_instance_id,
            ..
        } if media_instance_id == old_active.media_instance_id()
    ));
    assert!(matches!(
        runtime.controller.observe_automatic_snapshot(
            old_active.player_binding_generation(),
            Some(old_active.media_instance_id()),
            PlaybackState::Ended,
            crate::playlist_runtime::controller::EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::NoAction
    ));
}

#[test]
fn detached_next_and_previous_choose_first_and_last_new_targets() {
    for (direction, expected_index) in [
        (playlist_core::ManualNavigationDirection::Next, 0usize),
        (playlist_core::ManualNavigationDirection::Previous, 3usize),
    ] {
        let (mut runtime, _old_active) = runtime_with_active_old_queue();
        runtime
            .controller
            .queue
            .enable_shuffle()
            .expect("focused shuffle");
        confirm_interactive_replacement(
            &mut runtime,
            vec![
                single("first").into(),
                compound("middle", 2),
                single("last").into(),
            ],
        );
        let expected_item_id = runtime
            .controller
            .queue
            .iter_playable_ids()
            .nth(expected_index)
            .expect("expected source-order target");
        let outcome = runtime.controller.manual_navigation(
            direction,
            TransportActionOrigin::Ui,
            Duration::from_secs(10),
            PreviousRestartThreshold::from_milliseconds(3_000).expect("threshold"),
            DiscoveryManualWaitAvailability::Exhausted,
        );
        assert!(matches!(
            outcome,
            ControllerManualNavigationOutcome::StartInstall { install }
                if install.item_id == expected_item_id
        ));
        assert_eq!(
            runtime
                .controller
                .report_unstaged_manual_navigation_target_failure(expected_item_id),
            ManualNavigationFailureOutcome::AwaitingUserAfterFailure {
                item_id: expected_item_id,
            }
        );
    }
}

#[test]
fn stale_cancel_and_shutdown_never_mutate_queue() {
    let mut runtime = runtime();
    let first = runtime
        .stage_playlist_import(
            PlaylistImportIntent::AppendToQueue,
            PlaylistImportDraft::new(vec![single("first").into()], Vec::new(), None, 0),
        )
        .expect("first");
    let second = runtime
        .stage_playlist_import(
            PlaylistImportIntent::AppendToQueue,
            PlaylistImportDraft::new(vec![single("second").into()], Vec::new(), None, 0),
        )
        .expect("second");

    assert_eq!(
        runtime.continue_playlist_import(first.preview_id()),
        PlaylistImportContinueOutcome::Stale
    );
    assert!(runtime.cancel_playlist_import(second.preview_id()));
    assert_eq!(runtime.controller.queue().retained_item_count(), 0);

    let shutdown_preview = runtime
        .stage_playlist_import(
            PlaylistImportIntent::AppendToQueue,
            PlaylistImportDraft::new(vec![single("shutdown").into()], Vec::new(), None, 0),
        )
        .expect("shutdown preview");
    let _shutdown = runtime.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1)));
    assert!(runtime.pending_playlist_import_preview().is_none());
    assert_eq!(
        runtime.continue_playlist_import(shutdown_preview.preview_id()),
        PlaylistImportContinueOutcome::RuntimeClosed
    );
    assert_eq!(runtime.controller.queue().retained_item_count(), 0);
}

#[test]
fn structural_stale_failure_preserves_import_ids_and_queue() {
    let mut runtime = runtime();
    let preview = runtime
        .stage_playlist_import(
            PlaylistImportIntent::AppendToQueue,
            PlaylistImportDraft::new(vec![compound("stale", 2)], Vec::new(), None, 0),
        )
        .expect("preview");
    runtime
        .controller
        .append(vec![PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from("/winner.mkv")),
            None,
            metadata("winner"),
        )])
        .expect("serialized winner");
    let item_watermark = runtime.controller.queue.next_item_id_snapshot();
    let group_watermark = runtime.controller.queue.next_compound_group_id_snapshot();

    assert_eq!(
        runtime.continue_playlist_import(preview.preview_id()),
        PlaylistImportContinueOutcome::Stale
    );
    assert_eq!(runtime.controller.queue.retained_item_count(), 1);
    assert_eq!(
        runtime.controller.queue.next_item_id_snapshot(),
        item_watermark
    );
    assert_eq!(
        runtime.controller.queue.next_compound_group_id_snapshot(),
        group_watermark
    );
}
