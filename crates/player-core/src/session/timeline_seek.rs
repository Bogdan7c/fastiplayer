//! Exact correlated timeline seek, завершаемый только фактическим seek commit-ом.

use crossbeam_channel::Sender;
use media_core::MediaTime;

use crate::{
    ExactTimelineSeekOutcome, ExactTimelineSeekRequest, PlayerError, PlayerErrorKind, SeekRequest,
    TimelineSeekKind, media_install::timeline_seek::PendingExactTimelineSeek,
};

use super::PlayerSession;

impl PlayerSession {
    pub(crate) fn begin_exact_timeline_seek(
        &mut self,
        request: ExactTimelineSeekRequest,
        outcome_tx: Sender<ExactTimelineSeekOutcome>,
    ) {
        self.fail_pending_exact_timeline_seek(PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            "seek superseded by a newer request",
        ));
        if self.snapshot.media_instance_id != Some(request.media_instance_id) {
            let _ = outcome_tx.send(ExactTimelineSeekOutcome::StaleInstance {
                request_id: request.request_id,
            });
            return;
        }
        if !self.snapshot.timeline.seekable || !self.pipeline.has_demuxer() {
            let _ = outcome_tx.send(ExactTimelineSeekOutcome::NotSeekable {
                request_id: request.request_id,
            });
            return;
        }
        if self
            .snapshot
            .timeline
            .duration
            .is_some_and(|duration| request.target.as_duration() > duration.as_duration())
        {
            let outcome = match request.kind {
                TimelineSeekKind::SetPosition => ExactTimelineSeekOutcome::InvalidRange {
                    request_id: request.request_id,
                },
                TimelineSeekKind::Relative => ExactTimelineSeekOutcome::BeyondEnd {
                    request_id: request.request_id,
                },
            };
            let _ = outcome_tx.send(outcome);
            return;
        }
        if let Err(error) = self.seek(SeekRequest::absolute(request.target)) {
            let _ = outcome_tx.send(ExactTimelineSeekOutcome::Failed {
                request_id: request.request_id,
                error,
            });
            return;
        }
        if !self.seek_runtime.seek_landing_active() && !self.snapshot.timeline.seeking {
            let _ = outcome_tx.send(ExactTimelineSeekOutcome::Failed {
                request_id: request.request_id,
                error: PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    "exact timeline seek не был принят seek transaction",
                ),
            });
            return;
        }
        self.pending_exact_timeline_seek = Some(PendingExactTimelineSeek {
            request,
            outcome_tx,
        });
    }

    pub(super) fn finish_exact_timeline_seek(&mut self, position: MediaTime) {
        let Some(pending) = self.pending_exact_timeline_seek.take() else {
            return;
        };
        let outcome = if self.snapshot.media_instance_id == Some(pending.request.media_instance_id)
        {
            ExactTimelineSeekOutcome::Applied {
                request_id: pending.request.request_id,
                media_instance_id: pending.request.media_instance_id,
                position: self.relative_position_for_source(position),
            }
        } else {
            ExactTimelineSeekOutcome::StaleInstance {
                request_id: pending.request.request_id,
            }
        };
        let _ = pending.outcome_tx.send(outcome);
    }

    pub(super) fn fail_pending_exact_timeline_seek(&mut self, error: PlayerError) {
        let Some(pending) = self.pending_exact_timeline_seek.take() else {
            return;
        };
        let _ = pending.outcome_tx.send(ExactTimelineSeekOutcome::Failed {
            request_id: pending.request.request_id,
            error,
        });
    }

    pub(crate) fn reconcile_exact_timeline_seek_identity(&mut self) {
        let is_stale = self
            .pending_exact_timeline_seek
            .as_ref()
            .is_some_and(|pending| {
                self.snapshot.media_instance_id != Some(pending.request.media_instance_id)
            });
        if !is_stale {
            return;
        }
        let pending = self
            .pending_exact_timeline_seek
            .take()
            .expect("stale pending seek was just observed");
        let _ = pending
            .outcome_tx
            .send(ExactTimelineSeekOutcome::StaleInstance {
                request_id: pending.request.request_id,
            });
    }
}
