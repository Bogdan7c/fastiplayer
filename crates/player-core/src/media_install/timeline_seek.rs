//! Neutral exact-instance seek boundary для app/MPRIS без D-Bus типов.

use std::{fmt, num::NonZeroU64};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
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
    },
    BeyondEnd {
        request_id: TimelineSeekRequestId,
    },
    StaleInstance {
        request_id: TimelineSeekRequestId,
    },
    NotSeekable {
        request_id: TimelineSeekRequestId,
    },
    Expired {
        request_id: TimelineSeekRequestId,
        requested_position: MediaTime,
        available_range: Option<TimelineRange>,
    },
    Failed {
        request_id: TimelineSeekRequestId,
        error: PlayerError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactTimelineSeekReceiptError {
    MissingOwnerOutcome,
}

impl fmt::Display for ExactTimelineSeekReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("player owner завершился без exact timeline seek outcome")
    }
}

impl std::error::Error for ExactTimelineSeekReceiptError {}

/// Request-owned receipt хранит ровно один terminal outcome.
pub struct ExactTimelineSeekReceipt {
    request_id: TimelineSeekRequestId,
    outcome_rx: Receiver<ExactTimelineSeekOutcome>,
}

impl ExactTimelineSeekReceipt {
    pub(crate) fn new(
        request_id: TimelineSeekRequestId,
        outcome_rx: Receiver<ExactTimelineSeekOutcome>,
    ) -> Self {
        Self {
            request_id,
            outcome_rx,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> TimelineSeekRequestId {
        self.request_id
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
}

/// Session-owned pending completion связывает async seek commit с exact request.
pub(crate) struct PendingExactTimelineSeek {
    pub request: ExactTimelineSeekRequest,
    pub outcome_tx: Sender<ExactTimelineSeekOutcome>,
}
