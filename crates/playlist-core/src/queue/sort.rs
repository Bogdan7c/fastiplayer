//! Pure one-shot sorting canonical queue по заранее закэшированным metadata.

mod natural;
mod prepared;
#[cfg(test)]
mod tests;

pub use prepared::*;

use std::cmp::Ordering;
use std::fmt;

use media_core::{DiscNumber, MediaDuration, TrackNumber, TvEpisodeNumber, TvSeasonNumber};

use self::natural::{NaturalSortKey, prepare_natural_sort_key};
use super::PlaylistQueue;
use crate::{
    CachedPlaylistMetadata, PlaylistEntry, PlaylistEntryId, PlaylistLocator, PlaylistMediaKind,
};

/// Выбранный пользователем canonical sort vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaylistSortKey {
    /// Case-insensitive natural filename с числовыми runs.
    NaturalFilename,
    /// Нормализованный metadata title.
    Title,
    /// Упорядоченный normalized artists list.
    Artist,
    /// Нормализованный metadata album.
    Album,
    /// Typed media duration.
    Duration,
    /// Media-aware audio/video sequence tuple.
    SmartSequence,
}

/// Направление только выбранного primary sort key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortDirection {
    /// От меньшего primary value к большему.
    Ascending,
    /// От большего primary value к меньшему.
    Descending,
}

impl SortDirection {
    /// Разворачивает только ordering известных primary values.
    fn apply(self, ordering: Ordering) -> Ordering {
        match self {
            Self::Ascending => ordering,
            Self::Descending => ordering.reverse(),
        }
    }
}

/// Явный one-shot intent canonical reorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SortCanonicalQueue {
    key: PlaylistSortKey,
    direction: SortDirection,
}

impl SortCanonicalQueue {
    /// Собирает intent без positional bool или неявного sort mode.
    pub const fn new(key: PlaylistSortKey, direction: SortDirection) -> Self {
        Self { key, direction }
    }

    /// Возвращает выбранный sort key.
    pub const fn key(self) -> PlaylistSortKey {
        self.key
    }

    /// Возвращает выбранное направление.
    pub const fn direction(self) -> SortDirection {
        self.direction
    }
}

/// Typed результат one-shot canonical sort mutation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortCanonicalQueueOutcome {
    /// Canonical order опубликован одной structural revision.
    Reordered {
        /// Число top-level entries в атомарно опубликованном порядке.
        entry_count: usize,
    },
    /// Comparator уже задавал текущий canonical order.
    AlreadyInCanonicalOrder,
    /// D08 reservation удерживает structural mutation lock.
    InstallCommitLinearizing,
    /// Structural revision нельзя продвинуть без нарушения monotonicity.
    StructuralRevisionExhausted,
}

impl fmt::Debug for SortCanonicalQueueOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SortCanonicalQueueOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reordered { entry_count } => {
                write!(
                    formatter,
                    "canonical queue reordered ({entry_count} entries)"
                )
            }
            Self::AlreadyInCanonicalOrder => formatter.write_str("canonical queue already sorted"),
            Self::InstallCommitLinearizing => {
                formatter.write_str("install commit is linearizing canonical queue")
            }
            Self::StructuralRevisionExhausted => {
                formatter.write_str("structural queue revision exhausted")
            }
        }
    }
}

impl PlaylistQueue {
    /// Атомарно применяет pure sort к canonical order по cached metadata.
    pub fn sort_canonical(&mut self, intent: SortCanonicalQueue) -> SortCanonicalQueueOutcome {
        if self.active_reservation.is_some() {
            return SortCanonicalQueueOutcome::InstallCommitLinearizing;
        }

        let mut sortable_entries = self.entries.clone();
        let mut prepared_entries = prepare_sort_entries(&sortable_entries, intent.key, |_| {});
        prepared_entries.sort_by(|left, right| compare_entries(left, right, intent.direction));

        let order_is_unchanged = prepared_entries
            .iter()
            .enumerate()
            .all(|(new_index, entry)| new_index == entry.original_index);
        if order_is_unchanged {
            return SortCanonicalQueueOutcome::AlreadyInCanonicalOrder;
        }

        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return SortCanonicalQueueOutcome::StructuralRevisionExhausted;
        };

        let sorted_original_indices = prepared_entries
            .iter()
            .map(|entry| entry.original_index)
            .collect::<Vec<_>>();
        apply_prepared_order(&mut sortable_entries, &sorted_original_indices);
        self.entries = sortable_entries;
        self.structural_revision = next_structural_revision;

        SortCanonicalQueueOutcome::Reordered {
            entry_count: self.entries.len(),
        }
    }
}

/// Один immutable набор ключей, подготовленный ровно один раз для top-level entry.
struct PreparedSortEntry {
    original_index: usize,
    primary: PreparedPrimaryKey,
    natural_fallback: NaturalSortKey,
}

/// Только выбранный primary key, чтобы operation не готовила ненужные metadata keys.
enum PreparedPrimaryKey {
    NaturalFilename,
    Text(Option<NormalizedTextKey>),
    Artists(Option<Vec<NormalizedTextKey>>),
    Duration(Option<MediaDuration>),
    SmartSequence(Option<SmartSequenceKey>),
}

/// Case-insensitive text key с exact tie-breaker.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct NormalizedTextKey {
    folded: String,
    exact: String,
}

impl NormalizedTextKey {
    /// Case-fold выполняется здесь один раз, а не внутри comparator calls.
    fn new(normalized_text: &str) -> Self {
        Self {
            folded: normalized_text.to_lowercase(),
            exact: normalized_text.to_owned(),
        }
    }
}

/// Media-aware known smart sequence.
enum SmartSequenceKey {
    Audio(AudioSequenceKey),
    Video(VideoSequenceKey),
}

/// Album/disc/track/title tuple для audio item.
struct AudioSequenceKey {
    album: Option<NormalizedTextKey>,
    disc_number: Option<DiscNumber>,
    track_number: Option<TrackNumber>,
    title: Option<NormalizedTextKey>,
}

/// Season/episode/title tuple для video item.
struct VideoSequenceKey {
    season_number: Option<TvSeasonNumber>,
    episode_number: Option<TvEpisodeNumber>,
    title: Option<NormalizedTextKey>,
}

/// O(N) preparation boundary; observer позволяет deterministic proof в tests.
fn prepare_sort_entries(
    entries: &[PlaylistEntry],
    sort_key: PlaylistSortKey,
    mut on_entry_prepared: impl FnMut(PlaylistEntryId),
) -> Vec<PreparedSortEntry> {
    entries
        .iter()
        .enumerate()
        .map(|(original_index, entry)| {
            let (locator, metadata) = entry_sort_source(entry);
            let entry_id = entry.entry_id();
            let natural_fallback =
                prepare_natural_sort_key(locator, metadata.fallback_display_name(), entry_id);
            let primary = prepare_primary_key(metadata, sort_key);
            on_entry_prepared(entry_id);
            PreparedSortEntry {
                original_index,
                primary,
                natural_fallback,
            }
        })
        .collect()
}

/// Выбирает owner-authored summary одного top-level entry без обращения к parts.
fn entry_sort_source(entry: &PlaylistEntry) -> (&PlaylistLocator, &CachedPlaylistMetadata) {
    match entry {
        PlaylistEntry::Single(item) => (item.locator(), item.cached_metadata()),
        PlaylistEntry::Compound(group) => (group.provenance_locator(), group.cached_summary()),
    }
}

/// Строит только выбранный metadata primary key.
fn prepare_primary_key(
    metadata: &CachedPlaylistMetadata,
    sort_key: PlaylistSortKey,
) -> PreparedPrimaryKey {
    match sort_key {
        PlaylistSortKey::NaturalFilename => PreparedPrimaryKey::NaturalFilename,
        PlaylistSortKey::Title => {
            PreparedPrimaryKey::Text(metadata.title().map(NormalizedTextKey::new))
        }
        PlaylistSortKey::Artist => {
            let artists = (!metadata.artists().is_empty()).then(|| {
                metadata
                    .artists()
                    .iter()
                    .map(|artist| NormalizedTextKey::new(artist))
                    .collect()
            });
            PreparedPrimaryKey::Artists(artists)
        }
        PlaylistSortKey::Album => {
            PreparedPrimaryKey::Text(metadata.album().map(NormalizedTextKey::new))
        }
        PlaylistSortKey::Duration => PreparedPrimaryKey::Duration(metadata.duration()),
        PlaylistSortKey::SmartSequence => {
            PreparedPrimaryKey::SmartSequence(prepare_smart_sequence(metadata))
        }
    }
}

/// Классифицирует media kind и готовит соответствующий tuple без probe.
fn prepare_smart_sequence(metadata: &CachedPlaylistMetadata) -> Option<SmartSequenceKey> {
    match metadata.media_kind() {
        PlaylistMediaKind::Audio => Some(SmartSequenceKey::Audio(AudioSequenceKey {
            album: metadata.album().map(NormalizedTextKey::new),
            disc_number: metadata.disc_number(),
            track_number: metadata.track_number(),
            title: metadata.title().map(NormalizedTextKey::new),
        })),
        PlaylistMediaKind::Video => Some(SmartSequenceKey::Video(VideoSequenceKey {
            season_number: metadata.season_number(),
            episode_number: metadata.episode_number(),
            title: metadata.title().map(NormalizedTextKey::new),
        })),
        PlaylistMediaKind::Unknown => None,
    }
}

/// Comparator использует только immutable prepared keys.
fn compare_entries(
    left: &PreparedSortEntry,
    right: &PreparedSortEntry,
    direction: SortDirection,
) -> Ordering {
    let primary_ordering = match (&left.primary, &right.primary) {
        (PreparedPrimaryKey::NaturalFilename, PreparedPrimaryKey::NaturalFilename) => {
            return direction.apply(left.natural_fallback.cmp(&right.natural_fallback));
        }
        (PreparedPrimaryKey::Text(left_key), PreparedPrimaryKey::Text(right_key)) => {
            compare_known_optional(left_key.as_ref(), right_key.as_ref(), direction)
        }
        (PreparedPrimaryKey::Artists(left_key), PreparedPrimaryKey::Artists(right_key)) => {
            compare_known_optional(left_key.as_ref(), right_key.as_ref(), direction)
        }
        (PreparedPrimaryKey::Duration(left_key), PreparedPrimaryKey::Duration(right_key)) => {
            compare_known_optional(left_key.as_ref(), right_key.as_ref(), direction)
        }
        (
            PreparedPrimaryKey::SmartSequence(left_key),
            PreparedPrimaryKey::SmartSequence(right_key),
        ) => compare_optional_smart(left_key.as_ref(), right_key.as_ref(), direction),
        _ => unreachable!("all entries of one operation use the same sort-key variant"),
    };

    primary_ordering.then_with(|| left.natural_fallback.cmp(&right.natural_fallback))
}

/// Missing group всегда ниже known group; direction применяется только к двум known values.
fn compare_known_optional<T: Ord>(
    left: Option<&T>,
    right: Option<&T>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => direction.apply(left.cmp(right)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Сохраняет unknown media в missing group независимо от direction.
fn compare_optional_smart(
    left: Option<&SmartSequenceKey>,
    right: Option<&SmartSequenceKey>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare_smart_sequence(left, right, direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Сравнивает mixed media group и затем type-specific tuple.
fn compare_smart_sequence(
    left: &SmartSequenceKey,
    right: &SmartSequenceKey,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (SmartSequenceKey::Audio(left), SmartSequenceKey::Audio(right)) => {
            compare_audio_sequence(left, right, direction)
        }
        (SmartSequenceKey::Video(left), SmartSequenceKey::Video(right)) => {
            compare_video_sequence(left, right, direction)
        }
        (SmartSequenceKey::Audio(_), SmartSequenceKey::Video(_)) => direction.apply(Ordering::Less),
        (SmartSequenceKey::Video(_), SmartSequenceKey::Audio(_)) => {
            direction.apply(Ordering::Greater)
        }
    }
}

/// Lexicographic audio tuple; отсутствие каждого компонента остаётся после known.
fn compare_audio_sequence(
    left: &AudioSequenceKey,
    right: &AudioSequenceKey,
    direction: SortDirection,
) -> Ordering {
    compare_known_optional(left.album.as_ref(), right.album.as_ref(), direction)
        .then_with(|| {
            compare_known_optional(
                left.disc_number.as_ref(),
                right.disc_number.as_ref(),
                direction,
            )
        })
        .then_with(|| {
            compare_known_optional(
                left.track_number.as_ref(),
                right.track_number.as_ref(),
                direction,
            )
        })
        .then_with(|| compare_known_optional(left.title.as_ref(), right.title.as_ref(), direction))
}

/// Lexicographic video tuple; отсутствие каждого компонента остаётся после known.
fn compare_video_sequence(
    left: &VideoSequenceKey,
    right: &VideoSequenceKey,
    direction: SortDirection,
) -> Ordering {
    compare_known_optional(
        left.season_number.as_ref(),
        right.season_number.as_ref(),
        direction,
    )
    .then_with(|| {
        compare_known_optional(
            left.episode_number.as_ref(),
            right.episode_number.as_ref(),
            direction,
        )
    })
    .then_with(|| compare_known_optional(left.title.as_ref(), right.title.as_ref(), direction))
}

/// Применяет доказанную permutation in-place, не меняя Item IDs или соседнее state.
fn apply_prepared_order<T>(items: &mut [T], sorted_original_indices: &[usize]) {
    let mut original_index_at_position = (0..items.len()).collect::<Vec<_>>();
    let mut position_of_original_index = (0..items.len()).collect::<Vec<_>>();

    for (new_position, desired_original_index) in
        sorted_original_indices.iter().copied().enumerate()
    {
        let current_position = position_of_original_index[desired_original_index];
        if current_position == new_position {
            continue;
        }

        let displaced_original_index = original_index_at_position[new_position];
        items.swap(new_position, current_position);
        original_index_at_position.swap(new_position, current_position);
        position_of_original_index[desired_original_index] = new_position;
        position_of_original_index[displaced_original_index] = current_position;
    }
}
