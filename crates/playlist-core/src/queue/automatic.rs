//! Opaque automatic traversal plan and D08 commit boundary.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fmt;

use rand::Rng;

use crate::{PlaylistItemId, RepeatMode};

use super::navigation::{AutomaticEndedIntent, AutomaticStopReason, ManualNavigationDirection};
use super::shuffle::{ShuffleManualPreview, ShufflePreviewStep};
use super::{
    PlaylistQueue, PrepareReservedMutationError, PreparedQueueMutationToken, QueueRevisionSnapshot,
    ReservedQueueMutation, TraversalCurrentItemId,
};

/// Начальный automatic outcome сохраняет opaque plan рядом с concrete target.
pub enum AutomaticTraversalStart {
    OpenItem {
        item_id: PlaylistItemId,
        plan: Box<AutomaticTraversalPlan>,
    },
    ReplayCurrent {
        item_id: PlaylistItemId,
    },
    Stop(AutomaticStopReason),
}

/// Следующий шаг fixed-snapshot skip chain.
pub enum AutomaticTraversalAdvance {
    OpenItem {
        item_id: PlaylistItemId,
        plan: Box<AutomaticTraversalPlan>,
    },
    AllFailed {
        attempted_count: usize,
    },
}

/// Runtime-only plan не раскрывает shuffle upcoming/history controller-у.
pub struct AutomaticTraversalPlan {
    expected_revision: QueueRevisionSnapshot,
    repeat_mode: RepeatMode,
    eligible_item_ids: HashSet<PlaylistItemId>,
    attempted_item_ids: HashSet<PlaylistItemId>,
    canonical_candidates: Vec<PlaylistItemId>,
    canonical_cursor: usize,
    shuffle_preview: Option<ShuffleManualPreview>,
    target_item_id: PlaylistItemId,
}

impl AutomaticTraversalPlan {
    /// Concrete target, который должен коррелироваться с media-open request.
    pub const fn target_item_id(&self) -> PlaylistItemId {
        self.target_item_id
    }

    /// Bounded count нужен только для typed all-failed summary.
    pub fn attempted_count(&self) -> usize {
        self.attempted_item_ids.len()
    }
}

impl fmt::Debug for AutomaticTraversalPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AutomaticTraversalPlan")
            .field("expected_revision", &self.expected_revision)
            .field("repeat_mode", &self.repeat_mode)
            .field("eligible_count", &self.eligible_item_ids.len())
            .field("attempted_count", &self.attempted_item_ids.len())
            .field("target_item_id", &self.target_item_id)
            .finish()
    }
}

/// Token связывает fixed plan с единственным D08 reservation.
pub struct PreparedAutomaticTraversalToken {
    plan: AutomaticTraversalPlan,
    reservation_token: PreparedQueueMutationToken,
}

impl PreparedAutomaticTraversalToken {
    pub const fn target_item_id(&self) -> PlaylistItemId {
        self.plan.target_item_id
    }
}

impl fmt::Debug for PreparedAutomaticTraversalToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedAutomaticTraversalToken")
            .field("target_item_id", &self.target_item_id())
            .finish()
    }
}

/// Fallible prepare возвращает plan владельцу, чтобы cancellation/failure не теряли chain.
#[derive(Debug)]
pub struct PrepareAutomaticTraversalFailure {
    plan: Box<AutomaticTraversalPlan>,
    reason: PrepareReservedMutationError,
}

impl PrepareAutomaticTraversalFailure {
    pub const fn reason(&self) -> PrepareReservedMutationError {
        self.reason
    }

    pub fn into_plan(self) -> AutomaticTraversalPlan {
        *self.plan
    }
}

/// Successful commit публикует только exact target current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutomaticTraversalCommit {
    traversal_current: TraversalCurrentItemId,
}

impl AutomaticTraversalCommit {
    pub const fn traversal_current(self) -> TraversalCurrentItemId {
        self.traversal_current
    }
}

impl PlaylistQueue {
    /// Создаёт fixed committed snapshot для clean Ended transition.
    pub fn begin_automatic_traversal(
        &self,
        intent: AutomaticEndedIntent,
    ) -> AutomaticTraversalStart {
        let mut random = rand::rng();
        self.begin_automatic_traversal_with_rng(intent, false, &mut random)
    }

    /// Runtime error current уже считается первой неуспешной попыткой chain.
    pub fn begin_automatic_error_traversal(
        &self,
        intent: AutomaticEndedIntent,
    ) -> AutomaticTraversalStart {
        let mut random = rand::rng();
        self.begin_automatic_traversal_with_rng(intent, true, &mut random)
    }

    pub fn begin_automatic_traversal_with_rng<R: Rng + ?Sized>(
        &self,
        intent: AutomaticEndedIntent,
        current_already_failed: bool,
        random: &mut R,
    ) -> AutomaticTraversalStart {
        if self.is_empty() {
            return AutomaticTraversalStart::Stop(AutomaticStopReason::EmptyQueue);
        }
        let Some(current) = self.traversal_current() else {
            return AutomaticTraversalStart::Stop(AutomaticStopReason::CurrentItemAbsent);
        };
        let current_item_id = current.item_id();
        if intent.repeat_mode() == RepeatMode::RepeatOne {
            return if current_already_failed {
                AutomaticTraversalStart::Stop(AutomaticStopReason::EndOfQueue { current_item_id })
            } else {
                AutomaticTraversalStart::ReplayCurrent {
                    item_id: current_item_id,
                }
            };
        }

        let eligible_item_ids: HashSet<_> =
            self.items().iter().map(|item| item.item_id()).collect();
        let mut attempted_item_ids = HashSet::with_capacity(eligible_item_ids.len());
        if current_already_failed {
            attempted_item_ids.insert(current_item_id);
        }
        let canonical_candidates =
            self.automatic_canonical_candidates(current_item_id, intent.repeat_mode());
        let shuffle_preview = self
            .shuffle_traversal
            .as_ref()
            .map(ShuffleManualPreview::new);
        let mut plan = AutomaticTraversalPlan {
            expected_revision: self.revision_snapshot(),
            repeat_mode: intent.repeat_mode(),
            eligible_item_ids,
            attempted_item_ids,
            canonical_candidates,
            canonical_cursor: 0,
            shuffle_preview,
            target_item_id: current_item_id,
        };
        match self.next_automatic_candidate(&mut plan, random) {
            Some(item_id) => {
                plan.target_item_id = item_id;
                AutomaticTraversalStart::OpenItem {
                    item_id,
                    plan: Box::new(plan),
                }
            }
            None => {
                AutomaticTraversalStart::Stop(AutomaticStopReason::EndOfQueue { current_item_id })
            }
        }
    }

    /// Failed concrete target продвигает только plan snapshot; queue остаётся неизменной.
    pub fn advance_automatic_traversal_after_failure(
        &self,
        plan: AutomaticTraversalPlan,
    ) -> AutomaticTraversalAdvance {
        let mut random = rand::rng();
        self.advance_automatic_traversal_after_failure_with_rng(plan, &mut random)
    }

    pub fn advance_automatic_traversal_after_failure_with_rng<R: Rng + ?Sized>(
        &self,
        mut plan: AutomaticTraversalPlan,
        random: &mut R,
    ) -> AutomaticTraversalAdvance {
        plan.attempted_item_ids.insert(plan.target_item_id);
        plan.expected_revision = self.revision_snapshot();
        match self.next_automatic_candidate(&mut plan, random) {
            Some(item_id) => {
                plan.target_item_id = item_id;
                AutomaticTraversalAdvance::OpenItem {
                    item_id,
                    plan: Box::new(plan),
                }
            }
            None => AutomaticTraversalAdvance::AllFailed {
                attempted_count: plan.attempted_count(),
            },
        }
    }

    /// Повторно валидирует removal continuation против актуальной committed queue.
    pub fn revalidate_automatic_traversal(
        &self,
        mut plan: AutomaticTraversalPlan,
    ) -> AutomaticTraversalAdvance {
        plan.expected_revision = self.revision_snapshot();
        if self.automatic_candidate_is_available(&plan, plan.target_item_id) {
            return AutomaticTraversalAdvance::OpenItem {
                item_id: plan.target_item_id,
                plan: Box::new(plan),
            };
        }
        self.advance_automatic_traversal_after_failure(plan)
    }

    /// Matching Ready устанавливает reservation без раскрытия shuffle snapshot.
    pub fn prepare_automatic_traversal(
        &mut self,
        mut plan: AutomaticTraversalPlan,
    ) -> Result<PreparedAutomaticTraversalToken, PrepareAutomaticTraversalFailure> {
        let target_item_id = plan.target_item_id;
        // Structural admission/removal после старта chain не меняет fixed membership. Сам target
        // повторно валидируется reservation-ом относительно актуальной queue revision.
        plan.expected_revision = self.revision_snapshot();
        match self.prepare_reserved_mutation(
            plan.expected_revision,
            ReservedQueueMutation::select_committed(target_item_id),
        ) {
            Ok(reservation_token) => Ok(PreparedAutomaticTraversalToken {
                plan,
                reservation_token,
            }),
            Err(reason) => Err(PrepareAutomaticTraversalFailure {
                plan: Box::new(plan),
                reason,
            }),
        }
    }

    pub fn abort_automatic_traversal(
        &mut self,
        token: PreparedAutomaticTraversalToken,
    ) -> AutomaticTraversalPlan {
        self.abort_reserved(token.reservation_token);
        token.plan
    }

    /// Exact Installed коммитит current и opaque shuffle delta в одном owner turn.
    pub fn commit_automatic_traversal(
        &mut self,
        token: PreparedAutomaticTraversalToken,
    ) -> AutomaticTraversalCommit {
        let PreparedAutomaticTraversalToken {
            mut plan,
            reservation_token,
        } = token;
        let target_item_id = plan.target_item_id;
        let reservation_commit = self.commit_reserved(reservation_token);
        let traversal_current = reservation_commit.traversal_current();
        assert_eq!(traversal_current.item_id(), target_item_id);
        if let Some(mut shuffle_preview) = plan.shuffle_preview.take() {
            let retained_item_ids: HashSet<_> = self
                .items()
                .iter()
                .map(|item| item.item_id())
                .filter(|item_id| plan.eligible_item_ids.contains(item_id))
                .collect();
            shuffle_preview.retain_automatic_snapshot(&retained_item_ids);
            shuffle_preview.commit_into(
                self.shuffle_traversal
                    .as_mut()
                    .expect("automatic shuffle token requires enabled traversal"),
                target_item_id,
            );
        }
        AutomaticTraversalCommit { traversal_current }
    }

    fn next_automatic_candidate<R: Rng + ?Sized>(
        &self,
        plan: &mut AutomaticTraversalPlan,
        random: &mut R,
    ) -> Option<PlaylistItemId> {
        if plan.shuffle_preview.is_some() {
            let canonical_item_ids: Vec<_> = self
                .items()
                .iter()
                .map(|item| item.item_id())
                .filter(|item_id| plan.eligible_item_ids.contains(item_id))
                .collect();
            let maximum_steps = plan.eligible_item_ids.len().saturating_mul(2).max(1);
            for _ in 0..maximum_steps {
                let step = match plan.shuffle_preview.as_mut() {
                    Some(shuffle_preview) => shuffle_preview.step(
                        ManualNavigationDirection::Next,
                        plan.repeat_mode,
                        &canonical_item_ids,
                        self.traversal_current()
                            .map(TraversalCurrentItemId::item_id),
                        random,
                    ),
                    None => unreachable!("shuffle branch keeps its preview"),
                };
                let ShufflePreviewStep::Target(item_id) = step else {
                    continue;
                };
                if self.automatic_candidate_is_available(plan, item_id) {
                    return Some(item_id);
                }
                plan.attempted_item_ids.insert(item_id);
                if plan.attempted_item_ids.len() == plan.eligible_item_ids.len() {
                    return None;
                }
            }
            return None;
        }
        while let Some(item_id) = plan
            .canonical_candidates
            .get(plan.canonical_cursor)
            .copied()
        {
            plan.canonical_cursor += 1;
            if self.automatic_candidate_is_available(plan, item_id) {
                return Some(item_id);
            }
            plan.attempted_item_ids.insert(item_id);
        }
        None
    }

    fn automatic_candidate_is_available(
        &self,
        plan: &AutomaticTraversalPlan,
        item_id: PlaylistItemId,
    ) -> bool {
        plan.eligible_item_ids.contains(&item_id)
            && !plan.attempted_item_ids.contains(&item_id)
            && self.item(item_id).is_some()
    }

    fn automatic_canonical_candidates(
        &self,
        current_item_id: PlaylistItemId,
        repeat_mode: RepeatMode,
    ) -> Vec<PlaylistItemId> {
        let current_index = self
            .index_of(current_item_id)
            .expect("validated current remains committed");
        let mut candidates: Vec<_> = self.items()[current_index + 1..]
            .iter()
            .map(|item| item.item_id())
            .collect();
        if repeat_mode == RepeatMode::RepeatQueue {
            candidates.extend(
                self.items()[..=current_index]
                    .iter()
                    .map(|item| item.item_id()),
            );
        }
        candidates
    }
}
