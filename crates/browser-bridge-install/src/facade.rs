//! Фасад установщика: единственная точка входа приложения.
//!
//! Все методы блокирующие (файлы, процесс `flatpak`, вызов шины): приложение
//! вызывает их из фонового потока. Швы (окружение, источник, `flatpak`, шина)
//! передаются в конструктор, поэтому тесты подставляют фейки.

use std::path::PathBuf;

use crate::allow::{AllowError, grant_bus_access};
use crate::catalog::{BrowserTarget, BrowserTargetId, TARGETS, find_target};
use crate::detection::{Installation, detect_installation};
use crate::disconnect::{DisconnectWarning, disconnect_target};
use crate::environment::InstallEnvironment;
use crate::error::{InstallError, InstallErrorKind, InstallStep, InstallWarning};
use crate::flatpak::FlatpakTool;
use crate::install::connect_target;
use crate::reloader::SessionBusReloader;
use crate::source::{InstallSource, SourceProblem};
use crate::status::{
    BrowserBridgeStatus, BrowserReport, BrowserSurvey, StatusInputs, SurveyError, packaging_kind,
    target_status,
};

/// Итог подключения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOutcome {
    /// Предупреждения (перезаписан чужой файл, шина не перечитала службы, ...).
    pub warnings: Vec<InstallWarning>,
    /// Статус цели после подключения (у Flatpak может быть `NeedsFlatpakPermission`).
    pub status: BrowserBridgeStatus,
}

/// Итог отключения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectOutcome {
    /// Удалённые файлы.
    pub removed_files: Vec<PathBuf>,
    /// Предупреждения (чужой файл оставлен, служба принадлежит другой копии, ...).
    pub warnings: Vec<DisconnectWarning>,
    /// Удалённые пустые каталоги, созданные нами (`browser-host/`, `fastiplayer/`).
    pub removed_directories: Vec<PathBuf>,
    /// Статус цели после отключения.
    pub status: BrowserBridgeStatus,
}

/// Итог выдачи доступа Flatpak-браузеру.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowOutcome {
    /// Статус цели после выдачи доступа, пересчитанный заново.
    pub status: BrowserBridgeStatus,
}

/// Установщик моста в браузеры.
pub struct BrowserBridgeInstaller {
    environment: InstallEnvironment,
    source: Result<InstallSource, SourceProblem>,
    flatpak: Box<dyn FlatpakTool>,
    bus_reloader: Box<dyn SessionBusReloader>,
}

impl BrowserBridgeInstaller {
    /// Собирает установщик из швов. `source` — результат проверки источника: проблема
    /// не роняет установщик, а попадает в обзор и блокирует действия.
    pub fn new(
        environment: InstallEnvironment,
        source: Result<InstallSource, SourceProblem>,
        flatpak: Box<dyn FlatpakTool>,
        bus_reloader: Box<dyn SessionBusReloader>,
    ) -> Self {
        Self {
            environment,
            source,
            flatpak,
            bus_reloader,
        }
    }

    /// Обзор: все установленные браузеры каталога и статус каждого.
    pub fn survey(&self) -> Result<BrowserSurvey, SurveyError> {
        if !cfg!(target_os = "linux") {
            return Err(SurveyError::UnsupportedPlatform);
        }
        let (flatpak_apps, flatpak_problem) = match self.flatpak.installed_apps() {
            Ok(apps) => (apps, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let inputs = self.status_inputs();
        let browsers = TARGETS
            .iter()
            .filter_map(|target| {
                let installation = detect_installation(target, &self.environment, &flatpak_apps);
                if installation == Installation::Absent {
                    return None;
                }
                Some(BrowserReport {
                    target: target.target_id(),
                    display_name: target.display_name,
                    verification: target.verification,
                    packaging: packaging_kind(target),
                    status: target_status(&inputs, target, &installation),
                })
            })
            .collect();
        Ok(BrowserSurvey {
            source: self.source.as_ref().map(|_source| ()).map_err(Clone::clone),
            browsers,
            flatpak_problem,
        })
    }

    /// Подключает браузер: посредник, служба, файл подключения.
    pub fn connect(&self, target_id: &BrowserTargetId) -> Result<ConnectOutcome, InstallError> {
        let target = self.resolve_target(target_id)?;
        let source = self.source_or_error()?;
        let installation = self.detect(target);
        let warnings = connect_target(
            &self.environment,
            source,
            target,
            &installation,
            self.bus_reloader.as_ref(),
        )?;
        Ok(ConnectOutcome {
            warnings,
            status: self.status_of(target),
        })
    }

    /// Отключает браузер: убирает только свои файлы и только те, что больше никому не нужны.
    pub fn disconnect(
        &self,
        target_id: &BrowserTargetId,
    ) -> Result<DisconnectOutcome, InstallError> {
        let target = self.resolve_target(target_id)?;
        // Отключение не требует годного источника: достаточно знать путь плеера.
        let player = match &self.source {
            Ok(source) => Some(source.player_executable()),
            Err(problem) => problem.player_executable(),
        };
        let report = disconnect_target(
            &self.environment,
            player,
            target,
            self.bus_reloader.as_ref(),
        )?;
        Ok(DisconnectOutcome {
            removed_files: report.removed_files,
            warnings: report.warnings,
            removed_directories: report.removed_directories,
            status: self.status_of(target),
        })
    }

    /// «Allow»: даёт Flatpak-браузеру доступ к имени браузерного режима и пересчитывает статус.
    pub fn allow_flatpak_bus_access(
        &self,
        target_id: &BrowserTargetId,
    ) -> Result<AllowOutcome, AllowError> {
        let target = find_target(target_id).ok_or(AllowError::UnknownTarget)?;
        grant_bus_access(self.flatpak.as_ref(), target)?;
        Ok(AllowOutcome {
            status: self.status_of(target),
        })
    }

    fn resolve_target(
        &self,
        target_id: &BrowserTargetId,
    ) -> Result<&'static BrowserTarget, InstallError> {
        find_target(target_id).ok_or_else(|| {
            InstallError::of(InstallStep::CheckTarget, InstallErrorKind::UnknownTarget)
        })
    }

    fn source_or_error(&self) -> Result<&InstallSource, InstallError> {
        self.source.as_ref().map_err(|problem| {
            InstallError::of(
                InstallStep::CheckTarget,
                InstallErrorKind::Source(problem.clone()),
            )
        })
    }

    fn status_inputs(&self) -> StatusInputs<'_> {
        StatusInputs {
            environment: &self.environment,
            source: &self.source,
            flatpak: self.flatpak.as_ref(),
        }
    }

    /// Как установлена одна цель (список Flatpak запрашивается только для Flatpak-целей).
    fn detect(&self, target: &BrowserTarget) -> Installation {
        let flatpak_apps = if target.flatpak_app_id().is_some() {
            // Сбой утилиты здесь равен «приложение не найдено»: действие будет отказано
            // понятной ошибкой `TargetNotInstalled`, а причина видна в обзоре.
            self.flatpak.installed_apps().unwrap_or_default()
        } else {
            Vec::new()
        };
        detect_installation(target, &self.environment, &flatpak_apps)
    }

    /// Свежий статус одной цели.
    fn status_of(&self, target: &BrowserTarget) -> BrowserBridgeStatus {
        let installation = self.detect(target);
        target_status(&self.status_inputs(), target, &installation)
    }
}
