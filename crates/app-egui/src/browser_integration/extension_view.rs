//! Строка экстеншена в панели «Browser integration»: состояние установщика экстеншена →
//! текст, кнопки и заметки (сессия 07a).
//!
//! Чистые функции без состояния. Строка экстеншена привязывается к строке браузера по id
//! цели; здесь только перевод [`ExtensionReport`] в [`ExtensionRowView`]. Все пользовательские
//! тексты — английские.
//!
//! # Инварианты
//! - «Cancel» отменяет ещё не случившуюся установку и показывается, только пока
//!   экстеншен не стоит и подсказку положили мы ([`HintState::Ours`]): чужой файл не
//!   предлагаем удалять никогда, а при установленном экстеншене удаление подсказки
//!   ничего бы не изменило (Chromium его не убирает). Убрать установленный экстеншен
//!   может только человек на странице расширений браузера.
//! - Основная кнопка не больше одной и только там, где установка что-то сделает
//!   ([`ExtensionReport::install_action`]).

use browser_bridge_install::{
    ExtensionReport, ExtensionState, ExtensionUnsupported, HintState, InstallAction, UnreadableKind,
};

use crate::settings_ui::browser_panel::{
    BrowserButtonAvailability, BrowserCommand, BrowserHelpLink, BrowserRowButton, ExtensionRowView,
};

/// Справка о сборке для разработчиков: пока экстеншена нет в магазинах.
const DEVELOPMENT_BUILD_HELP_URL: &str = "https://github.com/Bogdan7c/fastiplayer/blob/main/docs/browser-bridge/README.md#installing-a-development-build";
/// Справка про Snap-браузеры (общая с остальной панелью).
const SNAP_HELP_URL: &str =
    "https://github.com/Bogdan7c/fastiplayer/blob/main/docs/browser-bridge/README.md#snap-browsers";

/// Строка экстеншена для одного браузера.
pub(super) fn extension_row_view(report: &ExtensionReport) -> ExtensionRowView {
    let browser = report.display_name;
    let mut row = ExtensionRowView {
        status_text: String::new(),
        buttons: Vec::new(),
        notes: Vec::new(),
        help_link: None,
    };
    match &report.state {
        ExtensionState::Installed => {
            row.status_text = "Extension: installed".to_owned();
        }
        ExtensionState::InstalledButDisabled => {
            row.status_text = format!("Extension: turned off in {browser}");
            row.notes.push(format!(
                "Turn it on in the extensions page of {browser}; Fastiplayer cannot do it for you."
            ));
        }
        ExtensionState::WaitingForBrowserRestart => {
            row.status_text = format!("Extension: restart {browser} to finish installing");
            // Запасной путь, если браузер подсказку проигнорировал.
            row.buttons.extend(primary_button(report));
        }
        ExtensionState::RemovedInBrowser => {
            row.status_text = format!("Extension: removed in {browser}");
            row.buttons.extend(primary_button(report));
        }
        ExtensionState::NotInstalled => {
            row.status_text = "Extension: not installed".to_owned();
            row.buttons.extend(primary_button(report));
        }
        ExtensionState::AwaitingPublication => {
            row.status_text = "Extension: available after publication".to_owned();
            row.help_link = Some(BrowserHelpLink {
                label: "Development build",
                url: DEVELOPMENT_BUILD_HELP_URL,
            });
        }
        ExtensionState::Unsupported(reason) => {
            row.status_text = unsupported_text(*reason, browser);
            if *reason == ExtensionUnsupported::Snap {
                row.help_link = Some(BrowserHelpLink {
                    label: "Why and what to do",
                    url: SNAP_HELP_URL,
                });
            }
        }
        ExtensionState::Unreadable { path, kind } => {
            let file = path
                .file_name()
                .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy());
            row.status_text = format!(
                "Extension: can't read {browser} profile ({file}: {})",
                unreadable_reason_text(*kind)
            );
        }
    }
    match report.hint {
        HintState::Ours if !is_extension_installed(&report.state) => {
            row.buttons.push(BrowserRowButton {
                label: "Cancel",
                command: BrowserCommand::CancelExtensionInstall,
                availability: BrowserButtonAvailability::Enabled,
            });
        }
        HintState::Foreign => row
            .notes
            .push("Another program left an extension hint for this browser".to_owned()),
        HintState::Ours | HintState::Absent => {}
    }
    row
}

/// Экстеншен уже стоит в браузере (включён или отключён): отменять нечего.
fn is_extension_installed(state: &ExtensionState) -> bool {
    matches!(
        state,
        ExtensionState::Installed | ExtensionState::InstalledButDisabled
    )
}

/// Основная кнопка: подпись зависит от того, что сделает установка.
fn primary_button(report: &ExtensionReport) -> Option<BrowserRowButton> {
    let label = match (&report.state, report.install_action?) {
        (ExtensionState::RemovedInBrowser, _) => "Open store page",
        (_, InstallAction::InstallSilently) => "Install extension",
        (_, InstallAction::OpenStorePage) => "Install in browser",
    };
    Some(BrowserRowButton {
        label,
        command: BrowserCommand::InstallExtension,
        availability: BrowserButtonAvailability::Enabled,
    })
}

/// Статус «ставить нельзя» с причиной.
fn unsupported_text(reason: ExtensionUnsupported, browser: &str) -> String {
    match reason {
        ExtensionUnsupported::Snap => "Extension: Snap browsers are not supported".to_owned(),
        ExtensionUnsupported::FlatpakFirefoxFamily => {
            format!("Extension: not supported for {browser} from Flatpak yet")
        }
        ExtensionUnsupported::NoWebStore => {
            format!("Extension: install it manually ({browser} has no web store)")
        }
        ExtensionUnsupported::CatalogMisconfigured => "Extension: unavailable".to_owned(),
    }
}

/// Короткая причина, почему файл профиля не прочитался.
pub(super) fn unreadable_reason_text(kind: UnreadableKind) -> String {
    match kind {
        UnreadableKind::Io(error_kind) => error_kind.to_string(),
        UnreadableKind::TooLarge => "file is too large".to_owned(),
        UnreadableKind::MalformedJson => "not valid JSON".to_owned(),
        UnreadableKind::NotARegularFile => "not a regular file".to_owned(),
    }
}
