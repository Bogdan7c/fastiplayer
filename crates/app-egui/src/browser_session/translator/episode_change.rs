//! Смена серии на той же странице в переходнике (протокол 1.9, сессия 16a):
//! `awaiting_next_episode` и `page_title_changed` уходят главному циклу уведомлением
//! [`IntakeNotice::EpisodeChange`] в порядке провода.
//!
//! Вынесено из `translator.rs` (предел 800 строк): там осталась одна строка делегирования.
//! Ни поколений, ни писателей потока эти слова не касаются: «ждём следующую серию» и
//! заголовок вкладки хранит и решает медиа-часть. Текст заголовка в журнал не пишется
//! (инвариант 7): здесь только счётчик.

use browser_bridge_protocol::PageSessionMessage;
use browser_bridge_session::SessionRef;
use tracing::warn;

use super::stream_port::StreamFactory;
use super::{IntakeNotice, ProtocolTranslator};
use crate::browser_session::intake_notice::PageEpisodeWord;

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// `awaiting_next_episode` или `page_title_changed` → уведомление главному циклу.
    pub(super) fn forward_episode_change(
        &mut self,
        session: &SessionRef,
        message: PageSessionMessage,
        out: &mut Vec<IntakeNotice>,
    ) {
        let word = match message {
            PageSessionMessage::AwaitingNextEpisode { patience_ms } => {
                PageEpisodeWord::AwaitingNextEpisode(patience_ms)
            }
            PageSessionMessage::PageTitleChanged { title } => PageEpisodeWord::TitleChanged(title),
            _ => {
                // Сюда приходят только два вида сообщений (так сопоставлен вызов).
                warn!("A page message that is not about an episode change reached its forwarding");
                self.counters.note_ignored_message();
                return;
            }
        };
        self.counters.episode_change_words += 1;
        out.push(IntakeNotice::EpisodeChange {
            session: session.clone(),
            word,
        });
    }
}
