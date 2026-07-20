//! Публичные serde/I/O-neutral типы shuffle traversal boundary.

use std::fmt;

use crate::PlaylistItemId;

use super::super::{QueueRestoreError, RemovalCurrentOutcome, TraversalCurrentEffect};

/// Максимальное число сохранённых фактических visits в rolling history.
pub const MAX_SHUFFLE_HISTORY_ENTRIES: usize = 1_024;

/// Типизированная позиция current внутри factual history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShuffleHistoryCursor(pub(in crate::queue::shuffle) usize);

impl ShuffleHistoryCursor {
    /// Создаёт persistence-facing cursor без преждевременной проверки history.
    pub fn from_index(index: usize) -> Self {
        Self(index)
    }

    /// Возвращает индекс для внешнего serde/I/O adapter-а.
    pub fn index(self) -> usize {
        self.0
    }
}

/// Serde-neutral exact snapshot включённого shuffle traversal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShuffleTraversalSnapshot {
    pub(in crate::queue::shuffle) history: Vec<PlaylistItemId>,
    pub(in crate::queue::shuffle) history_cursor: Option<ShuffleHistoryCursor>,
    pub(in crate::queue::shuffle) upcoming: Vec<PlaylistItemId>,
}

impl ShuffleTraversalSnapshot {
    /// Собирает persistence input; cross-field validation выполняет queue restore.
    pub fn new(
        history: Vec<PlaylistItemId>,
        history_cursor: Option<ShuffleHistoryCursor>,
        upcoming: Vec<PlaylistItemId>,
    ) -> Self {
        Self {
            history,
            history_cursor,
            upcoming,
        }
    }

    /// Возвращает factual history, где повторные visits разрешены.
    pub fn history(&self) -> &[PlaylistItemId] {
        &self.history
    }

    /// Возвращает точную persisted позицию current.
    pub fn history_cursor(&self) -> Option<ShuffleHistoryCursor> {
        self.history_cursor
    }

    /// Возвращает exact ordered upcoming текущего cycle.
    pub fn upcoming(&self) -> &[PlaylistItemId] {
        &self.upcoming
    }
}

/// Причина отказа строгого shuffle restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShuffleTraversalRestoreError {
    /// Factual history превышает доменный rolling cap.
    HistoryLimitExceeded { restored: usize, maximum: usize },
    /// History ссылается на отсутствующую canonical identity.
    HistoryItemNotCommitted { item_id: PlaylistItemId },
    /// Upcoming ссылается на отсутствующую canonical identity.
    UpcomingItemNotCommitted { item_id: PlaylistItemId },
    /// Set-like upcoming содержит duplicate identity.
    DuplicateUpcomingItem { item_id: PlaylistItemId },
    /// Cursor отсутствует либо присутствует несогласованно с history/current.
    InvalidHistoryCursor,
    /// Factual cursor указывает не на persisted current.
    CurrentDoesNotMatchHistory { current_item_id: PlaylistItemId },
    /// Current не может одновременно оставаться в upcoming текущего cycle.
    CurrentPresentInUpcoming { current_item_id: PlaylistItemId },
    /// Idle shuffle обязан содержать все canonical IDs ровно по одному.
    IdleUpcomingDoesNotCoverCanonicalQueue,
}

impl fmt::Display for ShuffleTraversalRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HistoryLimitExceeded { restored, maximum } => write!(
                formatter,
                "shuffle history содержит {restored} записей при максимуме {maximum}"
            ),
            Self::HistoryItemNotCommitted { item_id } => {
                write!(
                    formatter,
                    "shuffle history ссылается на отсутствующий item {item_id}"
                )
            }
            Self::UpcomingItemNotCommitted { item_id } => {
                write!(
                    formatter,
                    "shuffle upcoming ссылается на отсутствующий item {item_id}"
                )
            }
            Self::DuplicateUpcomingItem { item_id } => {
                write!(formatter, "shuffle upcoming повторяет item {item_id}")
            }
            Self::InvalidHistoryCursor => {
                formatter.write_str("shuffle history cursor не согласован с history/current")
            }
            Self::CurrentDoesNotMatchHistory { current_item_id } => write!(
                formatter,
                "shuffle history cursor не указывает на current item {current_item_id}"
            ),
            Self::CurrentPresentInUpcoming { current_item_id } => write!(
                formatter,
                "current item {current_item_id} ошибочно присутствует в shuffle upcoming"
            ),
            Self::IdleUpcomingDoesNotCoverCanonicalQueue => formatter
                .write_str("idle shuffle upcoming не содержит все canonical IDs ровно по одному"),
        }
    }
}

impl std::error::Error for ShuffleTraversalRestoreError {}

/// Полная ошибка atomic queue + shuffle restore.
#[derive(Debug)]
pub enum ShuffleQueueRestoreError {
    /// Canonical queue snapshot не прошёл базовую проверку.
    Queue(QueueRestoreError),
    /// Shuffle traversal не согласован с уже проверенной canonical queue.
    Traversal(ShuffleTraversalRestoreError),
}

impl fmt::Display for ShuffleQueueRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Queue(error) => write!(formatter, "canonical queue restore отклонён: {error}"),
            Self::Traversal(error) => {
                write!(formatter, "shuffle traversal restore отклонён: {error}")
            }
        }
    }
}

impl std::error::Error for ShuffleQueueRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Queue(error) => Some(error),
            Self::Traversal(error) => Some(error),
        }
    }
}

/// Ошибка атомарного shuffle toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShuffleToggleError {
    /// D08 install commit сейчас владеет mutation linearization.
    InstallCommitLinearizing,
    /// Traversal revision больше нельзя увеличить.
    TraversalRevisionExhausted,
}

impl fmt::Display for ShuffleToggleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => {
                formatter.write_str("shuffle toggle заблокирован install commit")
            }
            Self::TraversalRevisionExhausted => {
                formatter.write_str("shuffle toggle исчерпал traversal revision")
            }
        }
    }
}

impl std::error::Error for ShuffleToggleError {}

/// Результат idempotent shuffle toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShuffleToggleOutcome {
    /// Shuffle уже находился в требуемом состоянии.
    AlreadyEnabled,
    /// Shuffle включён с новым cycle.
    Enabled,
    /// Shuffle уже был выключен.
    AlreadyDisabled,
    /// Shuffle выключен, а history/upcoming/cursor полностью отброшены.
    Disabled,
}

/// Результат одного O(N) bulk removal commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BulkRemoveOutcome {
    /// Caller не передал ни одного ID.
    NoItemsRequested,
    /// Ни один requested ID не принадлежит canonical queue.
    NoMatchingItems,
    /// Все matching IDs удалены одной mutation.
    Removed {
        removed_item_count: usize,
        traversal_current_effect: TraversalCurrentEffect,
        current_outcome: RemovalCurrentOutcome,
    },
}

/// Ошибка preflight bulk removal; partial mutation запрещена.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BulkRemoveError {
    /// D08 reservation сейчас владеет mutation linearization.
    InstallCommitLinearizing,
    /// Caller передал subordinate playable part вместо owning compound identity.
    CompoundPartTarget {
        part_item_id: PlaylistItemId,
        compound_entry_id: crate::PlaylistEntryId,
    },
    /// Structural revision исчерпана.
    StructuralRevisionExhausted,
    /// Current removal требует недоступную traversal revision.
    TraversalRevisionExhausted,
}

impl fmt::Display for BulkRemoveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => {
                formatter.write_str("bulk remove заблокирован install commit")
            }
            Self::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => write!(
                formatter,
                "{part_item_id} является частью {compound_entry_id:?}; bulk remove требует group target"
            ),
            Self::StructuralRevisionExhausted => {
                formatter.write_str("bulk remove исчерпал structural revision")
            }
            Self::TraversalRevisionExhausted => {
                formatter.write_str("bulk remove исчерпал traversal revision")
            }
        }
    }
}

impl std::error::Error for BulkRemoveError {}
