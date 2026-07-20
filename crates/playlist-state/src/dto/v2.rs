//! Строгий playlist-state schema v2 и explicit DTO/domain mapping.

mod payload;

use playlist_core::{
    MAX_PLAYLIST_ITEMS, MAX_SHUFFLE_HISTORY_ENTRIES, NextPlaylistCompoundGroupId,
    NextPlaylistItemId, PlaylistCompoundGroupDraft, PlaylistCompoundGroupId,
    PlaylistCompoundMembership, PlaylistEntry, PlaylistEntryId, PlaylistItem, PlaylistItemDraft,
    PlaylistItemId, PlaylistQueue, PlaylistQueueRestore, RestoredPlaylistCompoundGroup,
    RestoredPlaylistEntry, RestoredPlaylistItem, ShuffleHistoryCursor, ShuffleQueueRestoreError,
    ShuffleTraversalSnapshot,
};
use serde::{Deserialize, Serialize};

use super::{
    CachedMetadataV1Dto, DtoLoadError, LocalPathV1Dto, Nullable, PlaylistItemV1Dto,
    PlaylistLocatorV1Dto, RepeatModeV1Dto, StateSerializationError,
};
use crate::CURRENT_PLAYLIST_STATE_SCHEMA_VERSION;
use crate::types::{LoadedPlaylistState, PlaylistStateSnapshot};

/// Строгий top-level DTO schema v2.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaylistStateV2Dto {
    schema_version: u64,
    next_item_id: u64,
    next_compound_group_id: u64,
    entries: Vec<PlaylistEntryV2Dto>,
    current_item_id: Nullable<u64>,
    repeat_mode: RepeatModeV1Dto,
    shuffle_enabled: bool,
    shuffle_history: Vec<u64>,
    shuffle_history_cursor: Nullable<u64>,
    shuffle_upcoming: Vec<PlaylistEntryIdV2Dto>,
}

/// Top-level structural entry сохраняет Single/Compound identity явно.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PlaylistEntryV2Dto {
    Single {
        item: PlaylistItemV2Dto,
    },
    Compound {
        group_id: u64,
        provenance_locator: PlaylistLocatorV1Dto,
        cached_summary: CachedMetadataV1Dto,
        durable_payload: Nullable<PlaylistCompoundDurablePayloadV2Dto>,
        parts: Vec<PlaylistCompoundPartV2Dto>,
    },
}

/// Playable item v2 расширяет exact legacy locator/cache durable payload-ом.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistItemV2Dto {
    item: PlaylistItemV1Dto,
    durable_payload: Nullable<PlaylistSingleDurablePayloadV2Dto>,
}

/// Part хранит redundant membership, чтобы corruption не меняла ownership молча.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistCompoundPartV2Dto {
    membership: PlaylistCompoundMembershipV2Dto,
    item: PlaylistItemV2Dto,
}

/// Exact persisted compound membership.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistCompoundMembershipV2Dto {
    group_id: u64,
    ordinal: u32,
}

/// Shuffle upcoming использует top-level Entry ID, а не ambiguous u64.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PlaylistEntryIdV2Dto {
    Single { item_id: u64 },
    Compound { group_id: u64 },
}

/// Durable single payload содержит только reopen-safe neutral values.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistSingleDurablePayloadV2Dto {
    reopen_locator: DurableReopenLocatorV2Dto,
    playback_span: Nullable<PlaylistPlaybackSpanV2Dto>,
    ancillary_track_hints: Vec<PlaylistAncillaryTrackHintV2Dto>,
    provenance: PlaylistImportProvenanceV2Dto,
    availability: PlaylistImportAvailabilityV2Dto,
}

/// Durable group payload не дублирует parts либо cached summary.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistCompoundDurablePayloadV2Dto {
    reopen_locator: DurableReopenLocatorV2Dto,
    provenance: PlaylistImportProvenanceV2Dto,
}

/// Closed durable locator enum: request/auth/endpoint fields здесь отсутствуют.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DurableReopenLocatorV2Dto {
    Local {
        path: LocalPathV1Dto,
    },
    Url {
        reopenable_url: String,
    },
    Service {
        service_owner: String,
        payload_version: u16,
        material_kind: StableServiceMaterialKindV2Dto,
        payload_bytes: Vec<u8>,
    },
}

/// DTO допускает только три stable service identity classes.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StableServiceMaterialKindV2Dto {
    #[serde(rename = "webpage_identity")]
    Webpage,
    #[serde(rename = "original_identity")]
    Original,
    #[serde(rename = "extractor_identity")]
    Extractor,
}

/// Exact absolute playback span без floating-point времени.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistPlaybackSpanV2Dto {
    start: MediaTimeV2Dto,
    end_exclusive: Nullable<MediaTimeV2Dto>,
}

/// Exact media time representation.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaTimeV2Dto {
    seconds: u64,
    subsec_nanos: u32,
}

/// Ancillary hint с structurally durable origin.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistAncillaryTrackHintV2Dto {
    semantic_identity: String,
    language: Nullable<String>,
    display_name: Nullable<String>,
    selection_kind: PlaylistAncillaryTrackSelectionKindV2Dto,
    origin: PlaylistAncillaryTrackOriginV2Dto,
    service_format_identity: Nullable<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaylistAncillaryTrackSelectionKindV2Dto {
    Manual,
    Automatic,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PlaylistAncillaryTrackOriginV2Dto {
    Embedded,
    External {
        reopen_locator: DurableReopenLocatorV2Dto,
    },
}

/// Durable import provenance без свободного child identity поля.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistImportProvenanceV2Dto {
    root_locator: DurableReopenLocatorV2Dto,
    source_kind: PlaylistImportSourceKindV2Dto,
    source_ordinal: Nullable<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaylistImportSourceKindV2Dto {
    M3u,
    M3u8,
    Xspf,
    Cue,
    Service,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaylistImportAvailabilityV2Dto {
    Available,
    Unavailable,
}

pub(super) fn deserialize(inspected_bytes: &[u8]) -> Result<LoadedPlaylistState, DtoLoadError> {
    let dto: PlaylistStateV2Dto =
        serde_json::from_slice(inspected_bytes).map_err(|_| DtoLoadError::InvalidPayload)?;
    dto.validate_resource_limits()?;
    dto.into_domain()
}

impl PlaylistStateV2Dto {
    pub(super) fn from_domain(
        snapshot: PlaylistStateSnapshot<'_>,
    ) -> Result<Self, StateSerializationError> {
        let queue = snapshot.queue();
        let entries = queue
            .iter_top_level_entries()
            .map(PlaylistEntryV2Dto::from_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let shuffle_snapshot = queue.shuffle_traversal_snapshot();
        let (shuffle_enabled, shuffle_history, shuffle_history_cursor, shuffle_upcoming) =
            match shuffle_snapshot {
                Some(traversal) => (
                    true,
                    traversal
                        .history()
                        .iter()
                        .map(|item_id| item_id.expose_value_for_persistence())
                        .collect(),
                    Nullable(
                        traversal
                            .history_cursor()
                            .map(|cursor| cursor.index() as u64),
                    ),
                    traversal
                        .upcoming()
                        .iter()
                        .copied()
                        .map(PlaylistEntryIdV2Dto::from_domain)
                        .collect(),
                ),
                None => (false, Vec::new(), Nullable(None), Vec::new()),
            };

        Ok(Self {
            schema_version: CURRENT_PLAYLIST_STATE_SCHEMA_VERSION,
            next_item_id: queue.next_item_id_snapshot().expose_value_for_persistence(),
            next_compound_group_id: queue
                .next_compound_group_id_snapshot()
                .expose_value_for_persistence(),
            entries,
            current_item_id: Nullable(
                queue
                    .traversal_current()
                    .map(|current| current.item_id().expose_value_for_persistence()),
            ),
            repeat_mode: snapshot.repeat_mode().into(),
            shuffle_enabled,
            shuffle_history,
            shuffle_history_cursor,
            shuffle_upcoming,
        })
    }

    pub(super) fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        if self.entries.len() > MAX_PLAYLIST_ITEMS
            || self.shuffle_history.len() > MAX_SHUFFLE_HISTORY_ENTRIES
            || self.shuffle_upcoming.len() > MAX_PLAYLIST_ITEMS
        {
            return Err(DtoLoadError::ResourceLimit);
        }

        let mut retained_item_count = 0usize;
        for entry in &self.entries {
            retained_item_count = retained_item_count
                .checked_add(entry.retained_item_count())
                .ok_or(DtoLoadError::ResourceLimit)?;
            if retained_item_count > MAX_PLAYLIST_ITEMS {
                return Err(DtoLoadError::ResourceLimit);
            }
            entry.validate_resource_limits()?;
        }
        Ok(())
    }

    fn into_domain(self) -> Result<LoadedPlaylistState, DtoLoadError> {
        if self.schema_version != CURRENT_PLAYLIST_STATE_SCHEMA_VERSION {
            return Err(DtoLoadError::DomainValue);
        }

        let restored_entries = self
            .entries
            .into_iter()
            .map(PlaylistEntryV2Dto::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let next_item_id = NextPlaylistItemId::from_persistence_value(self.next_item_id)
            .map_err(|_| DtoLoadError::QueueState)?;
        let next_compound_group_id =
            NextPlaylistCompoundGroupId::from_persistence_value(self.next_compound_group_id)
                .map_err(|_| DtoLoadError::QueueState)?;
        let current_item_id = self
            .current_item_id
            .0
            .map(PlaylistItemId::from_persistence_value)
            .transpose()
            .map_err(|_| DtoLoadError::QueueState)?;
        let queue_restore = PlaylistQueueRestore::from_entries(
            restored_entries,
            next_item_id,
            next_compound_group_id,
            current_item_id,
        );

        let queue = if self.shuffle_enabled {
            let history = persisted_item_ids(self.shuffle_history)?;
            let upcoming = self
                .shuffle_upcoming
                .into_iter()
                .map(PlaylistEntryIdV2Dto::into_domain)
                .collect::<Result<Vec<_>, _>>()?;
            let history_cursor = self
                .shuffle_history_cursor
                .0
                .map(|index| {
                    usize::try_from(index)
                        .map(ShuffleHistoryCursor::from_index)
                        .map_err(|_| DtoLoadError::ShuffleTraversal)
                })
                .transpose()?;
            let traversal = ShuffleTraversalSnapshot::new(history, history_cursor, upcoming);
            PlaylistQueue::restore_with_shuffle(queue_restore, traversal).map_err(|error| {
                match error {
                    ShuffleQueueRestoreError::Queue(_) => DtoLoadError::QueueState,
                    ShuffleQueueRestoreError::Traversal(_) => DtoLoadError::ShuffleTraversal,
                }
            })?
        } else {
            PlaylistQueue::restore(queue_restore).map_err(|_| DtoLoadError::QueueState)?
        };

        Ok(LoadedPlaylistState::new(queue, self.repeat_mode.into()))
    }
}

impl PlaylistEntryV2Dto {
    fn from_domain(entry: &PlaylistEntry) -> Result<Self, StateSerializationError> {
        match entry {
            PlaylistEntry::Single(item) => Ok(Self::Single {
                item: PlaylistItemV2Dto::from_domain(item)?,
            }),
            PlaylistEntry::Compound(group) => Ok(Self::Compound {
                group_id: group.group_id().expose_value_for_persistence(),
                provenance_locator: PlaylistLocatorV1Dto::from_domain(group.provenance_locator())?,
                cached_summary: CachedMetadataV1Dto::from_domain(group.cached_summary()),
                durable_payload: Nullable(
                    group
                        .durable_payload()
                        .map(PlaylistCompoundDurablePayloadV2Dto::from_domain)
                        .transpose()?,
                ),
                parts: group
                    .parts()
                    .map(PlaylistCompoundPartV2Dto::from_domain)
                    .collect::<Result<Vec<_>, _>>()?,
            }),
        }
    }

    fn retained_item_count(&self) -> usize {
        match self {
            Self::Single { .. } => 1,
            Self::Compound { parts, .. } => parts.len(),
        }
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        match self {
            Self::Single { item } => item.validate_resource_limits(),
            Self::Compound {
                provenance_locator,
                cached_summary,
                durable_payload,
                parts,
                ..
            } => {
                if parts.is_empty() || parts.len() > MAX_PLAYLIST_ITEMS {
                    return Err(DtoLoadError::ResourceLimit);
                }
                provenance_locator.validate_resource_limits()?;
                cached_summary.validate_resource_limits()?;
                if let Some(payload) = &durable_payload.0 {
                    payload.validate_resource_limits()?;
                }
                for part in parts {
                    part.item.validate_resource_limits()?;
                }
                Ok(())
            }
        }
    }

    fn into_domain(self) -> Result<RestoredPlaylistEntry, DtoLoadError> {
        match self {
            Self::Single { item } => item.into_domain().map(RestoredPlaylistEntry::Single),
            Self::Compound {
                group_id,
                provenance_locator,
                cached_summary,
                durable_payload,
                parts,
            } => {
                let group_id = PlaylistCompoundGroupId::from_persistence_value(group_id)
                    .map_err(|_| DtoLoadError::QueueState)?;
                let mut part_item_ids = Vec::with_capacity(parts.len());
                let mut part_drafts = Vec::with_capacity(parts.len());
                for (part_index, part) in parts.into_iter().enumerate() {
                    part.validate_membership(group_id, part_index)?;
                    let (item_id, draft) = part.item.into_draft()?;
                    part_item_ids.push(item_id);
                    part_drafts.push(draft);
                }
                let mut group_draft = PlaylistCompoundGroupDraft::new(
                    provenance_locator.into_domain()?,
                    cached_summary.into_domain()?,
                    part_drafts,
                )
                .map_err(|_| DtoLoadError::QueueState)?;
                if let Some(payload) = durable_payload.0 {
                    group_draft = group_draft.with_durable_payload(payload.into_domain()?);
                }
                let restored_group =
                    RestoredPlaylistCompoundGroup::new(group_id, group_draft, part_item_ids)
                        .map_err(|_| DtoLoadError::QueueState)?;
                Ok(RestoredPlaylistEntry::Compound(restored_group))
            }
        }
    }
}

impl PlaylistItemV2Dto {
    fn from_domain(item: &PlaylistItem) -> Result<Self, StateSerializationError> {
        Ok(Self {
            item: PlaylistItemV1Dto::from_domain(item)?,
            durable_payload: Nullable(
                item.durable_payload()
                    .map(PlaylistSingleDurablePayloadV2Dto::from_domain)
                    .transpose()?,
            ),
        })
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        self.item.validate_resource_limits()?;
        if let Some(payload) = &self.durable_payload.0 {
            payload.validate_resource_limits()?;
        }
        Ok(())
    }

    fn into_domain(self) -> Result<RestoredPlaylistItem, DtoLoadError> {
        let (item_id, draft) = self.into_draft()?;
        Ok(RestoredPlaylistItem::new(item_id, draft))
    }

    fn into_draft(self) -> Result<(PlaylistItemId, PlaylistItemDraft), DtoLoadError> {
        let (item_id, mut draft) = self.item.into_draft()?;
        if let Some(payload) = self.durable_payload.0 {
            draft = draft.with_durable_payload(payload.into_domain()?);
        }
        Ok((item_id, draft))
    }
}

impl PlaylistCompoundPartV2Dto {
    fn from_domain(
        part: &playlist_core::PlaylistCompoundPart,
    ) -> Result<Self, StateSerializationError> {
        Ok(Self {
            membership: PlaylistCompoundMembershipV2Dto::from_domain(part.membership()),
            item: PlaylistItemV2Dto::from_domain(part.item())?,
        })
    }

    fn validate_membership(
        &self,
        expected_group_id: PlaylistCompoundGroupId,
        zero_based_index: usize,
    ) -> Result<(), DtoLoadError> {
        let persisted_group_id =
            PlaylistCompoundGroupId::from_persistence_value(self.membership.group_id)
                .map_err(|_| DtoLoadError::QueueState)?;
        let expected_ordinal = u32::try_from(
            zero_based_index
                .checked_add(1)
                .ok_or(DtoLoadError::QueueState)?,
        )
        .map_err(|_| DtoLoadError::QueueState)?;
        if persisted_group_id != expected_group_id || self.membership.ordinal != expected_ordinal {
            return Err(DtoLoadError::QueueState);
        }
        Ok(())
    }
}

impl PlaylistCompoundMembershipV2Dto {
    fn from_domain(membership: PlaylistCompoundMembership) -> Self {
        Self {
            group_id: membership.group_id().expose_value_for_persistence(),
            ordinal: membership.ordinal().one_based(),
        }
    }
}

impl PlaylistEntryIdV2Dto {
    fn from_domain(entry_id: PlaylistEntryId) -> Self {
        match entry_id {
            PlaylistEntryId::Single(item_id) => Self::Single {
                item_id: item_id.expose_value_for_persistence(),
            },
            PlaylistEntryId::Compound(group_id) => Self::Compound {
                group_id: group_id.expose_value_for_persistence(),
            },
        }
    }

    fn into_domain(self) -> Result<PlaylistEntryId, DtoLoadError> {
        match self {
            Self::Single { item_id } => PlaylistItemId::from_persistence_value(item_id)
                .map(PlaylistEntryId::Single)
                .map_err(|_| DtoLoadError::ShuffleTraversal),
            Self::Compound { group_id } => {
                PlaylistCompoundGroupId::from_persistence_value(group_id)
                    .map(PlaylistEntryId::Compound)
                    .map_err(|_| DtoLoadError::ShuffleTraversal)
            }
        }
    }
}

fn persisted_item_ids(values: Vec<u64>) -> Result<Vec<PlaylistItemId>, DtoLoadError> {
    values
        .into_iter()
        .map(|value| {
            PlaylistItemId::from_persistence_value(value)
                .map_err(|_| DtoLoadError::ShuffleTraversal)
        })
        .collect()
}
