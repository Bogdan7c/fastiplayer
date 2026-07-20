//! Intent-based read boundary canonical очереди.

use std::sync::Arc;

use crate::{PlaylistItem, PlaylistItemId};

use super::PlaylistQueue;

#[cfg(test)]
mod tests;

/// Immutable owned projection playable rows для async/persistence handoff.
///
/// Snapshot намеренно не раскрывает slice/indexing или mutation API: порядок можно
/// прочитать, но использовать копию как authority для structural commit нельзя.
#[derive(Clone, Debug)]
pub struct OwnedPlayableItemsSnapshot {
    items: Arc<[PlaylistItem]>,
}

impl OwnedPlayableItemsSnapshot {
    /// Итерирует playable rows в observable canonical order.
    pub fn iter_playable_items(
        &self,
    ) -> impl ExactSizeIterator<Item = &PlaylistItem> + DoubleEndedIterator + '_ {
        self.items.iter()
    }

    /// Итерирует stable playable Item IDs в том же observable order.
    pub fn iter_playable_ids(
        &self,
    ) -> impl ExactSizeIterator<Item = PlaylistItemId> + DoubleEndedIterator + '_ {
        self.iter_playable_items().map(PlaylistItem::item_id)
    }

    /// Возвращает число сохранённых playable Item IDs внутри snapshot.
    #[must_use]
    pub fn retained_item_count(&self) -> usize {
        self.items.len()
    }

    /// Выполняет read-only lookup snapshot row по stable Item ID.
    #[must_use]
    pub fn item(&self, item_id: PlaylistItemId) -> Option<&PlaylistItem> {
        self.iter_playable_items()
            .find(|item| item.item_id() == item_id)
    }
}

impl PlaylistQueue {
    /// Итерирует playable rows без обещания contiguous queue storage.
    pub fn iter_playable_items(
        &self,
    ) -> impl ExactSizeIterator<Item = &PlaylistItem> + DoubleEndedIterator + '_ {
        self.items.iter()
    }

    /// Итерирует stable playable Item IDs без materialization промежуточного списка.
    pub fn iter_playable_ids(
        &self,
    ) -> impl ExactSizeIterator<Item = PlaylistItemId> + DoubleEndedIterator + '_ {
        self.iter_playable_items().map(PlaylistItem::item_id)
    }

    /// Возвращает число first-class top-level entries.
    ///
    /// До появления compound storage один entry соответствует одному playable item,
    /// но caller уже обязан выбрать именно structural entry semantics.
    #[must_use]
    pub const fn top_level_entry_count(&self) -> usize {
        self.items.len()
    }

    /// Возвращает число retained stable playable Item IDs.
    ///
    /// Capacity и persistence accounting используют именно этот intent.
    #[must_use]
    pub const fn retained_item_count(&self) -> usize {
        self.items.len()
    }

    /// Снимает immutable Arc-sharing playable snapshot для ownership handoff.
    ///
    /// Обычный синхронный read должен использовать borrowed iterators или lookup.
    #[must_use]
    pub fn owned_playable_items_snapshot(&self) -> OwnedPlayableItemsSnapshot {
        OwnedPlayableItemsSnapshot {
            items: Arc::from(self.items.clone()),
        }
    }
}
