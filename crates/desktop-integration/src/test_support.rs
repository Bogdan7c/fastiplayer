//! Общие помощники тестов, которым нужна настоящая шина D-Bus (только Linux).

#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use zbus::blocking::{Connection, connection};

/// Политика как у системного `session.conf`. Каталоги служб (`<servicedir>`)
/// добавляются отдельно и только по явной просьбе теста: без них шина не может
/// автозапустить настоящие службы рабочего стола, и тест от них не зависит.
const BUS_CONFIG_HEAD: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
"#;

/// Уникальный номер шины внутри тестового процесса.
static NEXT_BUS_NUMBER: AtomicUsize = AtomicUsize::new(0);

/// Частный `dbus-daemon`: тест не трогает настоящий рабочий стол пользователя.
pub(crate) struct PrivateSessionBus {
    pub(crate) address: String,
    child: Child,
    config_file: PathBuf,
}

impl PrivateSessionBus {
    /// Шина БЕЗ каталогов служб: автозапуска нет вообще.
    pub(crate) fn spawn() -> Self {
        Self::spawn_configured(None, &[])
    }

    /// Шина с одним каталогом служб `servicedir` (для тестов D-Bus-активации).
    ///
    /// `daemon_environment` добавляется в окружение демона; активированные службы
    /// наследуют его (плюс `DBUS_STARTER_ADDRESS`), так тест передаёт им параметры.
    pub(crate) fn spawn_with_servicedir(
        servicedir: &Path,
        daemon_environment: &[(&str, &OsStr)],
    ) -> Self {
        Self::spawn_configured(Some(servicedir), daemon_environment)
    }

    fn spawn_configured(servicedir: Option<&Path>, daemon_environment: &[(&str, &OsStr)]) -> Self {
        let config_file = std::env::temp_dir().join(format!(
            "fastiplayer-desktop-integration-test-{}-{}.conf",
            std::process::id(),
            NEXT_BUS_NUMBER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut config = BUS_CONFIG_HEAD.to_owned();
        if let Some(servicedir) = servicedir {
            config.push_str(&format!(
                "  <servicedir>{}</servicedir>\n",
                servicedir.display()
            ));
        }
        config.push_str("</busconfig>\n");
        std::fs::write(&config_file, config).expect("bus config");

        let mut command = Command::new("dbus-daemon");
        command
            .arg(format!("--config-file={}", config_file.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped());
        for (name, value) in daemon_environment {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("hermetic dbus-daemon must start");
        let stdout = child.stdout.take().expect("dbus address pipe");
        let address = BufReader::new(stdout)
            .lines()
            .next()
            .expect("dbus address line")
            .expect("valid dbus address");
        Self {
            address,
            child,
            config_file,
        }
    }

    /// Замораживает сам `dbus-daemon` (SIGSTOP): шина перестаёт отвечать на запросы.
    /// Размораживать не нужно, `Drop` убивает демон (SIGKILL действует и на остановленный).
    pub(crate) fn freeze(&self) {
        let status = Command::new("kill")
            .args(["-STOP", &self.child.id().to_string()])
            .status()
            .expect("kill must run");
        assert!(status.success(), "SIGSTOP of the bus daemon failed");
    }

    pub(crate) fn connect(&self) -> zbus::Result<Connection> {
        connection::Builder::address(self.address.as_str())?.build()
    }

    pub(crate) fn connect_with_timeout(&self, timeout: Duration) -> zbus::Result<Connection> {
        connection::Builder::address(self.address.as_str())?
            .method_timeout(timeout)
            .build()
    }
}

impl Drop for PrivateSessionBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config_file);
    }
}
