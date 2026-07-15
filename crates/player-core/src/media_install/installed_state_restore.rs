//! Exact-instance восстановление position/track state после strong media install.

use std::fmt;
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};
use media_core::TrackId;

use crate::{MediaInstallRequestId, MediaInstanceId, PlayerError};

/// Явное действие над video/audio track без двусмысленного `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledTrackRestore {
    /// Сохраняет default selection нового media.
    KeepDefault,
    /// Выбирает exact track из восстановленного snapshot-а.
    Select(TrackId),
}

/// Явное действие над subtitle track, включая отключение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledSubtitleRestore {
    /// Сохраняет default subtitle selection нового media.
    KeepDefault,
    /// Явно отключает subtitle track.
    Disabled,
    /// Выбирает exact subtitle track.
    Select(TrackId),
}

/// Явное действие над media position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledPositionRestore {
    /// Не запускает seek и сохраняет начальную позицию.
    KeepStart,
    /// Запускает exact absolute seek только для matching installed instance-а.
    SeekTo(Duration),
}

/// Полный exact-instance restore intent после correlated `Installed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledMediaStateRestore {
    /// Exact install request, который создал target instance.
    pub request_id: MediaInstallRequestId,
    /// Exact installed instance; newer media не может принять этот restore.
    pub media_instance_id: MediaInstanceId,
    /// Video track action.
    pub video_track: InstalledTrackRestore,
    /// Audio track action.
    pub audio_track: InstalledTrackRestore,
    /// Subtitle track action.
    pub subtitle_track: InstalledSubtitleRestore,
    /// Position action.
    pub position: InstalledPositionRestore,
}

/// Этап, на котором owner отверг matching restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledMediaRestoreFailureStage {
    /// Video track selection.
    VideoTrack,
    /// Audio track selection.
    AudioTrack,
    /// Subtitle track selection.
    SubtitleTrack,
    /// Absolute seek dispatch внутри player owner-а.
    Position,
}

/// Typed причина, по которой exact resume position недоступна без terminal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledPositionUnavailableReason {
    /// Reopened source не поддерживает absolute seek (например live stream).
    SourceNotSeekable,
}

/// Authoritative owner outcome exact restore-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledMediaStateRestoreOutcome {
    /// Все requested actions применены к exact instance.
    Applied { media_instance_id: MediaInstanceId },
    /// Media установлено, но exact requested position недоступна у этого source.
    PositionUnavailable {
        media_instance_id: MediaInstanceId,
        requested_position: Duration,
        available_position: Duration,
        reason: InstalledPositionUnavailableReason,
    },
    /// Request ещё staged и не имеет installed instance.
    NotInstalledYet,
    /// Request никогда не был известен либо уже superseded до install.
    UnknownOrSupersededRequest,
    /// Request/instance принадлежит прежнему media и не может затронуть current.
    StaleInstance,
    /// Matching owner начал restore, но конкретная операция завершилась ошибкой.
    Failed {
        stage: InstalledMediaRestoreFailureStage,
        error: PlayerError,
    },
}

/// Fatal loss request-owned owner outcome-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledMediaStateRestoreReceiptError {
    /// Worker/owner исчез после transport acceptance, не опубликовав outcome.
    MissingOwnerOutcome,
}

impl fmt::Display for InstalledMediaStateRestoreReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOwnerOutcome => {
                formatter.write_str("player owner завершился без installed media restore outcome")
            }
        }
    }
}

impl std::error::Error for InstalledMediaStateRestoreReceiptError {}

/// Request-owned receipt отделяет enqueue от фактического owner apply.
pub struct InstalledMediaStateRestoreReceipt {
    request_id: MediaInstallRequestId,
    outcome_rx: Receiver<InstalledMediaStateRestoreOutcome>,
}

impl InstalledMediaStateRestoreReceipt {
    pub(crate) fn new(
        request_id: MediaInstallRequestId,
        outcome_rx: Receiver<InstalledMediaStateRestoreOutcome>,
    ) -> Self {
        Self {
            request_id,
            outcome_rx,
        }
    }

    /// Exact request identity receipt-а.
    #[must_use]
    pub const fn request_id(&self) -> MediaInstallRequestId {
        self.request_id
    }

    /// Неблокирующий event-driven drain owner outcome-а.
    pub fn try_take_outcome(
        &self,
    ) -> Result<Option<InstalledMediaStateRestoreOutcome>, InstalledMediaStateRestoreReceiptError>
    {
        match self.outcome_rx.try_recv() {
            Ok(outcome) => Ok(Some(outcome)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(InstalledMediaStateRestoreReceiptError::MissingOwnerOutcome)
            }
        }
    }

    /// Блокируется без polling spin до exact owner outcome-а или fatal disconnect-а.
    pub fn wait_for_outcome(
        &self,
    ) -> Result<InstalledMediaStateRestoreOutcome, InstalledMediaStateRestoreReceiptError> {
        self.outcome_rx
            .recv()
            .map_err(|_| InstalledMediaStateRestoreReceiptError::MissingOwnerOutcome)
    }
}

/// Internal classification exact request/instance mapping-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstalledMediaTargetMatch {
    Matching,
    NotInstalledYet,
    UnknownOrSupersededRequest,
    StaleInstance,
}
