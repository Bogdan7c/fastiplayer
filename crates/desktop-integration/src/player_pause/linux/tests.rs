//! Тесты паузы на частной шине: поддельные MPRIS-службы записывают вызовы.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::interface;

use super::pause_with;
use crate::mpris_names::{MPRIS_BUS_NAME, MPRIS_OBJECT_PATH};
use crate::test_support::PrivateSessionBus;
use crate::{PlayerPauseError, PlayerPauseOutcome};

/// Поддельный MPRIS `Player`: записывает имена всех полученных методов.
struct FakeMprisPlayer {
    calls: Arc<Mutex<Vec<String>>>,
    /// Не отвечать на `Pause` (имитация зависшего плеера).
    freeze_on_pause: Option<Duration>,
}

impl FakeMprisPlayer {
    fn record(&self, method: &str) {
        self.calls.lock().expect("call log").push(method.to_owned());
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakeMprisPlayer {
    fn pause(&self) {
        self.record("Pause");
        if let Some(freeze) = self.freeze_on_pause {
            std::thread::sleep(freeze);
        }
    }
    fn play(&self) {
        self.record("Play");
    }
    fn play_pause(&self) {
        self.record("PlayPause");
    }
    fn stop(&self) {
        self.record("Stop");
    }
    fn next(&self) {
        self.record("Next");
    }
}

/// Запущенная поддельная служба: держит соединение с именем.
struct FakeMprisService {
    calls: Arc<Mutex<Vec<String>>>,
    _connection: Connection,
}

impl FakeMprisService {
    fn start(bus: &PrivateSessionBus, name: &str, freeze_on_pause: Option<Duration>) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let connection = bus.connect().expect("fake service connection");
        connection
            .object_server()
            .at(
                MPRIS_OBJECT_PATH,
                FakeMprisPlayer {
                    calls: Arc::clone(&calls),
                    freeze_on_pause,
                },
            )
            .expect("register fake player");
        connection.request_name(name).expect("own fake MPRIS name");
        Self {
            calls,
            _connection: connection,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("call log").clone()
    }
}

const CALL_TIMEOUT: Duration = Duration::from_millis(300);

fn pause(bus: &PrivateSessionBus) -> Result<PlayerPauseOutcome, PlayerPauseError> {
    pause_with(|| bus.connect_with_timeout(CALL_TIMEOUT))
}

#[test]
fn pause_reaches_the_fastiplayer_name_once_and_never_touches_browser_players() {
    let bus = PrivateSessionBus::spawn();
    let fastiplayer = FakeMprisService::start(&bus, MPRIS_BUS_NAME, None);
    let browser = FakeMprisService::start(&bus, "org.mpris.MediaPlayer2.chromium.instance1", None);
    let other_instance = FakeMprisService::start(&bus, "org.mpris.MediaPlayer2.fastiplayer2", None);

    let outcome = pause(&bus);

    assert_eq!(outcome, Ok(PlayerPauseOutcome::PauseRequested));
    assert_eq!(fastiplayer.calls(), ["Pause"]);
    assert!(browser.calls().is_empty(), "{:?}", browser.calls());
    assert!(other_instance.calls().is_empty());
}

#[test]
fn no_owner_means_player_not_running_and_nothing_is_activated() {
    // Шина с каталогом служб, где MPRIS-имя плеера МОГЛО БЫ автозапуститься:
    // пауза не имеет права его будить.
    let root = std::env::temp_dir().join(format!(
        "fastiplayer-pause-no-activation-{}",
        std::process::id()
    ));
    let services_directory = root.join("services");
    std::fs::create_dir_all(&services_directory).expect("services dir");
    let marker = root.join("activated");
    let script = root.join("start.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .expect("script");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    std::fs::write(
        services_directory.join(format!("{MPRIS_BUS_NAME}.service")),
        format!(
            "[D-BUS Service]\nName={MPRIS_BUS_NAME}\nExec='{}'\n",
            script.display()
        ),
    )
    .expect("service file");
    let bus = PrivateSessionBus::spawn_with_servicedir(&services_directory, &[]);
    let browser = FakeMprisService::start(&bus, "org.mpris.MediaPlayer2.chromium.instance1", None);

    let outcome = pause(&bus);
    // Дать шине время запустить скрипт, если бы пауза его разбудила.
    std::thread::sleep(Duration::from_millis(300));

    let activated = marker.exists();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(outcome, Ok(PlayerPauseOutcome::PlayerNotRunning));
    assert!(!activated, "pause must not activate the player");
    assert!(browser.calls().is_empty());
}

#[test]
fn frozen_player_gives_not_responding_within_the_call_timeout() {
    let bus = PrivateSessionBus::spawn();
    let player = FakeMprisService::start(&bus, MPRIS_BUS_NAME, Some(Duration::from_secs(3)));

    let started_at = std::time::Instant::now();
    let outcome = pause(&bus);

    assert_eq!(outcome, Err(PlayerPauseError::NotResponding));
    assert!(started_at.elapsed() < Duration::from_secs(2));
    assert_eq!(player.calls(), ["Pause"]);
}

#[test]
fn frozen_bus_daemon_gives_not_responding_instead_of_hanging() {
    // Регрессия: запросы к шине через `DBusProxy` игнорируют `method_timeout` и
    // повисли бы навсегда (на выходе процесса поток паузы ждут `join`).
    let bus = PrivateSessionBus::spawn();
    let started_at = std::time::Instant::now();

    let outcome = pause_with(|| {
        let connection = bus.connect_with_timeout(CALL_TIMEOUT)?;
        bus.freeze();
        Ok(connection)
    });

    assert_eq!(outcome, Err(PlayerPauseError::NotResponding));
    assert!(started_at.elapsed() < Duration::from_secs(2));
}
