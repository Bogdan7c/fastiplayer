//! `subtitle_choice{choice}` (протокол 1.7, сессия 14a, решение 283): плеер сообщает странице
//! выбор субтитров зрителя, чтобы экстеншен запомнил его для следующих серий.
//!
//! Субтитры рисуем мы, поэтому выбор живёт у нас, а странице нужен только итог: дорожка
//! страницы с номером или «выключено». Кто решает, что это действие зрителя (меню CC, клавиши
//! C/V, а не автовыбор и не подсказка страницы), — владелец субтитров; здесь только отправка
//! и версия протокола.
//!
//! **Ящик «одно место».** Сообщение надёжное, как `play`/`pause`: при полной очереди остаётся
//! и уходит в следующий проход. Неотправленный выбор вытесняется новым (место — вид сообщения
//! в `replacement_slot`): странице важен только последний выбор зрителя.
//!
//! **Версия.** `subtitle_choice` появился в 1.7. Экстеншен прежней версии его не знает и
//! отверг бы сообщение, поэтому со старым экстеншеном выбор не шлётся — с явным итогом
//! [`SubtitleChoiceSendOutcome::ProtocolTooOld`], а не молчаливой потерей.

use browser_bridge_protocol::session::SubtitleChoice;
use browser_bridge_protocol::session::page_subtitles::SUBTITLE_CHOICE_INTRODUCED_IN;
use browser_bridge_protocol::{PlayerSessionMessage, ProtocolVersion};

use super::PageOutbound;
use super::resource_requests::PageProtocol;

/// С какой версии протокола плеер сообщает выбор субтитров (равенство с протоколом
/// закреплено тестом).
pub(crate) const SUBTITLE_CHOICE_SINCE: ProtocolVersion = SUBTITLE_CHOICE_INTRODUCED_IN;

/// Что вышло с сообщением о выборе субтитров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a choice that was not queued is never remembered by the extension"]
pub(crate) enum SubtitleChoiceSendOutcome {
    /// Поставлено (неотправленный прежний выбор вытеснен).
    Queued,
    /// Не поставлено: страница ещё не подключилась (версия неизвестна).
    NotAttachedYet,
    /// Не поставлено: общая версия ниже 1.7 — экстеншен такого сообщения не знает.
    ProtocolTooOld,
}

impl PageOutbound {
    /// Ставит `subtitle_choice{choice}` в надёжную отправку, если общая версия позволяет.
    pub(crate) fn request_subtitle_choice(
        &mut self,
        choice: SubtitleChoice,
    ) -> SubtitleChoiceSendOutcome {
        match self.page_protocol {
            PageProtocol::NotAttachedYet => SubtitleChoiceSendOutcome::NotAttachedYet,
            PageProtocol::Agreed(version) if version.includes(SUBTITLE_CHOICE_SINCE) => {
                self.queue_event_replacing_same_kind(PlayerSessionMessage::SubtitleChoice {
                    choice,
                });
                SubtitleChoiceSendOutcome::Queued
            }
            PageProtocol::Agreed(_) => SubtitleChoiceSendOutcome::ProtocolTooOld,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use browser_bridge_protocol::session::PageSubtitleTrackId;
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

    fn page_track(number: u32) -> SubtitleChoice {
        SubtitleChoice::PageTrack {
            track: PageSubtitleTrackId::new(number).expect("track numbers start at 1"),
        }
    }

    fn attached(version: ProtocolVersion) -> (RecordingSender, PageOutbound) {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        outbound.note_agreed_protocol(version);
        (sender, outbound)
    }

    #[test]
    fn the_constant_matches_the_protocol() {
        assert_eq!(
            SUBTITLE_CHOICE_SINCE,
            PlayerSessionMessage::SubtitleChoice {
                choice: SubtitleChoice::Off {}
            }
            .introduced_in()
        );
    }

    #[test]
    fn at_1_7_only_the_latest_unsent_choice_leaves() {
        let (sender, mut outbound) = attached(ProtocolVersion::V1_7);
        for choice in [page_track(2), SubtitleChoice::Off {}, page_track(3)] {
            assert_eq!(
                outbound.request_subtitle_choice(choice),
                SubtitleChoiceSendOutcome::Queued
            );
        }
        outbound.flush(Instant::now());
        assert_eq!(
            sent(&sender),
            vec![PlayerSessionMessage::SubtitleChoice {
                choice: page_track(3)
            }],
            "one slot: the newest choice replaces the unsent ones"
        );
    }

    #[test]
    fn a_1_6_page_or_an_unattached_one_gets_nothing_and_the_cause_is_told() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        assert_eq!(
            outbound.request_subtitle_choice(SubtitleChoice::Off {}),
            SubtitleChoiceSendOutcome::NotAttachedYet
        );
        outbound.note_agreed_protocol(ProtocolVersion::V1_6);
        assert_eq!(
            outbound.request_subtitle_choice(page_track(1)),
            SubtitleChoiceSendOutcome::ProtocolTooOld
        );
        assert!(!outbound.has_pending());
        outbound.flush(Instant::now());
        assert!(sent(&sender).is_empty());
    }
}
