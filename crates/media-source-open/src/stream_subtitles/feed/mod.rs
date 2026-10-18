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
mod outage;
mod position;
mod schedule;
mod worker;

#[cfg(test)]
mod schedule_tests;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use source_core::CancellationToken;

pub use config::StreamSubtitleFeedConfig;
pub use message::{
    StreamSegmentSkipReason, StreamSubtitleEvents, StreamSubtitleMessage,
    StreamTrackUnavailableReason,
};
pub use position::StreamPlaybackPosition;

use super::model::{StreamSubtitleSource, StreamSubtitleTrackId};
#[cfg(test)]
pub(crate) use outage::OutagePolicy;
use worker::{FeedWorker, WorkerExit};

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
    /// Поток закончил штатно (см. `WorkerExit`): закрытый канал тогда не авария.
    worker_exit: WorkerExit,
    /// `WorkerLost` уже выдан: сообщение приходит ровно один раз.
    worker_lost_reported: AtomicBool,
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
        Self::launch(source, track, config, position, wake, |worker| worker)
    }

    /// То же, что [`start`](Self::start), но с заданной политикой повторов при сетевом перебое.
    #[cfg(test)]
    pub(crate) fn start_with_outage_policy(
        source: &StreamSubtitleSource,
        track: StreamSubtitleTrackId,
        config: StreamSubtitleFeedConfig,
        position: StreamPlaybackPosition,
        wake: Arc<dyn Fn() + Send + Sync>,
        outage_policy: OutagePolicy,
    ) -> Result<Self, StreamSubtitleFeedStartError> {
        Self::launch(source, track, config, position, wake, |worker| {
            worker.with_outage_policy(outage_policy)
        })
    }

    /// Общий запуск: собирает рабочего, даёт `adjust_worker` его подправить и запускает поток.
    fn launch(
        source: &StreamSubtitleSource,
        track: StreamSubtitleTrackId,
        config: StreamSubtitleFeedConfig,
        position: StreamPlaybackPosition,
        wake: Arc<dyn Fn() + Send + Sync>,
        adjust_worker: impl FnOnce(FeedWorker) -> FeedWorker,
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
        let worker = adjust_worker(worker);
        let worker_exit = WorkerExit::default();
        let thread_exit = worker_exit.clone();
        // Дескриптор потока не храним: поток самозавершается по отмене (см. `Drop`), а его
        // аварийную гибель приложение узнаёт по закрытому каналу без отметки `worker_exit`.
        std::thread::Builder::new()
            .name(FEED_THREAD_NAME.to_owned())
            .spawn(move || worker.run(&thread_exit))
            .map_err(|error| StreamSubtitleFeedStartError::ThreadSpawn(error.kind()))?;
        Ok(Self {
            track,
            cancellation,
            position,
            messages: Mutex::new(receiver),
            worker_exit,
            worker_lost_reported: AtomicBool::new(false),
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
        match receiver.try_recv() {
            Ok(message) => Some(message),
            Err(TryRecvError::Empty) => None,
            // Канал закрыт и пуст: поток закончил.
            Err(TryRecvError::Disconnected) => self.lost_worker_signal(),
        }
    }

    /// Закрытый канал: штатное завершение и отмена молчат, паника потока - один раз
    /// `TrackUnavailable(WorkerLost)`, иначе дорожка навсегда осталась бы «готовой» без реплик.
    fn lost_worker_signal(&self) -> Option<StreamSubtitleMessage> {
        if self.worker_exit.finished_cleanly() || self.cancellation.is_cancelled() {
            return None;
        }
        if self.worker_lost_reported.swap(true, Ordering::SeqCst) {
            return None;
        }
        tracing::error!("stream subtitle feed thread died unexpectedly");
        Some(StreamSubtitleMessage::TrackUnavailable(
            StreamTrackUnavailableReason::WorkerLost,
        ))
    }

    /// Забирает все накопленные сообщения (не блокирует).
    #[must_use]
    pub fn drain_messages(&self) -> Vec<StreamSubtitleMessage> {
        std::iter::from_fn(|| self.try_next_message()).collect()
    }

    /// Закончил ли поток ленты штатно (только для тестов отмены и аварий).
    #[cfg(test)]
    pub(crate) fn worker_finished_cleanly(&self) -> bool {
        self.worker_exit.finished_cleanly()
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
