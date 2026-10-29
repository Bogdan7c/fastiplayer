"""Тесты `scripts/install-desktop-entry.sh`: файл D-Bus activation браузерного режима.

Скрипт запускается в изолированном `XDG_DATA_HOME`, а утилиты кэшей рабочего
стола подменяются заглушками в начале `PATH`, чтобы тест не трогал настоящий
рабочий стол пользователя (кэш KDE sycoca и т. п.).
"""

from __future__ import annotations

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
INSTALLER = REPOSITORY_ROOT / "scripts" / "install-desktop-entry.sh"
SERVICE_NAME = "io.github.Bogdan7c.Fastiplayer.BrowserSession"
DESKTOP_CACHE_UTILITIES = ("kbuildsycoca6", "update-desktop-database", "gtk-update-icon-cache")
# Заглушка busctl записывает свои аргументы: так тест видит просьбу перечитать
# службы, не трогая настоящую шину пользователя.
BUSCTL_RECORDER = '#!/bin/sh\nprintf "%s\\n" "$*" >> "$BUSCTL_LOG"\nexit 0\n'
RELOAD_CALL = "--user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig"


class InstallDesktopEntryTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temporary = tempfile.TemporaryDirectory()
        self.root = Path(self._temporary.name)
        self.data_home = self.root / "data"
        self.stub_bin = self.root / "stub-bin"
        self.stub_bin.mkdir()
        for utility in DESKTOP_CACHE_UTILITIES:
            self._write_executable(self.stub_bin / utility, "#!/bin/sh\nexit 0\n")
        self._write_executable(self.stub_bin / "busctl", BUSCTL_RECORDER)
        self.busctl_log = self.root / "busctl.log"

    def tearDown(self) -> None:
        self._temporary.cleanup()

    def _write_executable(self, path: Path, content: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        path.chmod(path.stat().st_mode | stat.S_IXUSR)

    def _run_installer(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        environment = dict(os.environ)
        environment["XDG_DATA_HOME"] = str(self.data_home)
        environment["PATH"] = f"{self.stub_bin}{os.pathsep}{environment['PATH']}"
        environment["BUSCTL_LOG"] = str(self.busctl_log)
        return subprocess.run(
            ["bash", str(INSTALLER), *arguments],
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )

    def _service_file(self) -> Path:
        return self.data_home / "dbus-1" / "services" / f"{SERVICE_NAME}.service"

    def test_installs_activation_file_with_quoted_absolute_binary_path(self) -> None:
        binary = self.root / "dir with space" / "fastiplayer"
        self._write_executable(binary, "#!/bin/sh\nexit 0\n")

        completed = self._run_installer("--binary", str(binary))

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(
            self._service_file().read_text(),
            "[D-BUS Service]\n"
            f"Name={SERVICE_NAME}\n"
            f"Exec='{binary.resolve()}' --browser-session\n",
        )
        # dbus-broker без перечитывания не видит новый файл (сессия 02 моста).
        self.assertEqual(self.busctl_log.read_text().splitlines(), [RELOAD_CALL])

    def test_refuses_binary_path_that_dbus_cannot_quote(self) -> None:
        binary = self.root / "it's here" / "fastiplayer"
        self._write_executable(binary, "#!/bin/sh\nexit 0\n")

        completed = self._run_installer("--binary", str(binary))

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("apostrophe", completed.stderr)
        self.assertFalse(self._service_file().exists())

    def test_uninstall_removes_activation_file(self) -> None:
        binary = self.root / "bin" / "fastiplayer"
        self._write_executable(binary, "#!/bin/sh\nexit 0\n")
        self.assertEqual(self._run_installer("--binary", str(binary)).returncode, 0)
        self.assertTrue(self._service_file().exists())

        completed = self._run_installer("--uninstall")

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertFalse(self._service_file().exists())
        self.assertEqual(self.busctl_log.read_text().splitlines(), [RELOAD_CALL, RELOAD_CALL])


if __name__ == "__main__":
    unittest.main()
