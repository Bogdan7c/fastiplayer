//! Read-only снимок меню аудиодорожек и выбор «следующей» дорожки для клавиши `B`.
//!
//! Снимок строится чистой функцией из `PlayerSnapshot`: в нём нет ни состояния, ни I/O, поэтому
//! поведение проверяется без окна и без player-а.

use media_core::{TrackId, TrackKind};
use player_core::PlayerSnapshot;

use super::labels::{AudioTrackNumber, audio_track_label};

/// Сколько аудиодорожек нужно, чтобы выбор имел смысл (и кнопка «♪» показывалась).
const MIN_TRACKS_TO_SWITCH: usize = 2;

/// Одна строка меню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioTrackMenuItem {
    /// Идентификатор дорожки внутри текущего media.
    pub(crate) id: TrackId,
    /// Готовая подпись: «Russian — Dub (FLAC)».
    pub(crate) label: String,
    /// Эта дорожка сейчас играет (по snapshot player-а).
    pub(crate) is_selected: bool,
    /// Контейнер пометил дорожку как «по умолчанию».
    pub(crate) is_default: bool,
}

/// Снимок меню аудиодорожек.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AudioTrackMenuView {
    /// Все аудиодорожки текущего media в порядке контейнера.
    tracks: Vec<AudioTrackMenuItem>,
}

impl AudioTrackMenuView {
    /// Строит снимок из snapshot player-а: только аудиодорожки, выбор — по `audio_track`.
    #[must_use]
    pub(crate) fn from_snapshot(snapshot: &PlayerSnapshot) -> Self {
        let selected = snapshot.selected_tracks.audio_track;
        let tracks = snapshot
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .enumerate()
            .map(|(menu_index, track)| AudioTrackMenuItem {
                id: track.id,
                label: audio_track_label(
                    &track.labels,
                    &track.codec_id,
                    AudioTrackNumber::from_menu_index(menu_index),
                ),
                is_selected: selected == Some(track.id),
                is_default: track.labels.disposition.is_default,
            })
            .collect();
        Self { tracks }
    }

    /// Строки меню.
    #[must_use]
    pub(crate) fn tracks(&self) -> &[AudioTrackMenuItem] {
        &self.tracks
    }

    /// Сколько аудиодорожек у media.
    #[must_use]
    pub(crate) fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Есть ли из чего выбирать: кнопка «♪» показывается только тогда.
    #[must_use]
    pub(crate) fn has_choice(&self) -> bool {
        self.tracks.len() >= MIN_TRACKS_TO_SWITCH
    }

    /// Следующая дорожка по кругу после текущей (клавиша `B`); `None` — выбирать не из чего.
    ///
    /// Текущая — выбранная в snapshot. Если выбора ещё нет, считаем текущей дорожку «по
    /// умолчанию» (а при её отсутствии — первую): именно её плеер и играет сам.
    #[must_use]
    pub(crate) fn next_track(&self) -> Option<&AudioTrackMenuItem> {
        if !self.has_choice() {
            return None;
        }
        let current_index = self
            .tracks
            .iter()
            .position(|track| track.is_selected)
            .or_else(|| self.tracks.iter().position(|track| track.is_default))
            .unwrap_or(0);
        self.tracks.get((current_index + 1) % self.tracks.len())
    }
}
