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

use subtitle_core::SubtitleTrackId;

use super::intents::SubtitleVisibility;
use crate::page_subtitles::{PageSubtitleShelfId, PageSubtitleTrackNumber};

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
    /// Зритель выбрал эту дорожку страницы.
    ViewerPageTrack(PageSubtitleTrackNumber),
    /// Зритель выбрал не дорожку страницы: только автовыбор по языку.
    AutomaticOnly,
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

    /// Чья воля выбирает дорожку страницы.
    pub(super) fn track_preference(&self) -> PageTrackPreference {
        match self.track {
            ViewerTrackChoice::NotMade => PageTrackPreference::FollowPageShowing,
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
