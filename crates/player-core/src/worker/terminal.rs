use super::*;

/// Короткий park slice не превращает bounded join в busy loop и не продлевает общий deadline.
const TERMINAL_JOIN_POLL_SLICE: Duration = Duration::from_millis(1);

/// Абсолютный deadline для terminal shutdown player worker-а.
///
/// Wrapper не даёт callsite-у случайно передать безымянный `Duration` и позволяет нескольким
/// process owners делить один общий shutdown deadline без последовательного продления budget-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerWorkerShutdownDeadline {
    /// Монотонный момент, после которого bounded wait обязан вернуть управление caller-у.
    instant: Instant,
}

impl PlayerWorkerShutdownDeadline {
    /// Создаёт deadline из app-owned абсолютного монотонного момента.
    pub const fn at(instant: Instant) -> Self {
        Self { instant }
    }

    /// Создаёт отдельный deadline относительно текущего момента.
    pub fn after(budget: Duration) -> Self {
        Self::at(Instant::now() + budget)
    }

    /// Возвращает абсолютный момент для композиции общего process shutdown budget-а.
    pub const fn instant(self) -> Instant {
        self.instant
    }
}

/// Каким transport-ом terminal request дошёл до worker owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerWorkerShutdownRequestOutcome {
    /// И ordered command, и отдельный cancellation signal приняты своими transport-ами.
    CommandAndCancellationAccepted,

    /// Ordered command принята, но аварийный cancellation transport отказал.
    CommandAccepted {
        /// Причина, по которой cancellation transport не принял signal.
        cancellation_error: PlayerWorkerSendError,
    },

    /// Ordinary command не была принята, поэтому сработал отдельный cancellation channel.
    CancellationAccepted {
        /// Причина, по которой ordinary command transport не принял shutdown.
        command_error: PlayerWorkerSendError,
    },

    /// Ни ordinary command, ни аварийный cancellation channel не приняли request.
    RequestFailed {
        /// Причина отказа ordinary command transport-а.
        command_error: PlayerWorkerSendError,

        /// Причина отказа cancellation transport-а.
        cancellation_error: PlayerWorkerSendError,
    },
}

/// Typed результат одной bounded попытки terminal shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "shutdown outcome определяет, можно ли освобождать process-lifetime owners"]
pub enum PlayerWorkerShutdownOutcome {
    /// Поток завершился штатно и был join-нут в пределах deadline.
    Completed {
        /// Зафиксированный результат единственной отправки terminal request-а.
        request: PlayerWorkerShutdownRequestOutcome,
    },

    /// Поток уже был join-нут предыдущей попыткой.
    AlreadyCompleted,

    /// Deadline истёк; join handle и terminal ownership сохранены для повторного drain-а.
    TimedOut {
        /// Зафиксированный результат единственной отправки terminal request-а.
        request: PlayerWorkerShutdownRequestOutcome,
    },

    /// Поток завершился panic-ом и был join-нут в пределах deadline.
    ThreadPanicked {
        /// Зафиксированный результат единственной отправки terminal request-а.
        request: PlayerWorkerShutdownRequestOutcome,
    },
}

/// Внутренняя terminal state machine не позволяет повторно отправлять shutdown или join-ить handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlayerWorkerTerminalState {
    /// Worker принимает команды и terminal request ещё не отправлялся.
    Running,

    /// Terminal request отправлен ровно один раз, поток ещё не join-нут.
    ShutdownRequested(PlayerWorkerShutdownRequestOutcome),

    /// Предыдущая bounded попытка истекла, ownership всё ещё удерживается объектом.
    TimedOut(PlayerWorkerShutdownRequestOutcome),

    /// Join уже выполнен и terminal outcome больше не может измениться.
    Completed,
}

impl PlayerWorker {
    /// Закрывает admission и отправляет terminal request ровно один раз.
    fn begin_terminal_shutdown(&mut self) -> PlayerWorkerShutdownRequestOutcome {
        self.command_sender
            .admission_closed
            .store(true, Ordering::Release);

        let command_result = self
            .command_sender
            .command_tx
            .try_send(WorkerCommand::Player(PlayerCommand::Shutdown))
            .map_err(PlayerWorkerSendError::from);
        let cancellation_result = self
            .shutdown_tx
            .try_send(())
            .map_err(PlayerWorkerSendError::from);

        match (command_result, cancellation_result) {
            (Ok(()), Ok(())) => PlayerWorkerShutdownRequestOutcome::CommandAndCancellationAccepted,
            (Ok(()), Err(cancellation_error)) => {
                PlayerWorkerShutdownRequestOutcome::CommandAccepted { cancellation_error }
            }
            (Err(command_error), Ok(())) => {
                PlayerWorkerShutdownRequestOutcome::CancellationAccepted { command_error }
            }
            (Err(command_error), Err(cancellation_error)) => {
                PlayerWorkerShutdownRequestOutcome::RequestFailed {
                    command_error,
                    cancellation_error,
                }
            }
        }
    }

    /// Возвращает сохранённый request outcome либо впервые начинает terminal transition.
    fn terminal_request_outcome(&mut self) -> PlayerWorkerShutdownRequestOutcome {
        match self.terminal_state {
            PlayerWorkerTerminalState::Running => {
                let request = self.begin_terminal_shutdown();
                self.terminal_state = PlayerWorkerTerminalState::ShutdownRequested(request);
                request
            }
            PlayerWorkerTerminalState::ShutdownRequested(request)
            | PlayerWorkerTerminalState::TimedOut(request) => request,
            PlayerWorkerTerminalState::Completed => {
                unreachable!("completed worker не должен повторно запрашивать terminal outcome")
            }
        }
    }

    /// Join-ит только уже завершившийся поток и фиксирует окончательное terminal состояние.
    fn join_finished_thread(
        &mut self,
        request: PlayerWorkerShutdownRequestOutcome,
    ) -> PlayerWorkerShutdownOutcome {
        let Some(join_handle) = self.join_handle.take() else {
            self.terminal_state = PlayerWorkerTerminalState::Completed;
            return PlayerWorkerShutdownOutcome::AlreadyCompleted;
        };

        let join_result = join_handle.join();
        self.terminal_state = PlayerWorkerTerminalState::Completed;
        match join_result {
            Ok(()) => PlayerWorkerShutdownOutcome::Completed { request },
            Err(_panic_payload) => PlayerWorkerShutdownOutcome::ThreadPanicked { request },
        }
    }

    /// Terminal shutdown с абсолютным deadline и retained ownership при timeout.
    ///
    /// Первый вызов закрывает admission у всех clone-ов `PlayerCommandSender` до отправки
    /// shutdown. Повторный вызов не отправляет command/cancellation второй раз и только drain-ит
    /// сохранённый `JoinHandle`. `join` вызывается исключительно после `is_finished()`.
    #[must_use = "timeout требует terminal process-exit либо повторного drain до release lease"]
    pub fn shutdown_before(
        &mut self,
        deadline: PlayerWorkerShutdownDeadline,
    ) -> PlayerWorkerShutdownOutcome {
        if self.terminal_state == PlayerWorkerTerminalState::Completed || self.join_handle.is_none()
        {
            self.terminal_state = PlayerWorkerTerminalState::Completed;
            return PlayerWorkerShutdownOutcome::AlreadyCompleted;
        }

        let request = self.terminal_request_outcome();
        loop {
            let worker_finished = self
                .join_handle
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished);
            if worker_finished {
                return self.join_finished_thread(request);
            }

            let now = Instant::now();
            if now >= deadline.instant() {
                self.terminal_state = PlayerWorkerTerminalState::TimedOut(request);
                return PlayerWorkerShutdownOutcome::TimedOut { request };
            }

            let remaining = deadline.instant().saturating_duration_since(now);
            thread::park_timeout(remaining.min(TERMINAL_JOIN_POLL_SLICE));
        }
    }

    /// Legacy blocking shutdown сохраняет прежний lifecycle contract.
    ///
    /// После typed timeout caller должен предпочесть повторный `shutdown_before` или немедленный
    /// process exit. Явный вызов этого legacy API означает осознанное согласие ждать без deadline.
    pub fn shutdown(&mut self) -> Result<(), PlayerWorkerJoinError> {
        if self.terminal_state == PlayerWorkerTerminalState::Completed || self.join_handle.is_none()
        {
            self.terminal_state = PlayerWorkerTerminalState::Completed;
            return Ok(());
        }

        let _request = self.terminal_request_outcome();
        let Some(join_handle) = self.join_handle.take() else {
            self.terminal_state = PlayerWorkerTerminalState::Completed;
            return Ok(());
        };

        let join_result = join_handle.join().map_err(|_| PlayerWorkerJoinError);
        self.terminal_state = PlayerWorkerTerminalState::Completed;
        join_result
    }
}

impl Drop for PlayerWorker {
    /// Legacy owner по-прежнему делает blocking cleanup, но typed timeout не скрывает второй wait.
    fn drop(&mut self) {
        match self.terminal_state {
            PlayerWorkerTerminalState::Running
            | PlayerWorkerTerminalState::ShutdownRequested(_) => {
                if let Err(error) = self.shutdown() {
                    warn!(error = %error, "Player worker shutdown failed during drop");
                }
            }
            PlayerWorkerTerminalState::TimedOut(_) => {
                let worker_finished = self
                    .join_handle
                    .as_ref()
                    .is_some_and(thread::JoinHandle::is_finished);
                if worker_finished {
                    let request = self.terminal_request_outcome();
                    if matches!(
                        self.join_finished_thread(request),
                        PlayerWorkerShutdownOutcome::ThreadPanicked { .. }
                    ) {
                        warn!("Player worker panicked after bounded shutdown timeout");
                    }
                    return;
                }

                if let Some(join_handle) = self.join_handle.take() {
                    let _process_lifetime_handle = Box::leak(Box::new(join_handle));
                    warn!(
                        "Player worker still runs after bounded shutdown timeout; retaining join ownership until process exit"
                    );
                }
            }
            PlayerWorkerTerminalState::Completed => {}
        }
    }
}
