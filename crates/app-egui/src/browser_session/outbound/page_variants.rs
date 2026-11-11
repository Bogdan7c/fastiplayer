//! `select_variant` и `next_episode` (протокол 1.0, правила 14a): просьбы плеера к адаптеру
//! страницы переключить озвучку или качество и перейти к следующей серии.
//!
//! Сообщения надёжные, как `play`/`pause`: при полной очереди остаются и уходят в следующий
//! проход. **Ящик «один на вид»:** неотправленный `select_variant` вытесняется новым выбором
//! того же вида (озвучка вытесняет озвучку, качество — качество), а не другого вида: два
//! быстрых выбора озвучки дают странице один, последний.
//!
//! Что можно выбрать — знает склад вариантов медиа-части (меню, ревизии): здесь только
//! отправка и версия протокола. `select_variant` есть с 1.0, отдельной проверки версии у него
//! нет. `next_episode` тоже с 1.0, но по договору 14a шлётся только после
//! `page_navigation{available}` (1.7): со старым экстеншеном кнопки Next нет, и
//! [`PageOutbound::request_next_episode`] — страховка с явным итогом.

use browser_bridge_protocol::session::VariantKind;
use browser_bridge_protocol::text::VariantId;
use browser_bridge_protocol::{PlayerSessionMessage, ProtocolVersion};

use super::PageOutbound;
use super::resource_requests::PageProtocol;

/// С какой версии протокола плеер предлагает следующую серию: с той, где страница умеет
/// сказать `page_navigation` (равенство закреплено тестом).
pub(crate) const NEXT_EPISODE_OFFERED_SINCE: ProtocolVersion = ProtocolVersion::V1_7;

/// С какой версии протокола страница сама сообщает исход выбора зрителя
/// (`variant_outcome`, 15c2). Равенство со сторожем протокола закреплено тестом.
pub(crate) const VARIANT_OUTCOME_EXPECTED_SINCE: ProtocolVersion = ProtocolVersion::V1_8;

/// Сообщит ли страница исход выбора зрителя (чистый вопрос о версии).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VariantOutcomeReporting {
    /// Общая версия 1.8 и новее: на каждый выбор зрителя придёт `variant_outcome`.
    PageReports,
    /// Исхода не будет: страница ещё не подключилась или экстеншен старше 1.8. Плеер ведёт
    /// себя как в 1.7 (сразу «Audio: …»).
    PageStaysSilent,
}

/// Можно ли по версии протокола просить следующую серию (чистый вопрос).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextEpisodePermission {
    /// Можно: страница говорит на 1.7 и новее.
    Allowed,
    /// Страница ещё не подключилась (версия неизвестна).
    NotAttachedYet,
    /// Общая версия ниже 1.7: экстеншен не умеет `page_navigation`.
    ProtocolTooOld,
}

/// Что вышло с `select_variant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a selection that was not queued never reaches the page"]
pub(crate) enum SelectVariantSendOutcome {
    /// Поставлен (неотправленный выбор того же вида вытеснен).
    Queued,
    /// Не поставлен: страница ещё не подключилась.
    NotAttachedYet,
}

/// Что вышло с `next_episode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a request that was not queued never reaches the page"]
pub(crate) enum NextEpisodeSendOutcome {
    /// Поставлен.
    Queued,
    /// Не поставлен: страница ещё не подключилась.
    NotAttachedYet,
    /// Не поставлен: общая версия ниже 1.7.
    ProtocolTooOld,
}

impl PageOutbound {
    /// Можно ли по общей версии протокола просить следующую серию.
    pub(crate) const fn next_episode_permission(&self) -> NextEpisodePermission {
        match self.page_protocol {
            PageProtocol::NotAttachedYet => NextEpisodePermission::NotAttachedYet,
            PageProtocol::Agreed(version) if version.includes(NEXT_EPISODE_OFFERED_SINCE) => {
                NextEpisodePermission::Allowed
            }
            PageProtocol::Agreed(_) => NextEpisodePermission::ProtocolTooOld,
        }
    }

    /// Пришлёт ли страница исход выбора зрителя (по общей версии протокола).
    pub(crate) const fn variant_outcome_reporting(&self) -> VariantOutcomeReporting {
        match self.page_protocol {
            PageProtocol::Agreed(version) if version.includes(VARIANT_OUTCOME_EXPECTED_SINCE) => {
                VariantOutcomeReporting::PageReports
            }
            PageProtocol::NotAttachedYet | PageProtocol::Agreed(_) => {
                VariantOutcomeReporting::PageStaysSilent
            }
        }
    }

    /// Ставит `select_variant{kind, variant_id}` в надёжную отправку; неотправленный выбор
    /// того же вида вытесняется (место — `replacement_slot`).
    pub(crate) fn request_select_variant(
        &mut self,
        kind: VariantKind,
        variant_id: VariantId,
    ) -> SelectVariantSendOutcome {
        if matches!(self.page_protocol, PageProtocol::NotAttachedYet) {
            return SelectVariantSendOutcome::NotAttachedYet;
        }
        self.queue_event_replacing_same_kind(PlayerSessionMessage::SelectVariant {
            kind,
            variant_id,
        });
        SelectVariantSendOutcome::Queued
    }

    /// Ставит `next_episode{}` в надёжную отправку (если общая версия позволяет).
    pub(crate) fn request_next_episode(&mut self) -> NextEpisodeSendOutcome {
        match self.next_episode_permission() {
            NextEpisodePermission::Allowed => {
                self.queue_event_replacing_same_kind(PlayerSessionMessage::NextEpisode {});
                NextEpisodeSendOutcome::Queued
            }
            NextEpisodePermission::NotAttachedYet => NextEpisodeSendOutcome::NotAttachedYet,
            NextEpisodePermission::ProtocolTooOld => NextEpisodeSendOutcome::ProtocolTooOld,
        }
    }
}

/// Место неотправленного `select_variant` в очереди: своё у каждого вида меню.
pub(super) const fn select_variant_slot(kind: VariantKind) -> &'static str {
    match kind {
        VariantKind::Audio => "select_variant.audio",
        VariantKind::Quality => "select_variant.quality",
        VariantKind::Subtitles => "select_variant.subtitles",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use browser_bridge_protocol::session::page_navigation::PAGE_NAVIGATION_INTRODUCED_IN;
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

    fn id(text: &str) -> VariantId {
        VariantId::new(text).expect("variant id")
    }

    fn select(kind: VariantKind, variant: &str) -> PlayerSessionMessage {
        PlayerSessionMessage::SelectVariant {
            kind,
            variant_id: id(variant),
        }
    }

    fn attached(version: ProtocolVersion) -> (RecordingSender, PageOutbound) {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        outbound.note_agreed_protocol(version);
        (sender, outbound)
    }

    #[test]
    fn the_constant_matches_the_page_navigation_message() {
        assert_eq!(NEXT_EPISODE_OFFERED_SINCE, PAGE_NAVIGATION_INTRODUCED_IN);
    }

    #[test]
    fn the_outcome_gate_matches_the_protocol_message() {
        assert_eq!(
            VARIANT_OUTCOME_EXPECTED_SINCE,
            browser_bridge_protocol::session::VARIANT_OUTCOME_INTRODUCED_IN
        );
    }

    #[test]
    fn the_page_reports_outcomes_only_from_1_8_on() {
        let before_attach = PageOutbound::new(Box::new(RecordingSender::default()));
        assert_eq!(
            before_attach.variant_outcome_reporting(),
            VariantOutcomeReporting::PageStaysSilent
        );
        for (version, expected) in [
            (
                ProtocolVersion::V1_7,
                VariantOutcomeReporting::PageStaysSilent,
            ),
            (ProtocolVersion::V1_8, VariantOutcomeReporting::PageReports),
        ] {
            let (_sender, outbound) = attached(version);
            assert_eq!(
                outbound.variant_outcome_reporting(),
                expected,
                "{version:?}"
            );
        }
    }

    #[test]
    fn a_newer_selection_replaces_the_unsent_one_of_the_same_kind_only() {
        let (sender, mut outbound) = attached(ProtocolVersion::V1_7);
        for (kind, variant) in [
            (VariantKind::Audio, "orig"),
            (VariantKind::Quality, "720"),
            (VariantKind::Audio, "dub"),
        ] {
            assert_eq!(
                outbound.request_select_variant(kind, id(variant)),
                SelectVariantSendOutcome::Queued
            );
        }
        outbound.flush(Instant::now());
        assert_eq!(
            sent(&sender),
            vec![
                select(VariantKind::Quality, "720"),
                select(VariantKind::Audio, "dub")
            ],
            "one selection per kind, the newest one"
        );
    }

    #[test]
    fn selections_work_with_an_old_extension_but_not_before_attach() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        assert_eq!(
            outbound.request_select_variant(VariantKind::Audio, id("dub")),
            SelectVariantSendOutcome::NotAttachedYet
        );
        assert!(!outbound.has_pending());
        // `select_variant` есть с 1.0: меню вариантов работает и со старым экстеншеном.
        outbound.note_agreed_protocol(ProtocolVersion::V1_6);
        assert_eq!(
            outbound.request_select_variant(VariantKind::Audio, id("dub")),
            SelectVariantSendOutcome::Queued
        );
        outbound.flush(Instant::now());
        assert_eq!(sent(&sender), vec![select(VariantKind::Audio, "dub")]);
    }

    #[test]
    fn at_1_6_there_is_no_next_episode_and_nothing_is_queued() {
        let (sender, mut outbound) = attached(ProtocolVersion::V1_6);
        assert_eq!(
            outbound.next_episode_permission(),
            NextEpisodePermission::ProtocolTooOld
        );
        assert_eq!(
            outbound.request_next_episode(),
            NextEpisodeSendOutcome::ProtocolTooOld
        );
        outbound.flush(Instant::now());
        assert!(sent(&sender).is_empty());
        assert!(!outbound.has_pending());
    }

    #[test]
    fn at_1_7_next_episode_goes_to_the_page_once_per_flush() {
        let (sender, mut outbound) = attached(ProtocolVersion::V1_7);
        assert_eq!(
            outbound.next_episode_permission(),
            NextEpisodePermission::Allowed
        );
        assert_eq!(
            outbound.request_next_episode(),
            NextEpisodeSendOutcome::Queued
        );
        assert_eq!(
            outbound.request_next_episode(),
            NextEpisodeSendOutcome::Queued
        );
        outbound.flush(Instant::now());
        assert_eq!(sent(&sender), vec![PlayerSessionMessage::NextEpisode {}]);
    }

    #[test]
    fn before_attach_next_episode_is_refused_with_its_cause() {
        let sender = RecordingSender::default();
        let mut outbound = PageOutbound::new(Box::new(sender.clone()));
        assert_eq!(
            outbound.request_next_episode(),
            NextEpisodeSendOutcome::NotAttachedYet
        );
        assert!(!outbound.has_pending());
    }
}
