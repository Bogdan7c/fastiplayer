//! Layout окна настроек: единый vertical settings list и command footer.

use egui::{Button, Layout, RichText, ScrollArea, Ui};
use settings_core::{SettingGroupId, SettingSectionId};

use super::{
    SettingsUiAction, SettingsUiCommandState, SettingsUiModel, field_widget, section_list,
};

/// Минимальная высота прокручиваемой области, чтобы список не превращался в узкую полоску.
const SETTINGS_LIST_MIN_HEIGHT: f32 = 240.0;

/// Рисует всё содержимое settings window без прямого доступа к runtime.
pub fn show(ui: &mut Ui, model: &SettingsUiModel, actions: &mut Vec<SettingsUiAction>) {
    render_status(ui, model);

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .min_scrolled_height(SETTINGS_LIST_MIN_HEIGHT)
        .show(ui, |ui| {
            render_settings_list(ui, model, actions);
        });

    ui.separator();
    render_footer(ui, model.command_state, actions);
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

/// Рисует все settings fields одним вертикальным списком, сгруппированным по section/group.
fn render_settings_list(ui: &mut Ui, model: &SettingsUiModel, actions: &mut Vec<SettingsUiAction>) {
    let sections = section_list::sections_for_fields(&model.fields);
    if sections.is_empty() {
        ui.label("Настройки недоступны");
        return;
    }

    for section in &sections {
        render_section(ui, model, section, actions);
    }
}

/// Рисует один section: заголовок, reset surface и все его группы.
fn render_section(
    ui: &mut Ui,
    model: &SettingsUiModel,
    section: &section_list::SettingsUiSection,
    actions: &mut Vec<SettingsUiAction>,
) {
    render_section_header(ui, section, actions);

    let mut current_group: Option<SettingGroupId> = None;
    for field in model
        .fields
        .iter()
        .filter(|field| field.descriptor.placement.section == section.section)
    {
        if current_group.as_ref() != Some(&field.descriptor.placement.group) {
            current_group = Some(field.descriptor.placement.group.clone());
            render_group_header(
                ui,
                &section.section,
                &field.descriptor.placement.group,
                actions,
            );
        }

        field_widget::show(ui, field, actions);
        ui.add_space(8.0);
    }

    ui.separator();
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
