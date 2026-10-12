//! Typed component-catalog admission and lookup failures.

use super::*;

/// Ошибки catalog admission, lookup и immutable replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentVariantError {
    /// Variant или request принадлежит другому source lineage.
    SourceMismatch,
    /// Variant или request принадлежит другому active parent candidate.
    CrossParent,
    /// Exact identity относится к другой component catalog generation.
    StaleCatalogGeneration {
        /// Generation текущего catalog.
        expected: ComponentVariantCatalogGeneration,
        /// Generation request/variant.
        provided: ComponentVariantCatalogGeneration,
    },
    /// Identity помещена не в свою axis.
    WrongAxis {
        /// Axis, которую требует операция.
        expected: ComponentKind,
        /// Axis из identity.
        provided: ComponentKind,
    },
    /// Exact identity отсутствует в текущем catalog.
    MissingVariant {
        /// Axis, в которой выполнялся lookup.
        component: ComponentKind,
    },
    /// Refresh-stable identity отсутствует в свежем catalog.
    MissingSemanticVariant {
        /// Axis, в которой выполнялся semantic lookup.
        component: ComponentKind,
    },
    /// Catalog содержит повторяющуюся snapshot-local identity.
    DuplicateExactIdentity {
        /// Axis duplicate identity.
        component: ComponentKind,
    },
    /// Две rows имеют одну refresh-stable identity, поэтому rematch неоднозначен.
    AmbiguousSemanticIdentity {
        /// Axis ambiguous identity.
        component: ComponentKind,
    },
    /// Суммарная cardinality `V + A` превышает explicit caller budget.
    CatalogLimitExceeded {
        /// Фактическое число rows.
        provided_entries: usize,
        /// Caller-owned checked limit.
        maximum_entries: usize,
    },
    /// Логическая cardinality compatibility relation превышает caller budget.
    CompatibilityEdgeLimitExceeded {
        /// Фактическое число logical edges.
        provided_edges: usize,
        /// Caller-owned checked limit.
        maximum_edges: usize,
    },
    /// Compatibility или standalone reference не указывает на row своего pool.
    DanglingVariantReference {
        /// Pool, в котором ожидалась row.
        component: ComponentKind,
    },
    /// Standalone reference повторяется внутри одного mode set.
    DuplicateVariantReference {
        /// Повторяющаяся component axis.
        component: ComponentKind,
    },
    /// Sparse relation содержит одну exact пару несколько раз.
    DuplicateCompatibilityEdge,
    /// Exact component pair не разрешена catalog relation.
    IncompatibleComponentPair,
    /// Catalog не содержит ни одной selectable presentation.
    NoSelectablePresentation,
    /// Exact coupled identity отсутствует в catalog.
    MissingCoupledPresentation,
    /// Catalog содержит duplicate exact coupled identity.
    DuplicateCoupledExactIdentity,
    /// Catalog содержит ambiguous refresh-stable coupled identity.
    AmbiguousCoupledSemanticIdentity,
    /// Required axis пуста или отсутствует в layout.
    MissingRequiredAxis {
        /// Required axis.
        component: ComponentKind,
    },
    /// Request/selection shape не совпадает с catalog layout.
    LayoutMismatch,
}

impl fmt::Display for ComponentVariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceMismatch => formatter.write_str("The component variant is another source."),
            Self::CrossParent => {
                formatter.write_str("The component variant is another parent candidate.")
            }
            Self::StaleCatalogGeneration { expected, provided } => write!(
                formatter,
                "component catalog generation is stale: expected {}, received {}",
                expected.value(),
                provided.value()
            ),
            Self::WrongAxis { expected, provided } => write!(
                formatter,
                "component variant axis does not match: expected {expected:?}, received {provided:?}"
            ),
            Self::MissingVariant { component } => {
                write!(
                    formatter,
                    "exact {component:?} variant is not available in the catalog"
                )
            }
            Self::MissingSemanticVariant { component } => write!(
                formatter,
                "The semantic {component:?} variant is missing from the catalog."
            ),
            Self::DuplicateExactIdentity { component } => {
                write!(
                    formatter,
                    "catalog contains a duplicate exact {component:?} identity"
                )
            }
            Self::AmbiguousSemanticIdentity { component } => write!(
                formatter,
                "catalog contains an ambiguous semantic {component:?} identity"
            ),
            Self::CatalogLimitExceeded {
                provided_entries,
                maximum_entries,
            } => write!(
                formatter,
                "catalog contains {provided_entries} rows; the limit is {maximum_entries}"
            ),
            Self::CompatibilityEdgeLimitExceeded {
                provided_edges,
                maximum_edges,
            } => write!(
                formatter,
                "catalog contains {provided_edges} logical compatibility edges; the limit is {maximum_edges}"
            ),
            Self::DanglingVariantReference { component } => write!(
                formatter,
                "catalog contains a dangling {component:?} variant reference"
            ),
            Self::DuplicateVariantReference { component } => write!(
                formatter,
                "catalog contains a duplicate {component:?} variant reference"
            ),
            Self::DuplicateCompatibilityEdge => {
                formatter.write_str("catalog contains a duplicate compatibility edge")
            }
            Self::IncompatibleComponentPair => {
                formatter.write_str("video/audio component pair not allowed catalog relation")
            }
            Self::NoSelectablePresentation => {
                formatter.write_str("The catalog does not contain a selectable presentation.")
            }
            Self::MissingCoupledPresentation => {
                formatter.write_str("exact coupled presentation is not available in catalog")
            }
            Self::DuplicateCoupledExactIdentity => {
                formatter.write_str("catalog contains a duplicate exact coupled identity")
            }
            Self::AmbiguousCoupledSemanticIdentity => {
                formatter.write_str("catalog contains an ambiguous semantic coupled identity")
            }
            Self::MissingRequiredAxis { component } => {
                write!(formatter, "required {component:?} axis is empty or absent")
            }
            Self::LayoutMismatch => formatter
                .write_str("component selection shape is not consistent with catalog layout"),
        }
    }
}

impl std::error::Error for ComponentVariantError {}
