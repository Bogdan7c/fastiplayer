//! Secret-safe доменная модель активной web-media конфигурации.
//!
//! Модуль не владеет queue, transport или media-open lifecycle. Он получает уже
//! проверенный candidate snapshot до открытия транспорта и сохраняет только
//! безопасное описание форматов: generation fence источника, предпочтение
//! качества, playable candidates и независимые component variants.
//!
//! UI-проекция (модель и контроллер URL sidebar) живёт в `app-egui` и читает
//! эту модель только через публичные методы; приватные поля не пересекают
//! границу crate-а. Тестовые конструкторы помечены
//! `cfg(any(test, feature = "test-fixtures"))`.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use web_media_core::{
    CandidateDescriptor, CodecFamily, CodecKind, ContainerFamily, DynamicRange,
    ExactSelectionIdentity, StreamLayout, StreamLayoutKind, WebMediaSelection,
};
use web_media_playback_plan::{
    PlanningCandidateSnapshot, PlaybackCapabilitySnapshot, PlaybackSelectionPolicy, plan_playback,
};

pub mod component_variants;
use component_variants::WebMediaComponentVariantConfiguration;

#[cfg(test)]
mod component_variants_tests;

/// Поколение extraction snapshot-а без candidate format identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebMediaStreamGeneration {
    source: u64,
    extraction: u64,
}

impl WebMediaStreamGeneration {
    /// Строит generation fence из установленного provider-neutral selection.
    #[must_use]
    pub fn from_selection(selection: &WebMediaSelection) -> Self {
        let identity = selection.parent().exact();
        Self {
            source: identity.source().value(),
            extraction: identity.generation().value(),
        }
    }

    /// Строит synthetic generation только для hermetic UI/state тестов.
    /// Вне crate доступен лишь с feature `test-fixtures` (dev-dependencies app-egui).
    #[cfg(any(test, feature = "test-fixtures"))]
    pub const fn for_test(source: u64, extraction: u64) -> Self {
        Self { source, extraction }
    }

    /// Проверяет, что fresh extraction принадлежит той же source lineage.
    #[must_use]
    pub const fn has_same_source_lineage(self, other: Self) -> bool {
        self.source == other.source
    }
}

/// Источник runtime preference, показанный пользователю без queue mutation API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebMediaSelectionPreference {
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
    pub fn from_global_config(config: &fastiplayer_config::WebMediaConfig) -> Self {
        match config.preferred_video_height {
            Some(height) => Self::GlobalPreferredHeight(height.pixels()),
            None => Self::GlobalBestPlayable,
        }
    }
}

/// Безопасная пара container families для single либо separate A/V candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebMediaContainerSummary {
    pub video: Option<ContainerFamily>,
    pub audio: Option<ContainerFamily>,
}

/// Безопасное описание одного реально playable candidate-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebMediaCandidatePresentation {
    pub layout: StreamLayoutKind,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<(u32, u32)>,
    pub video_bitrate: Option<u64>,
    pub audio_bitrate: Option<u64>,
    pub video_codec: Option<CodecFamily>,
    pub audio_codec: Option<CodecFamily>,
    pub dynamic_range: Option<DynamicRange>,
    pub containers: WebMediaContainerSummary,
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

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn has_video(&self) -> bool {
        self.height.is_some() || self.video_codec.is_some()
    }
}

/// Installed provider-neutral конфигурация web-media с secret-safe UI projection.
#[derive(Clone, PartialEq, Eq)]
pub struct WebMediaStreamConfiguration {
    generation: WebMediaStreamGeneration,
    active_parent: ExactSelectionIdentity,
    candidates: Arc<[WebMediaCandidatePresentation]>,
    candidate_selections: Arc<[WebMediaSelection]>,
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
    /// Строит UI inventory только из N01/N21 neutral catalog contracts.
    pub fn from_neutral_catalog(
        planning_snapshot: &PlanningCandidateSnapshot,
        capabilities: PlaybackCapabilitySnapshot<'_>,
        policy: &PlaybackSelectionPolicy,
        neutral_selection: &WebMediaSelection,
        preference: WebMediaSelectionPreference,
    ) -> Result<Self, WebMediaStreamModelBuildError> {
        let active_parent = neutral_selection.parent().clone();
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

        for candidate in planning_snapshot.candidates() {
            let descriptor = candidate.descriptor();
            let playable = !rejected_identities.contains(descriptor.identity());
            let exact_parent = ExactSelectionIdentity::new(
                descriptor.identity().clone(),
                descriptor.semantic_identity().clone(),
            )
            .map_err(|_| WebMediaStreamModelBuildError::InvalidActiveCandidateIdentity)?;
            let selection = WebMediaSelection::candidate(exact_parent);
            let is_active = selection.parent() == &active_parent;
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
            generation: WebMediaStreamGeneration::from_selection(neutral_selection),
            active_parent,
            candidates: candidates.into(),
            candidate_selections: candidate_selections.into(),
            active_candidate,
            preference,
            component_variants: WebMediaComponentVariantConfiguration::Unavailable,
            hls_subtitle_renditions: Arc::from([]),
        })
    }

    /// Строит один stable parent для native manifest; полный master inventory
    /// устанавливается отдельно через canonical component catalog boundary.
    pub fn from_native_manifest(
        active_parent: ExactSelectionIdentity,
        preference: WebMediaSelectionPreference,
    ) -> Self {
        let active_selection = WebMediaSelection::candidate(active_parent.clone());
        let active_candidate = WebMediaCandidatePresentation {
            layout: StreamLayoutKind::ContentProbed,
            width: None,
            height: None,
            frame_rate: None,
            video_bitrate: None,
            audio_bitrate: None,
            video_codec: None,
            audio_codec: None,
            dynamic_range: None,
            containers: WebMediaContainerSummary {
                video: None,
                audio: None,
            },
        };
        Self {
            generation: WebMediaStreamGeneration::from_selection(&active_selection),
            active_parent,
            candidates: Arc::from([active_candidate.clone()]),
            candidate_selections: Arc::from([active_selection]),
            active_candidate,
            preference,
            component_variants: WebMediaComponentVariantConfiguration::Unavailable,
            hls_subtitle_renditions: Arc::from([]),
        }
    }

    #[must_use]
    pub fn generation(&self) -> WebMediaStreamGeneration {
        self.generation
    }

    #[must_use]
    pub fn preference(&self) -> WebMediaSelectionPreference {
        self.preference
    }

    #[must_use]
    pub fn candidates(&self) -> &[WebMediaCandidatePresentation] {
        &self.candidates
    }

    #[must_use]
    pub fn active_candidate(&self) -> &WebMediaCandidatePresentation {
        &self.active_candidate
    }

    /// Связывает descriptors только с exact подготовленным HLS candidate-ом.
    pub fn with_hls_subtitle_renditions(
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

    /// Возвращает neutral selection только после generation/index validation.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn selection_for_switch(
        &self,
        generation: WebMediaStreamGeneration,
        candidate_index: usize,
    ) -> Option<WebMediaSelection> {
        (self.generation == generation)
            .then(|| self.candidate_selections.get(candidate_index).cloned())
            .flatten()
    }

    /// Собирает synthetic installed конфигурацию только для hermetic тестов.
    ///
    /// Generation выводится из `active_parent` так же, как в production
    /// (`WebMediaStreamGeneration::from_selection`), поэтому fixture не может
    /// получить рассогласованные parent и generation fence. Каждому candidate-у
    /// сопоставляется selection того же parent-а, component variants недоступны,
    /// HLS subtitle renditions пусты.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn fixture(
        active_parent: ExactSelectionIdentity,
        candidates: Vec<WebMediaCandidatePresentation>,
        active_candidate: WebMediaCandidatePresentation,
        preference: WebMediaSelectionPreference,
    ) -> Self {
        let active_selection = WebMediaSelection::candidate(active_parent.clone());
        let candidate_selections = vec![active_selection.clone(); candidates.len()];
        Self {
            generation: WebMediaStreamGeneration::from_selection(&active_selection),
            active_parent,
            candidates: candidates.into(),
            candidate_selections: candidate_selections.into(),
            active_candidate,
            preference,
            component_variants: WebMediaComponentVariantConfiguration::Unavailable,
            hls_subtitle_renditions: Arc::from([]),
        }
    }
}

/// Ошибка построения safe projection не смешивается с transport/open failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebMediaStreamModelBuildError {
    AvailabilityPlanningFailed,
    InvalidCandidateContainer,
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
            Self::InvalidCandidateContainer => {
                "playable candidate не имеет безопасного container summary"
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

#[cfg(test)]
mod tests;
