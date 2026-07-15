use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use media_core::MediaTagMetadata;
use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};
use playlist_discovery::{
    DiscoveryProbe, DiscoveryWakePort, LocalMediaFingerprint, LocalMediaKind,
    ProbeOneLocalMediaError, ProbedLocalMedia, WakeDisconnected,
};

use super::*;
use crate::playlist_runtime::controller::{ControllerAppendOutcome, PlaylistController};

struct Wake;

impl DiscoveryWakePort for Wake {
    fn wake(&self) -> Result<(), WakeDisconnected> {
        Ok(())
    }
}

struct ProbeGate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl ProbeGate {
    fn new() -> Self {
        Self {
            open: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut open = self.open.lock().expect("gate lock");
        while !*open {
            open = self.changed.wait(open).expect("gate wait");
        }
    }

    fn release(&self) {
        *self.open.lock().expect("gate lock") = true;
        self.changed.notify_all();
    }
}

struct FakeProbe {
    starts: mpsc::Sender<PathBuf>,
    count: Arc<AtomicUsize>,
    gate: Option<Arc<ProbeGate>>,
}

impl DiscoveryProbe for FakeProbe {
    fn read_fingerprint(
        &self,
        _locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<LocalMediaFingerprint, ProbeOneLocalMediaError> {
        Ok(LocalMediaFingerprint::new(20, SystemTime::UNIX_EPOCH))
    }

    fn probe(
        &self,
        locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
        let probe_index = self.count.fetch_add(1, Ordering::SeqCst);
        let _sent = self.starts.send(locator.to_path_buf());
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        let filename = locator
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("media")
            .to_owned();
        if filename.starts_with("unsupported") {
            return Err(ProbeOneLocalMediaError::UnsupportedContainer {
                reason: "safe fake unsupported".to_owned(),
            });
        }
        if filename.starts_with("noav") {
            return Err(ProbeOneLocalMediaError::NoAudioVideoTracks);
        }
        if filename.starts_with("fail") || (filename == "flaky-duplicate.mkv" && probe_index == 1) {
            return Err(ProbeOneLocalMediaError::ProbeFailure {
                reason: "safe fake failure".to_owned(),
            });
        }
        Ok(ProbedLocalMedia::new(
            filename,
            LocalMediaKind::VideoContaining,
            None,
            MediaTagMetadata::default(),
            LocalMediaFingerprint::new(20, SystemTime::UNIX_EPOCH),
        ))
    }
}

fn executor(
    gate: Option<Arc<ProbeGate>>,
) -> (DiscoveryExecutor, mpsc::Receiver<PathBuf>, Arc<AtomicUsize>) {
    let (starts, receiver) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let executor = DiscoveryExecutor::start_with_probe(
        Arc::new(FakeProbe {
            starts,
            count: count.clone(),
            gate,
        }),
        Arc::new(Wake),
    )
    .expect("fake executor");
    (executor, receiver, count)
}

fn draft(path: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(path)),
        Some(LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH)),
        CachedPlaylistMetadata::new(path, PlaylistMediaKind::Video),
    )
}

fn drain_until_terminal(
    jobs: &mut DiscoveryActionJobs,
    executor: &DiscoveryExecutor,
    controller: &mut PlaylistController,
    queue_generation: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while jobs.read_model().active_manual_jobs > 0 || jobs.visible_active.is_some() {
        jobs.drain(Some(executor), controller, queue_generation);
        assert!(Instant::now() < deadline, "fake discovery must complete");
        std::thread::yield_now();
    }
}

#[test]
fn manual_batch_is_natural_atomic_allows_duplicates_and_rebases_to_current_tail() {
    let gate = Arc::new(ProbeGate::new());
    let (executor, starts, _count) = executor(Some(gate.clone()));
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    jobs.start_manual_add(
        &executor,
        vec![
            "video10.mkv".into(),
            "video2.mkv".into(),
            "video2.mkv".into(),
        ],
        7,
    )
    .expect("manual job");
    starts
        .recv_timeout(Duration::from_secs(1))
        .expect("probe started");
    controller
        .append(vec![draft("existing.mkv")])
        .expect("ordinary edit");
    gate.release();
    drain_until_terminal(&mut jobs, &executor, &mut controller, 7);
    let completion = jobs
        .read_model()
        .latest_manual_completion
        .expect("completion");
    assert_eq!(completion.outcome, ManualAddTerminalOutcome::Appended);
    assert_eq!(completion.added, 3);
    assert_eq!(controller.queue().len(), 4);
    let names = controller
        .queue()
        .items()
        .iter()
        .map(|item| item.cached_metadata().fallback_display_name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["existing.mkv", "video2.mkv", "video2.mkv", "video10.mkv"]
    );
}

#[test]
fn cancel_and_queue_generation_supersede_never_commit_unfinished_manual_batch() {
    for (cancel_explicitly, completion_generation) in [(true, 3), (false, 4)] {
        let gate = Arc::new(ProbeGate::new());
        let (executor, starts, _count) = executor(Some(gate.clone()));
        let mut jobs = DiscoveryActionJobs::new();
        let mut controller = PlaylistController::default();
        let job_id = jobs
            .start_manual_add(&executor, vec!["blocked.mkv".into()], 3)
            .expect("manual job");
        starts
            .recv_timeout(Duration::from_secs(1))
            .expect("started");
        if cancel_explicitly {
            assert!(jobs.cancel_manual_add(job_id));
        }
        gate.release();
        drain_until_terminal(&mut jobs, &executor, &mut controller, completion_generation);
        assert_eq!(controller.queue().len(), 0);
        let outcome = jobs
            .read_model()
            .latest_manual_completion
            .expect("completion")
            .outcome;
        assert!(matches!(
            outcome,
            ManualAddTerminalOutcome::Cancelled
                | ManualAddTerminalOutcome::SupersededQueueGeneration
        ));
    }
}

#[test]
fn visible_refresh_coalesces_and_valid_cache_skips_second_probe() {
    let (executor, _starts, count) = executor(None);
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(vec![draft("visible.mkv")])
        .expect("append")
    else {
        panic!("fixture item expected");
    };
    let item = controller.queue().item(item_ids[0]).expect("item");
    let expected_structural_revision = controller.view_snapshot().structural_revision();
    let demand = VisibleRefreshDemand {
        item_id: item_ids[0],
        locator: item.locator().clone(),
        expected_fingerprint: item.local_fingerprint(),
        expected_structural_revision,
        path: PathBuf::from("visible.mkv"),
    };
    let first = jobs.request_visible_refresh(vec![demand.clone(), demand]);
    assert_eq!(first.accepted, 1);
    assert_eq!(first.coalesced, 1);
    jobs.drain(Some(&executor), &mut controller, 1);
    drain_until_terminal(&mut jobs, &executor, &mut controller, 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let item = controller.queue().item(item_ids[0]).expect("retained row");
    assert_eq!(
        item.local_fingerprint(),
        Some(LocalSourceFingerprint::new(20, SystemTime::UNIX_EPOCH))
    );
    let refreshed = VisibleRefreshDemand {
        item_id: item_ids[0],
        locator: item.locator().clone(),
        expected_fingerprint: item.local_fingerprint(),
        expected_structural_revision,
        path: PathBuf::from("visible.mkv"),
    };
    let second = jobs.request_visible_refresh(vec![refreshed]);
    assert_eq!(second.coalesced, 1);
    jobs.drain(Some(&executor), &mut controller, 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn duplicate_occurrences_keep_independent_probe_outcomes() {
    let (executor, _starts, _count) = executor(None);
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    jobs.start_manual_add(
        &executor,
        vec!["flaky-duplicate.mkv".into(), "flaky-duplicate.mkv".into()],
        1,
    )
    .expect("manual duplicate job");
    drain_until_terminal(&mut jobs, &executor, &mut controller, 1);

    let completion = jobs
        .read_model()
        .latest_manual_completion
        .expect("completion");
    assert_eq!(completion.added, 1);
    assert_eq!(completion.probe_failed, 1);
    assert_eq!(controller.queue().len(), 1);
}

#[test]
fn zero_capacity_is_typed_noop_without_new_dirty_revision() {
    let (executor, _starts, _count) = executor(None);
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    controller
        .append(vec![draft("full.mkv"); playlist_core::MAX_PLAYLIST_ITEMS])
        .expect("fill queue to hard cap");
    let dirty_before = controller.dirty_revision();
    jobs.start_manual_add(&executor, vec!["rejected.mkv".into()], 1)
        .expect("manual job");
    drain_until_terminal(&mut jobs, &executor, &mut controller, 1);

    let completion = jobs
        .read_model()
        .latest_manual_completion
        .expect("completion");
    assert_eq!(completion.outcome, ManualAddTerminalOutcome::NoCapacity);
    assert_eq!(completion.added, 0);
    assert_eq!(completion.capacity_rejected, 1);
    assert_eq!(controller.dirty_revision(), dirty_before);
}

#[test]
fn manual_partial_summary_keeps_exact_failure_categories() {
    let (executor, _starts, _count) = executor(None);
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    jobs.start_manual_add(
        &executor,
        vec![
            "unsupported.mkv".into(),
            "noav.mkv".into(),
            "fail.mkv".into(),
            "ok.mkv".into(),
        ],
        1,
    )
    .expect("manual partial job");
    drain_until_terminal(&mut jobs, &executor, &mut controller, 1);

    let completion = jobs
        .read_model()
        .latest_manual_completion
        .expect("completion");
    assert_eq!(completion.added, 1);
    assert_eq!(completion.unsupported_container, 1);
    assert_eq!(completion.no_audio_video_tracks, 1);
    assert_eq!(completion.probe_failed, 1);
}

#[test]
fn matching_persisted_fingerprint_skips_first_visible_demux_probe() {
    let (executor, _starts, count) = executor(None);
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(vec![PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from("cached-visible.mkv")),
            Some(LocalSourceFingerprint::new(20, SystemTime::UNIX_EPOCH)),
            CachedPlaylistMetadata::new("cached-visible.mkv", PlaylistMediaKind::Video),
        )])
        .expect("append")
    else {
        panic!("fixture item expected");
    };
    let item = controller.queue().item(item_ids[0]).expect("item");
    let demand = VisibleRefreshDemand {
        item_id: item_ids[0],
        locator: item.locator().clone(),
        expected_fingerprint: item.local_fingerprint(),
        expected_structural_revision: controller.view_snapshot().structural_revision(),
        path: PathBuf::from("cached-visible.mkv"),
    };
    assert_eq!(jobs.request_visible_refresh(vec![demand]).accepted, 1);
    jobs.drain(Some(&executor), &mut controller, 1);
    drain_until_terminal(&mut jobs, &executor, &mut controller, 1);

    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(jobs.read_model().visible_commit_rejected, 0);
}

#[test]
fn structural_edit_rejects_late_visible_result_without_validating_stale_key() {
    let gate = Arc::new(ProbeGate::new());
    let (executor, starts, count) = executor(Some(gate.clone()));
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(vec![draft("stale-visible.mkv")])
        .expect("append")
    else {
        panic!("fixture item expected");
    };
    let item = controller.queue().item(item_ids[0]).expect("item");
    let demand = VisibleRefreshDemand {
        item_id: item_ids[0],
        locator: item.locator().clone(),
        expected_fingerprint: item.local_fingerprint(),
        expected_structural_revision: controller.view_snapshot().structural_revision(),
        path: PathBuf::from("stale-visible.mkv"),
    };
    assert_eq!(jobs.request_visible_refresh(vec![demand]).accepted, 1);
    jobs.drain(Some(&executor), &mut controller, 1);
    starts
        .recv_timeout(Duration::from_secs(1))
        .expect("probe started");
    controller
        .append(vec![draft("ordinary-edit.mkv")])
        .expect("structural edit");
    gate.release();
    drain_until_terminal(&mut jobs, &executor, &mut controller, 1);

    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(jobs.read_model().visible_stale, 1);
    assert_eq!(
        controller
            .queue()
            .item(item_ids[0])
            .expect("retained item")
            .local_fingerprint(),
        Some(LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH))
    );
}

#[test]
fn visible_failure_retains_row_and_allows_later_retry_without_dirty_mutation() {
    let (executor, _starts, _count) = executor(None);
    let mut jobs = DiscoveryActionJobs::new();
    let mut controller = PlaylistController::default();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(vec![draft("fail-visible.mkv")])
        .expect("append")
    else {
        panic!("fixture item expected");
    };
    let dirty_before = controller.dirty_revision();
    let item = controller.queue().item(item_ids[0]).expect("item");
    let demand = VisibleRefreshDemand {
        item_id: item_ids[0],
        locator: item.locator().clone(),
        expected_fingerprint: item.local_fingerprint(),
        expected_structural_revision: controller.view_snapshot().structural_revision(),
        path: PathBuf::from("fail-visible.mkv"),
    };
    assert_eq!(
        jobs.request_visible_refresh(vec![demand.clone()]).accepted,
        1
    );
    jobs.drain(Some(&executor), &mut controller, 1);
    drain_until_terminal(&mut jobs, &executor, &mut controller, 1);

    assert_eq!(controller.queue().len(), 1);
    assert_eq!(controller.dirty_revision(), dirty_before);
    assert!(
        controller.view_snapshot().visible_rows(0..1)[0]
            .runtime_error()
            .is_some()
    );
    assert_eq!(jobs.request_visible_refresh(vec![demand]).accepted, 1);
}
