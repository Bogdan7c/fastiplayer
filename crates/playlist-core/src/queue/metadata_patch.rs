//! Atomic D44 metadata-only salvage/update batch.

use std::collections::HashMap;
use std::fmt;

use crate::{CachedPlaylistMetadata, LocalSourceFingerprint, PlaylistItemId, PlaylistLocator};

use super::{PlaylistQueue, QueueRevision};

/// Один verified metadata patch с source identity precondition.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistMetadataPatch {
    item_id: PlaylistItemId,
    expected_locator: PlaylistLocator,
    expected_local_fingerprint: Option<LocalSourceFingerprint>,
    refreshed_local_fingerprint: Option<LocalSourceFingerprint>,
    cached_metadata: CachedPlaylistMetadata,
}

impl PlaylistMetadataPatch {
    /// Создаёт patch; queue повторно проверит ID + locator + fingerprint.
    pub fn new(
        item_id: PlaylistItemId,
        expected_locator: PlaylistLocator,
        expected_local_fingerprint: Option<LocalSourceFingerprint>,
        cached_metadata: CachedPlaylistMetadata,
    ) -> Self {
        Self {
            item_id,
            expected_locator,
            expected_local_fingerprint,
            refreshed_local_fingerprint: expected_local_fingerprint,
            cached_metadata,
        }
    }

    /// Создаёт D31 patch, который заменяет fingerprint вместе с cache.
    pub fn refreshed_local(
        item_id: PlaylistItemId,
        expected_locator: PlaylistLocator,
        expected_local_fingerprint: Option<LocalSourceFingerprint>,
        refreshed_local_fingerprint: LocalSourceFingerprint,
        cached_metadata: CachedPlaylistMetadata,
    ) -> Self {
        Self {
            item_id,
            expected_locator,
            expected_local_fingerprint,
            refreshed_local_fingerprint: Some(refreshed_local_fingerprint),
            cached_metadata,
        }
    }
}

impl fmt::Debug for PlaylistMetadataPatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistMetadataPatch")
            .field("item_id", &self.item_id)
            .field("expected_locator", &self.expected_locator)
            .field(
                "expected_local_fingerprint",
                &self.expected_local_fingerprint,
            )
            .field(
                "refreshed_local_fingerprint",
                &self.refreshed_local_fingerprint,
            )
            .field("cached_metadata", &self.cached_metadata)
            .finish()
    }
}

/// Per-patch typed outcome в исходном batch порядке.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MetadataPatchItemOutcome {
    /// Matching metadata реально изменила cache.
    Applied { item_id: PlaylistItemId },
    /// Matching metadata уже полностью совпадала.
    NoChange { item_id: PlaylistItemId },
    /// Item удалён/очередь заменена до batch commit.
    NotFound { item_id: PlaylistItemId },
    /// ID существует, но locator или fingerprint уже другой.
    SourceMismatch { item_id: PlaylistItemId },
}

impl fmt::Debug for MetadataPatchItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied { item_id } => formatter
                .debug_struct("Applied")
                .field("item_id", item_id)
                .finish(),
            Self::NoChange { item_id } => formatter
                .debug_struct("NoChange")
                .field("item_id", item_id)
                .finish(),
            Self::NotFound { item_id } => formatter
                .debug_struct("NotFound")
                .field("item_id", item_id)
                .finish(),
            Self::SourceMismatch { item_id } => formatter
                .debug_struct("SourceMismatch")
                .field("item_id", item_id)
                .finish(),
        }
    }
}

impl fmt::Display for MetadataPatchItemOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied { item_id } => write!(formatter, "metadata {item_id} обновлена"),
            Self::NoChange { item_id } => write!(formatter, "metadata {item_id} не изменилась"),
            Self::NotFound { item_id } => write!(formatter, "{item_id} не найден"),
            Self::SourceMismatch { item_id } => {
                write!(formatter, "source precondition {item_id} не совпала")
            }
        }
    }
}

/// Итог одного atomic metadata batch commit.
#[derive(Clone, PartialEq, Eq)]
pub struct MetadataPatchBatchOutcome {
    item_outcomes: Vec<MetadataPatchItemOutcome>,
    applied_count: usize,
}

impl MetadataPatchBatchOutcome {
    /// Возвращает результаты в порядке входных patches.
    pub fn item_outcomes(&self) -> &[MetadataPatchItemOutcome] {
        &self.item_outcomes
    }

    /// Возвращает число реально изменённых patch steps.
    pub const fn applied_count(&self) -> usize {
        self.applied_count
    }

    /// Отличает реальный metadata commit от полного no-op.
    pub const fn changed_metadata(&self) -> bool {
        self.applied_count > 0
    }
}

impl fmt::Debug for MetadataPatchBatchOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetadataPatchBatchOutcome")
            .field("item_outcomes", &self.item_outcomes)
            .field("applied_count", &self.applied_count)
            .finish()
    }
}

impl fmt::Display for MetadataPatchBatchOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "metadata batch: {} результатов, {} изменений",
            self.item_outcomes.len(),
            self.applied_count
        )
    }
}

/// Ошибка whole-batch preflight без partial metadata mutation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MetadataPatchBatchError {
    /// Metadata revision fixed-width counter исчерпан.
    MetadataRevisionExhausted,
}

impl fmt::Debug for MetadataPatchBatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MetadataPatchBatchError::MetadataRevisionExhausted")
    }
}

impl fmt::Display for MetadataPatchBatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("metadata revision исчерпана")
    }
}

impl std::error::Error for MetadataPatchBatchError {}

pub(super) struct PreparedMetadataPatchPlan {
    staged_cache_by_item_id:
        HashMap<PlaylistItemId, (Option<LocalSourceFingerprint>, CachedPlaylistMetadata)>,
    outcome: MetadataPatchBatchOutcome,
}

/// Полностью проверенный metadata-only commit без оставшихся fallible шагов.
pub struct PreparedMetadataPatchBatchCommit {
    plan: PreparedMetadataPatchPlan,
    next_metadata_revision: QueueRevision,
}

impl PreparedMetadataPatchBatchCommit {
    /// Отличает persistent cache mutation от revalidated no-op batch.
    #[must_use]
    pub fn changed_metadata(&self) -> bool {
        self.plan.changed_metadata()
    }
}

impl PreparedMetadataPatchPlan {
    pub(super) fn changed_metadata(&self) -> bool {
        self.outcome.changed_metadata()
    }

    /// Применяет staged metadata напрямую к nested canonical storage.
    pub(super) fn apply_to_entries(
        self,
        entries: &mut [crate::PlaylistEntry],
    ) -> MetadataPatchBatchOutcome {
        let mut staged_cache_by_item_id = self.staged_cache_by_item_id;
        for entry in entries {
            entry.for_each_playable_item_mut(&mut |item| {
                if let Some((local_fingerprint, cached_metadata)) =
                    staged_cache_by_item_id.remove(&item.item_id())
                {
                    item.replace_local_cache(local_fingerprint, cached_metadata);
                }
            });
        }
        debug_assert!(staged_cache_by_item_id.is_empty());
        self.outcome
    }
}

pub(super) fn prepare_metadata_patch_plan<'a>(
    items: impl IntoIterator<Item = &'a crate::PlaylistItem>,
    patches: &[PlaylistMetadataPatch],
) -> PreparedMetadataPatchPlan {
    let item_by_id = items
        .into_iter()
        .map(|item| (item.item_id(), item))
        .collect::<HashMap<_, _>>();
    let mut staged_cache_by_item_id = HashMap::new();
    let mut item_outcomes = Vec::with_capacity(patches.len());
    let mut applied_count = 0usize;

    for patch in patches {
        let Some(item) = item_by_id.get(&patch.item_id).copied() else {
            item_outcomes.push(MetadataPatchItemOutcome::NotFound {
                item_id: patch.item_id,
            });
            continue;
        };
        if item.locator() != &patch.expected_locator
            || item.local_fingerprint() != patch.expected_local_fingerprint
        {
            item_outcomes.push(MetadataPatchItemOutcome::SourceMismatch {
                item_id: patch.item_id,
            });
            continue;
        }
        let effective_cache = staged_cache_by_item_id
            .get(&patch.item_id)
            .cloned()
            .unwrap_or_else(|| (item.local_fingerprint(), item.cached_metadata().clone()));
        if effective_cache.0 == patch.refreshed_local_fingerprint
            && effective_cache.1 == patch.cached_metadata
        {
            item_outcomes.push(MetadataPatchItemOutcome::NoChange {
                item_id: patch.item_id,
            });
            continue;
        }

        staged_cache_by_item_id.insert(
            patch.item_id,
            (
                patch.refreshed_local_fingerprint,
                patch.cached_metadata.clone(),
            ),
        );
        item_outcomes.push(MetadataPatchItemOutcome::Applied {
            item_id: patch.item_id,
        });
        applied_count += 1;
    }

    PreparedMetadataPatchPlan {
        staged_cache_by_item_id,
        outcome: MetadataPatchBatchOutcome {
            item_outcomes,
            applied_count,
        },
    }
}

impl PlaylistQueue {
    /// Проверяет, создаст ли matching batch реальное cache изменение.
    pub fn metadata_patch_batch_has_changes(&self, patches: &[PlaylistMetadataPatch]) -> bool {
        prepare_metadata_patch_plan(self.iter_playable_items(), patches).changed_metadata()
    }

    /// Строит весь patch plan и только затем публикует matching changes.
    ///
    /// Metadata mutation разрешена при active reservation, потому что она не
    /// меняет allocator, canonical membership/order или traversal preconditions.
    pub fn apply_metadata_patch_batch(
        &mut self,
        patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<MetadataPatchBatchOutcome, MetadataPatchBatchError> {
        let commit = self.preflight_metadata_patch_batch(patches)?;
        Ok(self.commit_metadata_patch_batch(commit))
    }

    /// Выполняет revalidation и revision preflight без metadata mutation.
    pub fn preflight_metadata_patch_batch(
        &self,
        patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<PreparedMetadataPatchBatchCommit, MetadataPatchBatchError> {
        let plan = prepare_metadata_patch_plan(self.iter_playable_items(), &patches);
        let next_metadata_revision = if plan.changed_metadata() {
            self.metadata_revision
                .checked_next()
                .ok_or(MetadataPatchBatchError::MetadataRevisionExhausted)?
        } else {
            self.metadata_revision
        };

        Ok(PreparedMetadataPatchBatchCommit {
            plan,
            next_metadata_revision,
        })
    }

    /// Применяет только что preflighted plan; owner не передаёт управление между фазами.
    pub fn commit_metadata_patch_batch(
        &mut self,
        commit: PreparedMetadataPatchBatchCommit,
    ) -> MetadataPatchBatchOutcome {
        let changed_metadata = commit.changed_metadata();
        let outcome = commit.plan.apply_to_entries(&mut self.entries);
        if changed_metadata {
            self.metadata_revision = commit.next_metadata_revision;
        }
        outcome
    }
}
