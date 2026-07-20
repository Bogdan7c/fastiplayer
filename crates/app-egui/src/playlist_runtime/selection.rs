//! Process-lifetime multi-selection без влияния на playback и persistence.

use std::collections::HashSet;
use std::sync::Arc;

use playlist_core::{PlaylistEntryId, PlaylistQueue};

use super::view::PlaylistStructuralRevision;

/// Явно описывает, должен ли Clear сохранить keyboard interaction cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClearSelectionCursor {
    /// Escape снимает selection, но оставляет keyboard navigation на строке.
    Preserve,
    /// Клик по пустой области завершает и selection, и row interaction.
    Clear,
}

/// Exact app-level selection intent, построенный из revision-stable view snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateSelection {
    /// Обычный click/navigation заменяет selection одной строкой.
    Replace {
        /// Exact stable ID строки.
        entry_id: PlaylistEntryId,
        /// Structural revision, из которой UI взял ID.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+click переключает exact строку.
    Toggle {
        /// Exact stable ID строки.
        entry_id: PlaylistEntryId,
        /// Structural revision, из которой UI взял ID.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Shift заменяет selection exact canonical диапазоном.
    ReplaceRange {
        /// Уже разрешённые stable IDs диапазона в canonical порядке.
        entry_ids: Arc<[PlaylistEntryId]>,
        /// Stable range anchor.
        range_anchor: PlaylistEntryId,
        /// Строка, на которой заканчивается interaction.
        interaction_cursor: PlaylistEntryId,
        /// Structural revision, на которой разрешён диапазон.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+Shift добавляет exact canonical диапазон к selection.
    AddRange {
        /// Уже разрешённые stable IDs диапазона в canonical порядке.
        entry_ids: Arc<[PlaylistEntryId]>,
        /// Сохраняемый stable range anchor.
        range_anchor: PlaylistEntryId,
        /// Строка, на которой заканчивается interaction.
        interaction_cursor: PlaylistEntryId,
        /// Structural revision, на которой разрешён диапазон.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+A выбирает exact snapshot всей очереди.
    SelectAll {
        /// Все stable IDs в canonical порядке.
        entry_ids: Arc<[PlaylistEntryId]>,
        /// Anchor следующего Shift-range.
        range_anchor: Option<PlaylistEntryId>,
        /// Текущий keyboard interaction cursor.
        interaction_cursor: Option<PlaylistEntryId>,
        /// Structural revision exact snapshot-а.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+navigation переносит cursor без изменения selection.
    MoveCursor {
        /// Exact target строки.
        entry_id: PlaylistEntryId,
        /// Structural revision, из которой UI взял target.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Снимает selection с явно названной cursor-семантикой.
    Clear {
        /// Решение о сохранении keyboard interaction.
        cursor: ClearSelectionCursor,
    },
}

impl UpdateSelection {
    /// Возвращает expected revision для exact-ID intent-ов.
    const fn structural_revision(&self) -> Option<PlaylistStructuralRevision> {
        match self {
            Self::Replace {
                structural_revision,
                ..
            }
            | Self::Toggle {
                structural_revision,
                ..
            }
            | Self::ReplaceRange {
                structural_revision,
                ..
            }
            | Self::AddRange {
                structural_revision,
                ..
            }
            | Self::SelectAll {
                structural_revision,
                ..
            }
            | Self::MoveCursor {
                structural_revision,
                ..
            } => Some(*structural_revision),
            Self::Clear { .. } => None,
        }
    }
}

/// Typed результат не смешивает stale UI action, invalid IDs и реальный update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateSelectionOutcome {
    /// Selection, anchor или cursor изменились.
    Updated,
    /// Exact requested state уже опубликован.
    NoChange,
    /// UI action построен по старой structural revision.
    StaleStructuralRevision,
    /// Один exact ID отсутствует в committed queue.
    EntryNotFound {
        /// Первый отсутствующий ID.
        entry_id: PlaylistEntryId,
    },
    /// Exact range содержит повторяющийся ID.
    DuplicateEntryId {
        /// Первый обнаруженный повторяющийся ID.
        entry_id: PlaylistEntryId,
    },
    /// Range contract не содержит anchor или interaction cursor.
    InvalidRangeBoundary {
        /// Boundary, отсутствующий в exact range.
        entry_id: PlaylistEntryId,
    },
    /// Captured Shift-range не совпадает с canonical interval между boundary IDs.
    InvalidRangeItems,
    /// Select All payload не покрывает committed queue целиком.
    IncompleteSelectAll {
        /// Число committed строк authoritative queue.
        committed_count: usize,
        /// Число уникальных IDs в captured action.
        captured_count: usize,
    },
}

/// Arc-backed read model обеспечивает O(1) selected lookup в visible-row path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistSelectionSnapshot {
    selected_entry_ids: Arc<HashSet<PlaylistEntryId>>,
    range_anchor: Option<PlaylistEntryId>,
    interaction_cursor: Option<PlaylistEntryId>,
}

impl PlaylistSelectionSnapshot {
    /// Создаёт пустой process-lifetime selection snapshot.
    pub(super) fn empty() -> Self {
        Self {
            selected_entry_ids: Arc::new(HashSet::new()),
            range_anchor: None,
            interaction_cursor: None,
        }
    }

    /// O(1) membership lookup для виртуализированных строк.
    pub(crate) fn is_selected(&self, entry_id: PlaylistEntryId) -> bool {
        self.selected_entry_ids.contains(&entry_id)
    }

    /// Возвращает число выбранных stable IDs без обхода очереди.
    pub(crate) fn selected_count(&self) -> usize {
        self.selected_entry_ids.len()
    }

    /// Возвращает stable anchor для следующего Shift-range.
    pub(crate) const fn range_anchor(&self) -> Option<PlaylistEntryId> {
        self.range_anchor
    }

    /// Возвращает keyboard/pointer interaction cursor независимо от playback.
    pub(crate) const fn interaction_cursor(&self) -> Option<PlaylistEntryId> {
        self.interaction_cursor
    }

    /// Даёт controller-у shared set для exact Undo restore без O(N) payload clone.
    pub(super) fn selected_entry_ids(&self) -> &Arc<HashSet<PlaylistEntryId>> {
        &self.selected_entry_ids
    }
}

/// Mutable owner selection invariants; наружу публикуется только immutable snapshot.
#[derive(Debug, Clone)]
pub(super) struct PlaylistSelectionState {
    selected_entry_ids: Arc<HashSet<PlaylistEntryId>>,
    range_anchor: Option<PlaylistEntryId>,
    interaction_cursor: Option<PlaylistEntryId>,
}

impl Default for PlaylistSelectionState {
    fn default() -> Self {
        Self {
            selected_entry_ids: Arc::new(HashSet::new()),
            range_anchor: None,
            interaction_cursor: None,
        }
    }
}

impl PlaylistSelectionState {
    /// Cheap snapshot clone разделяет неизменяемый selected set.
    pub(super) fn snapshot(&self) -> PlaylistSelectionSnapshot {
        PlaylistSelectionSnapshot {
            selected_entry_ids: Arc::clone(&self.selected_entry_ids),
            range_anchor: self.range_anchor,
            interaction_cursor: self.interaction_cursor,
        }
    }

    /// Возвращает interaction cursor для focus adapters.
    pub(super) const fn interaction_cursor(&self) -> Option<PlaylistEntryId> {
        self.interaction_cursor
    }

    /// Возвращает cursor только если он действительно входит в selection.
    pub(super) fn selected_cursor(&self) -> Option<PlaylistEntryId> {
        self.interaction_cursor
            .filter(|entry_id| self.selected_entry_ids.contains(entry_id))
    }

    /// Применяет exact intent только к matching structural snapshot.
    pub(super) fn apply(
        &mut self,
        queue: &PlaylistQueue,
        structural_revision: PlaylistStructuralRevision,
        update: UpdateSelection,
    ) -> UpdateSelectionOutcome {
        if update
            .structural_revision()
            .is_some_and(|expected| expected != structural_revision)
        {
            return UpdateSelectionOutcome::StaleStructuralRevision;
        }

        let next = match update {
            UpdateSelection::Replace { entry_id, .. } => {
                if queue.top_level_entry(entry_id).is_none() {
                    return UpdateSelectionOutcome::EntryNotFound { entry_id };
                }
                Self {
                    selected_entry_ids: Arc::new(HashSet::from([entry_id])),
                    range_anchor: Some(entry_id),
                    interaction_cursor: Some(entry_id),
                }
            }
            UpdateSelection::Toggle { entry_id, .. } => {
                if queue.top_level_entry(entry_id).is_none() {
                    return UpdateSelectionOutcome::EntryNotFound { entry_id };
                }
                let mut selected_entry_ids = self.selected_entry_ids.as_ref().clone();
                if !selected_entry_ids.remove(&entry_id) {
                    selected_entry_ids.insert(entry_id);
                }
                Self {
                    selected_entry_ids: Arc::new(selected_entry_ids),
                    range_anchor: Some(entry_id),
                    interaction_cursor: Some(entry_id),
                }
            }
            UpdateSelection::ReplaceRange {
                entry_ids,
                range_anchor,
                interaction_cursor,
                ..
            } => match Self::validated_exact_range(
                queue,
                &entry_ids,
                range_anchor,
                interaction_cursor,
            ) {
                Ok(selected_entry_ids) => Self {
                    selected_entry_ids,
                    range_anchor: Some(range_anchor),
                    interaction_cursor: Some(interaction_cursor),
                },
                Err(outcome) => return outcome,
            },
            UpdateSelection::AddRange {
                entry_ids,
                range_anchor,
                interaction_cursor,
                ..
            } => match Self::validated_exact_range(
                queue,
                &entry_ids,
                range_anchor,
                interaction_cursor,
            ) {
                Ok(range_entry_ids) => {
                    let mut selected_entry_ids = self.selected_entry_ids.as_ref().clone();
                    selected_entry_ids.extend(range_entry_ids.iter().copied());
                    Self {
                        selected_entry_ids: Arc::new(selected_entry_ids),
                        range_anchor: Some(range_anchor),
                        interaction_cursor: Some(interaction_cursor),
                    }
                }
                Err(outcome) => return outcome,
            },
            UpdateSelection::SelectAll {
                entry_ids,
                range_anchor,
                interaction_cursor,
                ..
            } => {
                match Self::validated_exact_set(queue, &entry_ids, range_anchor, interaction_cursor)
                {
                    Ok(selected_entry_ids) => {
                        if selected_entry_ids.len() != queue.top_level_entry_count() {
                            return UpdateSelectionOutcome::IncompleteSelectAll {
                                committed_count: queue.top_level_entry_count(),
                                captured_count: selected_entry_ids.len(),
                            };
                        }
                        Self {
                            selected_entry_ids,
                            range_anchor,
                            interaction_cursor,
                        }
                    }
                    Err(outcome) => return outcome,
                }
            }
            UpdateSelection::MoveCursor { entry_id, .. } => {
                if queue.top_level_entry(entry_id).is_none() {
                    return UpdateSelectionOutcome::EntryNotFound { entry_id };
                }
                Self {
                    selected_entry_ids: Arc::clone(&self.selected_entry_ids),
                    range_anchor: self.range_anchor,
                    interaction_cursor: Some(entry_id),
                }
            }
            UpdateSelection::Clear { cursor } => Self {
                selected_entry_ids: Arc::new(HashSet::new()),
                range_anchor: None,
                interaction_cursor: match cursor {
                    ClearSelectionCursor::Preserve => self.interaction_cursor,
                    ClearSelectionCursor::Clear => None,
                },
            },
        };

        if self.same_state(&next) {
            return UpdateSelectionOutcome::NoChange;
        }
        *self = next;
        UpdateSelectionOutcome::Updated
    }

    /// Structural mutations сохраняют только IDs, всё ещё принадлежащие queue.
    pub(super) fn retain_committed(&mut self, queue: &PlaylistQueue) {
        if self.selected_entry_ids.is_empty()
            && self.range_anchor.is_none()
            && self.interaction_cursor.is_none()
        {
            return;
        }
        let committed_entry_ids: HashSet<_> = queue.iter_top_level_entry_ids().collect();
        let mut selected_entry_ids = self.selected_entry_ids.as_ref().clone();
        selected_entry_ids.retain(|entry_id| committed_entry_ids.contains(entry_id));
        if selected_entry_ids.len() != self.selected_entry_ids.len() {
            self.selected_entry_ids = Arc::new(selected_entry_ids);
        }
        self.range_anchor = self
            .range_anchor
            .filter(|entry_id| committed_entry_ids.contains(entry_id));
        self.interaction_cursor = self
            .interaction_cursor
            .filter(|entry_id| committed_entry_ids.contains(entry_id));
    }

    /// Removal owner публикует exact post-mutation selection и focus cursor.
    pub(super) fn replace_after_removal(
        &mut self,
        selected_entry_ids: HashSet<PlaylistEntryId>,
        range_anchor: Option<PlaylistEntryId>,
        interaction_cursor: Option<PlaylistEntryId>,
    ) {
        self.selected_entry_ids = Arc::new(selected_entry_ids);
        self.range_anchor = range_anchor;
        self.interaction_cursor = interaction_cursor;
    }

    /// Undo восстанавливает весь selection snapshot и фильтрует его по restored queue.
    pub(super) fn restore(&mut self, snapshot: PlaylistSelectionSnapshot, queue: &PlaylistQueue) {
        self.selected_entry_ids = Arc::clone(snapshot.selected_entry_ids());
        self.range_anchor = snapshot.range_anchor();
        self.interaction_cursor = snapshot.interaction_cursor();
        self.retain_committed(queue);
    }

    /// Валидирует exact range за O(N + K), не выполняя K линейных queue lookup-ов.
    fn validated_exact_set(
        queue: &PlaylistQueue,
        entry_ids: &[PlaylistEntryId],
        range_anchor: Option<PlaylistEntryId>,
        interaction_cursor: Option<PlaylistEntryId>,
    ) -> Result<Arc<HashSet<PlaylistEntryId>>, UpdateSelectionOutcome> {
        let committed_entry_ids: HashSet<_> = queue.iter_top_level_entry_ids().collect();
        let mut selected_entry_ids = HashSet::with_capacity(entry_ids.len());
        for entry_id in entry_ids {
            if !selected_entry_ids.insert(*entry_id) {
                return Err(UpdateSelectionOutcome::DuplicateEntryId {
                    entry_id: *entry_id,
                });
            }
            if !committed_entry_ids.contains(entry_id) {
                return Err(UpdateSelectionOutcome::EntryNotFound {
                    entry_id: *entry_id,
                });
            }
        }
        for boundary_entry_id in [range_anchor, interaction_cursor].into_iter().flatten() {
            if !selected_entry_ids.contains(&boundary_entry_id) {
                return Err(UpdateSelectionOutcome::InvalidRangeBoundary {
                    entry_id: boundary_entry_id,
                });
            }
        }
        Ok(Arc::new(selected_entry_ids))
    }

    /// Проверяет, что exact Shift payload совпадает с непрерывным canonical диапазоном.
    fn validated_exact_range(
        queue: &PlaylistQueue,
        entry_ids: &[PlaylistEntryId],
        range_anchor: PlaylistEntryId,
        interaction_cursor: PlaylistEntryId,
    ) -> Result<Arc<HashSet<PlaylistEntryId>>, UpdateSelectionOutcome> {
        let selected_entry_ids = Self::validated_exact_set(
            queue,
            entry_ids,
            Some(range_anchor),
            Some(interaction_cursor),
        )?;
        let anchor_index = queue
            .iter_top_level_entry_ids()
            .position(|entry_id| entry_id == range_anchor)
            .ok_or(UpdateSelectionOutcome::EntryNotFound {
                entry_id: range_anchor,
            })?;
        let cursor_index = queue
            .iter_top_level_entry_ids()
            .position(|entry_id| entry_id == interaction_cursor)
            .ok_or(UpdateSelectionOutcome::EntryNotFound {
                entry_id: interaction_cursor,
            })?;
        let range_start = anchor_index.min(cursor_index);
        let range_end = anchor_index.max(cursor_index);
        let expected_count = range_end - range_start + 1;
        let is_exact_range = selected_entry_ids.len() == expected_count
            && queue
                .iter_top_level_entry_ids()
                .skip(range_start)
                .take(expected_count)
                .all(|entry_id| selected_entry_ids.contains(&entry_id));
        if !is_exact_range {
            return Err(UpdateSelectionOutcome::InvalidRangeItems);
        }
        Ok(selected_entry_ids)
    }

    /// Сравнивает логическое состояние, а не адрес Arc allocation.
    fn same_state(&self, other: &Self) -> bool {
        self.selected_entry_ids == other.selected_entry_ids
            && self.range_anchor == other.range_anchor
            && self.interaction_cursor == other.interaction_cursor
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind,
    };

    use super::*;

    /// Строит committed queue и stable IDs без playback/runtime side effects.
    fn queue() -> (PlaylistQueue, Vec<PlaylistEntryId>) {
        let mut queue = PlaylistQueue::new();
        let outcome = queue
            .append_batch(
                (0..5)
                    .map(|index| {
                        PlaylistItemDraft::local(
                            LocalLocator::Native(PathBuf::from(format!("{index}.mp3"))),
                            None,
                            CachedPlaylistMetadata::new(
                                format!("{index}.mp3"),
                                PlaylistMediaKind::Audio,
                            ),
                        )
                    })
                    .collect(),
            )
            .expect("append");
        match outcome {
            playlist_core::AddItemsOutcome::Added(_) => {}
            playlist_core::AddItemsOutcome::NoItemsProvided => {
                panic!("fixture expected non-empty append")
            }
        }
        let ids = queue.iter_top_level_entry_ids().collect();
        (queue, ids)
    }

    /// Создаёт opaque revision через production checked transition, не раскрывая поле newtype.
    fn structural_revision(value: u64) -> PlaylistStructuralRevision {
        (0..value).fold(PlaylistStructuralRevision::INITIAL, |revision, _| {
            revision.checked_next().expect("fixture revision")
        })
    }

    #[test]
    fn desktop_selection_intents_preserve_anchor_cursor_and_exact_membership() {
        let (queue, ids) = queue();
        let revision = structural_revision(7);
        let mut selection = PlaylistSelectionState::default();

        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::Replace {
                    entry_id: ids[1],
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::Updated
        );
        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::AddRange {
                    entry_ids: Arc::from([ids[1], ids[2], ids[3]]),
                    range_anchor: ids[1],
                    interaction_cursor: ids[3],
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::Updated
        );
        let snapshot = selection.snapshot();
        assert_eq!(snapshot.selected_count(), 3);
        assert!(snapshot.is_selected(ids[1]));
        assert!(snapshot.is_selected(ids[2]));
        assert!(snapshot.is_selected(ids[3]));
        assert_eq!(snapshot.range_anchor(), Some(ids[1]));
        assert_eq!(snapshot.interaction_cursor(), Some(ids[3]));

        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::Toggle {
                    entry_id: ids[2],
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::Updated
        );
        assert!(!selection.snapshot().is_selected(ids[2]));
        assert_eq!(selection.snapshot().interaction_cursor(), Some(ids[2]));

        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::MoveCursor {
                    entry_id: ids[4],
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::Updated
        );
        assert_eq!(selection.snapshot().selected_count(), 2);
        assert_eq!(selection.snapshot().interaction_cursor(), Some(ids[4]));
    }

    #[test]
    fn stale_or_invalid_exact_selection_is_atomic() {
        let (queue, ids) = queue();
        let revision = structural_revision(3);
        let mut selection = PlaylistSelectionState::default();
        selection.apply(
            &queue,
            revision,
            UpdateSelection::Replace {
                entry_id: ids[0],
                structural_revision: revision,
            },
        );
        let before = selection.snapshot();

        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::Replace {
                    entry_id: ids[1],
                    structural_revision: structural_revision(2),
                },
            ),
            UpdateSelectionOutcome::StaleStructuralRevision
        );
        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::ReplaceRange {
                    entry_ids: Arc::from([ids[1], ids[1]]),
                    range_anchor: ids[1],
                    interaction_cursor: ids[1],
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::DuplicateEntryId { entry_id: ids[1] }
        );
        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::ReplaceRange {
                    entry_ids: Arc::from([ids[1], ids[3]]),
                    range_anchor: ids[1],
                    interaction_cursor: ids[3],
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::InvalidRangeItems
        );
        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::SelectAll {
                    entry_ids: Arc::from([ids[0], ids[1]]),
                    range_anchor: Some(ids[0]),
                    interaction_cursor: Some(ids[1]),
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::IncompleteSelectAll {
                committed_count: ids.len(),
                captured_count: 2,
            }
        );
        assert_eq!(selection.snapshot(), before);
    }

    #[test]
    fn select_all_and_clear_keep_cursor_policy_explicit() {
        let (queue, ids) = queue();
        let revision = structural_revision(4);
        let mut selection = PlaylistSelectionState::default();
        selection.apply(
            &queue,
            revision,
            UpdateSelection::SelectAll {
                entry_ids: ids.clone().into(),
                range_anchor: Some(ids[0]),
                interaction_cursor: Some(ids[2]),
                structural_revision: revision,
            },
        );
        assert_eq!(selection.snapshot().selected_count(), ids.len());

        selection.apply(
            &queue,
            revision,
            UpdateSelection::Clear {
                cursor: ClearSelectionCursor::Preserve,
            },
        );
        assert_eq!(selection.snapshot().selected_count(), 0);
        assert_eq!(selection.snapshot().interaction_cursor(), Some(ids[2]));

        selection.apply(
            &queue,
            revision,
            UpdateSelection::Clear {
                cursor: ClearSelectionCursor::Clear,
            },
        );
        assert_eq!(selection.snapshot().interaction_cursor(), None);
    }
}
