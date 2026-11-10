//! Общие помощники тестов дорожек страницы (мост, 13b1): склад, как его видит окно, и
//! документы страницы.
//!
//! Склад сеанса живёт в `browser_session` (туда тестам владельца нельзя), поэтому здесь его
//! тестовый двойник с той же формой снимка: номер склада, поступления, замена на месте.

use std::sync::Arc;

use super::track_selection::settle;
use super::*;
use crate::page_subtitles::{
    PageSubtitleArrival, PageSubtitleDisplay, PageSubtitleDocument, PageSubtitleFormatHint,
    PageSubtitleLabel, PageSubtitleLanguage, PageSubtitlePreference, PageSubtitleShelfId,
    PageSubtitleShelfView, PageSubtitleTrackNumber, PageTracksUpdate, ShelvedPageSubtitle,
};

/// WebVTT: одна реплика с 1 по 4 с.
pub(super) fn vtt_with(text: &str) -> String {
    format!("WEBVTT\n\n00:00:01.000 --> 00:00:04.000\n{text}\n")
}

/// Документ страницы.
pub(super) fn page_document(
    track: u32,
    language: Option<&str>,
    label: Option<&str>,
    display: PageSubtitleDisplay,
    bytes: &[u8],
) -> PageSubtitleDocument {
    PageSubtitleDocument {
        track: PageSubtitleTrackNumber::new(track),
        language: language.map_or(PageSubtitleLanguage::NotNamed, |tag| {
            PageSubtitleLanguage::Tagged(tag.to_owned())
        }),
        label: label.map_or(PageSubtitleLabel::NotGiven, |text| {
            PageSubtitleLabel::Text(text.to_owned())
        }),
        format_hint: PageSubtitleFormatHint::WebVtt,
        display,
        source: crate::page_subtitles::PageSubtitleSource::PageDownload,
        bytes: Arc::from(bytes),
    }
}

/// Тестовый склад сеанса: тот же номер дорожки заменяет документ на месте.
pub(super) struct TestShelf {
    id: PageSubtitleShelfId,
    next_arrival: PageSubtitleArrival,
    entries: Vec<ShelvedPageSubtitle>,
    preference: PageSubtitlePreference,
}

impl TestShelf {
    /// Склад первого сеанса.
    pub(super) fn new() -> Self {
        Self {
            id: PageSubtitleShelfId::FIRST,
            next_arrival: PageSubtitleArrival::FIRST,
            entries: Vec::new(),
            preference: PageSubtitlePreference::NotGiven,
        }
    }

    /// Склад следующего сеанса (другая вкладка в том же окне).
    pub(super) fn of_next_session(&self) -> Self {
        Self {
            id: self.id.next(),
            ..Self::new()
        }
    }

    /// Кладёт документ и возвращает снимок, как его получит окно.
    pub(super) fn store(&mut self, document: PageSubtitleDocument) -> PageSubtitleShelfView {
        let shelved = ShelvedPageSubtitle {
            arrival: self.next_arrival,
            document: Arc::new(document),
        };
        self.next_arrival = self.next_arrival.next();
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.document.track == shelved.document.track)
        {
            Some(previous) => *previous = shelved,
            None => self.entries.push(shelved),
        }
        self.view()
    }

    /// Страница подсказала выбор субтитров (14a); возвращает снимок, как его получит окно.
    pub(super) fn prefer(&mut self, preference: PageSubtitlePreference) -> PageSubtitleShelfView {
        self.preference = preference;
        self.view()
    }

    /// Снимок склада.
    pub(super) fn view(&self) -> PageSubtitleShelfView {
        PageSubtitleShelfView::new(self.id, self.entries.clone()).with_preference(self.preference)
    }
}

/// Владелец браузерного медиа без дорожек (как `ActiveMediaSource::BrowserStream`).
pub(super) fn browser_owner(config: &SubtitlesConfig, media: MediaInstanceId) -> SubtitleOwner {
    let mut owner = owner_with_config(config);
    owner.begin_media(media, SubtitleMediaSource::NotLocal);
    owner
}

/// Проход окна: склад → владелец, затем ожидание фонового разбора.
pub(super) fn offer(owner: &mut SubtitleOwner, shelf: &PageSubtitleShelfView) -> PageTracksUpdate {
    let update = owner.attach_page_tracks(shelf);
    settle(owner);
    update
}

/// Подпись выбранной и показанной строки меню (`None` — субтитры не показываются).
pub(super) fn shown_row(owner: &SubtitleOwner) -> Option<String> {
    let view = owner.menu_view();
    view.tracks
        .iter()
        .enumerate()
        .find(|(_, track)| track.is_selected)
        .map(|(index, track)| {
            crate::subtitle_message::track_text(
                &track.label,
                crate::subtitle_message::TrackNumber::from_menu_index(index),
            )
        })
}

/// Тексты реплик показываемой дорожки на `pts_millis` (пусто — показывать нечего).
pub(super) fn displayed_texts_at(owner: &SubtitleOwner, pts_millis: u64) -> Vec<String> {
    let Some(track) = owner
        .media
        .as_ref()
        .and_then(super::super::media_state::MediaSubtitles::displayed_track)
    else {
        return Vec::new();
    };
    let lookup = track.lookup_time_for(Duration::from_millis(pts_millis));
    let mut ids = Vec::new();
    track.timeline().collect_active_at(lookup, &mut ids);
    ids.into_iter()
        .filter_map(|id| track.timeline().event(id))
        .filter_map(|event| match event.content() {
            subtitle_core::SubtitleContent::Text(cue) => Some(cue.text.plain_text()),
            _ => None,
        })
        .collect()
}
