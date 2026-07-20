use std::fmt;
use std::time::Duration;

use media_core::{MediaDuration, MediaTime};

/// Нейтральное ограничение playback внутри одного уже открытого media source.
///
/// Границы всегда заданы в абсолютной timeline demuxer-а. Внешний player API
/// показывает позицию относительно `start`, поэтому `start` становится публичным
/// нулём, а `end_exclusive` — публичной длительностью ограниченного фрагмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediaPlaybackWindow {
    /// Абсолютное начало фрагмента в timeline исходного media.
    start: MediaTime,

    /// Абсолютный exclusive end либо физический EOF source-а.
    end_exclusive: Option<MediaTime>,
}

impl MediaPlaybackWindow {
    /// Создаёт непустое playback window с проверенной направленностью границ.
    pub fn new(
        start: MediaTime,
        end_exclusive: Option<MediaTime>,
    ) -> Result<Self, MediaPlaybackWindowError> {
        if end_exclusive.is_some_and(|end| end <= start) {
            return Err(MediaPlaybackWindowError::EndNotAfterStart);
        }

        Ok(Self {
            start,
            end_exclusive,
        })
    }

    /// Возвращает абсолютное начало в timeline demuxer-а.
    #[must_use]
    pub const fn start(self) -> MediaTime {
        self.start
    }

    /// Возвращает абсолютный exclusive end либо `None` для окна до EOF.
    #[must_use]
    pub const fn end_exclusive(self) -> Option<MediaTime> {
        self.end_exclusive
    }

    /// Проверяет границы окна против фактической длительности source-а.
    pub(crate) fn validate_source_duration(
        self,
        source_duration: Option<Duration>,
    ) -> Result<(), MediaPlaybackWindowSourceError> {
        let Some(source_duration) = source_duration else {
            return Ok(());
        };
        let source_end = MediaTime::from_duration(source_duration);
        if self.start >= source_end {
            return Err(MediaPlaybackWindowSourceError::StartOutsideSource);
        }
        if self.end_exclusive.is_some_and(|end| end > source_end) {
            return Err(MediaPlaybackWindowSourceError::EndOutsideSource);
        }
        Ok(())
    }

    /// Вычисляет публичную длительность с учётом bounded end или физического EOF.
    #[must_use]
    pub(crate) fn relative_duration(
        self,
        source_duration: Option<Duration>,
    ) -> Option<MediaDuration> {
        let absolute_end = self
            .end_exclusive
            .map(MediaTime::as_duration)
            .or(source_duration)?;
        absolute_end
            .checked_sub(self.start.as_duration())
            .map(MediaDuration::from_duration)
    }

    /// Переводит публичную relative position в absolute demux position.
    ///
    /// Значение за известным концом насыщается на end: ordinary player Seek
    /// сохраняет прежнюю clamp-семантику. Строгий MPRIS boundary проверяет range
    /// до вызова этого метода и потому продолжает возвращать typed rejection.
    #[must_use]
    pub(crate) fn absolute_position(
        self,
        relative_position: MediaTime,
        source_duration: Option<Duration>,
    ) -> MediaTime {
        let relative_duration = self.relative_duration(source_duration);
        let clamped_relative = relative_duration.map_or(relative_position, |duration| {
            MediaTime::from_duration(relative_position.as_duration().min(duration.as_duration()))
        });
        let absolute = self
            .start
            .as_duration()
            .checked_add(clamped_relative.as_duration())
            .unwrap_or(Duration::MAX);
        MediaTime::from_duration(absolute)
    }

    /// Переводит absolute clock/frame position в bounded public relative position.
    #[must_use]
    pub(crate) fn relative_position(
        self,
        absolute_position: MediaTime,
        source_duration: Option<Duration>,
    ) -> MediaTime {
        let relative = absolute_position
            .as_duration()
            .saturating_sub(self.start.as_duration());
        let clamped = self
            .relative_duration(source_duration)
            .map_or(relative, |duration| relative.min(duration.as_duration()));
        MediaTime::from_duration(clamped)
    }

    /// Сообщает, начинается ли packet до exclusive end текущего окна.
    #[must_use]
    pub(crate) fn admits_packet_at(self, absolute_pts: Duration) -> bool {
        self.end_exclusive
            .is_none_or(|end| absolute_pts < end.as_duration())
    }

    /// Сообщает, находится ли decoded/presented frame не раньше начала окна.
    #[must_use]
    pub(crate) fn admits_frame_at(self, absolute_pts: Duration) -> bool {
        absolute_pts >= self.start.as_duration() && self.admits_packet_at(absolute_pts)
    }
}

/// Ошибка построения window без знания конкретного media source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPlaybackWindowError {
    /// Закрытая граница не образует положительную длительность.
    EndNotAfterStart,
}

impl fmt::Display for MediaPlaybackWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndNotAfterStart => {
                formatter.write_str("playback window end должен быть позже start")
            }
        }
    }
}

impl std::error::Error for MediaPlaybackWindowError {}

/// Ошибка сопоставления корректного window с конкретным source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPlaybackWindowSourceError {
    /// Начало находится на EOF или позже известной длительности.
    StartOutsideSource,

    /// Explicit end находится позже известной длительности.
    EndOutsideSource,
}

impl fmt::Display for MediaPlaybackWindowSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartOutsideSource => {
                formatter.write_str("playback window start находится вне media source")
            }
            Self::EndOutsideSource => {
                formatter.write_str("playback window end находится вне media source")
            }
        }
    }
}

impl std::error::Error for MediaPlaybackWindowSourceError {}

/// Session-owned progress до synthetic EOF ограниченного playback window.
#[derive(Debug, Default)]
pub(crate) struct PlaybackWindowEndState {
    /// Первый packet выбранного audio track на/после end уже наблюдался.
    audio_end_seen: bool,

    /// Первый packet выбранного video track на/после end уже наблюдался.
    video_end_seen: bool,
}

impl PlaybackWindowEndState {
    /// Сбрасывает наблюдения при media install или seek внутри окна.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Фиксирует пересечение end выбранным track kind.
    pub(crate) fn note_selected_track_end(&mut self, kind: media_core::TrackKind) {
        match kind {
            media_core::TrackKind::Audio => self.audio_end_seen = true,
            media_core::TrackKind::Video => self.video_end_seen = true,
        }
    }

    /// Проверяет, пересекли ли end все реально выбранные playback tracks.
    pub(crate) const fn all_selected_tracks_ended(
        &self,
        has_selected_audio: bool,
        has_selected_video: bool,
    ) -> bool {
        (!has_selected_audio || self.audio_end_seen)
            && (!has_selected_video || self.video_end_seen)
            && (has_selected_audio || has_selected_video)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_or_reversed_window() {
        let start = MediaTime::from_secs(10);
        assert_eq!(
            MediaPlaybackWindow::new(start, Some(start)),
            Err(MediaPlaybackWindowError::EndNotAfterStart)
        );
        assert_eq!(
            MediaPlaybackWindow::new(start, Some(MediaTime::from_secs(9))),
            Err(MediaPlaybackWindowError::EndNotAfterStart)
        );
    }

    #[test]
    fn maps_absolute_and_relative_positions_with_end_clamp() {
        let window =
            MediaPlaybackWindow::new(MediaTime::from_secs(10), Some(MediaTime::from_secs(25)))
                .expect("window valid");
        assert_eq!(
            window.absolute_position(MediaTime::from_secs(4), Some(Duration::from_secs(60))),
            MediaTime::from_secs(14)
        );
        assert_eq!(
            window.absolute_position(MediaTime::from_secs(99), Some(Duration::from_secs(60))),
            MediaTime::from_secs(25)
        );
        assert_eq!(
            window.relative_position(MediaTime::from_secs(7), Some(Duration::from_secs(60))),
            MediaTime::ZERO
        );
        assert_eq!(
            window.relative_position(MediaTime::from_secs(40), Some(Duration::from_secs(60))),
            MediaTime::from_secs(15)
        );
    }

    #[test]
    fn open_end_uses_physical_source_duration() {
        let window =
            MediaPlaybackWindow::new(MediaTime::from_secs(40), None).expect("window valid");
        assert_eq!(
            window.relative_duration(Some(Duration::from_secs(55))),
            Some(MediaDuration::from_secs(15))
        );
        assert_eq!(window.relative_duration(None), None);
    }
}
