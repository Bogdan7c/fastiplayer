//! `show_page` (протокол 1.6, сессия 13c, решение 278): просьба плеера показать зрителю страницу.
//!
//! Зритель нажал «Change quality on the page» в меню кнопки качества: качество выбирает
//! плеер страницы, а не мы, поэтому плеер просит экстеншен вывести вкладку сеанса вперёд.
//! Сообщение надёжное, как `play`/`pause`: при полной очереди остаётся и уходит в следующий
//! проход, повтор неотправленного заменяет прежний (два нажатия подряд — одна просьба).
//!
//! **Версия.** `show_page` появился в 1.6. Экстеншен прежней версии такого сообщения не знает,
//! поэтому окно заранее спрашивает [`PageOutbound::page_view_permission`] и показывает пункт
//! выключенным ("Update the Fastiplayer Bridge extension…"), а не шлёт то, что отвергнут.
//! Проверка в [`PageOutbound::request_show_page`] — страховка с явным итогом, а не молчаливая
//! потеря.

use browser_bridge_protocol::{PlayerSessionMessage, ProtocolVersion};

use super::PageOutbound;
use super::resource_requests::PageProtocol;

/// С какой версии протокола есть `show_page` (`PlayerSessionMessage::introduced_in`;
/// равенство закреплено тестом).
pub(crate) const SHOW_PAGE_SINCE: ProtocolVersion = ProtocolVersion::V1_6;

/// Можно ли сейчас просить страницу показаться (чистый вопрос, ничего не ставит в очередь).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageViewPermission {
    /// Можно: страница говорит на 1.6 и новее.
    Allowed,
    /// Нельзя пока: страница ещё не подключилась (версия неизвестна).
    NotAttachedYet,
    /// Нельзя: общая версия ниже 1.6 — экстеншен устарел.
    ProtocolTooOld,
}

/// Что вышло с просьбой показать страницу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a request that was not queued means the page will never be shown"]
pub(crate) enum ShowPageRequestOutcome {
    /// Поставлена: уйдёт экстеншену, как только очередь примет.
    Queued,
    /// Не поставлена: страница ещё не подключалась (версия неизвестна).
    NotAttachedYet,
    /// Не поставлена: общая версия ниже 1.6.
    ProtocolTooOld,
}

impl PageOutbound {
    /// Можно ли сейчас просить страницу показаться (по общей версии протокола).
    pub(crate) const fn page_view_permission(&self) -> PageViewPermission {
        match self.page_protocol {
            PageProtocol::NotAttachedYet => PageViewPermission::NotAttachedYet,
            PageProtocol::Agreed(version) if version.includes(SHOW_PAGE_SINCE) => {
                PageViewPermission::Allowed
            }
            PageProtocol::Agreed(_) => PageViewPermission::ProtocolTooOld,
        }
    }

    /// Ставит `show_page{}` в надёжную отправку (если общая версия позволяет).
    pub(crate) fn request_show_page(&mut self) -> ShowPageRequestOutcome {
        match self.page_view_permission() {
            PageViewPermission::Allowed => {
                self.queue_event_replacing_same_kind(PlayerSessionMessage::ShowPage {});
                ShowPageRequestOutcome::Queued
            }
            PageViewPermission::NotAttachedYet => ShowPageRequestOutcome::NotAttachedYet,
            PageViewPermission::ProtocolTooOld => ShowPageRequestOutcome::ProtocolTooOld,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use browser_bridge_session::SendToPageError;

    use super::*;
    use crate::browser_session::outbound::PageSender;

    /// Отправитель, который записывает всё, что ему дали.
    #[derive(Clone, Default)]
    struct RecordingSender(Arc<Mutex<Vec<PlayerSessionMessage>>>);

    impl PageSender for RecordingSender {
        fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError> {
            if let Ok(mut sent) = self.0.lock() {
                sent.push(message.clone());
            }
            Ok(())
        }
    }

    fn sent(sender: &RecordingSender) -> Vec<PlayerSessionMessage> {
        sender.0.lock().map(|sent| sent.clone()).unwrap_or_default()
    }

    #[test]
    fn the_constant_matches_the_protocol() {
        assert_eq!(
            SHOW_PAGE_SINCE,
            PlayerSessionMessage::ShowPage {}.introduced_in()
        );
    }

    #[test]
    fn at_1_6_the_request_goes_to_the_page_as_show_page() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        outbound.note_agreed_protocol(ProtocolVersion::V1_6);
        assert_eq!(outbound.page_view_permission(), PageViewPermission::Allowed);
        assert_eq!(outbound.request_show_page(), ShowPageRequestOutcome::Queued);
        outbound.flush(Instant::now());
        assert_eq!(sent(&sender), vec![PlayerSessionMessage::ShowPage {}]);
    }

    #[test]
    fn two_requests_before_a_flush_leave_as_one() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        outbound.note_agreed_protocol(ProtocolVersion::V1_6);
        assert_eq!(outbound.request_show_page(), ShowPageRequestOutcome::Queued);
        assert_eq!(outbound.request_show_page(), ShowPageRequestOutcome::Queued);
        outbound.flush(Instant::now());
        assert_eq!(sent(&sender), vec![PlayerSessionMessage::ShowPage {}]);
    }

    #[test]
    fn below_1_6_or_before_attach_nothing_is_queued_and_the_cause_is_told() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        assert_eq!(
            outbound.page_view_permission(),
            PageViewPermission::NotAttachedYet
        );
        assert_eq!(
            outbound.request_show_page(),
            ShowPageRequestOutcome::NotAttachedYet
        );
        outbound.note_agreed_protocol(ProtocolVersion::V1_5);
        assert_eq!(
            outbound.page_view_permission(),
            PageViewPermission::ProtocolTooOld
        );
        assert_eq!(
            outbound.request_show_page(),
            ShowPageRequestOutcome::ProtocolTooOld
        );
        outbound.flush(Instant::now());
        assert!(sent(&sender).is_empty());
        assert!(!outbound.has_pending());
    }

    #[test]
    fn the_permission_query_queues_nothing() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        outbound.note_agreed_protocol(ProtocolVersion::V1_6);
        assert_eq!(outbound.page_view_permission(), PageViewPermission::Allowed);
        assert!(!outbound.has_pending());
        outbound.flush(Instant::now());
        assert!(sent(&sender).is_empty());
    }
}
