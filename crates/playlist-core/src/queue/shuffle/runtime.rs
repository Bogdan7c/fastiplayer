//! Runtime owner deterministic shuffle traversal.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use rand::prelude::SliceRandom;
use rand::{Rng, RngExt};

use crate::{PlaylistEntryId, PlaylistItemId, RepeatMode};

use super::super::{
    PlaylistQueue, PlaylistQueueRestore, TraversalCurrentItemId, TraversalCurrentMutationError,
    TraversalCurrentMutationOutcome,
};
use super::{
    MAX_SHUFFLE_HISTORY_ENTRIES, ShuffleHistoryCursor, ShuffleQueueRestoreError,
    ShuffleToggleError, ShuffleToggleOutcome, ShuffleTraversalRestoreError,
    ShuffleTraversalSnapshot,
};

/// Shared committed state: preview клонирует только `Arc`, а не O(N) vectors.
#[derive(Clone)]
pub(in crate::queue) struct ShuffleTraversal {
    history: Arc<Vec<PlaylistItemId>>,
    history_cursor: Option<usize>,
    upcoming: Arc<VecDeque<PlaylistEntryId>>,
}

/// Exact playable visit вместе с top-level block owner-ом.
#[derive(Clone, Copy)]
pub(in crate::queue) struct ShuffleVisitIdentity {
    item_id: PlaylistItemId,
    entry_id: PlaylistEntryId,
}

/// Один candidate, извлечённый из speculative upcoming.
struct SpeculativeUpcomingStep {
    item_id: PlaylistItemId,
    consumed_entry_id: Option<PlaylistEntryId>,
    started_new_cycle: bool,
}

/// COW preview: shared base клонируется O(1), upcoming копируется максимум один раз.
pub(in crate::queue) struct ShuffleManualPreview {
    base_history: Arc<Vec<PlaylistItemId>>,
    base_history_cursor: Option<usize>,
    logical_history_cursor: Option<usize>,
    working_upcoming: Arc<VecDeque<PlaylistEntryId>>,
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
        queue: &PlaylistQueue,
        canonical_entry_ids: &[PlaylistEntryId],
        committed_current_item_id: Option<PlaylistItemId>,
        random: &mut R,
    ) -> ShufflePreviewStep {
        match direction {
            super::super::navigation::ManualNavigationDirection::Next => self.step_next(
                repeat_mode,
                queue,
                canonical_entry_ids,
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
        queue: &PlaylistQueue,
        canonical_entry_ids: &[PlaylistEntryId],
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

        let latest_speculative_item_id = self
            .upcoming_steps
            .last()
            .map(|step| step.item_id)
            .or_else(|| {
                self.logical_history_cursor
                    .and_then(|cursor| self.base_history.get(cursor).copied())
            })
            .or(committed_current_item_id);
        if let Some(next_part_item_id) = latest_speculative_item_id
            .and_then(|item_id| queue.next_playable_item_id_in_entry(item_id))
        {
            self.upcoming_steps.push(SpeculativeUpcomingStep {
                item_id: next_part_item_id,
                consumed_entry_id: None,
                started_new_cycle: false,
            });
            return ShufflePreviewStep::Target(next_part_item_id);
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
            let last_entry_id = queue.structural_entry_id_for_item(last_item_id);
            self.working_upcoming = Arc::new(ShuffleTraversal::new_cycle(
                canonical_entry_ids,
                last_entry_id,
                random,
            ));
            started_new_cycle = true;
        }

        while let Some(entry_id) = Arc::make_mut(&mut self.working_upcoming).pop_front() {
            let Some(item_id) = queue.first_playable_item_id(entry_id) else {
                continue;
            };
            self.upcoming_steps.push(SpeculativeUpcomingStep {
                item_id,
                consumed_entry_id: Some(entry_id),
                started_new_cycle,
            });
            return ShufflePreviewStep::Target(item_id);
        }
        ShufflePreviewStep::Boundary
    }

    /// Backtrack возвращает candidate в preview upcoming либо двигает factual cursor назад.
    fn step_previous(&mut self) -> ShufflePreviewStep {
        if let Some(step) = self.upcoming_steps.pop() {
            if step.started_new_cycle {
                Arc::make_mut(&mut self.working_upcoming).clear();
            } else if let Some(consumed_entry_id) = step.consumed_entry_id {
                Arc::make_mut(&mut self.working_upcoming).push_front(consumed_entry_id);
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

    /// Готовит exact source-order target после interactive queue replacement.
    ///
    /// Replacement очищает persisted current, но product contract требует
    /// `Next -> first` и `Previous -> last`, а не случайный shuffle target.
    /// Exact top-level entry удаляется из upcoming целиком, поэтому compound
    /// никогда не остаётся в shuffle cycle второй раз после выбора его part-а.
    pub(in crate::queue) fn select_source_order_target(
        &mut self,
        queue: &PlaylistQueue,
        target_item_id: PlaylistItemId,
    ) {
        let target_entry_id = queue
            .structural_entry_id_for_item(target_item_id)
            .expect("source-order target obtained from queue must retain its top-level entry");
        Arc::make_mut(&mut self.working_upcoming).retain(|entry_id| *entry_id != target_entry_id);
        self.upcoming_steps.push(SpeculativeUpcomingStep {
            item_id: target_item_id,
            consumed_entry_id: Some(target_entry_id),
            started_new_cycle: false,
        });
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
        retained_entry_ids: &HashSet<PlaylistEntryId>,
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
            .retain(|entry_id| retained_entry_ids.contains(entry_id));
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
        canonical_entry_ids: &[PlaylistEntryId],
        current: Option<ShuffleVisitIdentity>,
        random: &mut R,
    ) -> Self {
        let current_item_id = current.map(|identity| identity.item_id);
        let current_entry_id = current.map(|identity| identity.entry_id);
        let history = current_item_id.into_iter().collect();
        let history_cursor = current_item_id.map(|_| 0);
        let mut upcoming: Vec<_> = canonical_entry_ids
            .iter()
            .copied()
            .filter(|entry_id| Some(*entry_id) != current_entry_id)
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
        canonical_entry_ids: &[PlaylistEntryId],
        current: Option<TraversalCurrentItemId>,
        current_entry_id: Option<PlaylistEntryId>,
    ) -> Result<Self, ShuffleTraversalRestoreError> {
        if snapshot.history.len() > MAX_SHUFFLE_HISTORY_ENTRIES {
            return Err(ShuffleTraversalRestoreError::HistoryLimitExceeded {
                restored: snapshot.history.len(),
                maximum: MAX_SHUFFLE_HISTORY_ENTRIES,
            });
        }
        let canonical_ids: HashSet<_> = canonical_item_ids.iter().copied().collect();
        let canonical_entries: HashSet<_> = canonical_entry_ids.iter().copied().collect();
        for item_id in &snapshot.history {
            if !canonical_ids.contains(item_id) {
                return Err(ShuffleTraversalRestoreError::HistoryItemNotCommitted {
                    item_id: *item_id,
                });
            }
        }
        let mut unique_upcoming = HashSet::with_capacity(snapshot.upcoming.len());
        for entry_id in &snapshot.upcoming {
            if !canonical_entries.contains(entry_id) {
                return Err(ShuffleTraversalRestoreError::UpcomingEntryNotCommitted {
                    entry_id: *entry_id,
                });
            }
            if !unique_upcoming.insert(*entry_id) {
                return Err(ShuffleTraversalRestoreError::DuplicateUpcomingEntry {
                    entry_id: *entry_id,
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
                let current_entry_id = current_entry_id
                    .expect("validated current must resolve to one committed top-level entry");
                if unique_upcoming.contains(&current_entry_id) {
                    return Err(
                        ShuffleTraversalRestoreError::CurrentEntryPresentInUpcoming {
                            current_item_id,
                            current_entry_id,
                        },
                    );
                }
            }
            None => {
                if !snapshot.history.is_empty() || history_cursor.is_some() {
                    return Err(ShuffleTraversalRestoreError::InvalidHistoryCursor);
                }
                if unique_upcoming != canonical_entries {
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

    /// Возвращает первый persisted/generated upcoming entry без mutation.
    pub(in crate::queue) fn next_upcoming(&self) -> Option<PlaylistEntryId> {
        self.upcoming.front().copied()
    }

    /// Коммитит обычный Play/navigation target как один factual visit.
    pub(in crate::queue) fn commit_direct_transition(&mut self, target: ShuffleVisitIdentity) {
        self.remove_from_upcoming(target.entry_id);
        self.append_factual_visit(target.item_id);
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
    fn remove_from_upcoming(&mut self, target_entry_id: PlaylistEntryId) {
        Arc::make_mut(&mut self.upcoming).retain(|entry_id| *entry_id != target_entry_id);
    }

    /// O(N+K) random merge сохраняет относительный порядок старых upcoming entries.
    pub(in crate::queue) fn merge_new_entries<R: Rng + ?Sized>(
        &mut self,
        new_entry_ids: &[PlaylistEntryId],
        random: &mut R,
    ) -> (usize, usize) {
        if new_entry_ids.is_empty() {
            return (0, 0);
        }
        let mut shuffled_new = new_entry_ids.to_vec();
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
    pub(in crate::queue) fn remove_entries_and_items(
        &mut self,
        removed_entry_ids: &HashSet<PlaylistEntryId>,
        removed_item_ids: &HashSet<PlaylistItemId>,
        remaining_canonical_entry_ids: &[PlaylistEntryId],
        current_was_removed: bool,
    ) -> (usize, usize) {
        let upcoming_entries_examined = self.upcoming.len();
        let history_items_examined = self.history.len();
        Arc::make_mut(&mut self.upcoming).retain(|entry_id| !removed_entry_ids.contains(entry_id));
        if current_was_removed {
            self.history = Arc::new(Vec::new());
            self.history_cursor = None;
            let already_upcoming: HashSet<_> = self.upcoming.iter().copied().collect();
            let missing = remaining_canonical_entry_ids
                .iter()
                .copied()
                .filter(|entry_id| !already_upcoming.contains(entry_id));
            Arc::make_mut(&mut self.upcoming).extend(missing);
            return (upcoming_entries_examined, history_items_examined);
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
        (upcoming_entries_examined, history_items_examined)
    }

    /// Приводит traversal к валидному persisted-idle state без reshuffle canonical rows.
    pub(in crate::queue) fn make_idle(&mut self, canonical_entry_ids: &[PlaylistEntryId]) {
        self.history = Arc::new(Vec::new());
        self.history_cursor = None;
        let canonical_set: HashSet<_> = canonical_entry_ids.iter().copied().collect();
        Arc::make_mut(&mut self.upcoming).retain(|entry_id| canonical_set.contains(entry_id));
        let already_upcoming: HashSet<_> = self.upcoming.iter().copied().collect();
        Arc::make_mut(&mut self.upcoming).extend(
            canonical_entry_ids
                .iter()
                .copied()
                .filter(|entry_id| !already_upcoming.contains(entry_id)),
        );
    }

    /// Создаёт новую permutation и не допускает last→same first при len > 1.
    pub(in crate::queue) fn new_cycle<R: Rng + ?Sized>(
        canonical_entry_ids: &[PlaylistEntryId],
        last_entry_id: Option<PlaylistEntryId>,
        random: &mut R,
    ) -> VecDeque<PlaylistEntryId> {
        let mut cycle = canonical_entry_ids.to_vec();
        cycle.shuffle(random);
        if cycle.len() > 1 && cycle.first().copied() == last_entry_id {
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
        let canonical_item_ids = queue.iter_playable_ids().collect::<Vec<_>>();
        let canonical_entry_ids = queue.iter_top_level_entry_ids().collect::<Vec<_>>();
        let current_entry_id = queue
            .traversal_current
            .and_then(|current| queue.structural_entry_id_for_item(current.item_id()));
        let traversal = ShuffleTraversal::restore(
            shuffle_snapshot,
            &canonical_item_ids,
            &canonical_entry_ids,
            queue.traversal_current,
            current_entry_id,
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
        let target_identity = self
            .shuffle_visit_identity(item_id)
            .expect("validated manual Play target must have a top-level owner");
        self.shuffle_traversal
            .as_mut()
            .expect("checked enabled shuffle")
            .commit_direct_transition(target_identity);
        self.traversal_revision = next_revision;
        Ok(TraversalCurrentMutationOutcome::Set(validated))
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
        let canonical_entry_ids: Vec<_> = self.iter_top_level_entry_ids().collect();
        let current = self
            .traversal_current
            .and_then(|current| self.shuffle_visit_identity(current.item_id()));
        self.shuffle_traversal = Some(ShuffleTraversal::fresh(
            &canonical_entry_ids,
            current,
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
        let current_item_id = self.traversal_current?.item_id();
        if let Some(next_part_item_id) = self.next_playable_item_id_in_entry(current_item_id) {
            return Some(next_part_item_id);
        }
        if let Some(entry_id) = traversal.next_upcoming() {
            return self.first_playable_item_id(entry_id);
        }
        if repeat_mode != RepeatMode::RepeatQueue {
            return None;
        }
        let canonical_entry_ids: Vec<_> = self.iter_top_level_entry_ids().collect();
        let current_entry_id = self.structural_entry_id_for_item(current_item_id);
        Self::generated_cycle_first(self, &canonical_entry_ids, current_entry_id, random)
    }

    /// Выбирает first нового cycle, сохраняя правило last→different first.
    fn generated_cycle_first<R: Rng + ?Sized>(
        queue: &PlaylistQueue,
        canonical_entry_ids: &[PlaylistEntryId],
        current_entry_id: Option<PlaylistEntryId>,
        random: &mut R,
    ) -> Option<PlaylistItemId> {
        ShuffleTraversal::new_cycle(canonical_entry_ids, current_entry_id, random)
            .front()
            .copied()
            .and_then(|entry_id| queue.first_playable_item_id(entry_id))
    }

    /// Связывает exact playable target с top-level block identity.
    pub(in crate::queue) fn shuffle_visit_identity(
        &self,
        item_id: PlaylistItemId,
    ) -> Option<ShuffleVisitIdentity> {
        self.structural_entry_id_for_item(item_id)
            .map(|entry_id| ShuffleVisitIdentity { item_id, entry_id })
    }
}
