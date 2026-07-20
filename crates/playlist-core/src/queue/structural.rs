//! Internal top-level structural identity resolution shared by mutation owners.

use crate::{PlaylistEntry, PlaylistEntryId, PlaylistItemId};

use super::PlaylistQueue;

/// Typed lookup keeps a forbidden part target distinct from ordinary absence.
pub(super) enum StructuralEntryLookupError {
    /// В committed top-level storage нет requested identity.
    NotFound,
    /// Requested single identity принадлежит compound и требует group target.
    CompoundPart {
        /// Playable identity, которую нельзя мутировать отдельно.
        part_item_id: PlaylistItemId,
        /// Structural owner, который caller обязан выбрать явно.
        compound_entry_id: PlaylistEntryId,
    },
}

impl PlaylistQueue {
    /// Разрешает structural identity только на границе top-level storage.
    pub(super) fn resolve_top_level_entry_index(
        &self,
        entry_id: PlaylistEntryId,
    ) -> Result<usize, StructuralEntryLookupError> {
        if let Some(entry_index) = self
            .entries
            .iter()
            .position(|entry| entry.entry_id() == entry_id)
        {
            return Ok(entry_index);
        }
        if let PlaylistEntryId::Single(part_item_id) = entry_id
            && let Some(compound_entry_id) = self
                .entries
                .iter()
                .filter_map(PlaylistEntry::as_compound)
                .find(|group| {
                    group
                        .parts()
                        .any(|part| part.item().item_id() == part_item_id)
                })
                .map(|group| PlaylistEntryId::Compound(group.group_id()))
        {
            return Err(StructuralEntryLookupError::CompoundPart {
                part_item_id,
                compound_entry_id,
            });
        }
        Err(StructuralEntryLookupError::NotFound)
    }
}
