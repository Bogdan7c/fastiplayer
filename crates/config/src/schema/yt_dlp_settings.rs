//! Settings registry для persisted `[yt_dlp]` policy.

use settings_core::{
    DefaultBehavior, NumericDescriptor, NumericRange, NumericStep, SelectDescriptor, SettingAccess,
    SettingAccessor, SettingApplyMode, SettingDescriptor, SettingDescriptorText, SettingEditor,
    SettingOption, SettingPlacement, SettingText, SettingValue, SettingValueType, SettingsError,
    SettingsRegistry, SettingsResult, SettingsSchema,
};

use super::{PreferredVideoHeight, YtDlpConfig, YtDlpHdrSelection};

/// Поле config-а, которым владеет один hand-written accessor.
#[derive(Debug, Clone, Copy)]
enum YtDlpField {
    Enabled,
    HdrSelection,
    PreferredVideoHeight,
    ResolveTimeout,
}

/// Adapter между neutral settings value и typed YtDlp config.
struct YtDlpAccessor {
    field: YtDlpField,
}

impl SettingsSchema for YtDlpConfig {
    /// Строит registry без nullable/newtype special case внутри UI.
    fn settings_registry() -> SettingsResult<SettingsRegistry<Self>> {
        let mut registry = SettingsRegistry::empty();
        register_setting(
            &mut registry,
            descriptor(
                "yt_dlp.enabled",
                "YtDlp adapter",
                "Разрешает YtDlp service adapter.",
                SettingValueType::Bool,
                SettingEditor::Toggle,
            ),
            YtDlpField::Enabled,
        )?;
        register_setting(
            &mut registry,
            descriptor(
                "yt_dlp.hdr_selection",
                "Динамический диапазон YtDlp",
                "Выбирать только SDR или предпочитать HDR при полной поддержке decoder и renderer с автоматическим SDR fallback.",
                SettingValueType::Select,
                SettingEditor::Select(SelectDescriptor::Static {
                    options: hdr_selection_options(),
                }),
            ),
            YtDlpField::HdrSelection,
        )?;
        register_setting(
            &mut registry,
            descriptor(
                "yt_dlp.preferred_video_height",
                "Предпочитаемая высота видео",
                "Лучшее доступное качество либо глобальная высота с fallback: точная, ближайшая ниже, затем ближайшая выше.",
                SettingValueType::Select,
                SettingEditor::Select(SelectDescriptor::Static {
                    options: preferred_height_options(),
                }),
            ),
            YtDlpField::PreferredVideoHeight,
        )?;
        register_setting(
            &mut registry,
            descriptor(
                "yt_dlp.resolve_timeout_ms",
                "YtDlp resolve timeout",
                "Максимальное время подготовки direct stream metadata через yt-dlp.",
                SettingValueType::Integer,
                SettingEditor::Numeric(NumericDescriptor::new(
                    NumericRange::Integer {
                        min: 1,
                        max: crate::validation::MAX_YT_DLP_RESOLVE_TIMEOUT_MS as i64,
                    },
                    NumericStep::Integer(100),
                    Some("ms".into()),
                )),
            ),
            YtDlpField::ResolveTimeout,
        )?;
        Ok(registry)
    }
}

impl SettingAccessor<YtDlpConfig> for YtDlpAccessor {
    /// Читает typed config как neutral setting value.
    fn get(&self, document: &YtDlpConfig) -> SettingsResult<SettingValue> {
        Ok(self.field.get(document))
    }

    /// Проверяет neutral value до изменения typed config-а.
    fn set(&self, document: &mut YtDlpConfig, value: SettingValue) -> SettingsResult<()> {
        self.field.set(document, value)
    }

    /// Сбрасывает только принадлежащее accessor-у поле.
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
    /// Проецирует config field на neutral value без потери `None` semantics.
    fn get(self, config: &YtDlpConfig) -> SettingValue {
        match self {
            Self::Enabled => SettingValue::Bool(config.enabled),
            Self::HdrSelection => {
                SettingValue::Select(hdr_selection_id(config.hdr_selection).into())
            }
            Self::PreferredVideoHeight => {
                SettingValue::Select(config.preferred_video_height.map_or_else(
                    || "best_playable".into(),
                    |height| height.pixels().to_string().into(),
                ))
            }
            Self::ResolveTimeout => SettingValue::Integer(
                i64::try_from(config.resolve_timeout_ms)
                    .expect("validated resolve timeout всегда помещается в i64"),
            ),
        }
    }

    /// Применяет neutral value к одному config field-у.
    fn set(self, config: &mut YtDlpConfig, value: SettingValue) -> SettingsResult<()> {
        match self {
            Self::Enabled => config.enabled = bool_value(value)?,
            Self::HdrSelection => config.hdr_selection = hdr_selection_value(value)?,
            Self::PreferredVideoHeight => {
                config.preferred_video_height = preferred_height_value(value)?;
            }
            Self::ResolveTimeout => config.resolve_timeout_ms = u64_value(value)?,
        }
        Ok(())
    }

    /// Восстанавливает field из default document-а.
    fn reset(self, config: &mut YtDlpConfig, default_config: &YtDlpConfig) {
        match self {
            Self::Enabled => config.enabled = default_config.enabled,
            Self::HdrSelection => config.hdr_selection = default_config.hdr_selection,
            Self::PreferredVideoHeight => {
                config.preferred_video_height = default_config.preferred_video_height;
            }
            Self::ResolveTimeout => config.resolve_timeout_ms = default_config.resolve_timeout_ms,
        }
    }
}

/// Регистрирует descriptor и его единственный typed accessor.
fn register_setting(
    registry: &mut SettingsRegistry<YtDlpConfig>,
    descriptor: SettingDescriptor,
    field: YtDlpField,
) -> SettingsResult<()> {
    registry.register(descriptor, YtDlpAccessor { field })
}

/// Строит общую config-owned metadata оболочку YtDlp setting-а.
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
        placement: SettingPlacement::new("yt_dlp", "service", "main-settings-window"),
        value_type,
        editor,
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: "yt_dlp".into(),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}

/// Возвращает stable HDR options без Debug-based идентификаторов.
fn hdr_selection_options() -> Vec<SettingOption> {
    [
        (
            YtDlpHdrSelection::SdrOnly,
            "settings.yt_dlp.hdr_selection.sdr_only",
            "Только SDR",
        ),
        (
            YtDlpHdrSelection::PreferHdrWhenAvailable,
            "settings.yt_dlp.hdr_selection.prefer_hdr",
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

/// Возвращает common quality choices; custom TOML value UI сохраняет как unavailable current.
fn preferred_height_options() -> Vec<SettingOption> {
    let best_playable = SettingOption {
        id: "best_playable".into(),
        label: SettingText::new(
            "settings.yt_dlp.preferred_video_height.best_playable",
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
                        format!("settings.yt_dlp.preferred_video_height.{pixels}"),
                        format!("{pixels}p"),
                    ),
                    description: None,
                }),
        )
        .collect()
}

/// Возвращает stable TOML/settings id HDR policy.
const fn hdr_selection_id(selection: YtDlpHdrSelection) -> &'static str {
    match selection {
        YtDlpHdrSelection::SdrOnly => "sdr_only",
        YtDlpHdrSelection::PreferHdrWhenAvailable => "prefer_hdr",
    }
}

/// Извлекает bool с понятной boundary-ошибкой.
fn bool_value(value: SettingValue) -> SettingsResult<bool> {
    match value {
        SettingValue::Bool(enabled) => Ok(enabled),
        _ => Err(SettingsError::access_failed("yt_dlp.enabled ожидает bool")),
    }
}

/// Извлекает известный stable HDR id.
fn hdr_selection_value(value: SettingValue) -> SettingsResult<YtDlpHdrSelection> {
    let SettingValue::Select(option_id) = value else {
        return Err(SettingsError::access_failed(
            "yt_dlp.hdr_selection ожидает select value",
        ));
    };
    match option_id.as_str() {
        "sdr_only" => Ok(YtDlpHdrSelection::SdrOnly),
        "prefer_hdr" => Ok(YtDlpHdrSelection::PreferHdrWhenAvailable),
        _ => Err(SettingsError::access_failed(
            "yt_dlp.hdr_selection получил неизвестный option id",
        )),
    }
}

/// Преобразует UI `best_playable`/число в validated optional newtype.
fn preferred_height_value(value: SettingValue) -> SettingsResult<Option<PreferredVideoHeight>> {
    let SettingValue::Select(option_id) = value else {
        return Err(SettingsError::access_failed(
            "yt_dlp.preferred_video_height ожидает select value",
        ));
    };
    if option_id.as_str() == "best_playable" {
        return Ok(None);
    }
    let pixels = option_id.as_str().parse::<u32>().map_err(|_| {
        SettingsError::access_failed(
            "yt_dlp.preferred_video_height ожидает best_playable или целое число",
        )
    })?;
    PreferredVideoHeight::new(pixels)
        .map(Some)
        .map_err(|error| SettingsError::access_failed(error.to_string()))
}

/// Извлекает неотрицательное integer значение в `u64`.
fn u64_value(value: SettingValue) -> SettingsResult<u64> {
    let SettingValue::Integer(number) = value else {
        return Err(SettingsError::access_failed(
            "yt_dlp.resolve_timeout_ms ожидает integer value",
        ));
    };
    u64::try_from(number).map_err(|_| {
        SettingsError::access_failed("yt_dlp.resolve_timeout_ms не может быть отрицательным")
    })
}
