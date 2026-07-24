//! Exact ambiguity-rejecting DASH Representation selection.

use std::fmt;

use dash_mpd_core::{
    DashAdaptationSet, DashContainer, DashMediaKind, DashPeriod, DashRepresentation,
};
use thiserror::Error;

/// Optional exact raster dimensions из caller evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashVideoDimensions {
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
}

/// Caller-owned evidence одной required Representation.
#[derive(Clone, PartialEq, Eq)]
pub struct DashRepresentationEvidence {
    /// Required component kind; selection не меняет layout догадкой.
    pub media_kind: DashMediaKind,
    /// Proven container из selected candidate/profile.
    pub container: DashContainer,
    /// Optional exact MPD Representation id.
    pub representation_id: Option<String>,
    /// Optional exact codecs attribute.
    pub codecs: Option<String>,
    /// Optional exact bandwidth.
    pub bandwidth: Option<u64>,
    /// Optional exact dimensions, если upstream descriptor их доказал.
    pub dimensions: Option<DashVideoDimensions>,
}

impl fmt::Debug for DashRepresentationEvidence {
    /// Не отражает внешние id/codecs values в diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashRepresentationEvidence")
            .field("media_kind", &self.media_kind)
            .field("container", &self.container)
            .field("has_representation_id", &self.representation_id.is_some())
            .field("has_codecs", &self.codecs.is_some())
            .field("bandwidth", &self.bandwidth)
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

/// Explicit presentation layout; runtime не выводит muxed/separate из количества rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashPresentationSelection {
    /// Один muxed либо single-component Representation.
    Single {
        /// Exact evidence required row-а.
        main: DashRepresentationEvidence,
    },
    /// Независимые video/audio Representation.
    Separate {
        /// Exact video evidence.
        video: DashRepresentationEvidence,
        /// Exact audio evidence.
        audio: DashRepresentationEvidence,
    },
}

/// Typed selection failure без MPD-provided strings.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DashRepresentationSelectionError {
    /// Ни одна Representation не совпала со всей caller evidence.
    #[error("DASH Representation is absent for required component")]
    Absent,
    /// Более одной Representation совпали; first-row fallback запрещён.
    #[error("DASH Representation evidence is ambiguous")]
    Ambiguous,
}

/// Выбирает ровно одну Representation во всём Period.
pub(crate) struct SelectedDashRepresentation<'period> {
    /// AdaptationSet владеет lexical BaseURL level.
    pub adaptation: &'period DashAdaptationSet,
    /// Exact selected Representation.
    pub representation: &'period DashRepresentation,
}

/// Выбирает ровно одну Representation во всём Period.
pub(crate) fn select_representation<'period>(
    period: &'period DashPeriod,
    evidence: &DashRepresentationEvidence,
) -> Result<SelectedDashRepresentation<'period>, DashRepresentationSelectionError> {
    let mut matches = period
        .adaptation_sets
        .iter()
        .flat_map(|adaptation| {
            adaptation
                .representations
                .iter()
                .map(move |representation| (adaptation, representation))
        })
        .filter(|(_, representation)| representation_matches(representation, evidence));
    let selected = matches
        .next()
        .ok_or(DashRepresentationSelectionError::Absent)?;
    if matches.next().is_some() {
        return Err(DashRepresentationSelectionError::Ambiguous);
    }
    Ok(SelectedDashRepresentation {
        adaptation: selected.0,
        representation: selected.1,
    })
}

/// Проверяет только exact retained evidence; отсутствующие dimensions не угадываются.
fn representation_matches(
    representation: &DashRepresentation,
    evidence: &DashRepresentationEvidence,
) -> bool {
    representation.media_kind == evidence.media_kind
        && representation.container == evidence.container
        && evidence
            .representation_id
            .as_ref()
            .is_none_or(|identifier| identifier == &representation.id)
        && evidence
            .codecs
            .as_ref()
            .is_none_or(|codecs| codecs == &representation.codecs)
        && evidence
            .bandwidth
            .is_none_or(|bandwidth| representation.bandwidth == Some(bandwidth))
        && evidence.dimensions.as_ref().is_none_or(|dimensions| {
            representation.width == Some(dimensions.width)
                && representation.height == Some(dimensions.height)
        })
}
