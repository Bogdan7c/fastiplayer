use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use media_core::{DemuxSeekResult, DemuxSeekability, Demuxer};
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendPortError, DetachedVideoBackendReply, DetachedVideoBackendRequest,
    DetachedVideoBackendResourceError, DetachedVideoBackendResourcePort,
};

use super::{MediaInstallControlReceipt, MediaInstallControlReceiptError};
use crate::worker::{COMMAND_CHANNEL_CAPACITY, PlayerCommandSender, WorkerCommand};
use crate::{
    AuthorizeInstallCommit, MediaInstallCompletion, MediaInstallControlOutcome,
    MediaInstallRequestId, PlayerCommand, PlayerWorker, PlayerWorkerConfig, PlayerWorkerSendError,
    PreparedMedia,
};

/// Empty prepared demuxer для no-audio/no-video worker transaction.
struct EmptyDemuxer;

impl Demuxer for EmptyDemuxer {
    /// Empty media не публикует tracks.
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    /// Duration известна, чтобы snapshot commit был observable.
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }

    /// Fake source остаётся seekable, хотя seek в тесте не вызывается.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    /// Empty media сразу сообщает EOF packets.
    fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
        Ok(None)
    }

    /// Seek не относится к transport test-у и возвращает явную ошибку.
    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!("empty worker test demuxer does not seek"))
    }
}

/// Port обязан остаться неиспользованным для media без video.
struct NoVideoResourcePort;

impl DetachedVideoBackendResourcePort for NoVideoResourcePort {
    type RequestId = MediaInstallRequestId;

    /// Defensive reply делает ошибочный video request observable без panic.
    fn request_detached_backend(
        &mut self,
        request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        Ok(DetachedVideoBackendReply::unavailable(
            *request.request_id(),
            DetachedVideoBackendResourceError::Unavailable {
                reason: "no-video port must not be requested".to_owned(),
            },
        ))
    }

    /// No-video transaction не публикует candidate status.
    fn publish_candidate_status(
        &mut self,
        _status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        Ok(())
    }

    /// No-video transaction не имеет player decoder half-а для cancel.
    fn cancel_candidate(
        &mut self,
        _request_id: Self::RequestId,
        _cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        Ok(())
    }
}

/// Ждёт owner-thread result с коротким deterministic deadline.
fn wait_until<T>(mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "player worker не опубликовал staged install outcome до deadline"
        );
        std::thread::yield_now();
    }
}

#[test]
fn public_worker_api_separates_enqueue_ready_authorization_and_installed() {
    let mut worker = PlayerWorker::spawn(PlayerWorkerConfig::default())
        .expect("player worker должен запуститься");
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(700).expect("test request id is non-zero"),
    );
    let prepared_media =
        PreparedMedia::from_external_label("worker candidate", Box::new(EmptyDemuxer));
    let receipt = worker
        .stage_prepared_media_install(
            request_id,
            prepared_media,
            false,
            Box::new(NoVideoResourcePort),
        )
        .expect("staged command должна войти в пустую worker queue");

    wait_until(|| receipt.try_take_ready_to_commit());
    assert!(receipt.try_take_completion().is_none());
    let control_receipt = worker
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect("authorization должна войти в worker queue");
    let control_outcome = wait_until(|| match control_receipt.try_take_outcome() {
        Ok(outcome) => outcome,
        Err(error) => panic!("accepted authorization потеряла owner outcome: {error}"),
    });
    assert_eq!(
        control_outcome,
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Installed {
            request_id: completion_request_id,
            ..
        }) if completion_request_id == request_id
    ));
    worker
        .shutdown()
        .expect("worker shutdown должен завершиться");
}

#[test]
fn authorization_queue_backpressure_returns_command_without_false_acceptance() {
    let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let sender = PlayerCommandSender {
        command_tx: command_tx.clone(),
    };
    for _ in 0..COMMAND_CHANNEL_CAPACITY {
        command_tx
            .try_send(WorkerCommand::Player(PlayerCommand::Pause))
            .expect("test queue должна принять fill command");
    }
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(701).expect("test request id is non-zero"),
    );

    let error = sender
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect_err("full queue должна отвергнуть authorization delivery");
    assert_eq!(error, PlayerWorkerSendError::Full);
}

#[test]
fn authorization_disconnect_is_transport_failure_not_install_rejection() {
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    drop(command_rx);
    let sender = PlayerCommandSender { command_tx };
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(703).expect("test request id is non-zero"),
    );

    let error = sender
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect_err("disconnected queue не может принять authorization");
    assert_eq!(error, PlayerWorkerSendError::Disconnected);
}

#[test]
fn dropped_owner_outcome_sender_is_fatal_missing_outcome_not_recoverable_reject() {
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(702).expect("test request id is non-zero"),
    );
    let (receipt, outcome_tx) = MediaInstallControlReceipt::new(request_id);
    drop(outcome_tx);

    assert_eq!(
        receipt.try_take_outcome(),
        Err(MediaInstallControlReceiptError::MissingOwnerOutcome)
    );
}
