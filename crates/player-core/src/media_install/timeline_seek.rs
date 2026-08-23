//! Neutral exact-instance seek boundary для app/MPRIS без D-Bus типов.

use std::{fmt, num::NonZeroU64, time::Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use media_core::{MediaTime, TimelineRange};

use crate::{MediaInstanceId, PlayerError};

/// Process-neutral correlation identity одного timeline seek запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineSeekRequestId(NonZeroU64);

impl TimelineSeekRequestId {
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Разделяет spec-range SetPosition и relative seek, который может означать Next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineSeekKind {
    SetPosition,
    Relative,
}

/// Exact request не выводит identity из mutable latest snapshot после enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactTimelineSeekRequest {
    pub request_id: TimelineSeekRequestId,
    pub media_instance_id: MediaInstanceId,
    pub target: MediaTime,
    pub kind: TimelineSeekKind,
}

/// Terminal owner outcome; enqueue сам по себе никогда не считается Applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactTimelineSeekOutcome {
    Applied {
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
        position: MediaTime,
    },
    InvalidRange {
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
    },
    BeyondEnd {
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
    },
    StaleInstance {
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
    },
    NotSeekable {
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
    },
    Expired {
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
        requested_position: MediaTime,
        available_range: Option<TimelineRange>,
    },
    Failed {
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
        error: PlayerError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactTimelineSeekReceiptError {
    /// Общий lifecycle deadline истёк раньше terminal owner outcome-а.
    DeadlineElapsed,
    /// Player owner исчез, не опубликовав обязательный terminal outcome.
    MissingOwnerOutcome,
}

impl fmt::Display for ExactTimelineSeekReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeadlineElapsed => {
                formatter.write_str("deadline ожидания exact timeline seek outcome истёк")
            }
            Self::MissingOwnerOutcome => {
                formatter.write_str("player owner завершился без exact timeline seek outcome")
            }
        }
    }
}

impl std::error::Error for ExactTimelineSeekReceiptError {}

/// Request-owned receipt хранит ровно один terminal outcome.
pub struct ExactTimelineSeekReceipt {
    request_id: TimelineSeekRequestId,
    media_instance_id: MediaInstanceId,
    outcome_rx: Receiver<ExactTimelineSeekOutcome>,
}

impl ExactTimelineSeekReceipt {
    pub(crate) fn new(
        request_id: TimelineSeekRequestId,
        media_instance_id: MediaInstanceId,
        outcome_rx: Receiver<ExactTimelineSeekOutcome>,
    ) -> Self {
        Self {
            request_id,
            media_instance_id,
            outcome_rx,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> TimelineSeekRequestId {
        self.request_id
    }

    /// Exact media instance остаётся доступен даже до terminal owner outcome-а.
    #[must_use]
    pub const fn media_instance_id(&self) -> MediaInstanceId {
        self.media_instance_id
    }

    pub fn try_take_outcome(
        &self,
    ) -> Result<Option<ExactTimelineSeekOutcome>, ExactTimelineSeekReceiptError> {
        match self.outcome_rx.try_recv() {
            Ok(outcome) => Ok(Some(outcome)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(ExactTimelineSeekReceiptError::MissingOwnerOutcome)
            }
        }
    }

    /// Блокирует только до переданного общего deadline и возвращает terminal outcome.
    ///
    /// Абсолютный deadline не позволяет каждому receipt-у начать новый полный timeout:
    /// несколько pending seek-ов делят один lifecycle-бюджет последовательно.
    pub fn wait_for_outcome_until(
        &self,
        deadline: Instant,
    ) -> Result<ExactTimelineSeekOutcome, ExactTimelineSeekReceiptError> {
        match self.outcome_rx.recv_deadline(deadline) {
            Ok(outcome) => Ok(outcome),
            Err(RecvTimeoutError::Timeout) => Err(ExactTimelineSeekReceiptError::DeadlineElapsed),
            Err(RecvTimeoutError::Disconnected) => {
                Err(ExactTimelineSeekReceiptError::MissingOwnerOutcome)
            }
        }
    }
}

/// Session-owned pending completion связывает async seek commit с exact request.
pub(crate) struct PendingExactTimelineSeek {
    pub request: ExactTimelineSeekRequest,
    pub outcome_tx: Sender<ExactTimelineSeekOutcome>,
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, time::Duration};

    use crossbeam_channel::bounded;

    use super::*;

    /// Создаёт минимальный receipt с test-owned sender-ом terminal outcome-а.
    fn receipt() -> (
        Sender<ExactTimelineSeekOutcome>,
        ExactTimelineSeekReceipt,
        ExactTimelineSeekRequest,
    ) {
        let request = ExactTimelineSeekRequest {
            request_id: TimelineSeekRequestId::new(NonZeroU64::MIN),
            media_instance_id: MediaInstanceId::from_non_zero(NonZeroU64::MIN),
            target: MediaTime::ZERO,
            kind: TimelineSeekKind::SetPosition,
        };
        let (outcome_tx, outcome_rx) = bounded(1);
        let receipt = ExactTimelineSeekReceipt::new(
            request.request_id,
            request.media_instance_id,
            outcome_rx,
        );
        (outcome_tx, receipt, request)
    }

    #[test]
    fn bounded_wait_returns_terminal_owner_outcome() {
        let (outcome_tx, receipt, request) = receipt();
        let expected = ExactTimelineSeekOutcome::Applied {
            request_id: request.request_id,
            media_instance_id: request.media_instance_id,
            position: request.target,
        };
        outcome_tx
            .send(expected.clone())
            .expect("test owner должен сохранить connected receipt");

        assert_eq!(
            receipt.wait_for_outcome_until(Instant::now() + Duration::from_secs(1)),
            Ok(expected)
        );
    }

    #[test]
    fn bounded_wait_distinguishes_deadline_from_missing_owner() {
        let (outcome_tx, receipt, _request) = receipt();

        assert_eq!(
            receipt.wait_for_outcome_until(Instant::now()),
            Err(ExactTimelineSeekReceiptError::DeadlineElapsed)
        );

        drop(outcome_tx);
        assert_eq!(
            receipt.wait_for_outcome_until(Instant::now() + Duration::from_secs(1)),
            Err(ExactTimelineSeekReceiptError::MissingOwnerOutcome)
        );
    }
}
