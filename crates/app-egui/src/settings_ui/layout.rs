//! Layout окна настроек: sections, scrollable content и command footer.

use egui::{Button, Layout, RichText, ScrollArea, Ui};
use settings_core::{SettingGroupId, SettingSectionId};

use super::{
    SettingsUiAction, SettingsUiCommandState, SettingsUiModel, SettingsUiState, field_widget,
    section_list,
};

/// Рисует всё содержимое settings window без прямого доступа к runtime.
pub fn show(
    ui: &mut Ui,
    model: &SettingsUiModel,
    state: &mut SettingsUiState,
    actions: &mut Vec<SettingsUiAction>,
) {
    section_list::ensure_valid_selected_section(state, &model.fields);
    render_status(ui, model);

    ui.horizontal(|ui| {
        ui.set_min_height(400.0);
        ui.vertical(|ui| {
            ui.set_width(180.0);
            section_list::show(ui, &model.fields, state, actions);
        });
        ui.separator();
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                render_selected_section(ui, model, state, actions);
            });
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

/// Рисует fields выбранного section-а, сгруппированные по descriptor placement.
fn render_selected_section(
    ui: &mut Ui,
    model: &SettingsUiModel,
    state: &SettingsUiState,
    actions: &mut Vec<SettingsUiAction>,
) {
    let Some(selected_section) = section_list::selected_section(state, &model.fields) else {
        ui.label("Настройки недоступны");
        return;
    };

    let mut current_group: Option<SettingGroupId> = None;

    for field in model
        .fields
        .iter()
        .filter(|field| field.descriptor.placement.section == *selected_section)
    {
        if current_group.as_ref() != Some(&field.descriptor.placement.group) {
            current_group = Some(field.descriptor.placement.group.clone());
            render_group_header(
                ui,
                selected_section,
                &field.descriptor.placement.group,
                actions,
            );
        }

        field_widget::show(ui, field, actions);
        ui.add_space(8.0);
    }
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
