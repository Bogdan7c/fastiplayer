//! Частная шина D-Bus стенда: она будит настоящий плеер по `.service`, записанному установщиком.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::error::EnvironmentError;
use super::process::ManagedProcess;

/// Шаблон конфигурации шины сеанса: только наш сокет, только наш каталог служб.
fn bus_config(address: &str, services_dir: &Path) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>{address}</listen>
  <auth>EXTERNAL</auth>
  <servicedir>{services}</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        services = services_dir.display()
    )
}

/// Что нужно знать, чтобы поднять шину.
pub(crate) struct BusSpec<'a> {
    /// Куда писать конфигурацию шины.
    pub(crate) config_path: &'a Path,
    /// Короткий каталог для сокета (пути Unix-сокетов ограничены ~108 байтами).
    pub(crate) socket_path: &'a Path,
    /// Каталог `.service`: сюда пишет установщик, отсюда шина будит плеер.
    pub(crate) services_dir: &'a Path,
    /// Куда уходит stderr шины, а с ним и stderr активированного плеера.
    pub(crate) log_path: &'a Path,
    /// Окружение шины: его наследует каждый запущенный ею процесс.
    pub(crate) env: &'a [(OsString, OsString)],
    /// Переменные, которые надо убрать из унаследованного окружения.
    pub(crate) env_removed: &'a [OsString],
}

/// Живая частная шина; при `Drop` убивается вся её группа, включая активированный плеер.
pub(crate) struct PrivateBus {
    address: String,
    _process: ManagedProcess,
}

impl PrivateBus {
    /// Запускает `dbus-daemon` и ждёт, пока он напечатает адрес.
    pub(crate) fn start(spec: &BusSpec<'_>) -> Result<Self, EnvironmentError> {
        let address = format!("unix:path={}", spec.socket_path.display());
        std::fs::write(spec.config_path, bus_config(&address, spec.services_dir)).map_err(
            |source| EnvironmentError::Io {
                action: "write bus config",
                path: spec.config_path.to_path_buf(),
                source,
            },
        )?;
        let log = File::create(spec.log_path).map_err(|source| EnvironmentError::Io {
            action: "create player log",
            path: spec.log_path.to_path_buf(),
            source,
        })?;
        let mut command = Command::new("dbus-daemon");
        command
            .arg(format!("--config-file={}", spec.config_path.display()))
            .args(["--nofork", "--print-address=1"])
            .envs(spec.env.iter().map(|(name, value)| (name, value)))
            .stdout(Stdio::piped())
            // Запущенный шиной плеер пишет в stderr демона: так получается player.log.
            .stderr(Stdio::from(log));
        for name in spec.env_removed {
            command.env_remove(name);
        }
        let mut process =
            ManagedProcess::spawn(command).map_err(|source| EnvironmentError::Spawn {
                program: "dbus-daemon".to_owned(),
                source,
            })?;
        let printed = process
            .take_stdout()
            .and_then(|stdout| BufReader::new(stdout).lines().next())
            .and_then(Result::ok)
            .filter(|line| line.starts_with("unix:"))
            .ok_or_else(|| EnvironmentError::BusDidNotStart {
                log: PathBuf::from(spec.log_path),
            })?;
        Ok(Self {
            address: printed,
            _process: process,
        })
    }

    /// Адрес шины для `DBUS_SESSION_BUS_ADDRESS` и просьбы «перечитай службы».
    pub(crate) fn address(&self) -> &str {
        &self.address
    }
}
