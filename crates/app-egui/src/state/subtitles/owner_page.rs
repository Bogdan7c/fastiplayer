//! Дорожки страницы браузера в владельце субтитров (мост, сессия 13b1, решения 270–274).
//!
//! # Что здесь
//! - [`MediaPageTracks`] — какие документы склада уже стали дорожками ЭТОГО медиа. Живёт в
//!   `MediaSubtitles` и умирает с ним: новое медиа (новое поколение потока) начинает с пустого
//!   учёта и получает весь склад заново на ближайшем проходе окна.
//! - [`SubtitleOwner::attach_page_tracks`] — intent «вот склад сеанса»: новые документы
//!   становятся дорожками в конце списка, замена того же номера обновляет дорожку на месте
//!   (идентификатор и выбор прежние, таймлайн разбирается заново).
//! - выбор: дорожка, которую страница показывала (`showing`), выбирается и включается, пока
//!   зритель в этом СЕАНСЕ сам не выбирал дорожку и не включал/выключал субтитры (зритель
//!   главнее; новый `showing` его не перебивает, и смена поколения его не сбрасывает —
//!   `page_viewer_choice`). Последний по времени `showing` побеждает. Иначе действует обычный
//!   автовыбор по языку (`TrackOrigin::Page`).
//! - реклама: пока на экране плашка рекламы, слой субтитров не рисуется
//!   ([`SubtitleOwner::set_page_ad_presence`]); после неё виден снова без перезагрузки.
//! - доказательства в журнале: строка готовности дорожки и счётчик кадров со слоем субтитров
//!   для сводки браузерного окна.
//!
//! # Инварианты
//! - склад чужого сеанса к медиа, у которого уже есть дорожки другого склада, не цепляется:
//!   это медиа старого сеанса, новое медиа нового сеанса придёт следом и получит свой склад;
//! - глобальное «субтитры включены» (`default_visibility`, конфиг) страница не меняет: только
//!   видимость текущего медиа.

use std::sync::Arc;

use subtitle_core::SubtitleTrackId;
use subtitle_raster::SubtitleFontLibrary;
use tracing::{info, warn};

use super::active_track::ActiveSubtitleTrack;
use super::intents::SubtitleVisibility;
use super::page_load_job::{ParsedPageTrack, format_log_label};
use super::page_track::{PageTrackDescription, distinguish_page_track_names};
use super::page_viewer_choice::{PageTrackPreference, ViewerTrackChoice, picked_track};
use super::sidecar_job::SubtitleLoadFailure;
use super::track_list::TrackLoadState;
use super::{JobResultDisposition, SubtitleOwner};
use crate::page_subtitles::{
    PAGE_SUBTITLE_TRACK_FAILED_LINE, PAGE_SUBTITLE_TRACK_READY_LINE, PageSubtitleArrival,
    PageSubtitleShelfId, PageSubtitleShelfView, PageSubtitleTrackNumber, PageTracksUpdate,
    ShelvedPageSubtitle,
};

/// Есть ли на экране реклама страницы (решение 274: во время рекламы субтитры не рисуются).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum PageAdPresence {
    /// Рекламы нет: субтитры рисуются как обычно.
    #[default]
    NoAd,
    /// Плашка рекламы на экране: слой субтитров не рисуется.
    AdOnScreen,
}

/// Сколько кадров ушло в рендер со слоем субтитров (для сводки браузерного окна).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ShownOverlayFrames(u64);

impl ShownOverlayFrames {
    /// Ещё один кадр со слоем.
    pub(super) fn note_frame(&mut self) {
        self.0 = self.0.saturating_add(1);
    }
}

/// Документ склада, ставший дорожкой медиа.
#[derive(Debug, Clone, Copy)]
struct AttachedPageTrack {
    /// Номер дорожки страницы.
    number: PageSubtitleTrackNumber,
    /// Какое поступление склада сейчас в дорожке.
    arrival: PageSubtitleArrival,
    /// Дорожка списка.
    track_id: SubtitleTrackId,
}

/// Дорожки страницы одного медиа.
#[derive(Debug, Default)]
pub(super) struct MediaPageTracks {
    /// Чей склад уже приложен (`None` — ещё ни одного документа).
    shelf: Option<PageSubtitleShelfId>,
    /// Приложенные документы.
    attached: Vec<AttachedPageTrack>,
    /// Дорожка, которую страница показывала последней (`showing`).
    shown_on_page: Option<SubtitleTrackId>,
}

impl MediaPageTracks {
    /// Дорожка страницы, которую выбирает воля `preference` (зрителя сеанса или страницы);
    /// автовыбор её не перебивает. `None` — решает автовыбор.
    pub(super) fn picked_track(&self, preference: PageTrackPreference) -> Option<SubtitleTrackId> {
        picked_track(preference, self.shown_on_page, |number| {
            self.track_of(number)
        })
    }

    /// Номер страницы у дорожки списка (`None` — это не дорожка страницы этого медиа).
    fn number_of(&self, track_id: SubtitleTrackId) -> Option<PageSubtitleTrackNumber> {
        self.attached
            .iter()
            .find(|attached| attached.track_id == track_id)
            .map(|attached| attached.number)
    }

    /// Можно ли приложить склад `shelf` к этому медиа (см. инварианты модуля).
    fn accepts_shelf(&self, shelf: PageSubtitleShelfId) -> bool {
        self.shelf.is_none_or(|attached| attached == shelf)
    }

    /// Документ склада уже в дорожке именно в этом виде.
    fn holds(&self, entry: &ShelvedPageSubtitle) -> bool {
        self.attached.iter().any(|attached| {
            attached.number == entry.document.track && attached.arrival == entry.arrival
        })
    }

    /// Дорожка списка для номера страницы.
    fn track_of(&self, number: PageSubtitleTrackNumber) -> Option<SubtitleTrackId> {
        self.attached
            .iter()
            .find(|attached| attached.number == number)
            .map(|attached| attached.track_id)
    }

    /// Запоминает, что номер `number` теперь в дорожке `track_id` в виде `arrival`.
    fn record(
        &mut self,
        number: PageSubtitleTrackNumber,
        arrival: PageSubtitleArrival,
        track_id: SubtitleTrackId,
    ) {
        match self
            .attached
            .iter_mut()
            .find(|attached| attached.number == number)
        {
            Some(attached) => attached.arrival = arrival,
            None => self.attached.push(AttachedPageTrack {
                number,
                arrival,
                track_id,
            }),
        }
    }
}

impl SubtitleOwner {
    /// Склад сеанса моста → дорожки ТЕКУЩЕГО медиа (каждый проход окна; без изменений дёшево).
    pub(super) fn attach_page_tracks(&mut self, shelf: &PageSubtitleShelfView) -> PageTracksUpdate {
        // Новый склад — новый сеанс: выбор зрителя прежнего сеанса к нему не относится.
        self.page_viewer.follow_shelf(shelf.shelf());
        let Some(media) = self.media.as_mut() else {
            return PageTracksUpdate::Unchanged;
        };
        if !media.page_tracks.accepts_shelf(shelf.shelf()) {
            return PageTracksUpdate::Unchanged;
        }
        // Новые и заменённые документы — в порядке поступления: последний `showing` побеждает.
        let mut fresh: Vec<&ShelvedPageSubtitle> = shelf
            .entries()
            .iter()
            .filter(|entry| !media.page_tracks.holds(entry))
            .collect();
        if fresh.is_empty() {
            return PageTracksUpdate::Unchanged;
        }
        fresh.sort_by_key(|entry| entry.arrival);
        media.page_tracks.shelf = Some(shelf.shelf());
        let mut page_showed_a_track = false;
        for entry in fresh {
            let description = PageTrackDescription::new(Arc::clone(&entry.document));
            let showing = description.was_showing_on_page();
            let number = description.number();
            let track_id = match media.page_tracks.track_of(number) {
                Some(existing) => {
                    // Загрузка прежнего документа больше не нужна: он заменён.
                    if let Some(load) = media.load_job.take_if(|load| load.track_id == existing) {
                        load.job.cancel();
                        self.retiring_jobs.push(load.job);
                    }
                    media.tracks.replace_page_track(existing, description);
                    existing
                }
                None => media.tracks.push_page_track(description),
            };
            media.page_tracks.record(number, entry.arrival, track_id);
            if showing {
                media.page_tracks.shown_on_page = Some(track_id);
                page_showed_a_track = true;
            } else if media.page_tracks.shown_on_page == Some(track_id) {
                // Страница больше не показывает эту дорожку: её выбор снова решает автовыбор.
                media.page_tracks.shown_on_page = None;
            }
        }
        distinguish_page_track_names(&mut media.tracks.page_descriptions_mut());
        let page_may_turn_on =
            !media.user_chose_track && self.page_viewer.lets_page_turn_subtitles_on();
        if page_showed_a_track && page_may_turn_on {
            // «Скачала — значит включено» (решение 271): только для этого медиа, конфиг не
            // трогаем — это не действие зрителя.
            media.visibility = SubtitleVisibility::Shown;
        }
        // Выбор: память выбора, затем дорожка страницы, затем язык (`reselect_automatically`).
        self.reselect_automatically();
        self.presenter.forget_shown_overlay();
        self.ensure_display_track_loading();
        PageTracksUpdate::Changed
    }

    /// Зритель выбрал дорожку `track_id` текущего медиа: выбор запоминается на весь сеанс
    /// (дорожка страницы — номером страницы, любая другая — как «не страница»).
    pub(super) fn note_viewer_track_choice(&mut self, track_id: SubtitleTrackId) {
        let choice = self
            .media
            .as_ref()
            .and_then(|media| media.page_tracks.number_of(track_id))
            .map_or(ViewerTrackChoice::OtherTrack, ViewerTrackChoice::PageTrack);
        self.page_viewer.note_track(choice);
    }

    /// Зритель включил или выключил субтитры: запоминается на весь сеанс.
    pub(super) fn note_viewer_visibility_choice(&mut self, visibility: SubtitleVisibility) {
        self.page_viewer.note_visibility(visibility);
    }

    /// Разобранный документ страницы становится готовой дорожкой; строка готовности в журнал.
    pub(super) fn accept_page_track_parsed(
        &mut self,
        track_id: SubtitleTrackId,
        track: ParsedPageTrack,
        fonts: Option<SubtitleFontLibrary>,
    ) -> JobResultDisposition {
        self.install_fonts_if_needed(fonts);
        let Some(media) = self.media.as_mut() else {
            return JobResultDisposition::DroppedStale;
        };
        media.load_job = None;
        let selected =
            media.visibility == SubtitleVisibility::Shown && media.selected == Some(track_id);
        let sync_offset = media.sync_offset;
        let source_id = self.source_ids.allocate();
        let Some(entry) = media.tracks.get_mut(track_id) else {
            return JobResultDisposition::DroppedStale;
        };
        let Some(page) = entry.description.page() else {
            warn!("A page subtitle result arrived for a track that is not from the page");
            return JobResultDisposition::DroppedStale;
        };
        let language = page.language().cloned();
        // Только номер, язык, формат и числа: подпись и текст — содержимое страницы.
        info!(
            track = page.number().get(),
            language = language
                .as_ref()
                .map_or("unknown", |language| language.as_str()),
            format = format_log_label(track.format),
            cues = track.timeline.len(),
            selected,
            "{PAGE_SUBTITLE_TRACK_READY_LINE}"
        );
        entry.load = TrackLoadState::Ready(ActiveSubtitleTrack::new(
            source_id,
            track.timeline,
            sync_offset,
            language,
        ));
        JobResultDisposition::Applied
    }

    /// Загрузка дорожки не удалась: если это дорожка страницы — строка в журнал (с номером).
    /// Само состояние «недоступна» ставит общий путь приёма результата.
    pub(super) fn note_page_track_failure(
        &self,
        track_id: SubtitleTrackId,
        failure: &SubtitleLoadFailure,
    ) {
        let page_number = self
            .media
            .as_ref()
            .and_then(|media| media.tracks.get(track_id))
            .and_then(|entry| entry.description.page())
            .map(PageTrackDescription::number);
        if let Some(number) = page_number {
            warn!(
                track = number.get(),
                reason = %failure,
                "{PAGE_SUBTITLE_TRACK_FAILED_LINE}"
            );
        }
    }

    /// Реклама страницы на экране или нет. Меняет только показ: дорожки и выбор те же.
    pub(super) fn set_page_ad_presence(&mut self, presence: PageAdPresence) {
        self.page_ad = presence;
    }

    /// Слой субтитров сейчас скрыт рекламой страницы.
    pub(super) fn hides_overlay_for_page_ad(&self) -> bool {
        self.page_ad == PageAdPresence::AdOnScreen
    }

    /// Сколько кадров ушло в рендер со слоем субтитров за жизнь владельца.
    pub(super) fn shown_overlay_frames(&self) -> u64 {
        self.shown_overlay_frames.0
    }
}
