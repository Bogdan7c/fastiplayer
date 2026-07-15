//! Atomic D44 metadata-only salvage/update batch.

use std::collections::HashMap;
use std::fmt;

use crate::{CachedPlaylistMetadata, LocalSourceFingerprint, PlaylistItemId, PlaylistLocator};

use super::PlaylistQueue;

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

impl PlaylistQueue {
    /// Проверяет, создаст ли matching batch реальное cache изменение.
    pub fn metadata_patch_batch_has_changes(&self, patches: &[PlaylistMetadataPatch]) -> bool {
        patches.iter().any(|patch| {
            self.item(patch.item_id).is_some_and(|item| {
                item.locator() == &patch.expected_locator
                    && item.local_fingerprint() == patch.expected_local_fingerprint
                    && (item.local_fingerprint() != patch.refreshed_local_fingerprint
                        || item.cached_metadata() != &patch.cached_metadata)
            })
        })
    }

    /// Строит весь patch plan и только затем публикует matching changes.
    ///
    /// Metadata mutation разрешена при active reservation, потому что она не
    /// меняет allocator, canonical membership/order или traversal preconditions.
    pub fn apply_metadata_patch_batch(
        &mut self,
        patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<MetadataPatchBatchOutcome, MetadataPatchBatchError> {
        let mut staged_cache_by_index = HashMap::new();
        let mut item_outcomes = Vec::with_capacity(patches.len());
        let mut applied_count = 0usize;

        for patch in patches {
            let Some(item_index) = self.index_of(patch.item_id) else {
                item_outcomes.push(MetadataPatchItemOutcome::NotFound {
                    item_id: patch.item_id,
                });
                continue;
            };
            let item = &self.items[item_index];
            if item.locator() != &patch.expected_locator
                || item.local_fingerprint() != patch.expected_local_fingerprint
            {
                item_outcomes.push(MetadataPatchItemOutcome::SourceMismatch {
                    item_id: patch.item_id,
                });
                continue;
            }
            let effective_cache = staged_cache_by_index
                .get(&item_index)
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

            staged_cache_by_index.insert(
                item_index,
                (patch.refreshed_local_fingerprint, patch.cached_metadata),
            );
            item_outcomes.push(MetadataPatchItemOutcome::Applied {
                item_id: patch.item_id,
            });
            applied_count += 1;
        }

        let next_metadata_revision = if staged_cache_by_index.is_empty() {
            None
        } else {
            Some(
                self.metadata_revision
                    .checked_next()
                    .ok_or(MetadataPatchBatchError::MetadataRevisionExhausted)?,
            )
        };

        for (item_index, (local_fingerprint, cached_metadata)) in staged_cache_by_index {
            self.items[item_index].replace_local_cache(local_fingerprint, cached_metadata);
        }
        if let Some(next_revision) = next_metadata_revision {
            self.metadata_revision = next_revision;
        }

        Ok(MetadataPatchBatchOutcome {
            item_outcomes,
            applied_count,
        })
    }
}
