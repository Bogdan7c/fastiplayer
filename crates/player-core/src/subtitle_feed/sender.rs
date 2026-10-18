//! Производитель трубы субтитров (поток worker-а, внутри `PlayerSession`).

use std::sync::Arc;

use crossbeam_channel::{Sender, TrySendError, bounded};
use tracing::warn;

use super::stats::SubtitleFeedStats;
use super::{SubtitleFeedMediaStart, SubtitleFeedMessage, SubtitleFeedPacket, SubtitleReadRun};

/// Каждое N-е переполнение (и самое первое) попадает в лог; payload в лог не пишется.
const OVERFLOW_LOG_EVERY: u64 = 256;

/// Итог неблокирующей отправки; вызывающий различает три разных ситуации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleFeedSendOutcome {
    /// Сообщение лежит в очереди.
    Delivered,

    /// Очередь полна: сообщение выброшено, счётчик увеличен.
    DroppedFull,

    /// Получателя нет (приложение не подключено или уже закрыто): сообщение выброшено
    /// без счётчика и без лога.
    ReceiverGone,
}

/// Производитель трубы. Никогда не блокируется.
#[derive(Debug)]
pub(crate) struct SubtitleFeedSender {
    /// Очередь сообщений.
    message_tx: Sender<SubtitleFeedMessage>,

    /// Счётчики потерь, общие с потребителем.
    stats: Arc<SubtitleFeedStats>,

    /// Текущий проход чтения демуксера; session ставит его на пакеты и двигает при seek.
    read_run: SubtitleReadRun,
}

impl SubtitleFeedSender {
    pub(super) fn new(
        message_tx: Sender<SubtitleFeedMessage>,
        stats: Arc<SubtitleFeedStats>,
    ) -> Self {
        Self {
            message_tx,
            stats,
            read_run: SubtitleReadRun::INITIAL,
        }
    }

    /// Труба без получателя: все отправки — `ReceiverGone`. Для session без приложения.
    #[must_use]
    pub(crate) fn disabled() -> Self {
        let (message_tx, _message_rx) = bounded(1);
        Self::new(message_tx, Arc::new(SubtitleFeedStats::default()))
    }

    /// Проход чтения, который session должен поставить на следующий пакет.
    #[must_use]
    pub(crate) const fn current_read_run(&self) -> SubtitleReadRun {
        self.read_run
    }

    /// Позиция чтения демуксера прыгнула: все следующие пакеты принадлежат новому проходу.
    pub(crate) fn begin_new_read_run(&mut self) -> SubtitleReadRun {
        self.read_run = self.read_run.next();
        self.read_run
    }

    /// Отправляет «начало media» с вложениями.
    pub(crate) fn try_send_media_start(
        &self,
        media_start: SubtitleFeedMediaStart,
    ) -> SubtitleFeedSendOutcome {
        let outcome = self.try_send(SubtitleFeedMessage::MediaStarted(media_start));
        if outcome == SubtitleFeedSendOutcome::DroppedFull {
            let total = self.stats.record_dropped_media_start();
            // Потеря «начала media» теряет вложенные шрифты: логируем всегда.
            warn!(
                dropped_media_starts = total,
                "Subtitle feed is full: media start (attached fonts) dropped"
            );
        }
        outcome
    }

    /// Отправляет пакет встроенной дорожки. При переполнении очереди пакет теряется и
    /// проход чтения двигается (см. тело метода).
    pub(crate) fn try_send_packet(
        &mut self,
        packet: SubtitleFeedPacket,
    ) -> SubtitleFeedSendOutcome {
        let outcome = self.try_send(SubtitleFeedMessage::Packet(packet));
        match outcome {
            SubtitleFeedSendOutcome::DroppedFull => {
                // Потерянный пакет = дыра в потоке: потребитель не должен считать, что
                // следующий пакет продолжает прежний проход без пропусков. Поэтому
                // ближайший принятый пакет начинает НОВЫЙ проход (для потребителя это
                // разрыв, как после seek), а снимок плеера показывает новый номер.
                self.begin_new_read_run();
                let total = self.stats.record_dropped_packet();
                if total.saturating_sub(1).is_multiple_of(OVERFLOW_LOG_EVERY) {
                    warn!(
                        dropped_packets = total,
                        "Subtitle feed is full: embedded subtitle packets dropped"
                    );
                }
            }
            // Нет получателя — штатная ситуация (например, session без приложения): молча.
            SubtitleFeedSendOutcome::ReceiverGone | SubtitleFeedSendOutcome::Delivered => {}
        }
        outcome
    }

    /// Общая неблокирующая отправка без учёта счётчиков.
    fn try_send(&self, message: SubtitleFeedMessage) -> SubtitleFeedSendOutcome {
        match self.message_tx.try_send(message) {
            Ok(()) => SubtitleFeedSendOutcome::Delivered,
            Err(TrySendError::Full(_)) => SubtitleFeedSendOutcome::DroppedFull,
            Err(TrySendError::Disconnected(_)) => SubtitleFeedSendOutcome::ReceiverGone,
        }
    }
}
