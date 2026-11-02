//! Тесты на настоящей частной шине: файл службы, появившийся ПОСЛЕ старта шины,
//! становится виден после `reload_session_bus_services`.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{SessionBusReloadError, reload_session_bus_services};
use crate::browser_session_entry::{
    BROWSER_SESSION_BUS_NAME, BrowserEntryBus, browser_session_activation_service_file,
};
use crate::test_support::PrivateSessionBus;

const RELOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Имена, которые шина считает запускаемыми (`ListActivatableNames`).
fn activatable_names(bus: &PrivateSessionBus) -> Vec<String> {
    let connection = bus.connect().expect("connect to private bus");
    let reply = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListActivatableNames",
            &(),
        )
        .expect("ListActivatableNames");
    reply.body().deserialize().expect("list of names")
}

/// Каталог служб, созданный до старта шины (пустой).
fn empty_servicedir(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "fastiplayer-bus-reload-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("servicedir");
    directory
}

/// Сквозная картина: файл появился после старта шины, и имя стало запускаемым.
/// (Сам факт вызова `ReloadConfig` закрепляет тест ниже: `dbus-daemon` подхватывает
/// файлы и сам, а `dbus-broker` — только по этой просьбе.)
#[test]
fn service_file_written_after_bus_start_becomes_activatable_after_reload() {
    let servicedir = empty_servicedir("late-file");
    let bus = PrivateSessionBus::spawn_with_servicedir(&servicedir, &[]);
    assert!(
        !activatable_names(&bus).contains(&BROWSER_SESSION_BUS_NAME.to_owned()),
        "name must be unknown before the file exists"
    );
    let content = browser_session_activation_service_file(&servicedir.join("player"))
        .expect("service file content");
    std::fs::write(
        servicedir.join(format!("{BROWSER_SESSION_BUS_NAME}.service")),
        content,
    )
    .expect("write .service after start");

    reload_session_bus_services(
        &BrowserEntryBus::Address(bus.address.clone()),
        RELOAD_TIMEOUT,
    )
    .expect("reload must succeed");

    let names = activatable_names(&bus);
    let _ = std::fs::remove_dir_all(&servicedir);
    assert!(
        names.contains(&BROWSER_SESSION_BUS_NAME.to_owned()),
        "name must be activatable after reload: {names:?}"
    );
}

/// `dbus-daemon` сам замечает новые файлы служб, поэтому одной проверки списка мало:
/// она не отличила бы вызванный `ReloadConfig` от невызванного. Второе соединение
/// подслушивает шину (eavesdrop разрешён политикой тестовой шины) и должно увидеть
/// именно вызов `ReloadConfig` от нашей функции.
#[test]
fn reload_sends_the_reload_config_call_to_the_bus_driver() {
    let bus = PrivateSessionBus::spawn();
    let monitor = bus.connect().expect("monitor connection");
    monitor
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &("eavesdrop='true',type='method_call',member='ReloadConfig'",),
        )
        .expect("AddMatch");
    let (seen_sender, seen_receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for message in zbus::blocking::MessageIterator::from(&monitor).flatten() {
            let header = message.header();
            let member = header.member().map(|member| member.as_str().to_owned());
            let destination = header.destination().map(|name| name.to_string());
            if seen_sender.send((member, destination)).is_err() {
                break;
            }
        }
    });

    reload_session_bus_services(
        &BrowserEntryBus::Address(bus.address.clone()),
        RELOAD_TIMEOUT,
    )
    .expect("reload must succeed");

    let (member, destination) = seen_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("the bus must see a ReloadConfig call");
    assert_eq!(member.as_deref(), Some("ReloadConfig"));
    assert_eq!(destination.as_deref(), Some("org.freedesktop.DBus"));
}

#[test]
fn unreachable_bus_address_is_reported_as_bus_unavailable() {
    let result = reload_session_bus_services(
        &BrowserEntryBus::Address("unix:path=/nonexistent/fastiplayer-bus".to_owned()),
        RELOAD_TIMEOUT,
    );
    assert!(
        matches!(result, Err(SessionBusReloadError::SessionBusUnavailable(_))),
        "{result:?}"
    );
}

#[test]
fn frozen_bus_makes_reload_fail_with_not_responding_within_the_timeout() {
    let bus = PrivateSessionBus::spawn();
    let started_at = Instant::now();

    // Соединение открывается нормально, а демон замирает сразу после подключения:
    // единственная защита от вечного ожидания — таймаут соединения.
    let result = super::linux::reload_with(
        |timeout| {
            let connection = bus.connect_with_timeout(timeout)?;
            bus.freeze();
            Ok(connection)
        },
        Duration::from_millis(400),
    );

    assert_eq!(result, Err(SessionBusReloadError::NotResponding));
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "must not hang"
    );
}
