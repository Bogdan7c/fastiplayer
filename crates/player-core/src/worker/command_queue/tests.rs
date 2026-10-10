use super::*;
use crate::{ScrubCommitPolicy, SeekRequest};
use media_core::MediaTime;

/// Очередь с маленькой основной ёмкостью, чтобы переполнение было дешёвым.
fn small_queue(main_capacity: usize) -> (WorkerCommandQueue, WorkerCommandInbox) {
    let (main_tx, main_rx) = bounded(main_capacity);
    worker_command_queue(main_tx, main_rx)
}

/// Забирает все команды в порядке, в котором их увидит worker.
fn drain_player_commands(inbox: &WorkerCommandInbox) -> Vec<PlayerCommand> {
    let mut commands = Vec::new();
    loop {
        match inbox.try_receive() {
            Ok(Some(WorkerCommand::Player(command))) => commands.push(command),
            Ok(Some(_other)) => panic!("тест ждёт только player commands"),
            Ok(None) | Err(CommandInboxDisconnected) => return commands,
        }
    }
}

fn scrub_target(milliseconds: u64) -> PlayerCommand {
    PlayerCommand::UpdateScrub(SeekRequest::absolute(MediaTime::from_millis(milliseconds)))
}

fn end_scrub() -> PlayerCommand {
    PlayerCommand::end_scrub(ScrubCommitPolicy::CommitLatestTarget)
}

#[test]
fn free_main_queue_takes_the_fast_path_without_touching_reserve() {
    let (queue, inbox) = small_queue(4);

    let admission = queue.try_send_player_command(PlayerCommand::Play);

    assert_eq!(admission, Ok(CommandAdmission::Queued));
    assert_eq!(inbox.main_receiver().len(), 1);
    assert_eq!(
        inbox.current_reserve_episode(),
        CommandReserveEpisode::default()
    );
    assert_eq!(drain_player_commands(&inbox), vec![PlayerCommand::Play]);
}

#[test]
fn overflowed_end_scrub_is_delivered_after_earlier_scrub_commands() {
    let (queue, inbox) = small_queue(2);
    queue
        .try_send_player_command(PlayerCommand::begin_scrub())
        .expect("begin fits");
    queue
        .try_send_player_command(scrub_target(1_000))
        .expect("target fits");

    let end_admission = queue.try_send_player_command(end_scrub());

    assert_eq!(end_admission, Ok(CommandAdmission::Reserved));
    assert_eq!(
        drain_player_commands(&inbox),
        vec![
            PlayerCommand::begin_scrub(),
            scrub_target(1_000),
            end_scrub()
        ]
    );
}

#[test]
fn reserve_stays_sticky_until_drained_so_newer_commands_cannot_overtake() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("first command fits");
    queue
        .try_send_player_command(PlayerCommand::TogglePlayback)
        .expect("overflow goes to reserve");

    // Worker прочитал основную очередь: место освободилось, но резерв ещё не пуст.
    assert!(matches!(
        inbox.try_receive(),
        Ok(Some(WorkerCommand::Player(PlayerCommand::Play)))
    ));
    let admission = queue.try_send_player_command(PlayerCommand::Pause);

    assert_eq!(admission, Ok(CommandAdmission::Reserved));
    assert_eq!(inbox.main_receiver().len(), 0);
    assert_eq!(
        drain_player_commands(&inbox),
        vec![PlayerCommand::TogglePlayback, PlayerCommand::Pause]
    );
}

#[test]
fn adjacent_latest_values_coalesce_but_ordered_commands_split_them() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");

    let admissions = [
        PlayerCommand::SetVolume(0.1),
        PlayerCommand::SetVolume(0.2),
        PlayerCommand::ToggleMute {
            fallback_volume: 1.0,
        },
        PlayerCommand::SetVolume(0.3),
        PlayerCommand::SetVolume(0.4),
    ]
    .map(|command| queue.try_send_player_command(command));

    assert_eq!(
        admissions,
        [
            Ok(CommandAdmission::Reserved),
            Ok(CommandAdmission::CoalescedInReserve),
            Ok(CommandAdmission::Reserved),
            Ok(CommandAdmission::Reserved),
            Ok(CommandAdmission::CoalescedInReserve),
        ]
    );
    assert_eq!(
        inbox.current_reserve_episode(),
        CommandReserveEpisode {
            reserved: 3,
            coalesced: 2,
            rejected: 0,
        }
    );
    assert_eq!(
        drain_player_commands(&inbox),
        vec![
            PlayerCommand::Play,
            PlayerCommand::SetVolume(0.2),
            PlayerCommand::ToggleMute {
                fallback_volume: 1.0
            },
            PlayerCommand::SetVolume(0.4),
        ]
    );
}

#[test]
fn different_latest_value_kinds_and_ordered_toggles_never_merge() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");

    for command in [
        PlayerCommand::SetVolume(0.5),
        scrub_target(2_000),
        PlayerCommand::TogglePlayback,
        PlayerCommand::TogglePlayback,
    ] {
        assert_eq!(
            queue.try_send_player_command(command),
            Ok(CommandAdmission::Reserved)
        );
    }

    assert_eq!(drain_player_commands(&inbox).len(), 5);
}

#[test]
fn receipt_commands_get_backpressure_while_reserve_is_not_empty() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");
    queue
        .try_send_player_command(end_scrub())
        .expect("overflow goes to reserve");
    let _play = inbox.try_receive();

    let while_reserved =
        queue.try_send_ordered_worker_command(WorkerCommand::Player(PlayerCommand::Stop));
    assert_eq!(while_reserved, Err(PlayerWorkerSendError::Full));

    assert_eq!(drain_player_commands(&inbox), vec![end_scrub()]);
    let after_drain =
        queue.try_send_ordered_worker_command(WorkerCommand::Player(PlayerCommand::Stop));
    assert_eq!(after_drain, Ok(()));
}

#[test]
fn full_reserve_rejects_ordered_commands_and_counts_the_loss() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");
    for _ in 0..COMMAND_RESERVE_CAPACITY {
        queue
            .try_send_player_command(PlayerCommand::TogglePlayback)
            .expect("reserve has room");
    }

    let rejected = queue.try_send_player_command(PlayerCommand::TogglePlayback);
    // Latest-value команда при полном резерве всё ещё может слиться с хвостом? Нет:
    // хвост — ordered toggle, поэтому громкость тоже отвергается.
    let rejected_volume = queue.try_send_player_command(PlayerCommand::SetVolume(0.5));

    assert_eq!(rejected, Err(PlayerWorkerSendError::Full));
    assert_eq!(rejected_volume, Err(PlayerWorkerSendError::Full));
    assert_eq!(inbox.current_reserve_episode().rejected, 2);
    assert_eq!(
        drain_player_commands(&inbox).len(),
        1 + COMMAND_RESERVE_CAPACITY
    );
    // Эпизод закончился вместе с опустошением резерва: счётчики начинаются заново.
    assert_eq!(
        inbox.current_reserve_episode(),
        CommandReserveEpisode::default()
    );
}

#[test]
fn latest_value_still_coalesces_into_full_reserve_tail() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");
    for _ in 1..COMMAND_RESERVE_CAPACITY {
        queue
            .try_send_player_command(PlayerCommand::TogglePlayback)
            .expect("reserve has room");
    }
    queue
        .try_send_player_command(PlayerCommand::SetVolume(0.1))
        .expect("last reserve slot");

    let coalesced = queue.try_send_player_command(PlayerCommand::SetVolume(0.9));

    assert_eq!(coalesced, Ok(CommandAdmission::CoalescedInReserve));
    assert_eq!(
        drain_player_commands(&inbox).last(),
        Some(&PlayerCommand::SetVolume(0.9))
    );
}

#[test]
fn lossless_command_is_accepted_beyond_reserve_capacity_in_order() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");
    for _ in 0..COMMAND_RESERVE_CAPACITY {
        queue
            .try_send_player_command(PlayerCommand::TogglePlayback)
            .expect("reserve has room");
    }

    let lossless = queue.send_lossless_worker_command(WorkerCommand::Player(PlayerCommand::Stop));

    assert_eq!(lossless, Ok(()));
    assert_eq!(
        drain_player_commands(&inbox).last(),
        Some(&PlayerCommand::Stop)
    );
}

#[test]
fn shutdown_bypass_overtakes_reserved_commands() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");
    queue
        .try_send_player_command(end_scrub())
        .expect("overflow goes to reserve");
    let _play = inbox.try_receive();

    queue
        .try_send_bypassing_reserve(WorkerCommand::Player(PlayerCommand::Shutdown))
        .expect("main queue has room");

    assert_eq!(
        drain_player_commands(&inbox),
        vec![PlayerCommand::Shutdown, end_scrub()]
    );
}

#[test]
fn closed_inbox_reports_disconnected_instead_of_silent_acceptance() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");
    drop(inbox);

    assert_eq!(
        queue.try_send_player_command(PlayerCommand::Pause),
        Err(PlayerWorkerSendError::Disconnected)
    );
    assert_eq!(
        queue.send_lossless_worker_command(WorkerCommand::Player(PlayerCommand::Stop)),
        Err(PlayerWorkerSendError::Disconnected)
    );
    assert_eq!(
        queue.try_send_ordered_worker_command(WorkerCommand::Player(PlayerCommand::Stop)),
        Err(PlayerWorkerSendError::Disconnected)
    );
}

#[test]
fn reserve_survives_dropped_senders_until_worker_reads_it() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");
    queue
        .try_send_player_command(end_scrub())
        .expect("overflow goes to reserve");
    drop(queue);

    assert_eq!(
        drain_player_commands(&inbox),
        vec![PlayerCommand::Play, end_scrub()]
    );
    assert!(matches!(inbox.try_receive(), Err(CommandInboxDisconnected)));
}

#[test]
fn reserve_wake_fires_once_for_a_sleeping_worker() {
    let (queue, inbox) = small_queue(1);
    queue
        .try_send_player_command(PlayerCommand::Play)
        .expect("fill main queue");

    queue
        .try_send_player_command(PlayerCommand::SetVolume(0.2))
        .expect("reserve");
    queue
        .try_send_player_command(PlayerCommand::SetVolume(0.3))
        .expect("coalesce");

    assert_eq!(inbox.reserve_wake_receiver().len(), 1);
}

#[test]
fn delivery_classification_marks_terminal_commands_as_ordered() {
    for command in [
        end_scrub(),
        PlayerCommand::begin_scrub(),
        PlayerCommand::TogglePlayback,
        PlayerCommand::Play,
        PlayerCommand::Pause,
        PlayerCommand::Seek(SeekRequest::absolute(MediaTime::from_millis(5))),
        PlayerCommand::ToggleMute {
            fallback_volume: 1.0,
        },
        PlayerCommand::Stop,
    ] {
        assert_eq!(
            PlayerCommandDelivery::of(&command),
            PlayerCommandDelivery::Ordered,
            "{command:?}"
        );
    }
    assert_eq!(
        PlayerCommandDelivery::of(&PlayerCommand::SetVolume(0.5)),
        PlayerCommandDelivery::LatestValue(LatestValueCommandKind::Volume)
    );
    assert_eq!(
        PlayerCommandDelivery::of(&PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_millis(5)
        ))),
        PlayerCommandDelivery::LatestValue(LatestValueCommandKind::ScrubTarget)
    );
}
