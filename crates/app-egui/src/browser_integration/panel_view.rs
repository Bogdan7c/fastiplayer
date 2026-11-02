//! Сборка модели вида панели «Browser integration» из состояния владельца.
//!
//! Чистые функции: обзор установщика и состояние операции на входе, модель для
//! `settings_ui` на выходе. Все пользовательские тексты (английские) живут здесь.

use browser_bridge_install::{
    BrowserBridgeStatus, BrowserReport, PackagingKind, SourceProblem, UnavailableReason,
    Verification,
};
use browser_bridge_install::{BrowserSurvey, ExtensionSurvey};

use super::extension_view::extension_row_view;
use super::operations::{ExtensionSurveyResult, SurveyResult};
use crate::settings_ui::browser_panel::{
    BrowserButtonAvailability, BrowserCommand, BrowserHelpLink, BrowserPanelActivity,
    BrowserPanelView, BrowserRowButton, BrowserRowView, BrowserScanState,
};

/// Справка про разрешение для Flatpak-браузера (Flatseal).
const FLATPAK_HELP_URL: &str =
    "https://github.com/Bogdan7c/fastiplayer/blob/main/docs/browser-bridge/flatpak.md";
/// Справка про Snap-браузеры.
const SNAP_HELP_URL: &str =
    "https://github.com/Bogdan7c/fastiplayer/blob/main/docs/browser-bridge/README.md#snap-browsers";

/// Статус подключения у строки, найденной только обзором экстеншена.
const CONNECTION_UNKNOWN_TEXT: &str = "Connection status unavailable";

/// Всё, что нужно для сборки панели.
pub(super) struct PanelInputs<'a> {
    /// Последний обзор (`None` — первый ещё не закончился).
    pub survey: Option<&'a SurveyResult>,
    /// Последний обзор экстеншена (`None` — ещё не закончился). Независим от `survey`.
    pub extensions: Option<&'a ExtensionSurveyResult>,
    /// Почему установщика нет вовсе (нет HOME), если так.
    pub installer_problem: Option<&'a str>,
    /// Выполняется ли операция.
    pub activity: BrowserPanelActivity,
    /// Итог последней операции.
    pub operation_notes: &'a [String],
}

/// Собирает модель панели.
pub(super) fn build_panel_view(inputs: &PanelInputs<'_>) -> BrowserPanelView {
    let mut view = BrowserPanelView {
        scan: BrowserScanState::Scanning,
        activity: inputs.activity,
        global_notes: Vec::new(),
        rows: Vec::new(),
        operation_notes: inputs.operation_notes.to_vec(),
    };
    if let Some(problem) = inputs.installer_problem {
        view.scan = BrowserScanState::Finished;
        view.global_notes
            .push(format!("Browser integration is unavailable: {problem}"));
        return view;
    }
    if inputs.survey.is_none() && inputs.extensions.is_none() {
        return view;
    }
    view.scan = BrowserScanState::Finished;
    // Сбой одного обзора не прячет результат другого: строки собираются из обоих.
    match inputs.survey {
        None => {}
        Some(Err(error)) => view.global_notes.push(error.to_string()),
        Some(Ok(survey)) => fill_from_survey(&mut view, survey),
    }
    match inputs.extensions {
        None => {}
        Some(Err(error)) => view
            .global_notes
            .push(format!("Extension status is unavailable: {error}")),
        Some(Ok(survey)) => attach_extensions(&mut view.rows, survey),
    }
    view
}

/// Привязывает строки экстеншена к строкам браузеров по id цели. Браузер, которого нет в
/// обзоре подключения (например, тот обзор упал), получает строку только с экстеншеном.
fn attach_extensions(rows: &mut Vec<BrowserRowView>, survey: &ExtensionSurvey) {
    for report in &survey.browsers {
        let extension = extension_row_view(report);
        match rows
            .iter_mut()
            .find(|row| row.target_id == report.target.as_str())
        {
            Some(row) => row.extension = Some(extension),
            None => rows.push(BrowserRowView {
                target_id: report.target.as_str().to_owned(),
                name: report.display_name.to_owned(),
                packaging_label: packaging_label(report.packaging),
                not_verified: false,
                status_text: CONNECTION_UNKNOWN_TEXT.to_owned(),
                buttons: Vec::new(),
                help_link: None,
                extension: Some(extension),
            }),
        }
    }
}

/// Подпись упаковки.
fn packaging_label(packaging: PackagingKind) -> &'static str {
    match packaging {
        PackagingKind::Package => "package",
        PackagingKind::Flatpak => "Flatpak",
    }
}

/// Переносит успешный обзор в модель.
fn fill_from_survey(view: &mut BrowserPanelView, survey: &BrowserSurvey) {
    if let Err(problem) = &survey.source {
        view.global_notes.push(source_problem_note(problem));
    }
    if survey.flatpak_problem.is_some() {
        view.global_notes
            .push("Flatpak did not answer — Flatpak browsers are not shown".to_owned());
    }
    let connecting = if survey.source.is_ok() {
        BrowserButtonAvailability::Enabled
    } else {
        BrowserButtonAvailability::Disabled
    };
    view.rows = survey
        .browsers
        .iter()
        .map(|report| row_view(report, connecting))
        .collect();
}

/// Строка о том, почему эта копия плеера не может подключать браузеры.
fn source_problem_note(problem: &SourceProblem) -> String {
    match problem {
        SourceProblem::HostBinaryMissing { .. } => {
            "This Fastiplayer build has no browser host next to it".to_owned()
        }
        SourceProblem::PlayerPathUnsupported { .. } => {
            "Fastiplayer's path contains characters D-Bus cannot start".to_owned()
        }
        SourceProblem::PlayerExecutableUnknown { .. } => {
            "Cannot find out where Fastiplayer is installed".to_owned()
        }
    }
}

/// Строка одного браузера. `connecting` — можно ли действиям, пишущим файлы (Connect,
/// Reconnect, Allow), работать с этой копии плеера; Disconnect не зависит от источника.
pub(super) fn row_view(
    report: &BrowserReport,
    connecting: BrowserButtonAvailability,
) -> BrowserRowView {
    let connect = |label| button(label, BrowserCommand::Connect, connecting);
    let disconnect = || {
        button(
            "Disconnect",
            BrowserCommand::Disconnect,
            BrowserButtonAvailability::Enabled,
        )
    };
    let name = report.display_name.to_owned();
    let (status_text, buttons, help_link) = match &report.status {
        BrowserBridgeStatus::Connected => ("Connected".to_owned(), vec![disconnect()], None),
        BrowserBridgeStatus::ConnectedToAnotherCopy { player_exists, .. } => (
            if *player_exists {
                "Connected to another Fastiplayer copy"
            } else {
                "Connected to a Fastiplayer copy that no longer exists"
            }
            .to_owned(),
            vec![connect("Reconnect to this copy"), disconnect()],
            None,
        ),
        BrowserBridgeStatus::NotConnected => {
            ("Not connected".to_owned(), vec![connect("Connect")], None)
        }
        BrowserBridgeStatus::Outdated => (
            "Outdated — reconnect".to_owned(),
            vec![connect("Reconnect"), disconnect()],
            None,
        ),
        BrowserBridgeStatus::NeedsFlatpakPermission => (
            "Needs Flatpak permission".to_owned(),
            vec![
                button("Allow", BrowserCommand::AllowFlatpakBusAccess, connecting),
                disconnect(),
            ],
            Some(BrowserHelpLink {
                label: "How to allow it in Flatseal",
                url: FLATPAK_HELP_URL,
            }),
        ),
        BrowserBridgeStatus::ForeignFile => (
            "Another program uses this connection file".to_owned(),
            vec![connect("Connect")],
            None,
        ),
        BrowserBridgeStatus::FlatpakUnsupported => (
            format!("{name} from Flatpak is not supported yet"),
            Vec::new(),
            None,
        ),
        BrowserBridgeStatus::SnapUnsupported => (
            "Snap browsers are not supported".to_owned(),
            Vec::new(),
            Some(BrowserHelpLink {
                label: "Why and what to do",
                url: SNAP_HELP_URL,
            }),
        ),
        BrowserBridgeStatus::Unavailable(reason) => (
            format!("Unavailable: {}", unavailable_reason_text(reason)),
            Vec::new(),
            None,
        ),
    };
    BrowserRowView {
        target_id: report.target.as_str().to_owned(),
        name,
        packaging_label: packaging_label(report.packaging),
        not_verified: report.verification == Verification::NotVerified,
        status_text,
        buttons,
        help_link,
        extension: None,
    }
}

/// Кнопка строки.
fn button(
    label: &'static str,
    command: BrowserCommand,
    availability: BrowserButtonAvailability,
) -> BrowserRowButton {
    BrowserRowButton {
        label,
        command,
        availability,
    }
}

/// Короткая причина «статус неизвестен» (без путей).
fn unavailable_reason_text(reason: &UnavailableReason) -> &'static str {
    match reason {
        UnavailableReason::SourceProblem(_) => "this copy of Fastiplayer cannot set up browsers",
        UnavailableReason::Expectation(_) => "cannot prepare the connection files",
        UnavailableReason::Filesystem { .. } => "cannot read the browser's folder",
        UnavailableReason::FlatpakPermissions(_) => "cannot read the Flatpak permissions",
    }
}
