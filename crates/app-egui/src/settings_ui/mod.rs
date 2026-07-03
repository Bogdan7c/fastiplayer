//! Visual-only слой настроек.
//!
//! Этот модуль намеренно работает только с neutral settings descriptor-ами,
//! draft-значениями, статусами и cached option snapshots. Runtime owner
//! применяет эти действия отдельно.

pub mod field_widget;
pub mod layout;
pub mod section_list;

use settings_core::{
    OptionProviderId, SettingDescriptor, SettingGroupId, SettingId, SettingOptions,
    SettingSectionId, SettingValue, SettingsSurfaceId,
};

/// Visual action, который UI настроек отдаёт наружу вместо прямой работы с runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsUiAction {
    /// Пользователь запросил открытие окна настроек.
    Open,

    /// Пользователь нажал launcher: открыть настройки, если они закрыты,
    /// иначе закрыть (с теми же rollback/discard семантиками, что и `Cancel`).
    /// Visual слой не знает текущее open-state — решение принимает runtime owner.
    ToggleOpen,

    /// Пользователь закрыл окно или нажал отмену; draft должен быть отброшен runtime-слоем.
    Cancel,

    /// Пользователь изменил draft-значение одного settings field-а.
    SetValue {
        /// Stable id изменённой настройки.
        setting_id: SettingId,

        /// Новое neutral draft-значение.
        value: SettingValue,
    },

    /// Пользователь запросил reset одного field-а.
    ResetField {
        /// Stable id настройки, которую нужно вернуть к default document.
        setting_id: SettingId,
    },

    /// Пользователь запросил reset группы внутри section.
    ResetGroup {
        /// Section, внутри которой находится группа.
        section: SettingSectionId,

        /// Section-local group id.
        group: SettingGroupId,
    },

    /// Пользователь запросил reset visual surface.
    ResetSurface {
        /// Surface id, которым владеет runtime/controller layer.
        surface: SettingsSurfaceId,
    },

    /// Пользователь запросил reset всего settings document-а.
    ResetAll,

    /// Пользователь запросил refresh cached dynamic options у runtime owner-а.
    RefreshOptions {
        /// Stable id provider-а, который нужно обновить.
        provider_id: OptionProviderId,
    },

    /// Пользователь нажал Apply; окно остаётся открытым до решения runtime-слоя.
    Apply,

    /// Пользователь нажал OK; runtime-слой закрывает окно только после полного успеха.
    Ok,
}

/// Один field visual-модели, собранный runtime-слоем из descriptor-а и draft state.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsUiField {
    /// Descriptor остаётся единственным источником label/editor/layout metadata.
    pub descriptor: SettingDescriptor,

    /// Текущее draft-значение, которое UI может только заменить через action.
    pub draft_value: SettingValue,

    /// Field отличается от committed/default baseline по мнению runtime/controller layer.
    pub is_dirty: bool,

    /// Field-level validation error, уже подготовленная runtime/controller layer.
    pub validation_error: Option<String>,

    /// Field-level apply/preview/status note, уже подготовленная runtime/controller layer.
    pub status: Option<String>,

    /// Cached dynamic options snapshot для select-like editor-ов.
    pub options: Option<SettingOptions>,
}

impl SettingsUiField {
    /// Создаёт visual field без ошибок, статусов и dynamic options.
    #[must_use]
    pub fn new(descriptor: SettingDescriptor, draft_value: SettingValue) -> Self {
        Self {
            descriptor,
            draft_value,
            is_dirty: false,
            validation_error: None,
            status: None,
            options: None,
        }
    }

    /// Помечает field изменённым в draft.
    #[must_use]
    pub fn dirty(mut self, is_dirty: bool) -> Self {
        self.is_dirty = is_dirty;
        self
    }

    /// Добавляет уже вычисленную validation error строку.
    #[must_use]
    pub fn with_validation_error(mut self, message: impl Into<String>) -> Self {
        self.validation_error = Some(message.into());
        self
    }

    /// Добавляет уже вычисленный статус field-а.
    #[must_use]
    pub fn with_status(mut self, message: impl Into<String>) -> Self {
        self.status = Some(message.into());
        self
    }

    /// Передаёт cached dynamic options snapshot от runtime owner-а.
    #[must_use]
    pub fn with_options(mut self, options: SettingOptions) -> Self {
        self.options = Some(options);
        self
    }
}

/// Summary/status модель, которую окно только отображает.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsUiStatus {
    /// Короткий общий статус apply/preview/persist.
    pub summary: Option<String>,

    /// Детали статуса, уже сгруппированные runtime layer-ом.
    pub details: Vec<String>,
}

/// Состояние command-кнопок, вычисленное из draft/validation/runtime busy state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsUiCommandState {
    /// Есть ли изменения, которые имеет смысл отправить в Apply.
    pub has_dirty_fields: bool,

    /// Есть ли validation errors, блокирующие Apply/OK.
    pub has_validation_errors: bool,

    /// Runtime занят apply/preview/persist операцией и не должен принимать новую команду.
    pub is_busy: bool,
}

impl SettingsUiCommandState {
    /// Собирает состояние кнопок из fields и busy-флага runtime owner-а.
    #[must_use]
    pub fn from_fields(fields: &[SettingsUiField], is_busy: bool) -> Self {
        Self {
            has_dirty_fields: fields.iter().any(|field| field.is_dirty),
            has_validation_errors: fields.iter().any(|field| field.validation_error.is_some()),
            is_busy,
        }
    }

    /// Apply разрешён только когда есть изменения, нет ошибок и runtime свободен.
    #[must_use]
    pub fn can_apply(self) -> bool {
        self.has_dirty_fields && !self.has_validation_errors && !self.is_busy
    }

    /// OK разрешён без validation errors и без активной runtime операции.
    #[must_use]
    pub fn can_ok(self) -> bool {
        !self.has_validation_errors && !self.is_busy
    }
}

/// Полная visual-модель одного settings UI frame-а.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsUiModel {
    /// Открыто ли окно по мнению runtime/shell layer-а.
    pub is_open: bool,

    /// Descriptor-driven fields, которые можно показывать как persistent settings.
    pub fields: Vec<SettingsUiField>,

    /// Готовое состояние командных кнопок.
    pub command_state: SettingsUiCommandState,

    /// Готовый статус apply/preview/persist.
    pub status: SettingsUiStatus,
}

impl SettingsUiModel {
    /// Создаёт модель окна и вычисляет command state из field dirty/error state.
    #[must_use]
    pub fn new(is_open: bool, fields: Vec<SettingsUiField>, is_busy: bool) -> Self {
        let command_state = SettingsUiCommandState::from_fields(&fields, is_busy);
        Self {
            is_open,
            fields,
            command_state,
            status: SettingsUiStatus::default(),
        }
    }

    /// Подставляет готовый runtime/status snapshot.
    #[must_use]
    pub fn with_status(mut self, status: SettingsUiStatus) -> Self {
        self.status = status;
        self
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use settings_core::{
        AutoFixedPositiveIntegerDescriptor, DefaultBehavior, NumericDescriptor, NumericRange,
        NumericStep, SelectDescriptor, SettingAccess, SettingApplyMode, SettingDescriptorText,
        SettingEditor, SettingOption, SettingOptionCurrentValue, SettingOptions,
        SettingOptionsStatus, SettingPlacement, SettingRouteId, SettingText, SettingValue,
        TextDescriptor, TextFormat,
    };

    use super::*;

    fn text(text_id: &str, fallback_ru: &str) -> SettingText {
        SettingText::new(text_id, fallback_ru)
    }

    fn descriptor(
        id: &str,
        editor: SettingEditor,
        value_type: settings_core::SettingValueType,
    ) -> SettingDescriptor {
        SettingDescriptor {
            id: SettingId::from(id),
            path: id.into(),
            text: SettingDescriptorText::new(text(
                &format!("settings.{id}.label"),
                "Тестовая настройка",
            )),
            placement: SettingPlacement::new("render", "color", "main-settings-window"),
            value_type,
            editor,
            access: SettingAccess::ReadWrite,
            default_behavior: DefaultBehavior::FromDefaultDocument,
            route: SettingRouteId::from("render"),
            apply_mode: SettingApplyMode::CommittedApply,
        }
    }

    fn bool_field(id: &str, is_dirty: bool) -> SettingsUiField {
        SettingsUiField::new(
            descriptor(
                id,
                SettingEditor::Toggle,
                settings_core::SettingValueType::Bool,
            ),
            SettingValue::Bool(false),
        )
        .dirty(is_dirty)
    }

    #[test]
    fn action_mapping_preserves_setting_id_and_value() {
        let setting_id = SettingId::from("render.enabled");
        let action = field_widget::set_value_action(&setting_id, SettingValue::Bool(true));

        assert_eq!(
            action,
            SettingsUiAction::SetValue {
                setting_id,
                value: SettingValue::Bool(true)
            }
        );
    }

    #[test]
    fn ok_and_apply_buttons_respect_dirty_error_and_busy_state() {
        let clean_state =
            SettingsUiCommandState::from_fields(&[bool_field("render.enabled", false)], false);
        assert!(!clean_state.can_apply());
        assert!(clean_state.can_ok());

        let dirty_state =
            SettingsUiCommandState::from_fields(&[bool_field("render.enabled", true)], false);
        assert!(dirty_state.can_apply());
        assert!(dirty_state.can_ok());

        let error_state = SettingsUiCommandState::from_fields(
            &[bool_field("render.enabled", true).with_validation_error("ошибка")],
            false,
        );
        assert!(!error_state.can_apply());
        assert!(!error_state.can_ok());

        let busy_state =
            SettingsUiCommandState::from_fields(&[bool_field("render.enabled", true)], true);
        assert!(!busy_state.can_apply());
        assert!(!busy_state.can_ok());
    }

    #[test]
    fn apply_and_ok_clicks_emit_only_when_enabled() {
        let enabled_state = SettingsUiCommandState {
            has_dirty_fields: true,
            has_validation_errors: false,
            is_busy: false,
        };
        let disabled_state = SettingsUiCommandState {
            has_dirty_fields: false,
            has_validation_errors: true,
            is_busy: false,
        };

        assert_eq!(
            layout::apply_action_for_button(true, enabled_state),
            Some(SettingsUiAction::Apply)
        );
        assert_eq!(layout::apply_action_for_button(true, disabled_state), None);
        assert_eq!(
            layout::ok_action_for_button(true, enabled_state),
            Some(SettingsUiAction::Ok)
        );
        assert_eq!(layout::ok_action_for_button(true, disabled_state), None);
    }

    #[test]
    fn reset_group_and_reset_all_emit_distinct_actions() {
        let reset_group = layout::reset_group_action(
            &SettingSectionId::from("render"),
            &SettingGroupId::from("color"),
        );
        let reset_surface = section_list::reset_surface_action(
            &settings_core::SettingsSurfaceId::from("main-settings-window"),
        );
        let reset_all = layout::reset_all_action_for_button(true);

        assert_eq!(
            reset_group,
            SettingsUiAction::ResetGroup {
                section: SettingSectionId::from("render"),
                group: SettingGroupId::from("color")
            }
        );
        assert_eq!(
            reset_surface,
            SettingsUiAction::ResetSurface {
                surface: settings_core::SettingsSurfaceId::from("main-settings-window")
            }
        );
        assert_eq!(reset_all, Some(SettingsUiAction::ResetAll));
        assert_ne!(Some(reset_group), reset_all);
        assert_ne!(Some(reset_surface), reset_all);
    }

    #[test]
    fn descriptor_driven_model_does_not_inject_current_playback_controls() {
        let model = SettingsUiModel::new(false, Vec::new(), false);
        let sections = section_list::sections_for_fields(&model.fields);

        assert!(sections.is_empty());
    }

    #[test]
    fn dynamic_unavailable_current_option_is_renderable_from_snapshot() {
        let unavailable_id = settings_core::SettingOptionId::from("usb-dac-42");
        let dynamic_options = SettingOptions {
            provider_id: "audio-output".into(),
            status: SettingOptionsStatus::Ready,
            options: vec![SettingOption::new(
                "default",
                text("settings.audio.output.default", "Системное устройство"),
            )],
            current: SettingOptionCurrentValue::UnavailableCurrent {
                id: unavailable_id.clone(),
                label: text("settings.audio.output.saved", "USB DAC"),
            },
        };

        let display_options = field_widget::select_display_options(
            &[],
            Some(&dynamic_options),
            Some(&unavailable_id),
        );

        assert_eq!(display_options[0].id, unavailable_id);
        assert!(!display_options[0].available);
        assert!(display_options[0].label.contains("USB DAC"));
    }

    #[test]
    fn source_guardrail_settings_ui_has_no_runtime_or_project_config_refs() {
        let settings_ui_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/settings_ui");
        let forbidden_patterns = [
            concat!("rustiplayer_", "config").to_string(),
            format!("{}{}", "app", "config"),
            format!("{}{}", "player", "worker"),
            concat!("render_", "wg", "pu").to_string(),
            concat!("render-", "wg", "pu").to_string(),
            concat!("wg", "pu").to_string(),
            concat!("to", "ml").to_string(),
            concat!("save_", "validated").to_string(),
        ];

        for entry_result in
            std::fs::read_dir(settings_ui_dir).expect("settings_ui directory must exist")
        {
            let entry = entry_result.expect("settings_ui entry must be readable");
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }

            let source =
                std::fs::read_to_string(&path).expect("settings_ui source must be readable");
            let normalized_source = source.to_lowercase();
            for forbidden_pattern in &forbidden_patterns {
                assert!(
                    !normalized_source.contains(forbidden_pattern),
                    "{} must not reference `{}`",
                    path.display(),
                    forbidden_pattern
                );
            }
        }
    }

    #[test]
    fn numeric_text_and_select_descriptors_can_feed_visual_model() {
        let fields = vec![
            SettingsUiField::new(
                descriptor(
                    "render.brightness",
                    SettingEditor::Numeric(NumericDescriptor::new(
                        NumericRange::Float { min: 0.0, max: 1.0 },
                        NumericStep::Float(0.01),
                        None,
                    )),
                    settings_core::SettingValueType::Float,
                ),
                SettingValue::Float(0.5),
            ),
            SettingsUiField::new(
                descriptor(
                    "ui.title",
                    SettingEditor::Text(TextDescriptor::new(TextFormat::SingleLine)),
                    settings_core::SettingValueType::Text,
                ),
                SettingValue::Text("rustiplayer".to_string()),
            ),
            SettingsUiField::new(
                descriptor(
                    "audio.device",
                    SettingEditor::Select(SelectDescriptor::Dynamic {
                        provider_id: "audio-output".into(),
                    }),
                    settings_core::SettingValueType::Select,
                ),
                SettingValue::Select("default".into()),
            ),
            SettingsUiField::new(
                descriptor(
                    "test.auto_fixed_resource_slots",
                    SettingEditor::AutoFixedPositiveInteger(
                        AutoFixedPositiveIntegerDescriptor::new(
                            text("settings.frame_server.budget.auto", "Авто"),
                            text("settings.frame_server.budget.fixed", "Фиксированно"),
                            None,
                        ),
                    ),
                    settings_core::SettingValueType::Text,
                ),
                SettingValue::Text("auto".to_string()),
            ),
        ];

        let model = SettingsUiModel::new(true, fields, false);

        assert_eq!(model.fields.len(), 4);
        assert!(model.command_state.can_ok());
    }
}
