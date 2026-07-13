//! Полный persisted display/sort cache без operation-specific comparator keys.

use std::fmt;
use std::time::SystemTime;

use media_core::{DiscNumber, MediaDuration, TrackNumber, TvEpisodeNumber, TvSeasonNumber};

/// Жёсткая граница списка исполнителей в одном cached metadata snapshot.
pub const MAX_CACHED_ARTISTS: usize = 32;

/// Классифицированный media kind, известный playlist domain.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaylistMediaKind {
    /// Тип ещё не подтверждён probe/open boundary.
    Unknown,
    /// Media без video track, но с audio track.
    Audio,
    /// Media как минимум с одним video track.
    Video,
}

impl fmt::Debug for PlaylistMediaKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => formatter.write_str("PlaylistMediaKind::Unknown"),
            Self::Audio => formatter.write_str("PlaylistMediaKind::Audio"),
            Self::Video => formatter.write_str("PlaylistMediaKind::Video"),
        }
    }
}

impl fmt::Display for PlaylistMediaKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => formatter.write_str("unknown"),
            Self::Audio => formatter.write_str("audio"),
            Self::Video => formatter.write_str("video"),
        }
    }
}

/// Best-effort local cache invalidation fingerprint из size + mtime.
///
/// Это не content hash и не доказательство неизменности содержимого.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LocalSourceFingerprint {
    file_size_bytes: u64,
    modified_at: SystemTime,
}

impl LocalSourceFingerprint {
    /// Создаёт fingerprint из значений, уже полученных I/O boundary.
    pub const fn new(file_size_bytes: u64, modified_at: SystemTime) -> Self {
        Self {
            file_size_bytes,
            modified_at,
        }
    }

    /// Возвращает размер для persistence mapping и source comparison.
    pub const fn file_size_bytes(self) -> u64 {
        self.file_size_bytes
    }

    /// Возвращает exact modification time для persistence mapping.
    pub const fn modified_at(self) -> SystemTime {
        self.modified_at
    }
}

impl fmt::Debug for LocalSourceFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSourceFingerprint")
            .field("file_size_bytes", &self.file_size_bytes)
            .field("modified_at", &self.modified_at)
            .finish()
    }
}

impl fmt::Display for LocalSourceFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} bytes at exact mtime", self.file_size_bytes)
    }
}

/// Persisted display/sort cache одного playlist item.
#[derive(Clone, PartialEq, Eq)]
pub struct CachedPlaylistMetadata {
    fallback_display_name: String,
    media_kind: PlaylistMediaKind,
    duration: Option<MediaDuration>,
    title: Option<String>,
    artists: Vec<String>,
    album: Option<String>,
    disc_number: Option<DiscNumber>,
    track_number: Option<TrackNumber>,
    season_number: Option<TvSeasonNumber>,
    episode_number: Option<TvEpisodeNumber>,
}

impl CachedPlaylistMetadata {
    /// Создаёт минимальный cache, достаточный для fallback display.
    pub fn new(fallback_display_name: impl Into<String>, media_kind: PlaylistMediaKind) -> Self {
        Self {
            fallback_display_name: fallback_display_name.into(),
            media_kind,
            duration: None,
            title: None,
            artists: Vec::new(),
            album: None,
            disc_number: None,
            track_number: None,
            season_number: None,
            episode_number: None,
        }
    }

    /// Устанавливает normalized duration без изменения остальных полей.
    pub fn with_duration(mut self, duration: Option<MediaDuration>) -> Self {
        self.duration = duration;
        self
    }

    /// Устанавливает normalized title без изменения остальных полей.
    pub fn with_title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    /// Устанавливает bounded normalized artists list.
    pub fn with_artists(mut self, artists: Vec<String>) -> Result<Self, CachedMetadataError> {
        if artists.len() > MAX_CACHED_ARTISTS {
            return Err(CachedMetadataError::ArtistsLimitExceeded {
                provided: artists.len(),
                maximum: MAX_CACHED_ARTISTS,
            });
        }

        self.artists = artists;
        Ok(self)
    }

    /// Устанавливает normalized album без изменения остальных полей.
    pub fn with_album(mut self, album: Option<String>) -> Self {
        self.album = album;
        self
    }

    /// Устанавливает все neutral sequence fields одним intent-method.
    pub fn with_sequence(
        mut self,
        disc_number: Option<DiscNumber>,
        track_number: Option<TrackNumber>,
        season_number: Option<TvSeasonNumber>,
        episode_number: Option<TvEpisodeNumber>,
    ) -> Self {
        self.disc_number = disc_number;
        self.track_number = track_number;
        self.season_number = season_number;
        self.episode_number = episode_number;
        self
    }

    /// Возвращает безопасный fallback display name для UI adapter-а.
    pub fn fallback_display_name(&self) -> &str {
        &self.fallback_display_name
    }

    /// Возвращает текущую media classification.
    pub const fn media_kind(&self) -> PlaylistMediaKind {
        self.media_kind
    }

    /// Возвращает normalized duration.
    pub const fn duration(&self) -> Option<MediaDuration> {
        self.duration
    }

    /// Возвращает normalized title.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Возвращает bounded artists slice без копирования.
    pub fn artists(&self) -> &[String] {
        &self.artists
    }

    /// Возвращает normalized album.
    pub fn album(&self) -> Option<&str> {
        self.album.as_deref()
    }

    /// Возвращает neutral disc number.
    pub const fn disc_number(&self) -> Option<DiscNumber> {
        self.disc_number
    }

    /// Возвращает neutral track number.
    pub const fn track_number(&self) -> Option<TrackNumber> {
        self.track_number
    }

    /// Возвращает neutral TV season number.
    pub const fn season_number(&self) -> Option<TvSeasonNumber> {
        self.season_number
    }

    /// Возвращает neutral TV episode number.
    pub const fn episode_number(&self) -> Option<TvEpisodeNumber> {
        self.episode_number
    }
}

impl fmt::Debug for CachedPlaylistMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedPlaylistMetadata")
            .field("fallback_display_name", &"<redacted>")
            .field("media_kind", &self.media_kind)
            .field("duration", &self.duration)
            .field("has_title", &self.title.is_some())
            .field("artist_count", &self.artists.len())
            .field("has_album", &self.album.is_some())
            .field("disc_number", &self.disc_number)
            .field("track_number", &self.track_number)
            .field("season_number", &self.season_number)
            .field("episode_number", &self.episode_number)
            .finish()
    }
}

/// Ошибка построения bounded cached metadata.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CachedMetadataError {
    /// Artists list превышает именованный domain limit.
    ArtistsLimitExceeded {
        /// Фактическое число artists без раскрытия их текста.
        provided: usize,
        /// Максимально допустимое число artists.
        maximum: usize,
    },
}

impl fmt::Debug for CachedMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArtistsLimitExceeded { provided, maximum } => formatter
                .debug_struct("CachedMetadataError::ArtistsLimitExceeded")
                .field("provided", provided)
                .field("maximum", maximum)
                .finish(),
        }
    }
}

impl fmt::Display for CachedMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArtistsLimitExceeded { provided, maximum } => write!(
                formatter,
                "artists list содержит {provided} значений при лимите {maximum}"
            ),
        }
    }
}

impl std::error::Error for CachedMetadataError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_metadata_keeps_neutral_sequence_types() {
        // Cache использует media-core types, а не дублирующие playlist numbers.
        let metadata = CachedPlaylistMetadata::new("Episode", PlaylistMediaKind::Video)
            .with_duration(Some(MediaDuration::from_secs(42)))
            .with_title(Some("Title".to_owned()))
            .with_artists(vec!["Artist".to_owned()])
            .expect("bounded artists")
            .with_album(Some("Album".to_owned()))
            .with_sequence(
                Some(DiscNumber::new(1)),
                Some(TrackNumber::new(2)),
                Some(TvSeasonNumber::new(3)),
                Some(TvEpisodeNumber::new(4)),
            );

        assert_eq!(metadata.duration(), Some(MediaDuration::from_secs(42)));
        assert_eq!(metadata.disc_number().map(DiscNumber::value), Some(1));
        assert_eq!(metadata.track_number().map(TrackNumber::value), Some(2));
        assert_eq!(metadata.season_number().map(TvSeasonNumber::value), Some(3));
        assert_eq!(
            metadata.episode_number().map(TvEpisodeNumber::value),
            Some(4)
        );
    }

    #[test]
    fn artists_limit_rejects_instead_of_silent_truncation() {
        // Domain не должен молча менять metadata, полученную discovery/service.
        let too_many_artists = vec!["artist".to_owned(); MAX_CACHED_ARTISTS + 1];
        let result = CachedPlaylistMetadata::new("Track", PlaylistMediaKind::Audio)
            .with_artists(too_many_artists);

        assert!(matches!(
            result,
            Err(CachedMetadataError::ArtistsLimitExceeded { .. })
        ));
    }
}
