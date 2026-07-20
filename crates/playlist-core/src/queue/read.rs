//! Intent-based read boundary canonical очереди.

use std::iter::{FlatMap, Map};
use std::slice;
use std::sync::Arc;

use crate::{PlaylistCompoundPart, PlaylistEntry, PlaylistEntryId, PlaylistItem, PlaylistItemId};

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
        PlayableItemsIter::new(&self.entries, self.retained_item_count())
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
        self.entries.len()
    }

    /// Возвращает число retained stable playable Item IDs.
    ///
    /// Capacity и persistence accounting используют именно этот intent.
    #[must_use]
    pub fn retained_item_count(&self) -> usize {
        self.entries
            .iter()
            .map(PlaylistEntry::retained_item_count)
            .sum()
    }

    /// Итерирует canonical top-level entries отдельно от derived playable order.
    pub fn iter_top_level_entries(
        &self,
    ) -> impl ExactSizeIterator<Item = &PlaylistEntry> + DoubleEndedIterator + '_ {
        self.entries.iter()
    }

    /// Итерирует structural identities в canonical top-level порядке.
    pub fn iter_top_level_entry_ids(
        &self,
    ) -> impl ExactSizeIterator<Item = PlaylistEntryId> + DoubleEndedIterator + '_ {
        self.iter_top_level_entries().map(PlaylistEntry::entry_id)
    }

    /// Выполняет read-only lookup top-level entry по structural identity.
    pub fn top_level_entry(&self, entry_id: PlaylistEntryId) -> Option<&PlaylistEntry> {
        self.entries
            .iter()
            .find(|entry| entry.entry_id() == entry_id)
    }

    /// Снимает immutable Arc-sharing playable snapshot для ownership handoff.
    ///
    /// Обычный синхронный read должен использовать borrowed iterators или lookup.
    #[must_use]
    pub fn owned_playable_items_snapshot(&self) -> OwnedPlayableItemsSnapshot {
        OwnedPlayableItemsSnapshot {
            items: Arc::from(self.iter_playable_items().cloned().collect::<Vec<_>>()),
        }
    }
}

/// Iterator playable items внутри одного top-level entry.
enum EntryPlayableItemsIter<'a> {
    /// Ровно один самостоятельный item.
    Single(std::option::IntoIter<&'a PlaylistItem>),
    /// Ordered parts одной compound group.
    Compound(
        Map<slice::Iter<'a, PlaylistCompoundPart>, fn(&PlaylistCompoundPart) -> &PlaylistItem>,
    ),
}

impl<'a> Iterator for EntryPlayableItemsIter<'a> {
    type Item = &'a PlaylistItem;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(item) => item.next(),
            Self::Compound(parts) => parts.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = match self {
            Self::Single(item) => item.len(),
            Self::Compound(parts) => parts.len(),
        };
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for EntryPlayableItemsIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(item) => item.next_back(),
            Self::Compound(parts) => parts.next_back(),
        }
    }
}

impl ExactSizeIterator for EntryPlayableItemsIter<'_> {}

/// Concrete borrowed flattening iterator без materialized parallel storage.
struct PlayableItemsIter<'a> {
    inner: FlattenedEntryItems<'a>,
    remaining: usize,
}

/// Читаемое имя concrete flat-map типа скрывает только std adapter plumbing.
type FlattenedEntryItems<'a> = FlatMap<
    slice::Iter<'a, PlaylistEntry>,
    EntryPlayableItemsIter<'a>,
    fn(&'a PlaylistEntry) -> EntryPlayableItemsIter<'a>,
>;

impl<'a> PlayableItemsIter<'a> {
    /// Создаёт iterator с уже доказанным exact retained count.
    fn new(entries: &'a [PlaylistEntry], retained_item_count: usize) -> Self {
        let map_entry: fn(&'a PlaylistEntry) -> EntryPlayableItemsIter<'a> = entry_playable_items;
        Self {
            inner: entries.iter().flat_map(map_entry),
            remaining: retained_item_count,
        }
    }
}

impl<'a> Iterator for PlayableItemsIter<'a> {
    type Item = &'a PlaylistItem;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next();
        if item.is_some() {
            self.remaining -= 1;
        }
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for PlayableItemsIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let item = self.inner.next_back();
        if item.is_some() {
            self.remaining -= 1;
        }
        item
    }
}

impl ExactSizeIterator for PlayableItemsIter<'_> {}

/// Преобразует один entry в borrowed playable projection.
fn entry_playable_items(entry: &PlaylistEntry) -> EntryPlayableItemsIter<'_> {
    match entry {
        PlaylistEntry::Single(item) => EntryPlayableItemsIter::Single(Some(item).into_iter()),
        PlaylistEntry::Compound(group) => {
            let part_item: fn(&PlaylistCompoundPart) -> &PlaylistItem = PlaylistCompoundPart::item;
            EntryPlayableItemsIter::Compound(group.parts_slice().iter().map(part_item))
        }
    }
}
