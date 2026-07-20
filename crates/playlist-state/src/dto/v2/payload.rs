//! Mapping и bounds durable payload-ов schema v2.

use std::num::NonZeroU32;
use std::time::Duration;

use media_core::MediaTime;
use playlist_core::{
    DurableReopenLocator, MAX_DURABLE_REOPEN_SERVICE_OWNER_BYTES,
    MAX_DURABLE_REOPEN_SERVICE_PAYLOAD_BYTES, MAX_PLAYLIST_ANCILLARY_DISPLAY_NAME_BYTES,
    MAX_PLAYLIST_ANCILLARY_FORMAT_IDENTITY_BYTES, MAX_PLAYLIST_ANCILLARY_IDENTITY_BYTES,
    MAX_PLAYLIST_ANCILLARY_LANGUAGE_BYTES, MAX_PLAYLIST_ANCILLARY_TRACK_HINTS,
    PlaylistAncillaryTrackHint, PlaylistAncillaryTrackOrigin, PlaylistAncillaryTrackSelectionKind,
    PlaylistCompoundDurablePayload, PlaylistCueDocumentExportEligibility, PlaylistCueFileType,
    PlaylistCueFrameIndex, PlaylistCueTrackExportSemantics, PlaylistImportAvailability,
    PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistPlaybackSpan,
    PlaylistSingleDurablePayload, SecretUrlLocator, ServiceReopenMaterialKind,
};

use super::{
    DurableReopenLocatorV2Dto, MediaTimeV2Dto, Nullable, PlaylistAncillaryTrackHintV2Dto,
    PlaylistAncillaryTrackOriginV2Dto, PlaylistAncillaryTrackSelectionKindV2Dto,
    PlaylistCompoundDurablePayloadV2Dto, PlaylistCueDocumentExportEligibilityV2Dto,
    PlaylistCueFileTypeV2Dto, PlaylistCueTrackExportSemanticsV2Dto,
    PlaylistImportAvailabilityV2Dto, PlaylistImportProvenanceV2Dto, PlaylistImportSourceKindV2Dto,
    PlaylistPlaybackSpanV2Dto, PlaylistSingleDurablePayloadV2Dto, StableServiceMaterialKindV2Dto,
};
use crate::StateSerializationError;
use crate::dto::{DtoLoadError, LocalPathV1Dto, MAX_LOCATOR_TEXT_BYTES};

impl PlaylistSingleDurablePayloadV2Dto {
    pub(super) fn from_domain(
        payload: &PlaylistSingleDurablePayload,
    ) -> Result<Self, StateSerializationError> {
        Ok(Self {
            reopen_locator: DurableReopenLocatorV2Dto::from_domain(payload.reopen_locator())?,
            playback_span: Nullable(payload.playback_span().map(PlaylistPlaybackSpanV2Dto::from)),
            cue_export_semantics: payload
                .cue_export_semantics()
                .map(PlaylistCueTrackExportSemanticsV2Dto::from_domain),
            ancillary_track_hints: payload
                .ancillary_track_hints()
                .iter()
                .map(PlaylistAncillaryTrackHintV2Dto::from_domain)
                .collect::<Result<Vec<_>, _>>()?,
            provenance: PlaylistImportProvenanceV2Dto::from_domain(payload.provenance())?,
            availability: payload.availability().into(),
        })
    }

    pub(super) fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        if self.ancillary_track_hints.len() > MAX_PLAYLIST_ANCILLARY_TRACK_HINTS {
            return Err(DtoLoadError::ResourceLimit);
        }
        self.reopen_locator.validate_resource_limits()?;
        self.provenance.validate_resource_limits()?;
        for hint in &self.ancillary_track_hints {
            hint.validate_resource_limits()?;
        }
        Ok(())
    }

    pub(super) fn into_domain(self) -> Result<PlaylistSingleDurablePayload, DtoLoadError> {
        let playback_span = self
            .playback_span
            .0
            .map(PlaylistPlaybackSpanV2Dto::into_domain)
            .transpose()?;
        let hints = self
            .ancillary_track_hints
            .into_iter()
            .map(PlaylistAncillaryTrackHintV2Dto::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let payload = PlaylistSingleDurablePayload::new(
            self.reopen_locator.into_domain()?,
            playback_span,
            hints,
            self.provenance.into_domain()?,
            self.availability.into(),
        )
        .map_err(|_| DtoLoadError::DomainValue)?;
        let cue_export_semantics = self
            .cue_export_semantics
            .map(PlaylistCueTrackExportSemanticsV2Dto::into_domain)
            .transpose()?;
        match cue_export_semantics {
            Some(semantics) => payload
                .with_cue_export_semantics(semantics)
                .map_err(|_| DtoLoadError::DomainValue),
            None => Ok(payload),
        }
    }
}

impl PlaylistCueTrackExportSemanticsV2Dto {
    fn from_domain(semantics: PlaylistCueTrackExportSemantics) -> Self {
        Self {
            file_type: semantics.file_type().into(),
            track_number: semantics.track_number(),
            index00_total_frames: semantics.index00().map(PlaylistCueFrameIndex::total_frames),
            index01_total_frames: semantics.index01().total_frames(),
            document_eligibility: semantics.document_eligibility().into(),
        }
    }

    fn into_domain(self) -> Result<PlaylistCueTrackExportSemantics, DtoLoadError> {
        PlaylistCueTrackExportSemantics::new(
            self.file_type.into(),
            self.track_number,
            self.index00_total_frames.map(PlaylistCueFrameIndex::new),
            PlaylistCueFrameIndex::new(self.index01_total_frames),
            self.document_eligibility.into(),
        )
        .map_err(|_| DtoLoadError::DomainValue)
    }
}

impl From<PlaylistCueFileType> for PlaylistCueFileTypeV2Dto {
    fn from(value: PlaylistCueFileType) -> Self {
        match value {
            PlaylistCueFileType::Wave => Self::Wave,
            PlaylistCueFileType::Aiff => Self::Aiff,
            PlaylistCueFileType::Mp3 => Self::Mp3,
            PlaylistCueFileType::Flac => Self::Flac,
        }
    }
}

impl From<PlaylistCueFileTypeV2Dto> for PlaylistCueFileType {
    fn from(value: PlaylistCueFileTypeV2Dto) -> Self {
        match value {
            PlaylistCueFileTypeV2Dto::Wave => Self::Wave,
            PlaylistCueFileTypeV2Dto::Aiff => Self::Aiff,
            PlaylistCueFileTypeV2Dto::Mp3 => Self::Mp3,
            PlaylistCueFileTypeV2Dto::Flac => Self::Flac,
        }
    }
}

impl From<PlaylistCueDocumentExportEligibility> for PlaylistCueDocumentExportEligibilityV2Dto {
    fn from(value: PlaylistCueDocumentExportEligibility) -> Self {
        match value {
            PlaylistCueDocumentExportEligibility::Exact => Self::Exact,
            PlaylistCueDocumentExportEligibility::Ineligible => Self::Ineligible,
        }
    }
}

impl From<PlaylistCueDocumentExportEligibilityV2Dto> for PlaylistCueDocumentExportEligibility {
    fn from(value: PlaylistCueDocumentExportEligibilityV2Dto) -> Self {
        match value {
            PlaylistCueDocumentExportEligibilityV2Dto::Exact => Self::Exact,
            PlaylistCueDocumentExportEligibilityV2Dto::Ineligible => Self::Ineligible,
        }
    }
}

impl PlaylistCompoundDurablePayloadV2Dto {
    pub(super) fn from_domain(
        payload: &PlaylistCompoundDurablePayload,
    ) -> Result<Self, StateSerializationError> {
        Ok(Self {
            reopen_locator: DurableReopenLocatorV2Dto::from_domain(payload.reopen_locator())?,
            provenance: PlaylistImportProvenanceV2Dto::from_domain(payload.provenance())?,
        })
    }

    pub(super) fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        self.reopen_locator.validate_resource_limits()?;
        self.provenance.validate_resource_limits()
    }

    pub(super) fn into_domain(self) -> Result<PlaylistCompoundDurablePayload, DtoLoadError> {
        Ok(PlaylistCompoundDurablePayload::new(
            self.reopen_locator.into_domain()?,
            self.provenance.into_domain()?,
        ))
    }
}

impl DurableReopenLocatorV2Dto {
    fn from_domain(locator: &DurableReopenLocator) -> Result<Self, StateSerializationError> {
        match locator {
            DurableReopenLocator::Local(local) => Ok(Self::Local {
                path: LocalPathV1Dto::from_domain(local)?,
            }),
            DurableReopenLocator::Url(url) => Ok(Self::Url {
                reopenable_url: url.expose_secret_for_persistence().to_owned(),
            }),
            DurableReopenLocator::ServicePayload(payload) => Ok(Self::Service {
                service_owner: payload.service_owner().to_owned(),
                payload_version: payload.payload_version().expose_value_for_persistence(),
                material_kind: StableServiceMaterialKindV2Dto::from_domain(
                    payload.material_kind(),
                )?,
                payload_bytes: payload.expose_payload_for_reopen().to_vec(),
            }),
        }
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        match self {
            Self::Local { path } => path.validate_resource_limits(),
            Self::Url { reopenable_url } => {
                validate_required_text(reopenable_url, MAX_LOCATOR_TEXT_BYTES)
            }
            Self::Service {
                service_owner,
                payload_bytes,
                ..
            } => {
                validate_required_text(service_owner, MAX_DURABLE_REOPEN_SERVICE_OWNER_BYTES)?;
                if payload_bytes.is_empty()
                    || payload_bytes.len() > MAX_DURABLE_REOPEN_SERVICE_PAYLOAD_BYTES
                {
                    return Err(DtoLoadError::ResourceLimit);
                }
                Ok(())
            }
        }
    }

    fn into_domain(self) -> Result<DurableReopenLocator, DtoLoadError> {
        match self {
            Self::Local { path } => Ok(DurableReopenLocator::local(path.into_domain()?)),
            Self::Url { reopenable_url } => {
                let locator = SecretUrlLocator::from_reopenable_url(reopenable_url)
                    .map_err(|_| DtoLoadError::DomainValue)?;
                Ok(DurableReopenLocator::url(locator))
            }
            Self::Service {
                service_owner,
                payload_version,
                material_kind,
                payload_bytes,
            } => DurableReopenLocator::from_service_payload(
                service_owner,
                payload_version,
                material_kind.into(),
                payload_bytes,
            )
            .map_err(|_| DtoLoadError::DomainValue),
        }
    }
}

impl StableServiceMaterialKindV2Dto {
    fn from_domain(
        material_kind: ServiceReopenMaterialKind,
    ) -> Result<Self, StateSerializationError> {
        match material_kind {
            ServiceReopenMaterialKind::StableWebpageIdentity => Ok(Self::Webpage),
            ServiceReopenMaterialKind::StableOriginalIdentity => Ok(Self::Original),
            ServiceReopenMaterialKind::StableExtractorIdentity => Ok(Self::Extractor),
            ServiceReopenMaterialKind::FormatUrl
            | ServiceReopenMaterialKind::ManifestUrl
            | ServiceReopenMaterialKind::FragmentUrl
            | ServiceReopenMaterialKind::KeyUrl
            | ServiceReopenMaterialKind::SignedEndpoint
            | ServiceReopenMaterialKind::Headers
            | ServiceReopenMaterialKind::Cookies
            | ServiceReopenMaterialKind::AuthorizationOrSession => {
                Err(StateSerializationError::InvalidDurableReopenLocator)
            }
        }
    }
}

impl From<StableServiceMaterialKindV2Dto> for ServiceReopenMaterialKind {
    fn from(value: StableServiceMaterialKindV2Dto) -> Self {
        match value {
            StableServiceMaterialKindV2Dto::Webpage => Self::StableWebpageIdentity,
            StableServiceMaterialKindV2Dto::Original => Self::StableOriginalIdentity,
            StableServiceMaterialKindV2Dto::Extractor => Self::StableExtractorIdentity,
        }
    }
}

impl From<PlaylistPlaybackSpan> for PlaylistPlaybackSpanV2Dto {
    fn from(span: PlaylistPlaybackSpan) -> Self {
        Self {
            start: span.start().into(),
            end_exclusive: Nullable(span.end_exclusive().map(MediaTimeV2Dto::from)),
        }
    }
}

impl PlaylistPlaybackSpanV2Dto {
    fn into_domain(self) -> Result<PlaylistPlaybackSpan, DtoLoadError> {
        PlaylistPlaybackSpan::new(
            self.start.into_domain()?,
            self.end_exclusive
                .0
                .map(MediaTimeV2Dto::into_domain)
                .transpose()?,
        )
        .map_err(|_| DtoLoadError::DomainValue)
    }
}

impl From<MediaTime> for MediaTimeV2Dto {
    fn from(time: MediaTime) -> Self {
        let duration = time.as_duration();
        Self {
            seconds: duration.as_secs(),
            subsec_nanos: duration.subsec_nanos(),
        }
    }
}

impl MediaTimeV2Dto {
    fn into_domain(self) -> Result<MediaTime, DtoLoadError> {
        if self.subsec_nanos >= 1_000_000_000 {
            return Err(DtoLoadError::DomainValue);
        }
        Ok(MediaTime::from_duration(Duration::new(
            self.seconds,
            self.subsec_nanos,
        )))
    }
}

impl PlaylistAncillaryTrackHintV2Dto {
    fn from_domain(hint: &PlaylistAncillaryTrackHint) -> Result<Self, StateSerializationError> {
        Ok(Self {
            semantic_identity: hint.semantic_identity().to_owned(),
            language: Nullable(hint.language().map(str::to_owned)),
            display_name: Nullable(hint.display_name().map(str::to_owned)),
            selection_kind: hint.selection_kind().into(),
            origin: PlaylistAncillaryTrackOriginV2Dto::from_domain(hint.origin())?,
            service_format_identity: Nullable(hint.service_format_identity().map(str::to_owned)),
        })
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        validate_required_text(
            &self.semantic_identity,
            MAX_PLAYLIST_ANCILLARY_IDENTITY_BYTES,
        )?;
        validate_optional_text(
            self.language.0.as_deref(),
            MAX_PLAYLIST_ANCILLARY_LANGUAGE_BYTES,
        )?;
        validate_optional_text(
            self.display_name.0.as_deref(),
            MAX_PLAYLIST_ANCILLARY_DISPLAY_NAME_BYTES,
        )?;
        validate_optional_text(
            self.service_format_identity.0.as_deref(),
            MAX_PLAYLIST_ANCILLARY_FORMAT_IDENTITY_BYTES,
        )?;
        self.origin.validate_resource_limits()
    }

    fn into_domain(self) -> Result<PlaylistAncillaryTrackHint, DtoLoadError> {
        PlaylistAncillaryTrackHint::new(
            self.semantic_identity,
            self.language.0,
            self.display_name.0,
            self.selection_kind.into(),
            self.origin.into_domain()?,
            self.service_format_identity.0,
        )
        .map_err(|_| DtoLoadError::DomainValue)
    }
}

impl From<PlaylistAncillaryTrackSelectionKind> for PlaylistAncillaryTrackSelectionKindV2Dto {
    fn from(value: PlaylistAncillaryTrackSelectionKind) -> Self {
        match value {
            PlaylistAncillaryTrackSelectionKind::Manual => Self::Manual,
            PlaylistAncillaryTrackSelectionKind::Automatic => Self::Automatic,
        }
    }
}

impl From<PlaylistAncillaryTrackSelectionKindV2Dto> for PlaylistAncillaryTrackSelectionKind {
    fn from(value: PlaylistAncillaryTrackSelectionKindV2Dto) -> Self {
        match value {
            PlaylistAncillaryTrackSelectionKindV2Dto::Manual => Self::Manual,
            PlaylistAncillaryTrackSelectionKindV2Dto::Automatic => Self::Automatic,
        }
    }
}

impl PlaylistAncillaryTrackOriginV2Dto {
    fn from_domain(origin: &PlaylistAncillaryTrackOrigin) -> Result<Self, StateSerializationError> {
        match origin {
            PlaylistAncillaryTrackOrigin::Embedded => Ok(Self::Embedded),
            PlaylistAncillaryTrackOrigin::External(locator) => Ok(Self::External {
                reopen_locator: DurableReopenLocatorV2Dto::from_domain(locator)?,
            }),
        }
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        match self {
            Self::Embedded => Ok(()),
            Self::External { reopen_locator } => reopen_locator.validate_resource_limits(),
        }
    }

    fn into_domain(self) -> Result<PlaylistAncillaryTrackOrigin, DtoLoadError> {
        match self {
            Self::Embedded => Ok(PlaylistAncillaryTrackOrigin::Embedded),
            Self::External { reopen_locator } => Ok(PlaylistAncillaryTrackOrigin::External(
                reopen_locator.into_domain()?,
            )),
        }
    }
}

impl PlaylistImportProvenanceV2Dto {
    fn from_domain(provenance: &PlaylistImportProvenance) -> Result<Self, StateSerializationError> {
        Ok(Self {
            root_locator: DurableReopenLocatorV2Dto::from_domain(provenance.root_locator())?,
            source_kind: provenance.source_kind().into(),
            source_ordinal: Nullable(provenance.source_ordinal().map(NonZeroU32::get)),
        })
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        self.root_locator.validate_resource_limits()?;
        if self.source_ordinal.0 == Some(0) {
            return Err(DtoLoadError::DomainValue);
        }
        Ok(())
    }

    fn into_domain(self) -> Result<PlaylistImportProvenance, DtoLoadError> {
        let source_ordinal = self
            .source_ordinal
            .0
            .map(|ordinal| NonZeroU32::new(ordinal).ok_or(DtoLoadError::DomainValue))
            .transpose()?;
        Ok(PlaylistImportProvenance::new(
            self.root_locator.into_domain()?,
            self.source_kind.into(),
            source_ordinal,
        ))
    }
}

impl From<PlaylistImportSourceKind> for PlaylistImportSourceKindV2Dto {
    fn from(value: PlaylistImportSourceKind) -> Self {
        match value {
            PlaylistImportSourceKind::M3u => Self::M3u,
            PlaylistImportSourceKind::M3u8 => Self::M3u8,
            PlaylistImportSourceKind::Xspf => Self::Xspf,
            PlaylistImportSourceKind::Cue => Self::Cue,
            PlaylistImportSourceKind::Service => Self::Service,
        }
    }
}

impl From<PlaylistImportSourceKindV2Dto> for PlaylistImportSourceKind {
    fn from(value: PlaylistImportSourceKindV2Dto) -> Self {
        match value {
            PlaylistImportSourceKindV2Dto::M3u => Self::M3u,
            PlaylistImportSourceKindV2Dto::M3u8 => Self::M3u8,
            PlaylistImportSourceKindV2Dto::Xspf => Self::Xspf,
            PlaylistImportSourceKindV2Dto::Cue => Self::Cue,
            PlaylistImportSourceKindV2Dto::Service => Self::Service,
        }
    }
}

impl From<PlaylistImportAvailability> for PlaylistImportAvailabilityV2Dto {
    fn from(value: PlaylistImportAvailability) -> Self {
        match value {
            PlaylistImportAvailability::Available => Self::Available,
            PlaylistImportAvailability::Unavailable => Self::Unavailable,
        }
    }
}

impl From<PlaylistImportAvailabilityV2Dto> for PlaylistImportAvailability {
    fn from(value: PlaylistImportAvailabilityV2Dto) -> Self {
        match value {
            PlaylistImportAvailabilityV2Dto::Available => Self::Available,
            PlaylistImportAvailabilityV2Dto::Unavailable => Self::Unavailable,
        }
    }
}

fn validate_required_text(value: &str, maximum_bytes: usize) -> Result<(), DtoLoadError> {
    if value.is_empty() {
        return Err(DtoLoadError::DomainValue);
    }
    if value.len() > maximum_bytes {
        return Err(DtoLoadError::ResourceLimit);
    }
    Ok(())
}

fn validate_optional_text(value: Option<&str>, maximum_bytes: usize) -> Result<(), DtoLoadError> {
    if let Some(value) = value {
        if value.is_empty() {
            return Err(DtoLoadError::DomainValue);
        }
        if value.len() > maximum_bytes {
            return Err(DtoLoadError::ResourceLimit);
        }
    }
    Ok(())
}
