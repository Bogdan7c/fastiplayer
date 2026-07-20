//! ID-less import drafts между parser/service mapping и будущим queue transaction.

use std::fmt;

use crate::payload::validate_ancillary_track_count;
use crate::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, MAX_PLAYLIST_ITEMS,
    PlaylistAncillaryTrackHint, PlaylistCompoundGroupDraft, PlaylistCueSemanticsAttachmentError,
    PlaylistCueTrackExportSemantics, PlaylistEntryDraft, PlaylistImportProvenance,
    PlaylistImportSourceKind, PlaylistItemDraft, PlaylistLocator, PlaylistPayloadBuildError,
    PlaylistPlaybackSpan, SecretUrlLocator,
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

/// Durable часть imported playable item, которую queue и persistence хранят
/// без знания parser-а, service adapter-а либо transport request material.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistSingleDurablePayload {
    reopen_locator: DurableReopenLocator,
    playback_span: Option<PlaylistPlaybackSpan>,
    cue_export_semantics: Option<PlaylistCueTrackExportSemantics>,
    ancillary_track_hints: Box<[PlaylistAncillaryTrackHint]>,
    provenance: PlaylistImportProvenance,
    availability: PlaylistImportAvailability,
}

impl PlaylistSingleDurablePayload {
    /// Создаёт validated durable payload до allocation/queue mutation.
    pub fn new(
        reopen_locator: DurableReopenLocator,
        playback_span: Option<PlaylistPlaybackSpan>,
        ancillary_track_hints: Vec<PlaylistAncillaryTrackHint>,
        provenance: PlaylistImportProvenance,
        availability: PlaylistImportAvailability,
    ) -> Result<Self, PlaylistPayloadBuildError> {
        validate_ancillary_track_count(ancillary_track_hints.len())?;

        Ok(Self {
            reopen_locator,
            playback_span,
            cue_export_semantics: None,
            ancillary_track_hints: ancillary_track_hints.into_boxed_slice(),
            provenance,
            availability,
        })
    }

    /// Возвращает durable identity будущего open/resolve.
    pub const fn reopen_locator(&self) -> &DurableReopenLocator {
        &self.reopen_locator
    }

    /// Возвращает optional playback window.
    pub const fn playback_span(&self) -> Option<PlaylistPlaybackSpan> {
        self.playback_span
    }

    /// Присоединяет exact CUE semantics только к согласованному CUE span payload.
    pub fn with_cue_export_semantics(
        mut self,
        semantics: PlaylistCueTrackExportSemantics,
    ) -> Result<Self, PlaylistCueSemanticsAttachmentError> {
        if self.provenance.source_kind() != PlaylistImportSourceKind::Cue {
            return Err(PlaylistCueSemanticsAttachmentError::NonCueProvenance);
        }
        let span = self
            .playback_span
            .ok_or(PlaylistCueSemanticsAttachmentError::MissingPlaybackSpan)?;
        if span.start() != semantics.index01().media_time() {
            return Err(PlaylistCueSemanticsAttachmentError::PlaybackStartMismatch);
        }
        self.cue_export_semantics = Some(semantics);
        Ok(self)
    }

    /// Возвращает optional exact CUE export semantics.
    pub const fn cue_export_semantics(&self) -> Option<PlaylistCueTrackExportSemantics> {
        self.cue_export_semantics
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

impl fmt::Debug for PlaylistSingleDurablePayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistSingleDurablePayload")
            .field("reopen_locator", &self.reopen_locator)
            .field("playback_span", &self.playback_span)
            .field("cue_export_semantics", &self.cue_export_semantics)
            .field(
                "ancillary_track_hint_count",
                &self.ancillary_track_hints.len(),
            )
            .field("provenance", &self.provenance)
            .field("availability", &self.availability)
            .finish()
    }
}

/// Durable group-level payload без cached summary и ordered parts.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistCompoundDurablePayload {
    reopen_locator: DurableReopenLocator,
    provenance: PlaylistImportProvenance,
}

impl PlaylistCompoundDurablePayload {
    /// Создаёт durable group payload из уже validated neutral values.
    pub const fn new(
        reopen_locator: DurableReopenLocator,
        provenance: PlaylistImportProvenance,
    ) -> Self {
        Self {
            reopen_locator,
            provenance,
        }
    }

    /// Возвращает durable compound root identity.
    pub const fn reopen_locator(&self) -> &DurableReopenLocator {
        &self.reopen_locator
    }

    /// Возвращает durable root provenance.
    pub const fn provenance(&self) -> &PlaylistImportProvenance {
        &self.provenance
    }
}

impl fmt::Debug for PlaylistCompoundDurablePayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistCompoundDurablePayload")
            .field("reopen_locator", &self.reopen_locator)
            .field("provenance", &self.provenance)
            .finish()
    }
}

/// ID-less imported playable item со всеми нейтральными payload values.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistSingleImportDraft {
    cached_metadata: CachedPlaylistMetadata,
    durable_payload: PlaylistSingleDurablePayload,
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
        let durable_payload = PlaylistSingleDurablePayload::new(
            reopen_locator,
            playback_span,
            ancillary_track_hints,
            provenance,
            availability,
        )?;

        Ok(Self {
            cached_metadata,
            durable_payload,
        })
    }

    /// Возвращает durable identity будущего open/resolve.
    pub const fn reopen_locator(&self) -> &DurableReopenLocator {
        self.durable_payload.reopen_locator()
    }

    /// Возвращает cached display/sort metadata.
    pub const fn cached_metadata(&self) -> &CachedPlaylistMetadata {
        &self.cached_metadata
    }

    /// Возвращает optional playback window.
    pub const fn playback_span(&self) -> Option<PlaylistPlaybackSpan> {
        self.durable_payload.playback_span()
    }

    /// Присоединяет exact CUE semantics до materialization в queue item.
    pub fn with_cue_export_semantics(
        mut self,
        semantics: PlaylistCueTrackExportSemantics,
    ) -> Result<Self, PlaylistCueSemanticsAttachmentError> {
        self.durable_payload = self.durable_payload.with_cue_export_semantics(semantics)?;
        Ok(self)
    }

    /// Возвращает optional exact CUE export semantics.
    pub const fn cue_export_semantics(&self) -> Option<PlaylistCueTrackExportSemantics> {
        self.durable_payload.cue_export_semantics()
    }

    /// Возвращает bounded ancillary hints.
    pub fn ancillary_track_hints(&self) -> &[PlaylistAncillaryTrackHint] {
        self.durable_payload.ancillary_track_hints()
    }

    /// Возвращает durable import provenance.
    pub const fn provenance(&self) -> &PlaylistImportProvenance {
        self.durable_payload.provenance()
    }

    /// Возвращает source-snapshot availability без runtime promise.
    pub const fn availability(&self) -> PlaylistImportAvailability {
        self.durable_payload.availability()
    }

    /// Возвращает цельный durable payload для будущего queue transaction.
    pub const fn durable_payload(&self) -> &PlaylistSingleDurablePayload {
        &self.durable_payload
    }
}

impl fmt::Debug for PlaylistSingleImportDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistSingleImportDraft")
            .field("durable_payload", &self.durable_payload)
            .field("cached_metadata", &"<redacted-metadata>")
            .finish()
    }
}

/// ID-less first-class compound import draft.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistCompoundImportDraft {
    cached_summary: CachedPlaylistMetadata,
    durable_payload: PlaylistCompoundDurablePayload,
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
            cached_summary,
            durable_payload: PlaylistCompoundDurablePayload::new(reopen_locator, provenance),
            parts: parts.into_boxed_slice(),
        })
    }

    /// Возвращает durable compound root identity.
    pub const fn reopen_locator(&self) -> &DurableReopenLocator {
        self.durable_payload.reopen_locator()
    }

    /// Возвращает cached group summary.
    pub const fn cached_summary(&self) -> &CachedPlaylistMetadata {
        &self.cached_summary
    }

    /// Возвращает durable root provenance.
    pub const fn provenance(&self) -> &PlaylistImportProvenance {
        self.durable_payload.provenance()
    }

    /// Возвращает цельный durable group payload.
    pub const fn durable_payload(&self) -> &PlaylistCompoundDurablePayload {
        &self.durable_payload
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
            .field("durable_payload", &self.durable_payload)
            .field("cached_summary", &"<redacted-metadata>")
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

/// Ошибка materialization neutral import draft в ID-less queue draft.
///
/// Этот boundary не выделяет Item/Group IDs и не меняет очередь. Он только
/// доказывает, что legacy operational locator можно получить из durable
/// identity либо из root provenance будущего service-owned child-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistImportMaterializationError {
    /// Service-owned child не содержит local/URL root для legacy app lookup.
    ServiceLocatorWithoutOperationalRoot,
    /// Private compound invariant был нарушен до queue boundary.
    EmptyCompoundInvariant,
}

impl fmt::Display for PlaylistImportMaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceLocatorWithoutOperationalRoot => {
                formatter.write_str("service import locator has no local/URL operational root")
            }
            Self::EmptyCompoundInvariant => {
                formatter.write_str("compound import lost all parts before queue materialization")
            }
        }
    }
}

impl std::error::Error for PlaylistImportMaterializationError {}

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

    /// Преобразует neutral import payload в queue-owned ID-less draft.
    ///
    /// Allocation остаётся внутри последующего `PlaylistQueue` commit-а.
    pub fn into_queue_draft(
        self,
    ) -> Result<PlaylistEntryDraft, PlaylistImportMaterializationError> {
        match self {
            Self::Single(single) => materialize_single(single).map(PlaylistEntryDraft::Single),
            Self::Compound(compound) => materialize_compound(compound),
        }
    }
}

impl From<PlaylistSingleImportDraft> for PlaylistImportEntryDraft {
    fn from(draft: PlaylistSingleImportDraft) -> Self {
        Self::Single(draft)
    }
}

/// Materialize-ит один imported item, сохраняя полный durable payload.
fn materialize_single(
    draft: PlaylistSingleImportDraft,
) -> Result<PlaylistItemDraft, PlaylistImportMaterializationError> {
    let PlaylistSingleImportDraft {
        cached_metadata,
        durable_payload,
    } = draft;
    let operational_locator = operational_locator(
        durable_payload.reopen_locator(),
        durable_payload.provenance().root_locator(),
    )?;
    let queue_draft = match operational_locator {
        PlaylistLocator::Local(locator) => PlaylistItemDraft::local(locator, None, cached_metadata),
        PlaylistLocator::Url(locator) => PlaylistItemDraft::url(locator, cached_metadata),
    };
    Ok(queue_draft.with_durable_payload(durable_payload))
}

/// Materialize-ит group и все parts до любого allocator-owned commit-а.
fn materialize_compound(
    draft: PlaylistCompoundImportDraft,
) -> Result<PlaylistEntryDraft, PlaylistImportMaterializationError> {
    let PlaylistCompoundImportDraft {
        cached_summary,
        durable_payload,
        parts,
    } = draft;
    let provenance_locator = operational_locator(
        durable_payload.reopen_locator(),
        durable_payload.provenance().root_locator(),
    )?;
    let materialized_parts = parts
        .into_vec()
        .into_iter()
        .map(materialize_single)
        .collect::<Result<Vec<_>, _>>()?;
    let group =
        PlaylistCompoundGroupDraft::new(provenance_locator, cached_summary, materialized_parts)
            .map_err(|_| PlaylistImportMaterializationError::EmptyCompoundInvariant)?
            .with_durable_payload(durable_payload);
    Ok(PlaylistEntryDraft::Compound(group))
}

/// Выбирает reversible operational locator без service payload disclosure.
fn operational_locator(
    primary: &DurableReopenLocator,
    root: &DurableReopenLocator,
) -> Result<PlaylistLocator, PlaylistImportMaterializationError> {
    durable_locator_as_queue_locator(primary)
        .or_else(|| durable_locator_as_queue_locator(root))
        .ok_or(PlaylistImportMaterializationError::ServiceLocatorWithoutOperationalRoot)
}

/// Клонирует только local/URL identity; opaque service bytes не раскрываются.
fn durable_locator_as_queue_locator(locator: &DurableReopenLocator) -> Option<PlaylistLocator> {
    match locator {
        DurableReopenLocator::Local(locator) => {
            Some(PlaylistLocator::Local(LocalLocator::clone(locator)))
        }
        DurableReopenLocator::Url(locator) => {
            Some(PlaylistLocator::Url(SecretUrlLocator::clone(locator)))
        }
        DurableReopenLocator::ServicePayload(_) => None,
    }
}

#[cfg(test)]
mod tests;
