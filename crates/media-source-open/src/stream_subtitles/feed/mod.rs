//! `StreamSubtitleFeed` - фоновая лента субтитров одной дорожки потокового источника.
//!
//! Лента живёт в отдельном именованном потоке: качает индекс и сегменты дорожки вокруг
//! позиции воспроизведения, разбирает их через транспорт (реплики сразу на шкале плеера)
//! и присылает приложению сообщения по ограниченной очереди. Видео ленту не ждёт,
//! ошибки ленты воспроизведение не останавливают.
//!
//! Владение и границы:
//! - приложение владеет [`StreamSubtitleFeed`], [`StreamPlaybackPosition`] и колбэком `wake`;
//! - поток владеет состоянием планирования (`worker`), транспорт - сетью и форматом;
//! - закрытие: `Drop` только переворачивает токен и будит поток, НЕ ждёт его (запрос,
//!   уже ушедший в сеть, завершится сам; результат никому не нужен).

mod config;
mod message;
mod position;
mod schedule;
mod worker;

#[cfg(test)]
mod schedule_tests;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, sync_channel};

use source_core::CancellationToken;

pub use config::StreamSubtitleFeedConfig;
pub use message::{
    StreamSegmentSkipReason, StreamSubtitleEvents, StreamSubtitleMessage,
    StreamTrackUnavailableReason,
};
pub use position::StreamPlaybackPosition;

use super::model::{StreamSubtitleSource, StreamSubtitleTrackId};
use worker::FeedWorker;

/// Имя фонового потока ленты (видно в отладчике и профайлере).
const FEED_THREAD_NAME: &str = "stream-subtitle-feed";

/// Не удалось запустить ленту.
#[derive(Debug, thiserror::Error)]
pub enum StreamSubtitleFeedStartError {
    /// У источника нет такой дорожки.
    #[error("subtitle track is not part of this stream source")]
    UnknownTrack,
    /// Система не создала поток.
    #[error("subtitle feed thread could not be started: {0}")]
    ThreadSpawn(std::io::ErrorKind),
}

/// Работающая лента субтитров.
pub struct StreamSubtitleFeed {
    track: StreamSubtitleTrackId,
    cancellation: CancellationToken,
    position: StreamPlaybackPosition,
    /// `Receiver` не `Sync`; мьютекс делает ленту безопасной для хранения в общем состоянии.
    messages: Mutex<Receiver<StreamSubtitleMessage>>,
}

impl StreamSubtitleFeed {
    /// Запускает ленту дорожки.
    ///
    /// `position` - ячейка, в которую приложение пишет позицию поиска реплик (шкала плеера).
    /// `wake` вызывается из потока ленты после каждого сообщения: приложение будит интерфейс,
    /// даже если воспроизведение стоит на паузе. Колбэк должен быть быстрым и не блокировать.
    pub fn start(
        source: &StreamSubtitleSource,
        track: StreamSubtitleTrackId,
        config: StreamSubtitleFeedConfig,
        position: StreamPlaybackPosition,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, StreamSubtitleFeedStartError> {
        if source.track(track).is_none() {
            return Err(StreamSubtitleFeedStartError::UnknownTrack);
        }
        let cancellation = CancellationToken::new();
        let (sender, receiver) = sync_channel(config.message_capacity.get());
        let worker = FeedWorker::new(
            Arc::clone(source.transport()),
            track,
            config,
            position.clone(),
            wake,
            cancellation.clone(),
            sender,
        );
        // Дескриптор потока не храним: поток самозавершается по отмене (см. `Drop`).
        std::thread::Builder::new()
            .name(FEED_THREAD_NAME.to_owned())
            .spawn(move || worker.run())
            .map_err(|error| StreamSubtitleFeedStartError::ThreadSpawn(error.kind()))?;
        Ok(Self {
            track,
            cancellation,
            position,
            messages: Mutex::new(receiver),
        })
    }

    /// Дорожка, которую качает лента.
    #[must_use]
    pub const fn track(&self) -> StreamSubtitleTrackId {
        self.track
    }

    /// Следующее сообщение, если оно уже есть (не блокирует).
    #[must_use]
    pub fn try_next_message(&self) -> Option<StreamSubtitleMessage> {
        let receiver = self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Пусто и «поток уже завершился» для приложения одно и то же: сообщений нет.
        receiver.try_recv().ok()
    }

    /// Забирает все накопленные сообщения (не блокирует).
    #[must_use]
    pub fn drain_messages(&self) -> Vec<StreamSubtitleMessage> {
        std::iter::from_fn(|| self.try_next_message()).collect()
    }

    /// Останавливает ленту (идемпотентно, не блокирует). Сообщения, уже лежащие в очереди, остаются.
    pub fn cancel(&self) {
        self.cancellation.cancel();
        self.position.wake_waiters();
    }
}

impl Drop for StreamSubtitleFeed {
    /// Закрытие ленты не ждёт поток: токен отмены и пробуждение, остальное поток делает сам.
    fn drop(&mut self) {
        self.cancel();
    }
}

impl std::fmt::Debug for StreamSubtitleFeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamSubtitleFeed")
            .field("track", &self.track)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}
