//! Панель «Browser integration»: настоящий кадр egui, клики и просьба открыть секцию.

use egui::epaint::ClippedShape;
use egui::{Event, PointerButton, Rect, Shape};
use settings_core::{
    DefaultBehavior, SettingAccess, SettingApplyMode, SettingDescriptor, SettingDescriptorText,
    SettingEditor, SettingId, SettingPlacement, SettingRouteId, SettingText, SettingValue,
    SettingValueType,
};

use super::browser_panel::{
    BrowserButtonAvailability, BrowserCommand, BrowserHelpLink, BrowserPanelActivity,
    BrowserPanelView, BrowserRowButton, BrowserRowView, BrowserScanState, ExtensionRowView,
};
use super::layout::show_with_host_view;
use super::section_extras::{BROWSER_INTEGRATION_SECTION_ID, SectionRequest, SettingsHostFacts};
use super::{SettingsUiAction, SettingsUiField, SettingsUiModel};
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

const HELP_URL: &str = "https://example.invalid/help";

fn field(section: &str, id: &str) -> SettingsUiField {
    let descriptor = SettingDescriptor {
        id: SettingId::from(id),
        path: id.into(),
        text: SettingDescriptorText::new(SettingText::new("settings.test.label", "A toggle")),
        placement: SettingPlacement::new(section, "main", "main-settings-window"),
        value_type: SettingValueType::Bool,
        editor: SettingEditor::Toggle,
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("test"),
        apply_mode: SettingApplyMode::CommittedApply,
    };
    SettingsUiField::new(descriptor, SettingValue::Bool(false))
}

fn model() -> SettingsUiModel {
    // Две секции: без просьбы выбрана первая по алфавиту («audio»).
    SettingsUiModel::new(
        true,
        vec![
            field("audio", "audio.test"),
            field(
                BROWSER_INTEGRATION_SECTION_ID,
                "browser_bridge.offer_on_startup",
            ),
        ],
        false,
    )
}

fn connect_button() -> BrowserRowButton {
    BrowserRowButton {
        label: "Connect",
        command: BrowserCommand::Connect,
        availability: BrowserButtonAvailability::Enabled,
    }
}

fn panel(activity: BrowserPanelActivity, help_link: Option<BrowserHelpLink>) -> BrowserPanelView {
    BrowserPanelView {
        scan: BrowserScanState::Finished,
        activity,
        global_notes: Vec::new(),
        rows: vec![BrowserRowView {
            target_id: "chromium".to_owned(),
            name: "Chromium".to_owned(),
            packaging_label: "package",
            not_verified: false,
            status_text: "Not connected".to_owned(),
            buttons: vec![connect_button()],
            help_link,
            extension: None,
        }],
        operation_notes: Vec::new(),
    }
}

fn label_rect(shapes: &[ClippedShape], text: &str) -> Option<Rect> {
    fn walk(shape: &Shape, text: &str) -> Option<Rect> {
        match shape {
            Shape::Text(t) if t.galley.text() == text => {
                Some(t.galley.rect.translate(t.pos.to_vec2()))
            }
            Shape::Vec(inner) => inner.iter().find_map(|s| walk(s, text)),
            _ => None,
        }
    }
    shapes.iter().find_map(|clipped| walk(&clipped.shape, text))
}

/// Кадры панели; `requests` — просьба секции на каждый кадр, `click_label` — клик после прогрева.
fn run(
    host: SettingsHostFacts,
    browser_panel: &BrowserPanelView,
    click_label: Option<&str>,
) -> (
    Vec<SettingsUiAction>,
    Vec<ClippedShape>,
    Vec<egui::OutputCommand>,
) {
    let model = model();
    let ctx = app_behavior_context();
    let mut actions = Vec::new();
    let mut commands = Vec::new();
    let mut frame = |events: Vec<Event>, time: f64, actions: &mut Vec<SettingsUiAction>| {
        let output = run_ui_frame(&ctx, input_at(time, events), |ui| {
            show_with_host_view(ui, &model, host, browser_panel, actions);
        });
        commands.extend(output.platform_output.commands);
        output.shapes
    };
    frame(Vec::new(), 0.0, &mut actions);
    let mut shapes = frame(Vec::new(), 0.05, &mut actions);
    if let Some(label) = click_label {
        let target = label_rect(&shapes, label).expect("label is drawn").center();
        let button = |pressed| Event::PointerButton {
            pos: target,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![Event::PointerMoved(target)], 0.1, &mut actions);
        frame(vec![button(true)], 0.15, &mut actions);
        shapes = frame(vec![button(false)], 0.2, &mut actions);
    }
    (actions, shapes, commands)
}

fn browser_request(serial: u64) -> SettingsHostFacts {
    SettingsHostFacts {
        section_request: Some(SectionRequest {
            section: BROWSER_INTEGRATION_SECTION_ID,
            serial,
        }),
        ..SettingsHostFacts::default()
    }
}

#[test]
fn section_request_shows_the_browser_panel_and_the_human_section_name() {
    let (_, shapes, _) = run(
        browser_request(1),
        &panel(BrowserPanelActivity::Idle, None),
        None,
    );
    assert!(
        label_rect(&shapes, "Chromium").is_some(),
        "the panel rows are drawn"
    );
    assert!(label_rect(&shapes, "Not connected").is_some());
    assert!(
        label_rect(&shapes, "Browser integration").is_some(),
        "the tab and the heading use the human name, not the id"
    );
    assert!(label_rect(&shapes, BROWSER_INTEGRATION_SECTION_ID).is_none());
}

#[test]
fn without_a_request_the_first_section_stays_selected() {
    let (_, shapes, _) = run(
        SettingsHostFacts::default(),
        &panel(BrowserPanelActivity::Idle, None),
        None,
    );
    assert!(label_rect(&shapes, "Chromium").is_none());
}

#[test]
fn clicking_connect_emits_a_browser_action_with_the_target_id_and_nothing_else() {
    let (actions, _, _) = run(
        browser_request(1),
        &panel(BrowserPanelActivity::Idle, None),
        Some("Connect"),
    );
    assert_eq!(
        actions,
        [SettingsUiAction::BrowserIntegration {
            target_id: "chromium".to_owned(),
            command: BrowserCommand::Connect,
        }],
        "no Cancel/Ok: the settings window stays open"
    );
}

#[test]
fn buttons_are_dead_and_working_is_shown_while_an_operation_runs() {
    let (actions, shapes, _) = run(
        browser_request(1),
        &panel(BrowserPanelActivity::Working, None),
        Some("Connect"),
    );
    assert!(actions.is_empty(), "{actions:?}");
    assert!(label_rect(&shapes, "Working…").is_some());
}

#[test]
fn help_link_opens_the_address_in_the_default_browser() {
    let link = BrowserHelpLink {
        label: "How to allow it in Flatseal",
        url: HELP_URL,
    };
    let (_, _, commands) = run(
        browser_request(1),
        &panel(BrowserPanelActivity::Idle, Some(link)),
        Some("How to allow it in Flatseal"),
    );
    assert!(
        commands.iter().any(|command| matches!(
            command,
            egui::OutputCommand::OpenUrl(open) if open.url == HELP_URL
        )),
        "{commands:?}"
    );
}

/// Панель с одной строкой экстеншена и заданными кнопками.
fn extension_panel(buttons: Vec<BrowserRowButton>, notes: Vec<String>) -> BrowserPanelView {
    let mut view = panel(BrowserPanelActivity::Idle, None);
    view.rows[0].extension = Some(ExtensionRowView {
        status_text: "Extension: not installed".to_owned(),
        buttons,
        notes,
        help_link: None,
    });
    view
}

fn extension_button(label: &'static str, command: BrowserCommand) -> BrowserRowButton {
    BrowserRowButton {
        label,
        command,
        availability: BrowserButtonAvailability::Enabled,
    }
}

#[test]
fn extension_line_buttons_emit_their_own_commands_with_the_target_id() {
    let buttons = || {
        vec![
            extension_button("Install extension", BrowserCommand::InstallExtension),
            extension_button("Remove", BrowserCommand::CancelExtensionInstall),
        ]
    };
    for (label, command) in [
        ("Install extension", BrowserCommand::InstallExtension),
        ("Remove", BrowserCommand::CancelExtensionInstall),
    ] {
        let (actions, shapes, _) = run(
            browser_request(1),
            &extension_panel(buttons(), vec!["Another program left a hint".to_owned()]),
            Some(label),
        );
        assert_eq!(
            actions,
            [SettingsUiAction::BrowserIntegration {
                target_id: "chromium".to_owned(),
                command,
            }],
            "{label}"
        );
        assert!(label_rect(&shapes, "Extension: not installed").is_some());
        assert!(label_rect(&shapes, "Another program left a hint").is_some());
    }
}

#[test]
fn extension_buttons_are_dead_while_an_operation_runs() {
    let mut view = extension_panel(
        vec![extension_button(
            "Install in browser",
            BrowserCommand::InstallExtension,
        )],
        Vec::new(),
    );
    view.activity = BrowserPanelActivity::Working;
    let (actions, _, _) = run(browser_request(1), &view, Some("Install in browser"));
    assert!(actions.is_empty(), "{actions:?}");
}
