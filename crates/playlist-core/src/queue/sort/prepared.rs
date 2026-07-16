//! Two-phase background preparation и guarded atomic Sort commit.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::super::metadata_patch::prepare_metadata_patch_plan;
use super::super::{MetadataPatchBatchOutcome, PlaylistMetadataPatch, QueueRevisionSnapshot};
use super::*;

/// Cheap immutable handoff в background: тяжёлые locator/metadata payload остаются Arc-shared.
#[derive(Clone)]
pub struct CanonicalSortSnapshot {
    items: Arc<[PlaylistItem]>,
    expected_revision: QueueRevisionSnapshot,
}

impl CanonicalSortSnapshot {
    /// Число строк без раскрытия mutable queue storage.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Подготавливает keys/permutation вне owner thread и проверяет cancel между chunks.
    pub fn prepare(
        self,
        metadata_patches: &[PlaylistMetadataPatch],
        intent: SortCanonicalQueue,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<PreparedCanonicalSort, CanonicalSortPreparationCancelled> {
        if is_cancelled() {
            return Err(CanonicalSortPreparationCancelled);
        }
        let expected_item_ids = self
            .items
            .iter()
            .map(PlaylistItem::item_id)
            .collect::<Vec<_>>();
        let mut effective_items = self.items.to_vec();
        prepare_metadata_patch_plan(&effective_items, metadata_patches).apply(&mut effective_items);

        let mut prepared_items = 0usize;
        let entries = prepare_sort_entries_cancellable(
            &effective_items,
            intent.key,
            &mut is_cancelled,
            &mut prepared_items,
        )?;
        let mut comparisons = 0usize;
        let sorted_entry_indices = stable_sorted_entry_indices(
            &entries,
            intent.direction,
            &mut is_cancelled,
            &mut comparisons,
        )?;
        let sorted_item_ids = sorted_entry_indices
            .iter()
            .map(|entry_index| effective_items[entries[*entry_index].original_index].item_id())
            .collect::<Vec<_>>();

        Ok(PreparedCanonicalSort {
            expected_revision: self.expected_revision,
            expected_item_ids: expected_item_ids.into_boxed_slice(),
            sorted_item_ids: sorted_item_ids.into_boxed_slice(),
            statistics: CanonicalSortPreparationStatistics {
                prepared_items,
                comparisons,
                shared_item_handles: effective_items.len(),
            },
        })
    }
}

/// Pure background plan без ссылок на live queue.
pub struct PreparedCanonicalSort {
    expected_revision: QueueRevisionSnapshot,
    expected_item_ids: Box<[PlaylistItemId]>,
    sorted_item_ids: Box<[PlaylistItemId]>,
    statistics: CanonicalSortPreparationStatistics,
}

impl PreparedCanonicalSort {
    /// Non-timing operation/memory characterization для diagnostics/tests.
    #[must_use]
    pub const fn statistics(&self) -> CanonicalSortPreparationStatistics {
        self.statistics
    }
}

/// Bounded accounting одного prepare pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalSortPreparationStatistics {
    /// Keys построены ровно по одному разу на item.
    pub prepared_items: usize,
    /// Comparator calls остаются O(N log N).
    pub comparisons: usize,
    /// Snapshot удерживает только cloned Arc-backed item handles.
    pub shared_item_handles: usize,
}

/// Cancellation не публикует partial permutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalSortPreparationCancelled;

/// Одно atomic domain применение matching metadata и prepared order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyPreparedCanonicalSortOutcome {
    metadata: MetadataPatchBatchOutcome,
    reordered: bool,
}

impl ApplyPreparedCanonicalSortOutcome {
    #[must_use]
    pub const fn metadata(&self) -> &MetadataPatchBatchOutcome {
        &self.metadata
    }

    #[must_use]
    pub const fn reordered(&self) -> bool {
        self.reordered
    }

    #[must_use]
    pub const fn changed_persistent_state(&self) -> bool {
        self.reordered || self.metadata.changed_metadata()
    }
}

/// Preflight error гарантирует отсутствие partial mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPreparedCanonicalSortError {
    /// Membership/order либо metadata изменились после snapshot.
    StaleQueueRevision,
    /// D08 reservation блокирует фактический reorder.
    InstallCommitLinearizing,
    /// Background result повреждён или принадлежит другой queue.
    InvalidPermutation,
    /// Monotonic structural revision исчерпана.
    StructuralRevisionExhausted,
    /// Monotonic metadata revision исчерпана.
    MetadataRevisionExhausted,
}

/// Полностью проверенный linear commit: после выдачи owner применяет его без fallible шага.
pub struct PreparedCanonicalSortCommit {
    next_items: Vec<PlaylistItem>,
    next_structural_revision: super::super::QueueRevision,
    next_metadata_revision: super::super::QueueRevision,
    outcome: ApplyPreparedCanonicalSortOutcome,
}

impl PreparedCanonicalSortCommit {
    #[must_use]
    pub const fn changed_persistent_state(&self) -> bool {
        self.outcome.changed_persistent_state()
    }

    #[must_use]
    pub const fn reordered(&self) -> bool {
        self.outcome.reordered()
    }
}

impl PlaylistQueue {
    /// Создаёт immutable Arc-sharing snapshot для background preparation.
    #[must_use]
    pub fn canonical_sort_snapshot(&self) -> CanonicalSortSnapshot {
        CanonicalSortSnapshot {
            items: Arc::from(self.items.clone()),
            expected_revision: self.revision_snapshot(),
        }
    }

    /// Revalidates и атомарно публикует metadata+order без промежуточного view state.
    pub fn apply_prepared_canonical_sort(
        &mut self,
        prepared: PreparedCanonicalSort,
        metadata_patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<ApplyPreparedCanonicalSortOutcome, ApplyPreparedCanonicalSortError> {
        let commit = self.preflight_prepared_canonical_sort(prepared, metadata_patches)?;
        Ok(self.commit_prepared_canonical_sort(commit))
    }

    /// Выполняет все fallible проверки и строит linear infallible commit.
    pub fn preflight_prepared_canonical_sort(
        &self,
        prepared: PreparedCanonicalSort,
        metadata_patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<PreparedCanonicalSortCommit, ApplyPreparedCanonicalSortError> {
        let current_revision = self.revision_snapshot();
        if current_revision.structural() != prepared.expected_revision.structural()
            || current_revision.metadata() != prepared.expected_revision.metadata()
        {
            return Err(ApplyPreparedCanonicalSortError::StaleQueueRevision);
        }
        let current_item_ids = self
            .items
            .iter()
            .map(PlaylistItem::item_id)
            .collect::<Vec<_>>();
        if current_item_ids.as_slice() != prepared.expected_item_ids.as_ref() {
            return Err(ApplyPreparedCanonicalSortError::StaleQueueRevision);
        }
        let unique_sorted_ids = prepared
            .sorted_item_ids
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if prepared.sorted_item_ids.len() != self.items.len()
            || unique_sorted_ids.len() != self.items.len()
            || !current_item_ids
                .iter()
                .all(|item_id| unique_sorted_ids.contains(item_id))
        {
            return Err(ApplyPreparedCanonicalSortError::InvalidPermutation);
        }
        let reordered = current_item_ids.as_slice() != prepared.sorted_item_ids.as_ref();
        if reordered && self.active_reservation.is_some() {
            return Err(ApplyPreparedCanonicalSortError::InstallCommitLinearizing);
        }

        let metadata_plan = prepare_metadata_patch_plan(&self.items, &metadata_patches);
        let next_metadata_revision = if metadata_plan.changed_metadata() {
            self.metadata_revision
                .checked_next()
                .ok_or(ApplyPreparedCanonicalSortError::MetadataRevisionExhausted)?
        } else {
            self.metadata_revision
        };
        let next_structural_revision = if reordered {
            self.structural_revision
                .checked_next()
                .ok_or(ApplyPreparedCanonicalSortError::StructuralRevisionExhausted)?
        } else {
            self.structural_revision
        };

        let mut next_items = self.items.clone();
        let metadata = metadata_plan.apply(&mut next_items);
        if reordered {
            let index_by_id = next_items
                .iter()
                .enumerate()
                .map(|(index, item)| (item.item_id(), index))
                .collect::<HashMap<_, _>>();
            let sorted_original_indices = prepared
                .sorted_item_ids
                .iter()
                .map(|item_id| index_by_id[item_id])
                .collect::<Vec<_>>();
            apply_prepared_order(&mut next_items, &sorted_original_indices);
        }

        Ok(PreparedCanonicalSortCommit {
            next_items,
            next_structural_revision,
            next_metadata_revision,
            outcome: ApplyPreparedCanonicalSortOutcome {
                metadata,
                reordered,
            },
        })
    }

    /// Публикует preflighted state; между preflight/commit queue owner не передаёт управление.
    pub fn commit_prepared_canonical_sort(
        &mut self,
        commit: PreparedCanonicalSortCommit,
    ) -> ApplyPreparedCanonicalSortOutcome {
        self.items = commit.next_items;
        if commit.outcome.metadata.changed_metadata() {
            self.metadata_revision = commit.next_metadata_revision;
        }
        if commit.outcome.reordered {
            self.structural_revision = commit.next_structural_revision;
        }
        commit.outcome
    }
}

fn prepare_sort_entries_cancellable(
    items: &[PlaylistItem],
    sort_key: PlaylistSortKey,
    is_cancelled: &mut impl FnMut() -> bool,
    prepared_items: &mut usize,
) -> Result<Vec<PreparedSortEntry>, CanonicalSortPreparationCancelled> {
    let mut entries = Vec::with_capacity(items.len());
    for (original_index, item) in items.iter().enumerate() {
        if is_cancelled() {
            return Err(CanonicalSortPreparationCancelled);
        }
        entries.push(PreparedSortEntry {
            original_index,
            primary: prepare_primary_key(item.cached_metadata(), sort_key),
            natural_fallback: prepare_natural_sort_key(item),
        });
        *prepared_items += 1;
    }
    Ok(entries)
}

fn stable_sorted_entry_indices(
    entries: &[PreparedSortEntry],
    direction: SortDirection,
    is_cancelled: &mut impl FnMut() -> bool,
    comparisons: &mut usize,
) -> Result<Vec<usize>, CanonicalSortPreparationCancelled> {
    let item_count = entries.len();
    let mut current = (0..item_count).collect::<Vec<_>>();
    let mut merged = vec![0usize; item_count];
    let mut width = 1usize;
    while width < item_count {
        let mut start = 0usize;
        while start < item_count {
            if is_cancelled() {
                return Err(CanonicalSortPreparationCancelled);
            }
            let middle = start.saturating_add(width).min(item_count);
            let end = middle.saturating_add(width).min(item_count);
            let (mut left, mut right, mut output) = (start, middle, start);
            while left < middle && right < end {
                if is_cancelled() {
                    return Err(CanonicalSortPreparationCancelled);
                }
                *comparisons += 1;
                if compare_entries(&entries[current[left]], &entries[current[right]], direction)
                    != Ordering::Greater
                {
                    merged[output] = current[left];
                    left += 1;
                } else {
                    merged[output] = current[right];
                    right += 1;
                }
                output += 1;
            }
            while left < middle {
                merged[output] = current[left];
                left += 1;
                output += 1;
            }
            while right < end {
                merged[output] = current[right];
                right += 1;
                output += 1;
            }
            start = end;
        }
        std::mem::swap(&mut current, &mut merged);
        width = width.saturating_mul(2);
    }
    Ok(current)
}
