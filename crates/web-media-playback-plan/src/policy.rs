use std::collections::HashSet;

use codec_core::VideoCodec;
use web_media_core::{ContainerFamily, PreferredHeightPolicy};

use crate::candidate::PlanningResourceLayout;

/// Neutral HDR bucket policy; config/service enum сюда не протекает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrSelectionPolicy {
    /// Допускается только доказанный SDR.
    SdrOnly,
    /// Playable HDR bucket сильнее SDR fallback bucket-а.
    PreferHdrWhenAvailable,
}

/// Полная pure selection policy после capability intersection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackSelectionPolicy {
    /// HDR bucket всегда сильнее codec/height/container/quality ranks.
    hdr: HdrSelectionPolicy,
    /// Existing configured video codec order.
    preferred_video_codecs: Box<[VideoCodec]>,
    /// S20Q preferred-height ordering.
    preferred_height: PreferredHeightPolicy,
    /// Explicit container tie-break без hardcode в planner-е.
    preferred_containers: Box<[ContainerFamily]>,
}

impl PlaybackSelectionPolicy {
    /// Проверяет duplicate-free deterministic preference lists.
    pub fn new(
        hdr: HdrSelectionPolicy,
        preferred_video_codecs: Vec<VideoCodec>,
        preferred_height: PreferredHeightPolicy,
        preferred_containers: Vec<ContainerFamily>,
    ) -> Result<Self, SelectionPolicyBuildError> {
        if contains_duplicate(&preferred_video_codecs) {
            return Err(SelectionPolicyBuildError::DuplicateVideoCodec);
        }
        if contains_duplicate(&preferred_containers) {
            return Err(SelectionPolicyBuildError::DuplicateContainer);
        }
        if preferred_containers.iter().any(|container| {
            matches!(
                container,
                ContainerFamily::MpegProgramStream
                    | ContainerFamily::Avi
                    | ContainerFamily::Asf
                    | ContainerFamily::Unknown
            )
        }) {
            return Err(SelectionPolicyBuildError::StaticRejectedContainer);
        }

        Ok(Self {
            hdr,
            preferred_video_codecs: preferred_video_codecs.into_boxed_slice(),
            preferred_height,
            preferred_containers: preferred_containers.into_boxed_slice(),
        })
    }

    /// Возвращает neutral HDR policy.
    pub const fn hdr(&self) -> HdrSelectionPolicy {
        self.hdr
    }

    /// Возвращает configured video codec order.
    pub const fn preferred_video_codecs(&self) -> &[VideoCodec] {
        &self.preferred_video_codecs
    }

    /// Возвращает S20Q height policy.
    pub const fn preferred_height(&self) -> PreferredHeightPolicy {
        self.preferred_height
    }

    /// Возвращает explicit container order.
    pub const fn preferred_containers(&self) -> &[ContainerFamily] {
        &self.preferred_containers
    }

    /// Возвращает codec rank либо `None`, если video codec исключён policy.
    pub(crate) fn video_codec_rank(&self, codec: Option<VideoCodec>) -> Option<usize> {
        match codec {
            None => Some(0),
            Some(codec) => self
                .preferred_video_codecs
                .iter()
                .position(|preferred| *preferred == codec),
        }
    }

    /// Строит deterministic container rank для одной layout shape.
    pub(crate) fn container_rank(
        &self,
        resources: PlanningResourceLayout,
    ) -> ContainerPreferenceRank {
        let rank = |container| {
            self.preferred_containers
                .iter()
                .position(|preferred| *preferred == container)
                .unwrap_or(self.preferred_containers.len())
        };

        match resources {
            PlanningResourceLayout::Muxed(resource)
            | PlanningResourceLayout::VideoOnly(resource)
            | PlanningResourceLayout::AudioOnly(resource) => ContainerPreferenceRank {
                primary: rank(resource.container),
                secondary: 0,
            },
            PlanningResourceLayout::Separate { video, audio } => ContainerPreferenceRank {
                primary: rank(video.container),
                secondary: rank(audio.container),
            },
        }
    }
}

/// Container tie-break: video/muxed primary, separate audio secondary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContainerPreferenceRank {
    /// Muxed/video/audio-only container либо separate video container.
    primary: usize,
    /// Separate audio container; для single-resource layout равен нулю.
    secondary: usize,
}

impl ContainerPreferenceRank {
    /// Возвращает primary rank для diagnostics/tests.
    pub const fn primary(self) -> usize {
        self.primary
    }

    /// Возвращает secondary rank для diagnostics/tests.
    pub const fn secondary(self) -> usize {
        self.secondary
    }
}

/// Ошибка построения deterministic policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPolicyBuildError {
    /// Video codec order содержит duplicate.
    DuplicateVideoCodec,
    /// Container order содержит duplicate.
    DuplicateContainer,
    /// Container preference пытается вернуть static profile exclusion.
    StaticRejectedContainer,
}

impl std::fmt::Display for SelectionPolicyBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "selection policy недействительна: {self:?}")
    }
}

impl std::error::Error for SelectionPolicyBuildError {}

/// Ищет duplicate без изменения caller order.
fn contains_duplicate<Value>(values: &[Value]) -> bool
where
    Value: Copy + Eq + std::hash::Hash,
{
    let mut unique = HashSet::with_capacity(values.len());
    values.iter().copied().any(|value| !unique.insert(value))
}
