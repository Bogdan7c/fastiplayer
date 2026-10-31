//! Пульт и стол: квитанции, порядок команд, «новее заменяет», отказы.

use std::time::{Duration, Instant};

use player_core::PlaybackState;

use super::super::test_support::stub_prepared_media;
use super::*;

fn labels(commands: &[RemoteCommand]) -> Vec<&'static str> {
    commands.iter().map(RemoteCommand::log_label).collect()
}

#[test]
fn commands_reach_the_window_in_order_with_distinct_load_tickets() {
    let desk = PlaybackDesk::new();
    let mut remote = WindowRemote::new(desk.clone());

    let first = remote
        .load(stub_prepared_media(), LoadStart::Playing)
        .expect("queued");
    remote.pause().expect("queued");
    remote.seek(Duration::from_secs(3)).expect("queued");
    remote.set_rate(PlaybackRate::NORMAL).expect("queued");
    remote.play().expect("queued");

    let commands = desk.take_commands();
    assert_eq!(
        labels(&commands),
        ["load", "pause", "seek", "set_rate", "play"]
    );
    let RemoteCommand::Load(order) = &commands[0] else {
        panic!("load first");
    };
    assert_eq!(order.ticket, first);
    assert_eq!(order.start, LoadStart::Playing);
    assert!(matches!(commands[2], RemoteCommand::Seek(target) if target == Duration::from_secs(3)));
    assert!(desk.take_commands().is_empty(), "taking empties the queue");

    let second = remote
        .load(stub_prepared_media(), LoadStart::Paused)
        .expect("queued");
    assert_ne!(first, second, "every load gets its own ticket");
}

#[test]
fn a_newer_load_replaces_an_unexecuted_older_one_but_keeps_transport_commands() {
    let desk = PlaybackDesk::new();
    let mut remote = WindowRemote::new(desk.clone());
    let _older = remote
        .load(stub_prepared_media(), LoadStart::Playing)
        .expect("queued");
    remote.pause().expect("queued");
    let newer = remote
        .load(stub_prepared_media(), LoadStart::Paused)
        .expect("queued");

    let commands = desk.take_commands();
    assert_eq!(labels(&commands), ["pause", "load"]);
    let RemoteCommand::Load(order) = &commands[1] else {
        panic!("load last");
    };
    assert_eq!(
        order.ticket, newer,
        "only the newest generation is installed"
    );
}

#[test]
fn pump_returns_what_the_window_published_last() {
    let desk = PlaybackDesk::new();
    let mut remote = WindowRemote::new(desk.clone());
    let initial = remote.pump(Instant::now());
    assert_eq!(initial.state, PlaybackState::Idle);
    assert_eq!(initial.shown_load, None);
    assert_eq!(initial.fault, None);

    let mut published = initial.clone();
    published.position = Duration::from_millis(1500);
    published.state = PlaybackState::Playing;
    desk.publish(published.clone());
    assert_eq!(remote.pump(Instant::now()), published);
}

#[test]
fn a_closed_window_and_a_full_queue_refuse_commands_instead_of_losing_them() {
    let desk = PlaybackDesk::new();
    let mut remote = WindowRemote::new(desk.clone());
    for _ in 0..REMOTE_COMMAND_CAPACITY {
        remote.play().expect("room left");
    }
    assert!(matches!(
        remote.pause(),
        Err(BrowserPlaybackCommandError::Window(
            WindowCommandRefusal::QueueFull
        ))
    ));
    assert_eq!(desk.take_commands().len(), REMOTE_COMMAND_CAPACITY);

    remote.play().expect("room again");
    desk.close_window();
    assert!(
        desk.take_commands().is_empty(),
        "closing drops what was waiting"
    );
    assert!(matches!(
        remote.load(stub_prepared_media(), LoadStart::Playing),
        Err(BrowserPlaybackCommandError::Window(
            WindowCommandRefusal::WindowClosed
        ))
    ));
}

#[test]
fn the_desk_keeps_the_latest_downloaded_range_until_the_window_closes() {
    let desk = PlaybackDesk::new();
    let mut remote = WindowRemote::new(desk.clone());
    assert!(
        !desk.downloaded_range().is_known(),
        "nothing is known at first"
    );

    let (_spans, range) = crate::downloaded_range::test_support::FakeDownloadedSpans::range(vec![
        Duration::ZERO..Duration::from_secs(2),
    ]);
    remote.offer_downloaded_range(range);
    assert!(desk.downloaded_range().covers(Duration::from_secs(1)));
    // Сведение — не команда: в очередь окна оно не попадает.
    assert!(desk.take_commands().is_empty());

    desk.close_window();
    assert!(!desk.downloaded_range().is_known());
    let (_spans, later) = crate::downloaded_range::test_support::FakeDownloadedSpans::range(vec![
        Duration::ZERO..Duration::from_secs(2),
    ]);
    remote.offer_downloaded_range(later);
    assert!(
        !desk.downloaded_range().is_known(),
        "a closed window keeps nothing"
    );
}
