//! Кнопка «Restart now» рядом со статусом поля: рисуется только по предложению и клик в
//! настоящем кадре egui доходит до `SettingsUiAction::RestartNow`.

use egui::epaint::ClippedShape;
use egui::{Event, PointerButton, Rect, Shape};
use settings_core::{
    DefaultBehavior, SettingAccess, SettingApplyMode, SettingDescriptor, SettingDescriptorText,
    SettingEditor, SettingId, SettingPlacement, SettingRouteId, SettingText, SettingValue,
    SettingValueType,
};

use super::field_widget;
use super::{RestartOffer, SettingsUiAction, SettingsUiField};
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

const BUTTON_LABEL: &str = "Restart now";
const STATUS_TEXT: &str = "Applies after restart";

fn profile_field(offer: RestartOffer) -> SettingsUiField {
    let descriptor = SettingDescriptor {
        id: SettingId::from("render.profile"),
        path: "render.profile".into(),
        text: SettingDescriptorText::new(SettingText::new("settings.test.label", "Render profile")),
        placement: SettingPlacement::new("render", "profile", "main-settings-window"),
        value_type: SettingValueType::Bool,
        editor: SettingEditor::Toggle,
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("render"),
        apply_mode: SettingApplyMode::CommittedApply,
    };
    SettingsUiField::new(descriptor, SettingValue::Bool(false))
        .with_status(STATUS_TEXT)
        .with_restart_offer(offer)
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

/// Рисует поле, затем при `click` кликает по кнопке; возвращает действия и кадр без клика.
fn run_field(field: &SettingsUiField, click: bool) -> (Vec<SettingsUiAction>, Vec<ClippedShape>) {
    let ctx = app_behavior_context();
    let mut actions = Vec::new();
    let frame = |events: Vec<Event>, time: f64, actions: &mut Vec<SettingsUiAction>| {
        run_ui_frame(&ctx, input_at(time, events), |ui| {
            field_widget::show(ui, field, actions);
        })
        .shapes
    };
    frame(Vec::new(), 0.0, &mut actions);
    let shapes = frame(Vec::new(), 0.05, &mut actions);
    if click {
        let target = label_rect(&shapes, BUTTON_LABEL)
            .expect("button is drawn")
            .center();
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
fn offered_restart_draws_status_and_button_and_click_emits_restart_now() {
    let (actions, shapes) = run_field(&profile_field(RestartOffer::Available), true);

    assert!(label_rect(&shapes, STATUS_TEXT).is_some());
    assert!(label_rect(&shapes, BUTTON_LABEL).is_some());
    assert_eq!(actions, vec![SettingsUiAction::RestartNow]);
}

#[test]
fn without_an_offer_only_the_status_is_drawn_and_nothing_is_emitted() {
    let (actions, shapes) = run_field(&profile_field(RestartOffer::None), false);

    assert!(label_rect(&shapes, STATUS_TEXT).is_some());
    assert!(label_rect(&shapes, BUTTON_LABEL).is_none());
    assert!(actions.is_empty(), "{actions:?}");
}
