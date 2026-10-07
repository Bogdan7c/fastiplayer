//! Инвариант «отрисовка не меняет значение»: открытие настроек и показ всех
//! полей без ввода пользователя не создаёт правок draft-а.

use egui::{Context, Pos2, RawInput, Rect, Vec2};

use super::*;
use crate::settings_ui::{SettingsUiAction, field_widget};

/// Число кадров, которые рисуем без ввода: фантомная правка egui проявлялась с первого кадра.
const IDLE_FRAMES: usize = 3;

#[test]
fn rendering_all_default_settings_fields_does_not_modify_draft() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        temp_config_path("render-keeps-draft"),
    ))
    .expect("settings runtime should build");
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter should build");
    runtime.open_settings().expect("settings должны открыться");

    let ctx = Context::default();
    for _ in 0..IDLE_FRAMES {
        let fields = runtime.ui_model().fields.clone();
        assert!(!fields.is_empty(), "модель настроек не должна быть пустой");
        let mut actions: Vec<SettingsUiAction> = Vec::new();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 40_000.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw_input, |ui| {
            for field in &fields {
                field_widget::show(ui, field, &mut actions);
            }
        });
        output.textures_delta.clear();

        assert_eq!(actions, Vec::new(), "отрисовка не должна выдавать действия");
        run_runtime_actions(&mut runtime, actions, &mut adapter);
    }

    let modified: Vec<_> = runtime
        .ui_model()
        .fields
        .iter()
        .filter(|field| field.is_dirty)
        .map(|field| field.descriptor.id.clone())
        .collect();
    assert_eq!(
        modified,
        Vec::new(),
        "ни одно поле не должно стать изменённым"
    );
}
