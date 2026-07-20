//! Atomic append/replace boundary для first-class top-level entries.

use std::collections::HashSet;
use std::fmt;

use crate::entry::{
    CompoundGroupIdAllocationError, CompoundGroupIdAllocationPlan, PlaylistCompoundGroupId,
};
use crate::id::{ItemIdAllocationError, ItemIdAllocationPlan};
use crate::{
    PlaylistCompoundGroup, PlaylistEntry, PlaylistEntryDraft, PlaylistEntryId, PlaylistItemId,
};

use super::{MAX_PLAYLIST_ITEMS, PlaylistQueue, TraversalCurrentEffect, shuffle};

#[cfg(test)]
mod tests;

/// IDs, опубликованные одним successful top-level entry commit.
#[derive(Clone, PartialEq, Eq)]
pub struct AllocatedPlaylistEntries {
    entry_ids: Vec<PlaylistEntryId>,
    playable_item_ids: Vec<PlaylistItemId>,
}

impl AllocatedPlaylistEntries {
    /// Итерирует structural identities в committed canonical порядке.
    pub fn iter_entry_ids(
        &self,
    ) -> impl ExactSizeIterator<Item = PlaylistEntryId> + DoubleEndedIterator + '_ {
        self.entry_ids.iter().copied()
    }

    /// Итерирует все subordinate playable Item IDs в derived source order.
    pub fn iter_playable_item_ids(
        &self,
    ) -> impl ExactSizeIterator<Item = PlaylistItemId> + DoubleEndedIterator + '_ {
        self.playable_item_ids.iter().copied()
    }

    /// Возвращает число committed top-level entries.
    pub const fn top_level_entry_count(&self) -> usize {
        self.entry_ids.len()
    }

    /// Возвращает число committed retained Item IDs.
    pub const fn retained_item_count(&self) -> usize {
        self.playable_item_ids.len()
    }

    /// Передаёт владение flat playable receipt старому single-only фасаду.
    pub(super) fn into_playable_item_ids(self) -> Vec<PlaylistItemId> {
        self.playable_item_ids
    }
}

impl fmt::Debug for AllocatedPlaylistEntries {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AllocatedPlaylistEntries")
            .field("entry_ids", &self.entry_ids)
            .field("playable_item_ids", &self.playable_item_ids)
            .finish()
    }
}

/// Результат atomic append top-level entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddPlaylistEntriesOutcome {
    /// Пустой batch не меняет queue и allocator-ы.
    NoEntriesProvided,
    /// Весь batch committed одним structural revision.
    Added(AllocatedPlaylistEntries),
}

/// Результат group-safe capped append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CappedPlaylistEntriesAppendOutcome {
    allocated_entries: AllocatedPlaylistEntries,
    capacity_rejected_entries: usize,
    capacity_rejected_items: usize,
}

impl CappedPlaylistEntriesAppendOutcome {
    /// Разделяет committed receipt и точные capacity rejection counts.
    pub fn into_parts(self) -> (AllocatedPlaylistEntries, usize, usize) {
        (
            self.allocated_entries,
            self.capacity_rejected_entries,
            self.capacity_rejected_items,
        )
    }
}

/// Результат atomic replace top-level entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplacePlaylistEntriesOutcome {
    /// Queue и current уже отсутствовали.
    AlreadyEmpty,
    /// Empty replacement очистил committed entries.
    Cleared {
        /// Число удалённых playable Item IDs.
        removed_item_count: usize,
        /// Влияние на persisted traversal current.
        traversal_current_effect: TraversalCurrentEffect,
    },
    /// Новый canonical top-level batch committed.
    Replaced {
        /// Все опубликованные structural/playable identities.
        allocated_entries: AllocatedPlaylistEntries,
        /// Влияние на persisted traversal current.
        traversal_current_effect: TraversalCurrentEffect,
    },
}

/// Typed preflight failure общего Single/Compound mutation boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistEntriesMutationError {
    /// D08 reservation сериализует внешний strong-install commit.
    InstallCommitLinearizing,
    /// Суммарное число retained parts превысило hard cap.
    CapacityExceeded {
        /// Уже committed retained Item IDs.
        current_retained_items: usize,
        /// Retained Item IDs во входном batch.
        requested_retained_items: usize,
        /// Hard safety limit.
        maximum: usize,
    },
    /// Item ID fixed-width range исчерпан.
    ItemIdArithmeticExhausted,
    /// Item allocator обнаружил committed collision.
    ItemIdCollision {
        /// Конфликтующая playable identity.
        item_id: PlaylistItemId,
    },
    /// Group ID fixed-width range исчерпан.
    CompoundGroupIdArithmeticExhausted,
    /// Group allocator обнаружил committed collision.
    CompoundGroupIdCollision {
        /// Конфликтующая structural identity.
        group_id: PlaylistCompoundGroupId,
    },
    /// Structural revision fixed-width range исчерпан.
    StructuralRevisionExhausted,
    /// Traversal revision fixed-width range исчерпан.
    TraversalRevisionExhausted,
}

impl fmt::Display for PlaylistEntriesMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallCommitLinearizing => {
                formatter.write_str("queue mutation заблокирована active install commit")
            }
            Self::CapacityExceeded {
                current_retained_items,
                requested_retained_items,
                maximum,
            } => write!(
                formatter,
                "capacity превышена: retained {current_retained_items}, requested {requested_retained_items}, maximum {maximum}"
            ),
            Self::ItemIdArithmeticExhausted => {
                formatter.write_str("пространство PlaylistItemId исчерпано")
            }
            Self::ItemIdCollision { item_id } => {
                write!(formatter, "будущий PlaylistItemId конфликтует: {item_id}")
            }
            Self::CompoundGroupIdArithmeticExhausted => {
                formatter.write_str("пространство PlaylistCompoundGroupId исчерпано")
            }
            Self::CompoundGroupIdCollision { group_id } => {
                write!(
                    formatter,
                    "будущий PlaylistCompoundGroupId конфликтует: {group_id}"
                )
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
            }
            Self::TraversalRevisionExhausted => formatter.write_str("traversal revision исчерпана"),
        }
    }
}

impl std::error::Error for PlaylistEntriesMutationError {}

impl PlaylistQueue {
    /// Атомарно добавляет ID-less top-level entries в canonical tail.
    pub fn append_entries(
        &mut self,
        drafts: Vec<PlaylistEntryDraft>,
    ) -> Result<AddPlaylistEntriesOutcome, PlaylistEntriesMutationError> {
        let mut random = rand::rng();
        self.append_entries_with_rng(drafts, &mut random)
    }

    /// Вариант append с injectable RNG для existing shuffle lifecycle.
    pub fn append_entries_with_rng<R: rand::Rng + ?Sized>(
        &mut self,
        drafts: Vec<PlaylistEntryDraft>,
        random: &mut R,
    ) -> Result<AddPlaylistEntriesOutcome, PlaylistEntriesMutationError> {
        if self.active_reservation.is_some() {
            return Err(PlaylistEntriesMutationError::InstallCommitLinearizing);
        }
        if drafts.is_empty() {
            return Ok(AddPlaylistEntriesOutcome::NoEntriesProvided);
        }

        let requested_retained_items = retained_draft_count(&drafts)?;
        let current_retained_items = self.retained_item_count();
        current_retained_items
            .checked_add(requested_retained_items)
            .filter(|resulting_count| *resulting_count <= MAX_PLAYLIST_ITEMS)
            .ok_or(PlaylistEntriesMutationError::CapacityExceeded {
                current_retained_items,
                requested_retained_items,
                maximum: MAX_PLAYLIST_ITEMS,
            })?;
        let next_structural_revision = self
            .structural_revision
            .checked_next()
            .ok_or(PlaylistEntriesMutationError::StructuralRevisionExhausted)?;
        let prepared = self.preflight_entry_allocation(drafts)?;

        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.merge_new_entries(&prepared.allocated.entry_ids, random);
        }
        self.item_id_allocator
            .commit_allocation(&prepared.item_allocation_plan);
        self.compound_group_id_allocator
            .commit_allocation(&prepared.group_allocation_plan);
        self.entries.extend(prepared.entries);
        self.structural_revision = next_structural_revision;

        Ok(AddPlaylistEntriesOutcome::Added(prepared.allocated))
    }

    /// Добавляет только целый top-level prefix, помещающийся в retained capacity.
    pub fn append_capped_entries(
        &mut self,
        mut drafts: Vec<PlaylistEntryDraft>,
    ) -> Result<CappedPlaylistEntriesAppendOutcome, PlaylistEntriesMutationError> {
        let remaining_capacity = MAX_PLAYLIST_ITEMS.saturating_sub(self.retained_item_count());
        let mut accepted_entries = 0usize;
        let mut accepted_items = 0usize;

        for draft in &drafts {
            let draft_items = draft.retained_item_count();
            let Some(next_accepted_items) = accepted_items.checked_add(draft_items) else {
                break;
            };
            if next_accepted_items > remaining_capacity {
                break;
            }
            accepted_entries += 1;
            accepted_items = next_accepted_items;
        }

        let capacity_rejected_entries = drafts.len().saturating_sub(accepted_entries);
        let capacity_rejected_items = drafts[accepted_entries..]
            .iter()
            .try_fold(0usize, |count, draft| {
                count.checked_add(draft.retained_item_count())
            })
            .expect("in-memory drafts cannot contain more than usize::MAX retained parts");
        drafts.truncate(accepted_entries);

        let allocated_entries = match self.append_entries(drafts)? {
            AddPlaylistEntriesOutcome::Added(allocated) => allocated,
            AddPlaylistEntriesOutcome::NoEntriesProvided => AllocatedPlaylistEntries {
                entry_ids: Vec::new(),
                playable_item_ids: Vec::new(),
            },
        };

        Ok(CappedPlaylistEntriesAppendOutcome {
            allocated_entries,
            capacity_rejected_entries,
            capacity_rejected_items,
        })
    }

    /// Атомарно заменяет canonical top-level entries и очищает current.
    pub fn replace_entries(
        &mut self,
        drafts: Vec<PlaylistEntryDraft>,
    ) -> Result<ReplacePlaylistEntriesOutcome, PlaylistEntriesMutationError> {
        let mut random = rand::rng();
        self.replace_entries_with_rng(drafts, &mut random)
    }

    /// Вариант replace с injectable RNG для existing shuffle lifecycle.
    pub fn replace_entries_with_rng<R: rand::Rng + ?Sized>(
        &mut self,
        drafts: Vec<PlaylistEntryDraft>,
        random: &mut R,
    ) -> Result<ReplacePlaylistEntriesOutcome, PlaylistEntriesMutationError> {
        if self.active_reservation.is_some() {
            return Err(PlaylistEntriesMutationError::InstallCommitLinearizing);
        }

        let requested_retained_items = retained_draft_count(&drafts)?;
        if requested_retained_items > MAX_PLAYLIST_ITEMS {
            return Err(PlaylistEntriesMutationError::CapacityExceeded {
                current_retained_items: 0,
                requested_retained_items,
                maximum: MAX_PLAYLIST_ITEMS,
            });
        }
        if drafts.is_empty() && self.entries.is_empty() && self.traversal_current.is_none() {
            return Ok(ReplacePlaylistEntriesOutcome::AlreadyEmpty);
        }

        let next_structural_revision = self
            .structural_revision
            .checked_next()
            .ok_or(PlaylistEntriesMutationError::StructuralRevisionExhausted)?;
        let next_traversal_revision = self
            .traversal_current
            .map(|_| {
                self.traversal_revision
                    .checked_next()
                    .ok_or(PlaylistEntriesMutationError::TraversalRevisionExhausted)
            })
            .transpose()?;
        let traversal_current_effect = if self.traversal_current.is_some() {
            TraversalCurrentEffect::Cleared
        } else {
            TraversalCurrentEffect::Preserved
        };

        if drafts.is_empty() {
            let removed_item_count = self.retained_item_count();
            let replacement_shuffle = self
                .shuffle_traversal
                .as_ref()
                .map(|_| shuffle::ShuffleTraversal::fresh(&[], None, random));
            self.entries.clear();
            self.traversal_current = None;
            self.shuffle_traversal = replacement_shuffle;
            self.structural_revision = next_structural_revision;
            if let Some(next_revision) = next_traversal_revision {
                self.traversal_revision = next_revision;
            }
            return Ok(ReplacePlaylistEntriesOutcome::Cleared {
                removed_item_count,
                traversal_current_effect,
            });
        }

        let prepared = self.preflight_entry_allocation(drafts)?;
        let replacement_shuffle = self
            .shuffle_traversal
            .as_ref()
            .map(|_| shuffle::ShuffleTraversal::fresh(&prepared.allocated.entry_ids, None, random));

        self.item_id_allocator
            .commit_allocation(&prepared.item_allocation_plan);
        self.compound_group_id_allocator
            .commit_allocation(&prepared.group_allocation_plan);
        self.entries = prepared.entries;
        self.traversal_current = None;
        self.shuffle_traversal = replacement_shuffle;
        self.structural_revision = next_structural_revision;
        if let Some(next_revision) = next_traversal_revision {
            self.traversal_revision = next_revision;
        }

        Ok(ReplacePlaylistEntriesOutcome::Replaced {
            allocated_entries: prepared.allocated,
            traversal_current_effect,
        })
    }

    /// Проверяет обе identity lineage и строит candidate storage до commit.
    fn preflight_entry_allocation(
        &self,
        drafts: Vec<PlaylistEntryDraft>,
    ) -> Result<PreparedPlaylistEntries, PlaylistEntriesMutationError> {
        let item_count = retained_draft_count(&drafts)?;
        let group_count = drafts.iter().filter(|draft| draft.is_compound()).count();
        let item_allocation_plan = self
            .item_id_allocator
            .preflight_allocation(item_count, &self.existing_item_ids())
            .map_err(map_item_allocation_error)?;
        let group_allocation_plan = self
            .compound_group_id_allocator
            .preflight_allocation(group_count, &self.existing_compound_group_ids())
            .map_err(map_group_allocation_error)?;
        let allocated = allocation_receipt(&drafts, &item_allocation_plan, &group_allocation_plan);
        let entries = entries_from_drafts(drafts, &item_allocation_plan, &group_allocation_plan);

        Ok(PreparedPlaylistEntries {
            entries,
            allocated,
            item_allocation_plan,
            group_allocation_plan,
        })
    }

    /// Собирает committed Group IDs для collision preflight.
    fn existing_compound_group_ids(&self) -> HashSet<PlaylistCompoundGroupId> {
        self.entries
            .iter()
            .filter_map(|entry| match entry.entry_id() {
                PlaylistEntryId::Single(_) => None,
                PlaylistEntryId::Compound(group_id) => Some(group_id),
            })
            .collect()
    }
}

/// Candidate state, полностью построенный до queue mutation.
struct PreparedPlaylistEntries {
    entries: Vec<PlaylistEntry>,
    allocated: AllocatedPlaylistEntries,
    item_allocation_plan: ItemIdAllocationPlan,
    group_allocation_plan: CompoundGroupIdAllocationPlan,
}

/// Checked суммирует retained Item ID demand batch-а.
fn retained_draft_count(
    drafts: &[PlaylistEntryDraft],
) -> Result<usize, PlaylistEntriesMutationError> {
    drafts.iter().try_fold(0usize, |count, draft| {
        count.checked_add(draft.retained_item_count()).ok_or(
            PlaylistEntriesMutationError::CapacityExceeded {
                current_retained_items: 0,
                requested_retained_items: usize::MAX,
                maximum: MAX_PLAYLIST_ITEMS,
            },
        )
    })
}

/// Строит caller receipt в canonical/derived порядке.
fn allocation_receipt(
    drafts: &[PlaylistEntryDraft],
    item_plan: &ItemIdAllocationPlan,
    group_plan: &CompoundGroupIdAllocationPlan,
) -> AllocatedPlaylistEntries {
    let mut entry_ids = Vec::with_capacity(drafts.len());
    let mut item_offset = 0usize;
    let mut group_offset = 0usize;

    for draft in drafts {
        match draft {
            PlaylistEntryDraft::Single(_) => {
                entry_ids.push(PlaylistEntryId::Single(
                    item_plan.allocated_item_ids[item_offset],
                ));
                item_offset += 1;
            }
            PlaylistEntryDraft::Compound(group) => {
                entry_ids.push(PlaylistEntryId::Compound(
                    group_plan.allocated_group_ids[group_offset],
                ));
                item_offset += group.retained_part_count();
                group_offset += 1;
            }
        }
    }

    debug_assert_eq!(item_offset, item_plan.allocated_item_ids.len());
    debug_assert_eq!(group_offset, group_plan.allocated_group_ids.len());
    AllocatedPlaylistEntries {
        entry_ids,
        playable_item_ids: item_plan.allocated_item_ids.clone(),
    }
}

/// Материализует canonical entries только из полностью preflighted ranges.
fn entries_from_drafts(
    drafts: Vec<PlaylistEntryDraft>,
    item_plan: &ItemIdAllocationPlan,
    group_plan: &CompoundGroupIdAllocationPlan,
) -> Vec<PlaylistEntry> {
    let mut entries = Vec::with_capacity(drafts.len());
    let mut item_offset = 0usize;
    let mut group_offset = 0usize;

    for draft in drafts {
        match draft {
            PlaylistEntryDraft::Single(item_draft) => {
                let item_id = item_plan.allocated_item_ids[item_offset];
                entries.push(PlaylistEntry::Single(item_draft.into_item(item_id)));
                item_offset += 1;
            }
            PlaylistEntryDraft::Compound(group_draft) => {
                let part_count = group_draft.retained_part_count();
                let part_item_ids =
                    &item_plan.allocated_item_ids[item_offset..item_offset + part_count];
                let group_id = group_plan.allocated_group_ids[group_offset];
                entries.push(PlaylistEntry::Compound(Box::new(
                    PlaylistCompoundGroup::from_draft(group_draft, group_id, part_item_ids),
                )));
                item_offset += part_count;
                group_offset += 1;
            }
        }
    }

    debug_assert_eq!(item_offset, item_plan.allocated_item_ids.len());
    debug_assert_eq!(group_offset, group_plan.allocated_group_ids.len());
    entries
}

/// Сохраняет typed Item allocator failure.
const fn map_item_allocation_error(error: ItemIdAllocationError) -> PlaylistEntriesMutationError {
    match error {
        ItemIdAllocationError::ArithmeticExhausted => {
            PlaylistEntriesMutationError::ItemIdArithmeticExhausted
        }
        ItemIdAllocationError::Collision { item_id } => {
            PlaylistEntriesMutationError::ItemIdCollision { item_id }
        }
    }
}

/// Сохраняет typed Group allocator failure.
const fn map_group_allocation_error(
    error: CompoundGroupIdAllocationError,
) -> PlaylistEntriesMutationError {
    match error {
        CompoundGroupIdAllocationError::ArithmeticExhausted => {
            PlaylistEntriesMutationError::CompoundGroupIdArithmeticExhausted
        }
        CompoundGroupIdAllocationError::Collision { group_id } => {
            PlaylistEntriesMutationError::CompoundGroupIdCollision { group_id }
        }
    }
}
