//! Английский текст проверяется после настоящей отрисовки, включая дерево
//! доступности. Сохранённый язык остаётся данными config и не выбирает перевод.

use egui::{Context, Event, PointerButton, Pos2, RawInput, Rect, Vec2};

use super::*;
use crate::settings_ui::{SettingsUiModel, layout};

fn rendered_labels_for_language(language: &str) -> Vec<String> {
    let mut config = AppConfig::default();
    config.ui.language = language.to_owned();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config))
        .expect("settings runtime should build");
    runtime.open_settings().expect("settings should open");
    let model = runtime.ui_model();
    let context = Context::default();
    context.enable_accesskit();
    let mut actions = Vec::new();
    let mut output = context.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 40_000.0))),
            ..Default::default()
        },
        |ui| {
            for field in &model.fields {
                crate::settings_ui::field_widget::show(ui, field, &mut actions);
            }
        },
    );
    output.textures_delta.clear();
    assert!(actions.is_empty(), "rendering must not edit settings");
    assert_eq!(runtime.committed_config().ui.language, language);
    let mut labels = Vec::new();
    for clipped in &output.shapes {
        collect_painted_text(&clipped.shape, &mut labels);
    }
    // Значение самого поля языка сохраняется и отображается буквально.
    labels.retain(|label| label != "ru" && label != "en");
    labels.sort();
    labels
}

/// Текст обычных Label находится в galley; accessibility labels принадлежат
/// интерактивным элементам и не заменяют проверку нарисованных заголовков.
fn collect_painted_text(shape: &egui::epaint::Shape, labels: &mut Vec<String>) {
    match shape {
        egui::epaint::Shape::Text(text) => labels.push(text.galley.job.text.clone()),
        egui::epaint::Shape::Vec(children) => {
            for child in children {
                collect_painted_text(child, labels);
            }
        }
        _ => {}
    }
}

#[test]
fn saved_language_does_not_change_rendered_settings_labels() {
    assert_eq!(AppConfig::default().ui.language, "ru");
    let default_language_labels = rendered_labels_for_language("ru");
    let alternate_language_labels = rendered_labels_for_language("en");
    assert_eq!(default_language_labels, alternate_language_labels);
    assert!(
        default_language_labels
            .iter()
            .any(|label| label == "UI language"),
        "rendered labels: {default_language_labels:?}"
    );
    assert!(
        default_language_labels
            .iter()
            .any(|label| label == "Sidebar width")
    );
    for label in &default_language_labels {
        assert!(
            !label.chars().any(|ch| matches!(ch, 'А'..='я' | 'Ё' | 'ё')),
            "rendered settings label must be English: {label}"
        );
    }
}

fn render_narrow_settings(
    context: &Context,
    model: &SettingsUiModel,
    events: Vec<Event>,
) -> (Vec<SettingsUiAction>, Vec<(String, Rect)>) {
    let mut actions = Vec::new();
    let mut output = context.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(
                    f32::from(fastiplayer_config::MIN_SIDEBAR_WIDTH_POINTS),
                    300.0,
                ),
            )),
            events,
            ..Default::default()
        },
        |ui| layout::show(ui, model, &mut actions),
    );
    output.textures_delta.clear();
    let buttons = output
        .platform_output
        .accesskit_update
        .expect("settings should produce an accessibility tree")
        .nodes
        .iter()
        .filter_map(|(_, node)| {
            let label = node.label()?;
            let bounds = node.bounds()?;
            Some((
                label.to_owned(),
                Rect::from_min_max(
                    Pos2::new(bounds.x0 as f32, bounds.y0 as f32),
                    Pos2::new(bounds.x1 as f32, bounds.y1 as f32),
                ),
            ))
        })
        .collect();
    (actions, buttons)
}

#[test]
fn english_apply_and_cancel_fit_narrow_panel_and_accept_clicks() {
    let mut runtime =
        SettingsRuntime::from_loaded_config(loaded_config_for_test(AppConfig::default()))
            .expect("settings runtime should build");
    runtime.open_settings().expect("settings should open");
    let mut model = runtime.ui_model().clone();
    // Правка нужна для реальной доступности Apply; содержимое списка остаётся
    // production snapshot, чтобы проверить footer под длинными полями настроек.
    model.command_state.has_dirty_fields = true;
    for (label, expected_action) in [
        ("Apply", SettingsUiAction::Apply),
        ("Cancel", SettingsUiAction::Cancel),
    ] {
        let context = Context::default();
        context.enable_accesskit();
        render_narrow_settings(&context, &model, Vec::new());
        let (_, buttons) = render_narrow_settings(&context, &model, Vec::new());
        let bounds = buttons
            .iter()
            .find(|(text, _)| text == label)
            .expect("English command must be rendered")
            .1;
        assert!(
            Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(
                    f32::from(fastiplayer_config::MIN_SIDEBAR_WIDTH_POINTS),
                    300.0
                )
            )
            .contains_rect(bounds),
            "{label} must fit the narrow panel: {bounds:?}"
        );
        let center = bounds.center();
        let pointer = |pressed| Event::PointerButton {
            pos: center,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        render_narrow_settings(
            &context,
            &model,
            vec![Event::PointerMoved(center), pointer(true)],
        );
        let (actions, _) = render_narrow_settings(&context, &model, vec![pointer(false)]);
        assert_eq!(
            actions,
            vec![expected_action],
            "{label} must remain clickable"
        );
    }
}
