//! Lifecycle-only barrier для pending exact timeline seek receipts.

use std::time::{Duration, Instant};

use player_core::{ExactTimelineSeekOutcome, ExactTimelineSeekReceiptError};
use tracing::{debug, warn};

use super::{AppState, RelativeBeyondEndNavigation, desktop_seek_request_id};
use crate::playlist_runtime::{LifecycleTimelineCheckpointPosition, PlaylistRuntime};

/// Terminal результат bounded lifecycle barrier-а без потери checkpoint policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleTimelineSeekSettlement {
    /// На lifecycle-входе pending seek отсутствовали.
    NoPendingSeek,
    /// Все принятые seek-и завершились до общего deadline.
    Settled {
        /// Последняя подтверждённая позиция после последовательной обработки outcomes.
        checkpoint_position: Duration,
    },
    /// Общий deadline истёк; оставшиеся receipts отменяются вместе с renderer-bound owner-ом.
    DeadlineElapsed {
        /// Последняя подтверждённая позиция, которую lifecycle обязан сохранить.
        checkpoint_position: Duration,
        /// Число текущего и последующих receipts, не получивших terminal outcome.
        abandoned_receipt_count: usize,
    },
    /// Player owner исчез без обязательного terminal outcome-а.
    MissingOwnerOutcome {
        /// Последняя подтверждённая позиция до потери owner-а.
        checkpoint_position: Duration,
        /// Число receipts, terminal состояние которых больше нельзя доказать.
        abandoned_receipt_count: usize,
    },
}

impl LifecycleTimelineSeekSettlement {
    /// Преобразует barrier outcome в явную checkpoint policy для владельца persistence.
    #[must_use]
    pub(crate) const fn checkpoint_position_policy(self) -> LifecycleTimelineCheckpointPosition {
        match self {
            Self::NoPendingSeek => LifecycleTimelineCheckpointPosition::LatestSnapshot,
            Self::Settled {
                checkpoint_position,
            } => LifecycleTimelineCheckpointPosition::SettledSeek(checkpoint_position),
            Self::DeadlineElapsed {
                checkpoint_position,
                ..
            } => LifecycleTimelineCheckpointPosition::CancelledPendingSeek(checkpoint_position),
            Self::MissingOwnerOutcome {
                checkpoint_position,
                ..
            } => LifecycleTimelineCheckpointPosition::MissingSeekOwnerOutcome(checkpoint_position),
        }
    }
}

/// Ожидает request-owned receipts и отдельно возвращает уже доказанные terminal outcomes.
pub(crate) fn settle_timeline_seek_receipts_until(
    receipts: Vec<player_core::ExactTimelineSeekReceipt>,
    deadline: Instant,
    pre_seek_position: Duration,
) -> (
    LifecycleTimelineSeekSettlement,
    Vec<ExactTimelineSeekOutcome>,
) {
    if receipts.is_empty() {
        return (LifecycleTimelineSeekSettlement::NoPendingSeek, Vec::new());
    }

    let receipt_count = receipts.len();
    let mut checkpoint_position = pre_seek_position;
    let mut terminal_outcomes = Vec::with_capacity(receipt_count);
    for (receipt_index, receipt) in receipts.into_iter().enumerate() {
        match receipt.wait_for_outcome_until(deadline) {
            Ok(outcome) => {
                if let ExactTimelineSeekOutcome::Applied { position, .. } = &outcome {
                    checkpoint_position = position.as_duration();
                }
                terminal_outcomes.push(outcome);
            }
            Err(ExactTimelineSeekReceiptError::DeadlineElapsed) => {
                return (
                    LifecycleTimelineSeekSettlement::DeadlineElapsed {
                        checkpoint_position,
                        abandoned_receipt_count: receipt_count - receipt_index,
                    },
                    terminal_outcomes,
                );
            }
            Err(ExactTimelineSeekReceiptError::MissingOwnerOutcome) => {
                return (
                    LifecycleTimelineSeekSettlement::MissingOwnerOutcome {
                        checkpoint_position,
                        abandoned_receipt_count: receipt_count - receipt_index,
                    },
                    terminal_outcomes,
                );
            }
        }
    }

    (
        LifecycleTimelineSeekSettlement::Settled {
            checkpoint_position,
        },
        terminal_outcomes,
    )
}

impl AppState {
    /// Ждёт pending seek receipts до общего lifecycle deadline без busy-loop-а.
    ///
    /// `pre_seek_position` берётся из snapshot, прочитанного до ожидания. Каждый
    /// подтверждённый `Applied` сдвигает checkpoint вперёд; timeout никогда не
    /// превращает ещё не подтверждённый UI intent в persisted position.
    pub(crate) fn settle_pending_timeline_seek_for_lifecycle(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        deadline: Instant,
        pre_seek_position: Duration,
    ) -> LifecycleTimelineSeekSettlement {
        let receipts = std::mem::take(&mut self.playlist_transport.timeline_seek_receipts);
        let (settlement, terminal_outcomes) =
            settle_timeline_seek_receipts_until(receipts, deadline, pre_seek_position);
        for outcome in terminal_outcomes {
            if let Some(navigation) =
                self.record_exact_timeline_seek_outcome(playlist_runtime, outcome)
            {
                debug!(
                    request_id = navigation.request_id.get(),
                    media_instance_id = navigation.media_instance_id.get(),
                    "Lifecycle settlement подавил queue navigation после relative BeyondEnd"
                );
            }
        }
        settlement
    }

    /// Применяет один terminal outcome одинаково для frame polling и lifecycle barrier-а.
    pub(super) fn record_exact_timeline_seek_outcome(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        outcome: ExactTimelineSeekOutcome,
    ) -> Option<RelativeBeyondEndNavigation> {
        let playlist_binding = self.playlist_runtime_binding();
        let resume_seek_checkpoint_allowed =
            self.last_player_snapshot.timeline.mode != media_core::TimelineMode::Live;
        match outcome {
            ExactTimelineSeekOutcome::Applied {
                request_id,
                media_instance_id,
                position,
            } => {
                let desktop_request_id = desktop_seek_request_id(request_id);
                playlist_runtime.publish_desktop_seeked(desktop_request_id, position);
                if resume_seek_checkpoint_allowed && let Some(binding) = playlist_binding {
                    playlist_runtime.record_confirmed_resume_seek(
                        binding,
                        media_instance_id,
                        position.as_duration(),
                    );
                }
                None
            }
            ExactTimelineSeekOutcome::BeyondEnd {
                request_id,
                media_instance_id,
            } => {
                playlist_runtime.record_desktop_seek_outcome(
                    desktop_integration::DesktopTimelineSeekOutcome::BeyondEnd {
                        request_id: desktop_seek_request_id(request_id),
                    },
                );
                Some(RelativeBeyondEndNavigation {
                    request_id,
                    media_instance_id,
                })
            }
            outcome => {
                let desktop_outcome = desktop_outcome_without_seeked_signal(&outcome);
                playlist_runtime.record_desktop_seek_outcome(desktop_outcome);
                debug!(
                    ?desktop_outcome,
                    "Exact timeline seek завершился без Seeked signal"
                );
                None
            }
        }
    }
}

/// Переводит terminal non-applied outcome в process-neutral desktop контракт.
fn desktop_outcome_without_seeked_signal(
    outcome: &ExactTimelineSeekOutcome,
) -> desktop_integration::DesktopTimelineSeekOutcome {
    match outcome {
        ExactTimelineSeekOutcome::InvalidRange { request_id, .. } => {
            desktop_integration::DesktopTimelineSeekOutcome::InvalidRange {
                request_id: desktop_seek_request_id(*request_id),
            }
        }
        ExactTimelineSeekOutcome::StaleInstance { request_id, .. } => {
            desktop_integration::DesktopTimelineSeekOutcome::StaleInstance {
                request_id: desktop_seek_request_id(*request_id),
            }
        }
        ExactTimelineSeekOutcome::NotSeekable { request_id, .. } => {
            desktop_integration::DesktopTimelineSeekOutcome::NotSeekable {
                request_id: desktop_seek_request_id(*request_id),
            }
        }
        ExactTimelineSeekOutcome::Expired { request_id, .. } => {
            desktop_integration::DesktopTimelineSeekOutcome::Expired {
                request_id: desktop_seek_request_id(*request_id),
            }
        }
        ExactTimelineSeekOutcome::Failed { request_id, .. } => {
            desktop_integration::DesktopTimelineSeekOutcome::Failed {
                request_id: desktop_seek_request_id(*request_id),
            }
        }
        ExactTimelineSeekOutcome::Applied { .. } | ExactTimelineSeekOutcome::BeyondEnd { .. } => {
            warn!("Applied/BeyondEnd попали в non-applied desktop outcome adapter");
            unreachable!("Applied and BeyondEnd are handled before this adapter")
        }
    }
}
