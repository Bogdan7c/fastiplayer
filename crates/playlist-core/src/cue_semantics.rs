//! Durable CUE export semantics без parser, filesystem или player authority.

use std::fmt;
use std::time::Duration;

use media_core::MediaTime;

/// CUE всегда адресует ровно 75 frames в одной секунде.
pub const PLAYLIST_CUE_FRAMES_PER_SECOND: u64 = 75;

/// Подтверждённый CUE `FILE` type token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistCueFileType {
    /// PCM WAVE source.
    Wave,
    /// AIFF source.
    Aiff,
    /// MPEG audio source.
    Mp3,
    /// FLAC source.
    Flac,
}

/// Fail-closed итог анализа всего исходного CUE document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistCueDocumentExportEligibility {
    /// Все retained команды и индексы поддерживаются exact serializer-ом.
    Exact,
    /// Исходный document содержит семантику, которую нельзя сохранить без потерь.
    Ineligible,
}

/// Exact CUE frame identity, которую нельзя восстанавливать из округлённых наносекунд.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlaylistCueFrameIndex(u64);

impl PlaylistCueFrameIndex {
    /// Создаёт exact checked identity из уже проверенного parser-ом total-frame значения.
    pub const fn new(total_frames: u64) -> Self {
        Self(total_frames)
    }

    /// Возвращает exact total-frame значение для CUE serializer-а.
    pub const fn total_frames(self) -> u64 {
        self.0
    }

    /// Проецирует exact 75-fps позицию в neutral media timeline.
    pub fn media_time(self) -> MediaTime {
        let whole_seconds = self.0 / PLAYLIST_CUE_FRAMES_PER_SECOND;
        let remaining_frames = self.0 % PLAYLIST_CUE_FRAMES_PER_SECOND;
        let subsecond_nanos =
            (remaining_frames * 1_000_000_000 / PLAYLIST_CUE_FRAMES_PER_SECOND) as u32;
        MediaTime::from_duration(Duration::new(whole_seconds, subsecond_nanos))
    }
}

/// Минимальная durable семантика одного imported CUE track для exact export preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaylistCueTrackExportSemantics {
    file_type: PlaylistCueFileType,
    track_number: u8,
    index00: Option<PlaylistCueFrameIndex>,
    index01: PlaylistCueFrameIndex,
    document_eligibility: PlaylistCueDocumentExportEligibility,
}

impl PlaylistCueTrackExportSemantics {
    /// Создаёт validated track semantics без знания queue identity.
    pub fn new(
        file_type: PlaylistCueFileType,
        track_number: u8,
        index00: Option<PlaylistCueFrameIndex>,
        index01: PlaylistCueFrameIndex,
        document_eligibility: PlaylistCueDocumentExportEligibility,
    ) -> Result<Self, PlaylistCueTrackSemanticsError> {
        if !(1..=99).contains(&track_number) {
            return Err(PlaylistCueTrackSemanticsError::InvalidTrackNumber);
        }
        if index00.is_some_and(|pregap| pregap > index01) {
            return Err(PlaylistCueTrackSemanticsError::Index00AfterIndex01);
        }
        Ok(Self {
            file_type,
            track_number,
            index00,
            index01,
            document_eligibility,
        })
    }

    /// Возвращает подтверждённый source type.
    pub const fn file_type(self) -> PlaylistCueFileType {
        self.file_type
    }

    /// Возвращает original CUE track number.
    pub const fn track_number(self) -> u8 {
        self.track_number
    }

    /// Возвращает optional pregap/sub-index перед playback start.
    pub const fn index00(self) -> Option<PlaylistCueFrameIndex> {
        self.index00
    }

    /// Возвращает exact playback start.
    pub const fn index01(self) -> PlaylistCueFrameIndex {
        self.index01
    }

    /// Возвращает fail-closed document-level eligibility.
    pub const fn document_eligibility(self) -> PlaylistCueDocumentExportEligibility {
        self.document_eligibility
    }
}

/// Ошибка построения exact CUE track semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistCueTrackSemanticsError {
    /// CUE track number обязан быть в `01..99`.
    InvalidTrackNumber,
    /// `INDEX 00` не может находиться после `INDEX 01`.
    Index00AfterIndex01,
}

impl fmt::Display for PlaylistCueTrackSemanticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTrackNumber => formatter.write_str("CUE track number must be in 01..99"),
            Self::Index00AfterIndex01 => {
                formatter.write_str("CUE INDEX 00 must not follow INDEX 01")
            }
        }
    }
}

impl std::error::Error for PlaylistCueTrackSemanticsError {}

/// Ошибка присоединения CUE semantics к несовместимому generic import payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistCueSemanticsAttachmentError {
    /// Provenance обязан подтверждать CUE origin.
    NonCueProvenance,
    /// CUE track обязан иметь bounded playback span.
    MissingPlaybackSpan,
    /// Exact `INDEX 01` не совпал с нейтральной start-проекцией span.
    PlaybackStartMismatch,
}

impl fmt::Display for PlaylistCueSemanticsAttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonCueProvenance => {
                formatter.write_str("CUE semantics require CUE import provenance")
            }
            Self::MissingPlaybackSpan => {
                formatter.write_str("CUE semantics require a playback span")
            }
            Self::PlaybackStartMismatch => {
                formatter.write_str("CUE INDEX 01 does not match playback span start")
            }
        }
    }
}

impl std::error::Error for PlaylistCueSemanticsAttachmentError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_semantics_reject_invalid_number_and_reversed_indexes() {
        assert_eq!(
            PlaylistCueTrackExportSemantics::new(
                PlaylistCueFileType::Flac,
                0,
                None,
                PlaylistCueFrameIndex::new(0),
                PlaylistCueDocumentExportEligibility::Exact,
            ),
            Err(PlaylistCueTrackSemanticsError::InvalidTrackNumber)
        );
        assert_eq!(
            PlaylistCueTrackExportSemantics::new(
                PlaylistCueFileType::Flac,
                1,
                Some(PlaylistCueFrameIndex::new(2)),
                PlaylistCueFrameIndex::new(1),
                PlaylistCueDocumentExportEligibility::Exact,
            ),
            Err(PlaylistCueTrackSemanticsError::Index00AfterIndex01)
        );
    }

    #[test]
    fn exact_frame_projection_keeps_authoritative_floor_policy() {
        assert_eq!(
            PlaylistCueFrameIndex::new(76).media_time(),
            MediaTime::from_duration(Duration::new(1, 13_333_333))
        );
    }
}
