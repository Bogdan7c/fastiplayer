//! Secret-safe URL-sidebar action и единая pending projection.
//!
//! Здесь нет service locator, candidate/component identity или raw metadata:
//! UI передаёт только generation fences, axis и safe row indices.

use super::component_variants::ComponentVariantSelectionAction;
use super::{WebMediaCandidatePresentation, WebMediaStreamGeneration};

/// Typed UI intent не раскрывает candidate identity либо service locator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarAction {
    /// Выбирает candidate из exact installed generation по безопасному индексу.
    SelectCandidate {
        generation: WebMediaStreamGeneration,
        candidate_index: usize,
    },
    /// Выбирает независимый component variant через generation-fenced safe row action.
    SelectComponentVariant(ComponentVariantSelectionAction),
}

impl UrlSidebarAction {
    /// Возвращает generation родительской installed stream configuration.
    pub(crate) const fn parent_generation(self) -> WebMediaStreamGeneration {
        match self {
            Self::SelectCandidate { generation, .. } => generation,
            Self::SelectComponentVariant(action) => action.parent_generation(),
        }
    }
}

/// Ровно один pending selector для общего candidate/component strong reopen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlSidebarPendingSelection {
    /// Candidate projection хранит только safe presentation и parent generation.
    Candidate {
        parent_generation: WebMediaStreamGeneration,
        candidate: WebMediaCandidatePresentation,
    },
    /// Component projection хранит только generation fences, axis и row index.
    Component(ComponentVariantSelectionAction),
}

impl UrlSidebarPendingSelection {
    /// Возвращает generation родительской installed stream configuration.
    #[must_use]
    pub(crate) const fn parent_generation(&self) -> WebMediaStreamGeneration {
        match self {
            Self::Candidate {
                parent_generation, ..
            } => *parent_generation,
            Self::Component(action) => action.parent_generation(),
        }
    }
}

/// Ошибка ephemeral selector transition до запуска media-open транзакции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarTransitionError {
    /// Уже есть один pending same-item switch.
    Busy,
}

#[cfg(test)]
mod tests {
    #[test]
    fn action_boundary_contains_only_safe_generations_axis_and_indices() {
        let source = include_str!("sidebar_action.rs");
        for forbidden_type_parts in [
            ["Candidate", "Identity"],
            ["ComponentVariant", "Key"],
            ["Semantic", "Identity"],
            ["YtDlpCandidate", "Selection"],
            ["YtDlpMedia", "Locator"],
        ] {
            let forbidden_type = forbidden_type_parts.concat();
            assert!(
                !source.contains(&forbidden_type),
                "action boundary не должен содержать {forbidden_type}"
            );
        }
    }
}
