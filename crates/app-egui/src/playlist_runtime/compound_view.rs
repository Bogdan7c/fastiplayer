//! Process-lifetime compound presentation model без зависимости от egui renderer.
//!
//! Публичные внутри crate row/action методы подготавливают S17V consumer и поэтому
//! намеренно ещё не вызываются production renderer-ом в этой сессии.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use playlist_core::{
    PlaylistCompoundGroupId, PlaylistCompoundPartOrdinal, PlaylistEntry, PlaylistEntryId,
    PlaylistItemId, PlaylistQueue,
};

use super::identity::ActiveMediaIdentity;
use super::selection::PlaylistSelectionSnapshot;
use super::view::PlaylistStructuralRevision;

/// Stable identity различает structural header и subordinate projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CompoundRuntimeRowId {
    /// Top-level queue entry участвует в selection/reorder/remove/export.
    Entry(PlaylistEntryId),
    /// Child является только projection и никогда не становится structural intent-ом.
    Part {
        /// Explicit compound identity не позволяет потерять group boundary.
        compound_entry_id: PlaylistEntryId,
        /// Exact playable identity используется только для strong-open.
        part_item_id: PlaylistItemId,
    },
}

/// Read-only row shape не содержит renderer widgets или geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompoundRuntimeRow {
    /// Обычная top-level media строка.
    Single {
        entry_id: PlaylistEntryId,
        item_id: PlaylistItemId,
        active: bool,
        selected: bool,
    },
    /// Group header остаётся одной structural строкой при любом disclosure state.
    CompoundHeader {
        entry_id: PlaylistEntryId,
        group_id: PlaylistCompoundGroupId,
        retained_part_count: usize,
        expanded: bool,
        active_part_item_id: Option<PlaylistItemId>,
        header_play_item_id: PlaylistItemId,
        selected: bool,
    },
    /// Subordinate projection не участвует в top-level queue count.
    CompoundPart {
        compound_entry_id: PlaylistEntryId,
        part_item_id: PlaylistItemId,
        ordinal: PlaylistCompoundPartOrdinal,
        active: bool,
    },
}

impl CompoundRuntimeRow {
    /// Возвращает stable row identity для virtualization/focus будущего renderer-а.
    pub(crate) const fn row_id(self) -> CompoundRuntimeRowId {
        match self {
            Self::Single { entry_id, .. } | Self::CompoundHeader { entry_id, .. } => {
                CompoundRuntimeRowId::Entry(entry_id)
            }
            Self::CompoundPart {
                compound_entry_id,
                part_item_id,
                ..
            } => CompoundRuntimeRowId::Part {
                compound_entry_id,
                part_item_id,
            },
        }
    }

    /// Structural identity доступна только header/single row.
    pub(crate) const fn structural_entry_id(self) -> Option<PlaylistEntryId> {
        match self {
            Self::Single { entry_id, .. } | Self::CompoundHeader { entry_id, .. } => Some(entry_id),
            Self::CompoundPart { .. } => None,
        }
    }

    /// Child projection никогда не masquerade-ит как draggable/removable row.
    pub(crate) const fn is_subordinate_part(self) -> bool {
        matches!(self, Self::CompoundPart { .. })
    }
}

/// Immutable snapshot отделяет visible count от domain capacity/count.
#[derive(Debug, Clone)]
pub(crate) struct CompoundRuntimeViewSnapshot {
    structural_revision: PlaylistStructuralRevision,
    rows: Arc<[CompoundRuntimeRow]>,
    top_level_entry_count: usize,
    header_indices: Arc<HashMap<PlaylistEntryId, usize>>,
    part_indices: Arc<HashMap<PlaylistItemId, usize>>,
    entry_id_by_item: Arc<HashMap<PlaylistItemId, PlaylistEntryId>>,
}

impl CompoundRuntimeViewSnapshot {
    /// Snapshot generation fence обязателен для всех row actions.
    pub(crate) const fn structural_revision(&self) -> PlaylistStructuralRevision {
        self.structural_revision
    }

    /// Domain top-level count не зависит от disclosure state.
    pub(crate) const fn top_level_entry_count(&self) -> usize {
        self.top_level_entry_count
    }

    /// UI visible count включает только раскрытые child projections.
    pub(crate) fn visible_row_count(&self) -> usize {
        self.rows.len()
    }

    /// Bounded virtualization slice возвращает owned Copy projections.
    pub(crate) fn visible_rows(&self, range: std::ops::Range<usize>) -> Vec<CompoundRuntimeRow> {
        let start = range.start.min(self.rows.len());
        let end = range.end.min(self.rows.len()).max(start);
        self.rows[start..end].to_vec()
    }

    /// Current Item скроллит к child только когда он реально раскрыт.
    pub(crate) fn current_item_scroll_target(
        &self,
        current_item_id: PlaylistItemId,
    ) -> Option<CompoundCurrentItemScrollTarget> {
        if self.part_indices.contains_key(&current_item_id) {
            return Some(CompoundCurrentItemScrollTarget::Part(current_item_id));
        }
        let entry_id = self.entry_id_by_item.get(&current_item_id).copied()?;
        Some(CompoundCurrentItemScrollTarget::Header(entry_id))
    }

    /// Header lookup используется collapsed group fallback без auto-expand.
    pub(crate) fn header_row_index(&self, entry_id: PlaylistEntryId) -> Option<usize> {
        self.header_indices.get(&entry_id).copied()
    }
}

/// Typed Current Item target не смешивает group header с exact playable part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompoundCurrentItemScrollTarget {
    Header(PlaylistEntryId),
    Part(PlaylistItemId),
}

/// Disclosure action является UI-only и generation-fenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToggleCompoundDisclosure {
    pub(crate) compound_entry_id: PlaylistEntryId,
    pub(crate) structural_revision: PlaylistStructuralRevision,
}

/// Typed outcome сохраняет stale/not-compound/not-found distinctions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToggleCompoundDisclosureOutcome {
    Expanded,
    Collapsed,
    StaleStructuralRevision,
    EntryNotFound,
    NotCompoundEntry,
}

/// Header Play intent содержит explicit group identity и structural generation fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompoundHeaderPlayAction {
    /// Explicit compound identity не позволяет вызвать group action для Single.
    pub(crate) compound_entry_id: PlaylistEntryId,
    /// Generation fence связывает click с конкретной structural projection.
    pub(crate) structural_revision: PlaylistStructuralRevision,
}

/// Header Play сначала разрешается в один exact target и не содержит fallback scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompoundHeaderPlayTarget {
    ExactItem(PlaylistItemId),
    StaleStructuralRevision,
    EntryNotFound,
    NotCompoundEntry,
}

/// Child action всегда несёт explicit group identity и exact part identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompoundPartPlayAction {
    pub(crate) compound_entry_id: PlaylistEntryId,
    pub(crate) part_item_id: PlaylistItemId,
    pub(crate) structural_revision: PlaylistStructuralRevision,
}

/// Part preflight не разрешает subordinate identity стать structural mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompoundPartPlayTarget {
    ExactItem(PlaylistItemId),
    StaleStructuralRevision,
    EntryNotFound,
    NotCompoundEntry,
    PartNotInGroup,
}

/// Mutable disclosure owner живёт рядом с controller, а не в renderer-bound state.
#[derive(Debug, Default)]
pub(super) struct CompoundRuntimeViewState {
    expanded_group_ids: HashSet<PlaylistCompoundGroupId>,
}

impl CompoundRuntimeViewState {
    /// Удаляет disclosure для групп, которых больше нет в committed queue.
    pub(super) fn retain_committed_groups(&mut self, queue: &PlaylistQueue) {
        self.expanded_group_ids.retain(|group_id| {
            queue
                .top_level_entry(PlaylistEntryId::Compound(*group_id))
                .is_some()
        });
    }

    /// Toggle меняет только process-lifetime UI state.
    pub(super) fn toggle(
        &mut self,
        queue: &PlaylistQueue,
        current_revision: PlaylistStructuralRevision,
        action: ToggleCompoundDisclosure,
    ) -> ToggleCompoundDisclosureOutcome {
        if action.structural_revision != current_revision {
            return ToggleCompoundDisclosureOutcome::StaleStructuralRevision;
        }
        let PlaylistEntryId::Compound(group_id) = action.compound_entry_id else {
            return ToggleCompoundDisclosureOutcome::NotCompoundEntry;
        };
        if queue.top_level_entry(action.compound_entry_id).is_none() {
            return ToggleCompoundDisclosureOutcome::EntryNotFound;
        }
        if self.expanded_group_ids.remove(&group_id) {
            ToggleCompoundDisclosureOutcome::Collapsed
        } else {
            self.expanded_group_ids.insert(group_id);
            ToggleCompoundDisclosureOutcome::Expanded
        }
    }

    /// Строит renderer-neutral rows из authoritative queue/current/active/selection.
    pub(super) fn snapshot(
        &self,
        queue: &PlaylistQueue,
        structural_revision: PlaylistStructuralRevision,
        active_media: Option<ActiveMediaIdentity>,
        selection: &PlaylistSelectionSnapshot,
    ) -> CompoundRuntimeViewSnapshot {
        let active_item_id = active_media.and_then(ActiveMediaIdentity::item_id);
        let current_item_id = queue.traversal_current().map(|current| current.item_id());
        let mut rows =
            Vec::with_capacity(queue.top_level_entry_count() + self.expanded_group_ids.len());
        let mut header_indices = HashMap::with_capacity(queue.top_level_entry_count());
        let mut part_indices = HashMap::new();
        let mut entry_id_by_item = HashMap::with_capacity(queue.retained_item_count());

        for entry in queue.iter_top_level_entries() {
            let entry_id = entry.entry_id();
            header_indices.insert(entry_id, rows.len());
            match entry {
                PlaylistEntry::Single(item) => {
                    entry_id_by_item.insert(item.item_id(), entry_id);
                    rows.push(CompoundRuntimeRow::Single {
                        entry_id,
                        item_id: item.item_id(),
                        active: active_item_id == Some(item.item_id()),
                        selected: selection.is_selected(entry_id),
                    });
                }
                PlaylistEntry::Compound(group) => {
                    entry_id_by_item
                        .extend(group.parts().map(|part| (part.item().item_id(), entry_id)));
                    let expanded = self.expanded_group_ids.contains(&group.group_id());
                    let active_part_item_id = group
                        .parts()
                        .map(|part| part.item().item_id())
                        .find(|part_item_id| Some(*part_item_id) == active_item_id);
                    let current_part_item_id = group
                        .parts()
                        .map(|part| part.item().item_id())
                        .find(|part_item_id| Some(*part_item_id) == current_item_id);
                    let first_part_item_id = group
                        .parts()
                        .next()
                        .expect("validated compound group always retains a part")
                        .item()
                        .item_id();
                    rows.push(CompoundRuntimeRow::CompoundHeader {
                        entry_id,
                        group_id: group.group_id(),
                        retained_part_count: group.retained_part_count(),
                        expanded,
                        active_part_item_id,
                        header_play_item_id: current_part_item_id.unwrap_or(first_part_item_id),
                        selected: selection.is_selected(entry_id),
                    });
                    if expanded {
                        for part in group.parts() {
                            let part_item_id = part.item().item_id();
                            part_indices.insert(part_item_id, rows.len());
                            rows.push(CompoundRuntimeRow::CompoundPart {
                                compound_entry_id: entry_id,
                                part_item_id,
                                ordinal: part.membership().ordinal(),
                                active: active_item_id == Some(part_item_id),
                            });
                        }
                    }
                }
            }
        }

        CompoundRuntimeViewSnapshot {
            structural_revision,
            rows: rows.into(),
            top_level_entry_count: queue.top_level_entry_count(),
            header_indices: Arc::new(header_indices),
            part_indices: Arc::new(part_indices),
            entry_id_by_item: Arc::new(entry_id_by_item),
        }
    }
}

/// Разрешает header Play в current-in-group либо first part без попыток открытия.
pub(super) fn resolve_header_play_target(
    queue: &PlaylistQueue,
    current_revision: PlaylistStructuralRevision,
    action: CompoundHeaderPlayAction,
) -> CompoundHeaderPlayTarget {
    if action.structural_revision != current_revision {
        return CompoundHeaderPlayTarget::StaleStructuralRevision;
    }
    let PlaylistEntryId::Compound(_) = action.compound_entry_id else {
        return CompoundHeaderPlayTarget::NotCompoundEntry;
    };
    let Some(PlaylistEntry::Compound(group)) = queue.top_level_entry(action.compound_entry_id)
    else {
        return CompoundHeaderPlayTarget::EntryNotFound;
    };
    let current_item_id = queue.traversal_current().map(|current| current.item_id());
    let target_item_id = group
        .parts()
        .map(|part| part.item().item_id())
        .find(|part_item_id| Some(*part_item_id) == current_item_id)
        .unwrap_or_else(|| {
            group
                .parts()
                .next()
                .expect("validated compound group always retains a part")
                .item()
                .item_id()
        });
    CompoundHeaderPlayTarget::ExactItem(target_item_id)
}

/// Проверяет exact subordinate target без mutation selection/disclosure/queue.
pub(super) fn resolve_part_play_target(
    queue: &PlaylistQueue,
    current_revision: PlaylistStructuralRevision,
    action: CompoundPartPlayAction,
) -> CompoundPartPlayTarget {
    if action.structural_revision != current_revision {
        return CompoundPartPlayTarget::StaleStructuralRevision;
    }
    let PlaylistEntryId::Compound(_) = action.compound_entry_id else {
        return CompoundPartPlayTarget::NotCompoundEntry;
    };
    let Some(PlaylistEntry::Compound(group)) = queue.top_level_entry(action.compound_entry_id)
    else {
        return CompoundPartPlayTarget::EntryNotFound;
    };
    if group
        .parts()
        .any(|part| part.item().item_id() == action.part_item_id)
    {
        CompoundPartPlayTarget::ExactItem(action.part_item_id)
    } else {
        CompoundPartPlayTarget::PartNotInGroup
    }
}

#[cfg(test)]
mod tests;
