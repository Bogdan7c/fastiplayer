//! `reopen_page` (протокол 1.4, «сирота», сессия 11b1): просьба плеера вернуть страницу.
//!
//! Страница ушла (`page_left`), но связь держится; зритель нажал play в окне — плеер
//! просит экстеншен открыть страницу заново на этой секунде (решения 130, 240). Сообщение
//! надёжное, как `play`/`pause`: при полной очереди остаётся и уходит в следующий проход,
//! повтор неотправленного заменяет прежний. Сколько раз просить, решает владелец состояния
//! возврата (`session_media::page_reopen`, одна просьба на сироту), а не очередь.
//!
//! **Версия.** `reopen_page` появился в 1.4. Сироты при общей версии ниже не бывает
//! (сторож сервера закрывает сеанс на `page_left`), а проверка здесь — страховка с явным
//! итогом ([`ReopenRequestOutcome::ProtocolTooOld`]), а не молчаливая потеря.

use browser_bridge_protocol::{MediaTime, PlayerSessionMessage, ProtocolVersion};

use super::PageOutbound;
use super::resource_requests::PageProtocol;

/// С какой версии протокола есть `reopen_page` (`PlayerSessionMessage::introduced_in`;
/// равенство закреплено тестом).
pub(crate) const REOPEN_PAGE_SINCE: ProtocolVersion = ProtocolVersion::V1_4;

/// Что вышло с просьбой вернуть страницу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a request that was not queued means the page will never come back"]
pub(crate) enum ReopenRequestOutcome {
    /// Поставлена: уйдёт экстеншену, как только очередь примет.
    Queued,
    /// Не поставлена: страница ещё не подключалась (версия неизвестна).
    NotAttachedYet,
    /// Не поставлена: общая версия ниже 1.4.
    ProtocolTooOld,
}

impl PageOutbound {
    /// Ставит `reopen_page{position}` в надёжную отправку (если общая версия позволяет).
    /// Позиция — в часах страницы (как `clock.position_us`).
    pub(crate) fn request_page_reopen(&mut self, position: MediaTime) -> ReopenRequestOutcome {
        match self.page_protocol {
            PageProtocol::NotAttachedYet => ReopenRequestOutcome::NotAttachedYet,
            PageProtocol::Agreed(version) if version.includes(REOPEN_PAGE_SINCE) => {
                self.queue_event_replacing_same_kind(PlayerSessionMessage::ReopenPage {
                    position_us: position,
                });
                ReopenRequestOutcome::Queued
            }
            PageProtocol::Agreed(_) => ReopenRequestOutcome::ProtocolTooOld,
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
        let message = PlayerSessionMessage::ReopenPage {
            position_us: MediaTime::ZERO,
        };
        assert_eq!(REOPEN_PAGE_SINCE, message.introduced_in());
    }

    #[test]
    fn at_1_4_the_request_goes_to_the_page_with_the_position() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        outbound.note_agreed_protocol(ProtocolVersion::V1_4);
        let outcome = outbound.request_page_reopen(MediaTime::from_micros(12_340_000).unwrap());
        assert_eq!(outcome, ReopenRequestOutcome::Queued);
        outbound.flush(Instant::now());
        assert_eq!(
            sent(&sender),
            vec![PlayerSessionMessage::ReopenPage {
                position_us: MediaTime::from_micros(12_340_000).unwrap(),
            }]
        );
    }

    #[test]
    fn below_1_4_or_before_attach_nothing_is_queued() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        assert_eq!(
            outbound.request_page_reopen(MediaTime::ZERO),
            ReopenRequestOutcome::NotAttachedYet
        );
        outbound.note_agreed_protocol(ProtocolVersion::V1_3);
        assert_eq!(
            outbound.request_page_reopen(MediaTime::ZERO),
            ReopenRequestOutcome::ProtocolTooOld
        );
        outbound.flush(Instant::now());
        assert!(sent(&sender).is_empty());
        assert!(!outbound.has_pending());
    }
}
