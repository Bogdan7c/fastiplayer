//! Финализация independent component variants внутри pre-barrier YtDlp preparation.
//!
//! Provider владеет свежим catalog-ом и своим exact default selection, а app-owned
//! open intent хранит только намерение: принять provider default либо повторить
//! semantic выбор пользователя на свежем catalog generation.

use std::fmt;
use std::sync::Arc;

use service_ytdlp::YtDlpCandidateSelection;
use web_media_core::{
    ComponentVariantCatalog, ComponentVariantError, ComponentVariantSelection,
    ComponentVariantSemanticSelectionRequest,
};

use crate::web_media_stream_model::{
    WebMediaSelectionPreference, WebMediaStreamConfiguration,
    component_variants::{
        ComponentVariantInstallationError, WebMediaComponentSelectionReopenIntent,
    },
};

use super::YtDlpCandidateOpenIntent;

/// Намерение выбора independent components при exact reopen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum YtDlpComponentSelectionOpenIntent {
    /// Fresh provider выбирает собственную exact конфигурацию, если catalog доступен.
    ProviderDefault,
    /// Fresh catalog обязан повторно сопоставить только стабильные semantic identities.
    Semantic(ComponentVariantSemanticSelectionRequest),
}

/// Один heap-owned exact reopen intent не раздувает каждый source request enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct YtDlpExactCandidateOpenIntent {
    /// Предыдущий exact parent selection для semantic rematch в fresh snapshot-е.
    pub(super) selection: Box<YtDlpCandidateSelection>,
    /// Исходная global/item policy не должна теряться при suspend/reopen.
    pub(super) preference: WebMediaSelectionPreference,
    /// Независимое component intent не смешивается с parent candidate selection.
    pub(super) component_selection: YtDlpComponentSelectionOpenIntent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct YtDlpComposedCandidateOpenIntent {
    pub(super) selection: Box<service_ytdlp::YtDlpComposedSelection>,
    pub(super) parent_preference: Box<YtDlpCandidateSelection>,
    pub(super) preference: WebMediaSelectionPreference,
}

impl YtDlpCandidateOpenIntent {
    /// Открывает exact parent, но сбрасывает independent components к fresh provider default.
    #[must_use]
    pub(crate) fn exact_parent_provider_default(
        selection: Box<YtDlpCandidateSelection>,
        preference: WebMediaSelectionPreference,
    ) -> Self {
        Self::Exact(Box::new(YtDlpExactCandidateOpenIntent {
            selection,
            preference,
            component_selection: YtDlpComponentSelectionOpenIntent::ProviderDefault,
        }))
    }

    /// Перестраивает тот же exact parent и сохраняет semantic component selection.
    #[must_use]
    pub(crate) fn exact_preserving_installed_stream_configuration(
        selection: Box<YtDlpCandidateSelection>,
        stream_configuration: &WebMediaStreamConfiguration,
    ) -> Self {
        let component_selection = match stream_configuration.component_selection_reopen_intent() {
            WebMediaComponentSelectionReopenIntent::ProviderDefault => {
                YtDlpComponentSelectionOpenIntent::ProviderDefault
            }
            WebMediaComponentSelectionReopenIntent::Semantic(selection) => {
                YtDlpComponentSelectionOpenIntent::Semantic(selection)
            }
        };
        Self::Exact(Box::new(YtDlpExactCandidateOpenIntent {
            selection,
            preference: stream_configuration.preference(),
            component_selection,
        }))
    }

    /// Открывает exact parent с component intent, не меняя установленную quality preference.
    #[allow(
        dead_code,
        reason = "C3A проводит typed intent; enabled component action подключит C3B"
    )]
    #[must_use]
    pub(crate) fn exact_with_component_semantic_selection(
        selection: Box<YtDlpCandidateSelection>,
        stream_configuration: &WebMediaStreamConfiguration,
        semantic_selection: ComponentVariantSemanticSelectionRequest,
    ) -> Self {
        Self::Exact(Box::new(YtDlpExactCandidateOpenIntent {
            selection,
            preference: stream_configuration.preference(),
            component_selection: YtDlpComponentSelectionOpenIntent::Semantic(semantic_selection),
        }))
    }

    #[must_use]
    pub(crate) fn composed(
        selection: Box<service_ytdlp::YtDlpComposedSelection>,
        parent_preference: Box<YtDlpCandidateSelection>,
        preference: WebMediaSelectionPreference,
    ) -> Self {
        Self::Composed(Box::new(YtDlpComposedCandidateOpenIntent {
            selection,
            parent_preference,
            preference,
        }))
    }

    /// Возвращает component intent до consuming parent snapshot resolution.
    pub(super) fn component_selection_intent(&self) -> YtDlpComponentSelectionOpenIntent {
        match self {
            Self::BestPlayable => YtDlpComponentSelectionOpenIntent::ProviderDefault,
            Self::Exact(exact) => exact.component_selection.clone(),
            Self::Composed(_) => YtDlpComponentSelectionOpenIntent::ProviderDefault,
        }
    }
}

/// Свежий provider-owned результат component catalog preparation.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "Текущие providers честно Unavailable; Installed — production seam следующего provider-а"
)]
pub(crate) enum PreparedComponentVariantCatalog {
    /// Текущий concrete provider не умеет independent component selection.
    Unavailable,
    /// Provider вернул свежий catalog и свой exact default selection того же generation.
    Installed {
        catalog: Arc<ComponentVariantCatalog>,
        provider_selection: ComponentVariantSelection,
    },
}

/// Typed ошибка чистой pre-barrier финализации component configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComponentVariantFinalizationError {
    /// Semantic reopen потребовал catalog у provider-а, который его не предоставил.
    ComponentCatalogUnavailable,
    /// Fresh catalog не содержит запрошенную semantic конфигурацию.
    SemanticRematch(ComponentVariantError),
    /// App-owned installation отвергла parent correlation либо exact selection.
    Installation(ComponentVariantInstallationError),
}

impl fmt::Display for ComponentVariantFinalizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ComponentCatalogUnavailable => formatter
                .write_str("provider не предоставил component catalog для semantic exact reopen"),
            Self::SemanticRematch(error) => {
                write!(
                    formatter,
                    "fresh component semantic rematch отклонён: {error}"
                )
            }
            Self::Installation(error) => {
                write!(formatter, "fresh component catalog не установлен: {error}")
            }
        }
    }
}

impl std::error::Error for ComponentVariantFinalizationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ComponentCatalogUnavailable => None,
            Self::SemanticRematch(error) => Some(error),
            Self::Installation(error) => Some(error),
        }
    }
}

/// Финализирует fresh stream configuration до публикации prepared descriptor-а.
///
/// Функция consume-ит незавершённую конфигурацию, поэтому любая ошибка не может
/// оставить вызывающему коду частично установленный catalog.
pub(super) fn finalize_component_variant_configuration(
    stream_configuration: WebMediaStreamConfiguration,
    intent: YtDlpComponentSelectionOpenIntent,
    prepared_catalog: PreparedComponentVariantCatalog,
) -> Result<WebMediaStreamConfiguration, ComponentVariantFinalizationError> {
    match (intent, prepared_catalog) {
        (
            YtDlpComponentSelectionOpenIntent::ProviderDefault,
            PreparedComponentVariantCatalog::Unavailable,
        ) => Ok(stream_configuration),
        (
            YtDlpComponentSelectionOpenIntent::ProviderDefault,
            PreparedComponentVariantCatalog::Installed {
                catalog,
                provider_selection,
            },
        ) => stream_configuration
            .with_component_variants(catalog, provider_selection)
            .map_err(ComponentVariantFinalizationError::Installation),
        (
            YtDlpComponentSelectionOpenIntent::Semantic(_),
            PreparedComponentVariantCatalog::Unavailable,
        ) => Err(ComponentVariantFinalizationError::ComponentCatalogUnavailable),
        (
            YtDlpComponentSelectionOpenIntent::Semantic(semantic_selection),
            PreparedComponentVariantCatalog::Installed { catalog, .. },
        ) => {
            let fresh_selection = catalog
                .rematch_semantic(semantic_selection)
                .map_err(ComponentVariantFinalizationError::SemanticRematch)?;
            stream_configuration
                .with_component_variants(catalog, fresh_selection)
                .map_err(ComponentVariantFinalizationError::Installation)
        }
    }
}
