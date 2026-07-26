//! Player-owned strict same-lineage position gate до destructive authorization.

use std::sync::Arc;
use std::time::{Duration, Instant};

use media_core::{DemuxSeekResult, MediaTime, TimelineMode};
use video_backend_api::DetachedVideoBackendCandidateStatus;

use crate::seek_state::demux_seek_request_for_transaction;
use crate::{
    InstalledLiveEdgeAdjustmentReason, MediaInstallControl, MediaInstallControlOutcome,
    MediaInstallFailure, MediaInstallFailureStage, MediaInstallPositionPreparation,
    MediaInstallRequestId, MediaInstanceId, PlayerError, PlayerErrorKind,
    PrepareMediaInstallPosition, PreparedDemuxSeekOutcome, PreparedDemuxSeekRequestId, SeekMode,
};

use super::{
    PlayerSession, PreparedStagedMedia, StagedMediaInstall, StagedMediaPreparation,
    ensure_matching_timeline_mode, live_edge_commit,
};

const STAGED_POSITION_RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(2);

pub(super) enum StagedPositionPreparation {
    NotRequired,
    SameLineage {
        expected_old_media_instance_id: MediaInstanceId,
        state: StagedPositionState,
    },
}

pub(super) enum StagedPositionState {
    NotStarted,
    WaitingWorkerReceipt(PendingStagedPositionSeek),
    Prepared(PreparedStagedPosition),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OldTimelineIdentity {
    mode: TimelineMode,
    duration: Option<Duration>,
    playback_window: Option<crate::MediaPlaybackWindow>,
}

pub(super) struct PendingStagedPositionSeek {
    request_id: PreparedDemuxSeekRequestId,
    initial_old_position: Duration,
    requested_source_position: MediaTime,
    old_timeline: OldTimelineIdentity,
    timeout_deadline: Instant,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum StagedPositionCommit {
    KeepStart,
    Seek {
        target_position: MediaTime,
        result: DemuxSeekResult,
    },
    AdjustedToLiveEdge {
        requested_position: Duration,
        live_edge: Duration,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PreparedStagedPosition {
    commit: StagedPositionCommit,
    initial_old_position: Duration,
    old_timeline: OldTimelineIdentity,
}

pub(crate) struct InstalledStagedPosition {
    pub(crate) request_id: MediaInstallRequestId,
    pub(crate) media_instance_id: MediaInstanceId,
    pub(crate) outcome: InstalledStagedPositionOutcome,
}

pub(crate) enum InstalledStagedPositionOutcome {
    Completed,
    AwaitingSeekCommit {
        seek_generation: u64,
    },
    AdjustedToLiveEdge {
        requested_position: Duration,
        live_edge: Duration,
        reason: InstalledLiveEdgeAdjustmentReason,
    },
    Failed(PlayerError),
}

impl StagedPositionPreparation {
    pub(super) const fn new(policy: MediaInstallPositionPreparation) -> Self {
        match policy {
            MediaInstallPositionPreparation::NotRequired => Self::NotRequired,
            MediaInstallPositionPreparation::SameLineage {
                expected_old_media_instance_id,
            } => Self::SameLineage {
                expected_old_media_instance_id,
                state: StagedPositionState::NotStarted,
            },
        }
    }

    pub(super) const fn is_required(&self) -> bool {
        matches!(self, Self::SameLineage { .. })
    }
}

impl PlayerSession {
    /// Запускает exact same-lineage position gate после app owner validation.
    pub(crate) fn prepare_staged_media_position(&mut self, request: PrepareMediaInstallPosition) {
        self.begin_staged_position_preparation(request);
    }

    pub(super) fn begin_staged_position_preparation(
        &mut self,
        request: PrepareMediaInstallPosition,
    ) {
        let Some(mut staged) = self.staged_media_install.active.take() else {
            return;
        };
        if staged.request_id != request.request_id || !staged.protocol.begin_position_preparation()
        {
            self.staged_media_install.active = Some(staged);
            return;
        }

        let result = self.start_staged_position_preparation(&mut staged);
        match result {
            Ok(PositionPreparationProgress::Ready(prepared_position)) => {
                if let StagedPositionPreparation::SameLineage { state, .. } =
                    &mut staged.position_preparation
                {
                    *state = StagedPositionState::Prepared(prepared_position);
                }
                staged.protocol.mark_ready_to_commit();
                self.staged_media_install.active = Some(staged);
            }
            Ok(PositionPreparationProgress::Pending(pending)) => {
                if let StagedPositionPreparation::SameLineage { state, .. } =
                    &mut staged.position_preparation
                {
                    *state = StagedPositionState::WaitingWorkerReceipt(pending);
                }
                self.staged_media_install.active = Some(staged);
            }
            Err(failure) => self.finish_staged_position_failure(staged, failure),
        }
    }

    fn start_staged_position_preparation(
        &mut self,
        staged: &mut StagedMediaInstall,
    ) -> Result<PositionPreparationProgress, MediaInstallFailure> {
        let StagedPositionPreparation::SameLineage {
            expected_old_media_instance_id,
            state: StagedPositionState::NotStarted,
        } = staged.position_preparation
        else {
            return Err(position_failure(
                "same-lineage position gate state is invalid",
            ));
        };
        self.validate_old_instance(expected_old_media_instance_id)?;
        let initial_old_position = self.snapshot.current_position;
        let old_timeline = self.old_timeline_identity();
        let prepared = ready_prepared_media_mut(staged)?;
        ensure_matching_timeline_mode(old_timeline.mode, prepared)?;

        if matches!(
            prepared.prepared_media.timeline_mode(),
            crate::PreparedMediaTimelineMode::Live { .. }
        ) {
            let live_snapshot = prepared
                .prepared_media
                .staged_live_timeline_snapshot()
                .ok_or_else(|| position_failure("staged live timeline is unavailable"))?;
            let requested = MediaTime::from_duration(initial_old_position);
            if !live_snapshot
                .state
                .seekable_range()
                .is_some_and(|range| range.contains(requested))
            {
                return Ok(PositionPreparationProgress::Ready(PreparedStagedPosition {
                    commit: live_edge_commit(initial_old_position, live_snapshot),
                    initial_old_position,
                    old_timeline,
                }));
            }
        } else {
            validate_static_target(prepared, initial_old_position)?;
            if initial_old_position.is_zero() {
                return Ok(PositionPreparationProgress::Ready(PreparedStagedPosition {
                    commit: StagedPositionCommit::KeepStart,
                    initial_old_position,
                    old_timeline,
                }));
            }
        }

        let public_target = MediaTime::from_duration(initial_old_position);
        let source_target = prepared
            .prepared_media
            .absolute_position_for_relative(public_target);
        let request = demux_seek_request_for_transaction(
            prepared.video_plan.is_some(),
            source_target.as_duration(),
            SeekMode::Accurate,
        )
        .map_err(|_| position_failure("staged position uses unsupported seek mode"))?;

        match prepared.demux_seek_runtime.enqueue_detached(request) {
            Ok(Some(request_id)) => Ok(PositionPreparationProgress::Pending(
                PendingStagedPositionSeek {
                    request_id,
                    initial_old_position,
                    requested_source_position: source_target,
                    old_timeline,
                    timeout_deadline: Instant::now()
                        .checked_add(self.staged_video_preflight_timeout)
                        .unwrap_or_else(Instant::now),
                },
            )),
            Ok(None) => {
                let result = prepared
                    .prepared_media
                    .seek_detached(request)
                    .map_err(|error| {
                        position_failure(format!("staged demux seek failed: {error}"))
                    })?;
                self.finish_position_result(
                    expected_old_media_instance_id,
                    prepared,
                    initial_old_position,
                    old_timeline,
                    source_target,
                    result,
                )
                .map(|commit| {
                    PositionPreparationProgress::Ready(PreparedStagedPosition {
                        commit,
                        initial_old_position,
                        old_timeline,
                    })
                })
            }
            Err(error) => Err(MediaInstallFailure::new(
                MediaInstallFailureStage::PositionPreparation,
                error,
            )),
        }
    }

    pub(crate) fn service_staged_position_preparation(&mut self) {
        let Some(mut staged) = self.staged_media_install.active.take() else {
            return;
        };
        let (expected_old_media_instance_id, pending) = match &staged.position_preparation {
            StagedPositionPreparation::SameLineage {
                expected_old_media_instance_id,
                state: StagedPositionState::WaitingWorkerReceipt(pending),
            } => (*expected_old_media_instance_id, pending.request_id),
            _ => {
                self.staged_media_install.active = Some(staged);
                return;
            }
        };

        let timeout_deadline = match &staged.position_preparation {
            StagedPositionPreparation::SameLineage {
                state: StagedPositionState::WaitingWorkerReceipt(pending),
                ..
            } => pending.timeout_deadline,
            _ => unreachable!("pending identity came from waiting state"),
        };
        if Instant::now() >= timeout_deadline {
            self.finish_staged_position_failure(
                staged,
                position_failure("staged demux worker seek exceeded bounded deadline"),
            );
            return;
        }

        let receipt = {
            let prepared = match ready_prepared_media_mut(&mut staged) {
                Ok(prepared) => prepared,
                Err(failure) => {
                    self.finish_staged_position_failure(staged, failure);
                    return;
                }
            };
            let mut matching = None;
            while let Some(receipt) = prepared.demux_seek_runtime.poll_receipt() {
                if receipt.request_id == pending {
                    matching = Some(receipt);
                    break;
                }
            }
            matching
        };
        let Some(receipt) = receipt else {
            self.staged_media_install.active = Some(staged);
            return;
        };

        let pending = match std::mem::replace(
            &mut staged.position_preparation,
            StagedPositionPreparation::NotRequired,
        ) {
            StagedPositionPreparation::SameLineage {
                state: StagedPositionState::WaitingWorkerReceipt(pending),
                ..
            } => pending,
            _ => unreachable!("matching receipt requires pending staged position"),
        };
        let result = match receipt.outcome {
            PreparedDemuxSeekOutcome::Succeeded(result)
                if result.requested_position == pending.requested_source_position =>
            {
                result
            }
            PreparedDemuxSeekOutcome::Succeeded(_) => {
                self.finish_staged_position_failure(
                    staged,
                    position_failure("staged seek receipt target mismatched request"),
                );
                return;
            }
            PreparedDemuxSeekOutcome::Failed => {
                self.finish_staged_position_failure(
                    staged,
                    position_failure("staged demux worker seek failed"),
                );
                return;
            }
            PreparedDemuxSeekOutcome::Cancelled
            | PreparedDemuxSeekOutcome::Superseded
            | PreparedDemuxSeekOutcome::Stale => {
                self.finish_staged_position_failure(
                    staged,
                    position_failure("staged demux worker receipt is not authoritative"),
                );
                return;
            }
        };

        let commit = {
            let prepared = match ready_prepared_media_mut(&mut staged) {
                Ok(prepared) => prepared,
                Err(failure) => {
                    self.finish_staged_position_failure(staged, failure);
                    return;
                }
            };
            self.finish_position_result(
                expected_old_media_instance_id,
                prepared,
                pending.initial_old_position,
                pending.old_timeline,
                pending.requested_source_position,
                result,
            )
        };
        match commit {
            Ok(commit) => {
                staged.position_preparation = StagedPositionPreparation::SameLineage {
                    expected_old_media_instance_id,
                    state: StagedPositionState::Prepared(PreparedStagedPosition {
                        commit,
                        initial_old_position: pending.initial_old_position,
                        old_timeline: pending.old_timeline,
                    }),
                };
                staged.protocol.mark_ready_to_commit();
                self.staged_media_install.active = Some(staged);
            }
            Err(failure) => self.finish_staged_position_failure(staged, failure),
        }
    }

    fn finish_position_result(
        &self,
        expected_old_media_instance_id: MediaInstanceId,
        prepared: &PreparedStagedMedia,
        initial_old_position: Duration,
        old_timeline: OldTimelineIdentity,
        requested_source_position: MediaTime,
        result: DemuxSeekResult,
    ) -> Result<StagedPositionCommit, MediaInstallFailure> {
        let fresh_position = self.validate_fresh_old_position(
            expected_old_media_instance_id,
            initial_old_position,
            old_timeline,
        )?;
        if result.requested_position != requested_source_position {
            return Err(position_failure(
                "staged demux result changed requested target",
            ));
        }
        validate_target_and_result(prepared, fresh_position, result)
    }

    pub(super) fn finalize_staged_position_before_authorization(
        &self,
        staged: &mut StagedMediaInstall,
    ) -> Result<Option<StagedPositionCommit>, MediaInstallFailure> {
        let (expected_old_media_instance_id, prepared_position) = match &staged.position_preparation
        {
            StagedPositionPreparation::SameLineage {
                expected_old_media_instance_id,
                state: StagedPositionState::Prepared(commit),
            } => (*expected_old_media_instance_id, *commit),
            StagedPositionPreparation::NotRequired => return Ok(None),
            _ => {
                return Err(position_failure(
                    "position gate is not ready for authorization",
                ));
            }
        };
        let fresh_position = self.validate_fresh_old_position(
            expected_old_media_instance_id,
            prepared_position.initial_old_position,
            prepared_position.old_timeline,
        )?;
        let prepared = ready_prepared_media_mut(staged)?;
        ensure_matching_timeline_mode(prepared_position.old_timeline.mode, prepared)?;

        let refreshed = match prepared_position.commit {
            StagedPositionCommit::KeepStart => {
                validate_static_target(prepared, fresh_position)?;
                if fresh_position.is_zero() {
                    StagedPositionCommit::KeepStart
                } else {
                    return Err(position_failure(
                        "old playback moved after zero-target preparation without demux anchor",
                    ));
                }
            }
            StagedPositionCommit::Seek { result, .. } => {
                validate_target_and_result(prepared, fresh_position, result)?
            }
            StagedPositionCommit::AdjustedToLiveEdge { .. } => {
                let snapshot = prepared
                    .prepared_media
                    .staged_live_timeline_snapshot()
                    .ok_or_else(|| position_failure("staged live timeline disappeared"))?;
                let requested = MediaTime::from_duration(fresh_position);
                if snapshot
                    .state
                    .seekable_range()
                    .is_some_and(|range| range.contains(requested))
                {
                    return Err(position_failure(
                        "live target became retained without a prepared demux anchor",
                    ));
                }
                live_edge_commit(fresh_position, snapshot)
            }
        };
        Ok(Some(refreshed))
    }

    fn validate_old_instance(
        &self,
        expected_old_media_instance_id: MediaInstanceId,
    ) -> Result<(), MediaInstallFailure> {
        if self.snapshot.media_instance_id != Some(expected_old_media_instance_id) {
            return Err(position_failure(
                "old media instance changed before authorization",
            ));
        }
        Ok(())
    }

    fn validate_fresh_old_position(
        &self,
        expected_old_media_instance_id: MediaInstanceId,
        initial_old_position: Duration,
        old_timeline: OldTimelineIdentity,
    ) -> Result<Duration, MediaInstallFailure> {
        self.validate_old_instance(expected_old_media_instance_id)?;
        if self.old_timeline_identity() != old_timeline {
            return Err(position_failure(
                "old media timeline mutated during staged seek",
            ));
        }
        let fresh_position = self.snapshot.current_position;
        if fresh_position < initial_old_position {
            return Err(position_failure("old playback position moved backward"));
        }
        Ok(fresh_position)
    }

    fn old_timeline_identity(&self) -> OldTimelineIdentity {
        OldTimelineIdentity {
            mode: self.snapshot.timeline.mode,
            duration: self.source_duration,
            playback_window: self.playback_window,
        }
    }

    fn finish_staged_position_failure(
        &mut self,
        mut staged: StagedMediaInstall,
        failure: MediaInstallFailure,
    ) {
        staged.protocol.complete_failed(failure);
        self.playback_intent_control
            .forget_staged_request(staged.request_id);
        self.staged_media_install.last_terminal_request_id = Some(staged.request_id);
    }

    pub(super) fn staged_position_wakeup_delay(&self) -> Option<Duration> {
        self.staged_media_install
            .active
            .as_ref()
            .and_then(|staged| {
                let StagedPositionPreparation::SameLineage {
                    state: StagedPositionState::WaitingWorkerReceipt(pending),
                    ..
                } = &staged.position_preparation
                else {
                    return None;
                };
                Some(
                    STAGED_POSITION_RECEIPT_POLL_INTERVAL.min(
                        pending
                            .timeout_deadline
                            .saturating_duration_since(Instant::now()),
                    ),
                )
            })
    }

    /// Применяет exact authorization/cancel в ordered owner stream.
    ///
    /// D52 update и commit сериализуются общим playback-intent mutex. Update, принятый до
    /// входа owner-а в commit closure, попадает в commit; update после выхода видит только
    /// exact just-installed request/instance mapping.
    pub(crate) fn apply_staged_media_install_control(
        &mut self,
        control: MediaInstallControl,
    ) -> MediaInstallControlOutcome {
        let control_request_id = super::media_install_control_request_id(control);
        let Some(mut staged_install) = self.staged_media_install.active.take() else {
            return if self.staged_media_install.last_terminal_request_id == Some(control_request_id)
            {
                MediaInstallControlOutcome::AlreadyTerminal
            } else {
                MediaInstallControlOutcome::StaleRequest
            };
        };

        if staged_install.request_id != control_request_id {
            self.staged_media_install.active = Some(staged_install);
            return MediaInstallControlOutcome::StaleRequest;
        }

        if matches!(control, MediaInstallControl::Authorize(_))
            && !staged_install.protocol.is_ready_to_commit()
        {
            self.staged_media_install.active = Some(staged_install);
            return MediaInstallControlOutcome::NotReady;
        }

        let (outcome, cancellation_cause) = match control {
            MediaInstallControl::Authorize(authorization) => {
                let position =
                    match self.finalize_staged_position_before_authorization(&mut staged_install) {
                        Ok(position) => position,
                        Err(failure) => {
                            staged_install.protocol.complete_failed(failure);
                            self.playback_intent_control
                                .forget_staged_request(staged_install.request_id);
                            self.staged_media_install.last_terminal_request_id =
                                Some(staged_install.request_id);
                            return MediaInstallControlOutcome::AuthorizationRejectedBeforeCommit;
                        }
                    };
                let StagedMediaPreparation::Ready(prepared_media) = &mut staged_install.preparation
                else {
                    self.staged_media_install.active = Some(staged_install);
                    return MediaInstallControlOutcome::NotReady;
                };
                let Some(prepared_media) = prepared_media.take() else {
                    self.staged_media_install.last_terminal_request_id =
                        Some(staged_install.request_id);
                    return MediaInstallControlOutcome::AlreadyTerminal;
                };
                let prepared_commit = prepared_media.into_commit(
                    staged_install.started_video_backend.take(),
                    staged_install.request_id,
                    position,
                );
                let media_instance_id = prepared_commit.media_instance_id;
                let playback_intent_control = Arc::clone(&self.playback_intent_control);
                let outcome = staged_install.protocol.apply_control(
                    MediaInstallControl::Authorize(authorization),
                    || {
                        let applied_intent = playback_intent_control.commit_staged_request(
                            staged_install.request_id,
                            media_instance_id,
                            |accepted_intent| {
                                self.commit_staged_media(prepared_commit, accepted_intent);
                            },
                        );
                        (media_instance_id, applied_intent)
                    },
                );
                (outcome, None)
            }
            MediaInstallControl::Cancel(cancellation) => {
                let outcome = staged_install
                    .protocol
                    .apply_control(MediaInstallControl::Cancel(cancellation), || {
                        unreachable!("cancel control не вызывает media commit closure")
                    });
                self.playback_intent_control
                    .forget_staged_request(staged_install.request_id);
                (outcome, Some(cancellation.cause))
            }
        };

        match outcome {
            MediaInstallControlOutcome::AuthorizationAccepted => {
                self.staged_media_install.last_terminal_request_id =
                    Some(staged_install.request_id);
            }
            MediaInstallControlOutcome::CancellationAccepted => {
                if let Some(cause) = cancellation_cause
                    && let Some(video_resource_port) = staged_install.video_resource_port.as_mut()
                    && let Err(error) = video_resource_port.publish_candidate_status(
                        DetachedVideoBackendCandidateStatus::Cancelled {
                            request_id: staged_install.request_id,
                            cause: super::detached_cancellation_cause(cause),
                        },
                    )
                {
                    tracing::warn!(
                        request_id = staged_install.request_id.get(),
                        %error,
                        "player terminal опубликован, но app candidate cancellation status не доставлен"
                    );
                }
                self.staged_media_install.last_terminal_request_id =
                    Some(staged_install.request_id);
            }
            MediaInstallControlOutcome::StaleRequest | MediaInstallControlOutcome::NotReady => {
                self.staged_media_install.active = Some(staged_install);
            }
            MediaInstallControlOutcome::AlreadyTerminal => {
                self.staged_media_install.last_terminal_request_id =
                    Some(staged_install.request_id);
            }
            MediaInstallControlOutcome::AuthorizationRejectedBeforeCommit => {
                self.staged_media_install.last_terminal_request_id =
                    Some(staged_install.request_id);
            }
        }
        outcome
    }
}

enum PositionPreparationProgress {
    Ready(PreparedStagedPosition),
    Pending(PendingStagedPositionSeek),
}

fn ready_prepared_media_mut(
    staged: &mut StagedMediaInstall,
) -> Result<&mut PreparedStagedMedia, MediaInstallFailure> {
    let StagedMediaPreparation::Ready(Some(prepared)) = &mut staged.preparation else {
        return Err(position_failure("staged media payload is not ready"));
    };
    Ok(prepared)
}

fn validate_static_target(
    prepared: &PreparedStagedMedia,
    target: Duration,
) -> Result<(), MediaInstallFailure> {
    if prepared
        .prepared_media
        .public_duration()
        .is_some_and(|duration| target > duration.as_duration())
    {
        return Err(position_failure(
            "same-lineage candidate is shorter than target",
        ));
    }
    if !target.is_zero()
        && !matches!(
            prepared.prepared_media.seekability(),
            media_core::DemuxSeekability::Seekable
        )
    {
        return Err(position_failure(
            "same-lineage candidate is not seekable at non-zero target",
        ));
    }
    Ok(())
}

fn validate_target_and_result(
    prepared: &PreparedStagedMedia,
    fresh_position: Duration,
    result: DemuxSeekResult,
) -> Result<StagedPositionCommit, MediaInstallFailure> {
    let target_position = prepared
        .prepared_media
        .absolute_position_for_relative(MediaTime::from_duration(fresh_position));
    if matches!(
        prepared.prepared_media.timeline_mode(),
        crate::PreparedMediaTimelineMode::Live { .. }
    ) {
        let snapshot = prepared
            .prepared_media
            .staged_live_timeline_snapshot()
            .ok_or_else(|| position_failure("staged live timeline disappeared"))?;
        let Some(range) = snapshot.state.seekable_range() else {
            // Seek уже выполнен на старом DVR snapshot. Подмена результата на новый live edge
            // рассинхронизировала бы опубликованную позицию и фактический cursor demuxer-а.
            return Err(position_failure(
                "prepared live seek target expired before authorization",
            ));
        };
        if !range.contains(target_position) {
            return Err(position_failure(
                "prepared live seek target expired before authorization",
            ));
        }
        if !range.contains(result.actual_position) {
            return Err(position_failure(
                "prepared live seek anchor expired before authorization",
            ));
        }
    } else {
        validate_static_target(prepared, fresh_position)?;
    }

    if result.actual_position > target_position {
        return Err(position_failure(
            "staged demux anchor lies after authoritative target",
        ));
    }
    Ok(StagedPositionCommit::Seek {
        target_position,
        result,
    })
}

pub(super) fn position_failure(message: impl Into<String>) -> MediaInstallFailure {
    let error = PlayerError::new(PlayerErrorKind::SeekUnavailable, message);
    MediaInstallFailure::new(MediaInstallFailureStage::PositionPreparation, error)
}
