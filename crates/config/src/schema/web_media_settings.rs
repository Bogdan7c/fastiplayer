//! Settings registry для provider-neutral секции `[web_media]`.

use settings_core::{
    DefaultBehavior, NumericDescriptor, NumericRange, NumericStep, SelectDescriptor, SettingAccess,
    SettingAccessor, SettingApplyMode, SettingDescriptor, SettingDescriptorText, SettingEditor,
    SettingOption, SettingPlacement, SettingText, SettingValue, SettingValueType, SettingsError,
    SettingsRegistry, SettingsResult, SettingsSchema,
};

use super::{PreferredVideoHeight, WebMediaConfig, WebMediaHdrSelection};

/// Поле web-media policy, которым владеет один hand-written accessor.
#[derive(Debug, Clone, Copy)]
enum WebMediaField {
    HdrSelection,
    PreferredVideoHeight,
    VodEndpointRecoveryEnabled,
    VodEndpointRecoveryMaxAttempts,
    VodEndpointRecoveryInitialBackoff,
    VodEndpointRecoveryMaxBackoff,
    VodEndpointRecoveryStableReset,
}

/// Adapter между neutral settings value и typed web-media policy.
struct WebMediaAccessor {
    field: WebMediaField,
}

impl SettingsSchema for WebMediaConfig {
    /// Строит provider-neutral registry без extractor-specific labels.
    fn settings_registry() -> SettingsResult<SettingsRegistry<Self>> {
        let mut registry = SettingsRegistry::empty();
        register_setting(
            &mut registry,
            descriptor(
                "web_media.hdr_selection",
                "Динамический диапазон web media",
                "Выбирать только SDR или предпочитать HDR при полной поддержке decoder и renderer с автоматическим SDR fallback.",
                "quality",
                SettingValueType::Select,
                SettingEditor::Select(SelectDescriptor::Static {
                    options: hdr_selection_options(),
                }),
            ),
            WebMediaField::HdrSelection,
        )?;
        register_setting(
            &mut registry,
            descriptor(
                "web_media.preferred_video_height",
                "Предпочитаемая высота web video",
                "Лучшее доступное представление либо глобальная высота с fallback: точная, ближайшая ниже, затем ближайшая выше.",
                "quality",
                SettingValueType::Select,
                SettingEditor::Select(SelectDescriptor::Static {
                    options: preferred_height_options(),
                }),
            ),
            WebMediaField::PreferredVideoHeight,
        )?;
        register_setting(
            &mut registry,
            descriptor(
                "web_media.vod_endpoint_recovery_enabled",
                "Автовосстановление web VOD",
                "Автоматически переоткрывать web VOD после истечения временного endpoint-а.",
                "recovery",
                SettingValueType::Bool,
                SettingEditor::Toggle,
            ),
            WebMediaField::VodEndpointRecoveryEnabled,
        )?;
        register_recovery_integer(
            &mut registry,
            RecoveryIntegerDescriptor {
                id: "web_media.vod_endpoint_recovery_max_consecutive_attempts",
                label_ru: "Попытки восстановления web VOD",
                description_ru: "Максимальное число последовательных переоткрытий до terminal failure.",
                maximum: crate::validation::MAX_WEB_MEDIA_VOD_RECOVERY_ATTEMPTS,
                step: 1,
                unit: "attempts",
            },
            WebMediaField::VodEndpointRecoveryMaxAttempts,
        )?;
        register_recovery_integer(
            &mut registry,
            RecoveryIntegerDescriptor {
                id: "web_media.vod_endpoint_recovery_initial_backoff_ms",
                label_ru: "Начальная задержка восстановления web VOD",
                description_ru: "Начальная задержка перед повторным source resolution.",
                maximum: crate::validation::MAX_WEB_MEDIA_VOD_RECOVERY_BACKOFF_MS,
                step: 50,
                unit: "ms",
            },
            WebMediaField::VodEndpointRecoveryInitialBackoff,
        )?;
        register_recovery_integer(
            &mut registry,
            RecoveryIntegerDescriptor {
                id: "web_media.vod_endpoint_recovery_max_backoff_ms",
                label_ru: "Максимальная задержка восстановления web VOD",
                description_ru: "Верхняя граница exponential backoff между recovery attempts.",
                maximum: crate::validation::MAX_WEB_MEDIA_VOD_RECOVERY_BACKOFF_MS,
                step: 100,
                unit: "ms",
            },
            WebMediaField::VodEndpointRecoveryMaxBackoff,
        )?;
        register_recovery_integer(
            &mut registry,
            RecoveryIntegerDescriptor {
                id: "web_media.vod_endpoint_recovery_stable_reset_ms",
                label_ru: "Сброс бюджета восстановления web VOD",
                description_ru: "Время стабильного playback, после которого последовательный recovery budget сбрасывается.",
                maximum: crate::validation::MAX_WEB_MEDIA_VOD_RECOVERY_STABLE_RESET_MS,
                step: 1_000,
                unit: "ms",
            },
            WebMediaField::VodEndpointRecoveryStableReset,
        )?;
        Ok(registry)
    }
}

impl SettingAccessor<WebMediaConfig> for WebMediaAccessor {
    fn get(&self, document: &WebMediaConfig) -> SettingsResult<SettingValue> {
        Ok(self.field.get(document))
    }

    fn set(&self, document: &mut WebMediaConfig, value: SettingValue) -> SettingsResult<()> {
        self.field.set(document, value)
    }

    fn reset(
        &self,
        document: &mut WebMediaConfig,
        default_document: &WebMediaConfig,
    ) -> SettingsResult<()> {
        self.field.reset(document, default_document);
        Ok(())
    }
}

impl WebMediaField {
    /// Проецирует typed policy на neutral settings value.
    fn get(self, config: &WebMediaConfig) -> SettingValue {
        match self {
            Self::HdrSelection => {
                SettingValue::Select(hdr_selection_id(config.hdr_selection).into())
            }
            Self::PreferredVideoHeight => {
                SettingValue::Select(config.preferred_video_height.map_or_else(
                    || "best_playable".into(),
                    |height| height.pixels().to_string().into(),
                ))
            }
            Self::VodEndpointRecoveryEnabled => {
                SettingValue::Bool(config.vod_endpoint_recovery_enabled)
            }
            Self::VodEndpointRecoveryMaxAttempts => integer_value(
                config.vod_endpoint_recovery_max_consecutive_attempts,
                "recovery attempt budget",
            ),
            Self::VodEndpointRecoveryInitialBackoff => integer_value(
                config.vod_endpoint_recovery_initial_backoff_ms,
                "initial recovery backoff",
            ),
            Self::VodEndpointRecoveryMaxBackoff => integer_value(
                config.vod_endpoint_recovery_max_backoff_ms,
                "maximum recovery backoff",
            ),
            Self::VodEndpointRecoveryStableReset => integer_value(
                config.vod_endpoint_recovery_stable_reset_ms,
                "stable recovery reset",
            ),
        }
    }

    /// Применяет neutral value только к принадлежащему accessor-у полю.
    fn set(self, config: &mut WebMediaConfig, value: SettingValue) -> SettingsResult<()> {
        match self {
            Self::HdrSelection => config.hdr_selection = hdr_selection_value(value)?,
            Self::PreferredVideoHeight => {
                config.preferred_video_height = preferred_height_value(value)?;
            }
            Self::VodEndpointRecoveryEnabled => {
                config.vod_endpoint_recovery_enabled =
                    bool_value("web_media.vod_endpoint_recovery_enabled", value)?;
            }
            Self::VodEndpointRecoveryMaxAttempts => {
                config.vod_endpoint_recovery_max_consecutive_attempts = u64_value(
                    "web_media.vod_endpoint_recovery_max_consecutive_attempts",
                    value,
                )?;
            }
            Self::VodEndpointRecoveryInitialBackoff => {
                config.vod_endpoint_recovery_initial_backoff_ms =
                    u64_value("web_media.vod_endpoint_recovery_initial_backoff_ms", value)?;
            }
            Self::VodEndpointRecoveryMaxBackoff => {
                config.vod_endpoint_recovery_max_backoff_ms =
                    u64_value("web_media.vod_endpoint_recovery_max_backoff_ms", value)?;
            }
            Self::VodEndpointRecoveryStableReset => {
                config.vod_endpoint_recovery_stable_reset_ms =
                    u64_value("web_media.vod_endpoint_recovery_stable_reset_ms", value)?;
            }
        }
        Ok(())
    }

    /// Восстанавливает field из default web-media policy.
    fn reset(self, config: &mut WebMediaConfig, default_config: &WebMediaConfig) {
        match self {
            Self::HdrSelection => config.hdr_selection = default_config.hdr_selection,
            Self::PreferredVideoHeight => {
                config.preferred_video_height = default_config.preferred_video_height;
            }
            Self::VodEndpointRecoveryEnabled => {
                config.vod_endpoint_recovery_enabled = default_config.vod_endpoint_recovery_enabled;
            }
            Self::VodEndpointRecoveryMaxAttempts => {
                config.vod_endpoint_recovery_max_consecutive_attempts =
                    default_config.vod_endpoint_recovery_max_consecutive_attempts;
            }
            Self::VodEndpointRecoveryInitialBackoff => {
                config.vod_endpoint_recovery_initial_backoff_ms =
                    default_config.vod_endpoint_recovery_initial_backoff_ms;
            }
            Self::VodEndpointRecoveryMaxBackoff => {
                config.vod_endpoint_recovery_max_backoff_ms =
                    default_config.vod_endpoint_recovery_max_backoff_ms;
            }
            Self::VodEndpointRecoveryStableReset => {
                config.vod_endpoint_recovery_stable_reset_ms =
                    default_config.vod_endpoint_recovery_stable_reset_ms;
            }
        }
    }
}

/// Named metadata одного recovery integer setting-а.
struct RecoveryIntegerDescriptor {
    id: &'static str,
    label_ru: &'static str,
    description_ru: &'static str,
    maximum: u64,
    step: i64,
    unit: &'static str,
}

/// Регистрирует recovery integer через named descriptor без позиционной каши.
fn register_recovery_integer(
    registry: &mut SettingsRegistry<WebMediaConfig>,
    spec: RecoveryIntegerDescriptor,
    field: WebMediaField,
) -> SettingsResult<()> {
    register_setting(
        registry,
        descriptor(
            spec.id,
            spec.label_ru,
            spec.description_ru,
            "recovery",
            SettingValueType::Integer,
            SettingEditor::Numeric(NumericDescriptor::new(
                NumericRange::Integer {
                    min: 1,
                    max: spec.maximum as i64,
                },
                NumericStep::Integer(spec.step),
                Some(spec.unit.into()),
            )),
        ),
        field,
    )
}

/// Регистрирует descriptor и его единственный typed accessor.
fn register_setting(
    registry: &mut SettingsRegistry<WebMediaConfig>,
    descriptor: SettingDescriptor,
    field: WebMediaField,
) -> SettingsResult<()> {
    registry.register(descriptor, WebMediaAccessor { field })
}

/// Строит общую config-owned metadata оболочку web-media setting-а.
fn descriptor(
    id: &'static str,
    label_ru: &'static str,
    description_ru: &'static str,
    group: &'static str,
    value_type: SettingValueType,
    editor: SettingEditor,
) -> SettingDescriptor {
    let label_id = format!("settings.{id}.label");
    SettingDescriptor {
        id: id.into(),
        path: id.into(),
        text: SettingDescriptorText::new(SettingText::new(label_id.clone(), label_ru))
            .with_description(SettingText::new(
                format!("settings.{id}.description"),
                description_ru,
            )),
        placement: SettingPlacement::new("web_media", group, "main-settings-window"),
        value_type,
        editor,
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: "web_media".into(),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}

/// Возвращает stable HDR options без Debug-based идентификаторов.
fn hdr_selection_options() -> Vec<SettingOption> {
    [
        (
            WebMediaHdrSelection::SdrOnly,
            "settings.web_media.hdr_selection.sdr_only",
            "Только SDR",
        ),
        (
            WebMediaHdrSelection::PreferHdrWhenAvailable,
            "settings.web_media.hdr_selection.prefer_hdr",
            "Предпочитать HDR",
        ),
    ]
    .into_iter()
    .map(|(selection, label_id, label_ru)| SettingOption {
        id: hdr_selection_id(selection).into(),
        label: SettingText::new(label_id, label_ru),
        description: None,
    })
    .collect()
}

/// Возвращает common web-video height choices.
fn preferred_height_options() -> Vec<SettingOption> {
    let best_playable = SettingOption {
        id: "best_playable".into(),
        label: SettingText::new(
            "settings.web_media.preferred_video_height.best_playable",
            "Лучшее доступное",
        ),
        description: None,
    };
    std::iter::once(best_playable)
        .chain(
            [144_u32, 240, 360, 480, 720, 1080, 1440, 2160, 4320]
                .into_iter()
                .map(|pixels| SettingOption {
                    id: pixels.to_string().into(),
                    label: SettingText::new(
                        format!("settings.web_media.preferred_video_height.{pixels}"),
                        format!("{pixels}p"),
                    ),
                    description: None,
                }),
        )
        .collect()
}

/// Возвращает stable TOML/settings id HDR policy.
const fn hdr_selection_id(selection: WebMediaHdrSelection) -> &'static str {
    match selection {
        WebMediaHdrSelection::SdrOnly => "sdr_only",
        WebMediaHdrSelection::PreferHdrWhenAvailable => "prefer_hdr",
    }
}

/// Извлекает known stable HDR id.
fn hdr_selection_value(value: SettingValue) -> SettingsResult<WebMediaHdrSelection> {
    let SettingValue::Select(option_id) = value else {
        return Err(SettingsError::access_failed(
            "web_media.hdr_selection ожидает select value",
        ));
    };
    match option_id.as_str() {
        "sdr_only" => Ok(WebMediaHdrSelection::SdrOnly),
        "prefer_hdr" => Ok(WebMediaHdrSelection::PreferHdrWhenAvailable),
        _ => Err(SettingsError::access_failed(
            "web_media.hdr_selection получил неизвестный option id",
        )),
    }
}

/// Преобразует UI `best_playable`/число в validated optional newtype.
fn preferred_height_value(value: SettingValue) -> SettingsResult<Option<PreferredVideoHeight>> {
    let SettingValue::Select(option_id) = value else {
        return Err(SettingsError::access_failed(
            "web_media.preferred_video_height ожидает select value",
        ));
    };
    if option_id.as_str() == "best_playable" {
        return Ok(None);
    }
    let pixels = option_id.as_str().parse::<u32>().map_err(|_| {
        SettingsError::access_failed(
            "web_media.preferred_video_height ожидает best_playable или целое число",
        )
    })?;
    PreferredVideoHeight::new(pixels)
        .map(Some)
        .map_err(|error| SettingsError::access_failed(error.to_string()))
}

/// Преобразует validated `u64` в settings integer без скрытого narrowing.
fn integer_value(number: u64, field_name: &'static str) -> SettingValue {
    SettingValue::Integer(
        i64::try_from(number)
            .unwrap_or_else(|_| panic!("validated {field_name} всегда помещается в i64")),
    )
}

/// Извлекает bool с понятной boundary-ошибкой.
fn bool_value(setting_path: &'static str, value: SettingValue) -> SettingsResult<bool> {
    match value {
        SettingValue::Bool(enabled) => Ok(enabled),
        _ => Err(SettingsError::access_failed(format!(
            "{setting_path} ожидает bool"
        ))),
    }
}

/// Извлекает неотрицательное integer значение в `u64`.
fn u64_value(setting_path: &'static str, value: SettingValue) -> SettingsResult<u64> {
    let SettingValue::Integer(number) = value else {
        return Err(SettingsError::access_failed(format!(
            "{setting_path} ожидает integer value"
        )));
    };
    u64::try_from(number).map_err(|_| {
        SettingsError::access_failed(format!("{setting_path} не может быть отрицательным"))
    })
}

#[cfg(test)]
mod tests {
    // Подключаем только внутренний accessor boundary и принадлежащие модулю типы.
    use super::*;

    #[test]
    fn accessor_rejects_invalid_values_without_mutating_owned_fields() {
        // Каждый case проверяет собственную exact failure-причину accessor-а.
        let invalid_values = [
            (
                WebMediaField::HdrSelection,
                SettingValue::Bool(true),
                "web_media.hdr_selection ожидает select value",
            ),
            (
                WebMediaField::HdrSelection,
                SettingValue::Select("unknown_hdr_policy".into()),
                "web_media.hdr_selection получил неизвестный option id",
            ),
            (
                WebMediaField::PreferredVideoHeight,
                SettingValue::Bool(true),
                "web_media.preferred_video_height ожидает select value",
            ),
            (
                WebMediaField::PreferredVideoHeight,
                SettingValue::Select("not_a_height".into()),
                "web_media.preferred_video_height ожидает best_playable или целое число",
            ),
            (
                WebMediaField::VodEndpointRecoveryEnabled,
                SettingValue::Integer(1),
                "web_media.vod_endpoint_recovery_enabled ожидает bool",
            ),
            (
                WebMediaField::VodEndpointRecoveryMaxAttempts,
                SettingValue::Bool(true),
                "web_media.vod_endpoint_recovery_max_consecutive_attempts ожидает integer value",
            ),
            (
                WebMediaField::VodEndpointRecoveryInitialBackoff,
                SettingValue::Integer(-1),
                "web_media.vod_endpoint_recovery_initial_backoff_ms не может быть отрицательным",
            ),
        ];
        // Один typed config позволяет заметить даже частичную мутацию между rejected calls.
        let mut config = WebMediaConfig::default();

        // Обходим trait implementation через его intent-методы get/set.
        for (field, invalid_value, expected_error) in invalid_values {
            // Снимок принадлежит самому owner-модулю и не раскрывает storage наружу.
            let value_before = field.get(&config);
            // Ошибочный neutral input обязан сохраниться как различимая SettingsError.
            let error = field
                .set(&mut config, invalid_value)
                .expect_err("accessor не должен принимать неверное neutral value");

            // Exact reason сохраняет различия между типом, id, parse и signedness.
            assert_eq!(
                error.to_string(),
                format!("setting access failed: {expected_error}")
            );
            // Owner field остаётся неизменным после любой rejected операции.
            assert_eq!(field.get(&config), value_before);
        }

        // Все integer projections обязаны пройти checked u64 -> i64 boundary.
        for integer_field in [
            WebMediaField::VodEndpointRecoveryMaxAttempts,
            WebMediaField::VodEndpointRecoveryInitialBackoff,
            WebMediaField::VodEndpointRecoveryMaxBackoff,
            WebMediaField::VodEndpointRecoveryStableReset,
        ] {
            // Значения config уже провалидированы и потому возвращаются как neutral Integer.
            assert!(matches!(
                integer_field.get(&config),
                SettingValue::Integer(_)
            ));
        }
    }
}
