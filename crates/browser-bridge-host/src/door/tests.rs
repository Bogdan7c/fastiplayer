//! Тесты двери: перевод каждой ошибки клиента D-Bus в свой вид отказа.

use super::*;

#[test]
fn every_client_error_maps_to_the_agreed_failure() {
    let cases = [
        (
            BrowserEntryCallError::SessionBusUnavailable("no bus".to_owned()),
            DoorFailure::PlayerUnavailable,
        ),
        (
            BrowserEntryCallError::NotInstalled,
            DoorFailure::PlayerUnavailable,
        ),
        (
            BrowserEntryCallError::ActivationFailed {
                detail: "exec failed".to_owned(),
            },
            DoorFailure::PlayerUnavailable,
        ),
        (
            BrowserEntryCallError::ForeignOwner,
            DoorFailure::PlayerUnavailable,
        ),
        (
            BrowserEntryCallError::Transport("reset".to_owned()),
            DoorFailure::PlayerUnavailable,
        ),
        (
            BrowserEntryCallError::NotResponding,
            DoorFailure::PlayerNotResponding,
        ),
        (
            BrowserEntryCallError::ShuttingDown,
            DoorFailure::PlayerShuttingDown,
        ),
        (
            BrowserEntryCallError::Rejected {
                detail: "too large".to_owned(),
            },
            DoorFailure::MessageRefusedByTransport,
        ),
        (
            BrowserEntryCallError::UnsupportedPlatform,
            DoorFailure::UnsupportedPlatform,
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(door_failure_for(&error), expected, "{error:?}");
    }
}

#[test]
fn only_a_broken_bus_connection_is_dropped() {
    // Сломанное соединение заменяется новым; «плеер молчит» или «завершается» —
    // нет: следующий звонок тем же звонящим разбудит новый процесс плеера.
    assert!(connection_is_broken(
        &BrowserEntryCallError::SessionBusUnavailable(String::new())
    ));
    assert!(connection_is_broken(&BrowserEntryCallError::Transport(
        String::new()
    )));
    for kept in [
        BrowserEntryCallError::NotResponding,
        BrowserEntryCallError::ShuttingDown,
        BrowserEntryCallError::NotInstalled,
        BrowserEntryCallError::ForeignOwner,
    ] {
        assert!(!connection_is_broken(&kept), "{kept:?}");
    }
}

#[test]
fn a_door_on_a_missing_bus_reports_player_unavailable_and_retries_next_time() {
    // Адрес шины, на котором никто не слушает: каждый звонок — честное «недоступен»,
    // без паники и без кэширования мёртвого соединения.
    let directory = std::env::temp_dir().join(format!(
        "fastiplayer-browser-host-door-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let address = format!("unix:path={}", directory.join("missing-bus").display());
    let mut door = EntryClientDoor::new(
        BrowserEntryBus::Address(address),
        BrowserEntryClientConfig::default(),
    );
    assert_eq!(door.ring("{}"), Err(DoorFailure::PlayerUnavailable));
    assert!(door.client.is_none());
    assert_eq!(door.ring("{}"), Err(DoorFailure::PlayerUnavailable));
    std::fs::remove_dir_all(&directory).unwrap();
}
