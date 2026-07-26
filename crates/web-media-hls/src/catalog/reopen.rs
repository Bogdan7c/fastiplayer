use std::fmt;

use hls_playlist_core::{
    ExactReference, MasterPlaylist, MediaRendition, MediaRenditionType, VariantStream,
};
use web_media_core::{
    ComponentVariantCatalog, ComponentVariantError, ComponentVariantExactIdentity,
    ComponentVariantSelection, ComponentVariantSemanticSelectionRequest,
    CoupledVariantExactIdentity,
};

use crate::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsMainTrackLayoutIntent,
    HlsRequiredContainer, HlsSubtitleRenditionDescriptor, HlsVariantSelectionIntent,
};

#[derive(Clone, Debug)]
pub(super) struct HlsCatalogRuntimeVideoRow {
    pub(super) identity: ComponentVariantExactIdentity,
    pub(super) variant: VariantStream,
    pub(super) container: HlsRequiredContainer,
}

#[derive(Clone, Debug)]
pub(super) enum HlsCatalogRuntimeAudioSource {
    Variant(VariantStream),
    Rendition(MediaRendition),
}

#[derive(Clone, Debug)]
pub(super) struct HlsCatalogRuntimeAudioRow {
    pub(super) identity: ComponentVariantExactIdentity,
    pub(super) source: HlsCatalogRuntimeAudioSource,
    pub(super) container: HlsRequiredContainer,
}

#[derive(Clone, Debug)]
pub(super) struct HlsCatalogRuntimeCoupledRow {
    pub(super) identity: CoupledVariantExactIdentity,
    pub(super) variant: VariantStream,
    pub(super) container: HlsRequiredContainer,
}

#[derive(Clone, Debug)]
pub(super) struct HlsCatalogRuntimeMap {
    pub(super) videos: Box<[HlsCatalogRuntimeVideoRow]>,
    pub(super) audios: Box<[HlsCatalogRuntimeAudioRow]>,
    pub(super) coupled: Box<[HlsCatalogRuntimeCoupledRow]>,
}

/// Opaque provider intent, созданный только validated catalog selection-ом.
#[derive(Clone)]
pub struct HlsCatalogReopenSelection {
    semantic: ComponentVariantSemanticSelectionRequest,
    main: HlsPrivateMainSelection,
    audio: Option<HlsPrivateAudioSelection>,
}

#[derive(Clone)]
enum HlsPrivateMainSelection {
    Variant {
        row: VariantStream,
        container: HlsRequiredContainer,
        shape: HlsMainTrackLayoutIntent,
    },
    Rendition {
        row: MediaRendition,
        container: HlsRequiredContainer,
    },
}

#[derive(Clone)]
struct HlsPrivateAudioSelection {
    row: MediaRendition,
    container: HlsRequiredContainer,
}

pub(crate) struct HlsResolvedCatalogSelection {
    pub(crate) main_reference: ExactReference,
    pub(crate) main_container: HlsRequiredContainer,
    pub(crate) main_shape: HlsMainTrackLayoutIntent,
    pub(crate) audio: Option<HlsResolvedCatalogAudio>,
    pub(crate) subtitles: Vec<HlsSubtitleRenditionDescriptor>,
}

pub(crate) struct HlsResolvedCatalogAudio {
    pub(crate) reference: ExactReference,
    pub(crate) container: HlsRequiredContainer,
}

#[derive(Clone, Copy)]
pub(crate) enum HlsCatalogMatchMode {
    Exact,
    Semantic,
}

impl fmt::Debug for HlsCatalogReopenSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsCatalogReopenSelection")
            .field("semantic", &self.semantic)
            .field("private_resources", &"<redacted>")
            .finish()
    }
}

impl HlsCatalogRuntimeMap {
    pub(super) fn resolve_exact(
        &self,
        catalog: &ComponentVariantCatalog,
        selection: &ComponentVariantSelection,
    ) -> Result<HlsCatalogReopenSelection, HlsCatalogReopenError> {
        let canonical = catalog.select_exact(selection.exact_selection_request())?;
        if &canonical != selection {
            return Err(HlsCatalogReopenError::SelectionMismatch);
        }
        let semantic = canonical.semantic_rematch_request();
        match canonical {
            ComponentVariantSelection::VideoAndAudio { video, audio, .. } => {
                let video_row = self.video(video.exact_identity())?;
                let audio_row = self.audio(audio.exact_identity())?;
                let HlsCatalogRuntimeAudioSource::Rendition(rendition) = &audio_row.source else {
                    return Err(HlsCatalogReopenError::InvalidPrivateTopology);
                };
                Ok(HlsCatalogReopenSelection {
                    semantic,
                    main: HlsPrivateMainSelection::Variant {
                        row: video_row.variant.clone(),
                        container: video_row.container,
                        shape: HlsMainTrackLayoutIntent::VideoOnly,
                    },
                    audio: Some(HlsPrivateAudioSelection {
                        row: rendition.clone(),
                        container: audio_row.container,
                    }),
                })
            }
            ComponentVariantSelection::VideoOnly { video, .. } => {
                let row = self.video(video.exact_identity())?;
                Ok(HlsCatalogReopenSelection {
                    semantic,
                    main: HlsPrivateMainSelection::Variant {
                        row: row.variant.clone(),
                        container: row.container,
                        shape: HlsMainTrackLayoutIntent::VideoOnly,
                    },
                    audio: None,
                })
            }
            ComponentVariantSelection::AudioOnly { audio, .. } => {
                let row = self.audio(audio.exact_identity())?;
                let main = match &row.source {
                    HlsCatalogRuntimeAudioSource::Variant(variant) => {
                        HlsPrivateMainSelection::Variant {
                            row: variant.clone(),
                            container: row.container,
                            shape: HlsMainTrackLayoutIntent::AudioOnly,
                        }
                    }
                    HlsCatalogRuntimeAudioSource::Rendition(rendition) => {
                        HlsPrivateMainSelection::Rendition {
                            row: rendition.clone(),
                            container: row.container,
                        }
                    }
                };
                Ok(HlsCatalogReopenSelection {
                    semantic,
                    main,
                    audio: None,
                })
            }
            ComponentVariantSelection::Coupled { presentation, .. } => {
                let row = self.coupled(presentation.exact_identity())?;
                Ok(HlsCatalogReopenSelection {
                    semantic,
                    main: HlsPrivateMainSelection::Variant {
                        row: row.variant.clone(),
                        container: row.container,
                        shape: HlsMainTrackLayoutIntent::MuxedAv,
                    },
                    audio: None,
                })
            }
        }
    }

    fn video(
        &self,
        identity: &ComponentVariantExactIdentity,
    ) -> Result<&HlsCatalogRuntimeVideoRow, HlsCatalogReopenError> {
        unique(self.videos.iter().filter(|row| &row.identity == identity))
    }

    fn audio(
        &self,
        identity: &ComponentVariantExactIdentity,
    ) -> Result<&HlsCatalogRuntimeAudioRow, HlsCatalogReopenError> {
        unique(self.audios.iter().filter(|row| &row.identity == identity))
    }

    fn coupled(
        &self,
        identity: &CoupledVariantExactIdentity,
    ) -> Result<&HlsCatalogRuntimeCoupledRow, HlsCatalogReopenError> {
        unique(self.coupled.iter().filter(|row| &row.identity == identity))
    }
}

impl HlsCatalogReopenSelection {
    pub(crate) fn resolve_master(
        &self,
        master: &MasterPlaylist,
        mode: HlsCatalogMatchMode,
    ) -> Result<HlsResolvedCatalogSelection, HlsCatalogReopenError> {
        let (main_reference, main_container, main_shape, variant) = match &self.main {
            HlsPrivateMainSelection::Variant {
                row,
                container,
                shape,
            } => {
                let variant = match mode {
                    HlsCatalogMatchMode::Exact => {
                        unique(master.variants.iter().filter(|candidate| *candidate == row))?
                    }
                    HlsCatalogMatchMode::Semantic => unique(
                        master
                            .variants
                            .iter()
                            .filter(|candidate| variant_semantically_matches(candidate, row)),
                    )?,
                };
                (variant.uri.clone(), *container, *shape, Some(variant))
            }
            HlsPrivateMainSelection::Rendition { row, container } => {
                let rendition = resolve_rendition(master, row, mode)?;
                let reference = rendition
                    .uri
                    .clone()
                    .ok_or(HlsCatalogReopenError::MissingPrivateRow)?;
                (
                    reference,
                    *container,
                    HlsMainTrackLayoutIntent::AudioOnly,
                    None,
                )
            }
        };
        let audio = self
            .audio
            .as_ref()
            .map(|audio| {
                let rendition = resolve_rendition(master, &audio.row, mode)?;
                Ok::<_, HlsCatalogReopenError>(HlsResolvedCatalogAudio {
                    reference: rendition
                        .uri
                        .clone()
                        .ok_or(HlsCatalogReopenError::MissingPrivateRow)?,
                    container: audio.container,
                })
            })
            .transpose()?;
        let subtitles = variant
            .and_then(|variant| variant.subtitle_group.as_deref())
            .map(|group| {
                master
                    .renditions
                    .iter()
                    .filter(|rendition| {
                        rendition.rendition_type == MediaRenditionType::Subtitles
                            && rendition.group_id.as_ref() == group
                    })
                    .filter_map(HlsSubtitleRenditionDescriptor::from_rendition)
                    .collect()
            })
            .unwrap_or_default();
        Ok(HlsResolvedCatalogSelection {
            main_reference,
            main_container,
            main_shape,
            audio,
            subtitles,
        })
    }

    pub(crate) fn runtime_intent(&self) -> HlsVariantSelectionIntent {
        let (resolution, codecs, main_track_layout) = match &self.main {
            HlsPrivateMainSelection::Variant { row, shape, .. } => (
                row.resolution.and_then(|(width, height)| {
                    Some((
                        std::num::NonZeroU32::new(width)?,
                        std::num::NonZeroU32::new(height)?,
                    ))
                }),
                row.codecs.clone(),
                *shape,
            ),
            HlsPrivateMainSelection::Rendition { .. } => {
                (None, None, HlsMainTrackLayoutIntent::AudioOnly)
            }
        };
        let audio = self
            .audio
            .as_ref()
            .map_or(HlsAudioLayoutIntent::Muxed, |audio| {
                HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
                    name: Some(audio.row.name.clone()),
                    language: audio.row.language.clone(),
                    channel_count: audio
                        .row
                        .channel_count
                        .and_then(|count| u16::try_from(count.get()).ok())
                        .and_then(std::num::NonZeroU16::new),
                })
            });
        HlsVariantSelectionIntent {
            resolution,
            codecs,
            audio,
            main_track_layout,
        }
    }
}

fn resolve_rendition<'master>(
    master: &'master MasterPlaylist,
    row: &MediaRendition,
    mode: HlsCatalogMatchMode,
) -> Result<&'master MediaRendition, HlsCatalogReopenError> {
    match mode {
        HlsCatalogMatchMode::Exact => unique(
            master
                .renditions
                .iter()
                .filter(|candidate| *candidate == row),
        ),
        HlsCatalogMatchMode::Semantic => unique(
            master
                .renditions
                .iter()
                .filter(|candidate| rendition_semantically_matches(candidate, row)),
        ),
    }
}

fn variant_semantically_matches(left: &VariantStream, right: &VariantStream) -> bool {
    left.bandwidth == right.bandwidth
        && left.average_bandwidth == right.average_bandwidth
        && codec_sets_equal(left.codecs.as_deref(), right.codecs.as_deref())
        && left.resolution == right.resolution
        && left.frame_rate == right.frame_rate
        && left.video_range == right.video_range
        && left.audio_group == right.audio_group
        && left.video_group == right.video_group
        && left.subtitle_group == right.subtitle_group
        && left.closed_captions == right.closed_captions
        && left.requires_output_protection == right.requires_output_protection
}

fn codec_sets_equal(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            let mut left = left.split(',').map(str::trim).collect::<Vec<_>>();
            let mut right = right.split(',').map(str::trim).collect::<Vec<_>>();
            left.sort_unstable();
            right.sort_unstable();
            left == right
        }
        (None, None) => true,
        _ => false,
    }
}

fn rendition_semantically_matches(left: &MediaRendition, right: &MediaRendition) -> bool {
    left.rendition_type == right.rendition_type
        && left.group_id == right.group_id
        && left.name == right.name
        && left.language == right.language
        && left.associated_language == right.associated_language
        && left.characteristics == right.characteristics
        && left.channel_count == right.channel_count
        && left.channels == right.channels
        && left.is_default == right.is_default
        && left.autoselect == right.autoselect
        && left.forced == right.forced
}

fn unique<'a, T>(mut rows: impl Iterator<Item = &'a T>) -> Result<&'a T, HlsCatalogReopenError> {
    let row = rows
        .next()
        .ok_or(HlsCatalogReopenError::MissingPrivateRow)?;
    if rows.next().is_some() {
        return Err(HlsCatalogReopenError::AmbiguousPrivateRow);
    }
    Ok(row)
}

/// Exact/semantic reopen никогда не заменяется provider default-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HlsCatalogReopenError {
    #[error("neutral catalog selection rejected: {0}")]
    Catalog(#[from] ComponentVariantError),
    #[error("selection is not the canonical row from this catalog")]
    SelectionMismatch,
    #[error("private HLS row is missing in current manifest")]
    MissingPrivateRow,
    #[error("private HLS semantic row is ambiguous in current manifest")]
    AmbiguousPrivateRow,
    #[error("neutral selection cannot be represented by HLS runtime topology")]
    InvalidPrivateTopology,
}
