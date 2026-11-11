//! Страница открытого сеанса, как её получает окно (04c): подпись + «новое ли это видео».
//!
//! Зачем «новое ли»: подпись приходит при КАЖДОМ открытии, в том числе при повторе того
//! же сеанса (расширение переспросило). Заголовок обновить можно всегда, а вот окно
//! поднимать поверх чужих дел — только когда видео действительно новое: старт сеанса
//! или замена вкладки (решение 40).
//!
//! С протокола 1.9 (16a) заголовок вкладки может смениться посреди сеанса
//! (`page_title_changed`): одностраничник поставил следующую серию. Окно получает ту же
//! подпись с новым заголовком и прежним доменом ([`PageArrival::TitleChanged`]) — заголовок
//! и Info обновляются, окно поверх чужих дел не выходит. Последнюю подпись и ещё не
//! забранную окном держит [`PageLabelHandoff`].

use browser_bridge_protocol::GrantOutcome;

use crate::page_label::PageLabel;

/// Откуда взялось открытие сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageArrival {
    /// Новый сеанс или замена вкладки: окну пора выйти вперёд.
    NewVideo,
    /// Повтор уже идущего сеанса: окно остаётся где было.
    SameSessionAgain,
    /// Тот же сеанс, страница сменила заголовок вкладки (16a): окно остаётся где было.
    TitleChanged,
}

impl PageArrival {
    /// Как исход выдачи сеанса определяет, новое ли видео.
    pub(super) const fn from_grant(grant: GrantOutcome) -> Self {
        match grant {
            GrantOutcome::Started | GrantOutcome::ReplacedPrevious => Self::NewVideo,
            GrantOutcome::ReusedExisting => Self::SameSessionAgain,
        }
    }
}

/// Страница, названная при открытии сеанса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenedPage {
    /// Домен и заголовок (Debug скрыт).
    pub(super) label: PageLabel,
    /// Новое ли видео.
    pub(super) arrival: PageArrival,
}

/// Подпись страницы на пути к окну: последняя известная и ещё не забранная окном.
///
/// Домен в `page_title_changed` не приходит (он не меняется в пределах сеанса), поэтому
/// новая подпись собирается из нового заголовка и домена последней известной.
#[derive(Debug, Default)]
pub(super) struct PageLabelHandoff {
    /// Последняя подпись, которую получило или получит окно.
    current: Option<PageLabel>,
    /// Подпись, которую окно ещё не забрало.
    unclaimed: Option<OpenedPage>,
}

/// Итог смены заголовка вкладки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetitleOutcome {
    /// Новая подпись ждёт окно.
    Handed,
    /// Страница ещё не названа (сеанса не было): домена нет, подписывать нечего.
    NoPageYet,
}

impl PageLabelHandoff {
    /// Открыт сеанс: его подпись вытесняет невостребованную (новая страница важнее).
    pub(super) fn note_opened(&mut self, page: OpenedPage) {
        self.current = Some(page.label.clone());
        self.unclaimed = Some(page);
    }

    /// Заголовок вкладки сменился посреди сеанса.
    ///
    /// Если окно ещё не забрало подпись открытия (`NewVideo`), меняется только её заголовок:
    /// иначе потерялись бы подъём окна и новый номер видео у MPRIS.
    pub(super) fn note_retitled(&mut self, title: &str) -> RetitleOutcome {
        let Some(current) = self.current.as_ref() else {
            return RetitleOutcome::NoPageYet;
        };
        let label = PageLabel::new(title, current.domain());
        self.current = Some(label.clone());
        match self.unclaimed.as_mut() {
            Some(unclaimed) => unclaimed.label = label,
            None => {
                self.unclaimed = Some(OpenedPage {
                    label,
                    arrival: PageArrival::TitleChanged,
                });
            }
        }
        RetitleOutcome::Handed
    }

    /// Подпись, которую окно ещё не забрало (`None` — с прошлого раза ничего). Забирает.
    pub(super) fn take_unclaimed(&mut self) -> Option<OpenedPage> {
        self.unclaimed.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(title: &str, arrival: PageArrival) -> OpenedPage {
        OpenedPage {
            label: PageLabel::new(title, "video.example"),
            arrival,
        }
    }

    #[test]
    fn a_retitle_before_any_page_has_no_domain_and_hands_nothing() {
        let mut handoff = PageLabelHandoff::default();
        assert_eq!(handoff.note_retitled("Серія 2"), RetitleOutcome::NoPageYet);
        assert_eq!(handoff.take_unclaimed(), None);
    }

    #[test]
    fn a_retitle_keeps_the_domain_and_does_not_raise_the_window() {
        let mut handoff = PageLabelHandoff::default();
        handoff.note_opened(opened("Серія 1", PageArrival::NewVideo));
        assert_eq!(
            handoff.take_unclaimed(),
            Some(opened("Серія 1", PageArrival::NewVideo))
        );
        assert_eq!(handoff.note_retitled("Серія 2"), RetitleOutcome::Handed);
        let retitled = handoff
            .take_unclaimed()
            .expect("a new label for the window");
        assert_eq!(retitled, opened("Серія 2", PageArrival::TitleChanged));
        // Не новое видео: окно не выходит вперёд, номер видео у MPRIS не меняется.
        assert_ne!(retitled.arrival, PageArrival::NewVideo);
        assert_eq!(retitled.label.window_title(), "Серія 2 — video.example");
        assert_eq!(handoff.take_unclaimed(), None);
    }

    #[test]
    fn a_retitle_of_an_unclaimed_new_video_keeps_its_arrival() {
        let mut handoff = PageLabelHandoff::default();
        handoff.note_opened(opened("Серія 1", PageArrival::NewVideo));
        handoff.note_retitled("Серія 2");
        // Окно получит одну подпись: новый заголовок, но подъём окна не потерян.
        assert_eq!(
            handoff.take_unclaimed(),
            Some(opened("Серія 2", PageArrival::NewVideo))
        );
    }

    #[test]
    fn an_empty_title_leaves_the_window_with_the_domain() {
        let mut handoff = PageLabelHandoff::default();
        handoff.note_opened(opened("Серія 1", PageArrival::NewVideo));
        let _first = handoff.take_unclaimed();
        handoff.note_retitled("");
        let retitled = handoff
            .take_unclaimed()
            .expect("a new label for the window");
        assert_eq!(retitled.label.window_title(), "video.example");
    }

    #[test]
    fn started_and_replaced_sessions_are_new_videos_a_reuse_is_not() {
        assert_eq!(
            PageArrival::from_grant(GrantOutcome::Started),
            PageArrival::NewVideo
        );
        assert_eq!(
            PageArrival::from_grant(GrantOutcome::ReplacedPrevious),
            PageArrival::NewVideo
        );
        assert_eq!(
            PageArrival::from_grant(GrantOutcome::ReusedExisting),
            PageArrival::SameSessionAgain
        );
    }
}
