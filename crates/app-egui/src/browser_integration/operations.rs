//! Операции установщика, которые выполняет фоновый поток, и их человеческие итоги.
//!
//! Здесь нет состояния: [`run_operation`] берёт установщик и операцию, возвращает
//! [`OperationResult`]. Тексты предупреждений и ошибок собираются тут же (в потоке),
//! чтобы владелец в UI-потоке только складывал готовые строки.

use browser_bridge_install::{
    AllowError, BrowserBridgeInstaller, BrowserSurvey, BrowserTargetId, DisconnectWarning,
    ExtensionInstaller, ExtensionSurvey, InstallError, InstallWarning, SurveyError, find_target,
};

use super::extension_operations::{cancel_extension_install_notes, install_extension_notes};

/// Результат обзора мостового подключения (файлы подключения).
pub(crate) type SurveyResult = Result<BrowserSurvey, SurveyError>;

/// Результат обзора экстеншена. Хранится отдельно от [`SurveyResult`]: сбой одного обзора
/// не должен прятать результат другого.
pub(crate) type ExtensionSurveyResult = Result<ExtensionSurvey, SurveyError>;

/// Оба установщика, которыми владеет браузерная интеграция.
pub(crate) struct Installers {
    /// Подключение браузера к плееру (файлы подключения, Flatpak-доступ).
    pub host: BrowserBridgeInstaller,
    /// Установка самого экстеншена.
    pub extension: ExtensionInstaller,
}

/// Оба обзора, выполненные одним заданием; результаты независимы.
#[derive(Debug)]
pub(crate) struct SurveyBundle {
    /// Обзор подключения.
    pub host: SurveyResult,
    /// Обзор экстеншена.
    pub extension: ExtensionSurveyResult,
}

/// Одна операция над установщиком.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserOperation {
    /// Обзор браузеров.
    Survey,
    /// Подключить (или переподключить) цель к этой копии плеера.
    Connect(BrowserTargetId),
    /// Убрать свои файлы цели.
    Disconnect(BrowserTargetId),
    /// Выдать Flatpak-цели доступ к имени моста на шине.
    AllowFlatpakBusAccess(BrowserTargetId),
    /// Поставить экстеншен в браузер цели (подсказка или страница магазина).
    InstallExtension(BrowserTargetId),
    /// Отменить ещё не случившуюся установку экстеншена (убрать нашу подсказку).
    CancelExtensionInstall(BrowserTargetId),
}

/// Итог операции.
#[derive(Debug)]
pub(crate) enum OperationResult {
    /// Оба обзора выполнены (каждый мог закончиться своей ошибкой).
    Survey(SurveyBundle),
    /// Подключение, отключение или выдача доступа выполнены; `notes` — предупреждения или
    /// текст ошибки для показа под списком (пусто — всё прошло без замечаний).
    Change { notes: Vec<String> },
}

/// Текст предупреждения о том, что шина не перечитала службы (общий для connect/disconnect).
const BUS_RELOAD_FAILED_NOTE: &str = "Could not ask D-Bus to reload services — restart your session if the browser cannot reach Fastiplayer";

/// Выполняет операцию (блокирующе; вызывается из фонового потока).
pub(crate) fn run_operation(
    installers: &Installers,
    operation: &BrowserOperation,
) -> OperationResult {
    let installer = &installers.host;
    match operation {
        BrowserOperation::Survey => OperationResult::Survey(SurveyBundle {
            host: installer.survey(),
            extension: installers.extension.survey(),
        }),
        BrowserOperation::InstallExtension(target) => OperationResult::Change {
            notes: install_extension_notes(&installers.extension, target),
        },
        BrowserOperation::CancelExtensionInstall(target) => OperationResult::Change {
            notes: cancel_extension_install_notes(&installers.extension, target),
        },
        BrowserOperation::Connect(target) => OperationResult::Change {
            notes: match installer.connect(target) {
                Ok(outcome) => outcome.warnings.iter().map(connect_warning_note).collect(),
                Err(error) => vec![failure_note("connect", target, &error)],
            },
        },
        BrowserOperation::Disconnect(target) => OperationResult::Change {
            notes: match installer.disconnect(target) {
                Ok(outcome) => outcome
                    .warnings
                    .iter()
                    .map(disconnect_warning_note)
                    .collect(),
                Err(error) => vec![failure_note("disconnect", target, &error)],
            },
        },
        BrowserOperation::AllowFlatpakBusAccess(target) => OperationResult::Change {
            notes: match installer.allow_flatpak_bus_access(target) {
                Ok(_outcome) => Vec::new(),
                Err(error) => vec![allow_failure_note(target, &error)],
            },
        },
    }
}

/// Имя браузера для сообщений (неизвестный id — сам id).
pub(super) fn browser_name(target: &BrowserTargetId) -> String {
    find_target(target).map_or_else(
        || target.as_str().to_owned(),
        |known| known.display_name.to_owned(),
    )
}

/// «Could not connect Chromium: <причина>».
fn failure_note(verb: &str, target: &BrowserTargetId, error: &InstallError) -> String {
    format!("Could not {verb} {}: {error}", browser_name(target))
}

/// «Could not allow Chrome: <причина>».
fn allow_failure_note(target: &BrowserTargetId, error: &AllowError) -> String {
    format!("Could not allow {}: {error}", browser_name(target))
}

/// Предупреждение подключения человеческим языком.
fn connect_warning_note(warning: &InstallWarning) -> String {
    match warning {
        InstallWarning::OverwroteForeignFile { .. } => {
            "Replaced a file that belonged to another program".to_owned()
        }
        InstallWarning::ReplacedDamagedFile { .. } => {
            "Replaced a damaged connection file".to_owned()
        }
        InstallWarning::BusReloadFailed(_) => BUS_RELOAD_FAILED_NOTE.to_owned(),
        InstallWarning::DurabilityUnconfirmed { .. } => {
            "A file may not have reached the disk yet".to_owned()
        }
        InstallWarning::TookOverFromAnotherCopy { .. } => {
            "Took over the connection from another Fastiplayer copy".to_owned()
        }
    }
}

/// Предупреждение отключения человеческим языком.
fn disconnect_warning_note(warning: &DisconnectWarning) -> String {
    match warning {
        DisconnectWarning::NotOurFileLeftInPlace { .. } => {
            "Left a file in place because it does not belong to Fastiplayer".to_owned()
        }
        DisconnectWarning::ServiceFileBelongsToAnotherCopy { .. } => {
            "Left the D-Bus service in place because another Fastiplayer copy uses it".to_owned()
        }
        DisconnectWarning::ServiceFileOwnerUnknown { .. } => {
            "Kept the D-Bus activation file: could not tell which Fastiplayer copy it belongs to"
                .to_owned()
        }
        DisconnectWarning::BusReloadFailed(_) => BUS_RELOAD_FAILED_NOTE.to_owned(),
    }
}
