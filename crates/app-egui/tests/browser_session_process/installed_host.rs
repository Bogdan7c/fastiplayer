//! Установленный мост «как его видит браузер» (сессия 06, пакет P4).
//!
//! Здесь НЕТ рукописных `.service` и путей к посреднику: файлы пишет настоящий
//! установщик `BrowserBridgeInstaller::connect`, а дальше тест делает то же, что
//! браузер: читает файл подключения, берёт из него `path`, запускает посредника с
//! аргументами браузера и говорит с ним рамками native messaging. Частная шина
//! смотрит в каталог служб из фейкового `$HOME` — поэтому плеер будит именно
//! `.service`, записанный установщиком. Доказано:
//! - Chromium и Firefox: файл подключения → посредник → шина → настоящий плеер →
//!   пропуск → записанный поток доходит до кадров на показе, посредник выходит с
//!   кодом 0 (`installed_host_launched_from_*_manifest_plays_frames`);
//! - плеер удалили после установки: посредник быстро и честно отвечает
//!   `player_unavailable`, не виснет и тоже выходит с кодом 0
//!   (`player_removed_after_install_makes_host_refuse_quickly`).
//!
//! HOME теста содержит пробелы, кириллицу, двойные кавычки и апостроф: путь
//! посредника в файле подключения и каталог служб обязаны пережить это целиком.

use std::os::unix::fs::PermissionsExt;
use std::sync::OnceLock;

use browser_bridge_install::{
    BrowserBridgeInstaller, BrowserTargetId, FlatpakError, FlatpakTool, InstallEnvironment,
    InstallSource, SessionBusReloadError, SessionBusReloader,
};
use browser_bridge_protocol::extension_identity::{FIREFOX_EXTENSION_ID, NATIVE_HOST_NAME};
use browser_bridge_protocol::{ControlRefusal, GrantOutcome, RefusalReason, ReplyTarget};

use desktop_integration::BROWSER_SESSION_BUS_NAME;

use super::native_host::{
    NativeHost, attach_with_timeout, chromium_argument,
    finish_playback_and_check_it_reached_render, native_host_binary, send_step,
};
use super::*;

/// Имя HOME: пробелы, кириллица, двойные кавычки и апостроф (путь ПЛЕЕРА апостроф
/// иметь не может, а HOME — может: он попадает в `path` файла подключения и в
/// каталог служб шины).
const AWKWARD_HOME_NAME: &str = "Дом \"Богдана\" и it's home";

/// Потолок ответа посредника, когда плеера уже нет (на деле — доли секунды).
const PLAYER_UNAVAILABLE_DEADLINE: Duration = Duration::from_secs(10);

// ---- швы установщика ----------------------------------------------------------------

/// `flatpak` без приложений: пакетные цели от него не зависят.
struct NoFlatpakApps;

impl FlatpakTool for NoFlatpakApps {
    fn installed_apps(&self) -> Result<Vec<String>, FlatpakError> {
        Ok(Vec::new())
    }

    fn permissions(&self, _app_id: &str) -> Result<String, FlatpakError> {
        panic!("no Flatpak apps are installed in this test");
    }

    fn grant_bridge_bus_access(&self, _app_id: &str) -> Result<(), FlatpakError> {
        panic!("no Flatpak apps are installed in this test");
    }
}

/// Просит перечитать службы именно ЧАСТНУЮ шину теста, а не рабочий стол разработчика.
struct PrivateBusReloader {
    address: String,
}

impl SessionBusReloader for PrivateBusReloader {
    fn reload_service_files(&self) -> Result<(), SessionBusReloadError> {
        desktop_integration::reload_session_bus_services(
            &BrowserEntryBus::Address(self.address.clone()),
            Duration::from_secs(5),
        )
    }
}

// ---- каталог установки плеера ---------------------------------------------------------

/// Каталог с плеером и посредником рядом.
///
/// Если вложенная сборка посредника положила его рядом с `fastiplayer` — берём
/// настоящий каталог сборки. Иначе копируем обоих во временный каталог; `lib/` с
/// FFmpeg (RUNPATH `$ORIGIN/lib`) подключаем ссылкой.
fn player_with_host_next_to_it() -> &'static Path {
    static INSTALL_DIR: OnceLock<PathBuf> = OnceLock::new();
    INSTALL_DIR.get_or_init(|| {
        let player = Path::new(env!("CARGO_BIN_EXE_fastiplayer"));
        let built_host = native_host_binary();
        let player_dir = player.parent().expect("player has a directory");
        if built_host.parent() == Some(player_dir) {
            return player_dir.to_path_buf();
        }
        let install_dir = std::env::temp_dir().join(format!(
            "fastiplayer-installed-host-source-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&install_dir).expect("install dir");
        std::fs::copy(player, install_dir.join("fastiplayer")).expect("copy player");
        std::fs::copy(built_host, install_dir.join("fastiplayer-browser-host")).expect("copy host");
        std::os::unix::fs::symlink(player_dir.join("lib"), install_dir.join("lib"))
            .expect("link vendored FFmpeg");
        install_dir
    })
}

/// Копия плеера и посредника во временном каталоге с ASCII-путём (для теста удаления).
fn copy_player_with_host(destination: &Path) -> PathBuf {
    std::fs::create_dir_all(destination).expect("copy dir");
    let player = destination.join("fastiplayer");
    std::fs::copy(env!("CARGO_BIN_EXE_fastiplayer"), &player).expect("copy player");
    std::fs::copy(
        native_host_binary(),
        destination.join("fastiplayer-browser-host"),
    )
    .expect("copy host");
    player
}

// ---- установленный мир --------------------------------------------------------------

/// Фейковый HOME с установленным мостом и частной шиной, смотрящей в его службы.
struct InstalledBridge {
    /// Порядок полей — порядок уничтожения: шина уходит раньше каталогов.
    bus: PrivateBusWithBrowserSession,
    home: PathBuf,
    _root: tempfile::TempDir,
}

impl InstalledBridge {
    /// Поднимает шину на каталоге служб HOME и подключает `target_id` установщиком.
    ///
    /// `browser_executable` — имя «браузера» в фейковом PATH (так цель обнаруживается).
    fn connect(bus_name: &str, player: PathBuf, target_id: &str, browser_executable: &str) -> Self {
        let root = tempfile::tempdir().expect("temp root");
        let home = root.path().join(AWKWARD_HOME_NAME);
        let bin_dir = root.path().join("bin");
        std::fs::create_dir_all(&home).expect("fake HOME");
        std::fs::create_dir_all(&bin_dir).expect("fake PATH dir");
        let browser = bin_dir.join(browser_executable);
        std::fs::write(&browser, b"#!stub-browser").expect("stub browser");
        std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755))
            .expect("stub browser is executable");

        let environment = InstallEnvironment::from_variables(
            Some(home.clone().into_os_string()),
            None,
            None,
            Some(std::env::join_paths([&bin_dir]).expect("PATH")),
        )
        .expect("install environment");
        // Шина смотрит в каталог, куда установщик кладёт `.service` — и только в него.
        let services_dir = environment.xdg_data_home.join("dbus-1/services");
        let bus = PrivateBusWithBrowserSession::start_with_services(
            bus_name,
            None,
            &TestVideo::Headless,
            &[],
            &ServiceFiles::DirectoryOf(services_dir),
        );
        let installer = BrowserBridgeInstaller::new(
            environment,
            InstallSource::from_player_executable(player),
            Box::new(NoFlatpakApps),
            Box::new(PrivateBusReloader {
                address: bus.address.clone(),
            }),
        );
        let outcome = installer
            .connect(&BrowserTargetId::new(target_id))
            .expect("the installer connects the browser");
        assert!(
            outcome.warnings.is_empty(),
            "a clean install has no warnings: {:?}",
            outcome.warnings
        );
        let service_file = home
            .join(".local/share/dbus-1/services")
            .join(format!("{BROWSER_SESSION_BUS_NAME}.service"));
        assert!(
            service_file.is_file(),
            "the installer wrote the D-Bus service file the bus watches"
        );
        Self {
            bus,
            home,
            _root: root,
        }
    }

    /// Файл подключения, как его найдёт браузер.
    fn manifest_path(&self, relative_to_home: &str) -> PathBuf {
        let path = self
            .home
            .join(relative_to_home)
            .join(format!("{NATIVE_HOST_NAME}.json"));
        assert!(path.is_file(), "the installer wrote {}", path.display());
        path
    }

    /// Запускает посредника ровно по файлу подключения: `path` из JSON, рабочий
    /// каталог — каталог посредника, аргументы — `arguments`.
    fn launch_host_from_manifest(&self, manifest: &Path, arguments: &[String]) -> NativeHost {
        let manifest_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(manifest).expect("manifest is readable"))
                .expect("manifest is JSON");
        assert_eq!(manifest_json["name"], NATIVE_HOST_NAME);
        assert_eq!(manifest_json["type"], "stdio");
        let host_path = PathBuf::from(
            manifest_json["path"]
                .as_str()
                .expect("manifest has a string path"),
        );
        assert!(
            host_path.is_absolute() && host_path.is_file(),
            "manifest path points to the copied host: {}",
            host_path.display()
        );
        let mut command = Command::new(&host_path);
        command
            .args(arguments)
            .current_dir(host_path.parent().expect("host has a directory"));
        NativeHost::spawn_command(command, &self.bus.address)
    }
}

/// Играет записанный поток через уже запущенного посредника до кадров.
fn play_recorded_stream_through(bridge: &InstalledBridge, host: &mut NativeHost) {
    host.say_hello();
    let grant = host.open_session(1, "tab-12-video-3");
    assert_eq!(grant.outcome, GrantOutcome::Started);
    bridge.bus.wait_for_log_line("Browser session opened");

    let mut socket = attach_with_timeout(&grant);
    let (steps, _bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    for step in steps {
        send_step(&mut socket, step);
    }
    // Браузер закрыл порт: посредник выходит штатно, а пропуск продолжает играть.
    assert_eq!(host.close_input_and_wait().code(), Some(0));
    finish_playback_and_check_it_reached_render(&bridge.bus, &mut socket);
}

// ---- тесты --------------------------------------------------------------------------

#[test]
fn installed_host_launched_from_chromium_manifest_plays_frames() {
    let player = player_with_host_next_to_it().join("fastiplayer");
    let bridge = InstalledBridge::connect("installed-chromium", player, "chromium", "chromium");
    let manifest = bridge.manifest_path(".config/chromium/NativeMessagingHosts");
    // Chromium передаёт первым аргументом источник вызова — наш экстеншен.
    let mut host = bridge.launch_host_from_manifest(&manifest, &chromium_argument());
    play_recorded_stream_through(&bridge, &mut host);
}

#[test]
fn installed_host_launched_from_firefox_manifest_plays_frames() {
    let player = player_with_host_next_to_it().join("fastiplayer");
    let bridge = InstalledBridge::connect("installed-firefox", player, "firefox", "firefox");
    let manifest = bridge.manifest_path(".mozilla/native-messaging-hosts");
    // Firefox передаёт путь к файлу подключения и ID экстеншена.
    let mut host = bridge.launch_host_from_manifest(
        &manifest,
        &[
            manifest.display().to_string(),
            FIREFOX_EXTENSION_ID.to_owned(),
        ],
    );
    play_recorded_stream_through(&bridge, &mut host);
}

#[test]
fn player_removed_after_install_makes_host_refuse_quickly() {
    // Установка идёт с КОПИИ плеера: настоящую сборку удалять нельзя.
    let copy_dir = tempfile::tempdir().expect("player copy dir");
    let player_copy = copy_player_with_host(&copy_dir.path().join("player"));
    let bridge = InstalledBridge::connect("installed-removed", player_copy, "chromium", "chromium");
    let manifest = bridge.manifest_path(".config/chromium/NativeMessagingHosts");
    let mut host = bridge.launch_host_from_manifest(&manifest, &chromium_argument());

    // Пользователь удалил плеер; `.service` и посредник остались от установки.
    std::fs::remove_dir_all(copy_dir.path().join("player")).expect("player removed");

    let asked_at = Instant::now();
    let reply = host.ask(super::HELLO);
    let elapsed = asked_at.elapsed();
    let PlayerControlMessage::Refused(ControlRefusal {
        in_reply_to,
        reason,
    }) = reply
    else {
        panic!("an absent player must produce a refusal, got {reply:?}");
    };
    assert_eq!(in_reply_to, ReplyTarget::Hello {});
    assert_eq!(reason, RefusalReason::PlayerUnavailable {});
    assert!(
        elapsed < PLAYER_UNAVAILABLE_DEADLINE,
        "the refusal took {elapsed:?}"
    );
    eprintln!("player_unavailable refusal arrived after {elapsed:?}");

    // Порт закрыт браузером: посредник и в этом случае выходит штатно.
    assert_eq!(host.close_input_and_wait().code(), Some(0));
}
