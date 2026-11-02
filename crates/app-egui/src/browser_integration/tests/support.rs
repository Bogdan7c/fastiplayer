//! Общие фейки и фикстура тестов браузерной интеграции: модель вида, правило предложения, маршрутизация и
//! поведение владельца с НАСТОЯЩИМ установщиком над временным HOME (фейки только у
//! `flatpak` и шины; настоящий HOME и настоящая шина не трогаются).

pub(super) use std::path::{Path, PathBuf};
pub(super) use std::sync::{Arc, Mutex};
pub(super) use std::time::{Duration, Instant};

pub(super) use browser_bridge_install::{
    BrowserBridgeInstaller, BrowserBridgeStatus, BrowserReport, BrowserSurvey, BrowserTargetId,
    FlatpakError, FlatpakTool, InstallEnvironment, InstallSource, PackagingKind,
    SessionBusReloadError, SessionBusReloader, SourceProblem, SurveyError, UnavailableReason,
    Verification,
};
pub(super) use tempfile::TempDir;

pub(super) use super::super::offer::offer_verdict;
pub(super) use super::super::panel_view::{PanelInputs, build_panel_view, row_view};
pub(super) use super::super::routing::{RoutingEffect, route_settings_actions};
pub(super) use super::super::*;
pub(super) use crate::app_wake::AppWakeOwner;
pub(super) use crate::settings_ui::SettingsUiAction;
pub(super) use crate::settings_ui::browser_panel::{
    BrowserButtonAvailability, BrowserCommand, BrowserPanelActivity, BrowserRowView,
    BrowserScanState,
};
pub(super) use browser_bridge_protocol::extension_identity::NATIVE_HOST_NAME;

// ---------- фейки и фикстура ----------

/// Права Flatpak-приложения без доступа к шине.
pub(super) const PERMISSIONS_WITHOUT_BUS: &str = "[Context]\nsockets=wayland;x11;\n";
/// Права после `override --talk-name=...Fastiplayer.*`.
pub(super) const PERMISSIONS_WITH_TALK_MASK: &str =
    "[Context]\nsockets=wayland;\n\n[Session Bus Policy]\nio.github.Bogdan7c.Fastiplayer.*=talk\n";

/// Фейковый `flatpak`: набор приложений и права, `override` открывает доступ.
#[derive(Clone, Default)]
pub(super) struct FakeFlatpak {
    pub(super) apps: Arc<Mutex<Vec<(String, String)>>>,
}

impl FakeFlatpak {
    pub(super) fn with_app(self, app_id: &str, permissions: &str) -> Self {
        self.apps
            .lock()
            .expect("apps")
            .push((app_id.to_owned(), permissions.to_owned()));
        self
    }
}

impl FlatpakTool for FakeFlatpak {
    fn installed_apps(&self) -> Result<Vec<String>, FlatpakError> {
        Ok(self
            .apps
            .lock()
            .expect("apps")
            .iter()
            .map(|(id, _)| id.clone())
            .collect())
    }

    fn permissions(&self, app_id: &str) -> Result<String, FlatpakError> {
        Ok(self
            .apps
            .lock()
            .expect("apps")
            .iter()
            .find(|(id, _)| id == app_id)
            .map(|(_, permissions)| permissions.clone())
            .unwrap_or_default())
    }

    fn grant_bridge_bus_access(&self, app_id: &str) -> Result<(), FlatpakError> {
        for (id, permissions) in self.apps.lock().expect("apps").iter_mut() {
            if id == app_id {
                *permissions = PERMISSIONS_WITH_TALK_MASK.to_owned();
            }
        }
        Ok(())
    }
}

/// Фейковая шина: просьба «перечитай службы» всегда успешна.
pub(super) struct QuietReloader;

impl SessionBusReloader for QuietReloader {
    fn reload_service_files(&self) -> Result<(), SessionBusReloadError> {
        Ok(())
    }
}

/// Временный мир: HOME, PATH с «браузерами», каталог с «плеером» и «посредником».
pub(super) struct World {
    pub(super) _root: TempDir,
    pub(super) home: PathBuf,
    pub(super) bin_dir: PathBuf,
    pub(super) player: PathBuf,
    pub(super) environment: InstallEnvironment,
}

pub(super) fn write_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, b"#!fake").expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

impl World {
    pub(super) fn new() -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        let home = root.path().join("home");
        let bin_dir = root.path().join("bin");
        let snap_dir = root.path().join("snap-bin");
        let app_dir = root.path().join("app");
        for directory in [&home, &bin_dir, &snap_dir, &app_dir] {
            std::fs::create_dir_all(directory).expect("dir");
        }
        let player = app_dir.join("fastiplayer");
        write_executable(&player);
        write_executable(&app_dir.join("fastiplayer-browser-host"));
        let mut environment = InstallEnvironment::from_variables(
            Some(home.clone().into_os_string()),
            None,
            None,
            Some(std::env::join_paths([&bin_dir]).expect("PATH")),
        )
        .expect("environment");
        environment.snap_bin_dirs = vec![snap_dir];
        Self {
            _root: root,
            home,
            bin_dir,
            player,
            environment,
        }
    }

    pub(super) fn install_browser(&self, executable: &str) {
        write_executable(&self.bin_dir.join(executable));
    }

    pub(super) fn installer(&self, flatpak: FakeFlatpak) -> BrowserBridgeInstaller {
        BrowserBridgeInstaller::new(
            self.environment.clone(),
            InstallSource::from_player_executable(self.player.clone()),
            Box::new(flatpak),
            Box::new(QuietReloader),
        )
    }

    pub(super) fn integration(
        &self,
        flatpak: FakeFlatpak,
        offer: OfferSetting,
    ) -> BrowserIntegration {
        BrowserIntegration::start_with_installer(
            self.installer(flatpak),
            AppWakePort::disconnected(AppWakeOwner::BrowserIntegration),
            offer,
        )
    }

    /// Файл подключения Chromium из пакета.
    pub(super) fn chromium_manifest(&self) -> PathBuf {
        self.home
            .join(".config/chromium/NativeMessagingHosts")
            .join(format!("{NATIVE_HOST_NAME}.json"))
    }
}

/// Ждёт, пока владелец закончит всё (операцию и следующий обзор).
pub(super) fn wait_until_idle(integration: &mut BrowserIntegration) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let _ = integration.poll();
        if integration.panel_view().activity == BrowserPanelActivity::Idle {
            return;
        }
        assert!(Instant::now() < deadline, "the operation never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn row<'a>(view: &'a BrowserPanelView, name: &str) -> &'a BrowserRowView {
    view.rows
        .iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("no row {name}: {:?}", view.rows))
}

pub(super) fn button_labels(row: &BrowserRowView) -> Vec<&'static str> {
    row.buttons.iter().map(|button| button.label).collect()
}

pub(super) fn survey_with(
    source: Result<(), SourceProblem>,
    statuses: Vec<BrowserBridgeStatus>,
) -> BrowserSurvey {
    BrowserSurvey {
        source,
        browsers: statuses.into_iter().map(report).collect(),
        flatpak_problem: None,
    }
}

pub(super) fn shown_offer(world: &World) -> (BrowserIntegration, NotificationId) {
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Enabled);
    wait_until_idle(&mut integration);
    assert_eq!(
        integration.take_startup_offer_verdict(),
        Some(OfferVerdict::Show)
    );
    let toast_id = NotificationId::for_tests(7);
    integration.mark_offer_shown(toast_id);
    (integration, toast_id)
}

pub(super) fn report(status: BrowserBridgeStatus) -> BrowserReport {
    BrowserReport {
        target: BrowserTargetId::new("chromium"),
        display_name: "Chromium",
        verification: Verification::Verified,
        packaging: PackagingKind::Package,
        status,
    }
}
