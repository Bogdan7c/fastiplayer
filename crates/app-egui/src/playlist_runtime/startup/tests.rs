use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
    RepeatMode,
};
use playlist_state::{
    InspectionOutcome, PlaylistStateSnapshot, PlaylistStateStore, ProtectedStateCause,
    QuarantineFailureCause, QuarantineFileName, QuarantineOutcome, SaveBlockReason,
};

use super::{
    PlaylistLineagePersistence, PlaylistStartupOwner, PlaylistStartupPhase,
    PlaylistStartupShutdownOutcome, PlaylistStartupStateStore, PlaylistStartupWarning,
};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::{
    PlaylistLoadGateState, PlaylistMediaOpenGateError, PlaylistRuntime, PlaylistStartupDrainOutcome,
};
use crate::process_shutdown::ShutdownDeadline;

fn draft(label: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(label)),
        None,
        CachedPlaylistMetadata::new(label.to_owned(), PlaylistMediaKind::Video),
    )
}

fn pending_runtime() -> PlaylistRuntime {
    PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime))
}

fn state_store_with_bytes(bytes: &[u8]) -> (tempfile::TempDir, Arc<PlaylistStateStore>) {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let state_path = directory
        .path()
        .join(playlist_state::PLAYLIST_STATE_FILENAME);
    std::fs::write(&state_path, bytes).expect("write state fixture");
    (directory, Arc::new(PlaylistStateStore::new(state_path)))
}

fn state_store_for_queue(
    queue: &PlaylistQueue,
    repeat_mode: RepeatMode,
) -> (tempfile::TempDir, Arc<PlaylistStateStore>) {
    let bytes = playlist_state::serialize_state(PlaylistStateSnapshot::new(queue, repeat_mode))
        .expect("serialize state fixture");
    state_store_with_bytes(&bytes)
}

fn drain_until_ready(runtime: &mut PlaylistRuntime, quarantine_file_name: QuarantineFileName) {
    for _ in 0..20_000 {
        match runtime
            .drain_playlist_state_startup(quarantine_file_name.clone())
            .expect("startup decision")
        {
            PlaylistStartupDrainOutcome::Ready => return,
            PlaylistStartupDrainOutcome::NoCompletion
            | PlaylistStartupDrainOutcome::ApplyingQuarantine
            | PlaylistStartupDrainOutcome::StaleCompletionIgnored => thread::yield_now(),
        }
    }
    panic!("startup decision did not complete within bounded test spins");
}

#[test]
fn missing_state_opens_persistent_allocator_gate() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let store = Arc::new(PlaylistStateStore::new(
        directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME),
    ));
    let mut runtime = pending_runtime();

    runtime
        .begin_production_playlist_state_inspection(store)
        .expect("start inspection");
    assert!(runtime.playlist_controller().is_none());
    assert_eq!(
        runtime.playlist_startup_view().phase,
        PlaylistStartupPhase::Inspecting
    );

    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );

    assert!(runtime.playlist_controller().is_some());
    assert_eq!(
        runtime.load_gate(),
        PlaylistLoadGateState::Open(PlaylistLineagePersistence::Persistent)
    );
}

#[test]
fn restored_current_none_stays_idle_without_implicit_selection() {
    let mut queue = PlaylistQueue::new();
    queue
        .append_batch(vec![draft("idle-one.mkv"), draft("idle-two.mkv")])
        .expect("append fixture");
    assert!(queue.traversal_current().is_none());
    let (_directory, store) = state_store_for_queue(&queue, RepeatMode::StopAtEnd);
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );

    assert!(runtime.startup_restored_current().is_none());
    assert_eq!(runtime.playlist_view_snapshot().len(), 2);
    assert!(
        runtime
            .playlist_controller()
            .expect("controller")
            .queue()
            .traversal_current()
            .is_none()
    );
}

#[test]
fn restored_current_and_skip_fallback_keep_start_paused_and_rows() {
    let mut queue = PlaylistQueue::new();
    let item_ids = match queue
        .append_batch(vec![
            draft("missing-one.mkv"),
            draft("fallback-two.mkv"),
            draft("fallback-three.mkv"),
        ])
        .expect("append fixture")
    {
        playlist_core::AddItemsOutcome::Added(item_ids) => item_ids.into_vec(),
        playlist_core::AddItemsOutcome::NoItemsProvided => panic!("fixture rows required"),
    };
    queue
        .set_traversal_current(item_ids[0])
        .expect("set restored current");
    let (_directory, store) = state_store_for_queue(&queue, RepeatMode::RepeatQueue);
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );
    runtime
        .controller
        .as_mut()
        .expect("controller")
        .set_error_behavior(crate::playlist_runtime::controller::PlaylistErrorBehavior::Skip);

    let first = runtime
        .startup_restored_current()
        .expect("restored current");
    assert_eq!(first.item_id(), item_ids[0]);
    assert_eq!(
        first.playback_intent(),
        player_core::PlaybackIntent::StartPaused
    );
    let fallback = runtime
        .report_startup_restore_failure(first, Arc::<str>::from("missing"))
        .expect("skip fallback");
    assert_eq!(fallback.item_id(), item_ids[1]);
    assert_eq!(
        fallback.playback_intent(),
        player_core::PlaybackIntent::StartPaused
    );
    assert_eq!(runtime.playlist_view_snapshot().len(), 3);
    assert!(
        runtime
            .playlist_controller()
            .expect("controller")
            .latest_dirty_signal()
            .is_none()
    );
}

#[test]
fn restored_pre_barrier_terminal_failure_continues_same_paused_chain() {
    let mut queue = PlaylistQueue::new();
    let item_ids = match queue
        .append_batch(vec![draft("first.mkv"), draft("second.mkv")])
        .expect("append fixture")
    {
        playlist_core::AddItemsOutcome::Added(item_ids) => item_ids.into_vec(),
        playlist_core::AddItemsOutcome::NoItemsProvided => panic!("fixture rows required"),
    };
    queue
        .set_traversal_current(item_ids[0])
        .expect("set restored current");
    let (_directory, store) = state_store_for_queue(&queue, RepeatMode::RepeatQueue);
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );
    runtime
        .controller
        .as_mut()
        .expect("controller")
        .set_error_behavior(crate::playlist_runtime::controller::PlaylistErrorBehavior::Skip);

    let request_id = crate::media_open::MediaOpenRequestId::from_non_zero(
        NonZeroU64::new(401).expect("request id"),
    );
    let player_request_id = player_core::MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(501).expect("player request id"),
    );
    let target = runtime.startup_restored_current().expect("restored target");
    runtime
        .accept_startup_restore_install(request_id, player_request_id, target)
        .expect("pre-barrier controller admission");

    let fallback = runtime
        .report_startup_restore_install_failure(
            request_id,
            Arc::<str>::from("player rejected before enqueue"),
        )
        .expect("typed pre-barrier failure keeps fallback");
    assert_eq!(fallback.item_id(), item_ids[1]);
    assert_eq!(
        fallback.playback_intent(),
        player_core::PlaybackIntent::StartPaused
    );
    assert_eq!(runtime.playlist_view_snapshot().len(), 2);
}

#[test]
fn player_authorization_is_typed_closed_until_allocator_decision() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let store = Arc::new(PlaylistStateStore::new(
        directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME),
    ));
    let mut runtime = pending_runtime();
    let request_id = crate::media_open::MediaOpenRequestId::from_non_zero(
        NonZeroU64::new(91).expect("request id is non-zero"),
    );

    assert_eq!(
        runtime.authorize_ready_media_open(request_id),
        Err(PlaylistMediaOpenGateError::LoadDecisionPending)
    );
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );
    assert_eq!(
        runtime.authorize_ready_media_open(request_id),
        Err(PlaylistMediaOpenGateError::Coordinator(
            crate::media_open::MediaOpenCommandError::NoCurrentRequest,
        ))
    );
}

#[test]
fn superseded_valid_restore_keeps_allocator_watermark_without_restored_items() {
    let mut queue = PlaylistQueue::new();
    let added = queue
        .append_batch(vec![draft("one.mkv"), draft("two.mkv"), draft("three.mkv")])
        .expect("append fixture");
    let highest_id = match added {
        playlist_core::AddItemsOutcome::Added(ids) => {
            *ids.into_vec().last().expect("highest fixture id")
        }
        playlist_core::AddItemsOutcome::NoItemsProvided => panic!("fixture rows required"),
    };
    assert!(matches!(
        queue.clear(),
        playlist_core::ClearQueueOutcome::Cleared { .. }
    ));
    let (_directory, store) = state_store_for_queue(&queue, RepeatMode::StopAtEnd);
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    runtime
        .record_startup_prepared_add(vec![draft("winning-user-add.mkv")])
        .expect("bounded ID-less draft");

    assert!(runtime.playlist_controller().is_none());
    assert_eq!(runtime.playlist_view_snapshot().len(), 0);
    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );

    let controller = runtime.playlist_controller().expect("opened controller");
    assert_eq!(controller.queue().len(), 1);
    let allocated_id = controller.queue().items()[0].item_id();
    assert!(
        allocated_id > highest_id,
        "persisted watermark must not be reused"
    );
    assert!(controller.queue().traversal_current().is_none());
}

#[test]
fn mode_only_overlay_preserves_restore_and_commits_one_dirty_revision() {
    let mut queue = PlaylistQueue::new();
    let item_id = match queue
        .append_one(draft("restored-current.mkv"))
        .expect("append fixture")
    {
        playlist_core::AddItemsOutcome::Added(ids) => ids.into_vec()[0],
        playlist_core::AddItemsOutcome::NoItemsProvided => panic!("fixture row required"),
    };
    queue
        .set_traversal_current(item_id)
        .expect("set restored current");
    let (_directory, store) = state_store_for_queue(&queue, RepeatMode::StopAtEnd);
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    let restore_generation = runtime.playlist_startup_view().restore_generation;
    runtime
        .record_startup_repeat_mode(RepeatMode::RepeatOne)
        .expect("repeat overlay");
    runtime
        .record_startup_shuffle_enabled(true)
        .expect("shuffle overlay");
    assert_eq!(
        runtime.playlist_startup_view().restore_generation,
        restore_generation
    );

    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );

    let controller = runtime.playlist_controller().expect("opened controller");
    assert_eq!(controller.queue().len(), 1);
    assert_eq!(
        controller
            .queue()
            .traversal_current()
            .map(|current| current.item_id()),
        Some(item_id)
    );
    assert!(controller.queue().shuffle_enabled());
    assert_eq!(controller.repeat_mode, RepeatMode::RepeatOne);
    assert_eq!(controller.dirty_revision().get(), 1);
}

#[test]
fn clear_and_media_replacement_supersede_only_restore_apply() {
    for media_replacement in [false, true] {
        let mut queue = PlaylistQueue::new();
        queue
            .append_one(draft("restored-but-superseded.mkv"))
            .expect("append fixture");
        let (_directory, store) = state_store_for_queue(&queue, RepeatMode::RepeatQueue);
        let mut runtime = pending_runtime();
        runtime
            .begin_playlist_state_inspection(store)
            .expect("start inspection");
        if media_replacement {
            runtime
                .record_startup_media_replacement()
                .expect("media replacement draft");
        } else {
            runtime.record_startup_clear().expect("clear draft");
        }

        drain_until_ready(
            &mut runtime,
            QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
        );

        let controller = runtime.playlist_controller().expect("opened controller");
        assert!(controller.queue().is_empty());
        assert_eq!(controller.repeat_mode, RepeatMode::RepeatQueue);
        assert_eq!(
            controller.dirty_revision().get(),
            u64::from(!media_replacement),
            "Clear persists immediately; media replacement waits for Installed commit"
        );
    }
}

#[test]
fn protected_versions_open_only_non_persistent_generation() {
    let cases = [
        (
            br#"{"schema_version":2}"#.as_slice(),
            SaveBlockReason::NewerSchema,
        ),
        (
            br#"{"schema_version":1,"schema_version":1}"#.as_slice(),
            SaveBlockReason::DuplicateVersion,
        ),
    ];

    for (bytes, expected_block) in cases {
        let (_directory, store) = state_store_with_bytes(bytes);
        let mut runtime = pending_runtime();
        runtime
            .begin_playlist_state_inspection(store)
            .expect("start inspection");
        drain_until_ready(
            &mut runtime,
            QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
        );

        assert!(matches!(
            runtime.load_gate(),
            PlaylistLoadGateState::Open(PlaylistLineagePersistence::NonPersistent {
                save_block,
                ..
            }) if save_block == expected_block
        ));
        assert!(runtime.playlist_startup_view().warning.is_some());
    }
}

#[test]
fn corrupt_state_is_quarantined_only_after_inspection_decision() {
    let bytes = br#"{"schema_version":1,"queue":{}}"#;
    let (directory, store) = state_store_with_bytes(bytes);
    let state_path = store.state_path().to_owned();
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    assert!(
        state_path.exists(),
        "read-only inspection has not renamed source"
    );

    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );

    assert!(!state_path.exists());
    assert_eq!(
        runtime.load_gate(),
        PlaylistLoadGateState::Open(PlaylistLineagePersistence::Persistent)
    );
    assert!(matches!(
        runtime.playlist_startup_view().warning,
        Some(PlaylistStartupWarning::CorruptStateQuarantined { .. })
    ));
    let quarantined_count = std::fs::read_dir(directory.path())
        .expect("read quarantine directory")
        .count();
    assert_eq!(quarantined_count, 1);
}

enum ScriptedQuarantineOutcome {
    SourceChanged,
    Failed,
}

struct ScriptedQuarantineStore {
    inner: PlaylistStateStore,
    outcome: Mutex<Option<ScriptedQuarantineOutcome>>,
}

impl PlaylistStartupStateStore for ScriptedQuarantineStore {
    fn inspect_state(&self) -> InspectionOutcome {
        self.inner.inspect_state()
    }

    fn apply_quarantine(
        &self,
        _inspected_identity: &playlist_state::InspectedFileIdentity,
        _quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        match self
            .outcome
            .lock()
            .expect("scripted outcome mutex")
            .take()
            .expect("one quarantine call")
        {
            ScriptedQuarantineOutcome::SourceChanged => QuarantineOutcome::SourceChanged,
            ScriptedQuarantineOutcome::Failed => QuarantineOutcome::FailedSaveBlocked {
                cause: QuarantineFailureCause::MoveFailed(std::io::ErrorKind::PermissionDenied),
            },
        }
    }
}

#[test]
fn quarantine_source_change_and_failure_open_only_save_blocked_generation() {
    let cases = [
        (
            ScriptedQuarantineOutcome::SourceChanged,
            SaveBlockReason::QuarantineSourceChanged,
        ),
        (
            ScriptedQuarantineOutcome::Failed,
            SaveBlockReason::QuarantineFailed,
        ),
    ];
    for (scripted_outcome, expected_block) in cases {
        let (directory, _) = state_store_with_bytes(br#"{"schema_version":1,"queue":{}}"#);
        let state_path = directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME);
        let store = Arc::new(ScriptedQuarantineStore {
            inner: PlaylistStateStore::new(&state_path),
            outcome: Mutex::new(Some(scripted_outcome)),
        });
        let mut runtime = pending_runtime();
        runtime
            .begin_playlist_state_inspection(store)
            .expect("start inspection");
        drain_until_ready(
            &mut runtime,
            QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
        );

        assert!(
            state_path.exists(),
            "blocked quarantine must preserve source"
        );
        assert!(matches!(
            runtime.load_gate(),
            PlaylistLoadGateState::Open(PlaylistLineagePersistence::NonPersistent {
                save_block,
                ..
            }) if save_block == expected_block
        ));
    }
}

#[test]
fn shutdown_before_decision_never_starts_quarantine() {
    let bytes = br#"{"schema_version":1,"queue":{}}"#;
    let (_directory, store) = state_store_with_bytes(bytes);
    let state_path = store.state_path().to_owned();
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");

    let _shutdown = runtime.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1)));
    thread::yield_now();

    assert!(state_path.exists());
    assert_eq!(
        runtime.playlist_startup_view().phase,
        PlaylistStartupPhase::Shutdown
    );
    assert!(runtime.playlist_controller().is_none());
    assert!(
        runtime
            .record_startup_repeat_mode(RepeatMode::RepeatQueue)
            .is_err()
    );
    assert!(runtime.record_startup_shuffle_enabled(true).is_err());
    let transport_model =
        runtime.playlist_transport_ui_model(Duration::ZERO, std::time::Instant::now());
    assert!(!transport_model.queue_modes_enabled);
    assert_eq!(transport_model.repeat_mode, RepeatMode::StopAtEnd);
    assert!(!transport_model.shuffle_enabled);
}

struct BlockingInspectionStore {
    entered_sender: Mutex<Option<SyncSender<()>>>,
    release: (Mutex<bool>, Condvar),
}

impl PlaylistStartupStateStore for BlockingInspectionStore {
    fn inspect_state(&self) -> InspectionOutcome {
        if let Some(sender) = self.entered_sender.lock().expect("entered mutex").take() {
            sender.send(()).expect("signal inspection entry");
        }
        let (released, release_changed) = &self.release;
        let mut released = released.lock().expect("release mutex");
        while !*released {
            released = release_changed.wait(released).expect("release wait");
        }
        InspectionOutcome::Missing
    }

    fn apply_quarantine(
        &self,
        _inspected_identity: &playlist_state::InspectedFileIdentity,
        _quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        panic!("missing-state fake must never quarantine")
    }
}

#[test]
fn blocking_large_inspection_keeps_draft_responsive_and_idless() {
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let store = Arc::new(BlockingInspectionStore {
        entered_sender: Mutex::new(Some(entered_sender)),
        release: (Mutex::new(false), Condvar::new()),
    });
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store.clone())
        .expect("start inspection");
    entered_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("inspection runs on background thread");

    runtime
        .record_startup_prepared_add(vec![draft("prepared-before-load.mkv")])
        .expect("event loop can update bounded draft while inspection blocks");
    assert!(runtime.playlist_controller().is_none());
    assert_eq!(runtime.playlist_view_snapshot().len(), 0);

    let (released, release_changed) = &store.release;
    *released.lock().expect("release mutex") = true;
    release_changed.notify_all();
    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );
    assert_eq!(
        runtime
            .playlist_controller()
            .expect("opened controller")
            .queue()
            .len(),
        1
    );
}

#[test]
fn startup_timeout_retains_join_handle_until_later_terminal_join() {
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let store = Arc::new(BlockingInspectionStore {
        entered_sender: Mutex::new(Some(entered_sender)),
        release: (Mutex::new(false), Condvar::new()),
    });
    let mut owner =
        PlaylistStartupOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    owner
        .begin_inspection(store.clone())
        .expect("start blocking inspection");
    entered_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("inspection entered background store");

    assert_eq!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::ZERO)),
        PlaylistStartupShutdownOutcome::TimedOut
    );
    assert!(owner.job.is_some());
    assert!(
        owner
            .job
            .as_ref()
            .and_then(|job| job.join_handle.as_ref())
            .is_some()
    );

    let (released, release_changed) = &store.release;
    *released.lock().expect("release mutex") = true;
    release_changed.notify_all();
    assert_eq!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        PlaylistStartupShutdownOutcome::Completed
    );
    assert!(owner.job.is_none());
    assert_eq!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        PlaylistStartupShutdownOutcome::AlreadyCompleted
    );
}

struct PanickingInspectionStore;

impl PlaylistStartupStateStore for PanickingInspectionStore {
    fn inspect_state(&self) -> InspectionOutcome {
        panic!("scripted startup inspection panic")
    }

    fn apply_quarantine(
        &self,
        _inspected_identity: &playlist_state::InspectedFileIdentity,
        _quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        panic!("panic fake must never quarantine")
    }
}

#[test]
fn startup_thread_panic_is_terminal_and_typed() {
    let mut owner =
        PlaylistStartupOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    owner
        .begin_inspection(Arc::new(PanickingInspectionStore))
        .expect("start panicking inspection");

    assert_eq!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        PlaylistStartupShutdownOutcome::ThreadPanicked
    );
    assert!(owner.job.is_none());
}

struct BlockingQuarantineStore {
    inner: Arc<PlaylistStateStore>,
    entered_sender: Mutex<Option<SyncSender<()>>>,
    release: (Mutex<bool>, Condvar),
}

impl PlaylistStartupStateStore for BlockingQuarantineStore {
    fn inspect_state(&self) -> InspectionOutcome {
        self.inner.inspect_state()
    }

    fn apply_quarantine(
        &self,
        inspected_identity: &playlist_state::InspectedFileIdentity,
        quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        if let Some(sender) = self.entered_sender.lock().expect("entered mutex").take() {
            sender.send(()).expect("signal quarantine entry");
        }
        let (released, release_changed) = &self.release;
        let mut released = released.lock().expect("release mutex");
        while !*released {
            released = release_changed.wait(released).expect("release wait");
        }
        self.inner
            .apply_quarantine(inspected_identity, quarantine_file_name)
    }
}

#[test]
fn quarantine_timeout_retains_same_job_until_join_without_applying_late_policy() {
    let (_directory, inner) = state_store_with_bytes(br#"{"schema_version":1,"queue":{}}"#);
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let store = Arc::new(BlockingQuarantineStore {
        inner,
        entered_sender: Mutex::new(Some(entered_sender)),
        release: (Mutex::new(false), Condvar::new()),
    });
    let mut owner =
        PlaylistStartupOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    owner
        .begin_inspection(store.clone())
        .expect("start corrupt inspection");
    let inspection_deadline = Instant::now() + Duration::from_secs(1);
    let (inspected_identity, corrupt_cause) = loop {
        match owner.drain_completion() {
            Some(super::StartupJobCompletion::Inspection {
                outcome:
                    InspectionOutcome::CorruptNeedsQuarantine {
                        inspected_identity,
                        cause,
                    },
                ..
            }) => break (inspected_identity, cause),
            Some(_) => panic!("corrupt fixture produced an unexpected inspection outcome"),
            None if Instant::now() < inspection_deadline => thread::yield_now(),
            None => panic!("corrupt inspection did not complete before test deadline"),
        }
    };
    owner
        .start_quarantine(
            corrupt_cause,
            inspected_identity,
            QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
        )
        .expect("start blocking quarantine");
    entered_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("quarantine entered background store");

    assert_eq!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::ZERO)),
        PlaylistStartupShutdownOutcome::TimedOut
    );
    assert!(owner.job.is_some());
    assert_eq!(owner.view().phase, PlaylistStartupPhase::Shutdown);

    let (released, release_changed) = &store.release;
    *released.lock().expect("release mutex") = true;
    release_changed.notify_all();
    assert_eq!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
        PlaylistStartupShutdownOutcome::Completed
    );
    assert_eq!(owner.view().persistence, None);
}

#[test]
fn unrecognized_version_keeps_typed_cause_in_read_only_model() {
    let (_directory, store) = state_store_with_bytes(br#"{"schema_version":"one"}"#);
    let mut runtime = pending_runtime();
    runtime
        .begin_playlist_state_inspection(store)
        .expect("start inspection");
    drain_until_ready(
        &mut runtime,
        QuarantineFileName::from_timestamp(SystemTime::UNIX_EPOCH),
    );

    assert!(matches!(
        runtime.playlist_startup_view().warning,
        Some(PlaylistStartupWarning::UnrecognizedVersion {
            cause: ProtectedStateCause::NonIntegerSchemaVersion,
        })
    ));
}
