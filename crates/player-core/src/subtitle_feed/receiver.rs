//! Потребитель трубы субтитров (UI-поток приложения).

use std::sync::Arc;

use crossbeam_channel::{Receiver, TryRecvError};

use super::SubtitleFeedMessage;
use super::stats::SubtitleFeedStats;

/// Единственный потребитель трубы; не клонируется (single-consumer).
#[derive(Debug)]
pub struct SubtitleFeedReceiver {
    /// Очередь сообщений.
    message_rx: Receiver<SubtitleFeedMessage>,

    /// Счётчики потерь, общие с производителем.
    stats: Arc<SubtitleFeedStats>,
}

impl SubtitleFeedReceiver {
    pub(super) fn new(
        message_rx: Receiver<SubtitleFeedMessage>,
        stats: Arc<SubtitleFeedStats>,
    ) -> Self {
        Self { message_rx, stats }
    }

    /// Дописывает в `out` не более `max_messages` сообщений в порядке отправки.
    ///
    /// Не блокирует и не аллоцирует, если очередь пуста. Возвращает число добавленных.
    pub fn drain_into(&self, out: &mut Vec<SubtitleFeedMessage>, max_messages: usize) -> usize {
        let mut drained = 0usize;
        while drained < max_messages {
            match self.message_rx.try_recv() {
                Ok(message) => {
                    out.push(message);
                    drained += 1;
                }
                // Пусто или производитель завершился: больше сообщений не будет.
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        drained
    }

    /// Сколько пакетов выброшено из-за переполнения за всё время.
    #[must_use]
    pub fn dropped_packets(&self) -> u64 {
        self.stats.dropped_packets()
    }

    /// Сколько сообщений «начало media» (то есть вложенных шрифтов) потеряно.
    #[must_use]
    pub fn dropped_media_starts(&self) -> u64 {
        self.stats.dropped_media_starts()
    }
}
