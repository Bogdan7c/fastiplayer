//! Persistence-facing exact top-level entry records.

use std::fmt;

use crate::{
    PlaylistCompoundGroup, PlaylistCompoundGroupDraft, PlaylistCompoundGroupId, PlaylistEntry,
    PlaylistItemId, RestoredPlaylistItem,
};

/// Exact compound identity с уже сохранёнными IDs.
#[derive(Clone, PartialEq, Eq)]
pub struct RestoredPlaylistCompoundGroup {
    group_id: PlaylistCompoundGroupId,
    draft: PlaylistCompoundGroupDraft,
    part_item_ids: Box<[PlaylistItemId]>,
}

impl RestoredPlaylistCompoundGroup {
    /// Связывает persisted Group/Item IDs с validated non-empty group draft.
    pub fn new(
        group_id: PlaylistCompoundGroupId,
        draft: PlaylistCompoundGroupDraft,
        part_item_ids: Vec<PlaylistItemId>,
    ) -> Result<Self, RestoredPlaylistCompoundGroupError> {
        if draft.retained_part_count() != part_item_ids.len() {
            return Err(
                RestoredPlaylistCompoundGroupError::PartIdentityCountMismatch {
                    part_count: draft.retained_part_count(),
                    identity_count: part_item_ids.len(),
                },
            );
        }
        Ok(Self {
            group_id,
            draft,
            part_item_ids: part_item_ids.into_boxed_slice(),
        })
    }

    /// Возвращает restored structural identity.
    pub const fn group_id(&self) -> PlaylistCompoundGroupId {
        self.group_id
    }

    /// Возвращает exact ordered part IDs для restore validation.
    pub fn part_item_ids(&self) -> &[PlaylistItemId] {
        &self.part_item_ids
    }

    /// Публикует committed group только после общей queue validation.
    pub(crate) fn into_group(self) -> PlaylistCompoundGroup {
        PlaylistCompoundGroup::from_draft(self.draft, self.group_id, &self.part_item_ids)
    }
}

impl fmt::Debug for RestoredPlaylistCompoundGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RestoredPlaylistCompoundGroup")
            .field("group_id", &self.group_id)
            .field("part_item_ids", &self.part_item_ids)
            .field("draft", &self.draft)
            .finish()
    }
}

/// Ошибка сборки persistence-facing compound record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoredPlaylistCompoundGroupError {
    /// DTO и group draft описывают разное число retained parts.
    PartIdentityCountMismatch {
        /// Число parts в draft.
        part_count: usize,
        /// Число exact persisted Item IDs.
        identity_count: usize,
    },
}

impl fmt::Display for RestoredPlaylistCompoundGroupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PartIdentityCountMismatch {
                part_count,
                identity_count,
            } => write!(
                formatter,
                "restored compound has {part_count} parts but {identity_count} part identities"
            ),
        }
    }
}

impl std::error::Error for RestoredPlaylistCompoundGroupError {}

/// Persistence-facing top-level entry без serde/I/O dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoredPlaylistEntry {
    /// Exact restored standalone item.
    Single(RestoredPlaylistItem),
    /// Exact restored compound group.
    Compound(RestoredPlaylistCompoundGroup),
}

impl RestoredPlaylistEntry {
    /// Возвращает retained Item ID demand для capacity validation.
    pub fn retained_item_count(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Compound(group) => group.part_item_ids.len(),
        }
    }

    /// Добавляет exact playable IDs в общий restore validation set.
    pub(crate) fn extend_item_ids(&self, item_ids: &mut Vec<PlaylistItemId>) {
        match self {
            Self::Single(item) => item_ids.push(item.item_id()),
            Self::Compound(group) => item_ids.extend_from_slice(group.part_item_ids()),
        }
    }

    /// Возвращает optional restored structural Group ID.
    pub const fn compound_group_id(&self) -> Option<PlaylistCompoundGroupId> {
        match self {
            Self::Single(_) => None,
            Self::Compound(group) => Some(group.group_id()),
        }
    }

    /// Публикует canonical entry после общей queue validation.
    pub(crate) fn into_entry(self) -> PlaylistEntry {
        match self {
            Self::Single(item) => PlaylistEntry::Single(item.into_item()),
            Self::Compound(group) => PlaylistEntry::Compound(Box::new(group.into_group())),
        }
    }
}
