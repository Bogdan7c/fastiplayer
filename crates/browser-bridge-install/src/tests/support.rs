//! Общие помощники тестов: фейковый HOME, фальшивые `flatpak` и шина.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use desktop_integration::SessionBusReloadError;
use tempfile::TempDir;

use crate::{
    BrowserBridgeInstaller, BrowserTarget, FlatpakError, FlatpakTool, InstallEnvironment,
    InstallSource, SessionBusReloader, SourceProblem,
};

/// Содержимое «посредника» в тестах: произвольные байты с нулями.
pub(super) const HOST_BYTES: &[u8] = b"#!fake-host\0\x01\x02\xff binary payload";

/// Права на запись по умолчанию для каталогов теста.
const EXECUTABLE_MODE: u32 = 0o755;

/// Права и поведение фейкового `flatpak`: разделяемое состояние, видимое тесту.
#[derive(Default)]
pub(super) struct FlatpakState {
    pub apps: Mutex<Vec<String>>,
    pub permissions: Mutex<BTreeMap<String, String>>,
    pub grants: Mutex<Vec<String>>,
    pub list_error: Mutex<Option<FlatpakError>>,
    pub permissions_error: Mutex<Option<FlatpakError>>,
    pub grant_error: Mutex<Option<FlatpakError>>,
}

/// Текст прав без доступа к шине (как у браузера из коробки).
pub(super) const PERMISSIONS_WITHOUT_BUS: &str = "[Context]\nshared=ipc;network;\nsockets=wayland;x11;\n\n[Session Bus Policy]\norg.freedesktop.Notifications=talk\n";
/// Текст прав с полным доступом к шине.
pub(super) const PERMISSIONS_FULL_BUS: &str = "[Context]\nsockets=session-bus;wayland;\n";
/// Текст прав, который появляется после `override --talk-name=...Fastiplayer.*`.
pub(super) const PERMISSIONS_WITH_TALK_MASK: &str =
    "[Context]\nsockets=wayland;\n\n[Session Bus Policy]\nio.github.Bogdan7c.Fastiplayer.*=talk\n";

/// Фейковый `flatpak`: `grant_bridge_bus_access` записывается и открывает доступ.
#[derive(Clone, Default)]
pub(super) struct FakeFlatpak {
    pub state: Arc<FlatpakState>,
}

impl FakeFlatpak {
    /// Приложение установлено, права заданы текстом.
    pub fn with_app(self, app_id: &str, permissions_text: &str) -> Self {
        self.state
            .apps
            .lock()
            .expect("apps")
            .push(app_id.to_owned());
        self.state
            .permissions
            .lock()
            .expect("permissions")
            .insert(app_id.to_owned(), permissions_text.to_owned());
        self
    }

    /// Какие app ID получили доступ через `grant_bridge_bus_access`.
    pub fn grants(&self) -> Vec<String> {
        self.state.grants.lock().expect("grants").clone()
    }
}

impl FlatpakTool for FakeFlatpak {
    fn installed_apps(&self) -> Result<Vec<String>, FlatpakError> {
        if let Some(error) = self.state.list_error.lock().expect("error").clone() {
            return Err(error);
        }
        Ok(self.state.apps.lock().expect("apps").clone())
    }

    fn permissions(&self, app_id: &str) -> Result<String, FlatpakError> {
        if let Some(error) = self.state.permissions_error.lock().expect("error").clone() {
            return Err(error);
        }
        Ok(self
            .state
            .permissions
            .lock()
            .expect("permissions")
            .get(app_id)
            .cloned()
            .unwrap_or_default())
    }

    fn grant_bridge_bus_access(&self, app_id: &str) -> Result<(), FlatpakError> {
        if let Some(error) = self.state.grant_error.lock().expect("error").clone() {
            return Err(error);
        }
        self.state
            .grants
            .lock()
            .expect("grants")
            .push(app_id.to_owned());
        self.state
            .permissions
            .lock()
            .expect("permissions")
            .insert(app_id.to_owned(), PERMISSIONS_WITH_TALK_MASK.to_owned());
        Ok(())
    }
}

/// Фейковая шина: считает просьбы «перечитай службы».
#[derive(Clone, Default)]
pub(super) struct RecordingReloader {
    calls: Arc<AtomicUsize>,
    fail: Arc<Mutex<Option<SessionBusReloadError>>>,
}

impl RecordingReloader {
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn failing_with(error: SessionBusReloadError) -> Self {
        let reloader = Self::default();
        *reloader.fail.lock().expect("fail") = Some(error);
        reloader
    }
}

impl SessionBusReloader for RecordingReloader {
    fn reload_service_files(&self) -> Result<(), SessionBusReloadError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.fail.lock().expect("fail").clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Фейковый мир: HOME, каталог PATH, каталог snap, «плеер» и «посредник» рядом.
pub(super) struct Fixture {
    pub root: TempDir,
    pub home: PathBuf,
    pub bin_dir: PathBuf,
    pub snap_dir: PathBuf,
    pub app_dir: PathBuf,
    pub player: PathBuf,
    pub environment: InstallEnvironment,
}

impl Fixture {
    /// Мир с обычным именем HOME.
    pub fn new() -> Self {
        Self::with_home_name("home")
    }

    /// Мир, в котором HOME называется как угодно (пробелы, кавычки, кириллица).
    pub fn with_home_name(home_name: &str) -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        let home = root.path().join(home_name);
        let bin_dir = root.path().join("bin");
        let snap_dir = root.path().join("snap-bin");
        let app_dir = root.path().join("app");
        for directory in [&home, &bin_dir, &snap_dir, &app_dir] {
            std::fs::create_dir_all(directory).expect("fixture dir");
        }
        let player = app_dir.join("fastiplayer");
        write_executable(&player, b"#!fake-player");
        write_executable(&app_dir.join("fastiplayer-browser-host"), HOST_BYTES);
        let mut environment = InstallEnvironment::from_variables(
            Some(home.clone().into_os_string()),
            None,
            None,
            Some(std::env::join_paths([&bin_dir]).expect("PATH")),
        )
        .expect("environment");
        environment.snap_bin_dirs = vec![snap_dir.clone()];
        Self {
            root,
            home,
            bin_dir,
            snap_dir,
            app_dir,
            player,
            environment,
        }
    }

    /// Положить «браузер» в PATH.
    pub fn install_browser_executable(&self, name: &str) {
        write_executable(&self.bin_dir.join(name), b"#!fake-browser");
    }

    /// Источник установки этого мира.
    pub fn source(&self) -> Result<InstallSource, SourceProblem> {
        InstallSource::from_player_executable(self.player.clone())
    }

    /// Установщик с фейками.
    pub fn installer(
        &self,
        flatpak: &FakeFlatpak,
        reloader: &RecordingReloader,
    ) -> BrowserBridgeInstaller {
        BrowserBridgeInstaller::new(
            self.environment.clone(),
            self.source(),
            Box::new(flatpak.clone()),
            Box::new(reloader.clone()),
        )
    }

    /// Подготовить «установленный браузер» для цели каталога.
    pub fn install_target(&self, target: &BrowserTarget, flatpak: FakeFlatpak) -> FakeFlatpak {
        match target.packaging {
            crate::Packaging::System { executables } => {
                self.install_browser_executable(executables[0]);
                flatpak
            }
            crate::Packaging::Flatpak { app_id } => flatpak.with_app(app_id, PERMISSIONS_FULL_BUS),
        }
    }

    /// Все файлы и симлинки под HOME: путь -> содержимое (для сравнения «до/после»).
    pub fn snapshot_home(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        collect_files(&self.home, &mut files);
        files
    }

    /// Только пути файлов под HOME.
    pub fn home_file_paths(&self) -> BTreeSet<PathBuf> {
        self.snapshot_home().into_keys().collect()
    }
}

fn collect_files(directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
        }
    }
}

/// Пишет исполняемый файл.
pub(super) fn write_executable(path: &Path, contents: &[u8]) {
    std::fs::write(path, contents).expect("write file");
    set_mode(path, EXECUTABLE_MODE);
}

/// Ставит права.
pub(super) fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Права файла (младшие 9 бит).
pub(super) fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

/// Пишет файл, создавая каталоги.
pub(super) fn write_with_parents(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("parents");
    std::fs::write(path, contents).expect("write");
}

/// Цель каталога по ID.
pub(super) fn target(id: &str) -> &'static BrowserTarget {
    crate::find_target(&crate::BrowserTargetId::new(id)).expect("target exists in the catalog")
}

/// ID цели.
pub(super) fn target_id(id: &str) -> crate::BrowserTargetId {
    crate::BrowserTargetId::new(id)
}

/// Файл подключения цели (первый путь) под данным HOME.
pub(super) fn first_manifest_path(fixture: &Fixture, id: &str) -> PathBuf {
    crate::layout::manifest_file_paths(&fixture.environment, target(id))
        .into_iter()
        .next()
        .expect("target has a manifest path")
}

/// Подключает цель и возвращает установщик с его фейками.
pub(super) fn connected(
    fixture: &Fixture,
    id: &str,
) -> (BrowserBridgeInstaller, FakeFlatpak, RecordingReloader) {
    let flatpak = fixture.install_target(target(id), FakeFlatpak::default());
    let reloader = RecordingReloader::default();
    let installer = fixture.installer(&flatpak, &reloader);
    installer.connect(&target_id(id)).expect("connect");
    (installer, flatpak, reloader)
}
