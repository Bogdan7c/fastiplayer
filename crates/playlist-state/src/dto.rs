use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use media_core::{DiscNumber, MediaDuration, TrackNumber, TvEpisodeNumber, TvSeasonNumber};
use playlist_core::{
    CachedPlaylistMetadata, LocalSourceFingerprint, MAX_CACHED_ARTISTS, MAX_PLAYLIST_ITEMS,
    MAX_SHUFFLE_HISTORY_ENTRIES, NextPlaylistItemId, PlaylistItemDraft, PlaylistItemId,
    PlaylistLocator, PlaylistMediaKind, PlaylistQueue, PlaylistQueueRestore, RepeatMode,
    RestoredPlaylistItem, SecretUrlLocator, ShuffleHistoryCursor, ShuffleQueueRestoreError,
    ShuffleTraversalSnapshot,
};
use serde::{Deserialize, Serialize};

use crate::types::{LoadedPlaylistState, PlaylistStateSnapshot, StateSerializationError};
use crate::{CURRENT_PLAYLIST_STATE_SCHEMA_VERSION, MAX_SUPPORTED_STATE_BYTES};

mod path;
mod v2;

use path::{LocalPathV1Dto, validate_domain_locator};

/// Строковые locator/display fields имеют отдельный предел внутри bounded DTO.
pub(super) const MAX_LOCATOR_TEXT_BYTES: usize = 256 * 1024;
/// Display/sort metadata не должна превращать одну строку в весь file budget.
const MAX_METADATA_TEXT_BYTES: usize = 16 * 1024;

/// Required nullable field: key обязан присутствовать, а JSON `null` валиден.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
struct Nullable<T>(Option<T>);

/// Строгий top-level DTO schema v1.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistStateV1Dto {
    schema_version: u64,
    next_item_id: u64,
    items: Vec<PlaylistItemV1Dto>,
    current_item_id: Nullable<u64>,
    repeat_mode: RepeatModeV1Dto,
    shuffle_enabled: bool,
    shuffle_history: Vec<u64>,
    shuffle_history_cursor: Nullable<u64>,
    shuffle_upcoming: Vec<u64>,
}

/// Opaque owned DTO одной согласованной committed domain revision.
///
/// Тип намеренно не раскрывает поля writer-у: allocator watermark уже снят
/// вместе с canonical items внутри `capture_owned_state` и не может быть
/// заменён либо вычислен отдельно перед записью.
pub(crate) struct OwnedPlaylistStateSnapshot {
    dto: v2::PlaylistStateV2Dto,
}

/// Одна canonical row в exact persisted order.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistItemV1Dto {
    item_id: u64,
    locator: PlaylistLocatorV1Dto,
    local_fingerprint: Nullable<LocalFingerprintV1Dto>,
    cached_metadata: CachedMetadataV1Dto,
}

/// Reopenable URL либо platform-tagged local path.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PlaylistLocatorV1Dto {
    Local { path: LocalPathV1Dto },
    Url { reopenable_url: String },
}

/// Exact size+mtime cache fingerprint; это не content identity.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalFingerprintV1Dto {
    file_size_bytes: u64,
    modified_at_unix_seconds: i64,
    modified_at_subsec_nanos: u32,
}

/// Полный D12 display/sort cache без ephemeral comparator keys.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedMetadataV1Dto {
    fallback_display_name: String,
    media_kind: MediaKindV1Dto,
    duration: Nullable<MediaDurationV1Dto>,
    title: Nullable<String>,
    artists: Vec<String>,
    album: Nullable<String>,
    disc_number: Nullable<u64>,
    track_number: Nullable<u64>,
    season_number: Nullable<u64>,
    episode_number: Nullable<u64>,
}

/// Duration хранится без floating-point потери.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaDurationV1Dto {
    seconds: u64,
    subsec_nanos: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MediaKindV1Dto {
    Unknown,
    Audio,
    Video,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RepeatModeV1Dto {
    StopAtEnd,
    RepeatQueue,
    RepeatOne,
}

/// Internal mapping category; public inspection никогда не включает raw DTO value.
pub(crate) enum DtoLoadError {
    InvalidPayload,
    ResourceLimit,
    DomainValue,
    QueueState,
    ShuffleTraversal,
}

/// Создаёт deterministic pretty JSON из exact domain snapshot.
pub(crate) fn serialize_state(
    snapshot: PlaylistStateSnapshot<'_>,
) -> Result<Vec<u8>, StateSerializationError> {
    let owned_snapshot = capture_owned_state(snapshot)?;
    serialize_owned_state(&owned_snapshot)
}

/// Атомарно на уровне immutable borrow снимает весь persisted domain state.
pub(crate) fn capture_owned_state(
    snapshot: PlaylistStateSnapshot<'_>,
) -> Result<OwnedPlaylistStateSnapshot, StateSerializationError> {
    validate_domain_snapshot(snapshot)?;
    let dto = v2::PlaylistStateV2Dto::from_domain(snapshot)?;
    dto.validate_resource_limits()
        .map_err(|_| StateSerializationError::ResourceLimitExceeded)?;

    Ok(OwnedPlaylistStateSnapshot { dto })
}

/// Выполняет тяжёлую JSON-сериализацию уже owned snapshot на writer thread.
pub(crate) fn serialize_owned_state(
    snapshot: &OwnedPlaylistStateSnapshot,
) -> Result<Vec<u8>, StateSerializationError> {
    let maximum_json_bytes = usize::try_from(MAX_SUPPORTED_STATE_BYTES)
        .map_err(|_| StateSerializationError::SerializedStateTooLarge)?;
    let mut output = LimitedJsonBuffer::new(maximum_json_bytes.saturating_sub(1));
    let serialization_result = {
        let mut serializer = serde_json::Serializer::pretty(&mut output);
        snapshot.dto.serialize(&mut serializer)
    };
    if output.exceeded_limit {
        return Err(StateSerializationError::SerializedStateTooLarge);
    }
    serialization_result.map_err(|_| StateSerializationError::JsonEncodingFailed)?;
    output.bytes.push(b'\n');
    Ok(output.bytes)
}

struct LimitedJsonBuffer {
    bytes: Vec<u8>,
    maximum_bytes: usize,
    exceeded_limit: bool,
}

impl LimitedJsonBuffer {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum_bytes,
            exceeded_limit: false,
        }
    }
}

impl Write for LimitedJsonBuffer {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let Some(next_length) = self.bytes.len().checked_add(input.len()) else {
            self.exceeded_limit = true;
            return Err(io::Error::other("playlist state JSON length overflow"));
        };
        if next_length > self.maximum_bytes {
            self.exceeded_limit = true;
            return Err(io::Error::other("playlist state JSON exceeds file limit"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Парсит уже envelope-classified supported schema bytes.
pub(crate) fn deserialize_supported(
    schema_version: u64,
    inspected_bytes: &[u8],
) -> Result<LoadedPlaylistState, DtoLoadError> {
    match schema_version {
        1 => {
            let dto: PlaylistStateV1Dto = serde_json::from_slice(inspected_bytes)
                .map_err(|_| DtoLoadError::InvalidPayload)?;
            dto.validate_resource_limits()?;
            dto.into_domain()
        }
        CURRENT_PLAYLIST_STATE_SCHEMA_VERSION => v2::deserialize(inspected_bytes),
        _ => Err(DtoLoadError::InvalidPayload),
    }
}

impl PlaylistStateV1Dto {
    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        if self.items.len() > MAX_PLAYLIST_ITEMS
            || self.shuffle_history.len() > MAX_SHUFFLE_HISTORY_ENTRIES
            || self.shuffle_upcoming.len() > MAX_PLAYLIST_ITEMS
        {
            return Err(DtoLoadError::ResourceLimit);
        }

        if !self.shuffle_enabled
            && (!self.shuffle_history.is_empty()
                || self.shuffle_history_cursor.0.is_some()
                || !self.shuffle_upcoming.is_empty())
        {
            return Err(DtoLoadError::DomainValue);
        }

        for item in &self.items {
            item.validate_resource_limits()?;
        }
        Ok(())
    }

    fn into_domain(self) -> Result<LoadedPlaylistState, DtoLoadError> {
        if self.schema_version != 1 {
            return Err(DtoLoadError::DomainValue);
        }

        let restored_items = self
            .items
            .into_iter()
            .map(PlaylistItemV1Dto::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let next_item_id = NextPlaylistItemId::from_persistence_value(self.next_item_id)
            .map_err(|_| DtoLoadError::QueueState)?;
        let current_item_id = self
            .current_item_id
            .0
            .map(PlaylistItemId::from_persistence_value)
            .transpose()
            .map_err(|_| DtoLoadError::QueueState)?;
        let queue_restore =
            PlaylistQueueRestore::new(restored_items, next_item_id, current_item_id);

        let queue = if self.shuffle_enabled {
            let history = persisted_ids(self.shuffle_history)?;
            let upcoming = persisted_ids(self.shuffle_upcoming)?
                .into_iter()
                .map(playlist_core::PlaylistEntryId::Single)
                .collect();
            let history_cursor = self
                .shuffle_history_cursor
                .0
                .map(|index| {
                    usize::try_from(index)
                        .map(ShuffleHistoryCursor::from_index)
                        .map_err(|_| DtoLoadError::ShuffleTraversal)
                })
                .transpose()?;
            let traversal = ShuffleTraversalSnapshot::new(history, history_cursor, upcoming);
            PlaylistQueue::restore_with_shuffle(queue_restore, traversal).map_err(|error| {
                match error {
                    ShuffleQueueRestoreError::Queue(_) => DtoLoadError::QueueState,
                    ShuffleQueueRestoreError::Traversal(_) => DtoLoadError::ShuffleTraversal,
                }
            })?
        } else {
            PlaylistQueue::restore(queue_restore).map_err(|_| DtoLoadError::QueueState)?
        };

        Ok(LoadedPlaylistState::new(queue, self.repeat_mode.into()))
    }
}

impl PlaylistItemV1Dto {
    fn from_domain(item: &playlist_core::PlaylistItem) -> Result<Self, StateSerializationError> {
        Ok(Self {
            item_id: item.item_id().expose_value_for_persistence(),
            locator: PlaylistLocatorV1Dto::from_domain(item.locator())?,
            local_fingerprint: Nullable(
                item.local_fingerprint()
                    .map(LocalFingerprintV1Dto::from_domain)
                    .transpose()?,
            ),
            cached_metadata: CachedMetadataV1Dto::from_domain(item.cached_metadata()),
        })
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        self.locator.validate_resource_limits()?;
        self.cached_metadata.validate_resource_limits()
    }

    fn into_domain(self) -> Result<RestoredPlaylistItem, DtoLoadError> {
        let (item_id, draft) = self.into_draft()?;
        Ok(RestoredPlaylistItem::new(item_id, draft))
    }

    fn into_draft(self) -> Result<(PlaylistItemId, PlaylistItemDraft), DtoLoadError> {
        let item_id = PlaylistItemId::from_persistence_value(self.item_id)
            .map_err(|_| DtoLoadError::QueueState)?;
        let metadata = self.cached_metadata.into_domain()?;
        let fingerprint = self
            .local_fingerprint
            .0
            .map(LocalFingerprintV1Dto::into_domain)
            .transpose()?;

        let draft = match self.locator {
            PlaylistLocatorV1Dto::Local { path } => {
                PlaylistItemDraft::local(path.into_domain()?, fingerprint, metadata)
            }
            PlaylistLocatorV1Dto::Url { reopenable_url } => {
                if fingerprint.is_some() {
                    return Err(DtoLoadError::DomainValue);
                }
                let locator = SecretUrlLocator::from_reopenable_url(reopenable_url)
                    .map_err(|_| DtoLoadError::DomainValue)?;
                PlaylistItemDraft::url(locator, metadata)
            }
        };
        Ok((item_id, draft))
    }
}

impl PlaylistLocatorV1Dto {
    fn from_domain(locator: &PlaylistLocator) -> Result<Self, StateSerializationError> {
        match locator {
            PlaylistLocator::Local(local) => Ok(Self::Local {
                path: LocalPathV1Dto::from_domain(local)?,
            }),
            PlaylistLocator::Url(secret_url) => Ok(Self::Url {
                reopenable_url: secret_url.expose_secret_for_persistence().to_owned(),
            }),
        }
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        match self {
            Self::Local { path } => path.validate_resource_limits(),
            Self::Url { reopenable_url } => validate_text(reopenable_url, MAX_LOCATOR_TEXT_BYTES),
        }
    }

    fn into_domain(self) -> Result<PlaylistLocator, DtoLoadError> {
        match self {
            Self::Local { path } => Ok(PlaylistLocator::Local(path.into_domain()?)),
            Self::Url { reopenable_url } => SecretUrlLocator::from_reopenable_url(reopenable_url)
                .map(PlaylistLocator::Url)
                .map_err(|_| DtoLoadError::DomainValue),
        }
    }
}

impl LocalFingerprintV1Dto {
    fn from_domain(fingerprint: LocalSourceFingerprint) -> Result<Self, StateSerializationError> {
        let (seconds, nanos) = system_time_to_unix_parts(fingerprint.modified_at())?;
        Ok(Self {
            file_size_bytes: fingerprint.file_size_bytes(),
            modified_at_unix_seconds: seconds,
            modified_at_subsec_nanos: nanos,
        })
    }

    fn into_domain(self) -> Result<LocalSourceFingerprint, DtoLoadError> {
        let modified_at = unix_parts_to_system_time(
            self.modified_at_unix_seconds,
            self.modified_at_subsec_nanos,
        )?;
        Ok(LocalSourceFingerprint::new(
            self.file_size_bytes,
            modified_at,
        ))
    }
}

impl CachedMetadataV1Dto {
    fn from_domain(metadata: &CachedPlaylistMetadata) -> Self {
        let duration = metadata.duration().map(|duration| {
            let exact = duration.as_duration();
            MediaDurationV1Dto {
                seconds: exact.as_secs(),
                subsec_nanos: exact.subsec_nanos(),
            }
        });
        Self {
            fallback_display_name: metadata.fallback_display_name().to_owned(),
            media_kind: metadata.media_kind().into(),
            duration: Nullable(duration),
            title: Nullable(metadata.title().map(str::to_owned)),
            artists: metadata.artists().to_vec(),
            album: Nullable(metadata.album().map(str::to_owned)),
            disc_number: Nullable(metadata.disc_number().map(DiscNumber::value)),
            track_number: Nullable(metadata.track_number().map(TrackNumber::value)),
            season_number: Nullable(metadata.season_number().map(TvSeasonNumber::value)),
            episode_number: Nullable(metadata.episode_number().map(TvEpisodeNumber::value)),
        }
    }

    fn validate_resource_limits(&self) -> Result<(), DtoLoadError> {
        validate_text(&self.fallback_display_name, MAX_METADATA_TEXT_BYTES)?;
        validate_optional_text(&self.title.0, MAX_METADATA_TEXT_BYTES)?;
        validate_optional_text(&self.album.0, MAX_METADATA_TEXT_BYTES)?;
        if self.artists.len() > MAX_CACHED_ARTISTS {
            return Err(DtoLoadError::ResourceLimit);
        }
        for artist in &self.artists {
            validate_text(artist, MAX_METADATA_TEXT_BYTES)?;
        }
        Ok(())
    }

    fn into_domain(self) -> Result<CachedPlaylistMetadata, DtoLoadError> {
        let duration = self
            .duration
            .0
            .map(|duration| {
                if duration.subsec_nanos >= 1_000_000_000 {
                    return Err(DtoLoadError::DomainValue);
                }
                Ok(MediaDuration::from_duration(Duration::new(
                    duration.seconds,
                    duration.subsec_nanos,
                )))
            })
            .transpose()?;

        CachedPlaylistMetadata::new(self.fallback_display_name, self.media_kind.into())
            .with_duration(duration)
            .with_title(self.title.0)
            .with_artists(self.artists)
            .map_err(|_| DtoLoadError::ResourceLimit)
            .map(|metadata| {
                metadata.with_album(self.album.0).with_sequence(
                    self.disc_number.0.map(DiscNumber::new),
                    self.track_number.0.map(TrackNumber::new),
                    self.season_number.0.map(TvSeasonNumber::new),
                    self.episode_number.0.map(TvEpisodeNumber::new),
                )
            })
    }
}

fn persisted_ids(values: Vec<u64>) -> Result<Vec<PlaylistItemId>, DtoLoadError> {
    values
        .into_iter()
        .map(|value| {
            PlaylistItemId::from_persistence_value(value)
                .map_err(|_| DtoLoadError::ShuffleTraversal)
        })
        .collect()
}

fn validate_text(value: &str, maximum_bytes: usize) -> Result<(), DtoLoadError> {
    if value.len() > maximum_bytes {
        return Err(DtoLoadError::ResourceLimit);
    }
    Ok(())
}

fn validate_optional_text(
    value: &Option<String>,
    maximum_bytes: usize,
) -> Result<(), DtoLoadError> {
    if let Some(value) = value {
        validate_text(value, maximum_bytes)?;
    }
    Ok(())
}

/// Проверяет borrowed domain values до clone/materialization private DTO.
fn validate_domain_snapshot(
    snapshot: PlaylistStateSnapshot<'_>,
) -> Result<(), StateSerializationError> {
    let queue = snapshot.queue();
    if queue.retained_item_count() > MAX_PLAYLIST_ITEMS {
        return Err(StateSerializationError::ResourceLimitExceeded);
    }

    let mut budget = SerializationResourceBudget::new();
    budget.add(queue.retained_item_count().saturating_mul(512))?;
    if let Some(shuffle) = queue.shuffle_traversal_snapshot() {
        if shuffle.history().len() > MAX_SHUFFLE_HISTORY_ENTRIES
            || shuffle.upcoming().len() > MAX_PLAYLIST_ITEMS
        {
            return Err(StateSerializationError::ResourceLimitExceeded);
        }
        budget.add(
            shuffle
                .history()
                .len()
                .saturating_add(shuffle.upcoming().len())
                .saturating_mul(std::mem::size_of::<u64>()),
        )?;
    }

    for item in queue.iter_playable_items() {
        validate_domain_locator(item.locator(), &mut budget)?;
        let metadata = item.cached_metadata();
        validate_domain_metadata_text(metadata.fallback_display_name(), &mut budget)?;
        validate_optional_domain_metadata_text(metadata.title(), &mut budget)?;
        validate_optional_domain_metadata_text(metadata.album(), &mut budget)?;
        if metadata.artists().len() > MAX_CACHED_ARTISTS {
            return Err(StateSerializationError::ResourceLimitExceeded);
        }
        for artist in metadata.artists() {
            validate_domain_metadata_text(artist, &mut budget)?;
        }
    }
    Ok(())
}

fn validate_domain_metadata_text(
    value: &str,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    validate_domain_text(value, MAX_METADATA_TEXT_BYTES, budget)
}

fn validate_optional_domain_metadata_text(
    value: Option<&str>,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    if let Some(value) = value {
        validate_domain_metadata_text(value, budget)?;
    }
    Ok(())
}

fn validate_domain_text(
    value: &str,
    maximum_bytes: usize,
    budget: &mut SerializationResourceBudget,
) -> Result<(), StateSerializationError> {
    if value.len() > maximum_bytes {
        return Err(StateSerializationError::ResourceLimitExceeded);
    }
    budget.add(value.len())
}

pub(super) struct SerializationResourceBudget {
    accounted_bytes: usize,
    maximum_bytes: usize,
}

impl SerializationResourceBudget {
    fn new() -> Self {
        Self {
            accounted_bytes: 0,
            maximum_bytes: MAX_SUPPORTED_STATE_BYTES as usize,
        }
    }

    pub(super) fn add(&mut self, additional_bytes: usize) -> Result<(), StateSerializationError> {
        self.accounted_bytes = self
            .accounted_bytes
            .checked_add(additional_bytes)
            .ok_or(StateSerializationError::ResourceLimitExceeded)?;
        if self.accounted_bytes > self.maximum_bytes {
            return Err(StateSerializationError::ResourceLimitExceeded);
        }
        Ok(())
    }
}

fn system_time_to_unix_parts(time: SystemTime) -> Result<(i64, u32), StateSerializationError> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let seconds = i64::try_from(duration.as_secs())
                .map_err(|_| StateSerializationError::TimeOutOfRange)?;
            Ok((seconds, duration.subsec_nanos()))
        }
        Err(before_epoch) => {
            let duration = before_epoch.duration();
            let seconds = duration.as_secs();
            let nanos = duration.subsec_nanos();
            if nanos == 0 {
                let seconds = i64::try_from(seconds)
                    .ok()
                    .and_then(i64::checked_neg)
                    .ok_or(StateSerializationError::TimeOutOfRange)?;
                Ok((seconds, 0))
            } else {
                let seconds = i64::try_from(seconds)
                    .ok()
                    .and_then(|seconds| seconds.checked_add(1))
                    .and_then(i64::checked_neg)
                    .ok_or(StateSerializationError::TimeOutOfRange)?;
                Ok((seconds, 1_000_000_000 - nanos))
            }
        }
    }
}

fn unix_parts_to_system_time(seconds: i64, nanos: u32) -> Result<SystemTime, DtoLoadError> {
    if nanos >= 1_000_000_000 {
        return Err(DtoLoadError::DomainValue);
    }
    if seconds >= 0 {
        return UNIX_EPOCH
            .checked_add(Duration::new(seconds as u64, nanos))
            .ok_or(DtoLoadError::DomainValue);
    }

    let seconds_before = seconds.unsigned_abs();
    let duration_before = if nanos == 0 {
        Duration::new(seconds_before, 0)
    } else {
        Duration::new(seconds_before - 1, 1_000_000_000 - nanos)
    };
    UNIX_EPOCH
        .checked_sub(duration_before)
        .ok_or(DtoLoadError::DomainValue)
}

impl From<RepeatMode> for RepeatModeV1Dto {
    fn from(value: RepeatMode) -> Self {
        match value {
            RepeatMode::StopAtEnd => Self::StopAtEnd,
            RepeatMode::RepeatQueue => Self::RepeatQueue,
            RepeatMode::RepeatOne => Self::RepeatOne,
        }
    }
}

impl From<RepeatModeV1Dto> for RepeatMode {
    fn from(value: RepeatModeV1Dto) -> Self {
        match value {
            RepeatModeV1Dto::StopAtEnd => Self::StopAtEnd,
            RepeatModeV1Dto::RepeatQueue => Self::RepeatQueue,
            RepeatModeV1Dto::RepeatOne => Self::RepeatOne,
        }
    }
}

impl From<PlaylistMediaKind> for MediaKindV1Dto {
    fn from(value: PlaylistMediaKind) -> Self {
        match value {
            PlaylistMediaKind::Unknown => Self::Unknown,
            PlaylistMediaKind::Audio => Self::Audio,
            PlaylistMediaKind::Video => Self::Video,
        }
    }
}

impl From<MediaKindV1Dto> for PlaylistMediaKind {
    fn from(value: MediaKindV1Dto) -> Self {
        match value {
            MediaKindV1Dto::Unknown => Self::Unknown,
            MediaKindV1Dto::Audio => Self::Audio,
            MediaKindV1Dto::Video => Self::Video,
        }
    }
}
