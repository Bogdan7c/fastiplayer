//! Атомарное перемещение нескольких canonical строк как одного блока.

use std::collections::HashSet;
use std::fmt;

use crate::PlaylistItemId;

use super::{MoveItemIntent, PlaylistQueue};

/// Typed outcome группового перемещения не смешивает ошибки запроса и no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveItemsOutcome {
    /// Caller не передал ни одного stable Item ID.
    NoItemsRequested,
    /// Один Item ID повторился, поэтому запрос неоднозначен.
    DuplicateItemId {
        /// Первый обнаруженный повторяющийся ID.
        item_id: PlaylistItemId,
    },
    /// Один из requested Item ID отсутствует в committed queue.
    ItemNotFound {
        /// Первый отсутствующий ID в caller order.
        item_id: PlaylistItemId,
    },
    /// Named anchor отсутствует в committed queue.
    AnchorNotFound {
        /// Отсутствующий anchor ID.
        anchor_item_id: PlaylistItemId,
    },
    /// Anchor входит в перемещаемую группу и потому не задаёт внешнюю границу.
    AnchorSelected {
        /// Requested anchor, принадлежащий группе.
        anchor_item_id: PlaylistItemId,
    },
    /// Итоговый canonical order совпадает с текущим.
    AlreadyInPlace {
        /// Количество проверенных уникальных строк.
        item_count: usize,
    },
    /// Весь блок опубликован одной structural revision.
    Moved {
        /// Количество перемещённых строк.
        item_count: usize,
    },
    /// D08 reservation удерживает structural mutation lock.
    InstallCommitLinearizing,
    /// Structural revision нельзя продвинуть без нарушения monotonicity.
    StructuralRevisionExhausted,
}

impl fmt::Display for MoveItemsOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoItemsRequested => formatter.write_str("не переданы строки для перемещения"),
            Self::DuplicateItemId { item_id } => {
                write!(formatter, "Item ID {item_id} передан повторно")
            }
            Self::ItemNotFound { item_id } => write!(formatter, "Item ID {item_id} не найден"),
            Self::AnchorNotFound { anchor_item_id } => {
                write!(formatter, "anchor {anchor_item_id} не найден")
            }
            Self::AnchorSelected { anchor_item_id } => {
                write!(
                    formatter,
                    "anchor {anchor_item_id} входит в перемещаемую группу"
                )
            }
            Self::AlreadyInPlace { item_count } => {
                write!(
                    formatter,
                    "{item_count} строк уже находятся на requested месте"
                )
            }
            Self::Moved { item_count } => {
                write!(formatter, "{item_count} строк перемещены одним блоком")
            }
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit временно блокирует group move")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural revision исчерпана")
            }
        }
    }
}

impl PlaylistQueue {
    /// Перемещает requested IDs одним блоком в их текущем canonical порядке.
    pub fn move_items(
        &mut self,
        requested_item_ids: &[PlaylistItemId],
        intent: MoveItemIntent,
    ) -> MoveItemsOutcome {
        if requested_item_ids.is_empty() {
            return MoveItemsOutcome::NoItemsRequested;
        }

        // Один set одновременно валидирует uniqueness и обслуживает O(1) membership.
        let mut selected_item_ids = HashSet::with_capacity(requested_item_ids.len());
        for item_id in requested_item_ids {
            if !selected_item_ids.insert(*item_id) {
                return MoveItemsOutcome::DuplicateItemId { item_id: *item_id };
            }
        }

        // Проверка committed membership остаётся O(N + K), включая очередь на 50k строк.
        let committed_item_ids: HashSet<_> = self.items.iter().map(|item| item.item_id()).collect();
        for item_id in requested_item_ids {
            if !committed_item_ids.contains(item_id) {
                return MoveItemsOutcome::ItemNotFound { item_id: *item_id };
            }
        }

        // Anchor обязан быть внешней stable границей перемещаемого блока.
        let anchor_item_id = match intent {
            MoveItemIntent::ToFront | MoveItemIntent::ToBack => None,
            MoveItemIntent::Before(anchor_item_id) | MoveItemIntent::After(anchor_item_id) => {
                Some(anchor_item_id)
            }
        };
        if let Some(anchor_item_id) = anchor_item_id {
            if !committed_item_ids.contains(&anchor_item_id) {
                return MoveItemsOutcome::AnchorNotFound { anchor_item_id };
            }
            if selected_item_ids.contains(&anchor_item_id) {
                return MoveItemsOutcome::AnchorSelected { anchor_item_id };
            }
        }

        if self.active_reservation.is_some() {
            return MoveItemsOutcome::InstallCommitLinearizing;
        }

        // Canonical scan игнорирует caller order и тем самым сохраняет взаимный порядок строк.
        let selected_items = self
            .items
            .iter()
            .filter(|item| selected_item_ids.contains(&item.item_id()))
            .cloned()
            .collect::<Vec<_>>();
        let mut retained_items = self
            .items
            .iter()
            .filter(|item| !selected_item_ids.contains(&item.item_id()))
            .cloned()
            .collect::<Vec<_>>();

        // Индекс вычисляется уже в retained order, поэтому удалённые строки его не сдвигают.
        let insertion_index = match intent {
            MoveItemIntent::ToFront => 0,
            MoveItemIntent::ToBack => retained_items.len(),
            MoveItemIntent::Before(anchor_item_id) => retained_items
                .iter()
                .position(|item| item.item_id() == anchor_item_id)
                .expect("validated unselected anchor must remain committed"),
            MoveItemIntent::After(anchor_item_id) => {
                retained_items
                    .iter()
                    .position(|item| item.item_id() == anchor_item_id)
                    .expect("validated unselected anchor must remain committed")
                    + 1
            }
        };
        retained_items.splice(insertion_index..insertion_index, selected_items);

        if retained_items == self.items {
            return MoveItemsOutcome::AlreadyInPlace {
                item_count: selected_item_ids.len(),
            };
        }
        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return MoveItemsOutcome::StructuralRevisionExhausted;
        };

        // Единственная публикация не касается current, allocator, metadata или shuffle state.
        self.items = retained_items;
        self.structural_revision = next_structural_revision;
        MoveItemsOutcome::Moved {
            item_count: selected_item_ids.len(),
        }
    }
}
