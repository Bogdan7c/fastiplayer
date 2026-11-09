//! Выбор зрителя о субтитрах страницы за весь сеанс моста (13b1).
//!
//! # Зачем отдельно от `MediaSubtitles`
//! Состояние медиа умирает со сменой поколения (реклама посреди фильма, смена качества — новый
//! `MediaSource`, новое медиа). Если бы выбор зрителя жил там же, после рекламы `showing`
//! страницы снова включил бы субтитры, которые зритель выключил. Поэтому выбор живёт у
//! владельца субтитров и привязан к складу сеанса ([`PageSubtitleShelfId`]): новое медиа того же
//! склада применяет выбор зрителя вместо `showing`, новый склад (новый сеанс) его забывает.
//!
//! # Что запоминается
//! Только явные действия зрителя: включил или выключил субтитры ([`ViewerVisibilityChoice`]) и
//! какую дорожку выбрал ([`ViewerTrackChoice`]). Дорожка страницы запоминается номером страницы:
//! замена документа той же дорожки (тот же номер) не сбивает выбор.
//!
//! # Подсказка страницы (протокол 1.7, 14a)
//! Экстеншен помнит выбор зрителя в прошлых сериях и подсказывает его
//! ([`PageSubtitlePreference`]). Порядок силы: действие зрителя в этом сеансе > подсказка
//! страницы > `showing` страницы (13b) > наш автовыбор по языку. Подсказка живёт столько же,
//! сколько выбор зрителя (сеанс), и действует на каждое новое медиа сеанса.
//!
//! **Подсказка ждёт свою дорожку.** Пока дорожки с подсказанным номером нет, действует то, что
//! слабее (`showing`, затем автовыбор); как только она пришла — включается она. «Выключено»
//! действует сразу и не даёт `showing` включить субтитры.
//!
//! # Отчёт странице
//! После действия зрителя владелец кладёт сюда итог для памяти экстеншена
//! ([`PageViewerChoice::note_report`]): дорожка страницы или «выключено». Свой файл и встроенная
//! дорожка отчёта не дают — экстеншену их не запомнить.

use subtitle_core::SubtitleTrackId;

use super::intents::SubtitleVisibility;
use crate::page_subtitles::{
    PageSubtitlePreference, PageSubtitleShelfId, PageSubtitleTrackNumber, PageSubtitleViewerChoice,
};

/// Что зритель сделал с видимостью субтитров в этом сеансе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ViewerVisibilityChoice {
    /// Не трогал: страница вправе включить субтитры своим `showing`.
    #[default]
    NotMade,
    /// Включил сам.
    TurnedOn,
    /// Выключил сам: страница их больше не включает.
    TurnedOff,
}

/// Какую дорожку зритель выбрал в этом сеансе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ViewerTrackChoice {
    /// Не выбирал: решает `showing` страницы, иначе автовыбор.
    #[default]
    NotMade,
    /// Дорожку страницы с этим номером (переживает замену документа).
    PageTrack(PageSubtitleTrackNumber),
    /// Другую дорожку (встроенную в поток, файл): `showing` страницы не применяется, решает
    /// обычный автовыбор.
    OtherTrack,
}

/// Чья воля выбирает дорожку страницы в новом медиа сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageTrackPreference {
    /// Зритель не выбирал: дорожка, которую страница показывала последней.
    FollowPageShowing,
    /// Зритель не выбирал, а страница подсказала эту дорожку: она, как только придёт; до тех
    /// пор — дорожка, которую страница показывала.
    PagePreferredTrack(PageSubtitleTrackNumber),
    /// Зритель выбрал эту дорожку страницы.
    ViewerPageTrack(PageSubtitleTrackNumber),
    /// Зритель выбрал не дорожку страницы: только автовыбор по языку.
    AutomaticOnly,
}

/// Что воля страницы (подсказка, затем `showing`) делает с видимостью субтитров медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageVisibilityWill {
    /// Ничего: зритель уже решал сам, или странице нечего сказать.
    Untouched,
    /// Включить (только для этого медиа, конфиг не трогается).
    TurnOn,
    /// Выключить (подсказка «выключено»).
    TurnOff,
}

/// Что владелец знает о дорожках страницы медиа, когда спрашивает волю страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageTrackEvidence {
    /// Дорожка с подсказанным номером уже стала дорожкой этого медиа.
    pub(super) preferred_track_attached: bool,
    /// В этом приложении склада пришёл документ, который страница показывала (`showing`).
    pub(super) page_showed_a_track: bool,
}

/// Выбор зрителя за сеанс (один склад).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PageViewerChoice {
    /// Склад, к сеансу которого относится выбор (`None` — склада ещё не видели).
    shelf: Option<PageSubtitleShelfId>,
    /// Видимость.
    visibility: ViewerVisibilityChoice,
    /// Дорожка.
    track: ViewerTrackChoice,
    /// Последняя подсказка страницы этого сеанса.
    page_preference: PageSubtitlePreference,
    /// Итог последнего действия зрителя для памяти экстеншена (ещё не забран окном).
    report: Option<PageSubtitleViewerChoice>,
}

impl PageViewerChoice {
    /// Окно показало склад `shelf`: другой склад — другой сеанс, прежний выбор забывается.
    /// Выбор, сделанный до первого склада, остаётся за первым складом.
    pub(super) fn follow_shelf(&mut self, shelf: PageSubtitleShelfId) {
        if self.shelf.is_some_and(|known| known != shelf) {
            *self = Self::default();
        }
        self.shelf = Some(shelf);
    }

    /// Последнее слово страницы о субтитрах этого сеанса (приходит со снимком склада).
    pub(super) fn note_page_preference(&mut self, preference: PageSubtitlePreference) {
        self.page_preference = preference;
    }

    /// Итог действия зрителя для памяти экстеншена (`None` — сообщать нечего: свой файл,
    /// встроенная дорожка). Новый итог заменяет незабранный.
    pub(super) fn note_report(&mut self, report: Option<PageSubtitleViewerChoice>) {
        self.report = report;
    }

    /// Забирает итог действия зрителя (один раз).
    pub(super) fn take_report(&mut self) -> Option<PageSubtitleViewerChoice> {
        self.report.take()
    }

    /// Зритель включил или выключил субтитры.
    pub(super) fn note_visibility(&mut self, visibility: SubtitleVisibility) {
        self.visibility = match visibility {
            SubtitleVisibility::Shown => ViewerVisibilityChoice::TurnedOn,
            SubtitleVisibility::Hidden => ViewerVisibilityChoice::TurnedOff,
        };
    }

    /// Зритель выбрал дорожку (выбор дорожки всегда включает субтитры).
    pub(super) fn note_track(&mut self, track: ViewerTrackChoice) {
        self.track = track;
        self.visibility = ViewerVisibilityChoice::TurnedOn;
    }

    /// Может ли `showing` страницы включить субтитры: только пока зритель видимость не трогал
    /// и дорожку не выбирал.
    pub(super) fn lets_page_turn_subtitles_on(&self) -> bool {
        self.visibility == ViewerVisibilityChoice::NotMade
            && self.track == ViewerTrackChoice::NotMade
    }

    /// Что воля страницы делает с видимостью (правила — в описании модуля).
    pub(super) fn page_visibility_will(&self, evidence: PageTrackEvidence) -> PageVisibilityWill {
        if !self.lets_page_turn_subtitles_on() {
            return PageVisibilityWill::Untouched;
        }
        match self.page_preference {
            PageSubtitlePreference::Off => PageVisibilityWill::TurnOff,
            PageSubtitlePreference::PageTrack(_) if evidence.preferred_track_attached => {
                PageVisibilityWill::TurnOn
            }
            // Подсказки нет или её дорожка ещё не пришла: действует `showing` (решение 271).
            PageSubtitlePreference::PageTrack(_) | PageSubtitlePreference::NotGiven => {
                if evidence.page_showed_a_track {
                    PageVisibilityWill::TurnOn
                } else {
                    PageVisibilityWill::Untouched
                }
            }
        }
    }

    /// Чья воля выбирает дорожку страницы.
    pub(super) fn track_preference(&self) -> PageTrackPreference {
        match self.track {
            ViewerTrackChoice::NotMade => match self.page_preference {
                PageSubtitlePreference::PageTrack(number) => {
                    PageTrackPreference::PagePreferredTrack(number)
                }
                PageSubtitlePreference::NotGiven | PageSubtitlePreference::Off => {
                    PageTrackPreference::FollowPageShowing
                }
            },
            ViewerTrackChoice::PageTrack(number) => PageTrackPreference::ViewerPageTrack(number),
            ViewerTrackChoice::OtherTrack => PageTrackPreference::AutomaticOnly,
        }
    }
}

/// Дорожка списка, которую выбирает воля `preference`, по учёту дорожек страницы медиа.
pub(super) fn picked_track(
    preference: PageTrackPreference,
    shown_on_page: Option<SubtitleTrackId>,
    track_of: impl Fn(PageSubtitleTrackNumber) -> Option<SubtitleTrackId>,
) -> Option<SubtitleTrackId> {
    match preference {
        PageTrackPreference::FollowPageShowing => shown_on_page,
        PageTrackPreference::PagePreferredTrack(number) => track_of(number).or(shown_on_page),
        PageTrackPreference::ViewerPageTrack(number) => track_of(number),
        PageTrackPreference::AutomaticOnly => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_shelf_forgets_the_choice_and_the_first_shelf_keeps_an_early_one() {
        let first = PageSubtitleShelfId::FIRST;
        let mut choice = PageViewerChoice::default();
        choice.note_visibility(SubtitleVisibility::Hidden);
        choice.follow_shelf(first);
        assert!(
            !choice.lets_page_turn_subtitles_on(),
            "kept for the first shelf"
        );
        choice.follow_shelf(first);
        assert!(
            !choice.lets_page_turn_subtitles_on(),
            "the same shelf keeps it"
        );
        choice.follow_shelf(first.next());
        assert!(
            choice.lets_page_turn_subtitles_on(),
            "a new session forgets it"
        );
    }

    const NO_EVIDENCE: PageTrackEvidence = PageTrackEvidence {
        preferred_track_attached: false,
        page_showed_a_track: false,
    };

    #[test]
    fn the_page_preference_beats_showing_and_waits_for_its_track() {
        let preferred = PageSubtitleTrackNumber::new(5);
        let mut choice = PageViewerChoice::default();
        choice.note_page_preference(PageSubtitlePreference::PageTrack(preferred));
        assert_eq!(
            choice.track_preference(),
            PageTrackPreference::PagePreferredTrack(preferred)
        );
        let showing = Some(SubtitleTrackId::new(1));
        let preferred_track = SubtitleTrackId::new(4);
        // Дорожки 5 ещё нет: пока действует `showing`.
        assert_eq!(
            picked_track(choice.track_preference(), showing, |_| None),
            showing
        );
        let showed = PageTrackEvidence {
            page_showed_a_track: true,
            ..NO_EVIDENCE
        };
        assert_eq!(
            choice.page_visibility_will(showed),
            PageVisibilityWill::TurnOn
        );
        assert_eq!(
            choice.page_visibility_will(NO_EVIDENCE),
            PageVisibilityWill::Untouched,
            "nothing arrived yet: the page has nothing to switch on"
        );
        // Пришла — она главнее `showing`.
        let track_of = |wanted| (wanted == preferred).then_some(preferred_track);
        assert_eq!(
            picked_track(choice.track_preference(), showing, track_of),
            Some(preferred_track)
        );
        let attached = PageTrackEvidence {
            preferred_track_attached: true,
            ..NO_EVIDENCE
        };
        assert_eq!(
            choice.page_visibility_will(attached),
            PageVisibilityWill::TurnOn
        );
    }

    #[test]
    fn off_beats_showing_and_the_viewer_beats_the_preference() {
        let mut choice = PageViewerChoice::default();
        choice.note_page_preference(PageSubtitlePreference::Off);
        let everything = PageTrackEvidence {
            preferred_track_attached: true,
            page_showed_a_track: true,
        };
        assert_eq!(
            choice.page_visibility_will(everything),
            PageVisibilityWill::TurnOff
        );
        choice.note_visibility(SubtitleVisibility::Shown);
        assert_eq!(
            choice.page_visibility_will(everything),
            PageVisibilityWill::Untouched,
            "the viewer's own action in this session wins"
        );

        let mut picked = PageViewerChoice::default();
        picked.note_page_preference(PageSubtitlePreference::PageTrack(
            PageSubtitleTrackNumber::new(2),
        ));
        picked.note_track(ViewerTrackChoice::PageTrack(PageSubtitleTrackNumber::new(
            9,
        )));
        assert_eq!(
            picked.track_preference(),
            PageTrackPreference::ViewerPageTrack(PageSubtitleTrackNumber::new(9))
        );
    }

    #[test]
    fn the_report_is_taken_once_and_a_new_session_forgets_the_preference() {
        let mut choice = PageViewerChoice::default();
        choice.follow_shelf(PageSubtitleShelfId::FIRST);
        let preferred = PageSubtitleTrackNumber::new(3);
        choice.note_page_preference(PageSubtitlePreference::PageTrack(preferred));
        choice.note_report(Some(PageSubtitleViewerChoice::Off));
        assert_eq!(choice.take_report(), Some(PageSubtitleViewerChoice::Off));
        assert_eq!(choice.take_report(), None);
        assert_eq!(
            choice.track_preference(),
            PageTrackPreference::PagePreferredTrack(preferred)
        );
        choice.follow_shelf(PageSubtitleShelfId::FIRST.next());
        assert_eq!(
            choice.track_preference(),
            PageTrackPreference::FollowPageShowing,
            "a new session starts without the old page's word"
        );
    }

    #[test]
    fn a_picked_page_track_is_followed_by_number_and_another_track_disables_the_page_pick() {
        let number = PageSubtitleTrackNumber::new(7);
        let mut choice = PageViewerChoice::default();
        assert_eq!(
            choice.track_preference(),
            PageTrackPreference::FollowPageShowing
        );
        choice.note_track(ViewerTrackChoice::PageTrack(number));
        assert_eq!(
            choice.track_preference(),
            PageTrackPreference::ViewerPageTrack(number)
        );
        assert!(!choice.lets_page_turn_subtitles_on());
        let track_of = |wanted| (wanted == number).then(|| SubtitleTrackId::new(3));
        assert_eq!(
            picked_track(choice.track_preference(), None, track_of),
            Some(SubtitleTrackId::new(3))
        );
        choice.note_track(ViewerTrackChoice::OtherTrack);
        assert_eq!(
            picked_track(
                choice.track_preference(),
                Some(SubtitleTrackId::new(1)),
                track_of
            ),
            None
        );
    }
}
