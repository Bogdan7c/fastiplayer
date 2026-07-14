//! Exact-instance transport для playlist/controller и будущего MPRIS routing.

use std::fmt;

use crossbeam_channel::{Receiver, TryRecvError};

use crate::{MediaInstanceId, PlaybackIntent, PlayerError};

/// Недеструктивное действие над уже установленным media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactMediaTransportAction {
    /// Применяет stable Playing/Paused без seek и без uncorrelated fallback command-а.
    SetPlaybackIntent { intent: PlaybackIntent },
    /// Перезапускает timeline с нуля и применяет explicit Playing/Paused intent.
    RestartFromBeginning { intent: PlaybackIntent },
    /// Реализует нейтральный Stop как Pause, затем seek к нулю без очистки media owners.
    NeutralStop,
}

/// Exact request никогда не выводит identity из locator/title/текущего snapshot-а caller-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactMediaTransportRequest {
    /// Единственная media instance, над которой разрешено действие.
    pub media_instance_id: MediaInstanceId,
    /// Typed transport intent без destructive `PlayerCommand::Stop`.
    pub action: ExactMediaTransportAction,
}

/// Fallible этап exact transport-а сохраняется для partial-result обработки app owner-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactMediaTransportFailureStage {
    /// Pause не применился, поэтому seek не начинался.
    Pause,
    /// Seek к нулю не применился; preceding Pause мог уже завершиться.
    SeekToBeginning,
    /// Play после успешного seek не применился.
    Play,
}

/// Authoritative owner outcome не выдаёт enqueue за фактическое применение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactMediaTransportOutcome {
    /// Полное действие применено к matching instance.
    Applied { media_instance_id: MediaInstanceId },
    /// Target отсутствует либо уже заменён более новой instance.
    StaleInstance {
        requested_media_instance_id: MediaInstanceId,
        current_media_instance_id: Option<MediaInstanceId>,
    },
    /// Ни один подэтап действия не успел завершиться.
    Failed {
        media_instance_id: MediaInstanceId,
        stage: ExactMediaTransportFailureStage,
        error: PlayerError,
    },
    /// Предыдущий этап применился, но следующий завершился ошибкой.
    PartiallyApplied {
        media_instance_id: MediaInstanceId,
        completed_stage: ExactMediaTransportFailureStage,
        failed_stage: ExactMediaTransportFailureStage,
        error: PlayerError,
    },
}

/// Fatal loss request-owned result-а после command transport acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactMediaTransportReceiptError {
    /// Worker завершился, не опубликовав authoritative outcome.
    MissingOwnerOutcome,
}

impl fmt::Display for ExactMediaTransportReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOwnerOutcome => {
                formatter.write_str("player owner завершился без exact media transport outcome")
            }
        }
    }
}

impl std::error::Error for ExactMediaTransportReceiptError {}

/// Receipt отделяет bounded enqueue от serialized owner turn-а.
pub struct ExactMediaTransportReceipt {
    media_instance_id: MediaInstanceId,
    outcome_rx: Receiver<ExactMediaTransportOutcome>,
}

impl ExactMediaTransportReceipt {
    pub(crate) fn new(
        media_instance_id: MediaInstanceId,
        outcome_rx: Receiver<ExactMediaTransportOutcome>,
    ) -> Self {
        Self {
            media_instance_id,
            outcome_rx,
        }
    }

    /// Exact instance identity receipt-а.
    #[must_use]
    pub const fn media_instance_id(&self) -> MediaInstanceId {
        self.media_instance_id
    }

    /// Неблокирующий event-driven drain authoritative outcome-а.
    pub fn try_take_outcome(
        &self,
    ) -> Result<Option<ExactMediaTransportOutcome>, ExactMediaTransportReceiptError> {
        match self.outcome_rx.try_recv() {
            Ok(outcome) => Ok(Some(outcome)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(ExactMediaTransportReceiptError::MissingOwnerOutcome)
            }
        }
    }

    /// Блокируется без polling spin до exact owner outcome-а или fatal disconnect-а.
    pub fn wait_for_outcome(
        &self,
    ) -> Result<ExactMediaTransportOutcome, ExactMediaTransportReceiptError> {
        self.outcome_rx
            .recv()
            .map_err(|_| ExactMediaTransportReceiptError::MissingOwnerOutcome)
    }
}
