use std::sync::{Mutex, MutexGuard};

use crossbeam_channel::{Sender, TrySendError};
use tracing::warn;

use crate::{
    ScrubCommitIntent, ScrubCommitPolicy, ScrubGeneration, ScrubUpdateIntent, SeekRequest,
};

/// Ошибка постановки coalesced scrub update-а в sender-side wake boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubCoalescerSubmitError {
    /// Worker-side wake receiver уже закрыт, поэтому latest-slot нужно очистить.
    Disconnected,
}

/// Причина, по которой sender-side scrub command не имеет active operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubSequencerRejectReason {
    /// Пользователь ещё не отправлял успешный `BeginScrub`.
    NoActiveScrub,

    /// Последний active scrub уже был закрыт `EndScrub`.
    AlreadyFinished,

    /// Active scrub был инвалидирован внешней boundary-командой sender-а.
    SenderInterrupted,
}

/// Typed state sender-side scrub sequencer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrubSequencerState {
    /// Сейчас нет active scrub; generation остаётся sentinel для monotonic sequencing.
    Idle {
        /// Sentinel generation, от которого будет выдан следующий begin token.
        generation: ScrubGeneration,

        /// Почему sender считает scrub неактивным.
        reason: ScrubSequencerRejectReason,
    },

    /// UI/integration слой считает scrub активным.
    Active {
        /// Generation, который должен получить update/preview/end command.
        generation: ScrubGeneration,
    },
}

/// Решение sender-side sequencer-а после `BeginScrub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubSequencerBeginDecision {
    /// Begin открыл scrub из idle state.
    Started { generation: ScrubGeneration },

    /// Begin заменил предыдущий active scrub новым generation.
    ReplacedActive {
        /// Generation предыдущего active scrub-а.
        previous: ScrubGeneration,

        /// Новый generation для отправляемого `BeginScrub`.
        generation: ScrubGeneration,
    },
}

/// Решение sender-side sequencer-а для update/preview command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubSequencerUpdateDecision {
    /// Command получил active generation token и может идти дальше.
    Accepted { intent: ScrubUpdateIntent },

    /// Command не имеет active scrub и не должен будить worker.
    Rejected {
        /// Исходный seek request, который нельзя привязать к active generation.
        request: SeekRequest,

        /// Точная причина отказа sender-side lifecycle.
        reason: ScrubSequencerRejectReason,
    },
}

/// Решение sender-side sequencer-а для release/end command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubSequencerEndDecision {
    /// End получил active generation token и отправлен в worker.
    Accepted { intent: ScrubCommitIntent },

    /// End пришёл без active scrub и не должен закрывать чужой lifecycle.
    Rejected {
        /// Commit policy из public command, которую sender не отправил дальше.
        policy: ScrubCommitPolicy,

        /// Точная причина отказа sender-side lifecycle.
        reason: ScrubSequencerRejectReason,
    },
}

/// Результат sender-side invalidation для внешних boundary-команд.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubSequencerInterruptDecision {
    /// Active scrub был инвалидирован, late updates старого generation станут stale.
    Interrupted {
        /// Старый active generation.
        previous: ScrubGeneration,

        /// Новый idle sentinel generation.
        generation: ScrubGeneration,
    },

    /// Active scrub уже отсутствовал; sentinel всё равно сдвинут монотонно.
    Idle {
        /// Старый idle sentinel generation.
        previous: ScrubGeneration,

        /// Новый idle sentinel generation.
        generation: ScrubGeneration,

        /// Почему sender уже был idle до interrupt-а.
        reason: ScrubSequencerRejectReason,
    },
}

/// Sender-side generator typed scrub generation-ов для public команд.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScrubCommandSequencer {
    /// Единственный источник sender-side lifecycle и generation sentinel.
    state: ScrubSequencerState,
}

impl ScrubCommandSequencer {
    /// Создаёт sequencer без активного scrub-а.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            state: ScrubSequencerState::Idle {
                generation: ScrubGeneration::default(),
                reason: ScrubSequencerRejectReason::NoActiveScrub,
            },
        }
    }

    /// Возвращает generation текущего active или idle sentinel state-а.
    fn current_generation(&self) -> ScrubGeneration {
        match self.state {
            ScrubSequencerState::Idle { generation, .. }
            | ScrubSequencerState::Active { generation } => generation,
        }
    }

    /// Отправляет begin через caller boundary и меняет state только после successful send.
    pub(crate) fn dispatch_begin_scrub<DispatchError>(
        &mut self,
        dispatch: impl FnOnce(ScrubGeneration) -> Result<(), DispatchError>,
    ) -> Result<ScrubSequencerBeginDecision, DispatchError> {
        let generation = self.current_generation().next();
        let decision = match self.state {
            ScrubSequencerState::Active {
                generation: previous,
            } => ScrubSequencerBeginDecision::ReplacedActive {
                previous,
                generation,
            },
            ScrubSequencerState::Idle { .. } => ScrubSequencerBeginDecision::Started { generation },
        };

        dispatch(generation)?;
        self.state = ScrubSequencerState::Active { generation };
        Ok(decision)
    }

    /// Собирает update/preview intent для текущего активного scrub-а.
    pub(crate) fn update_intent(&self, request: SeekRequest) -> ScrubSequencerUpdateDecision {
        match self.state {
            ScrubSequencerState::Active { generation } => ScrubSequencerUpdateDecision::Accepted {
                intent: ScrubUpdateIntent::new(generation, request),
            },
            ScrubSequencerState::Idle { reason, .. } => {
                ScrubSequencerUpdateDecision::Rejected { request, reason }
            }
        }
    }

    /// Отправляет final intent и закрывает active generation только после successful send.
    pub(crate) fn dispatch_end_scrub<DispatchError>(
        &mut self,
        policy: ScrubCommitPolicy,
        dispatch: impl FnOnce(ScrubCommitIntent) -> Result<(), DispatchError>,
    ) -> Result<ScrubSequencerEndDecision, DispatchError> {
        let generation = match self.state {
            ScrubSequencerState::Active { generation } => generation,
            ScrubSequencerState::Idle { reason, .. } => {
                return Ok(ScrubSequencerEndDecision::Rejected { policy, reason });
            }
        };
        let intent = ScrubCommitIntent::new(generation, policy);

        dispatch(intent)?;
        self.state = ScrubSequencerState::Idle {
            generation: generation.next(),
            reason: ScrubSequencerRejectReason::AlreadyFinished,
        };

        Ok(ScrubSequencerEndDecision::Accepted { intent })
    }

    /// Инвалидирует active scrub после Stop/Open/Shutdown boundary на sender side.
    pub(crate) fn interrupt_scrub(&mut self) -> ScrubSequencerInterruptDecision {
        let previous = self.current_generation();
        let generation = previous.next();
        let previous_reason = match self.state {
            ScrubSequencerState::Active { .. } => None,
            ScrubSequencerState::Idle { reason, .. } => Some(reason),
        };

        self.state = ScrubSequencerState::Idle {
            generation,
            reason: ScrubSequencerRejectReason::SenderInterrupted,
        };

        match previous_reason {
            Some(reason) => ScrubSequencerInterruptDecision::Idle {
                previous,
                generation,
                reason,
            },
            None => ScrubSequencerInterruptDecision::Interrupted {
                previous,
                generation,
            },
        }
    }
}

/// Решение coalescer-а после попытки забрать latest scrub update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubCoalescedUpdateDecision {
    /// Slot пуст: worker-у нечего применять.
    Empty,

    /// Intent можно передать в driver для authoritative validation.
    Ready { intent: ScrubUpdateIntent },

    /// Intent принадлежит generation, чей `BeginScrub` worker ещё не обработал.
    FutureGeneration { intent: ScrubUpdateIntent },
}

/// Явная latest-wins прослойка для частых scrub updates.
pub(crate) struct ScrubUpdateCoalescer {
    /// Последняя цель scrub-а; sender только заменяет значение, worker только забирает.
    latest_request: Mutex<Option<ScrubUpdateIntent>>,

    /// Bounded wake-token: один pending token уже означает "latest_request надо проверить".
    wake_tx: Sender<()>,
}

impl ScrubUpdateCoalescer {
    /// Создаёт coalescer вокруг wake channel sender-а.
    #[must_use]
    pub(crate) fn new(wake_tx: Sender<()>) -> Self {
        Self {
            latest_request: Mutex::new(None),
            wake_tx,
        }
    }

    /// Записывает новую latest scrub-цель и будит worker одним bounded token-ом.
    pub(crate) fn submit_latest(
        &self,
        intent: ScrubUpdateIntent,
    ) -> Result<(), ScrubCoalescerSubmitError> {
        *self.latest_request_guard() = Some(intent);

        match self.wake_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => {
                *self.latest_request_guard() = None;
                Err(ScrubCoalescerSubmitError::Disconnected)
            }
        }
    }

    /// Забирает последнюю scrub-цель; все промежуточные цели намеренно coalesced.
    fn take_latest(&self) -> Option<ScrubUpdateIntent> {
        self.latest_request_guard().take()
    }

    /// Забирает latest intent, сохраняя future generation до соответствующего `BeginScrub`.
    pub(crate) fn take_for_generation(
        &self,
        current_generation: ScrubGeneration,
    ) -> ScrubCoalescedUpdateDecision {
        let Some(intent) = self.take_latest() else {
            return ScrubCoalescedUpdateDecision::Empty;
        };

        if intent.generation > current_generation {
            self.retain_future_intent(intent);
            return ScrubCoalescedUpdateDecision::FutureGeneration { intent };
        }

        ScrubCoalescedUpdateDecision::Ready { intent }
    }

    /// Сохраняет future-generation intent, если slot не получил более свежий generation.
    fn retain_future_intent(&self, intent: ScrubUpdateIntent) {
        let mut latest_request = self.latest_request_guard();
        if latest_request
            .as_ref()
            .is_some_and(|current_intent| current_intent.generation >= intent.generation)
        {
            return;
        }

        *latest_request = Some(intent);
    }

    /// Возвращает guard latest-slot-а и восстанавливается после poison без потери command path.
    fn latest_request_guard(&self) -> MutexGuard<'_, Option<ScrubUpdateIntent>> {
        match self.latest_request.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("Scrub coalescer mutex was poisoned; recovering latest request slot");
                poisoned.into_inner()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use media_core::MediaTime;

    use super::*;
    use crate::{SeekMode, SeekTarget};

    const SCRUB_WAKE_CHANNEL_CAPACITY_FOR_TESTS: usize = 1;

    fn seek_to_millis(milliseconds: u64) -> SeekRequest {
        SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_millis(milliseconds)),
            mode: SeekMode::Accurate,
        }
    }

    fn scrub_update_intent(generation: ScrubGeneration, milliseconds: u64) -> ScrubUpdateIntent {
        ScrubUpdateIntent::new(generation, seek_to_millis(milliseconds))
    }

    #[test]
    fn scrub_command_sequencer_rejects_update_before_begin_as_no_active() {
        let sequencer = ScrubCommandSequencer::new();
        let request = seek_to_millis(100);

        assert_eq!(
            sequencer.update_intent(request),
            ScrubSequencerUpdateDecision::Rejected {
                request,
                reason: ScrubSequencerRejectReason::NoActiveScrub,
            }
        );
    }

    #[test]
    fn sender_side_begin_end_sequencing_keeps_generation_monotonic() {
        let mut sequencer = ScrubCommandSequencer::new();
        let policy = ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE;

        let first_begin = sequencer
            .dispatch_begin_scrub(|generation| {
                assert_eq!(generation.as_u64(), 1);
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(
            first_begin,
            ScrubSequencerBeginDecision::Started {
                generation: ScrubGeneration::default().next(),
            }
        );

        let first_update = sequencer.update_intent(seek_to_millis(100));
        assert_eq!(
            first_update,
            ScrubSequencerUpdateDecision::Accepted {
                intent: scrub_update_intent(ScrubGeneration::default().next(), 100),
            }
        );

        let first_end = sequencer
            .dispatch_end_scrub(policy, |intent| {
                assert_eq!(intent.generation.as_u64(), 1);
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(
            first_end,
            ScrubSequencerEndDecision::Accepted {
                intent: ScrubCommitIntent::new(ScrubGeneration::default().next(), policy),
            }
        );

        assert_eq!(
            sequencer.update_intent(seek_to_millis(250)),
            ScrubSequencerUpdateDecision::Rejected {
                request: seek_to_millis(250),
                reason: ScrubSequencerRejectReason::AlreadyFinished,
            }
        );

        let second_begin = sequencer
            .dispatch_begin_scrub(|generation| {
                assert_eq!(generation.as_u64(), 3);
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(
            second_begin,
            ScrubSequencerBeginDecision::Started {
                generation: ScrubGeneration::default().next().next().next(),
            }
        );
    }

    #[test]
    fn sender_side_interrupt_invalidates_active_generation() {
        let mut sequencer = ScrubCommandSequencer::new();
        let first_generation = ScrubGeneration::default().next();
        let interrupted_generation = first_generation.next();

        sequencer
            .dispatch_begin_scrub(|_| Ok::<(), ()>(()))
            .unwrap();

        assert_eq!(
            sequencer.interrupt_scrub(),
            ScrubSequencerInterruptDecision::Interrupted {
                previous: first_generation,
                generation: interrupted_generation,
            }
        );
        assert_eq!(
            sequencer.update_intent(seek_to_millis(500)),
            ScrubSequencerUpdateDecision::Rejected {
                request: seek_to_millis(500),
                reason: ScrubSequencerRejectReason::SenderInterrupted,
            }
        );
    }

    #[test]
    fn scrub_coalescer_keeps_future_generation_until_worker_reaches_it() {
        let (wake_tx, _wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY_FOR_TESTS);
        let coalescer = ScrubUpdateCoalescer::new(wake_tx);
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();
        let future_intent = scrub_update_intent(second_generation, 250);

        coalescer.submit_latest(future_intent).unwrap();

        assert_eq!(
            coalescer.take_for_generation(first_generation),
            ScrubCoalescedUpdateDecision::FutureGeneration {
                intent: future_intent,
            }
        );
        assert_eq!(
            coalescer.take_for_generation(second_generation),
            ScrubCoalescedUpdateDecision::Ready {
                intent: future_intent,
            }
        );
        assert_eq!(
            coalescer.take_for_generation(second_generation),
            ScrubCoalescedUpdateDecision::Empty
        );
    }

    #[test]
    fn scrub_coalescer_keeps_latest_target_without_sender_receiver_drain() {
        let (wake_tx, wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY_FOR_TESTS);
        let coalescer = ScrubUpdateCoalescer::new(wake_tx);
        let generation = ScrubGeneration::default().next();

        coalescer
            .submit_latest(scrub_update_intent(generation, 100))
            .unwrap();
        coalescer
            .submit_latest(scrub_update_intent(generation, 250))
            .unwrap();
        coalescer
            .submit_latest(scrub_update_intent(generation, 900))
            .unwrap();

        assert_eq!(wake_rx.len(), 1);
        assert_eq!(
            coalescer.take_latest(),
            Some(scrub_update_intent(generation, 900))
        );
        assert_eq!(coalescer.take_latest(), None);
    }

    #[test]
    fn scrub_coalescer_reports_disconnected_without_keeping_stale_target() {
        let (wake_tx, wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY_FOR_TESTS);
        let coalescer = ScrubUpdateCoalescer::new(wake_tx);
        let generation = ScrubGeneration::default().next();
        drop(wake_rx);

        let send_result = coalescer.submit_latest(scrub_update_intent(generation, 100));

        assert_eq!(send_result, Err(ScrubCoalescerSubmitError::Disconnected));
        assert_eq!(coalescer.take_latest(), None);
    }

    #[test]
    fn coalescer_preserves_latest_future_generation_when_older_future_is_restored() {
        let (wake_tx, _wake_rx) = bounded(SCRUB_WAKE_CHANNEL_CAPACITY_FOR_TESTS);
        let coalescer = ScrubUpdateCoalescer::new(wake_tx);
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();
        let third_generation = second_generation.next();

        coalescer
            .submit_latest(scrub_update_intent(second_generation, 200))
            .unwrap();
        assert_eq!(
            coalescer.take_for_generation(first_generation),
            ScrubCoalescedUpdateDecision::FutureGeneration {
                intent: scrub_update_intent(second_generation, 200),
            }
        );

        coalescer
            .submit_latest(scrub_update_intent(third_generation, 300))
            .unwrap();
        assert_eq!(
            coalescer.take_for_generation(first_generation),
            ScrubCoalescedUpdateDecision::FutureGeneration {
                intent: scrub_update_intent(third_generation, 300),
            }
        );
        assert_eq!(
            coalescer.take_for_generation(second_generation),
            ScrubCoalescedUpdateDecision::FutureGeneration {
                intent: scrub_update_intent(third_generation, 300),
            }
        );
        assert_eq!(
            coalescer.take_for_generation(third_generation),
            ScrubCoalescedUpdateDecision::Ready {
                intent: scrub_update_intent(third_generation, 300),
            }
        );
    }
}
