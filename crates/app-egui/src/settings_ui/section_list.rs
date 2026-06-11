//! Section list для descriptor-driven settings UI.

use settings_core::{SettingSectionId, SettingsSurfaceId};

use super::{SettingsUiAction, SettingsUiField};

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

/// Pure mapping для reset surface command.
#[must_use]
pub(crate) fn reset_surface_action(surface: &SettingsSurfaceId) -> SettingsUiAction {
    SettingsUiAction::ResetSurface {
        surface: surface.clone(),
    }
}

/// Делает label section-а из stable id и dirty-счётчика.
#[must_use]
pub(crate) fn section_label(section: &SettingsUiSection) -> String {
    if section.dirty_fields == 0 {
        section.section.as_str().to_string()
    } else {
        format!("{} ({})", section.section.as_str(), section.dirty_fields)
    }
}
