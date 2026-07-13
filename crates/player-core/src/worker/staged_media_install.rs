use std::fmt;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded};

use crate::{
    MediaInstallControl, MediaInstallControlOutcome, MediaInstallPhaseCompletionPort,
    MediaInstallRequestId, MediaInstallVideoResourcePort, PreparedMedia,
};

/// Полный transport payload strong staged install command-а.
pub(super) struct StagePreparedMediaInstallCommand {
    /// Exact install request identity.
    pub(super) request_id: MediaInstallRequestId,

    /// Detached prepared media ownership candidate-а.
    pub(super) prepared_media: PreparedMedia,

    /// Playback intent, применяемый только при accepted authorization.
    pub(super) autoplay: bool,

    /// Request-owned ready/terminal publication port.
    pub(super) install_port: Arc<dyn MediaInstallPhaseCompletionPort>,

    /// Port к exact app-owned detached backend/materializer half-у.
    pub(super) video_resource_port: MediaInstallVideoResourcePort,
}

/// Ordered control payload с отдельным lossless owner outcome.
pub(super) struct MediaInstallControlCommand {
    /// Exact authorization/cancel control.
    pub(super) control: MediaInstallControl,

    /// Capacity-one terminal sender, независимый от lossy worker events.
    pub(super) outcome_tx: Sender<MediaInstallControlOutcome>,
}

/// Ошибка lossless control-outcome receipt после successful command enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallControlReceiptError {
    /// Worker завершился, не записав typed authorization/cancellation outcome.
    MissingOwnerOutcome,
}

impl fmt::Display for MediaInstallControlReceiptError {
    /// Форматирует fatal owner-outcome invariant без потери классификации.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOwnerOutcome => {
                formatter.write_str("player owner завершился без media install control outcome")
            }
        }
    }
}

impl std::error::Error for MediaInstallControlReceiptError {}

/// Request-owned receipt фактического применения authorization/cancel player owner-ом.
#[derive(Debug)]
pub struct MediaInstallControlReceipt {
    /// Exact request identity защищает caller от смешивания concurrent receipts.
    request_id: MediaInstallRequestId,

    /// Capacity-one terminal channel не зависит от lossy event stream-а.
    outcome_rx: Receiver<MediaInstallControlOutcome>,
}

impl MediaInstallControlReceipt {
    /// Создаёт paired receipt/writer до неблокирующего command enqueue.
    pub(super) fn new(
        request_id: MediaInstallRequestId,
    ) -> (Self, Sender<MediaInstallControlOutcome>) {
        let (outcome_tx, outcome_rx) = bounded(1);
        (
            Self {
                request_id,
                outcome_rx,
            },
            outcome_tx,
        )
    }

    /// Возвращает exact request identity receipt-а.
    #[must_use]
    pub const fn request_id(&self) -> MediaInstallRequestId {
        self.request_id
    }

    /// Неблокирующе забирает owner outcome exactly once.
    pub fn try_take_outcome(
        &self,
    ) -> Result<Option<MediaInstallControlOutcome>, MediaInstallControlReceiptError> {
        match self.outcome_rx.try_recv() {
            Ok(outcome) => Ok(Some(outcome)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(MediaInstallControlReceiptError::MissingOwnerOutcome)
            }
        }
    }
}

#[cfg(test)]
mod tests;
