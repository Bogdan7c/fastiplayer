//! Строка экстеншена в модели вида: каждое состояние → текст, кнопки, заметки.

use browser_bridge_install::{
    ExtensionReport, ExtensionState, ExtensionSurvey, ExtensionUnsupported, HintState,
    InstallAction, UnreadableKind,
};

use super::super::extension_view::extension_row_view;
use super::support::*;
use crate::settings_ui::browser_panel::ExtensionRowView;

fn extension_report(
    state: ExtensionState,
    install_action: Option<InstallAction>,
    hint: HintState,
) -> ExtensionReport {
    ExtensionReport {
        target: BrowserTargetId::new("chromium"),
        display_name: "Chromium",
        packaging: PackagingKind::Package,
        state,
        install_action,
        hint,
    }
}

fn view_of(
    state: ExtensionState,
    install_action: Option<InstallAction>,
    hint: HintState,
) -> ExtensionRowView {
    extension_row_view(&extension_report(state, install_action, hint))
}

fn labels(view: &ExtensionRowView) -> Vec<&'static str> {
    view.buttons.iter().map(|button| button.label).collect()
}

#[test]
fn installed_and_disabled_have_no_buttons() {
    let installed = view_of(ExtensionState::Installed, None, HintState::Absent);
    assert_eq!(installed.status_text, "Extension: installed");
    assert!(installed.buttons.is_empty());

    let disabled = view_of(
        ExtensionState::InstalledButDisabled,
        None,
        HintState::Absent,
    );
    assert_eq!(disabled.status_text, "Extension: turned off in Chromium");
    assert!(disabled.buttons.is_empty());
    assert_eq!(
        disabled.notes.len(),
        1,
        "tells the person how to turn it on"
    );
}

#[test]
fn waiting_for_restart_offers_the_store_page_fallback_and_cancel() {
    let view = view_of(
        ExtensionState::WaitingForBrowserRestart,
        Some(InstallAction::OpenStorePage),
        HintState::Ours,
    );
    assert_eq!(
        view.status_text,
        "Extension: restart Chromium to finish installing"
    );
    assert_eq!(labels(&view), ["Install in browser", "Cancel"]);
    assert_eq!(view.buttons[0].command, BrowserCommand::InstallExtension);
    assert_eq!(
        view.buttons[1].command,
        BrowserCommand::CancelExtensionInstall
    );
}

#[test]
fn removed_in_browser_opens_the_store_page() {
    let view = view_of(
        ExtensionState::RemovedInBrowser,
        Some(InstallAction::OpenStorePage),
        HintState::Absent,
    );
    assert_eq!(view.status_text, "Extension: removed in Chromium");
    assert_eq!(labels(&view), ["Open store page"]);
    assert_eq!(view.buttons[0].command, BrowserCommand::InstallExtension);
}

#[test]
fn not_installed_button_depends_on_the_install_action() {
    let silent = view_of(
        ExtensionState::NotInstalled,
        Some(InstallAction::InstallSilently),
        HintState::Absent,
    );
    assert_eq!(silent.status_text, "Extension: not installed");
    assert_eq!(labels(&silent), ["Install extension"]);

    let store = view_of(
        ExtensionState::NotInstalled,
        Some(InstallAction::OpenStorePage),
        HintState::Absent,
    );
    assert_eq!(labels(&store), ["Install in browser"]);
    assert_eq!(store.buttons[0].command, BrowserCommand::InstallExtension);

    let none = view_of(ExtensionState::NotInstalled, None, HintState::Absent);
    assert!(none.buttons.is_empty(), "no action means no button");
}

#[test]
fn awaiting_publication_has_no_button_but_a_development_build_link() {
    let view = view_of(ExtensionState::AwaitingPublication, None, HintState::Absent);
    assert_eq!(view.status_text, "Extension: available after publication");
    assert!(view.buttons.is_empty());
    let link = view.help_link.expect("development build link");
    assert!(link.url.ends_with("#installing-a-development-build"));
}

#[test]
fn unsupported_reasons_have_their_own_texts() {
    let text =
        |reason| view_of(ExtensionState::Unsupported(reason), None, HintState::Absent).status_text;
    assert_eq!(
        text(ExtensionUnsupported::Snap),
        "Extension: Snap browsers are not supported"
    );
    assert_eq!(
        text(ExtensionUnsupported::FlatpakFirefoxFamily),
        "Extension: not supported for Chromium from Flatpak yet"
    );
    assert_eq!(
        text(ExtensionUnsupported::NoWebStore),
        "Extension: install it manually (Chromium has no web store)"
    );
    assert_eq!(
        text(ExtensionUnsupported::CatalogMisconfigured),
        "Extension: unavailable"
    );
}

#[test]
fn unreadable_profile_names_the_file_and_the_reason_without_the_directory() {
    let view = view_of(
        ExtensionState::Unreadable {
            path: PathBuf::from("/home/someone/.config/chromium/Default/Preferences"),
            kind: UnreadableKind::MalformedJson,
        },
        None,
        HintState::Absent,
    );
    assert_eq!(
        view.status_text,
        "Extension: can't read Chromium profile (Preferences: not valid JSON)"
    );
}

#[test]
fn cancel_is_offered_only_for_our_hint_while_not_installed_and_a_foreign_hint_gets_a_note() {
    let ours = view_of(ExtensionState::NotInstalled, None, HintState::Ours);
    assert_eq!(labels(&ours), ["Cancel"]);

    // Установленный экстеншен: удаление подсказки ничего не изменит, кнопки нет.
    for installed in [
        ExtensionState::Installed,
        ExtensionState::InstalledButDisabled,
    ] {
        let view = view_of(installed.clone(), None, HintState::Ours);
        assert!(view.buttons.is_empty(), "{installed:?}: nothing to cancel");
    }

    let foreign = view_of(
        ExtensionState::NotInstalled,
        Some(InstallAction::OpenStorePage),
        HintState::Foreign,
    );
    assert_eq!(labels(&foreign), ["Install in browser"], "never Cancel");
    assert_eq!(
        foreign.notes,
        ["Another program left an extension hint for this browser"]
    );

    let absent = view_of(ExtensionState::Installed, None, HintState::Absent);
    assert!(absent.notes.is_empty() && absent.buttons.is_empty());
}

#[test]
fn primary_button_comes_before_cancel() {
    let view = view_of(
        ExtensionState::NotInstalled,
        Some(InstallAction::InstallSilently),
        HintState::Ours,
    );
    assert_eq!(labels(&view), ["Install extension", "Cancel"]);
}

fn extension_survey(reports: Vec<ExtensionReport>) -> ExtensionSurveyResult {
    Ok(ExtensionSurvey {
        browsers: reports,
        flatpak_problem: None,
    })
}

fn panel_with(
    survey: Option<&SurveyResult>,
    extensions: Option<&ExtensionSurveyResult>,
) -> BrowserPanelView {
    build_panel_view(&PanelInputs {
        survey,
        extensions,
        installer_problem: None,
        activity: BrowserPanelActivity::Idle,
        operation_notes: &[],
    })
}

#[test]
fn extension_line_is_joined_to_the_browser_row_by_target_id() {
    let host = Ok(survey_with(Ok(()), vec![BrowserBridgeStatus::NotConnected]));
    let extensions = extension_survey(vec![extension_report(
        ExtensionState::Installed,
        None,
        HintState::Absent,
    )]);
    let view = panel_with(Some(&host), Some(&extensions));
    assert_eq!(view.rows.len(), 1);
    assert_eq!(view.rows[0].status_text, "Not connected");
    assert_eq!(
        view.rows[0]
            .extension
            .as_ref()
            .map(|e| e.status_text.as_str()),
        Some("Extension: installed")
    );
}

#[test]
fn failed_host_survey_does_not_hide_extension_rows() {
    let host: SurveyResult = Err(SurveyError::UnsupportedPlatform);
    let extensions = extension_survey(vec![extension_report(
        ExtensionState::NotInstalled,
        Some(InstallAction::InstallSilently),
        HintState::Absent,
    )]);
    let view = panel_with(Some(&host), Some(&extensions));

    assert_eq!(
        view.global_notes,
        [SurveyError::UnsupportedPlatform.to_string()]
    );
    assert_eq!(view.rows.len(), 1);
    assert_eq!(view.rows[0].name, "Chromium");
    assert_eq!(view.rows[0].status_text, "Connection status unavailable");
    assert!(view.rows[0].buttons.is_empty());
    let extension = view.rows[0].extension.as_ref().expect("extension line");
    assert_eq!(extension.buttons[0].label, "Install extension");
    assert_eq!(view.scan, BrowserScanState::Finished);
}

#[test]
fn failed_extension_survey_keeps_host_rows_and_adds_its_own_note() {
    let host = Ok(survey_with(Ok(()), vec![BrowserBridgeStatus::Connected]));
    let extensions: ExtensionSurveyResult = Err(SurveyError::UnsupportedPlatform);
    let view = panel_with(Some(&host), Some(&extensions));

    assert_eq!(view.rows[0].status_text, "Connected");
    assert!(view.rows[0].extension.is_none());
    assert_eq!(
        view.global_notes,
        [format!(
            "Extension status is unavailable: {}",
            SurveyError::UnsupportedPlatform
        )]
    );
}
