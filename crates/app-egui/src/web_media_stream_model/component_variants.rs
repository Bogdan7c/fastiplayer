//! App-owned boundary независимого выбора video/audio component variants.
//!
//! Exact identities остаются внутри модели. Sidebar получает только enum/numeric
//! projection, а будущий controlled reopen — только semantic-only request.

#![allow(
    dead_code,
    reason = "S36C2 публикует production boundary до намеренно отдельного C3 runtime wiring"
)]

use std::fmt;
use std::sync::Arc;

use service_ytdlp::YtDlpCandidateSelection;
use web_media_core::{
    AudioComponentVariant, CodecFamily, CodecKind, ComponentKind, ComponentVariantCatalog,
    ComponentVariantCatalogGeneration, ComponentVariantError, ComponentVariantSelection,
    ComponentVariantSelectionRequest, ComponentVariantSemanticSelectionRequest, DynamicRange,
    ExactSelectionIdentity, VideoComponentVariant,
};

use super::{WebMediaStreamConfiguration, WebMediaStreamGeneration, known_codec};

/// Exact parent token всегда установлен production constructor-ом.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum ActiveParentCandidateSelection {
    /// Reopen-safe service token, который никогда не пересекает UI projection.
    Installed(Box<YtDlpCandidateSelection>),
    /// Synthetic projection tests не запускают media-open и не изобретают fake token.
    #[cfg(test)]
    ProjectionFixture,
}

/// Конфигурация component variants, принадлежащая exact Installed web-media source.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) enum WebMediaComponentVariantConfiguration {
    /// Provider/runtime не опубликовал component catalog для active candidate-а.
    #[default]
    Unavailable,
    /// Catalog и canonical selection прошли app-owned correlation boundary.
    Installed(InstalledWebMediaComponentVariants),
}

impl fmt::Debug for WebMediaComponentVariantConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("Unavailable"),
            Self::Installed(installed) => installed.fmt(formatter),
        }
    }
}

/// Exact Installed owner; custom `Debug` не раскрывает parent/component identities.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InstalledWebMediaComponentVariants {
    catalog: Arc<ComponentVariantCatalog>,
    selection: ComponentVariantSelection,
    presentation: WebMediaInstalledComponentVariantPresentation,
}

impl fmt::Debug for InstalledWebMediaComponentVariants {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstalledWebMediaComponentVariants")
            .field("catalog_generation", &self.catalog.identity().generation())
            .field("presentation", &self.presentation)
            .finish()
    }
}

/// Safe projection всей component configuration для URL sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WebMediaComponentVariantProjection {
    /// Раздельный выбор для active candidate-а отсутствует.
    Unavailable,
    /// Установленный catalog имеет одну из трёх однозначных layout shapes.
    Installed(WebMediaInstalledComponentVariantPresentation),
}

/// Safe shape installed catalog-а без ambiguous `Option` axes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WebMediaInstalledComponentVariantPresentation {
    /// Независимые additive video и audio rows, без Cartesian product.
    VideoAndAudio {
        catalog_generation: ComponentVariantCatalogGeneration,
        video: WebMediaVideoComponentVariantAxis,
        audio: WebMediaAudioComponentVariantAxis,
    },
    /// Только независимая video axis.
    VideoOnly {
        catalog_generation: ComponentVariantCatalogGeneration,
        video: WebMediaVideoComponentVariantAxis,
    },
    /// Только независимая audio axis.
    AudioOnly {
        catalog_generation: ComponentVariantCatalogGeneration,
        audio: WebMediaAudioComponentVariantAxis,
    },
}

/// Safe video axis с explicit active row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaVideoComponentVariantAxis {
    pub(crate) active_index: usize,
    pub(crate) variants: Arc<[WebMediaVideoComponentVariantPresentation]>,
}

/// Safe audio axis с explicit active row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaAudioComponentVariantAxis {
    pub(crate) active_index: usize,
    pub(crate) variants: Arc<[WebMediaAudioComponentVariantPresentation]>,
}

/// Только безопасные numeric/enum metadata video row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaVideoComponentVariantPresentation {
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) frame_rate: Option<(u32, u32)>,
    pub(crate) bitrate: Option<u64>,
    pub(crate) codec: Option<CodecFamily>,
    pub(crate) dynamic_range: DynamicRange,
}

/// Только безопасные numeric/enum metadata audio row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaAudioComponentVariantPresentation {
    pub(crate) bitrate: Option<u64>,
    pub(crate) sample_rate_hz: Option<u32>,
    pub(crate) channels: Option<u16>,
    pub(crate) codec: Option<CodecFamily>,
}

/// Model-local intent с двумя независимыми generation fences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ComponentVariantSelectionAction {
    pub(crate) parent_generation: WebMediaStreamGeneration,
    pub(crate) catalog_generation: ComponentVariantCatalogGeneration,
    pub(crate) component: ComponentKind,
    pub(crate) variant_index: usize,
}

impl ComponentVariantSelectionAction {
    /// Возвращает generation родительской installed stream configuration.
    #[must_use]
    pub(crate) const fn parent_generation(self) -> WebMediaStreamGeneration {
        self.parent_generation
    }

    /// Возвращает generation безопасного component catalog-а.
    #[must_use]
    pub(crate) const fn catalog_generation(self) -> ComponentVariantCatalogGeneration {
        self.catalog_generation
    }

    /// Возвращает выбранную независимую component axis.
    #[must_use]
    pub(crate) const fn component(self) -> ComponentKind {
        self.component
    }

    /// Возвращает safe row index внутри выбранной axis.
    #[must_use]
    pub(crate) const fn variant_index(self) -> usize {
        self.variant_index
    }
}

/// Результат model-local resolution; transport/open lifecycle здесь не запускается.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComponentVariantActionResolution {
    /// Пользователь указал уже активную row.
    NoChange,
    /// Будущий C3 может передать только refresh-stable selection в strong reopen.
    SemanticReopen(ComponentVariantSemanticSelectionRequest),
}

/// Typed ошибки app-owned installation boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComponentVariantInstallationError {
    /// Catalog относится не к exact active candidate-у установленной конфигурации.
    ActiveParentMismatch,
    /// Selection не удалось канонизировать через exact rows catalog-а.
    InvalidSelection(ComponentVariantError),
    /// Canonical selection не нашлась в safe presentation того же catalog-а.
    ActiveVariantMissing { component: ComponentKind },
}

impl fmt::Display for ComponentVariantInstallationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveParentMismatch => {
                formatter.write_str("component catalog принадлежит другому active candidate")
            }
            Self::InvalidSelection(error) => {
                write!(
                    formatter,
                    "component selection не прошёл canonical lookup: {error}"
                )
            }
            Self::ActiveVariantMissing { component } => {
                write!(
                    formatter,
                    "active {component:?} variant отсутствует в catalog"
                )
            }
        }
    }
}

impl std::error::Error for ComponentVariantInstallationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidSelection(error) => Some(error),
            Self::ActiveParentMismatch | Self::ActiveVariantMissing { .. } => None,
        }
    }
}

/// Typed ошибки model-local action resolver-а в обязательном порядке validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComponentVariantActionError {
    Unavailable,
    StaleParentGeneration {
        expected: WebMediaStreamGeneration,
        provided: WebMediaStreamGeneration,
    },
    StaleCatalogGeneration {
        expected: ComponentVariantCatalogGeneration,
        provided: ComponentVariantCatalogGeneration,
    },
    WrongAxis {
        component: ComponentKind,
    },
    VariantIndexOutOfRange {
        component: ComponentKind,
        provided: usize,
        variant_count: usize,
    },
    ReplacementFailed(ComponentVariantError),
}

impl fmt::Display for ComponentVariantActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("component variants недоступны"),
            Self::StaleParentGeneration { .. } => {
                formatter.write_str("action относится к прошлому parent generation")
            }
            Self::StaleCatalogGeneration { .. } => {
                formatter.write_str("action относится к прошлому component catalog")
            }
            Self::WrongAxis { component } => {
                write!(formatter, "catalog не содержит axis {component:?}")
            }
            Self::VariantIndexOutOfRange {
                component,
                provided,
                variant_count,
            } => write!(
                formatter,
                "индекс {provided} вне {component:?} axis из {variant_count} rows"
            ),
            Self::ReplacementFailed(error) => {
                write!(
                    formatter,
                    "immutable component replacement отклонён: {error}"
                )
            }
        }
    }
}

impl std::error::Error for ComponentVariantActionError {}

impl WebMediaStreamConfiguration {
    /// Возвращает exact token именно активного parent candidate-а для component reopen.
    ///
    /// UI не получает этот token: exact identity остаётся внутри component owner-а,
    /// а наружу выходит только готовый reopen intent.
    pub(crate) fn active_candidate_selection_for_component_switch(
        &self,
    ) -> Option<YtDlpCandidateSelection> {
        match &self.active_parent_selection {
            ActiveParentCandidateSelection::Installed(selection) => {
                Some(selection.as_ref().clone())
            }
            #[cfg(test)]
            ActiveParentCandidateSelection::ProjectionFixture => None,
        }
    }

    /// Устанавливает independent component catalog только для exact active parent-а.
    pub(crate) fn with_component_variants(
        mut self,
        catalog: Arc<ComponentVariantCatalog>,
        selection: ComponentVariantSelection,
    ) -> Result<Self, ComponentVariantInstallationError> {
        self.component_variants = WebMediaComponentVariantConfiguration::install(
            &self.active_parent,
            catalog,
            selection,
        )?;
        Ok(self)
    }

    /// Строит reopen intent без публикации catalog-а либо exact component identities.
    #[must_use]
    pub(crate) fn component_selection_reopen_intent(
        &self,
    ) -> crate::web_media_open::YtDlpComponentSelectionOpenIntent {
        self.component_variants.reopen_intent()
    }

    /// Возвращает только safe shape-typed projection без exact identities.
    #[must_use]
    pub(crate) fn component_variant_projection(&self) -> WebMediaComponentVariantProjection {
        self.component_variants.projection()
    }

    /// Валидирует model-local component action до запуска controlled strong reopen.
    pub(crate) fn resolve_component_variant_action(
        &self,
        action: ComponentVariantSelectionAction,
    ) -> Result<ComponentVariantActionResolution, ComponentVariantActionError> {
        self.component_variants
            .resolve_action(self.generation, action)
    }
}

impl WebMediaComponentVariantConfiguration {
    /// Устанавливает catalog только после exact parent correlation и canonical lookup.
    pub(super) fn install(
        active_parent: &ExactSelectionIdentity,
        catalog: Arc<ComponentVariantCatalog>,
        supplied_selection: ComponentVariantSelection,
    ) -> Result<Self, ComponentVariantInstallationError> {
        if catalog.identity().parent() != active_parent {
            return Err(ComponentVariantInstallationError::ActiveParentMismatch);
        }

        let canonical_selection = catalog
            .select_exact(selection_request(&supplied_selection))
            .map_err(ComponentVariantInstallationError::InvalidSelection)?;
        let presentation = build_presentation(&catalog, &canonical_selection)?;

        Ok(Self::Installed(InstalledWebMediaComponentVariants {
            catalog,
            selection: canonical_selection,
            presentation,
        }))
    }

    /// Клонирует только safe projection для view model.
    #[must_use]
    pub(super) fn projection(&self) -> WebMediaComponentVariantProjection {
        match self {
            Self::Unavailable => WebMediaComponentVariantProjection::Unavailable,
            Self::Installed(installed) => {
                WebMediaComponentVariantProjection::Installed(installed.presentation.clone())
            }
        }
    }

    /// Сохраняет только refresh-stable semantic выбор установленной конфигурации.
    #[must_use]
    fn reopen_intent(&self) -> crate::web_media_open::YtDlpComponentSelectionOpenIntent {
        match self {
            Self::Unavailable => {
                crate::web_media_open::YtDlpComponentSelectionOpenIntent::ProviderDefault
            }
            Self::Installed(installed) => {
                crate::web_media_open::YtDlpComponentSelectionOpenIntent::Semantic(
                    installed.selection.semantic_rematch_request(),
                )
            }
        }
    }

    /// Проверяет model-local action и строит semantic-only immutable replacement.
    pub(super) fn resolve_action(
        &self,
        active_parent_generation: WebMediaStreamGeneration,
        action: ComponentVariantSelectionAction,
    ) -> Result<ComponentVariantActionResolution, ComponentVariantActionError> {
        let Self::Installed(installed) = self else {
            return Err(ComponentVariantActionError::Unavailable);
        };

        if action.parent_generation != active_parent_generation {
            return Err(ComponentVariantActionError::StaleParentGeneration {
                expected: active_parent_generation,
                provided: action.parent_generation,
            });
        }

        let expected_catalog_generation = installed.catalog.identity().generation();
        if action.catalog_generation != expected_catalog_generation {
            return Err(ComponentVariantActionError::StaleCatalogGeneration {
                expected: expected_catalog_generation,
                provided: action.catalog_generation,
            });
        }

        let (variant_count, active_index) = installed.axis_state(action.component)?;
        if action.variant_index >= variant_count {
            return Err(ComponentVariantActionError::VariantIndexOutOfRange {
                component: action.component,
                provided: action.variant_index,
                variant_count,
            });
        }
        if action.variant_index == active_index {
            return Ok(ComponentVariantActionResolution::NoChange);
        }

        let replacement = installed.replace(action.component, action.variant_index)?;
        Ok(ComponentVariantActionResolution::SemanticReopen(
            replacement.semantic_rematch_request(),
        ))
    }
}

impl InstalledWebMediaComponentVariants {
    /// Возвращает cardinality и active index требуемой axis.
    fn axis_state(
        &self,
        component: ComponentKind,
    ) -> Result<(usize, usize), ComponentVariantActionError> {
        match (&self.presentation, component) {
            (
                WebMediaInstalledComponentVariantPresentation::VideoAndAudio { video, .. }
                | WebMediaInstalledComponentVariantPresentation::VideoOnly { video, .. },
                ComponentKind::Video,
            ) => Ok((video.variants.len(), video.active_index)),
            (
                WebMediaInstalledComponentVariantPresentation::VideoAndAudio { audio, .. }
                | WebMediaInstalledComponentVariantPresentation::AudioOnly { audio, .. },
                ComponentKind::Audio,
            ) => Ok((audio.variants.len(), audio.active_index)),
            (_, component) => Err(ComponentVariantActionError::WrongAxis { component }),
        }
    }

    /// Делает immutable replacement; другая axis остаётся byte-for-byte прежней.
    fn replace(
        &self,
        component: ComponentKind,
        variant_index: usize,
    ) -> Result<ComponentVariantSelection, ComponentVariantActionError> {
        match component {
            ComponentKind::Video => {
                let variants = self
                    .catalog
                    .required_video_variants()
                    .map_err(ComponentVariantActionError::ReplacementFailed)?;
                self.selection
                    .replace_video(&self.catalog, variants[variant_index].exact_identity())
                    .map_err(ComponentVariantActionError::ReplacementFailed)
            }
            ComponentKind::Audio => {
                let variants = self
                    .catalog
                    .required_audio_variants()
                    .map_err(ComponentVariantActionError::ReplacementFailed)?;
                self.selection
                    .replace_audio(&self.catalog, variants[variant_index].exact_identity())
                    .map_err(ComponentVariantActionError::ReplacementFailed)
            }
        }
    }
}

/// Восстанавливает exact layout-shaped request без доверия supplied object graph.
fn selection_request(selection: &ComponentVariantSelection) -> ComponentVariantSelectionRequest {
    match selection {
        ComponentVariantSelection::VideoAndAudio { video, audio, .. } => {
            ComponentVariantSelectionRequest::VideoAndAudio {
                video: video.exact_identity().clone(),
                audio: audio.exact_identity().clone(),
            }
        }
        ComponentVariantSelection::VideoOnly { video, .. } => {
            ComponentVariantSelectionRequest::VideoOnly {
                video: video.exact_identity().clone(),
            }
        }
        ComponentVariantSelection::AudioOnly { audio, .. } => {
            ComponentVariantSelectionRequest::AudioOnly {
                audio: audio.exact_identity().clone(),
            }
        }
        ComponentVariantSelection::Coupled { presentation, .. } => {
            ComponentVariantSelectionRequest::Coupled {
                presentation: presentation.exact_identity().clone(),
            }
        }
    }
}

/// Строит shape-typed safe projection и explicit active indices.
fn build_presentation(
    catalog: &ComponentVariantCatalog,
    selection: &ComponentVariantSelection,
) -> Result<WebMediaInstalledComponentVariantPresentation, ComponentVariantInstallationError> {
    let catalog_generation = catalog.identity().generation();
    match (catalog, selection) {
        (
            ComponentVariantCatalog::Topology { video, audio, .. }
            | ComponentVariantCatalog::VideoAndAudio { video, audio, .. },
            ComponentVariantSelection::VideoAndAudio {
                video: active_video,
                audio: active_audio,
                ..
            },
        ) => Ok(
            WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
                catalog_generation,
                video: video_axis(video, active_video.exact_identity())?,
                audio: audio_axis(audio, active_audio.exact_identity())?,
            },
        ),
        (
            ComponentVariantCatalog::VideoOnly { video, .. },
            ComponentVariantSelection::VideoOnly {
                video: active_video,
                ..
            },
        ) => Ok(WebMediaInstalledComponentVariantPresentation::VideoOnly {
            catalog_generation,
            video: video_axis(video, active_video.exact_identity())?,
        }),
        (
            ComponentVariantCatalog::AudioOnly { audio, .. },
            ComponentVariantSelection::AudioOnly {
                audio: active_audio,
                ..
            },
        ) => Ok(WebMediaInstalledComponentVariantPresentation::AudioOnly {
            catalog_generation,
            audio: audio_axis(audio, active_audio.exact_identity())?,
        }),
        _ => Err(ComponentVariantInstallationError::InvalidSelection(
            ComponentVariantError::LayoutMismatch,
        )),
    }
}

fn video_axis(
    variants: &[VideoComponentVariant],
    active_identity: &web_media_core::ComponentVariantExactIdentity,
) -> Result<WebMediaVideoComponentVariantAxis, ComponentVariantInstallationError> {
    let active_index = variants
        .iter()
        .position(|variant| variant.exact_identity() == active_identity)
        .ok_or(ComponentVariantInstallationError::ActiveVariantMissing {
            component: ComponentKind::Video,
        })?;
    let variants = variants
        .iter()
        .map(video_presentation)
        .collect::<Vec<_>>()
        .into();
    Ok(WebMediaVideoComponentVariantAxis {
        active_index,
        variants,
    })
}

fn audio_axis(
    variants: &[AudioComponentVariant],
    active_identity: &web_media_core::ComponentVariantExactIdentity,
) -> Result<WebMediaAudioComponentVariantAxis, ComponentVariantInstallationError> {
    let active_index = variants
        .iter()
        .position(|variant| variant.exact_identity() == active_identity)
        .ok_or(ComponentVariantInstallationError::ActiveVariantMissing {
            component: ComponentKind::Audio,
        })?;
    let variants = variants
        .iter()
        .map(audio_presentation)
        .collect::<Vec<_>>()
        .into();
    Ok(WebMediaAudioComponentVariantAxis {
        active_index,
        variants,
    })
}

fn video_presentation(
    variant: &VideoComponentVariant,
) -> WebMediaVideoComponentVariantPresentation {
    let track = variant.track();
    WebMediaVideoComponentVariantPresentation {
        width: track.width_pixels(),
        height: track.height().map(|height| height.pixels()),
        frame_rate: track
            .frame_rate()
            .map(|rate| (rate.numerator(), rate.denominator())),
        bitrate: track.bitrate().map(|bitrate| bitrate.bits_per_second()),
        codec: safe_codec_family(track.codec().kind()),
        dynamic_range: track.dynamic_range(),
    }
}

fn audio_presentation(
    variant: &AudioComponentVariant,
) -> WebMediaAudioComponentVariantPresentation {
    let track = variant.track();
    WebMediaAudioComponentVariantPresentation {
        bitrate: track.bitrate().map(|bitrate| bitrate.bits_per_second()),
        sample_rate_hz: track.sample_rate().map(|rate| rate.hertz()),
        channels: track.channels().map(|channels| channels.get()),
        codec: safe_codec_family(track.codec().kind()),
    }
}

/// Явно отбрасывает raw codec identity и parameter tokens на UI boundary.
fn safe_codec_family(kind: CodecKind) -> Option<CodecFamily> {
    known_codec(kind)
}
