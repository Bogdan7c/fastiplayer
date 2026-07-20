use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::{self, SyncSender};
use std::time::{Duration, Instant};

use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistCompoundImportDraft,
    PlaylistImportAvailability, PlaylistImportEntryDraft, PlaylistImportProvenance,
    PlaylistImportSourceKind, PlaylistMediaKind, PlaylistMetadataPatch, PlaylistSingleImportDraft,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::actions::{PlaylistConfirmationApplyOutcome, UrlAppendActionOutcome};
use crate::playlist_runtime::replacement_confirmation::{
    PlaylistConfirmationAction, QueueReplacementConfirmationDecision,
};
use crate::playlist_runtime::{PlaylistImportContinueOutcome, PlaylistRuntime};

/// Строит exact service provenance без ephemeral transport material.
fn root_provenance() -> (DurableReopenLocator, PlaylistImportProvenance) {
    let root_url = playlist_core::SecretUrlLocator::from_reopenable_url(
        "https://collection.example.test/root?token=exact",
    )
    .expect("test root URL");
    let root_locator = DurableReopenLocator::url(root_url);
    let provenance = PlaylistImportProvenance::new(
        root_locator.clone(),
        PlaylistImportSourceKind::Service,
        None,
    );
    (root_locator, provenance)
}

/// Создаёт part с metadata и explicit source availability.
fn part(label: &str, availability: PlaylistImportAvailability) -> PlaylistSingleImportDraft {
    let (root_locator, provenance) = root_provenance();
    let metadata = CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video)
        .with_title(Some(format!("Заголовок {label}")))
        .with_duration(Some(media_core::MediaDuration::from_duration(
            Duration::from_secs(77),
        )));
    PlaylistSingleImportDraft::new(
        root_locator,
        metadata,
        None,
        Vec::new(),
        provenance,
        availability,
    )
    .expect("focused URL part")
}

/// Создаёт first-class compound с одной unavailable part для end-to-end проверки.
fn compound_draft(sensitive_durable_locator_count: usize) -> PlaylistImportDraft {
    let (root_locator, provenance) = root_provenance();
    let group_metadata = CachedPlaylistMetadata::new("collection", PlaylistMediaKind::Video)
        .with_title(Some("Коллекция".to_owned()));
    let group = PlaylistCompoundImportDraft::new(
        root_locator,
        group_metadata,
        provenance,
        vec![
            part("one", PlaylistImportAvailability::Available),
            part("two", PlaylistImportAvailability::Unavailable),
        ],
    )
    .expect("focused compound");
    PlaylistImportDraft::new(
        vec![PlaylistImportEntryDraft::Compound(group)],
        Vec::new(),
        None,
        sensitive_durable_locator_count,
    )
}

/// Парсит test locator через тот же service-owned admission boundary.
fn locator(label: &str) -> service_ytdlp::YtDlpMediaLocator {
    service_ytdlp::parse_yt_dlp_media_locator(&format!(
        "https://{label}.example.test/watch?token=exact"
    ))
    .expect("test yt-dlp locator")
}

/// Ожидает condition только в bounded test budget.
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("condition должна выполниться до test deadline");
}

/// Immediate fake сохраняет sensitive count, который вычислил production classifier.
struct ImmediateCompoundResolver;

impl PlaylistUrlTopologyResolver for ImmediateCompoundResolver {
    fn resolve(
        &self,
        _locator: &service_ytdlp::YtDlpMediaLocator,
        _yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, ()> {
        if is_cancelled() {
            Err(())
        } else {
            Ok(compound_draft(sensitive_durable_locator_count))
        }
    }
}

/// Single fake закрепляет тот же preview path для обычного yt-dlp video.
struct ImmediateSingleResolver;

impl PlaylistUrlTopologyResolver for ImmediateSingleResolver {
    fn resolve(
        &self,
        _locator: &service_ytdlp::YtDlpMediaLocator,
        _yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, ()> {
        if is_cancelled() {
            return Err(());
        }
        Ok(PlaylistImportDraft::new(
            vec![PlaylistImportEntryDraft::Single(part(
                "video",
                PlaylistImportAvailability::Available,
            ))],
            Vec::new(),
            None,
            sensitive_durable_locator_count,
        ))
    }
}

/// Первый call ждёт generation cancellation, второй немедленно возвращает latest draft.
struct RapidSubmitResolver {
    calls: AtomicUsize,
    first_started: SyncSender<()>,
    first_cancelled: Arc<AtomicBool>,
}

impl PlaylistUrlTopologyResolver for RapidSubmitResolver {
    fn resolve(
        &self,
        _locator: &service_ytdlp::YtDlpMediaLocator,
        _yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, ()> {
        if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            self.first_started.send(()).expect("test receiver alive");
            while !is_cancelled() {
                thread::yield_now();
            }
            self.first_cancelled.store(true, Ordering::Release);
            return Err(());
        }
        Ok(compound_draft(sensitive_durable_locator_count))
    }
}

/// Управляемый fake позволяет применить metadata-only patch во время active extraction.
struct ReleasedCompoundResolver {
    started: SyncSender<()>,
    release: Arc<AtomicBool>,
}

impl PlaylistUrlTopologyResolver for ReleasedCompoundResolver {
    fn resolve(
        &self,
        _locator: &service_ytdlp::YtDlpMediaLocator,
        _yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, ()> {
        self.started.send(()).expect("test receiver alive");
        while !self.release.load(Ordering::Acquire) && !is_cancelled() {
            thread::yield_now();
        }
        if is_cancelled() {
            Err(())
        } else {
            Ok(compound_draft(sensitive_durable_locator_count))
        }
    }
}

#[test]
fn rapid_submit_cancels_running_and_delivers_only_exact_latest_generation() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let (started_sender, started_receiver) = mpsc::sync_channel(1);
    let first_cancelled = Arc::new(AtomicBool::new(false));
    let resolver = Arc::new(RapidSubmitResolver {
        calls: AtomicUsize::new(0),
        first_started: started_sender,
        first_cancelled: Arc::clone(&first_cancelled),
    });
    let mut owner = PlaylistUrlImportOwner::with_resolver(wake_port, resolver);
    let config = YtDlpConfig {
        enabled: true,
        ..YtDlpConfig::default()
    };

    owner
        .submit(locator("first"), config.clone(), 0)
        .expect("first request");
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("first resolver started");
    owner
        .submit(locator("latest"), config, 1)
        .expect("latest request replaces running");

    let mut completion = None;
    wait_until(|| {
        completion = owner.drain();
        completion.is_some()
    });
    assert!(first_cancelled.load(Ordering::Acquire));
    let PlaylistUrlImportCompletion::Resolved(draft) = completion.expect("latest completion")
    else {
        panic!("latest request должен завершиться успешно");
    };
    assert_eq!(draft.test_summary(), (1, 0, false, 1));
    assert!(owner.drain().is_none());
    assert!(matches!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        ProcessOwnerShutdownOutcome::Completed
    ));
}

#[test]
fn cancel_and_shutdown_are_bounded_and_never_publish_stale_completion() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let (started_sender, started_receiver) = mpsc::sync_channel(1);
    let first_cancelled = Arc::new(AtomicBool::new(false));
    let resolver = Arc::new(RapidSubmitResolver {
        calls: AtomicUsize::new(0),
        first_started: started_sender,
        first_cancelled: Arc::clone(&first_cancelled),
    });
    let mut owner = PlaylistUrlImportOwner::with_resolver(wake_port, resolver);
    owner
        .submit(locator("cancelled"), YtDlpConfig::default(), 0)
        .expect("request");
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("resolver started");

    owner.cancel_active();
    wait_until(|| first_cancelled.load(Ordering::Acquire));
    assert!(owner.drain().is_none());
    assert!(matches!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        ProcessOwnerShutdownOutcome::Completed
    ));
    assert!(owner.drain().is_none());
}

#[test]
fn poisoned_worker_state_fails_closed_and_shutdown_reports_terminal_failure() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut owner =
        PlaylistUrlImportOwner::with_resolver(wake_port, Arc::new(ImmediateCompoundResolver));
    let shared_state = Arc::clone(&owner.shared_state);
    let poisoner = thread::spawn(move || {
        let _guard = shared_state.lock().expect("test acquires worker state");
        panic!("intentional test poison");
    });
    assert!(poisoner.join().is_err());

    assert_eq!(
        owner.submit(locator("poisoned"), YtDlpConfig::default(), 0),
        Err(PlaylistUrlImportStartError::WorkerUnavailable)
    );
    assert!(matches!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        ProcessOwnerShutdownOutcome::ThreadPanicked {
            panicked_threads: 1,
            pending_threads: 0,
        }
    ));
}

#[test]
fn yt_dlp_video_uses_single_preview_then_append_without_confirmation_or_playback() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime = PlaylistRuntime::new(wake_port);
    runtime.resolve_missing_state_for_test();
    runtime
        .url_import
        .replace_resolver_for_test(Arc::new(ImmediateSingleResolver));

    assert_eq!(
        runtime
            .append_playlist_url(
                "https://video.example.test/watch",
                &YtDlpConfig {
                    enabled: true,
                    ..YtDlpConfig::default()
                },
            )
            .expect("single topology admission"),
        UrlAppendActionOutcome::ResolvingTopology
    );
    wait_until(|| {
        let _visible_change = runtime.drain_playlist_url_import_job();
        runtime.pending_playlist_import_preview().is_some()
    });
    let preview = runtime
        .pending_playlist_import_preview()
        .expect("single preview");
    assert_eq!(preview.accepted().singles(), 1);
    assert_eq!(preview.accepted().groups(), 0);
    assert_eq!(preview.sensitive_durable_locator_count(), 0);
    let preview_id = preview.preview_id();

    assert!(matches!(
        runtime.continue_playlist_import(preview_id),
        PlaylistImportContinueOutcome::Committed(_)
    ));
    assert_eq!(runtime.controller.queue().top_level_entry_count(), 1);
    assert_eq!(runtime.controller.queue().retained_item_count(), 1);
    assert!(runtime.pending_playlist_confirmation().is_none());
    assert!(runtime.controller.queue().traversal_current().is_none());
    assert!(runtime.controller.active_media().is_none());
}

#[test]
fn topology_append_uses_s08_preview_ack_groups_unavailable_and_metadata_without_playback() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime = PlaylistRuntime::new(wake_port);
    runtime.resolve_missing_state_for_test();
    runtime
        .url_import
        .replace_resolver_for_test(Arc::new(ImmediateCompoundResolver));
    let config = YtDlpConfig {
        enabled: true,
        ..YtDlpConfig::default()
    };

    assert_eq!(
        runtime
            .append_playlist_url("https://collection.example.test/root?token=exact", &config,)
            .expect("topology job admission"),
        UrlAppendActionOutcome::ResolvingTopology
    );
    assert_eq!(runtime.controller.queue().retained_item_count(), 0);
    assert!(runtime.controller.queue().traversal_current().is_none());
    assert!(runtime.controller.active_media().is_none());

    wait_until(|| {
        let _visible_change = runtime.drain_playlist_url_import_job();
        runtime.pending_playlist_import_preview().is_some()
    });
    let preview = runtime
        .pending_playlist_import_preview()
        .expect("S08 preview");
    assert_eq!(preview.intent(), PlaylistImportIntent::AppendToQueue);
    assert_eq!(preview.accepted().groups(), 1);
    assert_eq!(preview.accepted().retained_items(), 2);
    assert_eq!(preview.sensitive_durable_locator_count(), 1);
    assert_eq!(runtime.controller.queue().retained_item_count(), 0);

    assert_eq!(
        runtime.continue_playlist_import(preview.preview_id()),
        PlaylistImportContinueOutcome::AwaitingConfirmation
    );
    let confirmation = runtime
        .pending_playlist_confirmation()
        .expect("aggregated durable-locator acknowledgement");
    assert!(!confirmation.reasons().queue_replacement());
    assert!(confirmation.reasons().sensitive_url_persistence());
    assert!(matches!(
        runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
            intent_id: confirmation.intent_id(),
            decision: QueueReplacementConfirmationDecision::Confirm,
        }),
        PlaylistConfirmationApplyOutcome::Import(PlaylistImportContinueOutcome::Committed(_))
    ));

    assert_eq!(runtime.controller.queue().top_level_entry_count(), 1);
    assert_eq!(runtime.controller.queue().retained_item_count(), 2);
    assert!(runtime.controller.queue().traversal_current().is_none());
    assert!(runtime.controller.active_media().is_none());
    let items = runtime
        .controller
        .queue()
        .iter_playable_items()
        .collect::<Vec<_>>();
    assert_eq!(items[0].cached_metadata().title(), Some("Заголовок one"));
    assert_eq!(
        items[0].cached_metadata().duration(),
        Some(media_core::MediaDuration::from_duration(
            Duration::from_secs(77)
        ))
    );
    assert_eq!(
        items[1]
            .durable_payload()
            .expect("import payload")
            .availability(),
        PlaylistImportAvailability::Unavailable
    );
}

#[test]
fn cancelling_topology_preview_keeps_queue_current_and_playback_untouched() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime = PlaylistRuntime::new(wake_port);
    runtime.resolve_missing_state_for_test();
    runtime
        .url_import
        .replace_resolver_for_test(Arc::new(ImmediateCompoundResolver));
    let config = YtDlpConfig {
        enabled: true,
        ..YtDlpConfig::default()
    };
    runtime
        .append_playlist_url("https://collection.example.test/root", &config)
        .expect("topology job admission");
    wait_until(|| {
        let _visible_change = runtime.drain_playlist_url_import_job();
        runtime.pending_playlist_import_preview().is_some()
    });
    let preview_id = runtime
        .pending_playlist_import_preview()
        .expect("preview")
        .preview_id();

    assert!(runtime.cancel_playlist_import(preview_id));
    assert!(runtime.pending_playlist_import_preview().is_none());
    assert_eq!(runtime.controller.queue().retained_item_count(), 0);
    assert!(runtime.controller.queue().traversal_current().is_none());
    assert!(runtime.controller.active_media().is_none());
}

#[test]
fn metadata_patch_during_extraction_does_not_stale_or_mutate_append_semantics() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime = PlaylistRuntime::new(wake_port);
    runtime.resolve_missing_state_for_test();
    runtime
        .append_playlist_url(
            "https://media.example.test/existing.mp4",
            &YtDlpConfig::default(),
        )
        .expect("existing direct row");
    let existing_item = runtime
        .controller
        .queue()
        .iter_playable_items()
        .next()
        .expect("existing row");
    let existing_item_id = existing_item.item_id();
    let existing_locator = existing_item.locator().clone();
    let structural_revision_before = runtime.controller.view_snapshot().structural_revision();
    let (started_sender, started_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(AtomicBool::new(false));
    runtime
        .url_import
        .replace_resolver_for_test(Arc::new(ReleasedCompoundResolver {
            started: started_sender,
            release: Arc::clone(&release),
        }));

    runtime
        .append_playlist_url(
            "https://collection.example.test/root",
            &YtDlpConfig {
                enabled: true,
                ..YtDlpConfig::default()
            },
        )
        .expect("topology job admission");
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("resolver started");
    runtime
        .controller
        .apply_metadata_patches(vec![PlaylistMetadataPatch::new(
            existing_item_id,
            existing_locator,
            None,
            CachedPlaylistMetadata::new("existing", PlaylistMediaKind::Video)
                .with_title(Some("Обновлённое имя".to_owned())),
        )])
        .expect("metadata-only patch");
    assert_eq!(
        runtime.controller.view_snapshot().structural_revision(),
        structural_revision_before
    );
    release.store(true, Ordering::Release);
    wait_until(|| {
        let _visible_change = runtime.drain_playlist_url_import_job();
        runtime.pending_playlist_import_preview().is_some()
    });
    let preview_id = runtime
        .pending_playlist_import_preview()
        .expect("preview survives metadata-only revision")
        .preview_id();

    assert!(matches!(
        runtime.continue_playlist_import(preview_id),
        PlaylistImportContinueOutcome::Committed(_)
    ));
    assert_eq!(runtime.controller.queue().top_level_entry_count(), 2);
    assert_eq!(runtime.controller.queue().retained_item_count(), 3);
    assert_eq!(
        runtime
            .controller
            .queue()
            .item(existing_item_id)
            .expect("existing row remains")
            .cached_metadata()
            .title(),
        Some("Обновлённое имя")
    );
    assert!(runtime.controller.queue().traversal_current().is_none());
    assert!(runtime.controller.active_media().is_none());
}

#[test]
fn whole_group_capacity_preview_never_commits_a_partial_compound() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime = PlaylistRuntime::new(wake_port);
    runtime.resolve_missing_state_for_test();
    let existing = (0..playlist_core::MAX_PLAYLIST_ITEMS - 1)
        .map(|index| {
            playlist_core::PlaylistItemDraft::local(
                LocalLocator::Native(PathBuf::from(format!("/existing-{index}.mkv"))),
                None,
                CachedPlaylistMetadata::new("existing", PlaylistMediaKind::Video),
            )
        })
        .collect();
    runtime
        .controller
        .append(existing)
        .expect("near-capacity fixture");
    runtime
        .url_import
        .replace_resolver_for_test(Arc::new(ImmediateCompoundResolver));
    runtime
        .append_playlist_url(
            "https://collection.example.test/root",
            &YtDlpConfig {
                enabled: true,
                ..YtDlpConfig::default()
            },
        )
        .expect("topology job admission");
    wait_until(|| {
        let _visible_change = runtime.drain_playlist_url_import_job();
        runtime.pending_playlist_import_preview().is_some()
    });
    let preview = runtime
        .pending_playlist_import_preview()
        .expect("capacity preview");

    assert_eq!(preview.accepted().retained_items(), 0);
    let truncation = preview.capacity_truncation().expect("group rejected whole");
    assert_eq!(truncation.rejected_entries(), 1);
    assert_eq!(truncation.rejected_items(), 2);
    assert_eq!(
        runtime.controller.queue().retained_item_count(),
        playlist_core::MAX_PLAYLIST_ITEMS - 1
    );
}
