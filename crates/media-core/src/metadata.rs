//! Нейтральный snapshot статических и обновляемых metadata медиа.

/// Описание контейнера без зависимости от конкретного demux backend-а.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaContainerMetadata {
    /// Короткое user-facing имя формата, когда backend может назвать его точно.
    pub format_name: Option<String>,
}

/// Номер диска внутри многодискового издания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiscNumber(u64);

impl DiscNumber {
    /// Создаёт номер без доменной валидации, сохраняя точное значение demux backend-а.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает исходное числовое значение metadata-тега.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Номер трека внутри диска или самостоятельного издания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackNumber(u64);

impl TrackNumber {
    /// Создаёт номер без доменной валидации, сохраняя точное значение demux backend-а.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает исходное числовое значение metadata-тега.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Номер телевизионного сезона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TvSeasonNumber(u64);

impl TvSeasonNumber {
    /// Создаёт номер без доменной валидации, сохраняя точное значение demux backend-а.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает исходное числовое значение metadata-тега.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Номер телевизионного эпизода внутри сезона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TvEpisodeNumber(u64);

impl TvEpisodeNumber {
    /// Создаёт номер без доменной валидации, сохраняя точное значение demux backend-а.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает исходное числовое значение metadata-тега.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Нормализованные нейтральные теги, не зависящие от playlist, UI или demux backend-а.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaTagMetadata {
    /// Название трека или фильма, если оно известно.
    pub title: Option<String>,

    /// Исполнители в порядке, переданном metadata backend-ом.
    pub artists: Vec<String>,

    /// Название альбома или издания, если оно известно.
    pub album: Option<String>,

    /// Номер диска внутри многодискового издания.
    pub disc_number: Option<DiscNumber>,

    /// Номер трека внутри диска или издания.
    pub track_number: Option<TrackNumber>,

    /// Номер телевизионного сезона.
    pub tv_season_number: Option<TvSeasonNumber>,

    /// Номер телевизионного эпизода внутри сезона.
    pub tv_episode_number: Option<TvEpisodeNumber>,
}

impl MediaTagMetadata {
    /// Upsert не стирает уже известные значения отсутствующими полями revision-а.
    pub fn upsert(&mut self, revision: Self) {
        if revision.title.is_some() {
            self.title = revision.title;
        }
        if !revision.artists.is_empty() {
            self.artists = revision.artists;
        }
        if revision.album.is_some() {
            self.album = revision.album;
        }
        if revision.disc_number.is_some() {
            self.disc_number = revision.disc_number;
        }
        if revision.track_number.is_some() {
            self.track_number = revision.track_number;
        }
        if revision.tv_season_number.is_some() {
            self.tv_season_number = revision.tv_season_number;
        }
        if revision.tv_episode_number.is_some() {
            self.tv_episode_number = revision.tv_episode_number;
        }
    }

    /// Заполняет только неизвестные значения из менее приоритетного metadata snapshot-а.
    pub fn fill_missing_from(&mut self, fallback: Self) {
        if self.title.is_none() {
            self.title = fallback.title;
        }
        if self.artists.is_empty() {
            self.artists = fallback.artists;
        }
        if self.album.is_none() {
            self.album = fallback.album;
        }
        if self.disc_number.is_none() {
            self.disc_number = fallback.disc_number;
        }
        if self.track_number.is_none() {
            self.track_number = fallback.track_number;
        }
        if self.tv_season_number.is_none() {
            self.tv_season_number = fallback.tv_season_number;
        }
        if self.tv_episode_number.is_none() {
            self.tv_episode_number = fallback.tv_episode_number;
        }
    }
}

/// Полный immutable snapshot metadata на текущий момент чтения.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaMetadata {
    pub container: Option<MediaContainerMetadata>,
    pub tags: MediaTagMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_revision_is_upsert_instead_of_replacement() {
        let mut tags = MediaTagMetadata {
            title: Some("Title".into()),
            artists: vec!["Artist".into()],
            disc_number: Some(DiscNumber::new(2)),
            track_number: Some(TrackNumber::new(8)),
            ..Default::default()
        };
        tags.upsert(MediaTagMetadata {
            album: Some("Album".into()),
            tv_season_number: Some(TvSeasonNumber::new(3)),
            tv_episode_number: Some(TvEpisodeNumber::new(11)),
            ..Default::default()
        });
        assert_eq!(tags.title.as_deref(), Some("Title"));
        assert_eq!(tags.artists, ["Artist"]);
        assert_eq!(tags.album.as_deref(), Some("Album"));
        assert_eq!(tags.disc_number, Some(DiscNumber::new(2)));
        assert_eq!(tags.track_number, Some(TrackNumber::new(8)));
        assert_eq!(tags.tv_season_number, Some(TvSeasonNumber::new(3)));
        assert_eq!(tags.tv_episode_number, Some(TvEpisodeNumber::new(11)));
    }

    #[test]
    fn explicit_sequence_values_replace_known_values_and_repeat_idempotently() {
        let mut tags = MediaTagMetadata {
            disc_number: Some(DiscNumber::new(1)),
            track_number: Some(TrackNumber::new(4)),
            tv_season_number: Some(TvSeasonNumber::new(2)),
            tv_episode_number: Some(TvEpisodeNumber::new(7)),
            ..Default::default()
        };
        let revision = MediaTagMetadata {
            disc_number: Some(DiscNumber::new(3)),
            track_number: Some(TrackNumber::new(9)),
            tv_season_number: Some(TvSeasonNumber::new(5)),
            tv_episode_number: Some(TvEpisodeNumber::new(12)),
            ..Default::default()
        };

        tags.upsert(revision.clone());
        let after_first_upsert = tags.clone();
        tags.upsert(revision);

        assert_eq!(tags, after_first_upsert);
        assert_eq!(tags.disc_number.map(DiscNumber::value), Some(3));
        assert_eq!(tags.track_number.map(TrackNumber::value), Some(9));
        assert_eq!(tags.tv_season_number.map(TvSeasonNumber::value), Some(5));
        assert_eq!(tags.tv_episode_number.map(TvEpisodeNumber::value), Some(12));
    }

    #[test]
    fn fallback_fills_only_missing_sequence_values() {
        let mut primary = MediaTagMetadata {
            title: Some("Video title".into()),
            disc_number: Some(DiscNumber::new(1)),
            tv_season_number: Some(TvSeasonNumber::new(4)),
            ..Default::default()
        };

        primary.fill_missing_from(MediaTagMetadata {
            title: Some("Audio title".into()),
            track_number: Some(TrackNumber::new(6)),
            tv_season_number: Some(TvSeasonNumber::new(99)),
            tv_episode_number: Some(TvEpisodeNumber::new(10)),
            ..Default::default()
        });

        assert_eq!(primary.title.as_deref(), Some("Video title"));
        assert_eq!(primary.disc_number, Some(DiscNumber::new(1)));
        assert_eq!(primary.track_number, Some(TrackNumber::new(6)));
        assert_eq!(primary.tv_season_number, Some(TvSeasonNumber::new(4)));
        assert_eq!(primary.tv_episode_number, Some(TvEpisodeNumber::new(10)));
    }
}
