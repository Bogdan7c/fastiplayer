use std::fmt;

use crate::SemanticIdentity;
use crate::selection::ExactSelectionIdentity;
use crate::variant::{
    ComponentVariantCatalog, ComponentVariantError, ComponentVariantSelection,
    ComponentVariantSemanticSelectionRequest,
};

/// Shape provider-neutral active selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebMediaSelectionShapeKind {
    /// Выбран только parent candidate; component catalog не участвует.
    Candidate,
    /// Выбраны exact component rows из catalog parent candidate-а.
    Components,
}

/// Borrowed содержимое active selection без неочевидного `Option`.
#[derive(Debug, Clone, Copy)]
pub enum WebMediaSelectionShape<'a> {
    /// Parent candidate является complete active selection.
    Candidate,
    /// Active selection содержит проверенные component rows.
    Components(&'a ComponentVariantSelection),
}

/// Fresh owner, против которого выполняется semantic rematch.
#[derive(Debug, Clone, Copy)]
pub enum WebMediaSelectionRematchSource<'a> {
    /// Fresh parent candidate не использует component catalog.
    Candidate,
    /// Fresh parent candidate публикует этот проверенный component catalog.
    ComponentCatalog(&'a ComponentVariantCatalog),
}

impl WebMediaSelectionRematchSource<'_> {
    /// Возвращает shape свежего rematch owner-а.
    const fn shape_kind(self) -> WebMediaSelectionShapeKind {
        match self {
            Self::Candidate => WebMediaSelectionShapeKind::Candidate,
            Self::ComponentCatalog(_) => WebMediaSelectionShapeKind::Components,
        }
    }
}

/// Provider-neutral exact active selection поверх существующих identity/catalog API.
#[derive(Clone, PartialEq, Eq)]
pub struct WebMediaSelection {
    /// Exact+semantic parent candidate identity.
    parent: ExactSelectionIdentity,
    /// Optional-by-shape component selection без второго catalog representation.
    components: SelectedComponents,
}

/// Закрытая shape не позволяет caller-у собрать component selection без проверки parent.
#[derive(Clone, PartialEq, Eq)]
enum SelectedComponents {
    /// Parent candidate выбран целиком.
    Candidate,
    /// Component rows принадлежат тому же parent candidate.
    Components(ComponentVariantSelection),
}

impl WebMediaSelection {
    /// Создаёт selection полного parent candidate-а.
    pub const fn candidate(parent: ExactSelectionIdentity) -> Self {
        Self {
            parent,
            components: SelectedComponents::Candidate,
        }
    }

    /// Создаёт component selection только при exact совпадении catalog parent-а.
    pub fn with_components(
        parent: ExactSelectionIdentity,
        components: ComponentVariantSelection,
    ) -> Result<Self, WebMediaSelectionError> {
        if components.catalog_identity().parent() != &parent {
            return Err(WebMediaSelectionError::CrossParentSelection);
        }

        Ok(Self {
            parent,
            components: SelectedComponents::Components(components),
        })
    }

    /// Возвращает exact+semantic parent candidate identity.
    pub const fn parent(&self) -> &ExactSelectionIdentity {
        &self.parent
    }

    /// Возвращает active shape и, если нужно, borrowed component selection.
    pub const fn shape(&self) -> WebMediaSelectionShape<'_> {
        match &self.components {
            SelectedComponents::Candidate => WebMediaSelectionShape::Candidate,
            SelectedComponents::Components(components) => {
                WebMediaSelectionShape::Components(components)
            }
        }
    }

    /// Возвращает compact shape identity для diagnostics и matching.
    pub const fn shape_kind(&self) -> WebMediaSelectionShapeKind {
        match self.components {
            SelectedComponents::Candidate => WebMediaSelectionShapeKind::Candidate,
            SelectedComponents::Components(_) => WebMediaSelectionShapeKind::Components,
        }
    }

    /// Удаляет snapshot-local generations и создаёт refresh-stable reopen request.
    pub fn semantic_rematch_request(&self) -> WebMediaSemanticSelectionRequest {
        let shape = match &self.components {
            SelectedComponents::Candidate => SemanticSelectionShape::Candidate,
            SelectedComponents::Components(components) => {
                SemanticSelectionShape::Components(components.semantic_rematch_request())
            }
        };

        WebMediaSemanticSelectionRequest {
            parent: self.parent.semantic().clone(),
            shape,
        }
    }
}

impl fmt::Debug for WebMediaSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Debug публикует только уже redacted parent и semantic shape.
        formatter
            .debug_struct("WebMediaSelection")
            .field("parent", &self.parent)
            .field("shape", &self.shape_kind())
            .finish()
    }
}

/// Refresh-stable request, который нельзя использовать как exact open identity.
#[derive(Clone, PartialEq, Eq)]
pub struct WebMediaSemanticSelectionRequest {
    /// Refresh-stable parent candidate identity.
    parent: SemanticIdentity,
    /// Refresh-stable selection shape.
    shape: SemanticSelectionShape,
}

/// Закрытое semantic содержимое сохраняет layout shape без `Option` комбинаций.
#[derive(Clone, PartialEq, Eq)]
enum SemanticSelectionShape {
    /// Rematch требуется только parent candidate-у.
    Candidate,
    /// Rematch требуется parent candidate-у и его component rows.
    Components(ComponentVariantSemanticSelectionRequest),
}

impl WebMediaSemanticSelectionRequest {
    /// Возвращает refresh-stable parent identity.
    pub const fn parent(&self) -> &SemanticIdentity {
        &self.parent
    }

    /// Возвращает shape semantic request без раскрытия opaque keys.
    pub const fn shape_kind(&self) -> WebMediaSelectionShapeKind {
        match self.shape {
            SemanticSelectionShape::Candidate => WebMediaSelectionShapeKind::Candidate,
            SemanticSelectionShape::Components(_) => WebMediaSelectionShapeKind::Components,
        }
    }

    /// Rematch-ит request в exact identities свежего parent/catalog snapshot-а.
    pub fn rematch(
        &self,
        fresh_parent: ExactSelectionIdentity,
        source: WebMediaSelectionRematchSource<'_>,
    ) -> Result<WebMediaSelection, WebMediaSelectionError> {
        // Candidate semantic identity обязана существовать в свежем snapshot-е.
        if fresh_parent.semantic() != &self.parent {
            return Err(WebMediaSelectionError::MissingParentSemanticIdentity);
        }

        match (&self.shape, source) {
            (SemanticSelectionShape::Candidate, WebMediaSelectionRematchSource::Candidate) => {
                Ok(WebMediaSelection::candidate(fresh_parent))
            }
            (
                SemanticSelectionShape::Components(component_request),
                WebMediaSelectionRematchSource::ComponentCatalog(catalog),
            ) => {
                // Fresh catalog обязан принадлежать переданному fresh parent-у.
                if catalog.identity().parent() != &fresh_parent {
                    return Err(WebMediaSelectionError::FreshCatalogParentMismatch);
                }

                let components = catalog
                    .rematch_semantic(component_request.clone())
                    .map_err(WebMediaSelectionError::ComponentVariant)?;
                WebMediaSelection::with_components(fresh_parent, components)
            }
            _ => Err(WebMediaSelectionError::ShapeMismatch {
                expected: self.shape_kind(),
                provided: source.shape_kind(),
            }),
        }
    }
}

impl fmt::Debug for WebMediaSemanticSelectionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Component keys намеренно не входят даже в redacted outer Debug.
        formatter
            .debug_struct("WebMediaSemanticSelectionRequest")
            .field("parent", &self.parent)
            .field("shape", &self.shape_kind())
            .finish()
    }
}

/// Ошибки построения и semantic rematch provider-neutral selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebMediaSelectionError {
    /// Component selection принадлежит другому exact parent candidate-у.
    CrossParentSelection,
    /// Fresh parent больше не содержит requested semantic candidate identity.
    MissingParentSemanticIdentity,
    /// Fresh component catalog принадлежит не переданному fresh parent-у.
    FreshCatalogParentMismatch,
    /// Candidate/component shapes request-а и fresh owner-а различаются.
    ShapeMismatch {
        /// Shape semantic request-а.
        expected: WebMediaSelectionShapeKind,
        /// Shape fresh rematch owner-а.
        provided: WebMediaSelectionShapeKind,
    },
    /// Existing component catalog boundary отклонил exact/semantic operation.
    ComponentVariant(ComponentVariantError),
}

impl fmt::Display for WebMediaSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CrossParentSelection => {
                formatter.write_str("component selection belongs to another parent candidate")
            }
            Self::MissingParentSemanticIdentity => {
                formatter.write_str("semantic parent candidate disappeared after refresh")
            }
            Self::FreshCatalogParentMismatch => {
                formatter.write_str("fresh component catalog belongs to another parent candidate")
            }
            Self::ShapeMismatch { .. } => {
                formatter.write_str("semantic selection shape does not match fresh owner")
            }
            Self::ComponentVariant(source) => {
                write!(formatter, "component rematch failed: {source}")
            }
        }
    }
}

impl std::error::Error for WebMediaSelectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ComponentVariant(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WebMediaSelection, WebMediaSelectionError, WebMediaSelectionRematchSource,
        WebMediaSelectionShape, WebMediaSelectionShapeKind,
    };
    use crate::{
        CandidateFormatIdentity, CandidateIdentity, ComponentKind, ComponentVariantCatalog,
        ComponentVariantCatalogEntries, ComponentVariantCatalogGeneration,
        ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit, ComponentVariantError,
        ComponentVariantExactIdentity, ComponentVariantExactKey, ComponentVariantSelectionRequest,
        ComponentVariantSemanticIdentity, ComponentVariantSemanticKey, DynamicRange,
        ExactSelectionIdentity, ExtractionGeneration, NormalizedCodec, RawCodecIdentity,
        SemanticIdentity, SourceIdentity, VideoComponentVariant, VideoTrackDescriptor,
    };

    #[test]
    fn candidate_selection_roundtrips_semantically_into_fresh_exact_generation() {
        let original_parent = parent(3, 1, "format-v1", "stable-parent");
        let original_selection = WebMediaSelection::candidate(original_parent.clone());
        let request = original_selection.semantic_rematch_request();
        let fresh_parent = parent(3, 2, "format-v2", "stable-parent");

        let fresh_selection = request
            .rematch(
                fresh_parent.clone(),
                WebMediaSelectionRematchSource::Candidate,
            )
            .expect("stable semantic parent must rematch in fresh generation");

        // Fresh exact identity заменяет stale generation, semantic identity сохраняется.
        assert_eq!(fresh_selection.parent(), &fresh_parent);
        assert_ne!(fresh_selection.parent().exact(), original_parent.exact());
        assert_eq!(fresh_selection.parent().semantic(), request.parent());
        assert_eq!(
            fresh_selection.shape_kind(),
            WebMediaSelectionShapeKind::Candidate
        );
    }

    #[test]
    fn construction_rejects_component_selection_from_another_parent() {
        let active_parent = parent(1, 1, "active-format", "active-semantic");
        let foreign_parent = parent(1, 1, "foreign-format", "foreign-semantic");
        let (foreign_catalog, foreign_exact) =
            video_catalog(foreign_parent, 1, "foreign-video", "foreign-video-semantic");
        let foreign_selection = foreign_catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: foreign_exact,
            })
            .expect("foreign catalog row must be selectable");

        assert_eq!(
            WebMediaSelection::with_components(active_parent, foreign_selection),
            Err(WebMediaSelectionError::CrossParentSelection)
        );
    }

    #[test]
    fn semantic_rematch_reports_disappeared_component_without_fallback() {
        let original_parent = parent(7, 1, "format-v1", "stable-parent");
        let (original_catalog, original_exact) =
            video_catalog(original_parent.clone(), 1, "video-v1", "stable-video-v1");
        let original_components = original_catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: original_exact,
            })
            .expect("original component must be selectable");
        let selection = WebMediaSelection::with_components(original_parent, original_components)
            .expect("component parent must match");
        let rematch_request = selection.semantic_rematch_request();

        // Fresh parent сохраняет semantic identity, но requested component исчезает.
        let fresh_parent = parent(7, 2, "format-v2", "stable-parent");
        let (fresh_catalog, _) = video_catalog(
            fresh_parent.clone(),
            2,
            "video-v2",
            "different-video-semantic",
        );

        assert_eq!(
            rematch_request.rematch(
                fresh_parent,
                WebMediaSelectionRematchSource::ComponentCatalog(&fresh_catalog),
            ),
            Err(WebMediaSelectionError::ComponentVariant(
                ComponentVariantError::MissingSemanticVariant {
                    component: ComponentKind::Video,
                }
            ))
        );
    }

    #[test]
    fn semantic_rematch_rejects_invalid_fresh_shape() {
        let original_parent = parent(9, 1, "format-v1", "stable-parent");
        let (catalog, exact) =
            video_catalog(original_parent.clone(), 1, "video-v1", "stable-video");
        let components = catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly { video: exact })
            .expect("component must be selectable");
        let request = WebMediaSelection::with_components(original_parent, components)
            .expect("component parent must match")
            .semantic_rematch_request();
        let fresh_parent = parent(9, 2, "format-v2", "stable-parent");

        assert_eq!(
            request.rematch(fresh_parent, WebMediaSelectionRematchSource::Candidate),
            Err(WebMediaSelectionError::ShapeMismatch {
                expected: WebMediaSelectionShapeKind::Components,
                provided: WebMediaSelectionShapeKind::Candidate,
            })
        );
    }

    #[test]
    fn public_shape_and_error_chain_preserve_boundary_semantics() {
        // Candidate-only selection должна сообщать наружу именно candidate shape.
        let candidate_selection =
            WebMediaSelection::candidate(parent(11, 1, "candidate", "candidate-semantic"));
        // Публичный shape не должен раскрывать или выдумывать component selection.
        assert!(matches!(
            candidate_selection.shape(),
            WebMediaSelectionShape::Candidate
        ));

        // Собираем валидную component selection через настоящий catalog boundary.
        let component_parent = parent(12, 1, "parent", "parent-semantic");
        // Catalog и exact identity принадлежат одной generation и одному parent.
        let (component_catalog, component_exact) =
            video_catalog(component_parent.clone(), 1, "video", "video-semantic");
        // Exact lookup создаёт owned component selection с проверенными инвариантами.
        let selected_components = component_catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: component_exact,
            })
            .expect("валидный component должен выбираться");
        // Web selection принимает component shape только от того же parent.
        let component_selection =
            WebMediaSelection::with_components(component_parent, selected_components)
                .expect("component selection должна принадлежать parent");
        // Публичный borrowed shape обязан сохранить проверенный component selection.
        assert!(matches!(
            component_selection.shape(),
            WebMediaSelectionShape::Components(_)
        ));

        // Проверяем стабильные пользовательские причины всех boundary failures.
        let errors = [
            (
                WebMediaSelectionError::CrossParentSelection,
                "component selection belongs to another parent candidate",
            ),
            (
                WebMediaSelectionError::MissingParentSemanticIdentity,
                "semantic parent candidate disappeared after refresh",
            ),
            (
                WebMediaSelectionError::FreshCatalogParentMismatch,
                "fresh component catalog belongs to another parent candidate",
            ),
            (
                WebMediaSelectionError::ShapeMismatch {
                    expected: WebMediaSelectionShapeKind::Components,
                    provided: WebMediaSelectionShapeKind::Candidate,
                },
                "semantic selection shape does not match fresh owner",
            ),
            (
                WebMediaSelectionError::ComponentVariant(
                    ComponentVariantError::MissingSemanticVariant {
                        component: ComponentKind::Video,
                    },
                ),
                "component rematch failed: semantic Video variant отсутствует в catalog",
            ),
        ];
        // Display остаётся точным и не сваливает разные причины в общий bool/status.
        for (error, expected_message) in errors {
            // Форматированный текст является частью диагностического boundary.
            assert_eq!(error.to_string(), expected_message);
        }

        // Wrapper обязан сохранять исходную component-domain ошибку в standard error chain.
        let component_error = WebMediaSelectionError::ComponentVariant(
            ComponentVariantError::MissingSemanticVariant {
                component: ComponentKind::Video,
            },
        );
        // Источник доступен caller-у для typed диагностики.
        assert!(std::error::Error::source(&component_error).is_some());
        // Собственная selection-ошибка не должна притворяться обёрткой над чужой причиной.
        assert!(std::error::Error::source(&WebMediaSelectionError::CrossParentSelection).is_none());
    }

    #[test]
    fn debug_never_exposes_candidate_or_component_identity_material() {
        let parent = parent(
            11,
            1,
            "format?signature=top-secret",
            "semantic?token=top-secret",
        );
        let (catalog, exact) = video_catalog(
            parent.clone(),
            1,
            "video?signature=top-secret",
            "video-semantic?token=top-secret",
        );
        let components = catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly { video: exact })
            .expect("component must be selectable");
        let selection = WebMediaSelection::with_components(parent, components)
            .expect("component parent must match");
        let selection_debug = format!("{selection:?}");
        let request_debug = format!("{:?}", selection.semantic_rematch_request());

        for debug_output in [selection_debug, request_debug] {
            assert!(!debug_output.contains("top-secret"));
            assert!(!debug_output.contains("signature"));
            assert!(!debug_output.contains("token"));
        }
    }

    /// Создаёт exact parent identity для focused selection tests.
    fn parent(
        source_value: u64,
        generation: u64,
        format_key: &str,
        semantic_key: &str,
    ) -> ExactSelectionIdentity {
        let source = SourceIdentity::new(source_value);
        ExactSelectionIdentity::new(
            CandidateIdentity::new(
                source,
                ExtractionGeneration::new(generation),
                CandidateFormatIdentity::new(format_key).expect("format identity must be valid"),
            ),
            SemanticIdentity::new(source, semantic_key).expect("semantic identity must be valid"),
        )
        .expect("exact and semantic identities must share source")
    }

    /// Создаёт минимальный valid video-only catalog и exact row identity.
    fn video_catalog(
        parent: ExactSelectionIdentity,
        catalog_generation: u64,
        exact_key: &str,
        semantic_key: &str,
    ) -> (ComponentVariantCatalog, ComponentVariantExactIdentity) {
        let catalog_identity = ComponentVariantCatalogIdentity::new(
            parent.clone(),
            ComponentVariantCatalogGeneration::new(catalog_generation),
        );
        let exact_identity = ComponentVariantExactIdentity::new(
            catalog_identity.clone(),
            ComponentKind::Video,
            ComponentVariantExactKey::new(exact_key).expect("exact key must be valid"),
        );
        let semantic_identity = ComponentVariantSemanticIdentity::new(
            parent.semantic().clone(),
            ComponentKind::Video,
            ComponentVariantSemanticKey::new(semantic_key).expect("semantic key must be valid"),
        );
        let codec = NormalizedCodec::parse(
            RawCodecIdentity::new("avc1.64001f").expect("codec identity must be valid"),
        );
        let variant = VideoComponentVariant::new(
            exact_identity.clone(),
            semantic_identity,
            VideoTrackDescriptor::new(codec, None, None, None, None, DynamicRange::Sdr),
        );
        let limit = ComponentVariantCatalogLimit::new(1).expect("single row limit must be valid");
        let catalog = ComponentVariantCatalog::new(
            catalog_identity,
            limit,
            ComponentVariantCatalogEntries::VideoOnly {
                video: vec![variant],
            },
        )
        .expect("video-only catalog must be valid");

        (catalog, exact_identity)
    }
}
