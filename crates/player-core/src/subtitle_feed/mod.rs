//! Труба встроенных субтитров: `PlayerSession` (поток worker-а) -> UI-поток приложения.
//!
//! Пакеты субтитров идут мимо декодеров, поэтому у них свой канал. Контракт:
//!
//! - **Один производитель, один потребитель.** Пишет только session на потоке worker-а,
//!   читает только приложение (получает [`SubtitleFeedReceiver`] один раз у `PlayerWorker`).
//! - **Запись никогда не ждёт.** Очередь ограничена ([`SubtitleFeedCapacity`]); при
//!   переполнении сообщение выбрасывается, растёт счётчик и пишется редкий `warn!`.
//!   Видео и аудио от этого не страдают: труба не участвует в backpressure demux-а.
//! - **Канал сообщений не lossy-event канал плеера.** Это отдельный канал: события
//!   плеера могут теряться, а реплики и шрифты терять нельзя без счётчика.
//! - **Время.** [`SubtitleFeedPacket::presentation_time`] — это `Packet::pts` демуксера
//!   без изменений. Ровно этот же `Packet::pts` session кладёт в `PendingVideoPacket` и
//!   `PlayerDecodePacket`, а декодер возвращает его как `DecodedFrame::pts`, которым
//!   приложение ищет реплику (`SubmittedFrameForSubtitles::frame_pts`). Значит, время
//!   реплики и время кадра находятся в одной (абсолютной, source) временной шкале.
//! - **Seek не имеет «поколения».** После seek назад те же пакеты приходят снова;
//!   дедупликация — на стороне приложения (время реплики абсолютное).
//! - **Все дорожки.** Пакеты всех субтитровых дорожек пересылаются независимо от
//!   выбранной (решение владельца: мгновенное переключение дорожек).

mod message;
mod receiver;
mod sender;
mod stats;
#[cfg(test)]
mod tests;
mod track_headers;

pub use message::{SubtitleFeedMediaStart, SubtitleFeedMessage, SubtitleFeedPacket};
pub use receiver::SubtitleFeedReceiver;
#[cfg(test)]
pub(crate) use sender::SubtitleFeedSendOutcome;
pub(crate) use sender::SubtitleFeedSender;
pub use track_headers::SubtitleTrackHeader;
pub(crate) use track_headers::collect_subtitle_track_headers;

use std::num::NonZeroUsize;
use std::sync::Arc;

use crossbeam_channel::bounded;

use stats::SubtitleFeedStats;

/// Ёмкость трубы в сообщениях; ноль невозможен по построению.
///
/// Реальное значение приходит из конфига приложения (`PlayerWorkerConfig::with_subtitle_feed_capacity`).
/// [`Self::FALLBACK`] нужен только конструкторам конфига, пока приложение его не задало.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtitleFeedCapacity(NonZeroUsize);

impl SubtitleFeedCapacity {
    /// Запасное значение для `PlayerWorkerConfig::new/default` (4096 сообщений).
    ///
    /// Приложение обязано переопределить его значением из своего конфига; константа
    /// существует лишь затем, чтобы worker можно было создать без app-конфига (тесты).
    pub const FALLBACK: Self = Self(NonZeroUsize::MIN.saturating_add(4_095));

    /// Создаёт ёмкость из числа сообщений; `None` для нуля.
    #[must_use]
    pub const fn from_messages(messages: usize) -> Option<Self> {
        match NonZeroUsize::new(messages) {
            Some(messages) => Some(Self(messages)),
            None => None,
        }
    }

    /// Ёмкость в сообщениях.
    #[must_use]
    pub const fn messages(self) -> usize {
        self.0.get()
    }
}

impl Default for SubtitleFeedCapacity {
    fn default() -> Self {
        Self::FALLBACK
    }
}

/// Создаёт пару (производитель для session, потребитель для приложения).
#[must_use]
pub(crate) fn subtitle_feed(
    capacity: SubtitleFeedCapacity,
) -> (SubtitleFeedSender, SubtitleFeedReceiver) {
    let (message_tx, message_rx) = bounded(capacity.messages());
    let stats = Arc::new(SubtitleFeedStats::default());
    (
        SubtitleFeedSender::new(message_tx, Arc::clone(&stats)),
        SubtitleFeedReceiver::new(message_rx, stats),
    )
}
