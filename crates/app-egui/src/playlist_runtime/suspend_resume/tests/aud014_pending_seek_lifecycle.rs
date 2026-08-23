//! AUD-014: production worker seek receipt обязан быть settled до suspend checkpoint-а.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use media_core::{DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, DemuxSeekResult, Demuxer};
use player_core::{
    ExactTimelineSeekOutcome, ExactTimelineSeekRequest, FrameCounters, MediaInstallCompletion,
    PlayerWorker, PlayerWorkerConfig, PreparedDemuxSeekEnqueueError, PreparedDemuxSeekOutcome,
    PreparedDemuxSeekPort, PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId, PreparedMedia,
    TimelineSeekKind, TimelineSeekRequestId,
};

use super::*;
use crate::playlist_runtime::LifecycleTimelineCheckpointPosition;
use crate::state::{LifecycleTimelineSeekSettlement, settle_timeline_seek_receipts_until};

/// Все ожидания теста ограничены, чтобы regression не мог зависнуть в CI.
const TEST_DEADLINE_BUDGET: Duration = Duration::from_secs(2);

/// Demuxer сохраняет seekable timeline, но не создаёт EOF или decoder side effects.
struct PendingSeekDemuxer;

impl Demuxer for PendingSeekDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(120))
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::TemporarilyUnavailable(
            DemuxRetryHint::new(DemuxRetryHint::MAX_RETRY_AFTER)
                .expect("maximum retry hint обязан быть валиден"),
        ))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Test-owned port позволяет отделить command admission от terminal completion.
struct ControlledPreparedSeekPort {
    commands: Mutex<VecDeque<(PreparedDemuxSeekRequestId, DemuxSeekRequest)>>,
    receipts: Mutex<VecDeque<PreparedDemuxSeekReceipt>>,
    auto_complete_next_command: AtomicBool,
}

impl Default for ControlledPreparedSeekPort {
    fn default() -> Self {
        Self {
            commands: Mutex::new(VecDeque::new()),
            receipts: Mutex::new(VecDeque::new()),
            auto_complete_next_command: AtomicBool::new(true),
        }
    }
}

impl ControlledPreparedSeekPort {
    /// Ждёт, пока настоящий player worker передаст seek в prepared-media port.
    fn wait_for_command(&self) -> (PreparedDemuxSeekRequestId, DemuxSeekRequest) {
        let deadline = Instant::now() + TEST_DEADLINE_BUDGET;
        loop {
            if let Some(command) = self
                .commands
                .lock()
                .expect("controlled seek command lock")
                .pop_front()
            {
                return command;
            }
            assert!(
                Instant::now() < deadline,
                "player worker не передал seek в prepared-media port до test deadline"
            );
            std::thread::yield_now();
        }
    }
}

impl PreparedDemuxSeekPort for ControlledPreparedSeekPort {
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        let requested_position = request.timestamp;
        self.commands
            .lock()
            .expect("controlled seek command lock")
            .push_back((request_id, request));
        if self
            .auto_complete_next_command
            .swap(false, Ordering::AcqRel)
        {
            self.receipts
                .lock()
                .expect("controlled seek receipt lock")
                .push_back(PreparedDemuxSeekReceipt {
                    request_id,
                    outcome: PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
                        requested_position: MediaTime::from_duration(requested_position),
                        actual_position: MediaTime::from_duration(requested_position),
                        actual_track_timestamp: None,
                    }),
                });
        }
        Ok(())
    }

    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        self.receipts
            .lock()
            .expect("controlled seek receipt lock")
            .pop_front()
    }
}

/// Запускает настоящий worker и устанавливает seekable prepared media.
fn installed_worker() -> (
    PlayerWorker,
    Arc<ControlledPreparedSeekPort>,
    MediaInstanceId,
) {
    let mut worker = PlayerWorker::spawn(PlayerWorkerConfig::default())
        .expect("player worker должен стартовать");
    let seek_port = Arc::new(ControlledPreparedSeekPort::default());
    let erased_port: Arc<dyn PreparedDemuxSeekPort> = seek_port.clone();
    let prepared_media =
        PreparedMedia::from_external_label("aud014-fixture", Box::new(PendingSeekDemuxer))
            .with_worker_receipted_demux_seek(erased_port);
    let install_receipt = worker
        .load_prepared_media(prepared_media, false)
        .expect("prepared media command должна быть принята");
    let installed_snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.source_label.as_deref() == Some("aud014-fixture")
            && snapshot.media_instance_id.is_some()
    });
    let completion = wait_for_install_completion(&install_receipt);
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = completion
    else {
        panic!("prepared media должна завершить compatibility install");
    };
    assert_eq!(
        installed_snapshot.media_instance_id,
        Some(media_instance_id)
    );
    (worker, seek_port, media_instance_id)
}

/// Устанавливает тот же seekable media без async prepared port-а для happy-path barrier-а.
fn installed_legacy_seek_worker() -> (PlayerWorker, MediaInstanceId) {
    let mut worker = PlayerWorker::spawn(PlayerWorkerConfig::default())
        .expect("player worker должен стартовать");
    let prepared_media =
        PreparedMedia::from_external_label("aud014-legacy-fixture", Box::new(PendingSeekDemuxer));
    let install_receipt = worker
        .load_prepared_media(prepared_media, false)
        .expect("legacy prepared media command должна быть принята");
    let installed_snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.source_label.as_deref() == Some("aud014-legacy-fixture")
            && snapshot.media_instance_id.is_some()
    });
    let completion = wait_for_install_completion(&install_receipt);
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = completion
    else {
        panic!("legacy prepared media должна завершить compatibility install");
    };
    assert_eq!(
        installed_snapshot.media_instance_id,
        Some(media_instance_id)
    );
    (worker, media_instance_id)
}

/// Ждёт terminal compatibility install без предположений о межпоточном расписании.
fn wait_for_install_completion(
    receipt: &player_core::MediaInstallReceipt,
) -> MediaInstallCompletion {
    let deadline = Instant::now() + TEST_DEADLINE_BUDGET;
    loop {
        if let Some(completion) = receipt.try_take_completion() {
            return completion;
        }
        assert!(
            Instant::now() < deadline,
            "prepared media install не завершился до test deadline"
        );
        std::thread::yield_now();
    }
}

/// Читает настоящий worker snapshot до выполнения exact predicate-а.
fn wait_for_snapshot(
    worker: &mut PlayerWorker,
    predicate: impl Fn(&PlayerSnapshot) -> bool,
) -> PlayerSnapshot {
    let deadline = Instant::now() + TEST_DEADLINE_BUDGET;
    loop {
        let snapshot = worker.latest_snapshot(FrameCounters::default());
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "player snapshot не достиг ожидаемого состояния до test deadline"
        );
        std::thread::yield_now();
    }
}

/// Ставит exact request и возвращает app-owned receipt после реального port admission.
fn dispatch_admitted_seek(
    worker: &PlayerWorker,
    seek_port: &ControlledPreparedSeekPort,
    request_number: u64,
    media_instance_id: MediaInstanceId,
    target_position: Duration,
) -> (
    player_core::ExactTimelineSeekReceipt,
    PreparedDemuxSeekRequestId,
) {
    let receipt = worker
        .exact_timeline_seek(ExactTimelineSeekRequest {
            request_id: TimelineSeekRequestId::new(non_zero(request_number)),
            media_instance_id,
            target: MediaTime::from_duration(target_position),
            kind: TimelineSeekKind::SetPosition,
        })
        .expect("exact timeline seek command должна быть принята");
    let (prepared_request_id, prepared_request) = seek_port.wait_for_command();
    assert_eq!(prepared_request.timestamp, target_position);
    (receipt, prepared_request_id)
}

/// Ставит exact seek, не предполагая конкретной реализации demux boundary.
fn dispatch_exact_seek(
    worker: &PlayerWorker,
    request_number: u64,
    media_instance_id: MediaInstanceId,
    target_position: Duration,
) -> player_core::ExactTimelineSeekReceipt {
    worker
        .exact_timeline_seek(ExactTimelineSeekRequest {
            request_id: TimelineSeekRequestId::new(non_zero(request_number)),
            media_instance_id,
            target: MediaTime::from_duration(target_position),
            kind: TimelineSeekKind::SetPosition,
        })
        .expect("exact timeline seek command должна быть принята")
}

/// Регистрирует тот же media instance в настоящем process-lifetime PlaylistRuntime.
fn runtime_with_installed_media(
    media_instance_id: MediaInstanceId,
) -> (PlaylistRuntime, PlaylistRuntimeBinding, ActiveMediaIdentity) {
    let mut playlist_runtime = runtime();
    let binding = playlist_runtime
        .bind_resumed_app_state()
        .expect("playlist runtime binding");
    let active_media = playlist_runtime
        .register_successful_strong_install(
            MediaOpenRequestId::from_non_zero(non_zero(701)),
            MediaInstallRequestId::from_non_zero(non_zero(701)),
            media_instance_id,
            binding,
            ActiveMediaSource::LocalFile("aud014-fixture.mp4".into()),
            player_core::PlaybackIntent::StartPaused,
        )
        .expect("playlist runtime должен зарегистрировать worker media instance");
    (playlist_runtime, binding, active_media)
}

/// Выполняет настоящий capture/detach/rebind/resume и возвращает восстановленную позицию.
fn restored_position_after_checkpoint(
    playlist_runtime: &mut PlaylistRuntime,
    binding: PlaylistRuntimeBinding,
    snapshot: &PlayerSnapshot,
    checkpoint_position: LifecycleTimelineCheckpointPosition,
) -> SuspendedTimelineResumePosition {
    playlist_runtime
        .capture_suspended_media_checkpoint_after_seek_settlement(
            binding,
            snapshot,
            checkpoint_position,
        )
        .expect("settled suspend checkpoint должен быть захвачен");
    playlist_runtime.suspend_app_state_binding();
    playlist_runtime
        .bind_resumed_app_state()
        .expect("resume должен создать новый binding");
    playlist_runtime
        .begin_suspended_media_resume(false)
        .expect("captured checkpoint должен создать automatic resume attempt")
        .position
}

#[test]
fn applied_seek_before_deadline_restores_confirmed_target_position() {
    let (mut worker, media_instance_id) = installed_legacy_seek_worker();
    let initial_receipt =
        dispatch_exact_seek(&worker, 1, media_instance_id, Duration::from_secs(10));
    assert!(matches!(
        initial_receipt
            .wait_for_outcome_until(Instant::now() + TEST_DEADLINE_BUDGET)
            .expect("legacy initial seek должен завершиться"),
        ExactTimelineSeekOutcome::Applied { position, .. }
            if position == MediaTime::from_secs(10)
    ));
    let old_snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.current_position == Duration::from_secs(10)
    });
    let target_receipt =
        dispatch_exact_seek(&worker, 2, media_instance_id, Duration::from_secs(90));

    let (settlement, terminal_outcomes) = settle_timeline_seek_receipts_until(
        vec![target_receipt],
        Instant::now() + TEST_DEADLINE_BUDGET,
        old_snapshot.current_position,
    );
    assert!(matches!(
        terminal_outcomes.as_slice(),
        [ExactTimelineSeekOutcome::Applied { position, .. }]
            if *position == MediaTime::from_secs(90)
    ));
    assert_eq!(
        settlement,
        LifecycleTimelineSeekSettlement::Settled {
            checkpoint_position: Duration::from_secs(90),
        }
    );
    let post_settlement_snapshot = worker.latest_snapshot(FrameCounters::default());
    let (mut playlist_runtime, binding, _active_media) =
        runtime_with_installed_media(media_instance_id);

    assert_eq!(
        restored_position_after_checkpoint(
            &mut playlist_runtime,
            binding,
            &post_settlement_snapshot,
            settlement.checkpoint_position_policy(),
        ),
        SuspendedTimelineResumePosition::SeekTo(Duration::from_secs(90))
    );
    worker.shutdown().expect("player worker shutdown");
}

#[test]
fn admitted_seek_timeout_restores_documented_pre_seek_position() {
    let (mut worker, seek_port, media_instance_id) = installed_worker();
    let (initial_receipt, _initial_prepared_id) = dispatch_admitted_seek(
        &worker,
        &seek_port,
        11,
        media_instance_id,
        Duration::from_secs(10),
    );
    let initial_outcome = initial_receipt
        .wait_for_outcome_until(Instant::now() + TEST_DEADLINE_BUDGET)
        .expect("auto-completed initial seek должен завершиться до test deadline");
    assert!(matches!(
        initial_outcome,
        ExactTimelineSeekOutcome::Applied { .. }
    ));
    let old_snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.current_position == Duration::from_secs(10)
    });
    let (pending_receipt, _held_prepared_id) = dispatch_admitted_seek(
        &worker,
        &seek_port,
        12,
        media_instance_id,
        Duration::from_secs(90),
    );
    let seeking_snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.playback_state == PlaybackState::Seeking
            && snapshot.current_position == Duration::from_secs(10)
    });

    let (settlement, terminal_outcomes) = settle_timeline_seek_receipts_until(
        vec![pending_receipt],
        Instant::now(),
        old_snapshot.current_position,
    );
    assert!(terminal_outcomes.is_empty());
    assert_eq!(
        settlement,
        LifecycleTimelineSeekSettlement::DeadlineElapsed {
            checkpoint_position: Duration::from_secs(10),
            abandoned_receipt_count: 1,
        }
    );
    let (mut playlist_runtime, binding, _active_media) =
        runtime_with_installed_media(media_instance_id);

    assert_eq!(
        restored_position_after_checkpoint(
            &mut playlist_runtime,
            binding,
            &seeking_snapshot,
            settlement.checkpoint_position_policy(),
        ),
        SuspendedTimelineResumePosition::SeekTo(Duration::from_secs(10))
    );
    worker.shutdown().expect("player worker shutdown");
}
