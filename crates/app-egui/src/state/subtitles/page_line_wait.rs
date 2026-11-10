//! «Жду документ строки меню» (сессия 15a2): как выбор строки меню страницы превращается в
//! выбор зрителя, когда документ субтитров этой строки приходит.
//!
//! # Зачем
//! Строка меню CC — просьба к странице («включи немецкий»). Плеер сам документ не просит:
//! страница (YouTube) скачивает его и присылает обычным путём 13b как `page_download` со
//! `showing`. Чтобы пришедший документ не остался «просто дорожкой, которую зритель не
//! выбирал» (тогда `showing` страницы проигрывает прежнему выбору зрителя, а в память
//! расширения ничего не уходит), выбор строки ставит ожидание. Первый подходящий документ
//! после него считается выбором зрителя: субтитры включаются, дорожка запоминается на сеанс,
//! страница получает `subtitle_choice`.
//!
//! # Какой документ подходит
//! Только **новее запроса**: поступление склада больше того, что владелец видел в момент
//! выбора. Так документ, пришедший раньше (в том числе тот, что склад заново отдаёт новому
//! поколению медиа после рекламы), ожидание не закрывает. Замена той же дорожки (en → de →
//! en) получает новое поступление, поэтому подходит так же, как новая дорожка.
//! Документ обязан быть скачан страницей (`page_download`) и показываться на ней (`showing`):
//! остальные дорожки страницы выбором строки не становятся.
//!
//! # Когда ожидание снимается
//! Принято (выбор зрителя сделан), зритель выбрал другую дорожку или «выкл», запросил следующую
//! серию, начался новый сеанс моста (новый склад). Пока ждём, прежний показ не трогается.

use crate::page_subtitles::{
    PageSubtitleArrival, PageSubtitleDisplay, PageSubtitleSource, PageSubtitleTrackNumber,
    ShelvedPageSubtitle,
};

/// Ждёт ли владелец документ выбранной строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum PageDocumentWait {
    /// Строку не выбирали (или ожидание снято).
    #[default]
    NotWaiting,
    /// Строку выбрали: подходит документ с поступлением новее `newer_than`.
    Waiting {
        /// Самое позднее поступление, которое владелец видел в момент выбора (`None` — склад
        /// тогда был пуст: подойдёт любое).
        newer_than: Option<PageSubtitleArrival>,
    },
}

impl PageDocumentWait {
    /// Начать ждать документ новее `newer_than` (повторный выбор строки заменяет прежнее
    /// ожидание: нужна последняя строка).
    pub(super) fn begin(&mut self, newer_than: Option<PageSubtitleArrival>) {
        *self = Self::Waiting { newer_than };
    }

    /// Снять ожидание.
    pub(super) fn cancel(&mut self) {
        *self = Self::NotWaiting;
    }

    /// Ждём ли сейчас (проверяют тесты).
    #[cfg(test)]
    pub(super) fn is_waiting(self) -> bool {
        matches!(self, Self::Waiting { .. })
    }

    /// Дорожка страницы, которой отвечает ожидание, среди новых документов склада: последний
    /// по поступлению из подходящих (см. описание модуля). `None` — не ждём или подходящих нет.
    pub(super) fn answering_track(
        self,
        fresh: &[&ShelvedPageSubtitle],
    ) -> Option<PageSubtitleTrackNumber> {
        let Self::Waiting { newer_than } = self else {
            return None;
        };
        fresh
            .iter()
            .filter(|entry| newer_than.is_none_or(|seen| entry.arrival > seen))
            .filter(|entry| entry.document.source == PageSubtitleSource::PageDownload)
            .filter(|entry| entry.document.display == PageSubtitleDisplay::ShowingOnPage)
            .max_by_key(|entry| entry.arrival)
            .map(|entry| entry.document.track)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::page_subtitles::{
        PageSubtitleDocument, PageSubtitleFormatHint, PageSubtitleLabel, PageSubtitleLanguage,
    };

    fn arrival(number: u32) -> PageSubtitleArrival {
        (0..number).fold(PageSubtitleArrival::FIRST, |current, _| current.next())
    }

    fn shelved(
        track: u32,
        arrival_number: u32,
        source: PageSubtitleSource,
        display: PageSubtitleDisplay,
    ) -> ShelvedPageSubtitle {
        ShelvedPageSubtitle {
            arrival: arrival(arrival_number),
            document: Arc::new(PageSubtitleDocument {
                track: PageSubtitleTrackNumber::new(track),
                language: PageSubtitleLanguage::NotNamed,
                label: PageSubtitleLabel::NotGiven,
                format_hint: PageSubtitleFormatHint::WebVtt,
                display,
                source,
                bytes: Arc::from(b"WEBVTT".as_slice()),
            }),
        }
    }

    fn download(track: u32, arrival_number: u32) -> ShelvedPageSubtitle {
        shelved(
            track,
            arrival_number,
            PageSubtitleSource::PageDownload,
            PageSubtitleDisplay::ShowingOnPage,
        )
    }

    #[test]
    fn not_waiting_answers_nothing_even_for_a_fitting_document() {
        let entry = download(1, 5);
        assert_eq!(
            PageDocumentWait::NotWaiting.answering_track(&[&entry]),
            None
        );
    }

    #[test]
    fn only_documents_newer_than_the_request_answer_it() {
        let mut wait = PageDocumentWait::default();
        wait.begin(Some(arrival(3)));
        let old = download(1, 3);
        let new = download(2, 4);
        assert_eq!(
            wait.answering_track(&[&old]),
            None,
            "seen before the choice"
        );
        assert_eq!(
            wait.answering_track(&[&old, &new]),
            Some(PageSubtitleTrackNumber::new(2))
        );
    }

    #[test]
    fn an_empty_shelf_at_request_time_accepts_any_arrival() {
        let mut wait = PageDocumentWait::default();
        wait.begin(None);
        assert_eq!(
            wait.answering_track(&[&download(7, 0)]),
            Some(PageSubtitleTrackNumber::new(7))
        );
    }

    #[test]
    fn only_shown_page_downloads_answer_and_the_latest_wins() {
        let mut wait = PageDocumentWait::default();
        wait.begin(None);
        let element = shelved(
            1,
            1,
            PageSubtitleSource::PageElement,
            PageSubtitleDisplay::ShowingOnPage,
        );
        let hidden = shelved(
            2,
            2,
            PageSubtitleSource::PageDownload,
            PageSubtitleDisplay::NotShowingOnPage,
        );
        assert_eq!(wait.answering_track(&[&element, &hidden]), None);
        let first = download(3, 3);
        let second = download(4, 4);
        assert_eq!(
            wait.answering_track(&[&first, &element, &second]),
            Some(PageSubtitleTrackNumber::new(4)),
            "the latest fitting document wins"
        );
    }

    #[test]
    fn cancel_stops_waiting_and_begin_replaces_the_previous_request() {
        let mut wait = PageDocumentWait::default();
        wait.begin(Some(arrival(1)));
        assert!(wait.is_waiting());
        wait.begin(Some(arrival(9)));
        assert_eq!(wait.answering_track(&[&download(1, 5)]), None);
        wait.cancel();
        assert!(!wait.is_waiting());
        assert_eq!(wait.answering_track(&[&download(1, 20)]), None);
    }
}
