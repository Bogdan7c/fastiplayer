//! Итоги операций установщика экстеншена человеческим языком (сессия 07a).
//!
//! Чистые функции: исход или ошибку установщика превращают в список строк для показа
//! под списком браузеров. Ошибки ядра содержат отладочный вывод и пути, поэтому в интерфейс
//! идут только наши тексты.

use browser_bridge_install::{
    BrowserTargetId, ExtensionInstallError, ExtensionInstallOutcome, ExtensionInstaller, HintStep,
    LaunchError, RemoveHintOutcome,
};

use super::extension_view::unreadable_reason_text;
use super::operations::browser_name;

/// Кнопка Install: итог для показа (пусто нельзя — любой исход что-то сообщает).
pub(super) fn install_extension_notes(
    installer: &ExtensionInstaller,
    target: &BrowserTargetId,
) -> Vec<String> {
    let browser = browser_name(target);
    match installer.install(target) {
        Ok(ExtensionInstallOutcome::AlreadyInstalled) => {
            vec![format!("The extension is already installed in {browser}.")]
        }
        Ok(ExtensionInstallOutcome::AlreadyInstalledButDisabled) => vec![format!(
            "The extension is installed but turned off in {browser}. Turn it on in the extensions page."
        )],
        Ok(ExtensionInstallOutcome::HintWritten { .. }) => vec![format!(
            "Restart {browser} to finish installing the extension."
        )],
        Ok(ExtensionInstallOutcome::StorePageOpened { .. }) => {
            vec![format!(
                "Opened the store page in {browser} — press Add there."
            )]
        }
        Err(error) => vec![format!(
            "Could not install the extension in {browser}: {}",
            error_text(&error)
        )],
    }
}

/// Кнопка Cancel: итог для показа. Отменяется только ещё не случившаяся установка;
/// уже установленный экстеншен убирает человек на странице расширений браузера.
pub(super) fn cancel_extension_install_notes(
    installer: &ExtensionInstaller,
    target: &BrowserTargetId,
) -> Vec<String> {
    let browser = browser_name(target);
    match installer.cancel_pending_install(target) {
        Ok(RemoveHintOutcome::Removed { .. }) => {
            vec![format!(
                "Cancelled. {browser} will not install the extension."
            )]
        }
        Ok(RemoveHintOutcome::NothingToRemove) => vec!["Nothing to cancel.".to_owned()],
        Ok(RemoveHintOutcome::ForeignHintKept) => {
            vec!["The hint file belongs to another program and was left in place.".to_owned()]
        }
        Err(error) => vec![format!(
            "Could not cancel the extension install in {browser}: {}",
            error_text(&error)
        )],
    }
}

/// Причина ошибки по-английски, без отладочного вывода и без полных путей.
pub(super) fn error_text(error: &ExtensionInstallError) -> String {
    match error {
        ExtensionInstallError::UnsupportedPlatform => {
            "this platform is not supported yet".to_owned()
        }
        ExtensionInstallError::UnknownTarget => "unknown browser".to_owned(),
        ExtensionInstallError::BrowserNotInstalled => "the browser is not installed".to_owned(),
        ExtensionInstallError::NotPublished => {
            "the extension is not published in the store yet".to_owned()
        }
        ExtensionInstallError::Unsupported(_) => {
            "the extension cannot be installed in this browser".to_owned()
        }
        ExtensionInstallError::ForeignHintInTheWay { .. } => {
            "another program's hint file is in the way".to_owned()
        }
        ExtensionInstallError::FlatpakUnavailable(_) => {
            "the `flatpak` tool did not answer, so Flatpak browsers cannot be checked".to_owned()
        }
        ExtensionInstallError::NoHintForThisBrowser => {
            "this browser has no install hint".to_owned()
        }
        ExtensionInstallError::ProfileUnreadable { path, kind } => format!(
            "cannot read {}: {}",
            file_name_of(path),
            unreadable_reason_text(*kind)
        ),
        ExtensionInstallError::HintIo { step, path, kind } => {
            format!("{} {} ({kind})", hint_step_text(*step), file_name_of(path))
        }
        ExtensionInstallError::HintAtomicWrite { path, .. } => {
            format!("could not save {}", file_name_of(path))
        }
        ExtensionInstallError::Launch(LaunchError::ProgramNotFound) => {
            "the program that opens the browser was not found".to_owned()
        }
        ExtensionInstallError::Launch(LaunchError::Spawn(kind)) => {
            format!("could not start the browser ({kind})")
        }
    }
}

/// Что делали с файлом-подсказкой, когда ОС отказала.
fn hint_step_text(step: HintStep) -> &'static str {
    match step {
        HintStep::ReadHint => "could not read",
        HintStep::CreateDirectory => "could not create the folder for",
        HintStep::WriteHint => "could not write",
        HintStep::RemoveHint => "could not delete",
    }
}

/// Имя файла без каталога (путь содержит имя пользователя).
fn file_name_of(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || "a file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}
