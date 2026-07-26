//! Neutral selection resolution and refresh rematching.

use super::*;

pub(crate) fn rematch_logical_selection(
    presentation: &DashMpd,
    selection: &DashLogicalRepresentationSelection,
) -> Result<DashLogicalRepresentationSelection, DashRepresentationLaneSelectionError> {
    match selection {
        DashLogicalRepresentationSelection::Single(lane) => {
            rematch_logical_lane(presentation, lane).map(DashLogicalRepresentationSelection::Single)
        }
        DashLogicalRepresentationSelection::Separate { video, audio } => {
            Ok(DashLogicalRepresentationSelection::Separate {
                video: rematch_logical_lane(presentation, video)?,
                audio: rematch_logical_lane(presentation, audio)?,
            })
        }
    }
}

fn rematch_logical_lane(
    presentation: &DashMpd,
    lane: &DashLogicalRepresentationLane,
) -> Result<DashLogicalRepresentationLane, DashRepresentationLaneSelectionError> {
    let mut locations = Vec::with_capacity(presentation.periods.len());
    for period in &presentation.periods {
        let matches = period
            .adaptation_sets
            .iter()
            .enumerate()
            .flat_map(|(adaptation_index, adaptation)| {
                let contract = &lane.contract;
                adaptation.representations.iter().enumerate().filter_map(
                    move |(representation_index, representation)| {
                        (lane_contract(representation).ok().as_ref() == Some(contract))
                            .then_some((adaptation_index, representation_index))
                    },
                )
            })
            .collect::<Vec<_>>();
        let [location] = matches.as_slice() else {
            return Err(DashRepresentationLaneSelectionError::Absent);
        };
        locations.push(*location);
    }
    Ok(DashLogicalRepresentationLane {
        semantic_key: lane.semantic_key.clone(),
        locations: locations.into_boxed_slice(),
        contract: lane.contract.clone(),
    })
}

impl DashRepresentationLaneCatalog {
    /// Immutable provider-neutral additive catalog.
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        &self.catalog
    }

    /// Exact outer-candidate default после ambiguity-rejecting rematch.
    pub const fn provider_default(&self) -> &ComponentVariantSelection {
        &self.provider_default
    }

    /// Safe sibling diagnostics; valid siblings уже опубликованы независимо.
    pub const fn rejections(&self) -> &[DashRepresentationLaneRejection] {
        &self.rejections
    }

    /// Разрешает neutral selection в opaque logical lane без index/URL leakage.
    pub fn resolve_selection(
        &self,
        selection: &ComponentVariantSelection,
    ) -> Result<DashLogicalRepresentationSelection, DashRepresentationLaneSelectionError> {
        match selection {
            ComponentVariantSelection::VideoAndAudio { video, audio, .. } => {
                let video = self.find_component_lane(video.exact_identity())?;
                let audio = self.find_component_lane(audio.exact_identity())?;
                if video.kind != DashMediaKind::Video || audio.kind != DashMediaKind::Audio {
                    return Err(DashRepresentationLaneSelectionError::Layout);
                }
                Ok(DashLogicalRepresentationSelection::Separate {
                    video: video.lane.clone(),
                    audio: audio.lane.clone(),
                })
            }
            ComponentVariantSelection::VideoOnly { video, .. } => {
                self.single_component(video.exact_identity(), DashMediaKind::Video)
            }
            ComponentVariantSelection::AudioOnly { audio, .. } => {
                self.single_component(audio.exact_identity(), DashMediaKind::Audio)
            }
            ComponentVariantSelection::Coupled { presentation, .. } => self
                .runtime_rows
                .iter()
                .find(|row| {
                    row.coupled_exact.as_ref() == Some(presentation.exact_identity())
                        && row.kind == DashMediaKind::Muxed
                })
                .map(|row| DashLogicalRepresentationSelection::Single(row.lane.clone()))
                .ok_or(DashRepresentationLaneSelectionError::Absent),
        }
    }

    fn single_component(
        &self,
        identity: &ComponentVariantExactIdentity,
        expected: DashMediaKind,
    ) -> Result<DashLogicalRepresentationSelection, DashRepresentationLaneSelectionError> {
        let row = self.find_component_lane(identity)?;
        if row.kind != expected {
            return Err(DashRepresentationLaneSelectionError::Layout);
        }
        Ok(DashLogicalRepresentationSelection::Single(row.lane.clone()))
    }

    fn find_component_lane(
        &self,
        identity: &ComponentVariantExactIdentity,
    ) -> Result<&PublishedLane, DashRepresentationLaneSelectionError> {
        self.runtime_rows
            .iter()
            .find(|row| row.component_exact.as_ref() == Some(identity))
            .ok_or(DashRepresentationLaneSelectionError::Absent)
    }
}
