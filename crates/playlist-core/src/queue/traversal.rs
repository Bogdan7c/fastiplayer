//! Derived playable traversal поверх first-class top-level queue entries.

use std::collections::HashSet;

use crate::{PlaylistEntry, PlaylistEntryId, PlaylistItemId};

use super::PlaylistQueue;

impl PlaylistQueue {
    /// Возвращает первую playable identity top-level entry.
    ///
    /// Метод не раскрывает storage layout shuffle/navigation owner-ам и сохраняет
    /// source order subordinate parts.
    pub(super) fn first_playable_item_id(
        &self,
        entry_id: PlaylistEntryId,
    ) -> Option<PlaylistItemId> {
        let entry = self.top_level_entry(entry_id)?;
        match entry {
            PlaylistEntry::Single(item) => Some(item.item_id()),
            PlaylistEntry::Compound(group) => {
                group.parts().next().map(|part| part.item().item_id())
            }
        }
    }

    /// Возвращает следующую part только внутри того же top-level entry.
    ///
    /// `None` означает либо последнюю part, либо stale/foreign Item ID. Переход к
    /// следующему top-level entry остаётся ответственностью navigation/shuffle.
    pub(super) fn next_playable_item_id_in_entry(
        &self,
        item_id: PlaylistItemId,
    ) -> Option<PlaylistItemId> {
        let owner_entry_id = self.structural_entry_id_for_item(item_id)?;
        let owner_entry = self.top_level_entry(owner_entry_id)?;
        let mut owner_items = match owner_entry {
            PlaylistEntry::Single(_) => return None,
            PlaylistEntry::Compound(group) => group.parts(),
        };
        owner_items
            .find(|part| part.item().item_id() == item_id)
            .and_then(|_| owner_items.next())
            .map(|part| part.item().item_id())
    }

    /// Выводит top-level membership fixed automatic chain из retained Item IDs.
    ///
    /// Поздно добавленные entries не получают traversal authority, а surviving
    /// compound остаётся одним block только с полным набором parts исходного
    /// chain. Частичный compound здесь невозможен через public structural API
    /// и проверяется debug-инвариантом.
    pub(super) fn retained_entry_ids_for_items(
        &self,
        retained_item_ids: &HashSet<PlaylistItemId>,
    ) -> HashSet<PlaylistEntryId> {
        self.iter_top_level_entries()
            .filter_map(|entry| {
                let retained_part_count = match entry {
                    PlaylistEntry::Single(item) => {
                        usize::from(retained_item_ids.contains(&item.item_id()))
                    }
                    PlaylistEntry::Compound(group) => group
                        .parts()
                        .filter(|part| retained_item_ids.contains(&part.item().item_id()))
                        .count(),
                };
                debug_assert!(
                    retained_part_count == 0 || retained_part_count == entry.retained_item_count(),
                    "automatic fixed membership cannot retain only part of a compound entry"
                );
                (retained_part_count > 0).then(|| entry.entry_id())
            })
            .collect()
    }
}
