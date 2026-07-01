//! Generic field widgets по neutral `SettingEditor`.

use egui::{ComboBox, DragValue, RichText, Slider, TextEdit, Ui};
use settings_core::{
    AutoFixedPositiveIntegerDescriptor, DefaultBehavior, NumericDescriptor, NumericRange,
    NumericStep, OptionProviderId, SelectDescriptor, SelectListDescriptor, SettingAccess,
    SettingEditor, SettingId, SettingOption, SettingOptionCurrentValue, SettingOptionId,
    SettingOptions, SettingOptionsStatus, SettingValue, TextDescriptor, TextFormat,
    VectorDescriptor,
};

use super::{SettingsUiAction, SettingsUiField};

/// Display option для select widget-а, включая unavailable-current значение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectDisplayOption {
    /// Stable option id, который хранится в setting value.
    pub id: SettingOptionId,

    /// Готовая user-facing label строка.
    pub label: String,

    /// Доступна ли option в текущем cached provider snapshot-е.
    pub available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoFixedPositiveMode {
    Auto,
    Fixed,
}

/// Рисует один field по neutral descriptor-у.
pub fn show(ui: &mut Ui, field: &SettingsUiField, actions: &mut Vec<SettingsUiAction>) {
    ui.vertical(|ui| {
        render_field_header(ui, field, actions);

        if field.descriptor.access.permits_write() {
            render_editor(ui, field, actions);
        } else {
            render_read_only_value(ui, &field.draft_value);
        }

        render_field_messages(ui, field);
    });
}

/// Pure mapping для set value action.
#[must_use]
pub(crate) fn set_value_action(setting_id: &SettingId, value: SettingValue) -> SettingsUiAction {
    SettingsUiAction::SetValue {
        setting_id: setting_id.clone(),
        value,
    }
}

/// Строит display options для static/dynamic select-а.
#[must_use]
pub fn select_display_options(
    static_options: &[SettingOption],
    dynamic_options: Option<&SettingOptions>,
    selected_id: Option<&SettingOptionId>,
) -> Vec<SelectDisplayOption> {
    let mut display_options = Vec::new();

    if let Some(options_snapshot) = dynamic_options {
        append_unavailable_current_option(&mut display_options, options_snapshot, selected_id);
        append_available_options(&mut display_options, &options_snapshot.options);
    } else {
        append_available_options(&mut display_options, static_options);
    }

    display_options
}

/// Рисует label, description и reset field button.
fn render_field_header(ui: &mut Ui, field: &SettingsUiField, actions: &mut Vec<SettingsUiAction>) {
    ui.horizontal(|ui| {
        let label = if field.is_dirty {
            format!("{} *", field.descriptor.text.label.fallback_ru)
        } else {
            field.descriptor.text.label.fallback_ru.clone()
        };

        ui.label(RichText::new(label).strong());

        if reset_field_enabled(field) && ui.small_button("Сброс").clicked() {
            actions.push(SettingsUiAction::ResetField {
                setting_id: field.descriptor.id.clone(),
            });
        }
    });

    if let Some(description) = &field.descriptor.text.description {
        ui.label(&description.fallback_ru);
    }
}

/// Рисует editor, соответствующий `SettingEditor`.
fn render_editor(ui: &mut Ui, field: &SettingsUiField, actions: &mut Vec<SettingsUiAction>) {
    match &field.descriptor.editor {
        SettingEditor::Toggle => render_toggle(ui, field, actions),
        SettingEditor::Numeric(descriptor) => render_numeric(ui, field, descriptor, actions),
        SettingEditor::Select(descriptor) => render_select(ui, field, descriptor, actions),
        SettingEditor::SelectList(descriptor) => render_select_list(ui, field, descriptor, actions),
        SettingEditor::Text(descriptor) => render_text(ui, field, descriptor, actions),
        SettingEditor::AutoFixedPositiveInteger(descriptor) => {
            render_auto_fixed_positive_integer(ui, field, descriptor, actions);
        }
        SettingEditor::Vector(descriptor) => render_vector(ui, field, descriptor, actions),
        SettingEditor::ReadOnly => render_read_only_value(ui, &field.draft_value),
    }
}

/// Рисует bool toggle.
fn render_toggle(ui: &mut Ui, field: &SettingsUiField, actions: &mut Vec<SettingsUiAction>) {
    let SettingValue::Bool(mut checked) = field.draft_value.clone() else {
        render_type_mismatch(ui);
        return;
    };

    if ui.checkbox(&mut checked, "").changed() {
        actions.push(set_value_action(
            &field.descriptor.id,
            SettingValue::Bool(checked),
        ));
    }
}

/// Рисует numeric slider для integer/float value.
fn render_numeric(
    ui: &mut Ui,
    field: &SettingsUiField,
    descriptor: &NumericDescriptor,
    actions: &mut Vec<SettingsUiAction>,
) {
    match (&descriptor.range, &field.draft_value) {
        (NumericRange::Integer { min, max }, SettingValue::Integer(current_value)) => {
            let mut edited_value = *current_value;
            let mut slider =
                Slider::new(&mut edited_value, *min..=*max).step_by(integer_step(descriptor));
            if let Some(unit) = &descriptor.unit {
                slider = slider.text(unit.as_str());
            }
            if ui.add(slider).changed() {
                actions.push(set_value_action(
                    &field.descriptor.id,
                    SettingValue::Integer(edited_value),
                ));
            }
        }
        (NumericRange::Float { min, max }, SettingValue::Float(current_value)) => {
            let step = float_step(descriptor);
            let mut edited_value = *current_value;
            let mut slider = Slider::new(&mut edited_value, *min..=*max).step_by(step);
            if let Some(unit) = &descriptor.unit {
                slider = slider.text(unit.as_str());
            }
            if ui.add(slider).changed()
                && slider_value_really_changed(*current_value, edited_value, step)
            {
                actions.push(set_value_action(
                    &field.descriptor.id,
                    SettingValue::Float(edited_value),
                ));
            }
        }
        _ => render_type_mismatch(ui),
    }
}

/// Отличает реальное изменение слайдера от фантомного snap-а к step grid.
///
/// Config может хранить float как `f32`; модель отдаёт `f64` после roundtrip-а,
/// slider snap-ит его к step grid и сообщает `changed()` каждый кадр, хотя
/// пользователь ничего не трогал. Фантомный SetValue затирал ResetField и
/// заставлял runtime клонировать draft на каждом кадре. Реальное изменение
/// всегда не меньше шага, фантомное — порядка f32 quantization noise.
fn slider_value_really_changed(previous: f64, edited: f64, step: f64) -> bool {
    (edited - previous).abs() >= step * 0.5
}

/// Рисует static или dynamic select.
fn render_select(
    ui: &mut Ui,
    field: &SettingsUiField,
    descriptor: &SelectDescriptor,
    actions: &mut Vec<SettingsUiAction>,
) {
    let SettingValue::Select(current_id) = &field.draft_value else {
        render_type_mismatch(ui);
        return;
    };

    let static_options = match descriptor {
        SelectDescriptor::Static { options } => options.as_slice(),
        SelectDescriptor::Dynamic { .. } => &[],
    };
    let dynamic_provider_id = match descriptor {
        SelectDescriptor::Dynamic { provider_id } => Some(provider_id),
        SelectDescriptor::Static { .. } => None,
    };
    let dynamic_options = match descriptor {
        SelectDescriptor::Dynamic { .. } => field.options.as_ref(),
        SelectDescriptor::Static { .. } => None,
    };
    let display_options = select_display_options(static_options, dynamic_options, Some(current_id));

    if display_options.is_empty() {
        ui.horizontal(|ui| {
            ui.label("Опции недоступны");
            if let Some(provider_id) = dynamic_provider_id {
                render_refresh_options_button(ui, provider_id, actions);
            }
        });
        render_options_status(ui, dynamic_options);
        return;
    }

    let mut selected_id = current_id.clone();
    let selected_label = select_label_for_value(&selected_id, &display_options);

    ui.horizontal(|ui| {
        ComboBox::from_id_salt(field.descriptor.id.as_str())
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for display_option in &display_options {
                    if display_option.available {
                        ui.selectable_value(
                            &mut selected_id,
                            display_option.id.clone(),
                            display_option.label.as_str(),
                        );
                    } else {
                        ui.label(&display_option.label);
                    }
                }
            });

        if let Some(provider_id) = dynamic_provider_id {
            render_refresh_options_button(ui, provider_id, actions);
        }
    });

    if selected_id != *current_id {
        actions.push(set_value_action(
            &field.descriptor.id,
            SettingValue::Select(selected_id),
        ));
    }

    render_options_status(ui, dynamic_options);
}

/// Рисует explicit refresh action для dynamic options.
fn render_refresh_options_button(
    ui: &mut Ui,
    provider_id: &OptionProviderId,
    actions: &mut Vec<SettingsUiAction>,
) {
    if ui.small_button("Обновить").clicked() {
        actions.push(SettingsUiAction::RefreshOptions {
            provider_id: provider_id.clone(),
        });
    }
}

/// Рисует ordered select-list как набор checkbox-ов по static descriptor options.
fn render_select_list(
    ui: &mut Ui,
    field: &SettingsUiField,
    descriptor: &SelectListDescriptor,
    actions: &mut Vec<SettingsUiAction>,
) {
    let SettingValue::SelectList(current_ids) = &field.draft_value else {
        render_type_mismatch(ui);
        return;
    };

    let mut edited_ids = current_ids.clone();
    let mut changed = false;

    for option in &descriptor.options {
        let mut checked = edited_ids
            .iter()
            .any(|selected_id| selected_id == &option.id);
        if ui
            .checkbox(&mut checked, &option.label.fallback_ru)
            .changed()
        {
            changed = true;
            if checked {
                if descriptor.allow_duplicates || !edited_ids.iter().any(|id| id == &option.id) {
                    edited_ids.push(option.id.clone());
                }
            } else {
                edited_ids.retain(|id| id != &option.id);
            }
        }
    }

    if changed {
        actions.push(set_value_action(
            &field.descriptor.id,
            SettingValue::SelectList(edited_ids),
        ));
    }
}

/// Рисует text editor.
fn render_text(
    ui: &mut Ui,
    field: &SettingsUiField,
    descriptor: &TextDescriptor,
    actions: &mut Vec<SettingsUiAction>,
) {
    let SettingValue::Text(current_text) = &field.draft_value else {
        render_type_mismatch(ui);
        return;
    };

    let mut edited_text = current_text.clone();
    let changed = match descriptor.format {
        TextFormat::SingleLine => ui.text_edit_singleline(&mut edited_text).changed(),
        TextFormat::Multiline => ui
            .add(TextEdit::multiline(&mut edited_text).desired_rows(3))
            .changed(),
    };

    if changed {
        actions.push(set_value_action(
            &field.descriptor.id,
            SettingValue::Text(edited_text),
        ));
    }
}

/// Рисует Auto/Fixed-positive editor без project-specific setting id special cases.
fn render_auto_fixed_positive_integer(
    ui: &mut Ui,
    field: &SettingsUiField,
    descriptor: &AutoFixedPositiveIntegerDescriptor,
    actions: &mut Vec<SettingsUiAction>,
) {
    let SettingValue::Text(current_text) = &field.draft_value else {
        render_type_mismatch(ui);
        return;
    };

    let current_mode = auto_fixed_positive_mode(current_text);
    let mut selected_mode = current_mode;
    let mut fixed_value = fixed_positive_value_or_default(current_text);

    ui.horizontal(|ui| {
        ComboBox::from_id_salt((field.descriptor.id.as_str(), "auto_fixed_mode"))
            .selected_text(auto_fixed_positive_mode_label(descriptor, selected_mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut selected_mode,
                    AutoFixedPositiveMode::Auto,
                    descriptor.auto_label.fallback_ru.as_str(),
                );
                ui.selectable_value(
                    &mut selected_mode,
                    AutoFixedPositiveMode::Fixed,
                    descriptor.fixed_label.fallback_ru.as_str(),
                );
            });

        if selected_mode == AutoFixedPositiveMode::Fixed {
            let mut drag_value = DragValue::new(&mut fixed_value)
                .range(1..=usize::MAX)
                .speed(1);
            if let Some(unit) = &descriptor.unit {
                drag_value = drag_value.suffix(format!(" {}", unit.as_str()));
            }
            if ui.add(drag_value).changed() && fixed_value > 0 {
                actions.push(set_value_action(
                    &field.descriptor.id,
                    SettingValue::Text(fixed_value.to_string()),
                ));
            }
        }
    });

    if selected_mode != current_mode {
        let next_value = match selected_mode {
            AutoFixedPositiveMode::Auto => "auto".to_string(),
            AutoFixedPositiveMode::Fixed => fixed_value.to_string(),
        };
        actions.push(set_value_action(
            &field.descriptor.id,
            SettingValue::Text(next_value),
        ));
    }
}

fn auto_fixed_positive_mode(text: &str) -> AutoFixedPositiveMode {
    if text.trim() == "auto" {
        AutoFixedPositiveMode::Auto
    } else {
        AutoFixedPositiveMode::Fixed
    }
}

fn fixed_positive_value_or_default(text: &str) -> usize {
    text.trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

fn auto_fixed_positive_mode_label(
    descriptor: &AutoFixedPositiveIntegerDescriptor,
    mode: AutoFixedPositiveMode,
) -> &str {
    match mode {
        AutoFixedPositiveMode::Auto => descriptor.auto_label.fallback_ru.as_str(),
        AutoFixedPositiveMode::Fixed => descriptor.fixed_label.fallback_ru.as_str(),
    }
}

/// Рисует fixed-length numeric vector.
fn render_vector(
    ui: &mut Ui,
    field: &SettingsUiField,
    descriptor: &VectorDescriptor,
    actions: &mut Vec<SettingsUiAction>,
) {
    let SettingValue::NumericVector(current_values) = &field.draft_value else {
        render_type_mismatch(ui);
        return;
    };

    let mut edited_values = current_values.clone();
    let mut changed = false;

    for (index, value) in edited_values.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if let Some(label) = descriptor.labels.get(index) {
                ui.label(&label.fallback_ru);
            }
            changed |= render_vector_component(ui, value, &descriptor.element);
        });
    }

    if changed {
        actions.push(set_value_action(
            &field.descriptor.id,
            SettingValue::NumericVector(edited_values),
        ));
    }
}

/// Рисует один компонент numeric vector-а.
fn render_vector_component(ui: &mut Ui, value: &mut f64, descriptor: &NumericDescriptor) -> bool {
    let NumericRange::Float { min, max } = descriptor.range else {
        ui.label("Vector editor поддерживает только float range");
        return false;
    };

    let step = float_step(descriptor);
    let previous_value = *value;
    let mut slider = Slider::new(value, min..=max).step_by(step);
    if let Some(unit) = &descriptor.unit {
        slider = slider.text(unit.as_str());
    }
    ui.add(slider).changed() && slider_value_really_changed(previous_value, *value, step)
}

/// Рисует read-only representation neutral value-а.
fn render_read_only_value(ui: &mut Ui, value: &SettingValue) {
    ui.label(value_text(value));
}

/// Рисует validation/status messages field-а.
fn render_field_messages(ui: &mut Ui, field: &SettingsUiField) {
    if let Some(error) = &field.validation_error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }

    if let Some(status) = &field.status {
        ui.label(status);
    }
}

/// Рисует явную ошибку несоответствия value/editor.
fn render_type_mismatch(ui: &mut Ui) {
    ui.colored_label(
        ui.visuals().error_fg_color,
        "Тип draft-значения не совпадает с descriptor-ом",
    );
}

/// Можно ли показывать reset field button.
fn reset_field_enabled(field: &SettingsUiField) -> bool {
    field.descriptor.access == SettingAccess::ReadWrite
        && field.descriptor.default_behavior == DefaultBehavior::FromDefaultDocument
}

/// Добавляет unavailable-current option перед доступными option-ами.
fn append_unavailable_current_option(
    display_options: &mut Vec<SelectDisplayOption>,
    options_snapshot: &SettingOptions,
    selected_id: Option<&SettingOptionId>,
) {
    let SettingOptionCurrentValue::UnavailableCurrent { id, label } = &options_snapshot.current
    else {
        return;
    };

    if selected_id != Some(id) {
        return;
    }

    if options_snapshot
        .options
        .iter()
        .any(|option| &option.id == id)
    {
        return;
    }

    display_options.push(SelectDisplayOption {
        id: id.clone(),
        label: label.fallback_ru.clone(),
        available: false,
    });
}

/// Добавляет available options в display list.
fn append_available_options(
    display_options: &mut Vec<SelectDisplayOption>,
    options: &[SettingOption],
) {
    display_options.extend(options.iter().map(|option| SelectDisplayOption {
        id: option.id.clone(),
        label: option.label.fallback_ru.clone(),
        available: true,
    }));
}

/// Выбирает label для текущего select value-а.
fn select_label_for_value(
    selected_id: &SettingOptionId,
    display_options: &[SelectDisplayOption],
) -> String {
    display_options
        .iter()
        .find(|option| &option.id == selected_id)
        .map(|option| option.label.clone())
        .unwrap_or_else(|| selected_id.as_str().to_string())
}

/// Рисует status dynamic option snapshot-а.
fn render_options_status(ui: &mut Ui, dynamic_options: Option<&SettingOptions>) {
    let Some(options_snapshot) = dynamic_options else {
        return;
    };

    match &options_snapshot.status {
        SettingOptionsStatus::Loading => {
            ui.label("Список опций загружается");
        }
        SettingOptionsStatus::Ready => {}
        SettingOptionsStatus::Stale => {
            ui.label("Список опций устарел");
        }
        SettingOptionsStatus::Unavailable { message } => {
            ui.label(message);
        }
    }
}

/// Возвращает integer step для egui slider-а.
fn integer_step(descriptor: &NumericDescriptor) -> f64 {
    match descriptor.step {
        NumericStep::Integer(step) => step.max(1) as f64,
        NumericStep::Float(step) => step.max(1.0),
    }
}

/// Возвращает float step для egui slider-а.
fn float_step(descriptor: &NumericDescriptor) -> f64 {
    match descriptor.step {
        NumericStep::Integer(step) => (step.max(1)) as f64,
        NumericStep::Float(step) => step.max(f64::EPSILON),
    }
}

/// Форматирует neutral value для read-only отображения.
fn value_text(value: &SettingValue) -> String {
    match value {
        SettingValue::Bool(value) => value.to_string(),
        SettingValue::Integer(value) => value.to_string(),
        SettingValue::Float(value) => value.to_string(),
        SettingValue::Text(value) => value.clone(),
        SettingValue::Select(value) => value.as_str().to_string(),
        SettingValue::SelectList(values) => values
            .iter()
            .map(SettingOptionId::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        SettingValue::NumericVector(values) => values
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    }
}
