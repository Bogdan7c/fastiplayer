//! Exact-instance восстановление position/track state после strong media install.

use std::fmt;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
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
    /// Восстанавливает абсолютную live-позицию same-item switch-а по свежему timeline.
    ///
    /// Player сам перечитывает latest snapshot установленного live port-а: retained
    /// DVR target проходит обычный seek lifecycle, а expired/no-DVR target принимает
    /// provider-declared safe live edge без app-side range inspection или clamp-а.
    RestoreLiveSameItemPosition {
        /// Абсолютная позиция старого live instance-а перед commit barrier-ом.
        previous_absolute_position: Duration,
    },
    /// Усыновляет preauthorization same-lineage result без второго demux seek-а.
    AdoptPreparedSameLineagePosition,
}

/// Причина, по которой live same-item restore принял свежий safe live edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledLiveEdgeAdjustmentReason {
    /// Новый provider не объявил DVR range, поэтому seek старой позиции невозможен.
    DvrWindowUnavailable,
    /// Старая абсолютная позиция уже не входит в свежий DVR range нового port-а.
    PreviousPositionOutsideDvr {
        /// Fresh provider-owned range, использованный для exact membership check-а.
        available_range: media_core::TimelineRange,
    },
    /// Timeline продвинулся между final staged observation и install; нового demux anchor нет.
    PreparedAnchorUnavailableAfterTimelineAdvance {
        available_range: media_core::TimelineRange,
    },
}

/// Явное восстановление громкости exact installed media instance-а.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InstalledVolumeRestore {
    /// Сохраняет актуальную громкость player session без дополнительной записи.
    KeepCurrent,
    /// Применяет свежую громкость, снятую непосредственно перед install barrier-ом.
    Set(f32),
}

/// Полный exact-instance restore intent после correlated `Installed`.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Exact volume action.
    pub volume: InstalledVolumeRestore,
    /// Position action.
    pub position: InstalledPositionRestore,
}

/// Session-owned ожидание terminal seek outcome-а для exact installed restore.
pub(crate) struct PendingInstalledPositionRestore {
    /// Install request, который всё ещё должен владеть current instance.
    pub request_id: MediaInstallRequestId,
    /// Exact media instance, к которому был применён restore.
    pub media_instance_id: MediaInstanceId,
    /// Seek generation запрещает принять commit от заменившей операции.
    pub seek_generation: u64,
    /// Adopted staged seek сохраняет exact prepared demux anchor внутри fresh live DVR.
    pub requires_live_anchor_retention: bool,
    /// Request-owned канал authoritative restore outcome-а.
    pub outcome_tx: Sender<InstalledMediaStateRestoreOutcome>,
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
    /// Volume validation/application.
    Volume,
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
    /// Live same-item restore принял fresh provider-declared safe edge.
    AdjustedToLiveEdge {
        /// Exact instance, к которому применена корректировка.
        media_instance_id: MediaInstanceId,
        /// Абсолютная позиция старого instance-а, которую пытались сохранить.
        requested_position: Duration,
        /// Fresh safe live edge нового generation.
        live_edge: Duration,
        /// Typed причина отказа от восстановления старой позиции.
        reason: InstalledLiveEdgeAdjustmentReason,
    },
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
