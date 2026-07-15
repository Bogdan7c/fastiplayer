//! Exact release boundary для уже установленного media.
//!
//! Boundary нужен lifecycle owner-у после post-install restore failure: release адресован
//! одновременно strong request и конкретному instance, поэтому поздняя cleanup-команда не
//! может разрушить более новое media.

use std::fmt;

use crossbeam_channel::{Receiver, TryRecvError};

use crate::{MediaInstallRequestId, MediaInstanceId, PlayerError};

/// Exact intent освобождения уже установленного media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledMediaRelease {
    /// Strong request, который установил candidate.
    pub request_id: MediaInstallRequestId,
    /// Конкретный установленный instance, который разрешено освободить.
    pub media_instance_id: MediaInstanceId,
}

/// Authoritative owner outcome exact release-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledMediaReleaseOutcome {
    /// Matching current media освобождён и больше не является active instance.
    Applied { media_instance_id: MediaInstanceId },
    /// Request ещё не установлен либо уже отсутствует у owner-а.
    Absent,
    /// Request/instance устарел относительно текущего установленного media.
    StaleInstance,
    /// Matching owner начал release, но session отклонила операцию.
    Failed { error: PlayerError },
}

/// Ошибка чтения request-owned release receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledMediaReleaseReceiptError {
    /// Worker завершился, не опубликовав обязательный owner outcome.
    MissingOwnerOutcome,
}

impl fmt::Display for InstalledMediaReleaseReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOwnerOutcome => formatter.write_str(
                "player worker завершился без authoritative installed-media release outcome",
            ),
        }
    }
}

impl std::error::Error for InstalledMediaReleaseReceiptError {}

/// Request-owned receipt не приравнивает enqueue к фактическому release.
pub struct InstalledMediaReleaseReceipt {
    request_id: MediaInstallRequestId,
    outcome_rx: Receiver<InstalledMediaReleaseOutcome>,
}

impl InstalledMediaReleaseReceipt {
    pub(crate) fn new(
        request_id: MediaInstallRequestId,
        outcome_rx: Receiver<InstalledMediaReleaseOutcome>,
    ) -> Self {
        Self {
            request_id,
            outcome_rx,
        }
    }

    /// Возвращает strong request correlation receipt-а.
    #[must_use]
    pub const fn request_id(&self) -> MediaInstallRequestId {
        self.request_id
    }

    /// Неблокирующе забирает owner outcome ровно один раз.
    pub fn try_take_outcome(
        &self,
    ) -> Result<Option<InstalledMediaReleaseOutcome>, InstalledMediaReleaseReceiptError> {
        match self.outcome_rx.try_recv() {
            Ok(outcome) => Ok(Some(outcome)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(InstalledMediaReleaseReceiptError::MissingOwnerOutcome)
            }
        }
    }

    /// Блокирующе ждёт lossless owner outcome вне realtime/player owner thread-а.
    pub fn wait_for_outcome(
        &self,
    ) -> Result<InstalledMediaReleaseOutcome, InstalledMediaReleaseReceiptError> {
        self.outcome_rx
            .recv()
            .map_err(|_| InstalledMediaReleaseReceiptError::MissingOwnerOutcome)
    }
}
