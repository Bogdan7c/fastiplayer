//! Варианты страницы в переходнике (14a): `variant_menu_changed` (протокол 1.0),
//! `page_navigation` и `subtitle_preference` (1.7) идут главному циклу уведомлением
//! [`IntakeNotice::PageVariants`].
//!
//! Вынесено из `translator.rs` (предел 800 строк): там осталась одна строка делегирования.
//! Ни поколений, ни писателей потока эти сообщения не касаются: решает и хранит склад
//! вариантов медиа-части (`browser_session::page_variants`). Сведения — только меню и
//! «следующая серия есть»; названия сериала и номера серии в протоколе нет (решение 283).
//! Подсказка субтитров едет тем же путём (у неё та же природа — слово страницы вне поколений,
//! важно только последнее), а медиа-часть кладёт её на склад субтитров сеанса.

use browser_bridge_protocol::PageSessionMessage;
use browser_bridge_session::SessionRef;
use tracing::warn;

use super::stream_port::StreamFactory;
use super::{IntakeNotice, ProtocolTranslator};
use crate::browser_session::page_variants::PageVariantUpdate;

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// `variant_menu_changed`, `page_navigation` или `subtitle_preference` → уведомление
    /// главному циклу.
    pub(super) fn forward_page_variants(
        &mut self,
        session: &SessionRef,
        message: PageSessionMessage,
        out: &mut Vec<IntakeNotice>,
    ) {
        let update = match message {
            PageSessionMessage::VariantMenuChanged { menu } => PageVariantUpdate::Menu(menu),
            PageSessionMessage::PageNavigation { next_episode } => {
                PageVariantUpdate::Navigation(next_episode)
            }
            PageSessionMessage::SubtitlePreference { choice } => {
                PageVariantUpdate::SubtitlePreference(choice)
            }
            _ => {
                // Сюда приходят только три вида сообщений (так сопоставлен вызов).
                warn!("A page message that is not about page variants reached their forwarding");
                self.counters.ignored_messages += 1;
                return;
            }
        };
        self.counters.page_variant_words += 1;
        out.push(IntakeNotice::PageVariants {
            session: session.clone(),
            update,
        });
    }
}
