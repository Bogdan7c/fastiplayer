//! Process-lifetime compound presentation model без зависимости от egui renderer.
//!
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use playlist_core::{
    PlaylistCompoundGroupId, PlaylistCompoundPartOrdinal, PlaylistEntry, PlaylistEntryId,
    PlaylistItemId, PlaylistQueue,
};

use super::identity::{ActiveMediaIdentity, PendingTarget, PlaylistItemRuntimeError};
use super::selection::PlaylistSelectionSnapshot;
use super::view::{PlaylistStructuralRevision, PlaylistVisibleRow, PlaylistVisibleRowState};

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
        retained_part_count: usize,
        active: bool,
    },
}

/// Положение child внутри визуально цельной group geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompoundPartPosition {
    /// Единственная retained part одновременно начинает и завершает connector.
    Only,
    /// Первая часть многочастной группы.
    First,
    /// Промежуточная часть многочастной группы.
    Middle,
    /// Последняя часть завершает вертикальный connector.
    Last,
}

/// Одна visible projection связывает proven identity с immutable presentation.
#[derive(Debug, Clone)]
pub(crate) struct CompoundRuntimeVisibleRow {
    row: CompoundRuntimeRow,
    presentation: PlaylistVisibleRow,
    part_position: Option<CompoundPartPosition>,
}

impl CompoundRuntimeVisibleRow {
    /// Identity/ownership shape остаётся отделённой от display metadata.
    pub(crate) const fn row(&self) -> CompoundRuntimeRow {
        self.row
    }

    /// Общая presentation vocabulary переиспользует обычные row helpers.
    pub(crate) const fn presentation(&self) -> &PlaylistVisibleRow {
        &self.presentation
    }

    /// Geometry position существует только у subordinate part.
    pub(crate) const fn part_position(&self) -> Option<CompoundPartPosition> {
        self.part_position
    }
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
    #[cfg(test)]
    pub(crate) const fn is_subordinate_part(self) -> bool {
        matches!(self, Self::CompoundPart { .. })
    }
}

/// Pair identity меняется только когда геометрия visible list действительно могла измениться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistLayoutIdentity {
    structural_revision: u64,
    disclosure_revision: u64,
}

impl PlaylistLayoutIdentity {
    /// Named constructor не заставляет UI упаковывать две независимые revisions в число.
    pub(crate) const fn from_parts(
        structural_revision: PlaylistStructuralRevision,
        disclosure_revision: u64,
    ) -> Self {
        Self {
            structural_revision: structural_revision.get(),
            disclosure_revision,
        }
    }

    /// Animation unit tests не зависят от controller allocator-а revisions.
    #[cfg(test)]
    pub(crate) const fn for_test(structural_revision: u64) -> Self {
        Self {
            structural_revision,
            disclosure_revision: 0,
        }
    }
}

/// Immutable snapshot отделяет visible count от domain capacity/count.
#[derive(Debug, Clone)]
pub(crate) struct CompoundRuntimeViewSnapshot {
    structural_revision: PlaylistStructuralRevision,
    layout_identity: PlaylistLayoutIdentity,
    rows: Arc<[CompoundRuntimeVisibleRow]>,
    top_level_entry_count: usize,
    header_indices: Arc<HashMap<PlaylistEntryId, usize>>,
    part_indices: Arc<HashMap<PlaylistItemId, usize>>,
    entry_id_by_item: Arc<HashMap<PlaylistItemId, PlaylistEntryId>>,
    structural_slot_by_visible_slot: Arc<[usize]>,
}

impl CompoundRuntimeViewSnapshot {
    /// Pending runtime публикует согласованный пустой snapshot без fake controller owner-а.
    pub(super) fn empty() -> Self {
        Self {
            structural_revision: PlaylistStructuralRevision::INITIAL,
            layout_identity: PlaylistLayoutIdentity::from_parts(
                PlaylistStructuralRevision::INITIAL,
                0,
            ),
            rows: Arc::from([]),
            top_level_entry_count: 0,
            header_indices: Arc::new(HashMap::new()),
            part_indices: Arc::new(HashMap::new()),
            entry_id_by_item: Arc::new(HashMap::new()),
            structural_slot_by_visible_slot: Arc::from([0]),
        }
    }

    /// Snapshot generation fence обязателен для всех row actions.
    pub(crate) const fn structural_revision(&self) -> PlaylistStructuralRevision {
        self.structural_revision
    }

    /// Renderer invalidation не реагирует на active/error-only presentation revisions.
    pub(crate) const fn layout_identity(&self) -> PlaylistLayoutIdentity {
        self.layout_identity
    }

    /// Domain top-level count не зависит от disclosure state.
    #[cfg(test)]
    pub(crate) const fn top_level_entry_count(&self) -> usize {
        self.top_level_entry_count
    }

    /// UI visible count включает только раскрытые child projections.
    pub(crate) fn visible_row_count(&self) -> usize {
        self.rows.len()
    }

    /// Bounded virtualization slice возвращает owned Copy projections.
    #[cfg(test)]
    pub(crate) fn visible_rows(&self, range: std::ops::Range<usize>) -> Vec<CompoundRuntimeRow> {
        let start = range.start.min(self.rows.len());
        let end = range.end.min(self.rows.len()).max(start);
        self.rows[start..end]
            .iter()
            .map(CompoundRuntimeVisibleRow::row)
            .collect()
    }

    /// S17V получает metadata только для bounded visible range.
    pub(crate) fn visible_presented_rows(
        &self,
        range: std::ops::Range<usize>,
    ) -> Vec<CompoundRuntimeVisibleRow> {
        let start = range.start.min(self.rows.len());
        let end = range.end.min(self.rows.len()).max(start);
        self.rows[start..end].to_vec()
    }

    /// Stable row lookup обслуживает focus и Current Item без renderer scan-а.
    pub(crate) fn row_index(&self, row_id: CompoundRuntimeRowId) -> Option<usize> {
        match row_id {
            CompoundRuntimeRowId::Entry(entry_id) => self.header_indices.get(&entry_id).copied(),
            CompoundRuntimeRowId::Part { part_item_id, .. } => {
                self.part_indices.get(&part_item_id).copied()
            }
        }
    }

    /// Visible navigation читает ровно одну projection по индексу.
    pub(crate) fn row_id_at(&self, row_index: usize) -> Option<CompoundRuntimeRowId> {
        self.rows.get(row_index).map(|row| row.row().row_id())
    }

    /// Exact active part указывает на child только пока group раскрыта.
    pub(crate) fn active_row_index(&self, active_item_id: PlaylistItemId) -> Option<usize> {
        self.part_indices.get(&active_item_id).copied().or_else(|| {
            let entry_id = self.entry_id_by_item.get(&active_item_id)?;
            self.header_indices.get(entry_id).copied()
        })
    }

    /// Visible drag slot переводится в atomic top-level slot и не разрезает group.
    pub(crate) fn structural_insertion_slot(&self, visible_slot: usize) -> usize {
        self.structural_slot_by_visible_slot
            .get(visible_slot.min(self.rows.len()))
            .copied()
            .unwrap_or(self.top_level_entry_count)
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
    #[cfg(test)]
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
    disclosure_revision: u64,
}

/// Именованный owner-state сохраняет различия active, pending, error и selection.
pub(super) struct CompoundRuntimeProjectionState<'a> {
    pub(super) structural_revision: PlaylistStructuralRevision,
    pub(super) active_media: Option<ActiveMediaIdentity>,
    pub(super) pending_target: Option<PendingTarget>,
    pub(super) runtime_errors: &'a HashMap<PlaylistItemId, PlaylistItemRuntimeError>,
    pub(super) selection: &'a PlaylistSelectionSnapshot,
}

impl CompoundRuntimeViewState {
    /// Удаляет disclosure для групп, которых больше нет в committed queue.
    pub(super) fn retain_committed_groups(&mut self, queue: &PlaylistQueue) {
        let previous_count = self.expanded_group_ids.len();
        self.expanded_group_ids.retain(|group_id| {
            queue
                .top_level_entry(PlaylistEntryId::Compound(*group_id))
                .is_some()
        });
        if self.expanded_group_ids.len() != previous_count {
            self.disclosure_revision = self.disclosure_revision.saturating_add(1);
        }
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
        let outcome = if self.expanded_group_ids.remove(&group_id) {
            ToggleCompoundDisclosureOutcome::Collapsed
        } else {
            self.expanded_group_ids.insert(group_id);
            ToggleCompoundDisclosureOutcome::Expanded
        };
        self.disclosure_revision = self.disclosure_revision.saturating_add(1);
        outcome
    }

    /// Строит renderer-neutral rows из authoritative queue/current/active/selection.
    pub(super) fn snapshot(
        &self,
        queue: &PlaylistQueue,
        state: CompoundRuntimeProjectionState<'_>,
    ) -> CompoundRuntimeViewSnapshot {
        let active_item_id = state.active_media.and_then(ActiveMediaIdentity::item_id);
        let pending_item_id = state.pending_target.and_then(PendingTarget::item_id);
        let current_item_id = queue.traversal_current().map(|current| current.item_id());
        let mut rows =
            Vec::with_capacity(queue.top_level_entry_count() + self.expanded_group_ids.len());
        let mut header_indices = HashMap::with_capacity(queue.top_level_entry_count());
        let mut part_indices = HashMap::new();
        let mut entry_id_by_item = HashMap::with_capacity(queue.retained_item_count());
        let mut structural_slot_by_visible_slot =
            Vec::with_capacity(queue.top_level_entry_count() + self.expanded_group_ids.len() + 1);
        structural_slot_by_visible_slot.push(0);

        for (top_level_index, entry) in queue.iter_top_level_entries().enumerate() {
            let entry_id = entry.entry_id();
            header_indices.insert(entry_id, rows.len());
            match entry {
                PlaylistEntry::Single(item) => {
                    let item_id = item.item_id();
                    let active = active_item_id == Some(item_id);
                    let selected = state.selection.is_selected(entry_id);
                    entry_id_by_item.insert(item_id, entry_id);
                    rows.push(CompoundRuntimeVisibleRow {
                        row: CompoundRuntimeRow::Single {
                            entry_id,
                            item_id,
                            active,
                            selected,
                        },
                        presentation: PlaylistVisibleRow::from_cached_metadata(
                            entry_id,
                            item_id,
                            item.cached_metadata(),
                            PlaylistVisibleRowState {
                                active,
                                pending: pending_item_id == Some(item_id),
                                selected,
                                runtime_error: state.runtime_errors.get(&item_id).cloned(),
                            },
                        ),
                        part_position: None,
                    });
                    structural_slot_by_visible_slot.push(top_level_index + 1);
                }
                PlaylistEntry::Compound(group) => {
                    entry_id_by_item
                        .extend(group.parts().map(|part| (part.item().item_id(), entry_id)));
                    let expanded = self.expanded_group_ids.contains(&group.group_id());
                    let active_part_item_id = group
                        .parts()
                        .map(|part| part.item().item_id())
                        .find(|part_item_id| Some(*part_item_id) == active_item_id);
                    let pending_part_item_id = group
                        .parts()
                        .map(|part| part.item().item_id())
                        .find(|part_item_id| Some(*part_item_id) == pending_item_id);
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
                    let header_play_item_id = current_part_item_id.unwrap_or(first_part_item_id);
                    let selected = state.selection.is_selected(entry_id);
                    rows.push(CompoundRuntimeVisibleRow {
                        row: CompoundRuntimeRow::CompoundHeader {
                            entry_id,
                            group_id: group.group_id(),
                            retained_part_count: group.retained_part_count(),
                            expanded,
                            active_part_item_id,
                            header_play_item_id,
                            selected,
                        },
                        presentation: PlaylistVisibleRow::from_cached_metadata(
                            entry_id,
                            header_play_item_id,
                            group.cached_summary(),
                            PlaylistVisibleRowState {
                                active: active_part_item_id.is_some(),
                                pending: pending_part_item_id.is_some(),
                                selected,
                                runtime_error: state
                                    .runtime_errors
                                    .get(&header_play_item_id)
                                    .cloned(),
                            },
                        ),
                        part_position: None,
                    });
                    structural_slot_by_visible_slot.push(top_level_index + 1);
                    if expanded {
                        let retained_part_count = group.retained_part_count();
                        for (part_index, part) in group.parts().enumerate() {
                            let part_item_id = part.item().item_id();
                            let active = active_item_id == Some(part_item_id);
                            let part_position = if retained_part_count == 1 {
                                CompoundPartPosition::Only
                            } else if part_index == 0 {
                                CompoundPartPosition::First
                            } else if part_index + 1 == retained_part_count {
                                CompoundPartPosition::Last
                            } else {
                                CompoundPartPosition::Middle
                            };
                            part_indices.insert(part_item_id, rows.len());
                            rows.push(CompoundRuntimeVisibleRow {
                                row: CompoundRuntimeRow::CompoundPart {
                                    compound_entry_id: entry_id,
                                    part_item_id,
                                    ordinal: part.membership().ordinal(),
                                    retained_part_count,
                                    active,
                                },
                                presentation: PlaylistVisibleRow::from_cached_metadata(
                                    entry_id,
                                    part_item_id,
                                    part.item().cached_metadata(),
                                    PlaylistVisibleRowState {
                                        active,
                                        pending: pending_item_id == Some(part_item_id),
                                        selected: false,
                                        runtime_error: state
                                            .runtime_errors
                                            .get(&part_item_id)
                                            .cloned(),
                                    },
                                ),
                                part_position: Some(part_position),
                            });
                            structural_slot_by_visible_slot.push(top_level_index + 1);
                        }
                    }
                }
            }
        }

        CompoundRuntimeViewSnapshot {
            structural_revision: state.structural_revision,
            layout_identity: PlaylistLayoutIdentity::from_parts(
                state.structural_revision,
                self.disclosure_revision,
            ),
            rows: rows.into(),
            top_level_entry_count: queue.top_level_entry_count(),
            header_indices: Arc::new(header_indices),
            part_indices: Arc::new(part_indices),
            entry_id_by_item: Arc::new(entry_id_by_item),
            structural_slot_by_visible_slot: structural_slot_by_visible_slot.into(),
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
