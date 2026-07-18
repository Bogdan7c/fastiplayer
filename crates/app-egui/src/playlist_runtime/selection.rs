//! Process-lifetime multi-selection без влияния на playback и persistence.

use std::collections::HashSet;
use std::sync::Arc;

use playlist_core::{PlaylistItemId, PlaylistQueue};

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
        item_id: PlaylistItemId,
        /// Structural revision, из которой UI взял ID.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+click переключает exact строку.
    Toggle {
        /// Exact stable ID строки.
        item_id: PlaylistItemId,
        /// Structural revision, из которой UI взял ID.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Shift заменяет selection exact canonical диапазоном.
    ReplaceRange {
        /// Уже разрешённые stable IDs диапазона в canonical порядке.
        item_ids: Arc<[PlaylistItemId]>,
        /// Stable range anchor.
        range_anchor: PlaylistItemId,
        /// Строка, на которой заканчивается interaction.
        interaction_cursor: PlaylistItemId,
        /// Structural revision, на которой разрешён диапазон.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+Shift добавляет exact canonical диапазон к selection.
    AddRange {
        /// Уже разрешённые stable IDs диапазона в canonical порядке.
        item_ids: Arc<[PlaylistItemId]>,
        /// Сохраняемый stable range anchor.
        range_anchor: PlaylistItemId,
        /// Строка, на которой заканчивается interaction.
        interaction_cursor: PlaylistItemId,
        /// Structural revision, на которой разрешён диапазон.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+A выбирает exact snapshot всей очереди.
    SelectAll {
        /// Все stable IDs в canonical порядке.
        item_ids: Arc<[PlaylistItemId]>,
        /// Anchor следующего Shift-range.
        range_anchor: Option<PlaylistItemId>,
        /// Текущий keyboard interaction cursor.
        interaction_cursor: Option<PlaylistItemId>,
        /// Structural revision exact snapshot-а.
        structural_revision: PlaylistStructuralRevision,
    },
    /// Ctrl/Cmd+navigation переносит cursor без изменения selection.
    MoveCursor {
        /// Exact target строки.
        item_id: PlaylistItemId,
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
    ItemNotFound {
        /// Первый отсутствующий ID.
        item_id: PlaylistItemId,
    },
    /// Exact range содержит повторяющийся ID.
    DuplicateItemId {
        /// Первый обнаруженный повторяющийся ID.
        item_id: PlaylistItemId,
    },
    /// Range contract не содержит anchor или interaction cursor.
    InvalidRangeBoundary {
        /// Boundary, отсутствующий в exact range.
        item_id: PlaylistItemId,
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
    selected_item_ids: Arc<HashSet<PlaylistItemId>>,
    range_anchor: Option<PlaylistItemId>,
    interaction_cursor: Option<PlaylistItemId>,
}

impl PlaylistSelectionSnapshot {
    /// Создаёт пустой process-lifetime selection snapshot.
    pub(super) fn empty() -> Self {
        Self {
            selected_item_ids: Arc::new(HashSet::new()),
            range_anchor: None,
            interaction_cursor: None,
        }
    }

    /// O(1) membership lookup для виртуализированных строк.
    pub(crate) fn is_selected(&self, item_id: PlaylistItemId) -> bool {
        self.selected_item_ids.contains(&item_id)
    }

    /// Возвращает число выбранных stable IDs без обхода очереди.
    pub(crate) fn selected_count(&self) -> usize {
        self.selected_item_ids.len()
    }

    /// Возвращает stable anchor для следующего Shift-range.
    pub(crate) const fn range_anchor(&self) -> Option<PlaylistItemId> {
        self.range_anchor
    }

    /// Возвращает keyboard/pointer interaction cursor независимо от playback.
    pub(crate) const fn interaction_cursor(&self) -> Option<PlaylistItemId> {
        self.interaction_cursor
    }

    /// Возвращает cursor только когда он входит в selected set.
    pub(crate) fn selected_cursor(&self) -> Option<PlaylistItemId> {
        self.interaction_cursor
            .filter(|item_id| self.selected_item_ids.contains(item_id))
    }

    /// Даёт controller-у shared set для exact Undo restore без O(N) payload clone.
    pub(super) fn selected_item_ids(&self) -> &Arc<HashSet<PlaylistItemId>> {
        &self.selected_item_ids
    }
}

/// Mutable owner selection invariants; наружу публикуется только immutable snapshot.
#[derive(Debug, Clone)]
pub(super) struct PlaylistSelectionState {
    selected_item_ids: Arc<HashSet<PlaylistItemId>>,
    range_anchor: Option<PlaylistItemId>,
    interaction_cursor: Option<PlaylistItemId>,
}

impl Default for PlaylistSelectionState {
    fn default() -> Self {
        Self {
            selected_item_ids: Arc::new(HashSet::new()),
            range_anchor: None,
            interaction_cursor: None,
        }
    }
}

impl PlaylistSelectionState {
    /// Cheap snapshot clone разделяет неизменяемый selected set.
    pub(super) fn snapshot(&self) -> PlaylistSelectionSnapshot {
        PlaylistSelectionSnapshot {
            selected_item_ids: Arc::clone(&self.selected_item_ids),
            range_anchor: self.range_anchor,
            interaction_cursor: self.interaction_cursor,
        }
    }

    /// Возвращает interaction cursor для focus adapters.
    pub(super) const fn interaction_cursor(&self) -> Option<PlaylistItemId> {
        self.interaction_cursor
    }

    /// Возвращает cursor только если он действительно входит в selection.
    pub(super) fn selected_cursor(&self) -> Option<PlaylistItemId> {
        self.interaction_cursor
            .filter(|item_id| self.selected_item_ids.contains(item_id))
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
            UpdateSelection::Replace { item_id, .. } => {
                if queue.item(item_id).is_none() {
                    return UpdateSelectionOutcome::ItemNotFound { item_id };
                }
                Self {
                    selected_item_ids: Arc::new(HashSet::from([item_id])),
                    range_anchor: Some(item_id),
                    interaction_cursor: Some(item_id),
                }
            }
            UpdateSelection::Toggle { item_id, .. } => {
                if queue.item(item_id).is_none() {
                    return UpdateSelectionOutcome::ItemNotFound { item_id };
                }
                let mut selected_item_ids = self.selected_item_ids.as_ref().clone();
                if !selected_item_ids.remove(&item_id) {
                    selected_item_ids.insert(item_id);
                }
                Self {
                    selected_item_ids: Arc::new(selected_item_ids),
                    range_anchor: Some(item_id),
                    interaction_cursor: Some(item_id),
                }
            }
            UpdateSelection::ReplaceRange {
                item_ids,
                range_anchor,
                interaction_cursor,
                ..
            } => match Self::validated_exact_range(
                queue,
                &item_ids,
                range_anchor,
                interaction_cursor,
            ) {
                Ok(selected_item_ids) => Self {
                    selected_item_ids,
                    range_anchor: Some(range_anchor),
                    interaction_cursor: Some(interaction_cursor),
                },
                Err(outcome) => return outcome,
            },
            UpdateSelection::AddRange {
                item_ids,
                range_anchor,
                interaction_cursor,
                ..
            } => match Self::validated_exact_range(
                queue,
                &item_ids,
                range_anchor,
                interaction_cursor,
            ) {
                Ok(range_item_ids) => {
                    let mut selected_item_ids = self.selected_item_ids.as_ref().clone();
                    selected_item_ids.extend(range_item_ids.iter().copied());
                    Self {
                        selected_item_ids: Arc::new(selected_item_ids),
                        range_anchor: Some(range_anchor),
                        interaction_cursor: Some(interaction_cursor),
                    }
                }
                Err(outcome) => return outcome,
            },
            UpdateSelection::SelectAll {
                item_ids,
                range_anchor,
                interaction_cursor,
                ..
            } => {
                match Self::validated_exact_set(queue, &item_ids, range_anchor, interaction_cursor)
                {
                    Ok(selected_item_ids) => {
                        if selected_item_ids.len() != queue.len() {
                            return UpdateSelectionOutcome::IncompleteSelectAll {
                                committed_count: queue.len(),
                                captured_count: selected_item_ids.len(),
                            };
                        }
                        Self {
                            selected_item_ids,
                            range_anchor,
                            interaction_cursor,
                        }
                    }
                    Err(outcome) => return outcome,
                }
            }
            UpdateSelection::MoveCursor { item_id, .. } => {
                if queue.item(item_id).is_none() {
                    return UpdateSelectionOutcome::ItemNotFound { item_id };
                }
                Self {
                    selected_item_ids: Arc::clone(&self.selected_item_ids),
                    range_anchor: self.range_anchor,
                    interaction_cursor: Some(item_id),
                }
            }
            UpdateSelection::Clear { cursor } => Self {
                selected_item_ids: Arc::new(HashSet::new()),
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
        if self.selected_item_ids.is_empty()
            && self.range_anchor.is_none()
            && self.interaction_cursor.is_none()
        {
            return;
        }
        let committed_item_ids: HashSet<_> =
            queue.items().iter().map(|item| item.item_id()).collect();
        let mut selected_item_ids = self.selected_item_ids.as_ref().clone();
        selected_item_ids.retain(|item_id| committed_item_ids.contains(item_id));
        if selected_item_ids.len() != self.selected_item_ids.len() {
            self.selected_item_ids = Arc::new(selected_item_ids);
        }
        self.range_anchor = self
            .range_anchor
            .filter(|item_id| committed_item_ids.contains(item_id));
        self.interaction_cursor = self
            .interaction_cursor
            .filter(|item_id| committed_item_ids.contains(item_id));
    }

    /// Removal owner публикует exact post-mutation selection и focus cursor.
    pub(super) fn replace_after_removal(
        &mut self,
        selected_item_ids: HashSet<PlaylistItemId>,
        range_anchor: Option<PlaylistItemId>,
        interaction_cursor: Option<PlaylistItemId>,
    ) {
        self.selected_item_ids = Arc::new(selected_item_ids);
        self.range_anchor = range_anchor;
        self.interaction_cursor = interaction_cursor;
    }

    /// Undo восстанавливает весь selection snapshot и фильтрует его по restored queue.
    pub(super) fn restore(&mut self, snapshot: PlaylistSelectionSnapshot, queue: &PlaylistQueue) {
        self.selected_item_ids = Arc::clone(snapshot.selected_item_ids());
        self.range_anchor = snapshot.range_anchor();
        self.interaction_cursor = snapshot.interaction_cursor();
        self.retain_committed(queue);
    }

    /// Валидирует exact range за O(N + K), не выполняя K линейных queue lookup-ов.
    fn validated_exact_set(
        queue: &PlaylistQueue,
        item_ids: &[PlaylistItemId],
        range_anchor: Option<PlaylistItemId>,
        interaction_cursor: Option<PlaylistItemId>,
    ) -> Result<Arc<HashSet<PlaylistItemId>>, UpdateSelectionOutcome> {
        let committed_item_ids: HashSet<_> =
            queue.items().iter().map(|item| item.item_id()).collect();
        let mut selected_item_ids = HashSet::with_capacity(item_ids.len());
        for item_id in item_ids {
            if !selected_item_ids.insert(*item_id) {
                return Err(UpdateSelectionOutcome::DuplicateItemId { item_id: *item_id });
            }
            if !committed_item_ids.contains(item_id) {
                return Err(UpdateSelectionOutcome::ItemNotFound { item_id: *item_id });
            }
        }
        for boundary_item_id in [range_anchor, interaction_cursor].into_iter().flatten() {
            if !selected_item_ids.contains(&boundary_item_id) {
                return Err(UpdateSelectionOutcome::InvalidRangeBoundary {
                    item_id: boundary_item_id,
                });
            }
        }
        Ok(Arc::new(selected_item_ids))
    }

    /// Проверяет, что exact Shift payload совпадает с непрерывным canonical диапазоном.
    fn validated_exact_range(
        queue: &PlaylistQueue,
        item_ids: &[PlaylistItemId],
        range_anchor: PlaylistItemId,
        interaction_cursor: PlaylistItemId,
    ) -> Result<Arc<HashSet<PlaylistItemId>>, UpdateSelectionOutcome> {
        let selected_item_ids = Self::validated_exact_set(
            queue,
            item_ids,
            Some(range_anchor),
            Some(interaction_cursor),
        )?;
        let anchor_index = queue
            .items()
            .iter()
            .position(|item| item.item_id() == range_anchor)
            .ok_or(UpdateSelectionOutcome::ItemNotFound {
                item_id: range_anchor,
            })?;
        let cursor_index = queue
            .items()
            .iter()
            .position(|item| item.item_id() == interaction_cursor)
            .ok_or(UpdateSelectionOutcome::ItemNotFound {
                item_id: interaction_cursor,
            })?;
        let range_start = anchor_index.min(cursor_index);
        let range_end = anchor_index.max(cursor_index);
        let expected_count = range_end - range_start + 1;
        let is_exact_range = selected_item_ids.len() == expected_count
            && queue.items()[range_start..=range_end]
                .iter()
                .all(|item| selected_item_ids.contains(&item.item_id()));
        if !is_exact_range {
            return Err(UpdateSelectionOutcome::InvalidRangeItems);
        }
        Ok(selected_item_ids)
    }

    /// Сравнивает логическое состояние, а не адрес Arc allocation.
    fn same_state(&self, other: &Self) -> bool {
        self.selected_item_ids == other.selected_item_ids
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
    fn queue() -> (PlaylistQueue, Vec<PlaylistItemId>) {
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
        let ids = match outcome {
            playlist_core::AddItemsOutcome::Added(item_ids) => item_ids.into_vec(),
            playlist_core::AddItemsOutcome::NoItemsProvided => {
                panic!("fixture expected non-empty append")
            }
        };
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
                    item_id: ids[1],
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
                    item_ids: Arc::from([ids[1], ids[2], ids[3]]),
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
                    item_id: ids[2],
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
                    item_id: ids[4],
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
                item_id: ids[0],
                structural_revision: revision,
            },
        );
        let before = selection.snapshot();

        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::Replace {
                    item_id: ids[1],
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
                    item_ids: Arc::from([ids[1], ids[1]]),
                    range_anchor: ids[1],
                    interaction_cursor: ids[1],
                    structural_revision: revision,
                },
            ),
            UpdateSelectionOutcome::DuplicateItemId { item_id: ids[1] }
        );
        assert_eq!(
            selection.apply(
                &queue,
                revision,
                UpdateSelection::ReplaceRange {
                    item_ids: Arc::from([ids[1], ids[3]]),
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
                    item_ids: Arc::from([ids[0], ids[1]]),
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
                item_ids: ids.clone().into(),
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
