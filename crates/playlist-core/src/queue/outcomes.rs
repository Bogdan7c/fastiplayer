//! Typed queue outcomes/errors без secret-bearing payload formatting.

use std::fmt;

use crate::{AllocatorRestoreError, PlaylistItemId};

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
            Self::NoItemsProvided => formatter.write_str("append batch пуст; очередь не изменена"),
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
                formatter.write_str("install commit временно блокирует изменение очереди")
            }
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "append превысил лимит очереди {maximum}")
            }
            Self::ItemIdExhausted => formatter.write_str("диапазон PlaylistItemId исчерпан"),
            Self::ItemIdCollision { .. } => {
                formatter.write_str("allocator предложил уже существующий PlaylistItemId")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
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
            Self::Preserved => formatter.write_str("traversal current сохранён"),
            Self::Cleared => formatter.write_str("traversal current очищен"),
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
            } => write!(formatter, "очередь заменена: {allocated_item_ids}"),
            Self::Cleared {
                removed_item_count, ..
            } => write!(formatter, "очередь очищена: удалено {removed_item_count}"),
            Self::AlreadyEmpty => formatter.write_str("очередь уже пуста"),
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
                formatter.write_str("install commit временно блокирует replacement")
            }
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "replacement превысил лимит очереди {maximum}")
            }
            Self::ItemIdExhausted => formatter.write_str("диапазон PlaylistItemId исчерпан"),
            Self::ItemIdCollision { .. } => {
                formatter.write_str("allocator предложил уже существующий PlaylistItemId")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision исчерпана"),
        }
    }
}

impl std::error::Error for ReplaceQueueError {}

/// Outcome identity-based remove.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RemoveItemOutcome {
    /// Exact committed row удалена.
    Removed {
        /// Удалённая stable identity.
        item_id: PlaylistItemId,
        /// Влияние удаления на traversal current.
        traversal_current_effect: TraversalCurrentEffect,
        /// D71 persisted-current outcome без неявного successor-а.
        current_outcome: RemovalCurrentOutcome,
    },
    /// Item ID отсутствует в committed queue.
    NotFound { item_id: PlaylistItemId },
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
                item_id,
                traversal_current_effect,
                current_outcome,
            } => formatter
                .debug_struct("Removed")
                .field("item_id", item_id)
                .field("traversal_current_effect", traversal_current_effect)
                .field("current_outcome", current_outcome)
                .finish(),
            Self::NotFound { item_id } => formatter
                .debug_struct("NotFound")
                .field("item_id", item_id)
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
            Self::Removed { item_id, .. } => write!(formatter, "удалён {item_id}"),
            Self::NotFound { item_id } => write!(formatter, "{item_id} не найден"),
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit временно блокирует remove")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision исчерпана"),
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
    Before(PlaylistItemId),
    /// Поместить строку непосредственно после anchor ID.
    After(PlaylistItemId),
}

impl fmt::Debug for MoveItemIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToFront => formatter.write_str("MoveItemIntent::ToFront"),
            Self::ToBack => formatter.write_str("MoveItemIntent::ToBack"),
            Self::Before(item_id) => formatter
                .debug_tuple("MoveItemIntent::Before")
                .field(item_id)
                .finish(),
            Self::After(item_id) => formatter
                .debug_tuple("MoveItemIntent::After")
                .field(item_id)
                .finish(),
        }
    }
}

impl fmt::Display for MoveItemIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToFront => formatter.write_str("переместить в начало"),
            Self::ToBack => formatter.write_str("переместить в конец"),
            Self::Before(item_id) => write!(formatter, "переместить перед {item_id}"),
            Self::After(item_id) => write!(formatter, "переместить после {item_id}"),
        }
    }
}

/// Outcome move-by-ID mutation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MoveItemOutcome {
    /// Canonical order изменён.
    Moved { item_id: PlaylistItemId },
    /// Requested order уже достигнут.
    AlreadyInPlace { item_id: PlaylistItemId },
    /// Перемещаемая строка отсутствует.
    ItemNotFound { item_id: PlaylistItemId },
    /// Anchor отсутствует.
    AnchorNotFound { anchor_item_id: PlaylistItemId },
    /// Reservation удерживает structural mutation lock.
    InstallCommitLinearizing,
    /// Structural revision нельзя продвинуть.
    StructuralRevisionExhausted,
}

impl fmt::Debug for MoveItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Moved { item_id } => formatter
                .debug_struct("Moved")
                .field("item_id", item_id)
                .finish(),
            Self::AlreadyInPlace { item_id } => formatter
                .debug_struct("AlreadyInPlace")
                .field("item_id", item_id)
                .finish(),
            Self::ItemNotFound { item_id } => formatter
                .debug_struct("ItemNotFound")
                .field("item_id", item_id)
                .finish(),
            Self::AnchorNotFound { anchor_item_id } => formatter
                .debug_struct("AnchorNotFound")
                .field("anchor_item_id", anchor_item_id)
                .finish(),
            Self::InstallCommitLinearizing => formatter.write_str("InstallCommitLinearizing"),
            Self::StructuralRevisionExhausted => formatter.write_str("StructuralRevisionExhausted"),
        }
    }
}

impl fmt::Display for MoveItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Moved { item_id } => write!(formatter, "{item_id} перемещён"),
            Self::AlreadyInPlace { item_id } => write!(formatter, "{item_id} уже на месте"),
            Self::ItemNotFound { item_id } => write!(formatter, "{item_id} не найден"),
            Self::AnchorNotFound { anchor_item_id } => {
                write!(formatter, "anchor {anchor_item_id} не найден")
            }
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit временно блокирует move")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
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
            } => write!(formatter, "очередь очищена: удалено {removed_item_count}"),
            Self::AlreadyEmpty => formatter.write_str("очередь уже пуста"),
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit временно блокирует clear")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision исчерпана"),
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
                write!(formatter, "{item_id} не принадлежит committed queue")
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
            Self::Set(current) => write!(formatter, "current установлен на {current}"),
            Self::Cleared => formatter.write_str("current очищен"),
            Self::AlreadyCurrent(current) => write!(formatter, "{current} уже current"),
            Self::AlreadyAbsent => formatter.write_str("current уже отсутствует"),
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
                formatter.write_str("install commit временно блокирует смену current")
            }
            Self::ItemNotCommitted { item_id } => {
                write!(formatter, "{item_id} не принадлежит committed queue")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision исчерпана"),
        }
    }
}

impl std::error::Error for TraversalCurrentMutationError {}

/// Ошибка restore полного queue snapshot.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QueueRestoreError {
    /// Snapshot превышает hard capacity и не может быть усечён молча.
    CapacityExceeded { restored: usize, maximum: usize },
    /// Две restored строки имеют один stable Item ID.
    DuplicateItemId { item_id: PlaylistItemId },
    /// Allocator watermark не продолжает restored lineage.
    InvalidAllocator(AllocatorRestoreError),
    /// Persisted current не ссылается на committed row.
    CurrentItemNotCommitted { item_id: PlaylistItemId },
}

impl fmt::Debug for QueueRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded { restored, maximum } => formatter
                .debug_struct("CapacityExceeded")
                .field("restored", restored)
                .field("maximum", maximum)
                .finish(),
            Self::DuplicateItemId { item_id } => formatter
                .debug_struct("DuplicateItemId")
                .field("item_id", item_id)
                .finish(),
            Self::InvalidAllocator(error) => formatter
                .debug_tuple("InvalidAllocator")
                .field(error)
                .finish(),
            Self::CurrentItemNotCommitted { item_id } => formatter
                .debug_struct("CurrentItemNotCommitted")
                .field("item_id", item_id)
                .finish(),
        }
    }
}

impl fmt::Display for QueueRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "restored queue превышает лимит {maximum}")
            }
            Self::DuplicateItemId { item_id } => {
                write!(formatter, "restored queue повторяет {item_id}")
            }
            Self::InvalidAllocator(error) => fmt::Display::fmt(error, formatter),
            Self::CurrentItemNotCommitted { item_id } => {
                write!(formatter, "restored current {item_id} отсутствует в queue")
            }
        }
    }
}

impl std::error::Error for QueueRestoreError {}

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
                formatter.write_str("другой install commit уже линеаризуется")
            }
            Self::RevisionMismatch { .. } => {
                formatter.write_str("queue precondition revisions изменились")
            }
            Self::ItemNotCommitted { item_id } => {
                write!(formatter, "target {item_id} отсутствует в committed queue")
            }
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "candidate превышает лимит очереди {maximum}")
            }
            Self::ItemIdExhausted => formatter.write_str("диапазон PlaylistItemId исчерпан"),
            Self::ItemIdCollision { .. } => {
                formatter.write_str("allocator предложил уже существующий PlaylistItemId")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision исчерпана"),
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
