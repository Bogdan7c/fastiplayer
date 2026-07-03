use serde::{Deserialize, Serialize};
use settings_core::{
    DefaultBehavior, NumericDescriptor, NumericRange, NumericStep, SelectDescriptor, SettingAccess,
    SettingAccessor, SettingApplyMode, SettingDescriptor, SettingDescriptorText, SettingEditor,
    SettingOption, SettingPlacement, SettingText, SettingValue, SettingValueType, SettingsRegistry,
    SettingsResult, SettingsSchema,
};

use crate::validation;

/// Persisted TOML shape для `[frame_server]`.
///
/// Этот type намеренно живёт в `rustiplayer-config`: runtime/core mapping будет
/// отдельной boundary-задачей и не должен создавать dependency на `frame-server-core`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FrameServerConfig {
    /// Включает live drag preview updates; click/release seek остаётся точным.
    pub live_scrub_enabled: bool,

    /// Latest-only политика запуска decode-work для live scrub.
    pub live_scrub_decode_mode: FrameServerLiveScrubDecodeModeConfig,

    /// Лимит throttle для live scrub режима `throttled_latest`.
    pub live_scrub_max_hz: u16,
}

impl Default for FrameServerConfig {
    /// Возвращает persisted defaults для V1 frame-server config.
    fn default() -> Self {
        Self {
            live_scrub_enabled: true,
            live_scrub_decode_mode: FrameServerLiveScrubDecodeModeConfig::ThrottledLatest,
            live_scrub_max_hz: 60,
        }
    }
}

/// Persisted live-scrub decode mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameServerLiveScrubDecodeModeConfig {
    /// Decode work стартует не чаще `live_scrub_max_hz`, сохраняя latest target.
    ThrottledLatest,

    /// Каждый drag event допускается к старту, но stale target отменяется.
    EveryDragEvent,
}

impl FrameServerLiveScrubDecodeModeConfig {
    /// Stable metadata/TOML id без зависимости от Debug output.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::ThrottledLatest => "throttled_latest",
            Self::EveryDragEvent => "every_drag_event",
        }
    }
}

impl Default for FrameServerLiveScrubDecodeModeConfig {
    /// Default: latest-only live scrub с ограничением по частоте.
    fn default() -> Self {
        Self::ThrottledLatest
    }
}

impl SettingsSchema for FrameServerConfig {
    /// Ручная registry оставляет frame-server настройки в отдельной settings-секции.
    fn settings_registry() -> SettingsResult<SettingsRegistry<Self>> {
        let mut registry = SettingsRegistry::empty();

        register_frame_server_setting(
            &mut registry,
            frame_server_bool_descriptor(
                "frame_server.live_scrub_enabled",
                "frame_server.live_scrub_enabled",
                "live_scrub",
                "settings.frame_server.live_scrub_enabled.label",
                "Живой scrub",
                "Включает live drag preview updates; click/release exact seek остаётся активным.",
                "false отключает live drag/main-preview updates, но не точный seek по click/release.",
            ),
            |config| SettingValue::Bool(config.live_scrub_enabled),
            FrameServerField::LiveScrubEnabled,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_live_mode_descriptor(),
            |config| SettingValue::Select(config.live_scrub_decode_mode.stable_id().into()),
            FrameServerField::LiveScrubDecodeMode,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.live_scrub_max_hz",
                "frame_server.live_scrub_max_hz",
                "live_scrub",
                "settings.frame_server.live_scrub_max_hz.label",
                "Макс. частота live scrub",
                "Частота запуска decode-work в throttled_latest mode.",
                validation::MIN_FRAME_SERVER_LIVE_SCRUB_MAX_HZ as i64,
                validation::MAX_FRAME_SERVER_LIVE_SCRUB_MAX_HZ as i64,
                "hz",
            ),
            |config| SettingValue::Integer(i64::from(config.live_scrub_max_hz)),
            FrameServerField::LiveScrubMaxHz,
        )?;

        Ok(registry)
    }
}

#[derive(Debug, Clone, Copy)]
enum FrameServerField {
    LiveScrubEnabled,
    LiveScrubDecodeMode,
    LiveScrubMaxHz,
}

struct FrameServerAccessor {
    get_value: fn(&FrameServerConfig) -> SettingValue,
    field: FrameServerField,
}

impl SettingAccessor<FrameServerConfig> for FrameServerAccessor {
    fn get(&self, document: &FrameServerConfig) -> SettingsResult<SettingValue> {
        Ok((self.get_value)(document))
    }

    fn set(&self, document: &mut FrameServerConfig, value: SettingValue) -> SettingsResult<()> {
        self.field.set(document, value)
    }

    fn reset(
        &self,
        document: &mut FrameServerConfig,
        default_document: &FrameServerConfig,
    ) -> SettingsResult<()> {
        self.field.reset(document, default_document);
        Ok(())
    }
}

impl FrameServerField {
    fn set(self, config: &mut FrameServerConfig, value: SettingValue) -> SettingsResult<()> {
        match self {
            Self::LiveScrubEnabled => {
                config.live_scrub_enabled = bool_from_setting_value(value)?;
            }
            Self::LiveScrubDecodeMode => {
                config.live_scrub_decode_mode = live_mode_from_setting_value(value)?;
            }
            Self::LiveScrubMaxHz => {
                config.live_scrub_max_hz = u16_from_setting_value(value)?;
            }
        }

        Ok(())
    }

    fn reset(self, config: &mut FrameServerConfig, default_config: &FrameServerConfig) {
        match self {
            Self::LiveScrubEnabled => {
                config.live_scrub_enabled = default_config.live_scrub_enabled;
            }
            Self::LiveScrubDecodeMode => {
                config.live_scrub_decode_mode = default_config.live_scrub_decode_mode;
            }
            Self::LiveScrubMaxHz => {
                config.live_scrub_max_hz = default_config.live_scrub_max_hz;
            }
        }
    }
}

fn bool_from_setting_value(value: SettingValue) -> SettingsResult<bool> {
    let SettingValue::Bool(value) = value else {
        return Err(settings_core::SettingsError::access_failed(
            "frame_server bool accessor received non-bool value",
        ));
    };

    Ok(value)
}

fn u16_from_setting_value(value: SettingValue) -> SettingsResult<u16> {
    let SettingValue::Integer(value) = value else {
        return Err(settings_core::SettingsError::access_failed(
            "frame_server u16 accessor received non-integer value",
        ));
    };

    u16::try_from(value).map_err(|_| {
        settings_core::SettingsError::access_failed(
            "frame_server u16 accessor value is out of range",
        )
    })
}

fn live_mode_from_setting_value(
    value: SettingValue,
) -> SettingsResult<FrameServerLiveScrubDecodeModeConfig> {
    let SettingValue::Select(value) = value else {
        return Err(settings_core::SettingsError::access_failed(
            "frame_server live scrub mode accessor received non-select value",
        ));
    };

    match value.as_str() {
        "throttled_latest" => Ok(FrameServerLiveScrubDecodeModeConfig::ThrottledLatest),
        "every_drag_event" => Ok(FrameServerLiveScrubDecodeModeConfig::EveryDragEvent),
        other => Err(settings_core::SettingsError::access_failed(format!(
            "unknown frame_server live scrub decode mode `{other}`"
        ))),
    }
}

fn register_frame_server_setting(
    registry: &mut SettingsRegistry<FrameServerConfig>,
    descriptor: SettingDescriptor,
    get_value: fn(&FrameServerConfig) -> SettingValue,
    field: FrameServerField,
) -> SettingsResult<()> {
    registry.register(descriptor, FrameServerAccessor { get_value, field })
}

fn frame_server_bool_descriptor(
    id: &'static str,
    path: &'static str,
    group: &'static str,
    label_id: &'static str,
    label_ru: &'static str,
    description_ru: &'static str,
    help_ru: &'static str,
) -> SettingDescriptor {
    frame_server_descriptor(
        id,
        path,
        group,
        label_id,
        label_ru,
        description_ru,
        help_ru,
        SettingValueType::Bool,
        SettingEditor::Toggle,
    )
}

#[allow(clippy::too_many_arguments)]
fn frame_server_integer_descriptor(
    id: &'static str,
    path: &'static str,
    group: &'static str,
    label_id: &'static str,
    label_ru: &'static str,
    description_ru: &'static str,
    min: i64,
    max: i64,
    unit: &'static str,
) -> SettingDescriptor {
    frame_server_descriptor(
        id,
        path,
        group,
        label_id,
        label_ru,
        description_ru,
        "Изменение сохраняется через Settings Apply; live controller wiring подключается отдельно.",
        SettingValueType::Integer,
        SettingEditor::Numeric(NumericDescriptor::new(
            NumericRange::Integer { min, max },
            NumericStep::Integer(1),
            Some(unit.into()),
        )),
    )
}

fn frame_server_live_mode_descriptor() -> SettingDescriptor {
    frame_server_descriptor(
        "frame_server.live_scrub_decode_mode",
        "frame_server.live_scrub_decode_mode",
        "live_scrub",
        "settings.frame_server.live_scrub_decode_mode.label",
        "Режим live scrub decode",
        "Latest-only политика запуска decode-work во время live scrub.",
        "throttled_latest ограничивается live_scrub_max_hz; every_drag_event пробует каждую drag-цель.",
        SettingValueType::Select,
        SettingEditor::Select(SelectDescriptor::Static {
            options: vec![
                SettingOption::new(
                    "throttled_latest",
                    SettingText::new(
                        "settings.frame_server.live_scrub_decode_mode.throttled_latest",
                        "Последняя цель с ограничением частоты",
                    ),
                ),
                SettingOption::new(
                    "every_drag_event",
                    SettingText::new(
                        "settings.frame_server.live_scrub_decode_mode.every_drag_event",
                        "Каждое drag-событие",
                    ),
                ),
            ],
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn frame_server_descriptor(
    id: &'static str,
    path: &'static str,
    group: &'static str,
    label_id: &'static str,
    label_ru: &'static str,
    description_ru: &'static str,
    help_ru: &'static str,
    value_type: SettingValueType,
    editor: SettingEditor,
) -> SettingDescriptor {
    SettingDescriptor {
        id: id.into(),
        path: path.into(),
        text: SettingDescriptorText::new(SettingText::new(label_id, label_ru))
            .with_description(SettingText::new(
                format!("{label_id}.description"),
                description_ru,
            ))
            .with_help(SettingText::new(format!("{label_id}.help"), help_ru)),
        placement: SettingPlacement::new("frame_server", group, "main-settings-window")
            .with_group_default_open(false),
        value_type,
        editor,
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: "frame_server.apply".into(),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}
