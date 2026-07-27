use std::fmt;
use std::sync::Arc;

use service_ytdlp::{YtDlpCandidateSelection, YtDlpComposedSelection};
use web_media_core::{CodecFamily, DynamicRange, FrameRate, VideoTrackDescriptor};

use crate::web_media_stream_model::WebMediaStreamGeneration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum WebMediaMode {
    VideoAndAudio,
    VideoOnly,
    AudioOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WebMediaFacet {
    Mode,
    Codec,
    Resolution,
    FrameRate,
    DynamicRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WebMediaFacetOption {
    Mode(WebMediaMode),
    Codec(CodecFamily),
    Resolution { width: u32, height: u32 },
    FrameRate(FrameRate),
    DynamicRange(DynamicRange),
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebMediaFacetAction {
    pub(crate) generation: u64,
    pub(crate) facet: WebMediaFacet,
    pub(crate) option_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaPickerSelector {
    pub(crate) facet: WebMediaFacet,
    pub(crate) options: Arc<[WebMediaFacetOption]>,
    pub(crate) selected_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaPickerProjection {
    pub(crate) generation: u64,
    pub(crate) selectors: Arc<[WebMediaPickerSelector]>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum WebMediaSelectionTarget {
    #[cfg(test)]
    Fixture(u64),
    Parent {
        selection: Box<YtDlpCandidateSelection>,
    },
    Composed {
        selection: Box<YtDlpComposedSelection>,
        parent_preference: Box<YtDlpCandidateSelection>,
    },
}

impl fmt::Debug for WebMediaSelectionTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            #[cfg(test)]
            Self::Fixture(_) => "fixture",
            Self::Parent { .. } => "parent",
            Self::Composed { .. } => "composed",
        };
        formatter
            .debug_struct("WebMediaSelectionTarget")
            .field("kind", &kind)
            .field("identity", &"<opaque>")
            .finish()
    }
}

impl WebMediaSelectionTarget {
    pub(crate) fn remembered(&self) -> WebMediaRememberedPreference {
        match self {
            #[cfg(test)]
            Self::Fixture(_) => panic!("fixture target is not a remembered production intent"),
            Self::Parent { selection } => WebMediaRememberedPreference::Parent(selection.clone()),
            Self::Composed { selection, .. } => {
                WebMediaRememberedPreference::Composed(selection.clone())
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum WebMediaRememberedPreference {
    Parent(Box<YtDlpCandidateSelection>),
    Composed(Box<YtDlpComposedSelection>),
}

impl fmt::Debug for WebMediaRememberedPreference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebMediaRememberedPreference(<opaque-semantic>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WebMediaCatalogChoice {
    pub(crate) mode: WebMediaMode,
    pub(crate) video: Option<VideoTrackDescriptor>,
    pub(crate) rank: web_media_playback_plan::OpaqueAlternativeRank,
    pub(crate) target: WebMediaSelectionTarget,
}

impl fmt::Debug for WebMediaCatalogChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaCatalogChoice")
            .field("mode", &self.mode)
            .field("video", &self.video)
            .field("target", &self.target)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebMediaCatalogSafeError {
    AttachmentMismatch,
    InvalidCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WebMediaCatalogState {
    Inactive,
    Ready(Arc<WebMediaCatalog>),
    Failed {
        parent_generation: WebMediaStreamGeneration,
        error: WebMediaCatalogSafeError,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WebMediaCatalog {
    generation: u64,
    parent_generation: WebMediaStreamGeneration,
    choices: Arc<[WebMediaCatalogChoice]>,
    active_index: usize,
}

impl fmt::Debug for WebMediaCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaCatalog")
            .field("generation", &self.generation)
            .field("parent_generation", &self.parent_generation)
            .field("choice_count", &self.choices.len())
            .field("active_index", &self.active_index)
            .finish()
    }
}

impl WebMediaCatalog {
    pub(super) fn new(
        generation: u64,
        parent_generation: WebMediaStreamGeneration,
        choices: Arc<[WebMediaCatalogChoice]>,
        active: &WebMediaSelectionTarget,
    ) -> Option<Self> {
        let active_index = choices.iter().position(|choice| &choice.target == active)?;
        Some(Self {
            generation,
            parent_generation,
            choices,
            active_index,
        })
    }

    pub(crate) const fn parent_generation(&self) -> WebMediaStreamGeneration {
        self.parent_generation
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn contains_target(&self, target: &WebMediaSelectionTarget) -> bool {
        self.choices.iter().any(|choice| &choice.target == target)
    }

    pub(crate) fn active_choice(&self) -> &WebMediaCatalogChoice {
        &self.choices[self.active_index]
    }

    pub(crate) fn rematch_preference(
        &self,
        preference: &WebMediaRememberedPreference,
    ) -> Option<&WebMediaSelectionTarget> {
        self.choices.iter().find_map(|choice| {
            preference_matches(&choice.target, preference).then_some(&choice.target)
        })
    }

    pub(crate) fn picker_projection(&self) -> WebMediaPickerProjection {
        let active = self.active_choice();
        let selectors = [
            WebMediaFacet::Mode,
            WebMediaFacet::Codec,
            WebMediaFacet::Resolution,
            WebMediaFacet::FrameRate,
            WebMediaFacet::DynamicRange,
        ]
        .into_iter()
        .filter_map(|facet| {
            let options = self.options_for(facet, active);
            (!options.is_empty()).then(|| WebMediaPickerSelector {
                facet,
                selected_index: facet_value(active, facet)
                    .and_then(|selected| options.iter().position(|option| *option == selected)),
                options: options.into(),
            })
        })
        .collect::<Vec<_>>();
        WebMediaPickerProjection {
            generation: self.generation,
            selectors: selectors.into(),
        }
    }

    pub(crate) fn resolve_facet_action(
        &self,
        action: WebMediaFacetAction,
    ) -> Option<&WebMediaSelectionTarget> {
        if action.generation != self.generation {
            return None;
        }
        let active = self.active_choice();
        let selected = *self
            .options_for(action.facet, active)
            .get(action.option_index)?;
        let alternatives = self
            .choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| prefix_matches(active, choice, action.facet))
            .filter(|(_, choice)| facet_value(choice, action.facet) == Some(selected))
            .map(|(choice_index, choice)| {
                web_media_playback_plan::GroupedOpaqueAlternative::new(
                    choice_index,
                    preserved_facet_count(active, choice, action.facet),
                    choice.rank,
                )
            })
            .collect::<Vec<_>>();
        let choice_index =
            web_media_playback_plan::select_grouped_opaque_alternative(&alternatives).ok()?;
        self.choices.get(choice_index).map(|choice| &choice.target)
    }

    fn options_for(
        &self,
        facet: WebMediaFacet,
        active: &WebMediaCatalogChoice,
    ) -> Vec<WebMediaFacetOption> {
        let mut options = self
            .choices
            .iter()
            .filter(|choice| prefix_matches(active, choice, facet))
            .filter_map(|choice| facet_value(choice, facet))
            .collect::<Vec<_>>();
        options.sort_by(compare_facet_options);
        options.dedup();
        options
    }
}

fn preference_matches(
    target: &WebMediaSelectionTarget,
    preference: &WebMediaRememberedPreference,
) -> bool {
    match (target, preference) {
        (
            WebMediaSelectionTarget::Parent { selection: fresh },
            WebMediaRememberedPreference::Parent(previous),
        ) => fresh.semantic_identity() == previous.semantic_identity(),
        (
            WebMediaSelectionTarget::Composed {
                selection: fresh, ..
            },
            WebMediaRememberedPreference::Composed(previous),
        ) => {
            fresh.descriptor().semantic_identity() == previous.descriptor().semantic_identity()
                && fresh.audio_semantic_identity() == previous.audio_semantic_identity()
        }
        #[cfg(test)]
        (WebMediaSelectionTarget::Fixture(fresh), _) => {
            let _ = fresh;
            false
        }
        _ => false,
    }
}

fn prefix_matches(
    active: &WebMediaCatalogChoice,
    candidate: &WebMediaCatalogChoice,
    facet: WebMediaFacet,
) -> bool {
    let order = [
        WebMediaFacet::Mode,
        WebMediaFacet::Codec,
        WebMediaFacet::Resolution,
        WebMediaFacet::FrameRate,
        WebMediaFacet::DynamicRange,
    ];
    order
        .into_iter()
        .take_while(|current| *current != facet)
        .all(|current| facet_value(active, current) == facet_value(candidate, current))
}

fn preserved_facet_count(
    active: &WebMediaCatalogChoice,
    candidate: &WebMediaCatalogChoice,
    changed: WebMediaFacet,
) -> usize {
    [
        WebMediaFacet::Mode,
        WebMediaFacet::Codec,
        WebMediaFacet::Resolution,
        WebMediaFacet::FrameRate,
        WebMediaFacet::DynamicRange,
    ]
    .into_iter()
    .filter(|facet| *facet != changed)
    .filter(|facet| facet_value(active, *facet) == facet_value(candidate, *facet))
    .count()
}

fn facet_value(
    choice: &WebMediaCatalogChoice,
    facet: WebMediaFacet,
) -> Option<WebMediaFacetOption> {
    match facet {
        WebMediaFacet::Mode => Some(WebMediaFacetOption::Mode(choice.mode)),
        WebMediaFacet::Codec => choice
            .video
            .as_ref()
            .map(|video| match video.codec().kind() {
                web_media_core::CodecKind::Known(codec) => WebMediaFacetOption::Codec(codec),
                // Deferred HLS и прочие absent/unknown не прячут selector — показывают «Авто».
                web_media_core::CodecKind::Absent | web_media_core::CodecKind::Unknown => {
                    WebMediaFacetOption::Automatic
                }
            }),
        WebMediaFacet::Resolution => choice.video.as_ref().and_then(|video| {
            let height = video.height()?.pixels();
            // Width может отсутствовать у deferred ladder; height остаётся discriminator.
            Some(WebMediaFacetOption::Resolution {
                width: video.width_pixels().unwrap_or(0),
                height,
            })
        }),
        WebMediaFacet::FrameRate => choice.video.as_ref().map(|video| {
            video.frame_rate().map_or(
                WebMediaFacetOption::Automatic,
                WebMediaFacetOption::FrameRate,
            )
        }),
        WebMediaFacet::DynamicRange => {
            choice
                .video
                .as_ref()
                .map(|video| match video.dynamic_range() {
                    DynamicRange::Unknown => WebMediaFacetOption::Automatic,
                    range => WebMediaFacetOption::DynamicRange(range),
                })
        }
    }
}

fn compare_facet_options(
    left: &WebMediaFacetOption,
    right: &WebMediaFacetOption,
) -> std::cmp::Ordering {
    facet_option_rank(left)
        .cmp(&facet_option_rank(right))
        .then_with(|| left_option_key(left).cmp(&left_option_key(right)))
}

fn facet_option_rank(option: &WebMediaFacetOption) -> u8 {
    match option {
        WebMediaFacetOption::Mode(WebMediaMode::VideoAndAudio) => 0,
        WebMediaFacetOption::Mode(WebMediaMode::VideoOnly) => 1,
        WebMediaFacetOption::Mode(WebMediaMode::AudioOnly) => 2,
        WebMediaFacetOption::Codec(CodecFamily::H264) => 10,
        WebMediaFacetOption::Codec(CodecFamily::H265) => 11,
        WebMediaFacetOption::Codec(CodecFamily::Vp8) => 12,
        WebMediaFacetOption::Codec(CodecFamily::Vp9) => 13,
        WebMediaFacetOption::Codec(CodecFamily::Av1) => 14,
        WebMediaFacetOption::Codec(_) => 15,
        WebMediaFacetOption::Resolution { .. } => 20,
        WebMediaFacetOption::FrameRate(_) => 30,
        WebMediaFacetOption::DynamicRange(DynamicRange::Hdr) => 40,
        WebMediaFacetOption::DynamicRange(DynamicRange::Sdr) => 41,
        WebMediaFacetOption::DynamicRange(DynamicRange::Unknown) => 42,
        WebMediaFacetOption::Automatic => 50,
    }
}

fn left_option_key(option: &WebMediaFacetOption) -> (u64, u64) {
    match option {
        WebMediaFacetOption::Resolution { width, height } => {
            (u64::from(u32::MAX - *height), u64::from(u32::MAX - *width))
        }
        WebMediaFacetOption::FrameRate(rate) => {
            let scaled = u64::from(rate.numerator()).saturating_mul(1_000_000)
                / u64::from(rate.denominator());
            (u64::MAX - scaled, 0)
        }
        _ => (0, 0),
    }
}
