use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crossbeam_channel::{Receiver, RecvError, RecvTimeoutError, Sender, TrySendError, bounded};

use super::protocol::DecodeThreadError;
/// Monotonic номер последней активности decoder thread-а.
///
/// Epoch отделён от pulse channel-а: bounded pulse может coalesce-иться, но
/// caller всё равно видит последнюю активность через атомарный счётчик.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VideoDecoderActivityEpoch(u64);

impl VideoDecoderActivityEpoch {
    /// Начальный epoch до первой активности decoder thread-а.
    pub const INITIAL: Self = Self(0);

    /// Создаёт epoch из сохранённого числового значения.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает числовое значение для diagnostics/runtime state.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Проверяет, что этот epoch новее ранее observed epoch-а.
    #[must_use]
    pub const fn is_after(self, observed_epoch: Self) -> bool {
        self.0 > observed_epoch.0
    }
}

/// Typed причина, почему decoder activity notifier сейчас недоступен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDecoderActivityUnavailableReason {
    /// Backend/handle ещё не реализует activity notifier contract.
    UnsupportedNotifier,

    /// Pulse sender отключён; caller должен выключить этот source до замены backend-а.
    DisconnectedNotifier,

    /// Backend сообщил fatal состояние notifier-а без раскрытия backend-specific канала.
    FatalNotifier(DecodeThreadError),
}

impl std::fmt::Display for VideoDecoderActivityUnavailableReason {
    /// Печатает причину так, чтобы worker diagnostics могли отличать fallback paths.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedNotifier => {
                formatter.write_str("decoder activity notifier is unsupported")
            }
            Self::DisconnectedNotifier => {
                formatter.write_str("decoder activity notifier is disconnected")
            }
            Self::FatalNotifier(error) => {
                write!(formatter, "decoder activity notifier failed: {error}")
            }
        }
    }
}

/// Result ожидания decoder activity без схлопывания timeout/disconnect/stale pulse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDecoderActivityWaitOutcome {
    /// Epoch продвинулся относительно observed epoch-а.
    ActivityReceived {
        /// Последний известный epoch после activity.
        epoch: VideoDecoderActivityEpoch,
    },

    /// Pulse был получен, но epoch уже был known caller-у.
    NoNewActivityAfterEpoch {
        /// Epoch, относительно которого caller ждал новую activity.
        observed_epoch: VideoDecoderActivityEpoch,

        /// Текущий epoch после drain-а stale/coalesced pulse.
        current_epoch: VideoDecoderActivityEpoch,
    },

    /// Новая activity не появилась до fallback deadline-а.
    Timeout {
        /// Epoch, относительно которого caller ждал новую activity.
        observed_epoch: VideoDecoderActivityEpoch,

        /// Текущий epoch на момент timeout-а.
        current_epoch: VideoDecoderActivityEpoch,
    },

    /// Activity source недоступен; caller должен использовать bounded fallback poll.
    Unavailable {
        /// Typed причина недоступности notifier-а.
        reason: VideoDecoderActivityUnavailableReason,
    },
}

/// Неблокирующая сторона decoder thread-а, которая публикует coalesced activity pulses.
#[derive(Clone)]
pub struct VideoDecoderActivityNotifier {
    /// Общий monotonic epoch, который не теряется при coalescing pulse channel-а.
    shared_epoch: Arc<AtomicU64>,

    /// Bounded pulse channel capacity=1, чтобы decoder thread никогда не копил очередь.
    pulse_tx: Sender<()>,
}

impl VideoDecoderActivityNotifier {
    /// Создаёт связанную пару notifier/subscription для одного decoder activity source-а.
    #[must_use]
    pub fn new() -> (Self, VideoDecoderActivitySubscription) {
        let shared_epoch = Arc::new(AtomicU64::new(VideoDecoderActivityEpoch::INITIAL.get()));
        let (pulse_tx, pulse_rx) = bounded(1);
        (
            Self {
                shared_epoch: Arc::clone(&shared_epoch),
                pulse_tx,
            },
            VideoDecoderActivitySubscription {
                shared_epoch,
                pulse_rx,
            },
        )
    }

    /// Публикует activity без блокировки decoder thread-а.
    ///
    /// Epoch всегда продвигается до попытки отправить pulse. Если bounded pulse
    /// channel уже полон, новый pulse coalesce-ится: caller всё равно увидит
    /// последний epoch через snapshot/check contract.
    #[must_use]
    pub fn notify_activity(&self) -> VideoDecoderActivityEpoch {
        let activity_epoch = self.advance_epoch();
        match self.pulse_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
        }
        activity_epoch
    }

    /// Атомарно продвигает epoch без wraparound-а.
    fn advance_epoch(&self) -> VideoDecoderActivityEpoch {
        loop {
            let current_epoch = self.shared_epoch.load(Ordering::Acquire);
            let next_epoch = current_epoch.saturating_add(1);
            if next_epoch == current_epoch {
                return VideoDecoderActivityEpoch::new(current_epoch);
            }
            match self.shared_epoch.compare_exchange(
                current_epoch,
                next_epoch,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return VideoDecoderActivityEpoch::new(next_epoch),
                Err(_) => continue,
            }
        }
    }
}

impl std::fmt::Debug for VideoDecoderActivityNotifier {
    /// Печатает только stable diagnostics, не раскрывая внутренние channel handles.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoDecoderActivityNotifier")
            .field("current_epoch", &self.shared_epoch.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

/// Подписка worker/caller-а на нейтральные decoder activity pulses.
#[derive(Clone)]
pub struct VideoDecoderActivitySubscription {
    /// Общий monotonic epoch, читаемый перед select и после pulse wakeup-а.
    shared_epoch: Arc<AtomicU64>,

    /// Receiver coalesced pulse channel-а; clone используется только для neutral activity wait.
    pulse_rx: Receiver<()>,
}

impl VideoDecoderActivitySubscription {
    /// Создаёт snapshot, который caller может использовать во время одного planning/wait цикла.
    #[must_use]
    pub fn snapshot(&self) -> VideoDecoderActivitySnapshot {
        VideoDecoderActivitySnapshot::Available {
            captured_epoch: self.current_epoch(),
            subscription: self.clone(),
        }
    }

    /// Возвращает текущий live epoch из shared activity state.
    #[must_use]
    pub fn current_epoch(&self) -> VideoDecoderActivityEpoch {
        VideoDecoderActivityEpoch::new(self.shared_epoch.load(Ordering::Acquire))
    }

    /// Возвращает receiver clone для `select!`, не отдавая backend-specific channels.
    #[must_use]
    pub fn pulse_receiver(&self) -> Receiver<()> {
        self.pulse_rx.clone()
    }

    /// Проверяет activity после observed epoch-а без блокировки.
    #[must_use]
    pub fn activity_since(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
    ) -> VideoDecoderActivityWaitOutcome {
        let current_epoch = self.current_epoch();
        if current_epoch.is_after(observed_epoch) {
            VideoDecoderActivityWaitOutcome::ActivityReceived {
                epoch: current_epoch,
            }
        } else {
            VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch {
                observed_epoch,
                current_epoch,
            }
        }
    }

    /// Классифицирует результат recv branch-а из внешнего `select!`.
    #[must_use]
    pub fn activity_after_recv(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        recv_result: Result<(), RecvError>,
    ) -> VideoDecoderActivityWaitOutcome {
        match recv_result {
            Ok(()) => self.activity_since(observed_epoch),
            Err(_) => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: VideoDecoderActivityUnavailableReason::DisconnectedNotifier,
            },
        }
    }

    /// Ждёт activity до fallback timeout-а, предварительно закрывая lost-wakeup окно.
    #[must_use]
    pub fn wait_for_activity_after(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        timeout: Duration,
    ) -> VideoDecoderActivityWaitOutcome {
        let immediate_outcome = self.activity_since(observed_epoch);
        if matches!(
            immediate_outcome,
            VideoDecoderActivityWaitOutcome::ActivityReceived { .. }
        ) {
            return immediate_outcome;
        }

        match self.pulse_rx.recv_timeout(timeout) {
            Ok(()) => self.activity_since(observed_epoch),
            Err(RecvTimeoutError::Timeout) => self.timeout_or_late_activity(observed_epoch),
            Err(RecvTimeoutError::Disconnected) => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: VideoDecoderActivityUnavailableReason::DisconnectedNotifier,
            },
        }
    }

    /// После timeout-а повторно читает epoch, чтобы не потерять late activity.
    fn timeout_or_late_activity(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
    ) -> VideoDecoderActivityWaitOutcome {
        let current_epoch = self.current_epoch();
        if current_epoch.is_after(observed_epoch) {
            VideoDecoderActivityWaitOutcome::ActivityReceived {
                epoch: current_epoch,
            }
        } else {
            VideoDecoderActivityWaitOutcome::Timeout {
                observed_epoch,
                current_epoch,
            }
        }
    }
}

impl std::fmt::Debug for VideoDecoderActivitySubscription {
    /// Печатает только stable diagnostics, не раскрывая внутренний receiver.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoDecoderActivitySubscription")
            .field("current_epoch", &self.current_epoch())
            .finish_non_exhaustive()
    }
}

/// Snapshot activity boundary-а, который можно безопасно передать в planning/wait код.
#[derive(Debug, Clone)]
pub enum VideoDecoderActivitySnapshot {
    /// Activity source доступен; captured_epoch отражает момент создания snapshot-а.
    Available {
        /// Epoch, увиденный при создании snapshot-а.
        captured_epoch: VideoDecoderActivityEpoch,

        /// Clone нейтральной subscription, через которую caller ждёт pulse.
        subscription: VideoDecoderActivitySubscription,
    },

    /// Activity source недоступен; caller должен выбрать bounded fallback poll.
    Unavailable {
        /// Typed причина, почему wait через activity невозможен.
        reason: VideoDecoderActivityUnavailableReason,
    },
}

impl VideoDecoderActivitySnapshot {
    /// Создаёт snapshot недоступного notifier-а с typed reason.
    #[must_use]
    pub const fn unavailable(reason: VideoDecoderActivityUnavailableReason) -> Self {
        Self::Unavailable { reason }
    }

    /// Создаёт default unsupported snapshot для backend-ов без activity contract-а.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self::Unavailable {
            reason: VideoDecoderActivityUnavailableReason::UnsupportedNotifier,
        }
    }

    /// Возвращает epoch, захваченный в момент snapshot-а.
    #[must_use]
    pub const fn captured_epoch(&self) -> Option<VideoDecoderActivityEpoch> {
        match self {
            Self::Available { captured_epoch, .. } => Some(*captured_epoch),
            Self::Unavailable { .. } => None,
        }
    }

    /// Возвращает receiver clone для внешнего `select!`, если notifier доступен.
    #[must_use]
    pub fn pulse_receiver(&self) -> Option<Receiver<()>> {
        match self {
            Self::Available { subscription, .. } => Some(subscription.pulse_receiver()),
            Self::Unavailable { .. } => None,
        }
    }

    /// Проверяет lost-wakeup окно после planning и перед входом в `select!`.
    #[must_use]
    pub fn activity_since(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
    ) -> VideoDecoderActivityWaitOutcome {
        match self {
            Self::Available { subscription, .. } => subscription.activity_since(observed_epoch),
            Self::Unavailable { reason } => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: reason.clone(),
            },
        }
    }

    /// Классифицирует recv branch внешнего `select!` через typed neutral outcome.
    #[must_use]
    pub fn activity_after_recv(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        recv_result: Result<(), RecvError>,
    ) -> VideoDecoderActivityWaitOutcome {
        match self {
            Self::Available { subscription, .. } => {
                subscription.activity_after_recv(observed_epoch, recv_result)
            }
            Self::Unavailable { reason } => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: reason.clone(),
            },
        }
    }

    /// Ждёт activity через subscription или возвращает typed unavailable state.
    #[must_use]
    pub fn wait_for_activity_after(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        timeout: Duration,
    ) -> VideoDecoderActivityWaitOutcome {
        match self {
            Self::Available { subscription, .. } => {
                subscription.wait_for_activity_after(observed_epoch, timeout)
            }
            Self::Unavailable { reason } => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: reason.clone(),
            },
        }
    }
}
