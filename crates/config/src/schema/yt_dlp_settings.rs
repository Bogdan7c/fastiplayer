//! Settings registry для process controls секции `[yt_dlp]`.

use settings_core::{
    DefaultBehavior, NumericDescriptor, NumericRange, NumericStep, SettingAccess, SettingAccessor,
    SettingApplyMode, SettingDescriptor, SettingDescriptorText, SettingEditor, SettingPlacement,
    SettingText, SettingValue, SettingValueType, SettingsError, SettingsRegistry, SettingsResult,
    SettingsSchema,
};

use super::YtDlpConfig;

/// Поле process config-а, которым владеет один hand-written accessor.
#[derive(Debug, Clone, Copy)]
enum YtDlpField {
    Enabled,
    ResolveTimeout,
    SingleItemStdoutLimit,
    SingleItemStderrLimit,
    SingleItemJsonNodeLimit,
}

/// Adapter между neutral settings value и typed `yt-dlp` process config.
struct YtDlpAccessor {
    field: YtDlpField,
}

impl SettingsSchema for YtDlpConfig {
    /// Строит registry только для extractor/process controls.
    fn settings_registry() -> SettingsResult<SettingsRegistry<Self>> {
        let mut registry = SettingsRegistry::empty();
        register_setting(
            &mut registry,
            descriptor(
                "yt_dlp.enabled",
                "YtDlp adapter",
                "Разрешает запуск YtDlp extractor adapter-а.",
                SettingValueType::Bool,
                SettingEditor::Toggle,
            ),
            YtDlpField::Enabled,
        )?;
        register_integer_setting(
            &mut registry,
            IntegerSettingDescriptor {
                id: "yt_dlp.resolve_timeout_ms",
                label_ru: "YtDlp resolve timeout",
                description_ru: "Максимальное время работы системного yt-dlp для одного resolve.",
                maximum: crate::validation::MAX_YT_DLP_RESOLVE_TIMEOUT_MS,
                step: 100,
                unit: "ms",
            },
            YtDlpField::ResolveTimeout,
        )?;
        register_integer_setting(
            &mut registry,
            IntegerSettingDescriptor {
                id: "yt_dlp.single_item_stdout_limit_bytes",
                label_ru: "Лимит stdout YtDlp",
                description_ru: "Максимальный размер JSON stdout одного media item до немедленного завершения yt-dlp.",
                maximum: crate::validation::MAX_YT_DLP_SINGLE_ITEM_STDOUT_BYTES,
                step: 1024 * 1024,
                unit: "bytes",
            },
            YtDlpField::SingleItemStdoutLimit,
        )?;
        register_integer_setting(
            &mut registry,
            IntegerSettingDescriptor {
                id: "yt_dlp.single_item_stderr_limit_bytes",
                label_ru: "Лимит stderr YtDlp",
                description_ru: "Максимальный diagnostic stderr одного media item; содержимое stderr не сохраняется.",
                maximum: crate::validation::MAX_YT_DLP_SINGLE_ITEM_STDERR_BYTES,
                step: 1024 * 1024,
                unit: "bytes",
            },
            YtDlpField::SingleItemStderrLimit,
        )?;
        register_integer_setting(
            &mut registry,
            IntegerSettingDescriptor {
                id: "yt_dlp.single_item_json_node_limit",
                label_ru: "Лимит структуры JSON YtDlp",
                description_ru: "Максимальное число JSON values одного media item до построения metadata DOM.",
                maximum: crate::validation::MAX_YT_DLP_SINGLE_ITEM_JSON_NODES,
                step: 10_000,
                unit: "nodes",
            },
            YtDlpField::SingleItemJsonNodeLimit,
        )?;
        Ok(registry)
    }
}

impl SettingAccessor<YtDlpConfig> for YtDlpAccessor {
    fn get(&self, document: &YtDlpConfig) -> SettingsResult<SettingValue> {
        Ok(self.field.get(document))
    }

    fn set(&self, document: &mut YtDlpConfig, value: SettingValue) -> SettingsResult<()> {
        self.field.set(document, value)
    }

    fn reset(
        &self,
        document: &mut YtDlpConfig,
        default_document: &YtDlpConfig,
    ) -> SettingsResult<()> {
        self.field.reset(document, default_document);
        Ok(())
    }
}

impl YtDlpField {
    /// Проецирует process config field на neutral value.
    fn get(self, config: &YtDlpConfig) -> SettingValue {
        match self {
            Self::Enabled => SettingValue::Bool(config.enabled),
            Self::ResolveTimeout => integer_value(config.resolve_timeout_ms, "resolve timeout"),
            Self::SingleItemStdoutLimit => {
                integer_value(config.single_item_stdout_limit_bytes, "stdout limit")
            }
            Self::SingleItemStderrLimit => {
                integer_value(config.single_item_stderr_limit_bytes, "stderr limit")
            }
            Self::SingleItemJsonNodeLimit => {
                integer_value(config.single_item_json_node_limit, "JSON node limit")
            }
        }
    }

    /// Применяет neutral value к одному process config field-у.
    fn set(self, config: &mut YtDlpConfig, value: SettingValue) -> SettingsResult<()> {
        match self {
            Self::Enabled => config.enabled = bool_value("yt_dlp.enabled", value)?,
            Self::ResolveTimeout => {
                config.resolve_timeout_ms = u64_value("yt_dlp.resolve_timeout_ms", value)?;
            }
            Self::SingleItemStdoutLimit => {
                config.single_item_stdout_limit_bytes =
                    u64_value("yt_dlp.single_item_stdout_limit_bytes", value)?;
            }
            Self::SingleItemStderrLimit => {
                config.single_item_stderr_limit_bytes =
                    u64_value("yt_dlp.single_item_stderr_limit_bytes", value)?;
            }
            Self::SingleItemJsonNodeLimit => {
                config.single_item_json_node_limit =
                    u64_value("yt_dlp.single_item_json_node_limit", value)?;
            }
        }
        Ok(())
    }

    /// Восстанавливает field из default document-а.
    fn reset(self, config: &mut YtDlpConfig, default_config: &YtDlpConfig) {
        match self {
            Self::Enabled => config.enabled = default_config.enabled,
            Self::ResolveTimeout => config.resolve_timeout_ms = default_config.resolve_timeout_ms,
            Self::SingleItemStdoutLimit => {
                config.single_item_stdout_limit_bytes =
                    default_config.single_item_stdout_limit_bytes;
            }
            Self::SingleItemStderrLimit => {
                config.single_item_stderr_limit_bytes =
                    default_config.single_item_stderr_limit_bytes;
            }
            Self::SingleItemJsonNodeLimit => {
                config.single_item_json_node_limit = default_config.single_item_json_node_limit;
            }
        }
    }
}

/// Named metadata одного process integer setting-а.
struct IntegerSettingDescriptor {
    id: &'static str,
    label_ru: &'static str,
    description_ru: &'static str,
    maximum: u64,
    step: i64,
    unit: &'static str,
}

/// Регистрирует integer descriptor без дублирования process-specific metadata.
fn register_integer_setting(
    registry: &mut SettingsRegistry<YtDlpConfig>,
    spec: IntegerSettingDescriptor,
    field: YtDlpField,
) -> SettingsResult<()> {
    register_setting(
        registry,
        descriptor(
            spec.id,
            spec.label_ru,
            spec.description_ru,
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
    registry: &mut SettingsRegistry<YtDlpConfig>,
    descriptor: SettingDescriptor,
    field: YtDlpField,
) -> SettingsResult<()> {
    registry.register(descriptor, YtDlpAccessor { field })
}

/// Строит config-owned metadata оболочку process setting-а.
fn descriptor(
    id: &'static str,
    label_ru: &'static str,
    description_ru: &'static str,
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
        placement: SettingPlacement::new("yt_dlp", "process", "main-settings-window"),
        value_type,
        editor,
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: "yt_dlp".into(),
        apply_mode: SettingApplyMode::CommittedApply,
    }
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
