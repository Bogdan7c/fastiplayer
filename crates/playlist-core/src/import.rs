//! ID-less import drafts между parser/service mapping и будущим queue transaction.

use std::fmt;

use crate::payload::validate_ancillary_track_count;
use crate::{
    CachedPlaylistMetadata, DurableReopenLocator, MAX_PLAYLIST_ITEMS, PlaylistAncillaryTrackHint,
    PlaylistImportProvenance, PlaylistPayloadBuildError, PlaylistPlaybackSpan,
};

/// Import preview не может потребовать больше Item IDs, чем canonical queue.
pub const MAX_PLAYLIST_IMPORT_COMPOUND_PARTS: usize = MAX_PLAYLIST_ITEMS;

/// Readiness imported child-а без player/service dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistImportAvailability {
    /// Durable locator ожидаемо можно resolve/open через будущий adapter.
    Available,
    /// Stable child сохраняется, хотя source snapshot пометил его unavailable.
    Unavailable,
}

/// ID-less imported playable item со всеми нейтральными payload values.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistSingleImportDraft {
    reopen_locator: DurableReopenLocator,
    cached_metadata: CachedPlaylistMetadata,
    playback_span: Option<PlaylistPlaybackSpan>,
    ancillary_track_hints: Box<[PlaylistAncillaryTrackHint]>,
    provenance: PlaylistImportProvenance,
    availability: PlaylistImportAvailability,
}

impl PlaylistSingleImportDraft {
    /// Создаёт bounded single draft без выделения `PlaylistItemId`.
    pub fn new(
        reopen_locator: DurableReopenLocator,
        cached_metadata: CachedPlaylistMetadata,
        playback_span: Option<PlaylistPlaybackSpan>,
        ancillary_track_hints: Vec<PlaylistAncillaryTrackHint>,
        provenance: PlaylistImportProvenance,
        availability: PlaylistImportAvailability,
    ) -> Result<Self, PlaylistPayloadBuildError> {
        validate_ancillary_track_count(ancillary_track_hints.len())?;

        Ok(Self {
            reopen_locator,
            cached_metadata,
            playback_span,
            ancillary_track_hints: ancillary_track_hints.into_boxed_slice(),
            provenance,
            availability,
        })
    }

    /// Возвращает durable identity будущего open/resolve.
    pub const fn reopen_locator(&self) -> &DurableReopenLocator {
        &self.reopen_locator
    }

    /// Возвращает cached display/sort metadata.
    pub const fn cached_metadata(&self) -> &CachedPlaylistMetadata {
        &self.cached_metadata
    }

    /// Возвращает optional playback window.
    pub const fn playback_span(&self) -> Option<PlaylistPlaybackSpan> {
        self.playback_span
    }

    /// Возвращает bounded ancillary hints.
    pub fn ancillary_track_hints(&self) -> &[PlaylistAncillaryTrackHint] {
        &self.ancillary_track_hints
    }

    /// Возвращает durable import provenance.
    pub const fn provenance(&self) -> &PlaylistImportProvenance {
        &self.provenance
    }

    /// Возвращает source-snapshot availability без runtime promise.
    pub const fn availability(&self) -> PlaylistImportAvailability {
        self.availability
    }
}

impl fmt::Debug for PlaylistSingleImportDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistSingleImportDraft")
            .field("reopen_locator", &self.reopen_locator)
            .field("cached_metadata", &"<redacted-metadata>")
            .field("playback_span", &self.playback_span)
            .field(
                "ancillary_track_hint_count",
                &self.ancillary_track_hints.len(),
            )
            .field("provenance", &self.provenance)
            .field("availability", &self.availability)
            .finish()
    }
}

/// ID-less first-class compound import draft.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistCompoundImportDraft {
    reopen_locator: DurableReopenLocator,
    cached_summary: CachedPlaylistMetadata,
    provenance: PlaylistImportProvenance,
    parts: Box<[PlaylistSingleImportDraft]>,
}

impl PlaylistCompoundImportDraft {
    /// Создаёт compound draft, не допуская zero-part либо oversized group.
    pub fn new(
        reopen_locator: DurableReopenLocator,
        cached_summary: CachedPlaylistMetadata,
        provenance: PlaylistImportProvenance,
        parts: Vec<PlaylistSingleImportDraft>,
    ) -> Result<Self, PlaylistCompoundImportDraftError> {
        if parts.is_empty() {
            return Err(PlaylistCompoundImportDraftError::EmptyCompound);
        }
        if parts.len() > MAX_PLAYLIST_IMPORT_COMPOUND_PARTS {
            return Err(PlaylistCompoundImportDraftError::PartLimitExceeded {
                provided: parts.len(),
                maximum: MAX_PLAYLIST_IMPORT_COMPOUND_PARTS,
            });
        }

        Ok(Self {
            reopen_locator,
            cached_summary,
            provenance,
            parts: parts.into_boxed_slice(),
        })
    }

    /// Возвращает durable compound root identity.
    pub const fn reopen_locator(&self) -> &DurableReopenLocator {
        &self.reopen_locator
    }

    /// Возвращает cached group summary.
    pub const fn cached_summary(&self) -> &CachedPlaylistMetadata {
        &self.cached_summary
    }

    /// Возвращает durable root provenance.
    pub const fn provenance(&self) -> &PlaylistImportProvenance {
        &self.provenance
    }

    /// Возвращает ordered source parts без ID и structural authority.
    pub fn parts(
        &self,
    ) -> impl ExactSizeIterator<Item = &PlaylistSingleImportDraft> + DoubleEndedIterator + '_ {
        self.parts.iter()
    }

    /// Возвращает retained part count для import preview/capacity preflight.
    pub const fn retained_part_count(&self) -> usize {
        self.parts.len()
    }
}

impl fmt::Debug for PlaylistCompoundImportDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistCompoundImportDraft")
            .field("reopen_locator", &self.reopen_locator)
            .field("cached_summary", &"<redacted-metadata>")
            .field("provenance", &self.provenance)
            .field("retained_part_count", &self.parts.len())
            .finish()
    }
}

/// Ошибка построения compound import draft до allocation/queue mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistCompoundImportDraftError {
    /// Zero-part group не получает top-level identity.
    EmptyCompound,
    /// Group превышает общий retained item capacity bound.
    PartLimitExceeded {
        /// Фактическое число частей.
        provided: usize,
        /// Максимальное число частей.
        maximum: usize,
    },
}

impl fmt::Display for PlaylistCompoundImportDraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCompound => {
                formatter.write_str("compound import draft must retain at least one part")
            }
            Self::PartLimitExceeded { provided, maximum } => write!(
                formatter,
                "compound import draft contains {provided} parts; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for PlaylistCompoundImportDraftError {}

/// ID-less top-level import result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaylistImportEntryDraft {
    /// Самостоятельный imported playable item.
    Single(PlaylistSingleImportDraft),
    /// First-class compound imported entry.
    Compound(PlaylistCompoundImportDraft),
}

impl PlaylistImportEntryDraft {
    /// Возвращает retained Item ID demand без фактического allocation.
    pub const fn retained_item_count(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Compound(group) => group.retained_part_count(),
        }
    }

    /// Отличает first-class compound intent от single item.
    pub const fn is_compound(&self) -> bool {
        matches!(self, Self::Compound(_))
    }
}

impl From<PlaylistSingleImportDraft> for PlaylistImportEntryDraft {
    fn from(draft: PlaylistSingleImportDraft) -> Self {
        Self::Single(draft)
    }
}

#[cfg(test)]
mod tests;
