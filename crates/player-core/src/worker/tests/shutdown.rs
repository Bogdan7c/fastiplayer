//! Ограниченный shutdown worker-а, таймауты и типизированные отказы каналов.

use super::*;

#[test]
fn bounded_shutdown_completes_and_second_drain_is_already_completed() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();

    let completed =
        worker.shutdown_before(PlayerWorkerShutdownDeadline::after(Duration::from_secs(1)));

    assert_eq!(
        completed,
        PlayerWorkerShutdownOutcome::Completed {
            request: PlayerWorkerShutdownRequestOutcome::CommandAndCancellationAccepted,
        }
    );
    assert_eq!(
        worker.shutdown_before(PlayerWorkerShutdownDeadline::at(Instant::now())),
        PlayerWorkerShutdownOutcome::AlreadyCompleted
    );
}

#[test]
fn bounded_timeout_retains_handle_closes_admission_and_later_joins_without_double_send() {
    let (release_tx, release_rx) = bounded(1);
    let join_handle = std::thread::spawn(move || {
        release_rx
            .recv()
            .expect("test owner должен явно разрешить завершение потока");
    });
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded(1);
    let mut worker = worker_with_terminal_thread_for_tests(command_tx, shutdown_tx, join_handle);
    let cloned_sender = worker.command_sender();

    let timed_out = worker.shutdown_before(PlayerWorkerShutdownDeadline::at(Instant::now()));

    assert_eq!(
        timed_out,
        PlayerWorkerShutdownOutcome::TimedOut {
            request: PlayerWorkerShutdownRequestOutcome::CommandAndCancellationAccepted,
        }
    );
    assert!(worker.join_handle.is_some());
    assert_eq!(
        cloned_sender.try_send(PlayerCommand::Play),
        Err(PlayerWorkerSendError::Disconnected)
    );

    release_tx
        .send(())
        .expect("release channel должен оставаться подключённым");
    let completed =
        worker.shutdown_before(PlayerWorkerShutdownDeadline::after(Duration::from_secs(1)));
    assert_eq!(
        completed,
        PlayerWorkerShutdownOutcome::Completed {
            request: PlayerWorkerShutdownRequestOutcome::CommandAndCancellationAccepted,
        }
    );
    let shutdown_commands = command_rx
        .try_iter()
        .filter(|command| matches!(command, WorkerCommand::Player(PlayerCommand::Shutdown)))
        .count();
    assert_eq!(shutdown_commands, 1);
    assert_eq!(shutdown_rx.try_iter().count(), 1);
}

#[test]
fn bounded_shutdown_reports_worker_thread_panic() {
    let join_handle = std::thread::spawn(|| panic!("expected terminal test panic"));
    let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (shutdown_tx, _shutdown_rx) = bounded(1);
    let mut worker = worker_with_terminal_thread_for_tests(command_tx, shutdown_tx, join_handle);

    let outcome =
        worker.shutdown_before(PlayerWorkerShutdownDeadline::after(Duration::from_secs(1)));

    assert_eq!(
        outcome,
        PlayerWorkerShutdownOutcome::ThreadPanicked {
            request: PlayerWorkerShutdownRequestOutcome::CommandAndCancellationAccepted,
        }
    );
}

#[test]
fn command_disconnect_uses_typed_cancellation_fallback() {
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    drop(command_rx);
    let (shutdown_tx, shutdown_rx) = bounded(1);
    let join_handle = std::thread::spawn(move || {
        shutdown_rx
            .recv()
            .expect("cancellation fallback должен разбудить тестовый owner");
    });
    let mut worker = worker_with_terminal_thread_for_tests(command_tx, shutdown_tx, join_handle);

    let outcome =
        worker.shutdown_before(PlayerWorkerShutdownDeadline::after(Duration::from_secs(1)));

    assert_eq!(
        outcome,
        PlayerWorkerShutdownOutcome::Completed {
            request: PlayerWorkerShutdownRequestOutcome::CancellationAccepted {
                command_error: PlayerWorkerSendError::Disconnected,
            },
        }
    );
}

#[test]
fn cancellation_disconnect_is_typed_while_command_is_accepted() {
    let (release_tx, release_rx) = bounded(1);
    let join_handle = std::thread::spawn(move || {
        release_rx
            .recv()
            .expect("test owner должен явно разрешить завершение потока");
    });
    let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded(1);
    drop(shutdown_rx);
    let mut worker = worker_with_terminal_thread_for_tests(command_tx, shutdown_tx, join_handle);
    let request = PlayerWorkerShutdownRequestOutcome::CommandAccepted {
        cancellation_error: PlayerWorkerSendError::Disconnected,
    };

    assert_eq!(
        worker.shutdown_before(PlayerWorkerShutdownDeadline::at(Instant::now())),
        PlayerWorkerShutdownOutcome::TimedOut { request }
    );
    release_tx
        .send(())
        .expect("release channel должен оставаться подключённым");
    assert_eq!(
        worker.shutdown_before(PlayerWorkerShutdownDeadline::after(Duration::from_secs(1))),
        PlayerWorkerShutdownOutcome::Completed { request }
    );
}

#[test]
fn command_and_cancellation_failures_remain_typed_across_retry() {
    let (release_tx, release_rx) = bounded(1);
    let join_handle = std::thread::spawn(move || {
        release_rx
            .recv()
            .expect("test owner должен явно разрешить завершение потока");
    });
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    drop(command_rx);
    let (shutdown_tx, shutdown_rx) = bounded(1);
    drop(shutdown_rx);
    let mut worker = worker_with_terminal_thread_for_tests(command_tx, shutdown_tx, join_handle);
    let failed_request = PlayerWorkerShutdownRequestOutcome::RequestFailed {
        command_error: PlayerWorkerSendError::Disconnected,
        cancellation_error: PlayerWorkerSendError::Disconnected,
    };

    assert_eq!(
        worker.shutdown_before(PlayerWorkerShutdownDeadline::at(Instant::now())),
        PlayerWorkerShutdownOutcome::TimedOut {
            request: failed_request,
        }
    );

    release_tx
        .send(())
        .expect("release channel должен оставаться подключённым");
    assert_eq!(
        worker.shutdown_before(PlayerWorkerShutdownDeadline::after(Duration::from_secs(1))),
        PlayerWorkerShutdownOutcome::Completed {
            request: failed_request,
        }
    );
}

#[test]
fn drop_after_typed_timeout_does_not_repeat_unbounded_join() {
    let (release_tx, release_rx) = bounded(1);
    let (worker_finished_tx, worker_finished_rx) = bounded(1);
    let join_handle = std::thread::spawn(move || {
        release_rx
            .recv()
            .expect("test owner должен явно разрешить завершение потока");
        worker_finished_tx
            .send(())
            .expect("test observer должен дождаться реального завершения");
    });
    let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (shutdown_tx, _shutdown_rx) = bounded(1);
    let mut worker = worker_with_terminal_thread_for_tests(command_tx, shutdown_tx, join_handle);
    assert!(matches!(
        worker.shutdown_before(PlayerWorkerShutdownDeadline::at(Instant::now())),
        PlayerWorkerShutdownOutcome::TimedOut { .. }
    ));
    let (drop_completed_tx, drop_completed_rx) = bounded(1);
    let drop_thread = std::thread::spawn(move || {
        drop(worker);
        drop_completed_tx
            .send(())
            .expect("drop observer должен оставаться подключённым");
    });

    drop_completed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Drop после typed timeout не должен повторять unbounded join");
    release_tx
        .send(())
        .expect("release channel должен оставаться подключённым");
    worker_finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("retained process-lifetime thread должен штатно завершиться после release");
    drop_thread
        .join()
        .expect("test drop observer не должен panic-овать");
}
