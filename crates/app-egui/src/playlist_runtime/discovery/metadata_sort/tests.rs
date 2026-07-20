use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use media_core::MediaTagMetadata;
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, LocalSourceFingerprint, PlaylistItemDraft,
    PlaylistLocator, PlaylistMediaKind, PlaylistMetadataPatch, PlaylistSortKey, SecretUrlLocator,
    SortCanonicalQueue, SortDirection,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::PlaylistRuntime;
use crate::playlist_runtime::controller::{
    PlaylistController, PlaylistControllerInvariantViolation,
};
use crate::playlist_runtime::removal_undo::RemovalUndoOutcome;

struct DiscoveryWake;

impl playlist_discovery::DiscoveryWakePort for DiscoveryWake {
    fn wake(&self) -> Result<(), playlist_discovery::WakeDisconnected> {
        Ok(())
    }
}

struct PartialProbe;

impl playlist_discovery::DiscoveryProbe for PartialProbe {
    fn read_fingerprint(
        &self,
        _locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<
        playlist_discovery::LocalMediaFingerprint,
        playlist_discovery::ProbeOneLocalMediaError,
    > {
        Ok(playlist_discovery::LocalMediaFingerprint::new(
            20,
            SystemTime::UNIX_EPOCH,
        ))
    }

    fn probe(
        &self,
        locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<playlist_discovery::ProbedLocalMedia, playlist_discovery::ProbeOneLocalMediaError>
    {
        let filename = locator.file_name().unwrap().to_string_lossy().into_owned();
        if filename.starts_with("fail") {
            return Err(playlist_discovery::ProbeOneLocalMediaError::ProbeFailure {
                reason: "safe test failure".to_owned(),
            });
        }
        let tags = MediaTagMetadata {
            title: Some("Alpha".to_owned()),
            ..MediaTagMetadata::default()
        };
        Ok(playlist_discovery::ProbedLocalMedia::new(
            filename,
            playlist_discovery::LocalMediaKind::VideoContaining,
            None,
            tags,
            playlist_discovery::LocalMediaFingerprint::new(20, SystemTime::UNIX_EPOCH),
        ))
    }
}

fn local_draft(path: &str, title: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(path)),
        Some(LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH)),
        CachedPlaylistMetadata::new(path, PlaylistMediaKind::Audio)
            .with_title(Some(title.to_owned())),
    )
}

fn drain_terminal(
    owner: &mut MetadataSortOwner,
    executor: &BoundedExecutor,
    structural_revision: PlaylistStructuralRevision,
) -> MetadataSortTerminal {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(terminal) = owner.drain(Some(executor), structural_revision) {
            return terminal;
        }
        assert!(Instant::now() < deadline, "metadata sort did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn all_cached_and_natural_sort_use_cpu_executor_without_discovery_probe() {
    for intent in [
        SortCanonicalQueue::new(PlaylistSortKey::Title, SortDirection::Ascending),
        SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Ascending),
    ] {
        let mut controller = PlaylistController::new();
        controller
            .append(vec![
                local_draft("/music/z.flac", "Zulu"),
                local_draft("/music/a.flac", "Alpha"),
            ])
            .unwrap();
        let structural_revision = controller.view_snapshot().structural_revision();
        let cpu_executor = start_cpu_executor().unwrap();
        let mut owner =
            MetadataSortOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        owner
            .start(None, Some(&cpu_executor), &controller, intent)
            .unwrap();
        let MetadataSortTerminal::Prepared {
            prepared, patches, ..
        } = drain_terminal(&mut owner, &cpu_executor, structural_revision)
        else {
            panic!("cached sort must reach prepared terminal");
        };
        assert!(patches.is_empty());
        let commit = controller
            .preflight_canonical_sort(structural_revision, prepared, patches)
            .unwrap();
        let outcome = controller.commit_canonical_sort(commit);
        assert!(outcome.domain.reordered());
        assert_eq!(outcome.dirty.unwrap().revision().get(), 2);
    }
}

#[test]
fn url_with_missing_metadata_never_requires_discovery_executor() {
    let mut controller = PlaylistController::new();
    controller
        .append(vec![PlaylistItemDraft::url(
            SecretUrlLocator::from_reopenable_url("https://example.test/video").unwrap(),
            CachedPlaylistMetadata::new("video", PlaylistMediaKind::Unknown),
        )])
        .unwrap();
    let structural_revision = controller.view_snapshot().structural_revision();
    let cpu_executor = start_cpu_executor().unwrap();
    let mut owner =
        MetadataSortOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    owner
        .start(
            None,
            Some(&cpu_executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::Title, SortDirection::Ascending),
        )
        .unwrap();
    assert!(matches!(
        drain_terminal(&mut owner, &cpu_executor, structural_revision),
        MetadataSortTerminal::Prepared { patches, .. } if patches.is_empty()
    ));
}

#[test]
fn cpu_cancel_returns_salvage_without_partial_reorder() {
    let mut controller = PlaylistController::new();
    controller
        .append(
            (0..10_000)
                .rev()
                .map(|index| {
                    local_draft(
                        &format!("/music/episode {index}.flac"),
                        &format!("Episode {index}"),
                    )
                })
                .collect(),
        )
        .unwrap();
    let ids_before = controller.queue().iter_playable_ids().collect::<Vec<_>>();
    let structural_revision = controller.view_snapshot().structural_revision();
    let cpu_executor = start_cpu_executor().unwrap();
    let mut owner =
        MetadataSortOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    let job_id = owner
        .start(
            None,
            Some(&cpu_executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Ascending),
        )
        .unwrap();
    assert_eq!(owner.cancel(job_id), MetadataSortCancelOutcome::Requested);
    assert!(matches!(
        drain_terminal(&mut owner, &cpu_executor, structural_revision),
        MetadataSortTerminal::Salvage {
            outcome: MetadataSortTerminalOutcome::Cancelled,
            patches,
            ..
        } if patches.is_empty()
    ));
    assert_eq!(
        controller.queue().iter_playable_ids().collect::<Vec<_>>(),
        ids_before
    );
}

fn runtime_with_items(item_count: usize) -> (PlaylistRuntime, Vec<PlaylistItemId>) {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let outcome = runtime
        .controller
        .as_mut()
        .unwrap()
        .append(
            (0..item_count)
                .map(|index| {
                    local_draft(
                        &format!("/music/item-{index}.flac"),
                        &format!("Item {index}"),
                    )
                })
                .collect(),
        )
        .unwrap();
    let crate::playlist_runtime::controller::ControllerAppendOutcome::Added { item_ids, .. } =
        outcome
    else {
        panic!("fixture must append rows");
    };
    (runtime, item_ids)
}

#[test]
fn salvage_zero_one_and_many_keep_revalidation_and_undo_ordering() {
    let now = Instant::now();
    let (mut changed_runtime, changed_ids) = runtime_with_items(3);
    let _removed = changed_runtime.remove_playlist_item(changed_ids[0], now);
    let changed_item = changed_runtime
        .controller
        .as_ref()
        .unwrap()
        .queue()
        .item(changed_ids[1])
        .unwrap();
    let changed_patch = PlaylistMetadataPatch::new(
        changed_ids[1],
        changed_item.locator().clone(),
        changed_item.local_fingerprint(),
        changed_item
            .cached_metadata()
            .clone()
            .with_title(Some("Updated".to_owned())),
    );
    let completion = changed_runtime.apply_metadata_sort_salvage(
        MetadataSortJobId(90),
        MetadataSortTerminalOutcome::Cancelled,
        vec![changed_patch],
        DiscoveryFailureCounts::default(),
        Arc::from([]),
        0,
    );
    assert_eq!(completion.metadata_updated, 1);
    assert_eq!(
        changed_runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Unavailable
    );

    let (mut unchanged_runtime, unchanged_ids) = runtime_with_items(3);
    let _removed = unchanged_runtime.remove_playlist_item(unchanged_ids[0], now);
    let unchanged_item = unchanged_runtime
        .controller
        .as_ref()
        .unwrap()
        .queue()
        .item(unchanged_ids[1])
        .unwrap();
    let unchanged_patch = PlaylistMetadataPatch::new(
        unchanged_ids[1],
        unchanged_item.locator().clone(),
        unchanged_item.local_fingerprint(),
        unchanged_item.cached_metadata().clone(),
    );
    let completion = unchanged_runtime.apply_metadata_sort_salvage(
        MetadataSortJobId(91),
        MetadataSortTerminalOutcome::Cancelled,
        vec![unchanged_patch],
        DiscoveryFailureCounts::default(),
        Arc::from([]),
        0,
    );
    assert_eq!(completion.metadata_updated, 0);
    assert!(unchanged_runtime.removal_undo_status(now).is_some());

    let (mut many_runtime, many_ids) = runtime_with_items(5);
    let _removed = many_runtime.remove_playlist_item(many_ids[0], now);
    let patches = many_ids[1..]
        .iter()
        .enumerate()
        .map(|(patch_index, item_id)| {
            let item = many_runtime
                .controller
                .as_ref()
                .unwrap()
                .queue()
                .item(*item_id)
                .unwrap();
            let expected_locator = if patch_index == 2 {
                PlaylistLocator::Local(LocalLocator::Native("/music/replaced.flac".into()))
            } else {
                item.locator().clone()
            };
            let expected_fingerprint = if patch_index == 3 {
                Some(LocalSourceFingerprint::new(999, SystemTime::UNIX_EPOCH))
            } else {
                item.local_fingerprint()
            };
            PlaylistMetadataPatch::new(
                *item_id,
                expected_locator,
                expected_fingerprint,
                item.cached_metadata()
                    .clone()
                    .with_title(Some(format!("Updated {patch_index}"))),
            )
        })
        .collect();
    let completion = many_runtime.apply_metadata_sort_salvage(
        MetadataSortJobId(92),
        MetadataSortTerminalOutcome::Invalidated,
        patches,
        DiscoveryFailureCounts::default(),
        Arc::from([]),
        0,
    );
    assert_eq!(completion.metadata_updated, 2);
    assert_eq!(
        many_runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Unavailable
    );
    assert_eq!(
        many_runtime
            .controller
            .as_ref()
            .unwrap()
            .queue()
            .item(many_ids[3])
            .unwrap()
            .cached_metadata()
            .title(),
        Some("Item 3")
    );
    assert_eq!(
        many_runtime
            .controller
            .as_ref()
            .unwrap()
            .queue()
            .item(many_ids[4])
            .unwrap()
            .cached_metadata()
            .title(),
        Some("Item 4")
    );
}

#[test]
fn rejected_salvage_preflight_preserves_undo_for_fatal_and_revision_exhaustion() {
    let now = Instant::now();
    for rejection in ["fatal", "dirty-exhausted"] {
        let (mut runtime, item_ids) = runtime_with_items(3);
        let _removed = runtime.remove_playlist_item(item_ids[0], now);
        let item = runtime
            .controller
            .as_ref()
            .unwrap()
            .queue()
            .item(item_ids[1])
            .unwrap();
        let patch = PlaylistMetadataPatch::new(
            item_ids[1],
            item.locator().clone(),
            item.local_fingerprint(),
            item.cached_metadata()
                .clone()
                .with_title(Some("Rejected update".to_owned())),
        );
        let controller = runtime.controller.as_mut().unwrap();
        match rejection {
            "fatal" => {
                controller.fatal_invariant =
                    Some(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
            }
            "dirty-exhausted" => {
                controller.force_metadata_dirty_revision_exhaustion_for_test();
            }
            _ => unreachable!("fixed rejection matrix"),
        }

        let completion = runtime.apply_metadata_sort_salvage(
            MetadataSortJobId(100),
            MetadataSortTerminalOutcome::Invalidated,
            vec![patch],
            DiscoveryFailureCounts::default(),
            Arc::from([]),
            0,
        );
        assert_eq!(completion.outcome, MetadataSortTerminalOutcome::Failed);
        assert_eq!(completion.metadata_updated, 0);
        assert!(runtime.removal_undo_status(now).is_some());
        assert_eq!(
            runtime
                .controller
                .as_ref()
                .unwrap()
                .queue()
                .item(item_ids[1])
                .unwrap()
                .cached_metadata()
                .title(),
            Some("Item 1")
        );
    }
}

#[test]
fn clear_invalidates_sort_without_resurrecting_snapshot_items() {
    let mut controller = PlaylistController::new();
    controller
        .append(
            (0..10_000)
                .map(|index| {
                    local_draft(
                        &format!("/music/episode-{index}.flac"),
                        &format!("Episode {index}"),
                    )
                })
                .collect(),
        )
        .unwrap();
    let cpu_executor = start_cpu_executor().unwrap();
    let mut owner =
        MetadataSortOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    let job_id = owner
        .start(
            None,
            Some(&cpu_executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Descending),
        )
        .unwrap();

    owner.cancel_for_queue_replacement();
    assert_eq!(
        owner.cancel(job_id),
        MetadataSortCancelOutcome::AlreadyInvalidated
    );
    assert_eq!(
        owner.cancel(MetadataSortJobId(job_id.0 - 1)),
        MetadataSortCancelOutcome::StaleJob
    );
    controller.clear_queue();
    let structural_revision = controller.view_snapshot().structural_revision();
    assert!(matches!(
        drain_terminal(&mut owner, &cpu_executor, structural_revision),
        MetadataSortTerminal::Salvage {
            outcome: MetadataSortTerminalOutcome::Invalidated,
            patches,
            ..
        } if patches.is_empty()
    ));
    assert!(controller.queue().is_empty());
}

#[test]
fn queued_cancel_notifies_wake_before_terminal_drain_and_is_first_writer_wins() {
    let executor = BoundedExecutor::start(ExecutorConfig::new(
        NonZeroUsize::new(1).unwrap(),
        NonZeroUsize::new(2).unwrap(),
        "metadata-sort-wake-test",
    ))
    .unwrap();
    let gate = Arc::new(std::sync::Barrier::new(2));
    let (started_sender, started_receiver) = std::sync::mpsc::sync_channel(1);
    let active_gate = Arc::clone(&gate);
    let blocker = executor
        .try_submit(move |_| {
            started_sender.send(()).unwrap();
            active_gate.wait();
        })
        .unwrap();
    started_receiver.recv().unwrap();

    let mut controller = PlaylistController::new();
    controller
        .append(vec![local_draft("/music/queued.flac", "Queued")])
        .unwrap();
    let structural_revision = controller.view_snapshot().structural_revision();
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let wake_epoch_before = wake_port.publish_epoch_for_test();
    let mut owner = MetadataSortOwner::new(wake_port.clone());
    let job_id = owner
        .start(
            None,
            Some(&executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Ascending),
        )
        .unwrap();
    assert_eq!(owner.cancel(job_id), MetadataSortCancelOutcome::Requested);
    assert_eq!(
        owner.cancel(job_id),
        MetadataSortCancelOutcome::AlreadyRequested
    );
    gate.wait();

    let deadline = Instant::now() + Duration::from_secs(2);
    while wake_port.publish_epoch_for_test() == wake_epoch_before {
        assert!(
            Instant::now() < deadline,
            "terminal notifier did not wake app"
        );
        std::thread::yield_now();
    }
    assert_eq!(blocker.try_take(), TaskPoll::Completed(()));
    assert!(matches!(
        drain_terminal(&mut owner, &executor, structural_revision),
        MetadataSortTerminal::Salvage {
            outcome: MetadataSortTerminalOutcome::Cancelled,
            ..
        }
    ));
}

#[test]
fn panicked_cpu_task_notifies_wake_and_drains_as_failed_salvage() {
    let executor = start_cpu_executor().unwrap();
    let mut controller = PlaylistController::new();
    controller
        .append(vec![local_draft("/music/panic.flac", "Panic")])
        .unwrap();
    let structural_revision = controller.view_snapshot().structural_revision();
    let snapshot = controller.queue().canonical_sort_snapshot();
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let wake_epoch_before = wake_port.publish_epoch_for_test();
    let terminal_wake = wake_port.clone();
    let handle = executor
        .try_submit_with_terminal_notifier::<_, Result<PreparedCanonicalSort, _>, _>(
            |_| panic!("expected CPU preparation panic"),
            move || {
                let _delivery = terminal_wake.request_wake();
            },
        )
        .unwrap();
    let mut owner = MetadataSortOwner::new(wake_port.clone());
    owner.active = Some(ActiveMetadataSort {
        job_id: MetadataSortJobId(77),
        expected_structural_revision: structural_revision,
        snapshot,
        intent: SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Ascending),
        patches: Vec::new(),
        phase: ActivePhase::Cpu(CpuPhase { handle }),
        cancel_outcome: None,
        failure_counts: DiscoveryFailureCounts::default(),
        diagnostics: Arc::from([]),
        omitted_diagnostics: 0,
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    while wake_port.publish_epoch_for_test() == wake_epoch_before {
        assert!(Instant::now() < deadline, "panic terminal did not wake app");
        std::thread::yield_now();
    }
    assert!(matches!(
        owner.drain(Some(&executor), structural_revision),
        Some(MetadataSortTerminal::Salvage {
            outcome: MetadataSortTerminalOutcome::Failed,
            ..
        })
    ));
}

#[test]
fn maximum_job_id_is_used_once_then_reports_typed_exhaustion() {
    let mut controller = PlaylistController::new();
    controller
        .append(vec![local_draft("/music/max.flac", "Max")])
        .unwrap();
    let structural_revision = controller.view_snapshot().structural_revision();
    let executor = start_cpu_executor().unwrap();
    let mut owner =
        MetadataSortOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    owner.next_job_id = Some(u64::MAX);
    let job_id = owner
        .start(
            None,
            Some(&executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Ascending),
        )
        .unwrap();
    assert_eq!(job_id, MetadataSortJobId(u64::MAX));
    let _terminal = drain_terminal(&mut owner, &executor, structural_revision);
    assert!(matches!(
        owner.start(
            None,
            Some(&executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Ascending),
        ),
        Err(MetadataSortStartError::JobIdExhausted)
    ));
}

#[test]
fn partial_probe_failure_still_prepares_sort_and_reports_typed_warning_count() {
    let mut controller = PlaylistController::new();
    controller
        .append(vec![
            PlaylistItemDraft::local(
                LocalLocator::Native("/music/good.mkv".into()),
                Some(LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH)),
                CachedPlaylistMetadata::new("good.mkv", PlaylistMediaKind::Video),
            ),
            PlaylistItemDraft::local(
                LocalLocator::Native("/music/fail.mkv".into()),
                None,
                CachedPlaylistMetadata::new("fail.mkv", PlaylistMediaKind::Video),
            ),
        ])
        .unwrap();
    let structural_revision = controller.view_snapshot().structural_revision();
    let discovery_executor = playlist_discovery::DiscoveryExecutor::start_with_probe(
        Arc::new(PartialProbe),
        Arc::new(DiscoveryWake),
    )
    .unwrap();
    let cpu_executor = start_cpu_executor().unwrap();
    let mut owner =
        MetadataSortOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    owner
        .start(
            Some(&discovery_executor),
            Some(&cpu_executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::Title, SortDirection::Ascending),
        )
        .unwrap();
    let MetadataSortTerminal::Prepared {
        prepared,
        patches,
        failure_counts,
        ..
    } = drain_terminal(&mut owner, &cpu_executor, structural_revision)
    else {
        panic!("individual probe failure must not abort the sort");
    };
    assert_eq!(patches.len(), 1);
    assert_eq!(failure_counts.probe_failed, 1);
    let commit = controller
        .preflight_canonical_sort(structural_revision, prepared, patches)
        .unwrap();
    let outcome = controller.commit_canonical_sort(commit);
    assert_eq!(outcome.domain.metadata().applied_count(), 1);
}
