//! Layout окна настроек: единый vertical settings list и command footer.

use egui::collapsing_header::CollapsingState;
use egui::{Button, Layout, RichText, ScrollArea, Ui};
use settings_core::{SettingGroupId, SettingSectionId};

use super::{
    SettingsUiAction, SettingsUiCommandState, SettingsUiField, SettingsUiModel, field_widget,
    section_list,
};

/// Дополнительный вертикальный запас под separator и отступы вокруг footer-а.
const FOOTER_VERTICAL_SPACING_ROWS: f32 = 3.0;

/// Рисует всё содержимое settings window без прямого доступа к runtime.
///
/// Раскладка: ряд табов секций, scroll list выбранной секции, затем status
/// в зоне фиксированной высоты и footer. Статус намеренно рисуется внизу в
/// зарезервированном блоке: его появление/смена текста (preview/reset ticks)
/// не должны менять позиции полей и дёргать панель во время drag-а слайдера.
pub fn show(ui: &mut Ui, model: &SettingsUiModel, actions: &mut Vec<SettingsUiAction>) {
    let status_height = status_reserved_height(ui);

    let sections = section_list::sections_for_fields(&model.fields);
    if sections.is_empty() {
        ui.label("Настройки недоступны");
    } else {
        let selected_section = &sections[render_section_tabs(ui, &sections)];
        ui.separator();

        let settings_list_max_height = settings_list_max_height(
            ui.available_height(),
            footer_reserved_height(ui) + status_height,
        );

        let section_fields: Vec<&SettingsUiField> = model
            .fields
            .iter()
            .filter(|field| field.descriptor.placement.section == selected_section.section)
            .collect();

        ScrollArea::vertical()
            .id_salt(selected_section.section.as_str())
            .auto_shrink([false, false])
            .max_height(settings_list_max_height)
            .show(ui, |ui| {
                render_section(ui, &section_fields, selected_section, actions);
            });
    }

    ui.separator();
    ui.allocate_ui(egui::vec2(ui.available_width(), status_height), |ui| {
        ui.set_min_height(status_height);
        ScrollArea::vertical()
            .id_salt("settings_status")
            .auto_shrink([false, false])
            .max_height(status_height)
            .show(ui, |ui| {
                render_status(ui, model);
            });
    });
    ui.separator();
    render_footer(ui, model.command_state, actions);
}

/// Строки, резервируемые под status snapshot между списком и footer-ом.
const STATUS_RESERVED_ROWS: f32 = 3.0;

/// Фиксированная высота status-зоны, не зависящая от текущего текста статуса.
fn status_reserved_height(ui: &Ui) -> f32 {
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    (row_height + ui.spacing().item_spacing.y) * STATUS_RESERVED_ROWS
}

/// Рисует ряд табов секций и возвращает индекс выбранной.
///
/// Выбор — чистое view state, поэтому живёт в egui temp memory, а не в runtime.
fn render_section_tabs(ui: &mut Ui, sections: &[section_list::SettingsUiSection]) -> usize {
    let selection_id = ui.make_persistent_id("settings_section_tab");
    let stored_section: Option<String> = ui.data_mut(|data| data.get_temp(selection_id));
    let mut selected_index = stored_section
        .and_then(|stored| {
            sections
                .iter()
                .position(|section| section.section.as_str() == stored)
        })
        .unwrap_or(0);

    ui.horizontal_wrapped(|ui| {
        for (index, section) in sections.iter().enumerate() {
            if ui
                .selectable_label(
                    index == selected_index,
                    section_list::section_label(section),
                )
                .clicked()
            {
                selected_index = index;
            }
        }
    });

    ui.data_mut(|data| {
        data.insert_temp(
            selection_id,
            sections[selected_index].section.as_str().to_string(),
        );
    });

    selected_index
}

/// Считает высоту, которую можно отдать scroll list-у, не выталкивая footer из окна.
#[must_use]
pub(crate) fn settings_list_max_height(available_height: f32, footer_reserved_height: f32) -> f32 {
    if available_height.is_finite() {
        (available_height - footer_reserved_height).max(0.0)
    } else {
        f32::INFINITY
    }
}

/// Оценивает место под нижнюю строку команд через текущие spacing-настройки egui.
fn footer_reserved_height(ui: &Ui) -> f32 {
    let spacing = ui.spacing();
    spacing.interact_size.y + spacing.item_spacing.y * FOOTER_VERTICAL_SPACING_ROWS
}

/// Рисует общий status snapshot, если runtime передал его в модель.
fn render_status(ui: &mut Ui, model: &SettingsUiModel) {
    if let Some(summary) = &model.status.summary {
        ui.label(RichText::new(summary).strong());
    }

    for detail in &model.status.details {
        ui.label(detail);
    }
}

/// Рисует выбранный section: заголовок с surface reset и группы как сворачиваемые блоки.
fn render_section(
    ui: &mut Ui,
    fields: &[&SettingsUiField],
    section: &section_list::SettingsUiSection,
    actions: &mut Vec<SettingsUiAction>,
) {
    render_section_header(ui, section, actions);

    let mut groups: Vec<(&SettingGroupId, Vec<&SettingsUiField>)> = Vec::new();
    for field in fields.iter().copied() {
        let group = &field.descriptor.placement.group;
        match groups.last_mut() {
            Some((current_group, group_fields)) if *current_group == group => {
                group_fields.push(field);
            }
            _ => groups.push((group, vec![field])),
        }
    }

    for (group_index, (group, group_fields)) in groups.iter().enumerate() {
        // group.as_str() не уникален внутри секции (например, два разных
        // "profile" в render), поэтому в ID входит и порядковый номер группы.
        let group_state_id = ui.make_persistent_id((
            "settings_group",
            section.section.as_str(),
            group.as_str(),
            group_index,
        ));
        CollapsingState::load_with_default_open(ui.ctx(), group_state_id, group_index == 0)
            .show_header(ui, |ui| {
                render_group_header(ui, &section.section, group, actions);
            })
            .body(|ui| {
                for field in group_fields {
                    field_widget::show(ui, field, actions);
                    ui.add_space(8.0);
                }
            });
    }
}

/// Рисует заголовок section-а и surface reset action для всего экрана настроек.
fn render_section_header(
    ui: &mut Ui,
    section: &section_list::SettingsUiSection,
    actions: &mut Vec<SettingsUiAction>,
) {
    ui.horizontal(|ui| {
        ui.heading(section_list::section_label(section));
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Сбросить экран").clicked() {
                actions.push(section_list::reset_surface_action(&section.surface));
            }
        });
    });
}

/// Рисует заголовок группы и group reset action.
fn render_group_header(
    ui: &mut Ui,
    section: &SettingSectionId,
    group: &SettingGroupId,
    actions: &mut Vec<SettingsUiAction>,
) {
    ui.horizontal(|ui| {
        ui.heading(group.as_str());
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Сбросить группу").clicked() {
                actions.push(reset_group_action(section, group));
            }
        });
    });
}

/// Рисует нижнюю command row: Cancel, Reset all, Apply, OK.
fn render_footer(
    ui: &mut Ui,
    command_state: SettingsUiCommandState,
    actions: &mut Vec<SettingsUiAction>,
) {
    ui.horizontal(|ui| {
        if let Some(action) = reset_all_action_for_button(ui.button("Сбросить всё").clicked())
        {
            actions.push(action);
        }

        if ui.button("Отмена").clicked() {
            actions.push(SettingsUiAction::Cancel);
        }

        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(action) = ok_action_for_button(
                ui.add_enabled(command_state.can_ok(), Button::new("OK"))
                    .clicked(),
                command_state,
            ) {
                actions.push(action);
            }

            if let Some(action) = apply_action_for_button(
                ui.add_enabled(command_state.can_apply(), Button::new("Применить"))
                    .clicked(),
                command_state,
            ) {
                actions.push(action);
            }
        });
    });
}

/// Pure mapping для Apply button с учётом enabled state.
#[must_use]
pub(crate) fn apply_action_for_button(
    clicked: bool,
    command_state: SettingsUiCommandState,
) -> Option<SettingsUiAction> {
    (clicked && command_state.can_apply()).then_some(SettingsUiAction::Apply)
}

/// Pure mapping для OK button с учётом enabled state.
#[must_use]
pub(crate) fn ok_action_for_button(
    clicked: bool,
    command_state: SettingsUiCommandState,
) -> Option<SettingsUiAction> {
    (clicked && command_state.can_ok()).then_some(SettingsUiAction::Ok)
}

/// Pure mapping для reset all button.
#[must_use]
pub(crate) fn reset_all_action_for_button(clicked: bool) -> Option<SettingsUiAction> {
    clicked.then_some(SettingsUiAction::ResetAll)
}

/// Pure mapping для reset group command.
#[must_use]
pub(crate) fn reset_group_action(
    section: &SettingSectionId,
    group: &SettingGroupId,
) -> SettingsUiAction {
    SettingsUiAction::ResetGroup {
        section: section.clone(),
        group: group.clone(),
    }
}
