//! Страница открытого сеанса, как её получает окно (04c): подпись + «новое ли это видео».
//!
//! Зачем «новое ли»: подпись приходит при КАЖДОМ открытии, в том числе при повторе того
//! же сеанса (расширение переспросило). Заголовок обновить можно всегда, а вот окно
//! поднимать поверх чужих дел — только когда видео действительно новое: старт сеанса
//! или замена вкладки (решение 40).

use browser_bridge_protocol::GrantOutcome;

use crate::page_label::PageLabel;

/// Откуда взялось открытие сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageArrival {
    /// Новый сеанс или замена вкладки: окну пора выйти вперёд.
    NewVideo,
    /// Повтор уже идущего сеанса: окно остаётся где было.
    SameSessionAgain,
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

#[cfg(test)]
mod tests {
    use super::*;

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
