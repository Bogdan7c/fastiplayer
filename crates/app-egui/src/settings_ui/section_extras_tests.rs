//! Кнопка «Edit appearance on video…»: состояния, подсказки и клик в настоящем кадре egui.

use egui::epaint::ClippedShape;
use egui::{Event, PointerButton, Rect, Shape};
use settings_core::{
    DefaultBehavior, SettingAccess, SettingApplyMode, SettingDescriptor, SettingDescriptorText,
    SettingEditor, SettingId, SettingPlacement, SettingRouteId, SettingText, SettingValue,
    SettingValueType,
};

use super::layout::show_with_host_facts;
use super::section_extras::{
    InstalledMediaAvailability, SettingsHostFacts, SubtitleAppearanceEntry,
};
use super::{SettingsUiAction, SettingsUiCommandState, SettingsUiField, SettingsUiModel};
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

const ENTRY_LABEL: &str = "Edit appearance on video…";

fn present() -> SettingsHostFacts {
    SettingsHostFacts {
        installed_media: InstalledMediaAvailability::Present,
    }
}

fn dirty_state(has_dirty_fields: bool) -> SettingsUiCommandState {
    SettingsUiCommandState {
        has_dirty_fields,
        has_validation_errors: false,
        is_busy: false,
    }
}

fn subtitles_field(section: &str, is_dirty: bool) -> SettingsUiField {
    let descriptor = SettingDescriptor {
        id: SettingId::from("subtitles.text_size_percent"),
        path: "subtitles.text_size_percent".into(),
        text: SettingDescriptorText::new(SettingText::new("settings.test.label", "Text size")),
        placement: SettingPlacement::new(section, "style", "main-settings-window"),
        value_type: SettingValueType::Bool,
        editor: SettingEditor::Toggle,
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("subtitles"),
        apply_mode: SettingApplyMode::CommittedApply,
    };
    SettingsUiField::new(descriptor, SettingValue::Bool(false)).dirty(is_dirty)
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

/// Кадры панели настроек; `click_label` - подпись, по которой кликнуть после прогрева.
fn run_settings(
    section: &str,
    host: SettingsHostFacts,
    dirty: bool,
    click_label: Option<&str>,
) -> (Vec<SettingsUiAction>, Vec<ClippedShape>) {
    let model = SettingsUiModel::new(true, vec![subtitles_field(section, dirty)], false);
    let ctx = app_behavior_context();
    let mut actions = Vec::new();
    let frame = |events: Vec<Event>, time: f64, actions: &mut Vec<SettingsUiAction>| {
        run_ui_frame(&ctx, input_at(time, events), |ui| {
            show_with_host_facts(ui, &model, host, actions);
        })
        .shapes
    };
    frame(Vec::new(), 0.0, &mut actions);
    let shapes = frame(Vec::new(), 0.05, &mut actions);
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
        frame(vec![button(false)], 0.2, &mut actions);
    }
    (actions, shapes)
}

#[test]
fn entry_state_depends_on_media_first_then_on_unapplied_changes() {
    assert_eq!(
        SubtitleAppearanceEntry::resolve(present(), dirty_state(false)),
        SubtitleAppearanceEntry::Enabled
    );
    assert_eq!(
        SubtitleAppearanceEntry::resolve(SettingsHostFacts::default(), dirty_state(false)),
        SubtitleAppearanceEntry::DisabledNoMedia
    );
    assert_eq!(
        SubtitleAppearanceEntry::resolve(present(), dirty_state(true)),
        SubtitleAppearanceEntry::DisabledUnappliedChanges
    );
    assert_eq!(
        SubtitleAppearanceEntry::resolve(SettingsHostFacts::default(), dirty_state(true)),
        SubtitleAppearanceEntry::DisabledNoMedia,
        "the more basic reason wins"
    );
}

#[test]
fn disabled_entries_explain_themselves() {
    assert_eq!(SubtitleAppearanceEntry::Enabled.disabled_hint(), None);
    assert_eq!(
        SubtitleAppearanceEntry::DisabledNoMedia.disabled_hint(),
        Some("Open a video first")
    );
    assert_eq!(
        SubtitleAppearanceEntry::DisabledUnappliedChanges.disabled_hint(),
        Some("Apply or cancel your changes first")
    );
}

#[test]
fn click_on_the_enabled_button_emits_the_entry_action() {
    let (actions, shapes) = run_settings("subtitles", present(), false, Some(ENTRY_LABEL));
    assert_eq!(
        actions,
        vec![SettingsUiAction::EditSubtitleAppearanceOnVideo]
    );
    assert!(label_rect(&shapes, "Open a video first").is_none());
}

#[test]
fn without_media_the_button_is_disabled_with_a_visible_hint() {
    let (actions, shapes) = run_settings(
        "subtitles",
        SettingsHostFacts::default(),
        false,
        Some(ENTRY_LABEL),
    );
    assert!(actions.is_empty(), "{actions:?}");
    assert!(label_rect(&shapes, "Open a video first").is_some());
}

#[test]
fn with_unapplied_changes_the_button_is_disabled_with_a_visible_hint() {
    let (actions, shapes) = run_settings("subtitles", present(), true, Some(ENTRY_LABEL));
    assert!(actions.is_empty(), "{actions:?}");
    assert!(label_rect(&shapes, "Apply or cancel your changes first").is_some());
}

#[test]
fn other_sections_get_no_entry_button() {
    let (_, shapes) = run_settings("render", present(), false, None);
    assert!(label_rect(&shapes, ENTRY_LABEL).is_none());
}
