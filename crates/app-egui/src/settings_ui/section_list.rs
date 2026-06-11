//! Section list для descriptor-driven settings UI.

use std::collections::BTreeSet;

use egui::Ui;
use settings_core::{SelectDescriptor, SettingEditor, SettingSectionId, SettingsSurfaceId};

use super::{SettingsUiAction, SettingsUiField, SettingsUiState};

/// Summary одного visual section-а, вычисляемый только из descriptor placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsUiSection {
    /// Stable section id из descriptor placement.
    pub section: SettingSectionId,

    /// Preferred surface первого field-а в section-е.
    pub surface: SettingsSurfaceId,

    /// Количество dirty fields внутри section-а.
    pub dirty_fields: usize,
}

/// Рисует список sections и surface reset command для выбранного section-а.
pub fn show(
    ui: &mut Ui,
    fields: &[SettingsUiField],
    state: &mut SettingsUiState,
    actions: &mut Vec<SettingsUiAction>,
) {
    let sections = sections_for_fields(fields);

    for section in &sections {
        let is_selected = state.selected_section.as_ref() == Some(&section.section);
        let label = section_label(section);
        if ui.selectable_label(is_selected, label).clicked() {
            state.selected_section = Some(section.section.clone());
            actions.extend(refresh_actions_for_section(fields, &section.section));
        }
    }

    if let Some(selected_surface) = selected_surface(state, &sections) {
        ui.separator();
        if ui.button("Сбросить экран").clicked() {
            actions.push(reset_surface_action(selected_surface));
        }
    }
}

/// Строит sections из fields без добавления каких-либо встроенных playback controls.
#[must_use]
pub fn sections_for_fields(fields: &[SettingsUiField]) -> Vec<SettingsUiSection> {
    let mut sections: Vec<SettingsUiSection> = Vec::new();

    for field in fields {
        if let Some(existing_section) = sections
            .iter_mut()
            .find(|section| section.section == field.descriptor.placement.section)
        {
            if field.is_dirty {
                existing_section.dirty_fields += 1;
            }
            continue;
        }

        sections.push(SettingsUiSection {
            section: field.descriptor.placement.section.clone(),
            surface: field.descriptor.placement.preferred_surface.clone(),
            dirty_fields: usize::from(field.is_dirty),
        });
    }

    sections.sort_by(|left, right| left.section.cmp(&right.section));
    sections
}

/// Гарантирует, что selected section указывает на существующий section.
pub fn ensure_valid_selected_section(state: &mut SettingsUiState, fields: &[SettingsUiField]) {
    if selected_section(state, fields).is_some() {
        return;
    }

    state.selected_section = sections_for_fields(fields)
        .first()
        .map(|section| section.section.clone());
}

/// Возвращает выбранный section или первый доступный section.
#[must_use]
pub fn selected_section<'fields>(
    state: &SettingsUiState,
    fields: &'fields [SettingsUiField],
) -> Option<&'fields SettingSectionId> {
    let requested_section = state.selected_section.as_ref();

    fields
        .iter()
        .find(|field| Some(&field.descriptor.placement.section) == requested_section)
        .or_else(|| fields.first())
        .map(|field| &field.descriptor.placement.section)
}

/// Pure mapping для reset surface command.
#[must_use]
pub(crate) fn reset_surface_action(surface: &SettingsSurfaceId) -> SettingsUiAction {
    SettingsUiAction::ResetSurface {
        surface: surface.clone(),
    }
}

/// Делает label section-а из stable id и dirty-счётчика.
fn section_label(section: &SettingsUiSection) -> String {
    if section.dirty_fields == 0 {
        section.section.as_str().to_string()
    } else {
        format!("{} ({})", section.section.as_str(), section.dirty_fields)
    }
}

/// Находит surface выбранного section-а.
fn selected_surface<'sections>(
    state: &SettingsUiState,
    sections: &'sections [SettingsUiSection],
) -> Option<&'sections SettingsSurfaceId> {
    let selected_section = state.selected_section.as_ref()?;
    sections
        .iter()
        .find(|section| &section.section == selected_section)
        .map(|section| &section.surface)
}

/// Создаёт refresh actions для dynamic providers выбранной секции.
fn refresh_actions_for_section(
    fields: &[SettingsUiField],
    section: &SettingSectionId,
) -> Vec<SettingsUiAction> {
    let provider_ids = fields
        .iter()
        .filter(|field| &field.descriptor.placement.section == section)
        .filter_map(|field| match &field.descriptor.editor {
            SettingEditor::Select(SelectDescriptor::Dynamic { provider_id }) => {
                Some(provider_id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    provider_ids
        .into_iter()
        .map(|provider_id| SettingsUiAction::RefreshOptions { provider_id })
        .collect()
}
