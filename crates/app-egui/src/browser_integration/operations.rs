//! Операции установщика, которые выполняет фоновый поток, и их человеческие итоги.
//!
//! Здесь нет состояния: [`run_operation`] берёт установщик и операцию, возвращает
//! [`OperationResult`]. Тексты предупреждений и ошибок собираются тут же (в потоке),
//! чтобы владелец в UI-потоке только складывал готовые строки.

use browser_bridge_install::{
    AllowError, BrowserBridgeInstaller, BrowserSurvey, BrowserTargetId, DisconnectWarning,
    InstallError, InstallWarning, SurveyError, find_target,
};

/// Результат обзора.
pub(crate) type SurveyResult = Result<BrowserSurvey, SurveyError>;

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
}

/// Итог операции.
#[derive(Debug)]
pub(crate) enum OperationResult {
    /// Обзор выполнен (или не может быть выполнен на этой платформе).
    Survey(SurveyResult),
    /// Подключение, отключение или выдача доступа выполнены; `notes` — предупреждения или
    /// текст ошибки для показа под списком (пусто — всё прошло без замечаний).
    Change { notes: Vec<String> },
}

/// Текст предупреждения о том, что шина не перечитала службы (общий для connect/disconnect).
const BUS_RELOAD_FAILED_NOTE: &str = "Could not ask D-Bus to reload services — restart your session if the browser cannot reach Fastiplayer";

/// Выполняет операцию (блокирующе; вызывается из фонового потока).
pub(crate) fn run_operation(
    installer: &BrowserBridgeInstaller,
    operation: &BrowserOperation,
) -> OperationResult {
    match operation {
        BrowserOperation::Survey => OperationResult::Survey(installer.survey()),
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
fn browser_name(target: &BrowserTargetId) -> String {
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
