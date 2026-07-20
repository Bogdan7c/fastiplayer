//! Атомарное перемещение нескольких canonical строк как одного блока.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::{PlaylistEntryId, PlaylistItemId};

use super::structural::StructuralEntryLookupError;
use super::{MoveItemIntent, MoveItemOutcome, PlaylistQueue};

/// Typed outcome группового перемещения не смешивает ошибки запроса и no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveItemsOutcome {
    /// Caller не передал ни одной structural identity.
    NoItemsRequested,
    /// Одна structural identity повторилась, поэтому запрос неоднозначен.
    DuplicateEntryId {
        /// Первый обнаруженный повторяющийся ID.
        entry_id: PlaylistEntryId,
    },
    /// Одна из requested entries отсутствует в committed queue.
    EntryNotFound {
        /// Первый отсутствующий ID в caller order.
        entry_id: PlaylistEntryId,
    },
    /// Caller передал playable part вместо owning compound identity.
    CompoundPartTarget {
        /// Ошибочно переданный playable identity.
        part_item_id: PlaylistItemId,
        /// Required structural owner.
        compound_entry_id: PlaylistEntryId,
    },
    /// Named anchor отсутствует в committed queue.
    AnchorNotFound {
        /// Отсутствующий anchor ID.
        anchor_entry_id: PlaylistEntryId,
    },
    /// Anchor указывает на subordinate playable part.
    CompoundPartAnchor {
        /// Ошибочно переданный playable identity.
        part_item_id: PlaylistItemId,
        /// Required structural owner.
        compound_entry_id: PlaylistEntryId,
    },
    /// Anchor входит в перемещаемую группу и потому не задаёт внешнюю границу.
    AnchorSelected {
        /// Requested anchor, принадлежащий группе.
        anchor_entry_id: PlaylistEntryId,
    },
    /// Итоговый canonical order совпадает с текущим.
    AlreadyInPlace {
        /// Количество проверенных уникальных строк.
        item_count: usize,
    },
    /// Весь блок опубликован одной structural revision.
    Moved {
        /// Количество перемещённых строк.
        item_count: usize,
    },
    /// D08 reservation удерживает structural mutation lock.
    InstallCommitLinearizing,
    /// Structural revision нельзя продвинуть без нарушения monotonicity.
    StructuralRevisionExhausted,
}

impl fmt::Display for MoveItemsOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoItemsRequested => formatter.write_str("не переданы строки для перемещения"),
            Self::DuplicateEntryId { entry_id } => {
                write!(formatter, "Entry ID {entry_id:?} передан повторно")
            }
            Self::EntryNotFound { entry_id } => {
                write!(formatter, "Entry ID {entry_id:?} не найден")
            }
            Self::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => write!(
                formatter,
                "{part_item_id} является частью {compound_entry_id:?}; требуется compound target"
            ),
            Self::AnchorNotFound { anchor_entry_id } => {
                write!(formatter, "anchor {anchor_entry_id:?} не найден")
            }
            Self::CompoundPartAnchor {
                part_item_id,
                compound_entry_id,
            } => write!(
                formatter,
                "anchor {part_item_id} является частью {compound_entry_id:?}"
            ),
            Self::AnchorSelected { anchor_entry_id } => {
                write!(
                    formatter,
                    "anchor {anchor_entry_id:?} входит в перемещаемую группу"
                )
            }
            Self::AlreadyInPlace { item_count } => {
                write!(
                    formatter,
                    "{item_count} строк уже находятся на requested месте"
                )
            }
            Self::Moved { item_count } => {
                write!(formatter, "{item_count} строк перемещены одним блоком")
            }
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit временно блокирует group move")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
            }
        }
    }
}

impl PlaylistQueue {
    /// Перемещает exact top-level entry относительно intent-named anchor.
    pub fn move_item(
        &mut self,
        entry_id: PlaylistEntryId,
        intent: MoveItemIntent,
    ) -> MoveItemOutcome {
        if self.active_reservation.is_some() {
            return MoveItemOutcome::InstallCommitLinearizing;
        }
        let source_index = match self.resolve_top_level_entry_index(entry_id) {
            Ok(source_index) => source_index,
            Err(StructuralEntryLookupError::NotFound) => {
                return MoveItemOutcome::EntryNotFound { entry_id };
            }
            Err(StructuralEntryLookupError::CompoundPart {
                part_item_id,
                compound_entry_id,
            }) => {
                return MoveItemOutcome::CompoundPartTarget {
                    part_item_id,
                    compound_entry_id,
                };
            }
        };
        let target_index = match self.move_target_index(source_index, intent) {
            Ok(target_index) => target_index,
            Err(StructuralEntryLookupError::NotFound) => {
                let anchor_entry_id = intent
                    .anchor()
                    .expect("only anchored intent can fail anchor lookup");
                return MoveItemOutcome::AnchorNotFound { anchor_entry_id };
            }
            Err(StructuralEntryLookupError::CompoundPart {
                part_item_id,
                compound_entry_id,
            }) => {
                return MoveItemOutcome::CompoundPartAnchor {
                    part_item_id,
                    compound_entry_id,
                };
            }
        };

        if source_index == target_index {
            return MoveItemOutcome::AlreadyInPlace { entry_id };
        }
        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return MoveItemOutcome::StructuralRevisionExhausted;
        };

        let moved_entry = self.entries.remove(source_index);
        self.entries.insert(target_index, moved_entry);
        self.structural_revision = next_structural_revision;

        MoveItemOutcome::Moved { entry_id }
    }

    /// Вычисляет final insertion index после удаления source entry.
    fn move_target_index(
        &self,
        source_index: usize,
        intent: MoveItemIntent,
    ) -> Result<usize, StructuralEntryLookupError> {
        match intent {
            MoveItemIntent::ToFront => Ok(0),
            MoveItemIntent::ToBack => Ok(self.entries.len().saturating_sub(1)),
            MoveItemIntent::Before(anchor_entry_id) => {
                let anchor_index = self.resolve_top_level_entry_index(anchor_entry_id)?;
                if anchor_index == source_index {
                    return Ok(source_index);
                }
                Ok(if source_index < anchor_index {
                    anchor_index - 1
                } else {
                    anchor_index
                })
            }
            MoveItemIntent::After(anchor_entry_id) => {
                let anchor_index = self.resolve_top_level_entry_index(anchor_entry_id)?;
                if anchor_index == source_index {
                    return Ok(source_index);
                }
                let anchor_after_removal = if source_index < anchor_index {
                    anchor_index - 1
                } else {
                    anchor_index
                };
                Ok(anchor_after_removal + 1)
            }
        }
    }

    /// Перемещает requested top-level entries одним блоком в canonical порядке.
    pub fn move_items(
        &mut self,
        requested_entry_ids: &[PlaylistEntryId],
        intent: MoveItemIntent,
    ) -> MoveItemsOutcome {
        if requested_entry_ids.is_empty() {
            return MoveItemsOutcome::NoItemsRequested;
        }

        let mut selected_entry_ids = HashSet::with_capacity(requested_entry_ids.len());
        for entry_id in requested_entry_ids {
            if !selected_entry_ids.insert(*entry_id) {
                return MoveItemsOutcome::DuplicateEntryId {
                    entry_id: *entry_id,
                };
            }
        }

        let committed_entry_ids = self
            .entries
            .iter()
            .map(crate::PlaylistEntry::entry_id)
            .collect::<HashSet<_>>();
        let compound_entry_by_part = self
            .entries
            .iter()
            .filter_map(crate::PlaylistEntry::as_compound)
            .flat_map(|group| {
                let compound_entry_id = PlaylistEntryId::Compound(group.group_id());
                group
                    .parts()
                    .map(move |part| (part.item().item_id(), compound_entry_id))
            })
            .collect::<HashMap<_, _>>();
        for entry_id in requested_entry_ids {
            if committed_entry_ids.contains(entry_id) {
                continue;
            }
            if let PlaylistEntryId::Single(part_item_id) = entry_id
                && let Some(compound_entry_id) = compound_entry_by_part.get(part_item_id)
            {
                return MoveItemsOutcome::CompoundPartTarget {
                    part_item_id: *part_item_id,
                    compound_entry_id: *compound_entry_id,
                };
            }
            return MoveItemsOutcome::EntryNotFound {
                entry_id: *entry_id,
            };
        }

        if let Some(anchor_entry_id) = intent.anchor() {
            if !committed_entry_ids.contains(&anchor_entry_id) {
                if let PlaylistEntryId::Single(part_item_id) = anchor_entry_id
                    && let Some(compound_entry_id) = compound_entry_by_part.get(&part_item_id)
                {
                    return MoveItemsOutcome::CompoundPartAnchor {
                        part_item_id,
                        compound_entry_id: *compound_entry_id,
                    };
                }
                return MoveItemsOutcome::AnchorNotFound { anchor_entry_id };
            }
            if selected_entry_ids.contains(&anchor_entry_id) {
                return MoveItemsOutcome::AnchorSelected { anchor_entry_id };
            }
        }

        if self.active_reservation.is_some() {
            return MoveItemsOutcome::InstallCommitLinearizing;
        }

        let selected_entries = self
            .entries
            .iter()
            .filter(|entry| selected_entry_ids.contains(&entry.entry_id()))
            .cloned()
            .collect::<Vec<_>>();
        let mut retained_entries = self
            .entries
            .iter()
            .filter(|entry| !selected_entry_ids.contains(&entry.entry_id()))
            .cloned()
            .collect::<Vec<_>>();

        let insertion_index = match intent {
            MoveItemIntent::ToFront => 0,
            MoveItemIntent::ToBack => retained_entries.len(),
            MoveItemIntent::Before(anchor_entry_id) => retained_entries
                .iter()
                .position(|entry| entry.entry_id() == anchor_entry_id)
                .expect("validated unselected anchor must remain committed"),
            MoveItemIntent::After(anchor_entry_id) => {
                retained_entries
                    .iter()
                    .position(|entry| entry.entry_id() == anchor_entry_id)
                    .expect("validated unselected anchor must remain committed")
                    + 1
            }
        };
        retained_entries.splice(insertion_index..insertion_index, selected_entries);

        if retained_entries == self.entries {
            return MoveItemsOutcome::AlreadyInPlace {
                item_count: selected_entry_ids.len(),
            };
        }
        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return MoveItemsOutcome::StructuralRevisionExhausted;
        };

        self.entries = retained_entries;
        self.structural_revision = next_structural_revision;
        MoveItemsOutcome::Moved {
            item_count: selected_entry_ids.len(),
        }
    }
}
