//! Planner-owned выбор среди opaque alternatives одной UI facet-группы.

use std::collections::HashSet;
use std::fmt;

use web_media_core::{CandidateIdentity, ExactSelectionIdentity, SemanticIdentity};

use crate::candidate::PlanningCandidateSnapshot;
use crate::capability::PlaybackCapabilitySnapshot;
use crate::planner::{CandidateRejection, PlaybackPlan, PlaybackPlanningError};
use crate::policy::PlaybackSelectionPolicy;

/// Best-first ranking всех playable candidates одного immutable snapshot-а.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayableOpaqueAlternativeRanking {
    ranked: Box<[PlaybackPlan]>,
    rejected_candidates: Box<[CandidateRejection]>,
}

impl PlayableOpaqueAlternativeRanking {
    /// Возвращает source-order-independent rank exact+semantic selection-а.
    pub fn rank_of(&self, selection: &ExactSelectionIdentity) -> Option<usize> {
        self.rank_of_candidate(selection.exact(), selection.semantic())
    }

    /// Возвращает rank для owners, хранящих exact и semantic identities раздельно.
    pub fn rank_of_candidate(
        &self,
        exact: &CandidateIdentity,
        semantic: &SemanticIdentity,
    ) -> Option<usize> {
        self.ranked
            .iter()
            .position(|plan| plan.exact_identity() == exact && plan.semantic_identity() == semantic)
    }

    /// Возвращает capability/policy rejections того же полного pass-а.
    pub const fn rejected_candidates(&self) -> &[CandidateRejection] {
        &self.rejected_candidates
    }
}

/// Ранжирует все playable opaque alternatives существующим planner policy.
pub fn rank_playable_opaque_alternatives(
    candidates: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Result<PlayableOpaqueAlternativeRanking, PlaybackPlanningError> {
    let (ranked, rejected_candidates) =
        crate::planner::rank_playable_candidates(candidates, capabilities, policy)?;
    Ok(PlayableOpaqueAlternativeRanking {
        ranked: ranked.into_boxed_slice(),
        rejected_candidates,
    })
}

/// Stable rank одного opaque app choice-а; меньшее значение предпочтительнее.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpaqueAlternativeRank {
    parent_playable_rank: usize,
    canonical_child_rank: usize,
}

impl OpaqueAlternativeRank {
    /// Candidate без provider-internal inventory использует только planner rank.
    #[must_use]
    pub const fn parent(parent_playable_rank: usize) -> Self {
        Self {
            parent_playable_rank,
            canonical_child_rank: usize::MAX,
        }
    }

    /// Provider row дополняет parent rank canonical source-order-independent rank-ом.
    #[must_use]
    pub const fn provider(parent_playable_rank: usize, canonical_provider_rank: usize) -> Self {
        Self {
            parent_playable_rank,
            canonical_child_rank: canonical_provider_rank,
        }
    }

    /// Возвращает planner-issued parent rank для provider child composition.
    #[must_use]
    pub const fn parent_playable_rank(self) -> usize {
        self.parent_playable_rank
    }
}

/// Один opaque choice после app-owned truthful facet filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupedOpaqueAlternative {
    choice_index: usize,
    preserved_facets: usize,
    rank: OpaqueAlternativeRank,
}

impl GroupedOpaqueAlternative {
    /// Строит named input без передачи identity либо source-local order comparator-у.
    #[must_use]
    pub const fn new(
        choice_index: usize,
        preserved_facets: usize,
        rank: OpaqueAlternativeRank,
    ) -> Self {
        Self {
            choice_index,
            preserved_facets,
            rank,
        }
    }
}

/// Group selection не допускает неявного source-order tie-break-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupedOpaqueAlternativeError {
    /// Caller передал пустую достижимую группу.
    Empty,
    /// Один logical choice не может встречаться дважды.
    DuplicateChoiceIndex,
    /// Полностью одинаковый rank не разрешается snapshot order-ом.
    DuplicateRank,
}

impl fmt::Display for GroupedOpaqueAlternativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "opaque alternative group is empty",
            Self::DuplicateChoiceIndex => {
                "opaque alternative group contains duplicate choice index"
            }
            Self::DuplicateRank => "opaque alternative group contains ambiguous duplicate rank",
        })
    }
}

impl std::error::Error for GroupedOpaqueAlternativeError {}

/// Выбирает лучший choice: сначала сохраняет максимум lower facets, затем planner rank.
pub fn select_grouped_opaque_alternative(
    alternatives: &[GroupedOpaqueAlternative],
) -> Result<usize, GroupedOpaqueAlternativeError> {
    if alternatives.is_empty() {
        return Err(GroupedOpaqueAlternativeError::Empty);
    }
    let mut choice_indices = HashSet::with_capacity(alternatives.len());
    let mut ranks = HashSet::with_capacity(alternatives.len());
    for alternative in alternatives {
        if !choice_indices.insert(alternative.choice_index) {
            return Err(GroupedOpaqueAlternativeError::DuplicateChoiceIndex);
        }
        if !ranks.insert((alternative.preserved_facets, alternative.rank)) {
            return Err(GroupedOpaqueAlternativeError::DuplicateRank);
        }
    }
    alternatives
        .iter()
        .min_by(|left, right| {
            right
                .preserved_facets
                .cmp(&left.preserved_facets)
                .then_with(|| left.rank.cmp(&right.rank))
        })
        .map(|alternative| alternative.choice_index)
        .ok_or(GroupedOpaqueAlternativeError::Empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_selection_preserves_facets_then_uses_rank_not_input_order() {
        let best = GroupedOpaqueAlternative::new(7, 3, OpaqueAlternativeRank::provider(1, 0));
        let worse = GroupedOpaqueAlternative::new(9, 3, OpaqueAlternativeRank::provider(2, 0));
        let preserves_more =
            GroupedOpaqueAlternative::new(11, 4, OpaqueAlternativeRank::provider(8, 0));

        assert_eq!(
            select_grouped_opaque_alternative(&[best, worse]).unwrap(),
            7
        );
        assert_eq!(
            select_grouped_opaque_alternative(&[worse, best]).unwrap(),
            7
        );
        assert_eq!(
            select_grouped_opaque_alternative(&[best, preserves_more]).unwrap(),
            11
        );
    }

    #[test]
    fn ambiguous_rank_is_rejected_instead_of_falling_back_to_source_order() {
        let rank = OpaqueAlternativeRank::parent(2);
        assert_eq!(
            select_grouped_opaque_alternative(&[
                GroupedOpaqueAlternative::new(1, 4, rank),
                GroupedOpaqueAlternative::new(2, 4, rank),
            ]),
            Err(GroupedOpaqueAlternativeError::DuplicateRank)
        );
    }
}
