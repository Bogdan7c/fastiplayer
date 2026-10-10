//! Сессия UX 17: команды транспорта не теряются при переполнении очереди worker-а.
//!
//! «Застрявший» worker моделируется детерминированно: команды отправляются через
//! публичный `PlayerCommandSender` в настоящую очередь с резервом, а runtime не читает
//! её, пока тест не разрешит. Это ровно то, что видит UI, когда worker thread занят
//! долгой синхронной операцией (seek локального файла, flush декодера, открытие
//! аудиоустройства). До исправления команды сверх 128 терялись с `Full`.

use super::*;

/// Заполняет основную очередь до отказа промежуточными `SetVolume` (как drag слайдера).
///
/// После этого любая следующая обычная команда может попасть только в резерв.
fn fill_main_queue_with_volume_ramp(
    runtime: &PlayerWorkerRuntime,
    command_sender: &PlayerCommandSender,
) {
    let mut step = 0_u16;
    while runtime.command_inbox.main_receiver().len() < COMMAND_CHANNEL_CAPACITY {
        // Шаг 0.001 даёт разные валидные значения громкости.
        let volume = f32::from(step) / 1_000.0;
        command_sender
            .try_send(PlayerCommand::SetVolume(volume))
            .expect("main queue has room");
        step += 1;
    }
}

/// «Отпускает» worker: обрабатывает всё накопленное обычным batch-путём.
fn drain_whole_command_queue(runtime: &mut PlayerWorkerRuntime) -> usize {
    let mut processed_total = 0;
    loop {
        let processed = runtime.drain_pending_command_batch();
        runtime.service_worker_fairness_checkpoint(processed);
        if processed == 0 {
            return processed_total;
        }
        processed_total += processed;
    }
}

/// Runtime с media в Playing и public sender-ом поверх его настоящей очереди.
fn playing_runtime_with_sender(
    seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
) -> (PlayerWorkerRuntime, PlayerCommandSender) {
    let (mut runtime, command_sender) = runtime_for_tests_with_public_sender(Instant::now());
    install_worker_video_media(&mut runtime, seek_request_log);
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));
    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Playing
    );
    (runtime, command_sender)
}

#[test]
fn end_scrub_sent_into_full_queue_still_releases_player_from_scrubbing() {
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));
    let (mut runtime, command_sender) = playing_runtime_with_sender(Arc::clone(&seek_request_log));

    // Начало drag успело попасть в очередь до того, как worker «застрял».
    command_sender
        .try_send(PlayerCommand::begin_scrub())
        .expect("begin scrub fits the empty queue");
    command_sender
        .try_send(PlayerCommand::UpdateScrub(seek_to_millis(20_000)))
        .expect("scrub target fits the queue");
    fill_main_queue_with_volume_ramp(&runtime, &command_sender);

    // Отпускание мыши при полной основной очереди: раньше здесь был `Full` и потеря.
    let end_scrub_result = command_sender.try_send(PlayerCommand::end_scrub(
        ScrubCommitPolicy::CommitLatestTarget,
    ));
    assert_eq!(end_scrub_result, Ok(()));

    let processed = drain_whole_command_queue(&mut runtime);
    assert_eq!(processed, COMMAND_CHANNEL_CAPACITY + 1);

    // EndScrub дошёл после Begin/Update: commit запустил seek к цели жеста.
    let snapshot = runtime.session.snapshot();
    assert_ne!(snapshot.playback_state, PlaybackState::Scrubbing);
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .as_slice(),
        &[DemuxSeekRequest::decode_point_before(Duration::from_secs(
            20
        ))]
    );
}

#[test]
fn pause_sent_into_full_queue_is_applied_after_earlier_commands() {
    let (mut runtime, command_sender) =
        playing_runtime_with_sender(Arc::new(Mutex::new(Vec::new())));
    fill_main_queue_with_volume_ramp(&runtime, &command_sender);

    // Пауза при полной основной очереди уходит в резерв и применяется последней.
    command_sender
        .try_send(PlayerCommand::Pause)
        .expect("pause goes to the reserve");
    let processed = drain_whole_command_queue(&mut runtime);

    assert_eq!(processed, COMMAND_CHANNEL_CAPACITY + 1);
    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Paused
    );
}

#[test]
fn thousand_volume_updates_into_stalled_worker_deliver_the_final_volume() {
    let (mut runtime, command_sender) =
        playing_runtime_with_sender(Arc::new(Mutex::new(Vec::new())));

    // 1000 SetVolume подряд, пока worker не читает очередь: 128 лягут в основную
    // очередь, остальные сольются в одну ячейку резерва.
    for step in 0..1_000_u16 {
        let volume = f32::from(step) / 2_000.0;
        command_sender
            .try_send(PlayerCommand::SetVolume(volume))
            .expect("volume must never be rejected");
    }
    command_sender
        .try_send(PlayerCommand::SetVolume(0.75))
        .expect("final volume must be accepted");

    let episode = runtime.command_inbox.current_reserve_episode();
    assert_eq!(episode.reserved, 1);
    assert_eq!(episode.rejected, 0);
    let processed = drain_whole_command_queue(&mut runtime);
    assert_eq!(processed, COMMAND_CHANNEL_CAPACITY + 1);

    let snapshot = runtime.session.snapshot();
    assert!(
        (snapshot.volume - 0.75).abs() < f32::EPSILON,
        "player volume {} must equal the final 0.75",
        snapshot.volume
    );
}

#[test]
fn media_replacement_during_scrub_exits_scrubbing_without_end_scrub() {
    let (mut runtime, _command_sender) =
        playing_runtime_with_sender(Arc::new(Mutex::new(Vec::new())));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::begin_scrub()));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::UpdateScrub(
        seek_to_millis(20_000),
    )));
    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Scrubbing
    );

    // UI-жест исчез (bounds = None из-за смены media) и End/Cancel не отправлен.
    install_worker_video_media(&mut runtime, Arc::new(Mutex::new(Vec::new())));

    let snapshot = runtime.session.snapshot();
    assert_ne!(snapshot.playback_state, PlaybackState::Scrubbing);
    assert!(!snapshot.timeline.scrubbing);
}

#[test]
fn healthy_worker_keeps_up_with_faster_than_ui_volume_drag() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).expect("spawn worker");
    let command_sender = worker.command_sender();
    let mut rejected_commands = 0;

    // 500 команд с шагом 1 мс — в ~7 раз чаще одного SetVolume на кадр при 144 Гц.
    for step in 0..500_u16 {
        let volume = f32::from(step) / 1_000.0;
        if command_sender
            .try_send(PlayerCommand::SetVolume(volume))
            .is_err()
        {
            rejected_commands += 1;
        }
        thread::sleep(Duration::from_millis(1));
    }
    command_sender
        .try_send(PlayerCommand::SetVolume(0.75))
        .expect("final volume must be accepted by a healthy worker");

    let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        (snapshot.volume - 0.75).abs() < f32::EPSILON
    });
    assert!((snapshot.volume - 0.75).abs() < f32::EPSILON);
    assert_eq!(rejected_commands, 0);
    worker.shutdown().expect("worker shutdown");
}
