//! Secret-safe read-only модель активной web-media конфигурации для URL sidebar.
//!
//! Модуль не владеет queue, transport или media-open lifecycle. Он получает уже
//! проверенный candidate snapshot до открытия транспорта, сохраняет только
//! безопасное описание форматов и строит view model из exact Installed source.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use player_core::{PlaybackState, PlayerSnapshot};
use service_ytdlp::{YtDlpCandidateSelection, YtDlpCandidateSnapshot};
use web_media_core::{
    CandidateDescriptor, CodecFamily, CodecKind, ContainerFamily, DynamicRange,
    ExactSelectionIdentity, StreamLayout, StreamLayoutKind, WebMediaSelection,
};
use web_media_playback_plan::{
    PlanningCandidateSnapshot, PlaybackCapabilitySnapshot, PlaybackSelectionPolicy, plan_playback,
};

use crate::media_open::ActiveMediaSource;
use crate::playlist_runtime::PlaylistViewModel;

pub(crate) mod component_variants;
use component_variants::{
    ActiveParentCandidateSelection, WebMediaComponentVariantConfiguration,
    WebMediaComponentVariantProjection,
};
mod sidebar_action;
pub(crate) use sidebar_action::{
    UrlSidebarAction, UrlSidebarPendingSelection, UrlSidebarTransitionError,
};
mod sidebar_controller;
pub(crate) use sidebar_controller::UrlSidebarController;
#[cfg(test)]
use sidebar_controller::{ItemOverrideState, SafeErrorState};

#[cfg(test)]
pub(crate) mod component_variants_tests;

/// Поколение extraction snapshot-а без candidate format identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebMediaStreamGeneration {
    source: u64,
    extraction: u64,
}

impl WebMediaStreamGeneration {
    /// Строит generation fence из установленного exact selection token-а.
    #[must_use]
    pub(crate) fn from_selection(selection: &YtDlpCandidateSelection) -> Self {
        let identity = selection.exact_identity();
        Self {
            source: identity.source().value(),
            extraction: identity.generation().value(),
        }
    }

    /// Строит synthetic generation только для hermetic UI/state тестов.
    #[cfg(test)]
    pub(crate) const fn for_test(source: u64, extraction: u64) -> Self {
        Self { source, extraction }
    }

    /// Проверяет, что fresh extraction принадлежит той же source lineage.
    #[must_use]
    pub(crate) const fn has_same_source_lineage(self, other: Self) -> bool {
        self.source == other.source
    }
}

/// Источник runtime preference, показанный пользователю без queue mutation API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebMediaSelectionPreference {
    /// Глобальная политика выбирает лучший playable candidate без заданной высоты.
    GlobalBestPlayable,
    /// Глобальная config предпочитает указанную высоту video.
    GlobalPreferredHeight(u32),
    /// Process-lifetime override конкретного queue item-а, принадлежащий будущему S25.
    ItemOverride(Option<u32>),
}

impl WebMediaSelectionPreference {
    /// Проецирует current global config в явную preference semantics.
    #[must_use]
    pub(crate) fn from_global_config(config: &rustiplayer_config::WebMediaConfig) -> Self {
        match config.preferred_video_height {
            Some(height) => Self::GlobalPreferredHeight(height.pixels()),
            None => Self::GlobalBestPlayable,
        }
    }
}

/// Безопасная пара container families для single либо separate A/V candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebMediaContainerSummary {
    pub(crate) video: Option<ContainerFamily>,
    pub(crate) audio: Option<ContainerFamily>,
}

/// Безопасное описание одного реально playable candidate-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaCandidatePresentation {
    pub(crate) layout: StreamLayoutKind,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) frame_rate: Option<(u32, u32)>,
    pub(crate) video_bitrate: Option<u64>,
    pub(crate) audio_bitrate: Option<u64>,
    pub(crate) video_codec: Option<CodecFamily>,
    pub(crate) audio_codec: Option<CodecFamily>,
    pub(crate) dynamic_range: Option<DynamicRange>,
    pub(crate) containers: WebMediaContainerSummary,
}

impl WebMediaCandidatePresentation {
    /// Извлекает только enum/numeric metadata; raw service identities не пересекают UI boundary.
    fn from_descriptor(
        descriptor: &CandidateDescriptor,
    ) -> Result<Self, WebMediaStreamModelBuildError> {
        let (
            video,
            audio,
            deferred_height,
            deferred_width,
            deferred_frame_rate,
            deferred_bitrate,
            deferred_dynamic_range,
            video_container,
            audio_container,
        ) = match descriptor.layout() {
            StreamLayout::Muxed(component) => (
                Some(component.video()),
                Some(component.audio()),
                None,
                None,
                None,
                None,
                None,
                Some(consistent_container(component.container())?),
                Some(consistent_container(component.container())?),
            ),
            StreamLayout::Separate { video, audio } => (
                Some(video.video()),
                Some(audio.audio()),
                None,
                None,
                None,
                None,
                None,
                Some(consistent_container(video.container())?),
                Some(consistent_container(audio.container())?),
            ),
            StreamLayout::VideoOnly(component) => (
                Some(component.video()),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(consistent_container(component.container())?),
                None,
            ),
            StreamLayout::AudioOnly(component) => (
                None,
                Some(component.audio()),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(consistent_container(component.container())?),
            ),
            StreamLayout::HlsMuxedCodecDeferred(component) => (
                None,
                None,
                Some(component.height().pixels()),
                component.width().map(web_media_core::VideoWidth::pixels),
                component
                    .frame_rate()
                    .map(|rate| (rate.numerator(), rate.denominator())),
                component.bitrate().map(|rate| rate.bits_per_second()),
                Some(component.dynamic_range()),
                Some(consistent_container(component.container())?),
                None,
            ),
            StreamLayout::ContentProbed(component) => {
                let hints = component.video_hints();
                let video_available = !component.video().is_absent();
                let audio_available = !component.audio().is_absent();
                let container = consistent_container(component.container())?;
                (
                    component.video().declared(),
                    component.audio().declared(),
                    video_available
                        .then(|| hints.height().map(web_media_core::VideoHeight::pixels))
                        .flatten(),
                    video_available
                        .then(|| hints.width().map(web_media_core::VideoWidth::pixels))
                        .flatten(),
                    video_available
                        .then(|| {
                            hints
                                .frame_rate()
                                .map(|rate| (rate.numerator(), rate.denominator()))
                        })
                        .flatten(),
                    video_available
                        .then(|| hints.bitrate().map(|rate| rate.bits_per_second()))
                        .flatten(),
                    video_available.then(|| hints.dynamic_range()),
                    video_available.then_some(container),
                    audio_available.then_some(container),
                )
            }
        };

        Ok(Self {
            layout: descriptor.layout().kind(),
            width: video
                .and_then(|track| track.width_pixels())
                .or(deferred_width),
            height: video
                .and_then(|track| track.height().map(|height| height.pixels()))
                .or(deferred_height),
            frame_rate: video
                .and_then(|track| {
                    track
                        .frame_rate()
                        .map(|rate| (rate.numerator(), rate.denominator()))
                })
                .or(deferred_frame_rate),
            video_bitrate: video
                .and_then(|track| track.bitrate().map(|rate| rate.bits_per_second()))
                .or(deferred_bitrate),
            audio_bitrate: audio
                .and_then(|track| track.bitrate().map(|rate| rate.bits_per_second())),
            video_codec: video.and_then(|track| known_codec(track.codec().kind())),
            audio_codec: audio.and_then(|track| known_codec(track.codec().kind())),
            dynamic_range: video
                .map(|track| track.dynamic_range())
                .or(deferred_dynamic_range),
            containers: WebMediaContainerSummary {
                video: video_container,
                audio: audio_container,
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn has_video(&self) -> bool {
        self.height.is_some() || self.video_codec.is_some()
    }
}

/// Installed конфигурация YtDlp source с safe projection и закрытыми switch tokens.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WebMediaStreamConfiguration {
    generation: WebMediaStreamGeneration,
    active_parent: ExactSelectionIdentity,
    active_parent_selection: ActiveParentCandidateSelection,
    candidates: Arc<[WebMediaCandidatePresentation]>,
    candidate_selections: Arc<[YtDlpCandidateSelection]>,
    active_candidate: WebMediaCandidatePresentation,
    preference: WebMediaSelectionPreference,
    component_variants: WebMediaComponentVariantConfiguration,
    hls_subtitle_renditions: Arc<[crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition]>,
}

impl fmt::Debug for WebMediaStreamConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaStreamConfiguration")
            .field("generation", &self.generation)
            .field("candidate_count", &self.candidates.len())
            .field("active_candidate", &self.active_candidate)
            .field("preference", &self.preference)
            .field("component_variants", &self.component_variants)
            .field(
                "hls_subtitle_rendition_count",
                &self.hls_subtitle_renditions().len(),
            )
            .finish()
    }
}

impl WebMediaStreamConfiguration {
    /// Строит inventory только из candidates, которые S21C planning признаёт playable.
    #[cfg(test)]
    pub(crate) fn from_yt_dlp_snapshot(
        candidate_snapshot: &YtDlpCandidateSnapshot,
        planning_snapshot: &PlanningCandidateSnapshot,
        capabilities: PlaybackCapabilitySnapshot<'_>,
        policy: &PlaybackSelectionPolicy,
        active_selection: &YtDlpCandidateSelection,
        preference: WebMediaSelectionPreference,
    ) -> Result<Self, WebMediaStreamModelBuildError> {
        let active_parent = ExactSelectionIdentity::new(
            active_selection.exact_identity().clone(),
            active_selection.semantic_identity().clone(),
        )
        .map_err(|_| WebMediaStreamModelBuildError::InvalidActiveCandidateIdentity)?;
        let neutral_selection = WebMediaSelection::candidate(active_parent);
        Self::from_yt_dlp_snapshot_with_neutral_selection(
            candidate_snapshot,
            planning_snapshot,
            capabilities,
            policy,
            active_selection,
            &neutral_selection,
            preference,
        )
    }

    /// Строит UI inventory, переиспользуя N01 neutral active selection adapter-а.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_yt_dlp_snapshot_with_neutral_selection(
        candidate_snapshot: &YtDlpCandidateSnapshot,
        planning_snapshot: &PlanningCandidateSnapshot,
        capabilities: PlaybackCapabilitySnapshot<'_>,
        policy: &PlaybackSelectionPolicy,
        active_selection: &YtDlpCandidateSelection,
        neutral_selection: &WebMediaSelection,
        preference: WebMediaSelectionPreference,
    ) -> Result<Self, WebMediaStreamModelBuildError> {
        candidate_snapshot
            .validate_planning_snapshot_alignment(planning_snapshot)
            .map_err(|_| WebMediaStreamModelBuildError::CandidateSnapshotAlignmentFailed)?;
        let active_identity = active_selection.exact_identity();
        let active_parent = neutral_selection.parent().clone();
        if active_parent.exact() != active_identity
            || active_parent.semantic() != active_selection.semantic_identity()
        {
            return Err(WebMediaStreamModelBuildError::InvalidActiveCandidateIdentity);
        }
        // BestPlayable оценивает весь inventory один раз и возвращает typed rejection
        // каждого недоступного candidate-а; source order не участвует в selection.
        let availability = plan_playback(
            planning_snapshot,
            capabilities,
            &web_media_core::SelectionRequest::BestPlayable,
            policy,
        )
        .map_err(|_| WebMediaStreamModelBuildError::AvailabilityPlanningFailed)?;
        let rejected_identities: HashSet<_> = availability
            .rejected_candidates()
            .iter()
            .map(|rejection| rejection.exact_identity())
            .collect();
        let mut candidates = Vec::new();
        let mut candidate_selections = Vec::new();
        let mut active_candidate = None;

        for candidate in candidate_snapshot.accepted_candidates() {
            let descriptor = candidate.descriptor();
            let playable = !rejected_identities.contains(descriptor.identity());
            let selection = candidate_snapshot
                .selection_for(candidate)
                .map_err(|_| WebMediaStreamModelBuildError::CandidateSelectionFailed)?;
            let is_active = &selection == active_selection;
            if !playable {
                if is_active {
                    return Err(WebMediaStreamModelBuildError::ActiveCandidateNotPlayable);
                }
                continue;
            }

            let presentation = WebMediaCandidatePresentation::from_descriptor(descriptor)?;
            if is_active {
                active_candidate = Some(presentation.clone());
            }
            candidates.push(presentation);
            candidate_selections.push(selection);
        }

        let active_candidate =
            active_candidate.ok_or(WebMediaStreamModelBuildError::ActiveCandidateMissing)?;
        Ok(Self {
            generation: WebMediaStreamGeneration::from_selection(active_selection),
            active_parent,
            active_parent_selection: ActiveParentCandidateSelection::Installed(Box::new(
                active_selection.clone(),
            )),
            candidates: candidates.into(),
            candidate_selections: candidate_selections.into(),
            active_candidate,
            preference,
            component_variants: WebMediaComponentVariantConfiguration::Unavailable,
            hls_subtitle_renditions: Arc::from([]),
        })
    }

    #[must_use]
    pub(crate) fn generation(&self) -> WebMediaStreamGeneration {
        self.generation
    }

    #[must_use]
    pub(crate) fn preference(&self) -> WebMediaSelectionPreference {
        self.preference
    }

    #[must_use]
    pub(crate) fn candidates(&self) -> &[WebMediaCandidatePresentation] {
        &self.candidates
    }

    #[must_use]
    pub(crate) fn active_candidate(&self) -> &WebMediaCandidatePresentation {
        &self.active_candidate
    }

    /// Связывает descriptors только с exact подготовленным HLS candidate-ом.
    pub(crate) fn with_hls_subtitle_renditions(
        mut self,
        renditions: Arc<[crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition]>,
    ) -> Self {
        self.hls_subtitle_renditions = renditions;
        self
    }

    /// Возвращает installed descriptors без URI и без возможности скрытого fetch-а.
    pub(crate) fn hls_subtitle_renditions(
        &self,
    ) -> &[crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition] {
        &self.hls_subtitle_renditions
    }

    /// Возвращает exact+semantic token только после generation/index validation.
    #[cfg(test)]
    pub(crate) fn candidate_selection_for_switch(
        &self,
        generation: WebMediaStreamGeneration,
        candidate_index: usize,
    ) -> Option<YtDlpCandidateSelection> {
        (self.generation == generation)
            .then(|| self.candidate_selections.get(candidate_index).cloned())
            .flatten()
    }
}

/// Ошибка построения safe projection не смешивается с transport/open failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebMediaStreamModelBuildError {
    AvailabilityPlanningFailed,
    CandidateSnapshotAlignmentFailed,
    InvalidCandidateContainer,
    CandidateSelectionFailed,
    InvalidActiveCandidateIdentity,
    ActiveCandidateMissing,
    ActiveCandidateNotPlayable,
}

impl fmt::Display for WebMediaStreamModelBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::AvailabilityPlanningFailed => {
                "не удалось построить inventory playable candidates"
            }
            Self::CandidateSnapshotAlignmentFailed => {
                "service и planner candidate snapshots не соответствуют друг другу"
            }
            Self::InvalidCandidateContainer => {
                "playable candidate не имеет безопасного container summary"
            }
            Self::CandidateSelectionFailed => {
                "playable candidate не удалось связать с exact switch token"
            }
            Self::InvalidActiveCandidateIdentity => {
                "exact и semantic active candidate identities имеют разный source"
            }
            Self::ActiveCandidateMissing => "active candidate отсутствует в safe inventory",
            Self::ActiveCandidateNotPlayable => "active candidate не прошёл capability planning",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WebMediaStreamModelBuildError {}

/// Контекст active queue binding, не дающий view прямого доступа к queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarItemScope {
    Detached,
    SingleItem,
    CompoundPart,
}

/// Конечный playback status, который URL view может показать без worker handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UrlSidebarPlaybackStatus {
    pub(crate) is_live: bool,
    pub(crate) seekable: bool,
    pub(crate) buffering: bool,
    pub(crate) refresh_on_reopen: bool,
}

/// Только bounded категории ошибок; произвольная error chain в UI-model не попадает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarSafeError {
    SourceUnavailable,
    SameItemSwitchBusy,
    SameItemSwitchStale,
    SameItemSwitchCancelled,
}

/// Модель одной секции существующего sidebar host-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlSidebarModel {
    Inactive,
    DirectMedia {
        source_label: Arc<str>,
        status: UrlSidebarPlaybackStatus,
    },
    YtDlp {
        generation: WebMediaStreamGeneration,
        source_label: Arc<str>,
        candidates: Arc<[WebMediaCandidatePresentation]>,
        active_candidate: WebMediaCandidatePresentation,
        pending_selection: Option<Box<UrlSidebarPendingSelection>>,
        component_variants: Box<WebMediaComponentVariantProjection>,
        preference: WebMediaSelectionPreference,
        item_scope: UrlSidebarItemScope,
        status: UrlSidebarPlaybackStatus,
        safe_error: Option<UrlSidebarSafeError>,
        catalog: crate::web_media_catalog::WebMediaCatalogState,
        fallback_notice: bool,
    },
}

#[derive(Debug, Clone, Copy)]
struct UrlSidebarItemBinding {
    scope: UrlSidebarItemScope,
    item_id: Option<playlist_core::PlaylistItemId>,
}

enum UrlSidebarSourceProjection<'source> {
    Inactive,
    DirectMedia {
        source_label: &'source str,
    },
    NativeHls {
        source_label: &'source str,
    },
    YtDlp {
        source_label: &'source str,
        configuration: &'source WebMediaStreamConfiguration,
    },
}

impl UrlSidebarController {
    pub(crate) fn model_with_catalog(
        &self,
        active_source: Option<&ActiveMediaSource>,
        player_snapshot: &PlayerSnapshot,
        playlist_model: Option<&PlaylistViewModel>,
        catalog: crate::web_media_catalog::WebMediaCatalogState,
        fallback_notice: bool,
    ) -> UrlSidebarModel {
        let source = match active_source.map(ActiveMediaSource::physical_source) {
            None | Some(ActiveMediaSource::LocalFile(_)) => UrlSidebarSourceProjection::Inactive,
            Some(ActiveMediaSource::Web(intent)) => {
                if let Some(locator) = intent.direct_locator() {
                    UrlSidebarSourceProjection::DirectMedia {
                        source_label: locator.safe_label(),
                    }
                } else if let Some((source, _selection)) = intent.native_hls_reopen() {
                    UrlSidebarSourceProjection::NativeHls {
                        source_label: source.safe_label().as_str(),
                    }
                } else if let Some(extractor) = intent.extractor_bridge() {
                    UrlSidebarSourceProjection::YtDlp {
                        source_label: extractor.locator.safe_label(),
                        configuration: extractor.stream_configuration,
                    }
                } else {
                    unreachable!("every web intent owns one typed adapter projection")
                }
            }
            Some(ActiveMediaSource::PlaybackWindow { .. }) => {
                unreachable!("physical_source removes playback-window wrappers")
            }
        };
        self.model_from_source_with_catalog(
            source,
            player_snapshot,
            item_binding(playlist_model),
            catalog,
            fallback_notice,
        )
    }

    fn model_from_source_with_catalog(
        &self,
        source: UrlSidebarSourceProjection<'_>,
        player_snapshot: &PlayerSnapshot,
        item_binding: UrlSidebarItemBinding,
        catalog: crate::web_media_catalog::WebMediaCatalogState,
        fallback_notice: bool,
    ) -> UrlSidebarModel {
        match source {
            UrlSidebarSourceProjection::Inactive => UrlSidebarModel::Inactive,
            UrlSidebarSourceProjection::DirectMedia { source_label } => {
                UrlSidebarModel::DirectMedia {
                    source_label: Arc::from(source_label),
                    status: playback_status(player_snapshot, false),
                }
            }
            UrlSidebarSourceProjection::NativeHls { source_label } => {
                // Native HLS не имеет extractor catalog-а: UI показывает только source/status,
                // не выдумывая YtDlp format controls.
                UrlSidebarModel::DirectMedia {
                    source_label: Arc::from(source_label),
                    status: playback_status(player_snapshot, false),
                }
            }
            UrlSidebarSourceProjection::YtDlp {
                source_label,
                configuration: stream_configuration,
            } => {
                let generation = stream_configuration.generation();
                UrlSidebarModel::YtDlp {
                    generation,
                    source_label: Arc::from(source_label),
                    candidates: Arc::from(stream_configuration.candidates()),
                    active_candidate: stream_configuration.active_candidate().clone(),
                    pending_selection: self
                        .pending_selection
                        .as_ref()
                        .filter(|pending| pending.parent_generation() == generation)
                        .cloned()
                        .map(Box::new),
                    component_variants: Box::new(
                        stream_configuration.component_variant_projection(),
                    ),
                    preference: self
                        .item_override
                        .as_ref()
                        .filter(|item_override| {
                            item_override.source_lineage == generation.source
                                && item_override.item_id == item_binding.item_id
                        })
                        .map(|item_override| {
                            WebMediaSelectionPreference::ItemOverride(
                                item_override.preferred_height,
                            )
                        })
                        .unwrap_or_else(|| stream_configuration.preference()),
                    item_scope: item_binding.scope,
                    status: playback_status(player_snapshot, true),
                    safe_error: self
                        .safe_error
                        .as_ref()
                        .filter(|error| error.generation == generation)
                        .map(|error| error.error)
                        .or_else(|| {
                            (player_snapshot.playback_state == PlaybackState::Failed)
                                .then_some(UrlSidebarSafeError::SourceUnavailable)
                        }),
                    catalog,
                    fallback_notice,
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn model(
        &self,
        active_source: Option<&ActiveMediaSource>,
        player_snapshot: &PlayerSnapshot,
        playlist_model: Option<&PlaylistViewModel>,
    ) -> UrlSidebarModel {
        self.model_with_catalog(
            active_source,
            player_snapshot,
            playlist_model,
            crate::web_media_catalog::WebMediaCatalogState::Inactive,
            false,
        )
    }

    #[cfg(test)]
    fn model_from_source(
        &self,
        source: UrlSidebarSourceProjection<'_>,
        player_snapshot: &PlayerSnapshot,
        item_binding: UrlSidebarItemBinding,
    ) -> UrlSidebarModel {
        self.model_from_source_with_catalog(
            source,
            player_snapshot,
            item_binding,
            crate::web_media_catalog::WebMediaCatalogState::Inactive,
            false,
        )
    }
}

fn consistent_container(
    container: &web_media_core::ContainerIdentity,
) -> Result<ContainerFamily, WebMediaStreamModelBuildError> {
    container
        .consistent_family()
        .map_err(|_| WebMediaStreamModelBuildError::InvalidCandidateContainer)
        .and_then(|family| family.ok_or(WebMediaStreamModelBuildError::InvalidCandidateContainer))
}

fn known_codec(kind: CodecKind) -> Option<CodecFamily> {
    match kind {
        CodecKind::Known(codec) => Some(codec),
        CodecKind::Absent | CodecKind::Unknown => None,
    }
}

fn playback_status(snapshot: &PlayerSnapshot, refresh_on_reopen: bool) -> UrlSidebarPlaybackStatus {
    UrlSidebarPlaybackStatus {
        // S23 production path поддерживает только finite progressive HTTP(S).
        is_live: false,
        seekable: snapshot
            .media_info
            .as_ref()
            .is_some_and(|media_info| media_info.seekable),
        buffering: snapshot.playback_state == PlaybackState::Buffering,
        refresh_on_reopen,
    }
}

fn item_binding(playlist_model: Option<&PlaylistViewModel>) -> UrlSidebarItemBinding {
    let Some(model) = playlist_model else {
        return UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::Detached,
            item_id: None,
        };
    };
    let Some(active_item_id) = model.active_item_id() else {
        return UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::Detached,
            item_id: None,
        };
    };
    let scope = match model
        .compound_snapshot()
        .structural_entry_id_for_item(active_item_id)
    {
        Some(playlist_core::PlaylistEntryId::Compound(_)) => UrlSidebarItemScope::CompoundPart,
        Some(playlist_core::PlaylistEntryId::Single(_)) => UrlSidebarItemScope::SingleItem,
        None => UrlSidebarItemScope::Detached,
    };
    UrlSidebarItemBinding {
        scope,
        item_id: Some(active_item_id),
    }
}

#[cfg(test)]
mod tests;
