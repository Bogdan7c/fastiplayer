//! Checked playback-window arithmetic для imported media items.

use std::fmt;

use media_core::{MediaDuration, MediaTime};

/// Ограниченный playback window внутри одного media source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaylistPlaybackSpan {
    start: MediaTime,
    end_exclusive: Option<MediaTime>,
}

impl PlaylistPlaybackSpan {
    /// Создаёт span и отвергает пустую либо обратную закрытую границу.
    pub fn new(
        start: MediaTime,
        end_exclusive: Option<MediaTime>,
    ) -> Result<Self, PlaylistPlaybackSpanError> {
        if end_exclusive.is_some_and(|end| end <= start) {
            return Err(PlaylistPlaybackSpanError::EndNotAfterStart);
        }

        Ok(Self {
            start,
            end_exclusive,
        })
    }

    /// Создаёт bounded span из start и ненулевой duration через checked addition.
    pub fn from_start_and_duration(
        start: MediaTime,
        duration: MediaDuration,
    ) -> Result<Self, PlaylistPlaybackSpanError> {
        if duration == MediaDuration::ZERO {
            return Err(PlaylistPlaybackSpanError::ZeroDuration);
        }

        let end_exclusive = start
            .as_duration()
            .checked_add(duration.as_duration())
            .map(MediaTime::from_duration)
            .ok_or(PlaylistPlaybackSpanError::EndOverflow)?;

        Self::new(start, Some(end_exclusive))
    }

    /// Возвращает абсолютную start position относительно начала source.
    pub const fn start(self) -> MediaTime {
        self.start
    }

    /// Возвращает exclusive end либо `None` для span до EOF.
    pub const fn end_exclusive(self) -> Option<MediaTime> {
        self.end_exclusive
    }

    /// Возвращает checked duration закрытого span либо `None` для EOF-bound span.
    pub fn duration(self) -> Option<MediaDuration> {
        self.end_exclusive.map(|end| {
            let duration = end
                .as_duration()
                .checked_sub(self.start.as_duration())
                .expect("PlaylistPlaybackSpan invariant requires end > start");
            MediaDuration::from_duration(duration)
        })
    }
}

/// Ошибка построения playback span без публикации частично валидного значения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistPlaybackSpanError {
    /// Exclusive end не находится строго после start.
    EndNotAfterStart,
    /// Duration равна нулю и создала бы пустой span.
    ZeroDuration,
    /// Сложение start и duration не представимо через `MediaTime`.
    EndOverflow,
}

impl fmt::Display for PlaylistPlaybackSpanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndNotAfterStart => {
                formatter.write_str("playback span end must be strictly after start")
            }
            Self::ZeroDuration => formatter.write_str("playback span duration must be non-zero"),
            Self::EndOverflow => formatter.write_str("playback span end exceeds timeline bounds"),
        }
    }
}

impl std::error::Error for PlaylistPlaybackSpanError {}
