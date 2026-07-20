//! Canonical top-level immutable export snapshot.

use std::collections::HashSet;
use std::fmt;

use playlist_core::{PlaylistEntry, PlaylistEntryId, PlaylistQueue};

/// Caller intent для canonical export scope.
#[derive(Clone, Copy, Debug)]
pub enum PlaylistExportScope<'a> {
    /// Все top-level entries в canonical order.
    Full,
    /// Только explicit top-level identities; caller order не влияет на output.
    Selected(&'a [PlaylistEntryId]),
}

/// Immutable owned snapshot, который можно передать background preflight job.
#[derive(Clone)]
pub struct PlaylistExportSnapshot {
    entries: Box<[PlaylistEntry]>,
    retained_item_count: usize,
}

impl PlaylistExportSnapshot {
    /// Снимает canonical scope без queue mutation и без flat cache внутри queue.
    pub fn capture(
        queue: &PlaylistQueue,
        scope: PlaylistExportScope<'_>,
    ) -> Result<Self, PlaylistExportSnapshotError> {
        let entries = match scope {
            PlaylistExportScope::Full => queue.iter_top_level_entries().cloned().collect(),
            PlaylistExportScope::Selected(selected_entry_ids) => {
                capture_selected_entries(queue, selected_entry_ids)?
            }
        };
        let retained_item_count = entries.iter().map(PlaylistEntry::retained_item_count).sum();
        Ok(Self {
            entries: entries.into_boxed_slice(),
            retained_item_count,
        })
    }

    /// Возвращает top-level entry count выбранного immutable scope.
    pub const fn top_level_entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Возвращает flattened playable count с полным включением compound parts.
    pub const fn retained_item_count(&self) -> usize {
        self.retained_item_count
    }

    /// Internal serializer boundary не раскрывает snapshot storage caller-у.
    pub(super) fn entries(&self) -> &[PlaylistEntry] {
        &self.entries
    }
}

impl fmt::Debug for PlaylistExportSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistExportSnapshot")
            .field("top_level_entry_count", &self.entries.len())
            .field("retained_item_count", &self.retained_item_count)
            .finish()
    }
}

/// Typed scope capture failure без locator/metadata payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistExportSnapshotError {
    /// Selected branch обязан иметь хотя бы одну top-level identity.
    EmptySelection,
    /// Одна structural identity передана больше одного раза.
    DuplicateSelection(PlaylistEntryId),
    /// Structural identity отсутствует в текущем canonical snapshot.
    MissingTopLevelEntry(PlaylistEntryId),
    /// Subordinate part Item ID нельзя экспортировать как отдельный scope.
    CompoundPartIsNotTopLevel(PlaylistEntryId),
}

impl fmt::Display for PlaylistExportSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySelection => formatter.write_str("export selection пуста"),
            Self::DuplicateSelection(entry_id) => {
                write!(formatter, "export selection повторяет {entry_id:?}")
            }
            Self::MissingTopLevelEntry(entry_id) => {
                write!(formatter, "top-level export entry {entry_id:?} отсутствует")
            }
            Self::CompoundPartIsNotTopLevel(entry_id) => {
                write!(formatter, "{entry_id:?} является subordinate compound part")
            }
        }
    }
}

impl std::error::Error for PlaylistExportSnapshotError {}

/// Валидирует selection целиком, затем materialize-ит canonical ordered subset.
fn capture_selected_entries(
    queue: &PlaylistQueue,
    selected_entry_ids: &[PlaylistEntryId],
) -> Result<Vec<PlaylistEntry>, PlaylistExportSnapshotError> {
    if selected_entry_ids.is_empty() {
        return Err(PlaylistExportSnapshotError::EmptySelection);
    }

    let mut unique_selection = HashSet::with_capacity(selected_entry_ids.len());
    for entry_id in selected_entry_ids.iter().copied() {
        if !unique_selection.insert(entry_id) {
            return Err(PlaylistExportSnapshotError::DuplicateSelection(entry_id));
        }
        if queue.top_level_entry(entry_id).is_some() {
            continue;
        }
        if let PlaylistEntryId::Single(item_id) = entry_id
            && queue.item(item_id).is_some()
        {
            return Err(PlaylistExportSnapshotError::CompoundPartIsNotTopLevel(
                entry_id,
            ));
        }
        return Err(PlaylistExportSnapshotError::MissingTopLevelEntry(entry_id));
    }

    Ok(queue
        .iter_top_level_entries()
        .filter(|entry| unique_selection.contains(&entry.entry_id()))
        .cloned()
        .collect())
}
