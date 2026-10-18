//! Подставной `yt-dlp`: shell-скрипт на PATH, печатающий заранее заданный JSON.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;

use service_ytdlp::{ExtractorProcessInvocation, ExtractorProcessLauncher, YtDlpExtractorAdapter};

struct ScriptedYtDlpLauncher {
    executable_directory: PathBuf,
}

impl ExtractorProcessLauncher for ScriptedYtDlpLauncher {
    fn spawn(
        &self,
        command: &mut Command,
        _invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        let mut command_path = OsString::from(&self.executable_directory);
        if let Some(system_path) = std::env::var_os("PATH") {
            command_path.push(":");
            command_path.push(system_path);
        }
        command.env("PATH", command_path);
        command.spawn()
    }
}

/// Каталог с подставным `yt-dlp`; удаляется вместе с объектом.
pub(crate) struct FakeYtDlp {
    directory: tempfile::TempDir,
}

impl FakeYtDlp {
    pub(crate) fn new(json: &str) -> Self {
        let fake = Self {
            directory: tempfile::tempdir().expect("fake yt-dlp directory"),
        };
        fake.set_json(json);
        fake
    }

    /// Заменяет ответ скрипта; каждый запуск дописывает строку в файл счётчика.
    pub(crate) fn set_json(&self, json: &str) {
        let script = format!(
            "#!/bin/sh\necho run >> '{}'\ncat <<'FASTIPLAYER_EOF'\n{json}\nFASTIPLAYER_EOF\n",
            self.counter_path().display()
        );
        let executable = self.directory.path().join("yt-dlp");
        fs::write(&executable, script).expect("write fake yt-dlp");
        let mut permissions = fs::metadata(&executable).expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("chmod fake yt-dlp");
    }

    fn counter_path(&self) -> PathBuf {
        self.directory.path().join("runs.txt")
    }

    /// Сколько раз yt-dlp запускали.
    pub(crate) fn run_count(&self) -> usize {
        fs::read_to_string(self.counter_path()).map_or(0, |text| text.lines().count())
    }

    pub(crate) fn adapter(&self) -> YtDlpExtractorAdapter {
        YtDlpExtractorAdapter::with_process_launcher(Arc::new(ScriptedYtDlpLauncher {
            executable_directory: Path::new(self.directory.path()).to_path_buf(),
        }))
    }
}
