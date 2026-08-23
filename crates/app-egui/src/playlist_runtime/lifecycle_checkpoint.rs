//! Typed выбор timeline-позиции для suspend/shutdown checkpoint-а.

use std::time::Duration;

/// Явно фиксирует происхождение позиции, которую lifecycle обязан сохранить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleTimelineCheckpointPosition {
    /// Pending seek отсутствовал, поэтому authoritative остаётся обычный player snapshot.
    LatestSnapshot,
    /// Все pending receipts терминальны; позиция учитывает последний подтверждённый `Applied`.
    SettledSeek(Duration),
    /// Lifecycle deadline отменил незавершённый seek и сохранил pre-seek позицию.
    CancelledPendingSeek(Duration),
    /// Player owner исчез без outcome-а; безопасным fallback остаётся pre-seek позиция.
    MissingSeekOwnerOutcome(Duration),
}

impl LifecycleTimelineCheckpointPosition {
    /// Возвращает явную позицию или разрешает владельцу использовать snapshot semantics.
    #[must_use]
    pub(crate) const fn explicit_position(self) -> Option<Duration> {
        match self {
            Self::LatestSnapshot => None,
            Self::SettledSeek(position)
            | Self::CancelledPendingSeek(position)
            | Self::MissingSeekOwnerOutcome(position) => Some(position),
        }
    }
}
