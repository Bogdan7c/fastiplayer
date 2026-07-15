//! Runtime owner deterministic shuffle traversal.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use rand::seq::SliceRandom;
use rand::{Rng, RngExt};

use crate::{PlaylistItemId, RepeatMode};

use super::super::{
    PlaylistQueue, PlaylistQueueRestore, TraversalCurrentEffect, TraversalCurrentItemId,
    TraversalCurrentMutationError, TraversalCurrentMutationOutcome,
};
use super::{
    BulkRemoveError, BulkRemoveOutcome, MAX_SHUFFLE_HISTORY_ENTRIES, ShuffleHistoryCursor,
    ShuffleQueueRestoreError, ShuffleToggleError, ShuffleToggleOutcome,
    ShuffleTraversalRestoreError, ShuffleTraversalSnapshot,
};

/// Shared committed state: preview клонирует только `Arc`, а не O(N) vectors.
#[derive(Clone)]
pub(in crate::queue) struct ShuffleTraversal {
    history: Arc<Vec<PlaylistItemId>>,
    history_cursor: Option<usize>,
    upcoming: Arc<VecDeque<PlaylistItemId>>,
}

/// Один candidate, извлечённый из speculative upcoming.
struct SpeculativeUpcomingStep {
    item_id: PlaylistItemId,
    started_new_cycle: bool,
}

/// COW preview: shared base клонируется O(1), upcoming копируется максимум один раз.
pub(in crate::queue) struct ShuffleManualPreview {
    base_history: Arc<Vec<PlaylistItemId>>,
    base_history_cursor: Option<usize>,
    logical_history_cursor: Option<usize>,
    working_upcoming: Arc<VecDeque<PlaylistItemId>>,
    upcoming_steps: Vec<SpeculativeUpcomingStep>,
}

/// Результат одного speculative Next/Previous шага.
pub(in crate::queue) enum ShufflePreviewStep {
    Target(PlaylistItemId),
    Boundary,
    PreviousFromPersistedIdle,
    ReturnedToCommittedOrigin(PlaylistItemId),
}

impl ShuffleManualPreview {
    /// Заимствует committed base через Arc и не копирует O(N) collections.
    pub(in crate::queue) fn new(traversal: &ShuffleTraversal) -> Self {
        Self {
            base_history: Arc::clone(&traversal.history),
            base_history_cursor: traversal.history_cursor,
            logical_history_cursor: traversal.history_cursor,
            working_upcoming: Arc::clone(&traversal.upcoming),
            upcoming_steps: Vec::new(),
        }
    }

    /// Двигает logical cursor, не меняя committed traversal.
    pub(in crate::queue) fn step<R: Rng + ?Sized>(
        &mut self,
        direction: super::super::navigation::ManualNavigationDirection,
        repeat_mode: RepeatMode,
        canonical_item_ids: &[PlaylistItemId],
        committed_current_item_id: Option<PlaylistItemId>,
        random: &mut R,
    ) -> ShufflePreviewStep {
        match direction {
            super::super::navigation::ManualNavigationDirection::Next => self.step_next(
                repeat_mode,
                canonical_item_ids,
                committed_current_item_id,
                random,
            ),
            super::super::navigation::ManualNavigationDirection::Previous => self.step_previous(),
        }
    }

    /// Forward сначала использует известный factual tail, затем consumes upcoming.
    fn step_next<R: Rng + ?Sized>(
        &mut self,
        repeat_mode: RepeatMode,
        canonical_item_ids: &[PlaylistItemId],
        committed_current_item_id: Option<PlaylistItemId>,
        random: &mut R,
    ) -> ShufflePreviewStep {
        if self.upcoming_steps.is_empty()
            && let Some(cursor) = self.logical_history_cursor
            && let Some(item_id) = self.base_history.get(cursor + 1).copied()
        {
            let next_cursor = cursor + 1;
            self.logical_history_cursor = Some(next_cursor);
            if Some(next_cursor) == self.base_history_cursor {
                return ShufflePreviewStep::ReturnedToCommittedOrigin(item_id);
            }
            return ShufflePreviewStep::Target(item_id);
        }

        let mut started_new_cycle = false;
        if self.working_upcoming.is_empty() {
            if repeat_mode != RepeatMode::RepeatQueue {
                return ShufflePreviewStep::Boundary;
            }
            let Some(last_item_id) = self
                .upcoming_steps
                .last()
                .map(|step| step.item_id)
                .or_else(|| {
                    self.logical_history_cursor
                        .and_then(|cursor| self.base_history.get(cursor).copied())
                })
                .or(committed_current_item_id)
            else {
                return ShufflePreviewStep::Boundary;
            };
            self.working_upcoming = Arc::new(ShuffleTraversal::new_cycle(
                canonical_item_ids,
                last_item_id,
                random,
            ));
            started_new_cycle = true;
        }

        let item_id = Arc::make_mut(&mut self.working_upcoming)
            .pop_front()
            .expect("non-empty speculative upcoming after boundary handling");
        self.upcoming_steps.push(SpeculativeUpcomingStep {
            item_id,
            started_new_cycle,
        });
        ShufflePreviewStep::Target(item_id)
    }

    /// Backtrack возвращает candidate в preview upcoming либо двигает factual cursor назад.
    fn step_previous(&mut self) -> ShufflePreviewStep {
        if let Some(step) = self.upcoming_steps.pop() {
            if step.started_new_cycle {
                Arc::make_mut(&mut self.working_upcoming).clear();
            } else {
                Arc::make_mut(&mut self.working_upcoming).push_front(step.item_id);
            }
            if let Some(previous_step) = self.upcoming_steps.last() {
                return ShufflePreviewStep::Target(previous_step.item_id);
            }
            return self.target_at_logical_history_cursor_or_origin();
        }

        let Some(cursor) = self.logical_history_cursor else {
            return ShufflePreviewStep::PreviousFromPersistedIdle;
        };
        let Some(previous_cursor) = cursor.checked_sub(1) else {
            return ShufflePreviewStep::Boundary;
        };
        self.logical_history_cursor = Some(previous_cursor);
        if Some(previous_cursor) == self.base_history_cursor {
            let item_id = self.base_history[previous_cursor];
            return ShufflePreviewStep::ReturnedToCommittedOrigin(item_id);
        }
        ShufflePreviewStep::Target(self.base_history[previous_cursor])
    }

    /// После снятия последнего upcoming step preview возвращается exact к origin.
    fn target_at_logical_history_cursor_or_origin(&self) -> ShufflePreviewStep {
        match self.logical_history_cursor {
            Some(cursor) if Some(cursor) == self.base_history_cursor => {
                ShufflePreviewStep::ReturnedToCommittedOrigin(self.base_history[cursor])
            }
            Some(cursor) => ShufflePreviewStep::Target(self.base_history[cursor]),
            None => ShufflePreviewStep::PreviousFromPersistedIdle,
        }
    }

    /// Success публикует exact upcoming delta и только один factual target visit.
    pub(in crate::queue) fn commit_into(
        self,
        traversal: &mut ShuffleTraversal,
        latest_target_item_id: PlaylistItemId,
    ) {
        traversal.history = self.base_history;
        traversal.history_cursor = self.base_history_cursor;
        traversal.upcoming = self.working_upcoming;
        if self.upcoming_steps.is_empty() {
            let cursor = self
                .logical_history_cursor
                .expect("history-target preview must have a cursor");
            traversal.commit_history_cursor(cursor);
        } else {
            traversal.append_factual_visit(latest_target_item_id);
        }
    }

    /// Удаляет из snapshot-preview строки, которых больше нет в зафиксированной automatic chain.
    ///
    /// Это позволяет structural removal пропустить ровно прежний Item ID, не подмешивая поздно
    /// добавленные строки и не воскрешая удалённые history/upcoming entries при success commit.
    pub(in crate::queue) fn retain_automatic_snapshot(
        &mut self,
        retained_item_ids: &HashSet<PlaylistItemId>,
    ) {
        let retained_base_cursor = retained_cursor_after_filter(
            self.base_history.as_slice(),
            self.base_history_cursor,
            retained_item_ids,
        );
        let retained_logical_cursor = retained_cursor_after_filter(
            self.base_history.as_slice(),
            self.logical_history_cursor,
            retained_item_ids,
        );
        Arc::make_mut(&mut self.base_history).retain(|item_id| retained_item_ids.contains(item_id));
        self.base_history_cursor = retained_base_cursor;
        self.logical_history_cursor = retained_logical_cursor;
        Arc::make_mut(&mut self.working_upcoming)
            .retain(|item_id| retained_item_ids.contains(item_id));
        self.upcoming_steps
            .retain(|step| retained_item_ids.contains(&step.item_id));
    }
}

fn retained_cursor_after_filter(
    history: &[PlaylistItemId],
    cursor: Option<usize>,
    retained_item_ids: &HashSet<PlaylistItemId>,
) -> Option<usize> {
    let cursor = cursor?;
    let retained_through_cursor = history
        .iter()
        .take(cursor.saturating_add(1))
        .filter(|item_id| retained_item_ids.contains(item_id))
        .count();
    retained_through_cursor.checked_sub(1)
}

impl ShuffleTraversal {
    /// Создаёт deterministic enabled state для пустой idle queue.
    pub(in crate::queue) fn empty_idle() -> Self {
        Self {
            history: Arc::new(Vec::new()),
            history_cursor: None,
            upcoming: Arc::new(VecDeque::new()),
        }
    }

    /// Создаёт новый cycle, сохраняя current и исключая его из upcoming.
    pub(in crate::queue) fn fresh<R: Rng + ?Sized>(
        canonical_item_ids: &[PlaylistItemId],
        current: Option<TraversalCurrentItemId>,
        random: &mut R,
    ) -> Self {
        let current_item_id = current.map(TraversalCurrentItemId::item_id);
        let history = current_item_id.into_iter().collect();
        let history_cursor = current_item_id.map(|_| 0);
        let mut upcoming: Vec<_> = canonical_item_ids
            .iter()
            .copied()
            .filter(|item_id| Some(*item_id) != current_item_id)
            .collect();
        upcoming.shuffle(random);
        Self {
            history: Arc::new(history),
            history_cursor,
            upcoming: Arc::new(upcoming.into()),
        }
    }

    /// Проверяет persistence snapshot только против committed IDs/current.
    fn restore(
        snapshot: ShuffleTraversalSnapshot,
        canonical_item_ids: &[PlaylistItemId],
        current: Option<TraversalCurrentItemId>,
    ) -> Result<Self, ShuffleTraversalRestoreError> {
        if snapshot.history.len() > MAX_SHUFFLE_HISTORY_ENTRIES {
            return Err(ShuffleTraversalRestoreError::HistoryLimitExceeded {
                restored: snapshot.history.len(),
                maximum: MAX_SHUFFLE_HISTORY_ENTRIES,
            });
        }
        let canonical_ids: HashSet<_> = canonical_item_ids.iter().copied().collect();
        for item_id in &snapshot.history {
            if !canonical_ids.contains(item_id) {
                return Err(ShuffleTraversalRestoreError::HistoryItemNotCommitted {
                    item_id: *item_id,
                });
            }
        }
        let mut unique_upcoming = HashSet::with_capacity(snapshot.upcoming.len());
        for item_id in &snapshot.upcoming {
            if !canonical_ids.contains(item_id) {
                return Err(ShuffleTraversalRestoreError::UpcomingItemNotCommitted {
                    item_id: *item_id,
                });
            }
            if !unique_upcoming.insert(*item_id) {
                return Err(ShuffleTraversalRestoreError::DuplicateUpcomingItem {
                    item_id: *item_id,
                });
            }
        }

        let history_cursor = snapshot.history_cursor.map(ShuffleHistoryCursor::index);
        match current {
            Some(current) => {
                let current_item_id = current.item_id();
                let Some(cursor) = history_cursor else {
                    return Err(ShuffleTraversalRestoreError::InvalidHistoryCursor);
                };
                if snapshot.history.get(cursor) != Some(&current_item_id) {
                    return Err(ShuffleTraversalRestoreError::CurrentDoesNotMatchHistory {
                        current_item_id,
                    });
                }
                if unique_upcoming.contains(&current_item_id) {
                    return Err(ShuffleTraversalRestoreError::CurrentPresentInUpcoming {
                        current_item_id,
                    });
                }
            }
            None => {
                if !snapshot.history.is_empty() || history_cursor.is_some() {
                    return Err(ShuffleTraversalRestoreError::InvalidHistoryCursor);
                }
                if unique_upcoming != canonical_ids {
                    return Err(
                        ShuffleTraversalRestoreError::IdleUpcomingDoesNotCoverCanonicalQueue,
                    );
                }
            }
        }

        Ok(Self {
            history: Arc::new(snapshot.history),
            history_cursor,
            upcoming: Arc::new(snapshot.upcoming.into()),
        })
    }

    /// Публикует bounded persistence-facing копию одних ID.
    fn snapshot(&self) -> ShuffleTraversalSnapshot {
        ShuffleTraversalSnapshot {
            history: self.history.as_ref().clone(),
            history_cursor: self.history_cursor.map(ShuffleHistoryCursor),
            upcoming: self.upcoming.iter().copied().collect(),
        }
    }

    /// Возвращает первый persisted/generated upcoming target без mutation.
    pub(in crate::queue) fn next_upcoming(&self) -> Option<PlaylistItemId> {
        self.upcoming.front().copied()
    }

    /// Коммитит обычный Play/navigation target как один factual visit.
    pub(in crate::queue) fn commit_direct_transition(&mut self, target_item_id: PlaylistItemId) {
        self.remove_from_upcoming(target_item_id);
        self.append_factual_visit(target_item_id);
    }

    /// Перемещает cursor по уже factual history без создания duplicate visit.
    pub(in crate::queue) fn commit_history_cursor(&mut self, cursor: usize) {
        debug_assert!(cursor < self.history.len());
        self.history_cursor = Some(cursor);
    }

    /// Добавляет ровно один реальный target, отбрасывая forward tail новой ветки.
    pub(in crate::queue) fn append_factual_visit(&mut self, target_item_id: PlaylistItemId) {
        let history = Arc::make_mut(&mut self.history);
        let retained_len = self.history_cursor.map_or(0, |cursor| cursor + 1);
        history.truncate(retained_len);
        history.push(target_item_id);
        if history.len() > MAX_SHUFFLE_HISTORY_ENTRIES {
            let excess = history.len() - MAX_SHUFFLE_HISTORY_ENTRIES;
            history.drain(..excess);
        }
        self.history_cursor = Some(history.len() - 1);
    }

    /// Удаляет ID из set-like upcoming, не меняя порядок остальных.
    fn remove_from_upcoming(&mut self, target_item_id: PlaylistItemId) {
        Arc::make_mut(&mut self.upcoming).retain(|item_id| *item_id != target_item_id);
    }

    /// O(N+K) random merge сохраняет относительный порядок старого upcoming.
    pub(in crate::queue) fn merge_new_items<R: Rng + ?Sized>(
        &mut self,
        new_item_ids: &[PlaylistItemId],
        random: &mut R,
    ) -> (usize, usize) {
        if new_item_ids.is_empty() {
            return (0, 0);
        }
        let mut shuffled_new = new_item_ids.to_vec();
        shuffled_new.shuffle(random);
        let old_upcoming = self.upcoming.as_ref();
        let old_upcoming_len = old_upcoming.len();
        let mut merged = VecDeque::with_capacity(old_upcoming.len() + shuffled_new.len());
        let mut old_index = 0;
        let mut new_index = 0;
        while old_index < old_upcoming.len() || new_index < shuffled_new.len() {
            let old_remaining = old_upcoming.len() - old_index;
            let new_remaining = shuffled_new.len() - new_index;
            let take_old = if old_remaining == 0 {
                false
            } else if new_remaining == 0 {
                true
            } else {
                random.random_range(0..old_remaining + new_remaining) < old_remaining
            };
            if take_old {
                merged.push_back(old_upcoming[old_index]);
                old_index += 1;
            } else {
                merged.push_back(shuffled_new[new_index]);
                new_index += 1;
            }
        }
        self.upcoming = Arc::new(merged);
        (old_upcoming_len, shuffled_new.len())
    }

    /// Один retain/rebuild удаляет все references и чинит cursor.
    pub(in crate::queue) fn remove_items(
        &mut self,
        removed_item_ids: &HashSet<PlaylistItemId>,
        remaining_canonical_item_ids: &[PlaylistItemId],
        current_was_removed: bool,
    ) -> (usize, usize) {
        let upcoming_items_examined = self.upcoming.len();
        let history_items_examined = self.history.len();
        Arc::make_mut(&mut self.upcoming).retain(|item_id| !removed_item_ids.contains(item_id));
        if current_was_removed {
            self.history = Arc::new(Vec::new());
            self.history_cursor = None;
            let already_upcoming: HashSet<_> = self.upcoming.iter().copied().collect();
            let missing = remaining_canonical_item_ids
                .iter()
                .copied()
                .filter(|item_id| !already_upcoming.contains(item_id));
            Arc::make_mut(&mut self.upcoming).extend(missing);
            return (upcoming_items_examined, history_items_examined);
        }

        let old_cursor = self.history_cursor;
        let history = Arc::make_mut(&mut self.history);
        let removed_before_or_at_cursor = old_cursor.map_or(0, |cursor| {
            history
                .iter()
                .take(cursor + 1)
                .filter(|item_id| removed_item_ids.contains(item_id))
                .count()
        });
        history.retain(|item_id| !removed_item_ids.contains(item_id));
        if let Some(cursor) = old_cursor {
            self.history_cursor = Some(cursor - removed_before_or_at_cursor);
        }
        (upcoming_items_examined, history_items_examined)
    }

    /// Приводит traversal к валидному persisted-idle state без reshuffle canonical rows.
    pub(in crate::queue) fn make_idle(&mut self, canonical_item_ids: &[PlaylistItemId]) {
        self.history = Arc::new(Vec::new());
        self.history_cursor = None;
        let canonical_set: HashSet<_> = canonical_item_ids.iter().copied().collect();
        Arc::make_mut(&mut self.upcoming).retain(|item_id| canonical_set.contains(item_id));
        let already_upcoming: HashSet<_> = self.upcoming.iter().copied().collect();
        Arc::make_mut(&mut self.upcoming).extend(
            canonical_item_ids
                .iter()
                .copied()
                .filter(|item_id| !already_upcoming.contains(item_id)),
        );
    }

    /// Создаёт новую permutation и не допускает last→same first при len > 1.
    pub(in crate::queue) fn new_cycle<R: Rng + ?Sized>(
        canonical_item_ids: &[PlaylistItemId],
        last_item_id: PlaylistItemId,
        random: &mut R,
    ) -> VecDeque<PlaylistItemId> {
        let mut cycle = canonical_item_ids.to_vec();
        cycle.shuffle(random);
        if cycle.len() > 1 && cycle.first() == Some(&last_item_id) {
            let swap_index = random.random_range(1..cycle.len());
            cycle.swap(0, swap_index);
        }
        cycle.into()
    }
}

impl PlaylistQueue {
    /// Восстанавливает canonical queue и enabled shuffle одним atomic constructor-ом.
    pub fn restore_with_shuffle(
        queue_snapshot: PlaylistQueueRestore,
        shuffle_snapshot: ShuffleTraversalSnapshot,
    ) -> Result<Self, ShuffleQueueRestoreError> {
        let mut queue = Self::restore(queue_snapshot).map_err(ShuffleQueueRestoreError::Queue)?;
        let canonical_item_ids: Vec<_> = queue.items().iter().map(|item| item.item_id()).collect();
        let traversal = ShuffleTraversal::restore(
            shuffle_snapshot,
            &canonical_item_ids,
            queue.traversal_current,
        )
        .map_err(ShuffleQueueRestoreError::Traversal)?;
        queue.shuffle_traversal = Some(traversal);
        Ok(queue)
    }

    /// Показывает runtime shuffle flag без раскрытия storage.
    pub fn shuffle_enabled(&self) -> bool {
        self.shuffle_traversal.is_some()
    }

    /// Возвращает bounded exact snapshot либо `None`, когда shuffle выключен.
    pub fn shuffle_traversal_snapshot(&self) -> Option<ShuffleTraversalSnapshot> {
        self.shuffle_traversal
            .as_ref()
            .map(ShuffleTraversal::snapshot)
    }

    /// Коммитит explicit manual Play; повторный visit того же ID остаётся factual.
    pub fn commit_manual_play(
        &mut self,
        item_id: PlaylistItemId,
    ) -> Result<TraversalCurrentMutationOutcome, TraversalCurrentMutationError> {
        if self.shuffle_traversal.is_none()
            || self.traversal_current != Some(TraversalCurrentItemId(item_id))
        {
            return self.set_traversal_current(item_id);
        }
        if self.active_reservation.is_some() {
            return Err(TraversalCurrentMutationError::InstallCommitLinearizing);
        }
        let validated = self
            .validate_traversal_current(item_id)
            .map_err(|_| TraversalCurrentMutationError::ItemNotCommitted { item_id })?;
        let next_revision = self
            .traversal_revision
            .checked_next()
            .ok_or(TraversalCurrentMutationError::TraversalRevisionExhausted)?;
        self.shuffle_traversal
            .as_mut()
            .expect("checked enabled shuffle")
            .commit_direct_transition(item_id);
        self.traversal_revision = next_revision;
        Ok(TraversalCurrentMutationOutcome::Set(validated))
    }

    /// Удаляет requested IDs одним canonical retain и одним traversal rebuild.
    pub fn remove_batch(
        &mut self,
        requested_item_ids: &[PlaylistItemId],
    ) -> Result<BulkRemoveOutcome, BulkRemoveError> {
        if self.active_reservation.is_some() {
            return Err(BulkRemoveError::InstallCommitLinearizing);
        }
        if requested_item_ids.is_empty() {
            return Ok(BulkRemoveOutcome::NoItemsRequested);
        }
        let requested: HashSet<_> = requested_item_ids.iter().copied().collect();
        let committed_to_remove: HashSet<_> = self
            .items
            .iter()
            .map(|item| item.item_id())
            .filter(|item_id| requested.contains(item_id))
            .collect();
        if committed_to_remove.is_empty() {
            return Ok(BulkRemoveOutcome::NoMatchingItems);
        }
        self.commit_bulk_remove(&committed_to_remove)
    }

    /// `Remove Others` сохраняет exact ID и не вызывает K одиночных removals.
    pub fn remove_others(
        &mut self,
        retained_item_id: PlaylistItemId,
    ) -> Result<BulkRemoveOutcome, BulkRemoveError> {
        if self.active_reservation.is_some() {
            return Err(BulkRemoveError::InstallCommitLinearizing);
        }
        if self.item(retained_item_id).is_none() {
            return Ok(BulkRemoveOutcome::NoMatchingItems);
        }
        let committed_to_remove: HashSet<_> = self
            .items
            .iter()
            .map(|item| item.item_id())
            .filter(|item_id| *item_id != retained_item_id)
            .collect();
        if committed_to_remove.is_empty() {
            return Ok(BulkRemoveOutcome::NoMatchingItems);
        }
        self.commit_bulk_remove(&committed_to_remove)
    }

    /// Общий preflight/commit сохраняет atomicity и один revision publish.
    fn commit_bulk_remove(
        &mut self,
        committed_to_remove: &HashSet<PlaylistItemId>,
    ) -> Result<BulkRemoveOutcome, BulkRemoveError> {
        let next_structural_revision = self
            .structural_revision
            .checked_next()
            .ok_or(BulkRemoveError::StructuralRevisionExhausted)?;
        let clears_current = self
            .traversal_current
            .is_some_and(|current| committed_to_remove.contains(&current.item_id()));
        let next_traversal_revision = clears_current
            .then(|| {
                self.traversal_revision
                    .checked_next()
                    .ok_or(BulkRemoveError::TraversalRevisionExhausted)
            })
            .transpose()?;
        let remaining_canonical_item_ids: Vec<_> = self
            .items
            .iter()
            .map(|item| item.item_id())
            .filter(|item_id| !committed_to_remove.contains(item_id))
            .collect();
        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.remove_items(
                committed_to_remove,
                &remaining_canonical_item_ids,
                clears_current,
            );
        }
        self.items
            .retain(|item| !committed_to_remove.contains(&item.item_id()));
        self.structural_revision = next_structural_revision;
        let traversal_current_effect = if clears_current {
            self.traversal_current = None;
            self.traversal_revision =
                next_traversal_revision.expect("preflighted traversal revision");
            TraversalCurrentEffect::Cleared
        } else {
            TraversalCurrentEffect::Preserved
        };
        Ok(BulkRemoveOutcome::Removed {
            removed_item_count: committed_to_remove.len(),
            traversal_current_effect,
        })
    }

    /// Production toggle получает автоматически seeded thread-local entropy.
    pub fn enable_shuffle(&mut self) -> Result<ShuffleToggleOutcome, ShuffleToggleError> {
        let mut random = rand::rng();
        self.enable_shuffle_with_rng(&mut random)
    }

    /// Deterministic boundary для тестов и replayable domain simulations.
    pub fn enable_shuffle_with_rng<R: Rng + ?Sized>(
        &mut self,
        random: &mut R,
    ) -> Result<ShuffleToggleOutcome, ShuffleToggleError> {
        if self.active_reservation.is_some() {
            return Err(ShuffleToggleError::InstallCommitLinearizing);
        }
        if self.shuffle_traversal.is_some() {
            return Ok(ShuffleToggleOutcome::AlreadyEnabled);
        }
        let next_revision = self
            .traversal_revision
            .checked_next()
            .ok_or(ShuffleToggleError::TraversalRevisionExhausted)?;
        let canonical_item_ids: Vec<_> = self.items.iter().map(|item| item.item_id()).collect();
        self.shuffle_traversal = Some(ShuffleTraversal::fresh(
            &canonical_item_ids,
            self.traversal_current,
            random,
        ));
        self.traversal_revision = next_revision;
        Ok(ShuffleToggleOutcome::Enabled)
    }

    /// Выключает shuffle и полностью discard-ит traversal state.
    pub fn disable_shuffle(&mut self) -> Result<ShuffleToggleOutcome, ShuffleToggleError> {
        if self.active_reservation.is_some() {
            return Err(ShuffleToggleError::InstallCommitLinearizing);
        }
        if self.shuffle_traversal.is_none() {
            return Ok(ShuffleToggleOutcome::AlreadyDisabled);
        }
        let next_revision = self
            .traversal_revision
            .checked_next()
            .ok_or(ShuffleToggleError::TraversalRevisionExhausted)?;
        self.shuffle_traversal = None;
        self.traversal_revision = next_revision;
        Ok(ShuffleToggleOutcome::Disabled)
    }

    /// Возвращает следующий shuffle target с repeat-cycle policy D07b/D33.
    pub(in crate::queue) fn shuffle_next_target_with_rng<R: Rng + ?Sized>(
        &self,
        repeat_mode: RepeatMode,
        random: &mut R,
    ) -> Option<PlaylistItemId> {
        let traversal = self.shuffle_traversal.as_ref()?;
        if let Some(forward_cursor) = traversal.history_cursor.and_then(|cursor| {
            let next_cursor = cursor + 1;
            traversal.history.get(next_cursor).map(|_| next_cursor)
        }) {
            return traversal.history.get(forward_cursor).copied();
        }
        if let Some(item_id) = traversal.next_upcoming() {
            return Some(item_id);
        }
        if repeat_mode != RepeatMode::RepeatQueue {
            return None;
        }
        let current_item_id = self.traversal_current?.item_id();
        let canonical_item_ids: Vec<_> = self.items.iter().map(|item| item.item_id()).collect();
        Self::generated_cycle_first(&canonical_item_ids, current_item_id, random)
    }

    /// Выбирает first нового cycle, сохраняя правило last→different first.
    fn generated_cycle_first<R: Rng + ?Sized>(
        canonical_item_ids: &[PlaylistItemId],
        current_item_id: PlaylistItemId,
        random: &mut R,
    ) -> Option<PlaylistItemId> {
        ShuffleTraversal::new_cycle(canonical_item_ids, current_item_id, random)
            .front()
            .copied()
    }
}
