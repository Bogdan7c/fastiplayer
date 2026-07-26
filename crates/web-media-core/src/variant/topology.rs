//! Аддитивная topology component catalog без материализации Cartesian product.

use std::fmt;
use std::num::NonZeroUsize;

use super::*;

/// Жёсткий safety ceiling логических compatibility edges одного catalog.
pub const MAX_COMPONENT_VARIANT_COMPATIBILITY_EDGES: usize = 1_048_576;

/// Ошибка explicit compatibility-edge budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentVariantEdgeLimitError {
    /// Нулевой limit не допускает ни одной composed пары.
    Zero,
    /// Caller limit превышает crate safety ceiling.
    AboveMaximum {
        /// Запрошенное число edges.
        provided_edges: usize,
        /// Общий safety ceiling.
        maximum_edges: usize,
    },
}

impl fmt::Display for ComponentVariantEdgeLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("component compatibility edge limit равен нулю"),
            Self::AboveMaximum {
                provided_edges,
                maximum_edges,
            } => write!(
                formatter,
                "component compatibility edge limit {provided_edges} превышает максимум {maximum_edges}"
            ),
        }
    }
}

impl std::error::Error for ComponentVariantEdgeLimitError {}

/// Caller-owned checked budget логической relation composed A/V.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentVariantEdgeLimit(NonZeroUsize);

impl ComponentVariantEdgeLimit {
    /// Проверяет ненулевой caller budget и общий safety ceiling.
    pub fn new(maximum_edges: usize) -> Result<Self, ComponentVariantEdgeLimitError> {
        let maximum_edges =
            NonZeroUsize::new(maximum_edges).ok_or(ComponentVariantEdgeLimitError::Zero)?;
        if maximum_edges.get() > MAX_COMPONENT_VARIANT_COMPATIBILITY_EDGES {
            return Err(ComponentVariantEdgeLimitError::AboveMaximum {
                provided_edges: maximum_edges.get(),
                maximum_edges: MAX_COMPONENT_VARIANT_COMPATIBILITY_EDGES,
            });
        }
        Ok(Self(maximum_edges))
    }

    /// Возвращает maximum logical edge count.
    pub const fn maximum_edges(self) -> usize {
        self.0.get()
    }
}

/// Одна явно доказанная compatible video/audio пара.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentVariantCompatibilityEdge {
    video: ComponentVariantExactIdentity,
    audio: ComponentVariantExactIdentity,
}

impl ComponentVariantCompatibilityEdge {
    /// Создаёт edge; catalog admission проверит scope, axes и dangling references.
    pub const fn new(
        video: ComponentVariantExactIdentity,
        audio: ComponentVariantExactIdentity,
    ) -> Self {
        Self { video, audio }
    }

    /// Возвращает exact video endpoint.
    pub const fn video(&self) -> &ComponentVariantExactIdentity {
        &self.video
    }

    /// Возвращает exact audio endpoint.
    pub const fn audio(&self) -> &ComponentVariantExactIdentity {
        &self.audio
    }
}

/// Непроверенная relation component pools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentVariantCompatibilityEntries {
    /// Composed A/V из этих pools не публикуется.
    Unavailable,
    /// Каждая video row совместима с каждой audio row без хранения `V x A`.
    AllPairs {
        /// Budget проверяет логическую cardinality relation.
        edge_limit: ComponentVariantEdgeLimit,
    },
    /// Только перечисленные пары совместимы.
    Sparse {
        /// Caller-owned edge budget.
        edge_limit: ComponentVariantEdgeLimit,
        /// Exact pair relation.
        edges: Vec<ComponentVariantCompatibilityEdge>,
    },
}

/// Проверенная immutable relation component pools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentVariantCompatibility {
    kind: ComponentVariantCompatibilityKind,
    logical_edge_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ComponentVariantCompatibilityKind {
    Unavailable,
    AllPairs,
    Sparse(Box<[ComponentVariantCompatibilityEdge]>),
}

impl ComponentVariantCompatibility {
    /// Возвращает число логически допустимых пар без требования их materialization.
    pub const fn logical_edge_count(&self) -> usize {
        self.logical_edge_count
    }

    /// Сообщает, разрешена ли exact component pair.
    pub fn allows(
        &self,
        video: &ComponentVariantExactIdentity,
        audio: &ComponentVariantExactIdentity,
    ) -> bool {
        match &self.kind {
            ComponentVariantCompatibilityKind::Unavailable => false,
            ComponentVariantCompatibilityKind::AllPairs => true,
            ComponentVariantCompatibilityKind::Sparse(edges) => edges
                .iter()
                .any(|edge| edge.video() == video && edge.audio() == audio),
        }
    }

    pub(super) const fn unavailable() -> Self {
        Self {
            kind: ComponentVariantCompatibilityKind::Unavailable,
            logical_edge_count: 0,
        }
    }

    pub(super) const fn all_pairs(logical_edge_count: usize) -> Self {
        Self {
            kind: ComponentVariantCompatibilityKind::AllPairs,
            logical_edge_count,
        }
    }

    pub(super) fn sparse(edges: Vec<ComponentVariantCompatibilityEdge>) -> Self {
        let logical_edge_count = edges.len();
        Self {
            kind: ComponentVariantCompatibilityKind::Sparse(edges.into_boxed_slice()),
            logical_edge_count,
        }
    }
}

/// Snapshot-local exact identity coupled/muxed presentation.
#[derive(Clone, PartialEq, Eq)]
pub struct CoupledVariantExactIdentity {
    catalog: ComponentVariantCatalogIdentity,
    key: ComponentVariantExactKey,
}

impl CoupledVariantExactIdentity {
    /// Создаёт exact coupled identity внутри одного catalog generation.
    pub const fn new(
        catalog: ComponentVariantCatalogIdentity,
        key: ComponentVariantExactKey,
    ) -> Self {
        Self { catalog, key }
    }

    /// Возвращает catalog scope.
    pub const fn catalog(&self) -> &ComponentVariantCatalogIdentity {
        &self.catalog
    }
}

impl fmt::Debug for CoupledVariantExactIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CoupledVariantExactIdentity")
            .field("catalog", &self.catalog)
            .field("key", &self.key)
            .finish()
    }
}

/// Refresh-stable semantic identity coupled/muxed presentation.
#[derive(Clone, PartialEq, Eq)]
pub struct CoupledVariantSemanticIdentity {
    parent: SemanticIdentity,
    key: ComponentVariantSemanticKey,
}

impl CoupledVariantSemanticIdentity {
    /// Создаёт semantic identity внутри одной parent lineage.
    pub const fn new(parent: SemanticIdentity, key: ComponentVariantSemanticKey) -> Self {
        Self { parent, key }
    }

    /// Возвращает refresh-stable parent identity.
    pub const fn parent(&self) -> &SemanticIdentity {
        &self.parent
    }

    /// Возвращает source lineage.
    pub const fn source(&self) -> SourceIdentity {
        self.parent.source()
    }
}

impl fmt::Debug for CoupledVariantSemanticIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CoupledVariantSemanticIdentity")
            .field("parent", &self.parent)
            .field("key", &self.key)
            .finish()
    }
}

/// Одна complete coupled A/V presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoupledComponentVariant {
    exact_identity: CoupledVariantExactIdentity,
    semantic_identity: CoupledVariantSemanticIdentity,
    video: VideoTrackDescriptor,
    audio: AudioTrackDescriptor,
}

impl CoupledComponentVariant {
    /// Собирает provider-normalized row; catalog admission проверит scope и uniqueness.
    pub const fn new(
        exact_identity: CoupledVariantExactIdentity,
        semantic_identity: CoupledVariantSemanticIdentity,
        video: VideoTrackDescriptor,
        audio: AudioTrackDescriptor,
    ) -> Self {
        Self {
            exact_identity,
            semantic_identity,
            video,
            audio,
        }
    }

    /// Возвращает snapshot-local identity.
    pub const fn exact_identity(&self) -> &CoupledVariantExactIdentity {
        &self.exact_identity
    }

    /// Возвращает refresh-stable identity.
    pub const fn semantic_identity(&self) -> &CoupledVariantSemanticIdentity {
        &self.semantic_identity
    }

    /// Возвращает video descriptor.
    pub const fn video(&self) -> &VideoTrackDescriptor {
        &self.video
    }

    /// Возвращает audio descriptor.
    pub const fn audio(&self) -> &AudioTrackDescriptor {
        &self.audio
    }
}
