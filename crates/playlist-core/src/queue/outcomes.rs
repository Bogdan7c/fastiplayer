//! Typed queue outcomes/errors без secret-bearing payload formatting.

use std::fmt;

use crate::{PlaylistEntryId, PlaylistItemId};

use super::{QueueRevisionSnapshot, RemovalCurrentOutcome, TraversalCurrentItemId};

/// IDs, опубликованные только вместе с успешным domain commit.
#[derive(Clone, PartialEq, Eq)]
pub struct AllocatedPlaylistItemIds(pub(super) Vec<PlaylistItemId>);

impl AllocatedPlaylistItemIds {
    /// Возвращает committed IDs в canonical порядке соответствующего batch.
    pub fn as_slice(&self) -> &[PlaylistItemId] {
        &self.0
    }

    /// Передаёт ownership committed IDs вызывающему orchestration layer.
    pub fn into_vec(self) -> Vec<PlaylistItemId> {
        self.0
    }
}

/// Результат D67 append-а, который атомарно принимает только доступный prefix.
#[derive(Clone, PartialEq, Eq)]
pub struct CappedTailAppendOutcome {
    pub(super) allocated_item_ids: AllocatedPlaylistItemIds,
    pub(super) capacity_rejected: usize,
}

impl CappedTailAppendOutcome {
    /// Передаёт ownership только действительно committed IDs и rejected count.
    pub fn into_parts(self) -> (Vec<PlaylistItemId>, usize) {
        (self.allocated_item_ids.into_vec(), self.capacity_rejected)
    }
}

impl fmt::Debug for CappedTailAppendOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CappedTailAppendOutcome")
            .field("allocated_item_ids", &self.allocated_item_ids)
            .field("capacity_rejected", &self.capacity_rejected)
            .finish()
    }
}

impl fmt::Debug for AllocatedPlaylistItemIds {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AllocatedPlaylistItemIds")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for AllocatedPlaylistItemIds {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} committed playlist item IDs", self.0.len())
    }
}

/// Успешный или осознанно no-op append outcome.
#[derive(Clone, PartialEq, Eq)]
pub enum AddItemsOutcome {
    /// Все drafts опубликованы атомарно с перечисленными IDs.
    Added(AllocatedPlaylistItemIds),
    /// Пустой batch не изменил queue/revisions/allocator.
    NoItemsProvided,
}

impl fmt::Debug for AddItemsOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Added(ids) => formatter.debug_tuple("Added").field(ids).finish(),
            Self::NoItemsProvided => formatter.write_str("NoItemsProvided"),
        }
    }
}

impl fmt::Display for AddItemsOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Added(ids) => fmt::Display::fmt(ids, formatter),
            Self::NoItemsProvided => {
                formatter.write_str("append batch is empty; queue is not changed")
            }
        }
    }
}

/// Причина атомарного отказа append boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddItemsError {
    /// D08 reservation временно линеаризует structural/traversal commit.
    InstallCommitLinearizing,
    /// Batch превысил hard capacity.
    CapacityExceeded {
        /// Текущий committed размер.
        current: usize,
        /// Число новых drafts.
        requested: usize,
        /// Именованный hard limit.
        maximum: usize,
    },
    /// Checked fixed-width allocator не может выдать весь batch.
    ItemIdExhausted,
    /// Allocator invariant обнаружил уже существующий будущий ID.
    ItemIdCollision { item_id: PlaylistItemId },
    /// Structural revision исчерпала fixed-width counter.
    StructuralRevisionExhausted,
}

impl fmt::Debug for AddItemsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::CapacityExceeded {
                current,
                requested,
                maximum,
            } => formatter
                .debug_struct("CapacityExceeded")
                .field("current", current)
                .field("requested", requested)
                .field("maximum", maximum)
                .finish(),
            Self::ItemIdExhausted => formatter.write_str("ItemIdExhausted"),
            Self::ItemIdCollision { item_id } => formatter
                .debug_struct("ItemIdCollision")
                .field("item_id", item_id)
                .finish(),
            Self::StructuralRevisionExhausted => formatter.write_str("StructuralRevisionExhausted"),
        }
    }
}

impl fmt::Display for AddItemsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit temporarily blocks queue change")
            }
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "append exceeded the queue limit {maximum}")
            }
            Self::ItemIdExhausted => {
                formatter.write_str("The range of PlaylistItemId is exhausted")
            }
            Self::ItemIdCollision { .. } => {
                formatter.write_str("allocator proposed an existing PlaylistItemId")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision exhausted")
            }
        }
    }
}

impl std::error::Error for AddItemsError {}

/// Влияние structural mutation на optional traversal current.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraversalCurrentEffect {
    /// Current не существовал либо ссылался на другую строку.
    Preserved,
    /// Mutation удалила current и установила его в `None`.
    Cleared,
}

impl fmt::Debug for TraversalCurrentEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preserved => formatter.write_str("TraversalCurrentEffect::Preserved"),
            Self::Cleared => formatter.write_str("TraversalCurrentEffect::Cleared"),
        }
    }
}

impl fmt::Display for TraversalCurrentEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preserved => formatter.write_str("traversal current saved"),
            Self::Cleared => formatter.write_str("traversal current cleaned"),
        }
    }
}

/// Outcome полного replacement canonical queue.
#[derive(Clone, PartialEq, Eq)]
pub enum ReplaceQueueOutcome {
    /// Новый non-empty canonical список опубликован с новыми stable IDs.
    Replaced {
        /// IDs нового списка в canonical порядке.
        allocated_item_ids: AllocatedPlaylistItemIds,
        /// Replacement всегда очищает прежний current в Session 02 API.
        traversal_current_effect: TraversalCurrentEffect,
    },
    /// Replacement пустым списком очистил существующую очередь.
    Cleared {
        /// Число удалённых committed строк.
        removed_item_count: usize,
        /// Был ли очищен traversal current.
        traversal_current_effect: TraversalCurrentEffect,
    },
    /// Пустая очередь уже соответствовала пустому replacement.
    AlreadyEmpty,
}

impl fmt::Debug for ReplaceQueueOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replaced {
                allocated_item_ids,
                traversal_current_effect,
            } => formatter
                .debug_struct("Replaced")
                .field("allocated_item_ids", allocated_item_ids)
                .field("traversal_current_effect", traversal_current_effect)
                .finish(),
            Self::Cleared {
                removed_item_count,
                traversal_current_effect,
            } => formatter
                .debug_struct("Cleared")
                .field("removed_item_count", removed_item_count)
                .field("traversal_current_effect", traversal_current_effect)
                .finish(),
            Self::AlreadyEmpty => formatter.write_str("AlreadyEmpty"),
        }
    }
}

impl fmt::Display for ReplaceQueueOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replaced {
                allocated_item_ids, ..
            } => write!(formatter, "queue replaced: {allocated_item_ids}"),
            Self::Cleared {
                removed_item_count, ..
            } => write!(formatter, "queue cleared: {removed_item_count} deleted"),
            Self::AlreadyEmpty => formatter.write_str("queue is empty."),
        }
    }
}

/// Причина атомарного отказа replace boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReplaceQueueError {
    /// D08 reservation удерживает mutation lock.
    InstallCommitLinearizing,
    /// Candidate сам по себе превышает hard capacity.
    CapacityExceeded { requested: usize, maximum: usize },
    /// Checked allocator не может выдать весь replacement range.
    ItemIdExhausted,
    /// Allocator предложил ID, присутствующий в old committed queue.
    ItemIdCollision { item_id: PlaylistItemId },
    /// Structural revision нельзя продвинуть.
    StructuralRevisionExhausted,
    /// Traversal revision нельзя продвинуть при очистке current.
    TraversalRevisionExhausted,
}

impl fmt::Debug for ReplaceQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::CapacityExceeded { requested, maximum } => formatter
                .debug_struct("CapacityExceeded")
                .field("requested", requested)
                .field("maximum", maximum)
                .finish(),
            Self::ItemIdExhausted => formatter.write_str("ItemIdExhausted"),
            Self::ItemIdCollision { item_id } => formatter
                .debug_struct("ItemIdCollision")
                .field("item_id", item_id)
                .finish(),
            Self::StructuralRevisionExhausted => formatter.write_str("StructuralRevisionExhausted"),
            Self::TraversalRevisionExhausted => formatter.write_str("TraversalRevisionExhausted"),
        }
    }
}

impl fmt::Display for ReplaceQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit temporarily blocks replacement")
            }
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "replacement exceeded the queue limit {maximum}")
            }
            Self::ItemIdExhausted => {
                formatter.write_str("The range of PlaylistItemId is exhausted")
            }
            Self::ItemIdCollision { .. } => {
                formatter.write_str("allocator proposed an existing PlaylistItemId")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision exhausted")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision exhausted"),
        }
    }
}

impl std::error::Error for ReplaceQueueError {}

/// Outcome identity-based remove.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RemoveItemOutcome {
    /// Exact committed top-level entry удалена.
    Removed {
        /// Удалённая structural identity.
        entry_id: PlaylistEntryId,
        /// Влияние удаления на traversal current.
        traversal_current_effect: TraversalCurrentEffect,
        /// D71 persisted-current outcome без неявного successor-а.
        current_outcome: RemovalCurrentOutcome,
    },
    /// Structural identity отсутствует в committed queue.
    NotFound { entry_id: PlaylistEntryId },
    /// Caller передал playable part вместо owning compound identity.
    CompoundPartTarget {
        /// Ошибочно переданный stable playable identity.
        part_item_id: PlaylistItemId,
        /// Явная structural identity, которую обязан повторно отправить caller.
        compound_entry_id: PlaylistEntryId,
    },
    /// Reservation удерживает structural mutation lock.
    InstallCommitLinearizing,
    /// Structural revision нельзя продвинуть.
    StructuralRevisionExhausted,
    /// Traversal revision нельзя продвинуть при удалении current.
    TraversalRevisionExhausted,
}

impl fmt::Debug for RemoveItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Removed {
                entry_id,
                traversal_current_effect,
                current_outcome,
            } => formatter
                .debug_struct("Removed")
                .field("entry_id", entry_id)
                .field("traversal_current_effect", traversal_current_effect)
                .field("current_outcome", current_outcome)
                .finish(),
            Self::NotFound { entry_id } => formatter
                .debug_struct("NotFound")
                .field("entry_id", entry_id)
                .finish(),
            Self::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => formatter
                .debug_struct("CompoundPartTarget")
                .field("part_item_id", part_item_id)
                .field("compound_entry_id", compound_entry_id)
                .finish(),
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::StructuralRevisionExhausted => formatter.write_str("StructuralRevisionExhausted"),
            Self::TraversalRevisionExhausted => formatter.write_str("TraversalRevisionExhausted"),
        }
    }
}

impl fmt::Display for RemoveItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Removed { entry_id, .. } => write!(formatter, "deleted {entry_id:?}"),
            Self::NotFound { entry_id } => write!(formatter, "{entry_id:?} not found"),
            Self::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => write!(
                formatter,
                "{part_item_id} is part of {compound_entry_id:?}; compound target is required"
            ),
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit temporarily blocks removal")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision exhausted")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision exhausted"),
        }
    }
}

/// Intent перемещения по stable IDs без публичного numeric index API.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MoveItemIntent {
    /// Поместить строку первой.
    ToFront,
    /// Поместить строку последней.
    ToBack,
    /// Поместить строку непосредственно перед anchor ID.
    Before(PlaylistEntryId),
    /// Поместить строку непосредственно после anchor ID.
    After(PlaylistEntryId),
}

impl MoveItemIntent {
    /// Возвращает named structural anchor только для relative intent.
    pub const fn anchor(self) -> Option<PlaylistEntryId> {
        match self {
            Self::ToFront | Self::ToBack => None,
            Self::Before(entry_id) | Self::After(entry_id) => Some(entry_id),
        }
    }
}

impl fmt::Debug for MoveItemIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToFront => formatter.write_str("MoveItemIntent::ToFront"),
            Self::ToBack => formatter.write_str("MoveItemIntent::ToBack"),
            Self::Before(entry_id) => formatter
                .debug_tuple("MoveItemIntent::Before")
                .field(entry_id)
                .finish(),
            Self::After(entry_id) => formatter
                .debug_tuple("MoveItemIntent::After")
                .field(entry_id)
                .finish(),
        }
    }
}

impl fmt::Display for MoveItemIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToFront => formatter.write_str("move in"),
            Self::ToBack => formatter.write_str("end up"),
            Self::Before(entry_id) => write!(formatter, "move ahead of {entry_id:?}"),
            Self::After(entry_id) => write!(formatter, "move after {entry_id:?}"),
        }
    }
}

/// Outcome move-by-ID mutation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MoveItemOutcome {
    /// Canonical order изменён.
    Moved { entry_id: PlaylistEntryId },
    /// Requested order уже достигнут.
    AlreadyInPlace { entry_id: PlaylistEntryId },
    /// Перемещаемая top-level entry отсутствует.
    EntryNotFound { entry_id: PlaylistEntryId },
    /// Caller передал playable part вместо owning compound identity.
    CompoundPartTarget {
        part_item_id: PlaylistItemId,
        compound_entry_id: PlaylistEntryId,
    },
    /// Anchor отсутствует.
    AnchorNotFound { anchor_entry_id: PlaylistEntryId },
    /// Anchor указывает на playable part вместо owning compound identity.
    CompoundPartAnchor {
        part_item_id: PlaylistItemId,
        compound_entry_id: PlaylistEntryId,
    },
    /// Reservation удерживает structural mutation lock.
    InstallCommitLinearizing,
    /// Structural revision нельзя продвинуть.
    StructuralRevisionExhausted,
}

impl fmt::Debug for MoveItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Moved { entry_id } => formatter
                .debug_struct("Moved")
                .field("entry_id", entry_id)
                .finish(),
            Self::AlreadyInPlace { entry_id } => formatter
                .debug_struct("AlreadyInPlace")
                .field("entry_id", entry_id)
                .finish(),
            Self::EntryNotFound { entry_id } => formatter
                .debug_struct("EntryNotFound")
                .field("entry_id", entry_id)
                .finish(),
            Self::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => formatter
                .debug_struct("CompoundPartTarget")
                .field("part_item_id", part_item_id)
                .field("compound_entry_id", compound_entry_id)
                .finish(),
            Self::AnchorNotFound { anchor_entry_id } => formatter
                .debug_struct("AnchorNotFound")
                .field("anchor_entry_id", anchor_entry_id)
                .finish(),
            Self::CompoundPartAnchor {
                part_item_id,
                compound_entry_id,
            } => formatter
                .debug_struct("CompoundPartAnchor")
                .field("part_item_id", part_item_id)
                .field("compound_entry_id", compound_entry_id)
                .finish(),
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::StructuralRevisionExhausted => formatter.write_str("StructuralRevisionExhausted"),
        }
    }
}

impl fmt::Display for MoveItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Moved { entry_id } => write!(formatter, "{entry_id:?} moved"),
            Self::AlreadyInPlace { entry_id } => {
                write!(formatter, "{entry_id:?} already in place")
            }
            Self::EntryNotFound { entry_id } => write!(formatter, "{entry_id:?} not found"),
            Self::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => write!(
                formatter,
                "{part_item_id} is part of {compound_entry_id:?}; compound target is required"
            ),
            Self::AnchorNotFound { anchor_entry_id } => {
                write!(formatter, "anchor {anchor_entry_id:?} not found")
            }
            Self::CompoundPartAnchor {
                part_item_id,
                compound_entry_id,
            } => {
                write!(
                    formatter,
                    "anchor {part_item_id} is part of {compound_entry_id:?}"
                )
            }
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit temporarily blocks movement")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision exhausted")
            }
        }
    }
}

/// Outcome clear mutation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClearQueueOutcome {
    /// Все committed строки удалены.
    Cleared {
        removed_item_count: usize,
        traversal_current_effect: TraversalCurrentEffect,
        current_outcome: RemovalCurrentOutcome,
    },
    /// Queue уже пуста и current уже отсутствует.
    AlreadyEmpty,
    /// Reservation удерживает structural mutation lock.
    InstallCommitLinearizing,
    /// Structural revision нельзя продвинуть.
    StructuralRevisionExhausted,
    /// Traversal revision нельзя продвинуть.
    TraversalRevisionExhausted,
}

impl fmt::Debug for ClearQueueOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cleared {
                removed_item_count,
                traversal_current_effect,
                current_outcome,
            } => formatter
                .debug_struct("Cleared")
                .field("removed_item_count", removed_item_count)
                .field("traversal_current_effect", traversal_current_effect)
                .field("current_outcome", current_outcome)
                .finish(),
            Self::AlreadyEmpty => formatter.write_str("AlreadyEmpty"),
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::StructuralRevisionExhausted => formatter.write_str("StructuralRevisionExhausted"),
            Self::TraversalRevisionExhausted => formatter.write_str("TraversalRevisionExhausted"),
        }
    }
}

impl fmt::Display for ClearQueueOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cleared {
                removed_item_count, ..
            } => write!(formatter, "queue cleared: {removed_item_count} deleted"),
            Self::AlreadyEmpty => formatter.write_str("queue is empty."),
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit temporarily blocks clearing")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision exhausted")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision exhausted"),
        }
    }
}

/// Ошибка validation optional traversal current boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraversalCurrentValidationError {
    /// ID не принадлежит текущей committed canonical queue.
    ItemNotCommitted { item_id: PlaylistItemId },
}

impl fmt::Debug for TraversalCurrentValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotCommitted { item_id } => formatter
                .debug_struct("ItemNotCommitted")
                .field("item_id", item_id)
                .finish(),
        }
    }
}

impl fmt::Display for TraversalCurrentValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotCommitted { item_id } => {
                write!(formatter, "{item_id} does not belong to committed queue")
            }
        }
    }
}

impl std::error::Error for TraversalCurrentValidationError {}

/// Успешный/no-op current mutation outcome.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraversalCurrentMutationOutcome {
    /// Current установлен на validated committed ID.
    Set(TraversalCurrentItemId),
    /// Current очищен.
    Cleared,
    /// Requested current уже установлен.
    AlreadyCurrent(TraversalCurrentItemId),
    /// Current уже отсутствует.
    AlreadyAbsent,
}

impl fmt::Debug for TraversalCurrentMutationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set(current) => formatter.debug_tuple("Set").field(current).finish(),
            Self::Cleared => formatter.write_str("Cleared"),
            Self::AlreadyCurrent(current) => formatter
                .debug_tuple("AlreadyCurrent")
                .field(current)
                .finish(),
            Self::AlreadyAbsent => formatter.write_str("AlreadyAbsent"),
        }
    }
}

impl fmt::Display for TraversalCurrentMutationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set(current) => write!(formatter, "current installed at {current}"),
            Self::Cleared => formatter.write_str("current"),
            Self::AlreadyCurrent(current) => write!(formatter, "{current} is now"),
            Self::AlreadyAbsent => formatter.write_str("current is not already available."),
        }
    }
}

/// Ошибка current mutation до изменения state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraversalCurrentMutationError {
    /// Reservation удерживает traversal mutation lock.
    InstallCommitLinearizing,
    /// Requested ID отсутствует в committed queue.
    ItemNotCommitted { item_id: PlaylistItemId },
    /// Traversal revision нельзя продвинуть.
    TraversalRevisionExhausted,
}

impl fmt::Debug for TraversalCurrentMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::ItemNotCommitted { item_id } => formatter
                .debug_struct("ItemNotCommitted")
                .field("item_id", item_id)
                .finish(),
            Self::TraversalRevisionExhausted => formatter.write_str("TraversalRevisionExhausted"),
        }
    }
}

impl fmt::Display for TraversalCurrentMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit temporarily blocks changing the current item")
            }
            Self::ItemNotCommitted { item_id } => {
                write!(formatter, "{item_id} does not belong to committed queue")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision exhausted"),
        }
    }
}

impl std::error::Error for TraversalCurrentMutationError {}

/// Ошибка полного reservation preflight до player authorization barrier.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PrepareReservedMutationError {
    /// Queue уже хранит один active reservation lock.
    InstallCommitLinearizing,
    /// Structural/traversal preconditions устарели.
    RevisionMismatch {
        expected: QueueRevisionSnapshot,
        actual: QueueRevisionSnapshot,
    },
    /// Existing target отсутствует в committed queue.
    ItemNotCommitted { item_id: PlaylistItemId },
    /// Replacement candidate превышает hard capacity.
    CapacityExceeded { requested: usize, maximum: usize },
    /// Checked allocator не может выдать весь candidate range.
    ItemIdExhausted,
    /// Allocator обнаружил collision до lock installation.
    ItemIdCollision { item_id: PlaylistItemId },
    /// Structural revision нельзя заранее подготовить.
    StructuralRevisionExhausted,
    /// Traversal revision нельзя заранее подготовить.
    TraversalRevisionExhausted,
}

impl fmt::Debug for PrepareReservedMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::RevisionMismatch { expected, actual } => formatter
                .debug_struct("RevisionMismatch")
                .field("expected", expected)
                .field("actual", actual)
                .finish(),
            Self::ItemNotCommitted { item_id } => formatter
                .debug_struct("ItemNotCommitted")
                .field("item_id", item_id)
                .finish(),
            Self::CapacityExceeded { requested, maximum } => formatter
                .debug_struct("CapacityExceeded")
                .field("requested", requested)
                .field("maximum", maximum)
                .finish(),
            Self::ItemIdExhausted => formatter.write_str("ItemIdExhausted"),
            Self::ItemIdCollision { item_id } => formatter
                .debug_struct("ItemIdCollision")
                .field("item_id", item_id)
                .finish(),
            Self::StructuralRevisionExhausted => formatter.write_str("StructuralRevisionExhausted"),
            Self::TraversalRevisionExhausted => formatter.write_str("TraversalRevisionExhausted"),
        }
    }
}

impl fmt::Display for PrepareReservedMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => {
                formatter.write_str("The other install commit is already linearized.")
            }
            Self::RevisionMismatch { .. } => {
                formatter.write_str("queue precondition revisions changed")
            }
            Self::ItemNotCommitted { item_id } => {
                write!(
                    formatter,
                    "target {item_id} is missing from the committed queue"
                )
            }
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "candidate exceeds the queue limit of {maximum}")
            }
            Self::ItemIdExhausted => {
                formatter.write_str("The range of PlaylistItemId is exhausted")
            }
            Self::ItemIdCollision { .. } => {
                formatter.write_str("allocator proposed an existing PlaylistItemId")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision exhausted")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision exhausted"),
        }
    }
}

impl std::error::Error for PrepareReservedMutationError {}

/// Результат infallible business commit exact reservation token.
#[derive(Clone, PartialEq, Eq)]
pub struct ReservedMutationCommit {
    pub(super) allocated_item_ids: AllocatedPlaylistItemIds,
    pub(super) traversal_current: TraversalCurrentItemId,
}

impl ReservedMutationCommit {
    /// Возвращает IDs, которые впервые стали публичными в этом commit point.
    pub fn allocated_item_ids(&self) -> &AllocatedPlaylistItemIds {
        &self.allocated_item_ids
    }

    /// Возвращает committed traversal current нового/существующего target.
    pub const fn traversal_current(&self) -> TraversalCurrentItemId {
        self.traversal_current
    }
}

impl fmt::Debug for ReservedMutationCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReservedMutationCommit")
            .field("allocated_item_ids", &self.allocated_item_ids)
            .field("traversal_current", &self.traversal_current)
            .finish()
    }
}

impl fmt::Display for ReservedMutationCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "reserved mutation committed; {}; current {}",
            self.allocated_item_ids, self.traversal_current
        )
    }
}
