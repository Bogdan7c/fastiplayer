//! Stable identity и монотонный allocator элементов очереди.

use std::collections::HashSet;
use std::fmt;
use std::num::NonZeroU64;

/// Непрозрачная стабильная идентичность одного вхождения в очередь.
///
/// Одинаковые locator-ы намеренно могут иметь разные Item ID. Нулевое значение
/// зарезервировано и никогда не является валидной identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaylistItemId(NonZeroU64);

impl PlaylistItemId {
    /// Восстанавливает Item ID из persistence DTO без обхода non-zero invariant.
    pub fn from_persistence_value(value: u64) -> Result<Self, PlaylistItemIdPersistenceError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(PlaylistItemIdPersistenceError::ReservedZero)
    }

    /// Возвращает fixed-width значение только для persistence/correlation boundary.
    pub const fn expose_value_for_persistence(self) -> u64 {
        self.0.get()
    }

    /// Создаёт ID из уже проверенного allocator-ом non-zero значения.
    pub(crate) const fn from_non_zero(value: NonZeroU64) -> Self {
        Self(value)
    }
}

impl fmt::Debug for PlaylistItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PlaylistItemId")
            .field(&self.0.get())
            .finish()
    }
}

impl fmt::Display for PlaylistItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "playlist-item-{}", self.0.get())
    }
}

/// Ошибка преобразования persistence-значения в непрозрачный Item ID.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlaylistItemIdPersistenceError {
    /// Ноль зарезервирован как отсутствие identity.
    ReservedZero,
}

impl fmt::Debug for PlaylistItemIdPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaylistItemIdPersistenceError::ReservedZero")
    }
}

impl fmt::Display for PlaylistItemIdPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("нулевой PlaylistItemId зарезервирован")
    }
}

impl std::error::Error for PlaylistItemIdPersistenceError {}

/// Persistence-facing snapshot следующего ещё не выданного Item ID.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NextPlaylistItemId(NonZeroU64);

impl NextPlaylistItemId {
    /// Валидирует сохранённый non-zero high-watermark.
    pub fn from_persistence_value(value: u64) -> Result<Self, AllocatorRestoreError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(AllocatorRestoreError::ReservedZeroWatermark)
    }

    /// Возвращает snapshot-значение только для persistence DTO.
    pub const fn expose_value_for_persistence(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Debug for NextPlaylistItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("NextPlaylistItemId")
            .field(&self.0.get())
            .finish()
    }
}

impl fmt::Display for NextPlaylistItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "next-playlist-item-{}", self.0.get())
    }
}

/// Владелец монотонного high-watermark Item ID.
///
/// Allocator не предоставляет отдельную публичную операцию reserve: IDs могут
/// выйти наружу только вместе с успешным queue commit.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistItemIdAllocator {
    next_item_id: NonZeroU64,
}

impl PlaylistItemIdAllocator {
    /// Создаёт новую lineage, в которой первый выданный ID будет равен 1.
    pub const fn initial() -> Self {
        Self {
            next_item_id: NonZeroU64::MIN,
        }
    }

    /// Восстанавливает allocator только из watermark, строго большего всех IDs.
    pub fn restore(
        next_item_id: NextPlaylistItemId,
        restored_item_ids: &[PlaylistItemId],
    ) -> Result<Self, AllocatorRestoreError> {
        if let Some(maximum_item_id) = restored_item_ids.iter().copied().max()
            && next_item_id.0 <= maximum_item_id.0
        {
            return Err(AllocatorRestoreError::WatermarkNotAboveRestoredIds {
                next_item_id,
                maximum_item_id,
            });
        }

        Ok(Self {
            next_item_id: next_item_id.0,
        })
    }

    /// Снимает persistence snapshot, не раскрывая mutable counter.
    pub const fn snapshot(&self) -> NextPlaylistItemId {
        NextPlaylistItemId(self.next_item_id)
    }

    /// Полностью проверяет будущий диапазон без изменения high-watermark.
    pub(crate) fn preflight_allocation(
        &self,
        item_count: usize,
        existing_item_ids: &HashSet<PlaylistItemId>,
    ) -> Result<ItemIdAllocationPlan, ItemIdAllocationError> {
        if item_count == 0 {
            return Ok(ItemIdAllocationPlan {
                allocated_item_ids: Vec::new(),
                next_item_id_after_commit: self.next_item_id,
            });
        }

        let item_count_u64 =
            u64::try_from(item_count).map_err(|_| ItemIdAllocationError::ArithmeticExhausted)?;
        let next_after_range = self
            .next_item_id
            .get()
            .checked_add(item_count_u64)
            .and_then(NonZeroU64::new)
            .ok_or(ItemIdAllocationError::ArithmeticExhausted)?;
        let mut allocated_item_ids = Vec::with_capacity(item_count);

        for offset in 0..item_count_u64 {
            let raw_item_id = self
                .next_item_id
                .get()
                .checked_add(offset)
                .and_then(NonZeroU64::new)
                .ok_or(ItemIdAllocationError::ArithmeticExhausted)?;
            let item_id = PlaylistItemId::from_non_zero(raw_item_id);

            if existing_item_ids.contains(&item_id) {
                return Err(ItemIdAllocationError::Collision { item_id });
            }

            allocated_item_ids.push(item_id);
        }

        Ok(ItemIdAllocationPlan {
            allocated_item_ids,
            next_item_id_after_commit: next_after_range,
        })
    }

    /// Продвигает high-watermark уже проверенного плана в commit point.
    pub(crate) fn commit_allocation(&mut self, plan: &ItemIdAllocationPlan) {
        self.next_item_id = plan.next_item_id_after_commit;
    }
}

impl Default for PlaylistItemIdAllocator {
    fn default() -> Self {
        Self::initial()
    }
}

impl fmt::Debug for PlaylistItemIdAllocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistItemIdAllocator")
            .field("next_item_id", &self.snapshot())
            .finish()
    }
}

/// Ошибка восстановления allocator high-watermark.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AllocatorRestoreError {
    /// Нулевой watermark нарушает non-zero identity invariant.
    ReservedZeroWatermark,
    /// Watermark повторно выдал бы существующий или более ранний ID.
    WatermarkNotAboveRestoredIds {
        /// Невалидный следующий ID из persistence state.
        next_item_id: NextPlaylistItemId,
        /// Наибольший ID среди восстановленных строк.
        maximum_item_id: PlaylistItemId,
    },
}

impl fmt::Debug for AllocatorRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedZeroWatermark => {
                formatter.write_str("AllocatorRestoreError::ReservedZeroWatermark")
            }
            Self::WatermarkNotAboveRestoredIds {
                next_item_id,
                maximum_item_id,
            } => formatter
                .debug_struct("AllocatorRestoreError::WatermarkNotAboveRestoredIds")
                .field("next_item_id", next_item_id)
                .field("maximum_item_id", maximum_item_id)
                .finish(),
        }
    }
}

impl fmt::Display for AllocatorRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedZeroWatermark => {
                formatter.write_str("next_item_id не может быть равен нулю")
            }
            Self::WatermarkNotAboveRestoredIds { .. } => formatter
                .write_str("next_item_id должен быть строго больше всех восстановленных Item ID"),
        }
    }
}

impl std::error::Error for AllocatorRestoreError {}

/// Полностью проверенный, но ещё не опубликованный диапазон IDs.
pub(crate) struct ItemIdAllocationPlan {
    pub(crate) allocated_item_ids: Vec<PlaylistItemId>,
    next_item_id_after_commit: NonZeroU64,
}

/// Внутренняя причина отказа allocation preflight.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemIdAllocationError {
    /// Fixed-width checked arithmetic не может представить весь диапазон.
    ArithmeticExhausted,
    /// Будущий ID уже присутствует в canonical queue.
    Collision { item_id: PlaylistItemId },
}

impl fmt::Debug for ItemIdAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArithmeticExhausted => formatter.write_str("ArithmeticExhausted"),
            Self::Collision { item_id } => formatter
                .debug_struct("Collision")
                .field("item_id", item_id)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_allocator_preflights_first_id_without_publishing_it() {
        // Preflight не меняет snapshot, пока queue owner не вызвал commit.
        let mut allocator = PlaylistItemIdAllocator::initial();
        let empty_ids = HashSet::new();
        let plan = allocator
            .preflight_allocation(1, &empty_ids)
            .expect("first ID range");

        assert_eq!(
            plan.allocated_item_ids,
            vec![PlaylistItemId::from_persistence_value(1).expect("non-zero")]
        );
        assert_eq!(allocator.snapshot().expose_value_for_persistence(), 1);

        allocator.commit_allocation(&plan);
        assert_eq!(allocator.snapshot().expose_value_for_persistence(), 2);
    }

    #[test]
    fn collision_rejects_entire_plan_without_advancing_allocator() {
        // Inconsistent existing set моделирует defensive collision detection.
        let allocator = PlaylistItemIdAllocator::initial();
        let colliding_id = PlaylistItemId::from_persistence_value(1).expect("non-zero");
        let existing_ids = HashSet::from([colliding_id]);

        assert!(matches!(
            allocator.preflight_allocation(2, &existing_ids),
            Err(ItemIdAllocationError::Collision { item_id }) if item_id == colliding_id
        ));
        assert_eq!(allocator.snapshot().expose_value_for_persistence(), 1);
    }
}
