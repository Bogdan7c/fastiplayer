//! Невидимый экран: `kwin_wayland --virtual` на собственной крошечной шине.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::error::EnvironmentError;
use super::process::ManagedProcess;

/// Имя сокета Wayland невидимого экрана (внутри каталога выполнения).
pub(crate) const WAYLAND_SOCKET_NAME: &str = "wl-fpbench";
/// Размер экрана: как у владельца на ноутбуке, влезает любое тестовое видео.
const VIRTUAL_SCREEN_WIDTH: &str = "1280";
const VIRTUAL_SCREEN_HEIGHT: &str = "720";
/// Сколько ждать сокет kwin (запуск обычно занимает секунду).
const SOCKET_WAIT: Duration = Duration::from_secs(15);

/// kwin с его шиной; при `Drop` убиваются обе группы.
pub(crate) struct VirtualDisplay {
    _kwin: ManagedProcess,
    _bus: ManagedProcess,
}

impl VirtualDisplay {
    /// Поднимает экран. `runtime_dir` — короткий приватный каталог (0700) для сокетов.
    pub(crate) fn start(runtime_dir: &Path, run_dir: &Path) -> Result<Self, EnvironmentError> {
        // kwin нужна сессионная шина, но НЕ та, где живёт плеер: у него свой чистый мир.
        let bus_address = format!("unix:path={}", runtime_dir.join("kwin-bus").display());
        let bus_config = run_dir.join("kwin-bus.conf");
        std::fs::write(&bus_config, kwin_bus_config(&bus_address)).map_err(|source| {
            EnvironmentError::Io {
                action: "write kwin bus config",
                path: bus_config.clone(),
                source,
            }
        })?;
        let mut bus_command = Command::new("dbus-daemon");
        bus_command
            .arg(format!("--config-file={}", bus_config.display()))
            .args(["--nofork", "--print-address=1"])
            // Адрес печатается, когда демон уже слушает сокет: ждём эту строку и только
            // потом запускаем kwin (иначе kwin мог стартовать раньше, чем шина готова).
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut bus =
            ManagedProcess::spawn(bus_command).map_err(|source| EnvironmentError::Spawn {
                program: "dbus-daemon (kwin)".to_owned(),
                source,
            })?;
        wait_for_bus_address(&mut bus, &bus_config)?;

        let kwin_home = run_dir.join("kwin-home");
        let log_path = run_dir.join("kwin.log");
        let log = File::create(&log_path).map_err(|source| EnvironmentError::Io {
            action: "create kwin log",
            path: log_path.clone(),
            source,
        })?;
        let log_for_stderr = log.try_clone().map_err(|source| EnvironmentError::Io {
            action: "clone kwin log",
            path: log_path.clone(),
            source,
        })?;
        let mut kwin_command = Command::new("kwin_wayland");
        // Чистое окружение (как `env -i`): kwin не должен подхватить сеанс владельца.
        kwin_command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &kwin_home)
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .env("XDG_CONFIG_HOME", kwin_home.join("config"))
            .env("XDG_DATA_HOME", kwin_home.join("data"))
            .env("XDG_CACHE_HOME", kwin_home.join("cache"))
            .env("XDG_STATE_HOME", kwin_home.join("state"))
            .env("DBUS_SESSION_BUS_ADDRESS", &bus_address)
            .args(["--virtual", "--no-lockscreen"])
            .args(["--width", VIRTUAL_SCREEN_WIDTH])
            .args(["--height", VIRTUAL_SCREEN_HEIGHT])
            .arg(format!("--socket={WAYLAND_SOCKET_NAME}"))
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_for_stderr));
        std::fs::create_dir_all(&kwin_home).map_err(|source| EnvironmentError::Io {
            action: "create kwin home",
            path: kwin_home.clone(),
            source,
        })?;
        let mut kwin =
            ManagedProcess::spawn(kwin_command).map_err(|source| EnvironmentError::Spawn {
                program: "kwin_wayland".to_owned(),
                source,
            })?;
        wait_for_socket(&mut kwin, &runtime_dir.join(WAYLAND_SOCKET_NAME), &log_path)?;
        Ok(Self {
            _kwin: kwin,
            _bus: bus,
        })
    }

    /// Переменные для всего, что рисует на этом экране (только Wayland, без X11).
    pub(crate) fn drawing_env(runtime_dir: &Path) -> Vec<(OsString, OsString)> {
        vec![
            ("WAYLAND_DISPLAY".into(), WAYLAND_SOCKET_NAME.into()),
            ("XDG_RUNTIME_DIR".into(), runtime_dir.as_os_str().to_owned()),
        ]
    }
}

/// Ждёт строку `unix:...` от шины kwin; пустой вывод (демон упал) — ошибка с путём конфигурации.
fn wait_for_bus_address(bus: &mut ManagedProcess, config: &Path) -> Result<(), EnvironmentError> {
    let printed = bus
        .take_stdout()
        .and_then(|stdout| BufReader::new(stdout).lines().next())
        .and_then(Result::ok)
        .filter(|line| line.starts_with("unix:"));
    match printed {
        Some(_) => Ok(()),
        None => Err(EnvironmentError::BusDidNotStart {
            log: config.to_path_buf(),
        }),
    }
}

/// Ждёт появления сокета; если kwin вышел раньше, падает сразу, не дожидаясь таймаута.
fn wait_for_socket(
    kwin: &mut ManagedProcess,
    socket: &Path,
    log: &Path,
) -> Result<(), EnvironmentError> {
    let deadline = Instant::now() + SOCKET_WAIT;
    while Instant::now() < deadline {
        if socket.exists() {
            return Ok(());
        }
        // Если kwin уже умер, сокета не будет никогда: ждать все 15 секунд бессмысленно.
        let exited = kwin
            .wait_timeout(Duration::ZERO)
            .map_err(|source| EnvironmentError::Io {
                action: "poll kwin_wayland",
                path: log.to_path_buf(),
                source,
            })?;
        if let Some(status) = exited {
            return Err(EnvironmentError::DisplayExited {
                status: status.to_string(),
                log: PathBuf::from(log),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(EnvironmentError::DisplayDidNotStart {
        socket: PathBuf::from(socket),
        log: PathBuf::from(log),
    })
}

/// Конфигурация шины kwin: без каталога служб, любые политики разрешены.
fn kwin_bus_config(address: &str) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>{address}</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
"#
    )
}
