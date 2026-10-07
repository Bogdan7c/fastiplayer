//! Функциональные тесты числовых слайдеров: отрисовка не меняет значение,
//! а настоящая правка пользователя доходит до `SettingsUiAction::SetValue`.

use egui::{Context, Event, PointerButton, Pos2, RawInput, Rect, Vec2};
use settings_core::{
    DefaultBehavior, NumericDescriptor, NumericRange, NumericStep, SettingAccess, SettingApplyMode,
    SettingDescriptor, SettingDescriptorText, SettingEditor, SettingId, SettingPlacement,
    SettingRouteId, SettingText, SettingValue, SettingValueType,
};

use super::*;

/// Размер «экрана» тестового egui-контекста.
const SCREEN_SIZE: Vec2 = Vec2::new(800.0, 400.0);

fn numeric_field(
    id: &str,
    range: NumericRange,
    step: NumericStep,
    value: SettingValue,
    value_type: SettingValueType,
) -> SettingsUiField {
    let descriptor = SettingDescriptor {
        id: SettingId::from(id),
        path: id.into(),
        text: SettingDescriptorText::new(SettingText::new("settings.test.label", "Тест")),
        placement: SettingPlacement::new("playlist", "main", "main-settings-window"),
        value_type,
        editor: SettingEditor::Numeric(NumericDescriptor::new(range, step, None)),
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("playlist"),
        apply_mode: SettingApplyMode::CommittedApply,
    };
    SettingsUiField::new(descriptor, value)
}

/// Integer-поле «min 1, шаг 100», значение 2000 — вне сетки `1 + k*100`.
fn off_grid_integer_field() -> SettingsUiField {
    numeric_field(
        "playlist.dropped_folder_max_files",
        NumericRange::Integer {
            min: 1,
            max: 10_000,
        },
        NumericStep::Integer(100),
        SettingValue::Integer(2000),
        SettingValueType::Integer,
    )
}

/// Float-поле «min 0.1, шаг 0.25», значение 1.0 — вне сетки `0.1 + k*0.25`.
fn off_grid_float_field() -> SettingsUiField {
    numeric_field(
        "render.test_float",
        NumericRange::Float { min: 0.1, max: 5.0 },
        NumericStep::Float(0.25),
        SettingValue::Float(1.0),
        SettingValueType::Float,
    )
}

/// Прогоняет один кадр и возвращает действия и прямоугольник поля.
fn run_frame(
    ctx: &Context,
    field: &SettingsUiField,
    events: Vec<Event>,
) -> (Vec<SettingsUiAction>, Rect) {
    let mut actions = Vec::new();
    let mut field_rect = Rect::NOTHING;
    let raw_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN_SIZE)),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(raw_input, |ui| {
        field_rect = ui.scope(|ui| show(ui, field, &mut actions)).response.rect;
    });
    // Дельты текстур нужно «потребить», иначе epaint паникует при drop.
    output.textures_delta.clear();
    (actions, field_rect)
}

/// Рисует поле несколько кадров подряд без ввода; возвращает все действия.
fn render_idle_frames(field: &SettingsUiField, frames: usize) -> Vec<SettingsUiAction> {
    let ctx = Context::default();
    (0..frames)
        .flat_map(|_| run_frame(&ctx, field, Vec::new()).0)
        .collect()
}

#[test]
fn integer_slider_off_grid_value_is_not_changed_by_rendering() {
    let actions = render_idle_frames(&off_grid_integer_field(), 5);

    assert_eq!(actions, Vec::new(), "отрисовка не должна менять значение");
}

#[test]
fn integer_slider_out_of_range_value_is_not_changed_by_rendering() {
    let mut field = off_grid_integer_field();
    field.draft_value = SettingValue::Integer(50_000);

    let actions = render_idle_frames(&field, 5);

    assert_eq!(
        actions,
        Vec::new(),
        "значение вне диапазона трогает только валидация"
    );
}

#[test]
fn float_slider_off_grid_value_is_not_changed_by_rendering() {
    let actions = render_idle_frames(&off_grid_float_field(), 5);

    assert_eq!(actions, Vec::new(), "отрисовка не должна менять значение");
}

/// Кликает в правую часть слайдера и возвращает действия за все кадры клика.
fn click_near_right_edge(field: &SettingsUiField) -> Vec<SettingsUiAction> {
    let ctx = Context::default();
    // Два холостых кадра: раскладка и размер слайдера стабилизируются.
    let (_, rect) = run_frame(&ctx, field, Vec::new());
    let _ = run_frame(&ctx, field, Vec::new());
    // Слайдер занимает левую часть строки; кликаем внутри его трека.
    let target = Pos2::new(rect.left() + rect.width() * 0.2, rect.top() + 30.0);
    let press = |pressed| Event::PointerButton {
        pos: target,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let mut actions = Vec::new();
    actions.extend(run_frame(&ctx, field, vec![Event::PointerMoved(target)]).0);
    actions.extend(run_frame(&ctx, field, vec![press(true)]).0);
    actions.extend(run_frame(&ctx, field, vec![press(false)]).0);
    actions
}

#[test]
fn integer_slider_user_click_emits_snapped_set_value() {
    let actions = click_near_right_edge(&off_grid_integer_field());

    let Some(SettingsUiAction::SetValue { setting_id, value }) = actions.last() else {
        panic!("клик по слайдеру обязан выдать SetValue, получено {actions:?}");
    };
    assert_eq!(setting_id.as_str(), "playlist.dropped_folder_max_files");
    let SettingValue::Integer(edited) = value else {
        panic!("ожидался Integer, получено {value:?}");
    };
    assert_ne!(*edited, 2000, "значение должно измениться кликом");
    assert_eq!(
        (edited - 1) % 100,
        0,
        "правка пользователя привязана к сетке шага"
    );
    assert!((1..=10_000).contains(edited));
}

#[test]
fn float_slider_user_click_emits_set_value() {
    let actions = click_near_right_edge(&off_grid_float_field());

    let Some(SettingsUiAction::SetValue { value, .. }) = actions.last() else {
        panic!("клик по слайдеру обязан выдать SetValue, получено {actions:?}");
    };
    let SettingValue::Float(edited) = value else {
        panic!("ожидался Float, получено {value:?}");
    };
    assert_ne!(*edited, 1.0);
    assert!((0.1..=5.0).contains(edited));
}
