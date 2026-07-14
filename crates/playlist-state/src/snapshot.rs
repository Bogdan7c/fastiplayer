use std::fmt;

use crate::dto::{self, OwnedPlaylistStateSnapshot};
use crate::{PlaylistStateSnapshot, StateSerializationError};

/// Монотонная process-local revision committed playlist state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SaveRevision(u64);

impl SaveRevision {
    /// Первая revision, которую разрешено отправить writer-у.
    pub const FIRST: Self = Self(1);

    /// Возвращает следующую revision либо typed exhaustion.
    pub const fn checked_next(self) -> Result<Self, SaveRevisionExhausted> {
        match self.0.checked_add(1) {
            Some(next_revision) => Ok(Self(next_revision)),
            None => Err(SaveRevisionExhausted),
        }
    }

    /// Возвращает opaque numeric value только для diagnostics/tests.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Process исчерпал представление persistence revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveRevisionExhausted;

impl fmt::Display for SaveRevisionExhausted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("исчерпана монотонная revision playlist-state")
    }
}

impl std::error::Error for SaveRevisionExhausted {}

/// Immutable owned snapshot одной committed revision.
///
/// Private DTO уже содержит matching `next_item_id`; ни atomic writer, ни
/// background worker не имеют API для отдельной подстановки watermark.
pub struct ImmutableSaveSnapshot {
    revision: SaveRevision,
    owned_state: OwnedPlaylistStateSnapshot,
}

impl ImmutableSaveSnapshot {
    /// Снимает согласованный domain state до передачи ownership worker-у.
    pub fn capture(
        revision: SaveRevision,
        snapshot: PlaylistStateSnapshot<'_>,
    ) -> Result<Self, StateSerializationError> {
        let owned_state = dto::capture_owned_state(snapshot)?;
        Ok(Self {
            revision,
            owned_state,
        })
    }

    /// Возвращает revision без раскрытия private disk DTO.
    pub const fn revision(&self) -> SaveRevision {
        self.revision
    }

    /// JSON encoding остаётся внутри persistence owner crate.
    pub(crate) fn serialize_json(&self) -> Result<Vec<u8>, StateSerializationError> {
        dto::serialize_owned_state(&self.owned_state)
    }
}

impl fmt::Debug for ImmutableSaveSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableSaveSnapshot")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}
