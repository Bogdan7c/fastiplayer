//! Read-only снимок меню аудиодорожек и выбор «следующей» строки для клавиши `B`.
//!
//! Снимок строится чистой функцией из `PlayerSnapshot` (встроенные дорожки текущего media) и,
//! для web-источников, из проекции каталога web-версий звука (`web_versions.rs`). В нём нет ни
//! состояния, ни I/O, поэтому поведение проверяется без окна и без player-а.
//!
//! # Единый список
//! - Нет web-версий — как раньше: только встроенные дорожки.
//! - Есть web-версии (≥ 2) — строки меню и есть версии, активная отмечена. Встроенные дорожки
//!   web-источника в этом случае не показываются: у HLS/DASH rendition-а это звук уже
//!   выбранной версии, а смешанный список имел бы две отметки «играет» (версия + дорожка) и
//!   ломал бы круг клавиши `B`. Редкий случай «в одной версии несколько дорожек» — известное
//!   ограничение (memory `app-egui/web-audio-versions-menu`).
//! - Окно браузерного режима с меню озвучек страницы (14a) — строки и есть озвучки страницы
//!   (`page_dubs.rs`), по тем же причинам; во время рекламы они неактивны.

use media_core::{TrackId, TrackKind};
use media_source_open::web_media_open::audio_languages::WebAudioLanguageAction;
use player_core::PlayerSnapshot;

use super::labels::{AudioTrackNumber, audio_track_label};
use super::page_dubs::PageDubRows;
use super::web_audio_languages::WebAudioLanguageEntry;
use super::web_versions::WebAudioVersionEntry;
use crate::page_variants::{PageVariantAvailability, PageVariantChoice};
use crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction;

/// Сколько строк нужно, чтобы выбор имел смысл (и кнопка «♪» показывалась).
const MIN_TRACKS_TO_SWITCH: usize = 2;

/// Что именно выбирает строка меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioTrackMenuTarget {
    /// Встроенная дорожка текущего media: `PlayerCommand::SelectAudioTrack`.
    EmbeddedTrack(TrackId),
    /// Другая web-версия звука того же видео: same-item component switch.
    WebAudioVersion(ComponentVariantSelectionAction),
    /// Другой язык звука (дубляж) того же ролика: same-item switch на ту же позицию.
    WebAudioLanguage(WebAudioLanguageAction),
    /// Озвучка плеера страницы (окно браузерного режима, 14a): выбор уходит странице.
    PageDub(PageVariantChoice),
}

/// Что web-источник предлагает вместо встроенных дорожек.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WebAudioMenuSource {
    /// Версии звука HLS/DASH (разные кодеки/раскладки).
    Versions(Vec<WebAudioVersionEntry>),
    /// Языки звука yt-dlp-видео (оригинал и дубляжи).
    Languages(Vec<WebAudioLanguageEntry>),
    /// Озвучки плеера страницы (окно браузерного режима).
    PageDubs(PageDubRows),
}

/// Одна строка меню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioTrackMenuItem {
    /// Что выбрать при клике.
    pub(crate) target: AudioTrackMenuTarget,
    /// Готовая подпись: «Russian — Dub (FLAC)», «English (E-AC-3 5.1)».
    pub(crate) label: String,
    /// Эта строка сейчас играет (дорожка по snapshot player-а / установленная web-версия).
    pub(crate) is_selected: bool,
    /// Контейнер пометил дорожку как «по умолчанию» (у web-версий всегда `false`).
    pub(crate) is_default: bool,
}

/// Снимок меню аудиодорожек.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AudioTrackMenuView {
    /// Строки в порядке показа: web-версии либо встроенные дорожки в порядке контейнера.
    tracks: Vec<AudioTrackMenuItem>,
    /// Можно ли сейчас выбирать строки (озвучки страницы во время рекламы — нельзя).
    availability: PageVariantAvailability,
}

impl AudioTrackMenuView {
    /// Строит снимок только из встроенных дорожек snapshot-а player-а (тестовые фикстуры).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_snapshot(snapshot: &PlayerSnapshot) -> Self {
        Self::compose(snapshot, None)
    }

    /// Меню из готовых строк-языков (подпись, играет ли) для тестов виджета «♪».
    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_web_languages_test(rows: &[(WebAudioLanguageAction, &str, bool)]) -> Self {
        Self {
            tracks: rows
                .iter()
                .map(|&(action, label, is_active)| {
                    web_item(
                        AudioTrackMenuTarget::WebAudioLanguage(action),
                        label.to_owned(),
                        is_active,
                    )
                })
                .collect(),
            availability: PageVariantAvailability::Selectable,
        }
    }

    /// Меню озвучек страницы поверх встроенных дорожек `snapshot` (тесты «♪» и `B`).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_page_dubs_test(
        snapshot: &PlayerSnapshot,
        menu: &crate::page_variants::PageVariantMenuView,
        availability: PageVariantAvailability,
    ) -> Self {
        Self::compose(
            snapshot,
            Some(super::page_dubs::page_dub_menu_source(menu, availability)),
        )
    }

    /// Строит единый список: строки web-источника, если они переданы, иначе встроенные дорожки.
    #[must_use]
    pub(super) fn compose(
        snapshot: &PlayerSnapshot,
        web_source: Option<WebAudioMenuSource>,
    ) -> Self {
        let mut availability = PageVariantAvailability::Selectable;
        let tracks = match web_source {
            None => embedded_tracks(snapshot),
            Some(WebAudioMenuSource::PageDubs(rows)) => {
                availability = rows.availability;
                rows.entries
                    .into_iter()
                    .map(|dub| {
                        web_item(
                            AudioTrackMenuTarget::PageDub(dub.choice),
                            dub.label,
                            dub.is_active,
                        )
                    })
                    .collect()
            }
            Some(WebAudioMenuSource::Versions(versions)) => versions
                .into_iter()
                .map(|version| {
                    web_item(
                        AudioTrackMenuTarget::WebAudioVersion(version.action),
                        version.label,
                        version.is_active,
                    )
                })
                .collect(),
            Some(WebAudioMenuSource::Languages(languages)) => languages
                .into_iter()
                .map(|language| {
                    web_item(
                        AudioTrackMenuTarget::WebAudioLanguage(language.action),
                        language.label,
                        language.is_active,
                    )
                })
                .collect(),
        };
        Self {
            tracks,
            availability,
        }
    }

    /// Строки меню.
    #[must_use]
    pub(crate) fn tracks(&self) -> &[AudioTrackMenuItem] {
        &self.tracks
    }

    /// Можно ли сейчас выбирать строки (во время рекламы озвучки страницы неактивны).
    #[must_use]
    pub(crate) const fn availability(&self) -> PageVariantAvailability {
        self.availability
    }

    /// Сколько строк в меню.
    #[must_use]
    pub(crate) fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Есть ли из чего выбирать: кнопка «♪» показывается только тогда.
    #[must_use]
    pub(crate) fn has_choice(&self) -> bool {
        self.tracks.len() >= MIN_TRACKS_TO_SWITCH
    }

    /// Следующая строка по кругу после текущей (клавиша `B`); `None` — выбирать не из чего.
    ///
    /// Текущая — отмеченная (у web-источника это активная версия). Если отметки ещё
    /// нет, считаем текущей дорожку «по умолчанию» (а при её отсутствии — первую): именно её
    /// плеер и играет сам.
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

/// Строка web-источника: «по умолчанию» у неё не бывает, играющая отмечена `is_active`.
fn web_item(target: AudioTrackMenuTarget, label: String, is_active: bool) -> AudioTrackMenuItem {
    AudioTrackMenuItem {
        target,
        label,
        is_selected: is_active,
        is_default: false,
    }
}

/// Встроенные аудиодорожки snapshot-а в порядке контейнера.
fn embedded_tracks(snapshot: &PlayerSnapshot) -> Vec<AudioTrackMenuItem> {
    let selected = snapshot.selected_tracks.audio_track;
    snapshot
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .enumerate()
        .map(|(menu_index, track)| AudioTrackMenuItem {
            target: AudioTrackMenuTarget::EmbeddedTrack(track.id),
            label: audio_track_label(
                &track.labels,
                &track.codec_id,
                AudioTrackNumber::from_menu_index(menu_index),
            ),
            is_selected: selected == Some(track.id),
            is_default: track.labels.disposition.is_default,
        })
        .collect()
}
