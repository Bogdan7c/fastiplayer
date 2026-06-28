use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, Visitor},
};
use settings_core::{
    DefaultBehavior, NumericDescriptor, NumericRange, NumericStep, SelectDescriptor, SettingAccess,
    SettingAccessor, SettingApplyMode, SettingDescriptor, SettingDescriptorText, SettingEditor,
    SettingOption, SettingPlacement, SettingText, SettingValue, SettingValueType, SettingsRegistry,
    SettingsResult, SettingsSchema, TextDescriptor, TextFormat,
};

use crate::validation;

/// Persisted TOML shape для `[frame_server]`.
///
/// Этот type намеренно живёт в `rustiplayer-config`: runtime/core mapping будет
/// отдельной boundary-задачей и не должен создавать dependency на `frame-server-core`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FrameServerConfig {
    /// Выключает только визуальный HoverPreview, но не hidden current-target prepare.
    pub hover_preview_enabled: bool,

    /// Frame budget для hover-preview ресурсов: `auto` или fixed positive.
    pub hover_pool_frames: FrameServerBudgetConfig,

    /// Thread budget для hover work: `auto` или fixed positive.
    pub hover_thread_count: FrameServerBudgetConfig,

    /// Primary hover prepare window capacity.
    pub hover_prepare_window_slots: u8,

    /// Software-path primary hover prepare window capacity.
    pub software_hover_prepare_window_slots: u8,

    /// Recent-superseded click-back retention для hardware/general path.
    pub recent_superseded_prepare_slots: u8,

    /// Recent-superseded click-back retention для software path.
    pub software_recent_superseded_prepare_slots: u8,

    /// UX grace после pointer leave; это не decode coverage guarantee.
    pub hover_leave_grace_ms: u16,

    /// Network hover prepare inter-start throttle; `0` означает no delay.
    pub network_hover_prepare_throttle_ms: u16,

    /// Включает live drag preview updates; click/release seek остаётся exact.
    pub live_scrub_enabled: bool,

    /// Latest-only live scrub decode launch policy.
    pub live_scrub_decode_mode: FrameServerLiveScrubDecodeModeConfig,

    /// Throttle limit для `throttled_latest` live scrub mode.
    pub live_scrub_max_hz: u16,
}

impl Default for FrameServerConfig {
    /// Возвращает persisted defaults из S12 design decision.
    fn default() -> Self {
        Self {
            hover_preview_enabled: true,
            hover_pool_frames: FrameServerBudgetConfig::Auto,
            hover_thread_count: FrameServerBudgetConfig::Auto,
            hover_prepare_window_slots: 1,
            software_hover_prepare_window_slots: 1,
            recent_superseded_prepare_slots: 1,
            software_recent_superseded_prepare_slots: 1,
            hover_leave_grace_ms: 500,
            network_hover_prepare_throttle_ms: 300,
            live_scrub_enabled: true,
            live_scrub_decode_mode: FrameServerLiveScrubDecodeModeConfig::ThrottledLatest,
            live_scrub_max_hz: 60,
        }
    }
}

/// Persisted resource budget: automatic resolver или explicit fixed value.
///
/// Значение `Fixed(0)` временно представимо, чтобы validation могла выдать
/// доменную ошибку `frame_server.*`, а не общий serde parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameServerBudgetConfig {
    /// Runtime resolver выберет budget позже.
    Auto,

    /// Пользователь зафиксировал budget; validation требует значение больше нуля.
    Fixed(usize),
}

impl FrameServerBudgetConfig {
    /// Возвращает fixed value для validation без раскрытия enum layout callsite-ам.
    #[must_use]
    pub const fn fixed_value(self) -> Option<usize> {
        match self {
            Self::Auto => None,
            Self::Fixed(value) => Some(value),
        }
    }

    /// Stable text для read-only Settings metadata.
    #[must_use]
    pub fn metadata_text(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Fixed(value) => value.to_string(),
        }
    }
}

impl Serialize for FrameServerBudgetConfig {
    /// Сохраняет public TOML форму: `auto` как строка, fixed budget как число.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Fixed(value) => serializer.serialize_u64(*value as u64),
        }
    }
}

impl<'de> Deserialize<'de> for FrameServerBudgetConfig {
    /// Принимает только строку `auto` или integer; quoted numbers не маскируются под fixed.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(FrameServerBudgetConfigVisitor)
    }
}

struct FrameServerBudgetConfigVisitor;

impl<'de> Visitor<'de> for FrameServerBudgetConfigVisitor {
    type Value = FrameServerBudgetConfig;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("`auto` or an integer frame-server budget")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value == "auto" {
            Ok(FrameServerBudgetConfig::Auto)
        } else {
            Err(E::invalid_value(de::Unexpected::Str(value), &self))
        }
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value < 0 {
            return Err(E::invalid_value(de::Unexpected::Signed(value), &self));
        }

        usize::try_from(value)
            .map(FrameServerBudgetConfig::Fixed)
            .map_err(|_| E::invalid_value(de::Unexpected::Signed(value), &self))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        usize::try_from(value)
            .map(FrameServerBudgetConfig::Fixed)
            .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(value), &self))
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
    /// S12 default: latest-only live scrub with hz throttle.
    fn default() -> Self {
        Self::ThrottledLatest
    }
}

impl SettingsSchema for FrameServerConfig {
    /// Ручная registry нужна только потому, что S12 budget имеет compound TOML форму.
    fn settings_registry() -> SettingsResult<SettingsRegistry<Self>> {
        let mut registry = SettingsRegistry::empty();

        register_frame_server_setting(
            &mut registry,
            frame_server_bool_descriptor(
                "frame_server.hover_preview_enabled",
                "frame_server.hover_preview_enabled",
                "hover",
                "settings.frame_server.hover_preview_enabled.label",
                "Превью при наведении",
                "Включает только визуальный hover preview; hidden prepare текущей цели остаётся активным.",
                "false скрывает визуальный preview, но не добавляет off-switch для predecode.",
            ),
            |config| SettingValue::Bool(config.hover_preview_enabled),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_budget_descriptor(
                "frame_server.hover_pool_frames",
                "frame_server.hover_pool_frames",
                "resources",
                "settings.frame_server.hover_pool_frames.label",
                "Бюджет hover-кадров",
                "Frame pool budget: auto или положительное число.",
                "S12 не применяет provider minimums, upper cap, clamp или runtime resolver.",
            ),
            |config| SettingValue::Text(config.hover_pool_frames.metadata_text()),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_budget_descriptor(
                "frame_server.hover_thread_count",
                "frame_server.hover_thread_count",
                "resources",
                "settings.frame_server.hover_thread_count.label",
                "Потоки hover",
                "Thread budget: auto или положительное число.",
                "S12 только сохраняет TOML форму; runtime admission появится отдельно.",
            ),
            |config| SettingValue::Text(config.hover_thread_count.metadata_text()),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.hover_prepare_window_slots",
                "frame_server.hover_prepare_window_slots",
                "hover",
                "settings.frame_server.hover_prepare_window_slots.label",
                "Слоты hover prepare",
                "Primary hover prepare window capacity.",
                1,
                validation::MAX_FRAME_SERVER_HOVER_PREPARE_WINDOW_SLOTS as i64,
                "slots",
            ),
            |config| SettingValue::Integer(i64::from(config.hover_prepare_window_slots)),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.software_hover_prepare_window_slots",
                "frame_server.software_hover_prepare_window_slots",
                "hover",
                "settings.frame_server.software_hover_prepare_window_slots.label",
                "Software-слоты hover prepare",
                "Software-path primary hover prepare window capacity.",
                1,
                validation::MAX_FRAME_SERVER_SOFTWARE_HOVER_PREPARE_WINDOW_SLOTS as i64,
                "slots",
            ),
            |config| SettingValue::Integer(i64::from(config.software_hover_prepare_window_slots)),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.recent_superseded_prepare_slots",
                "frame_server.recent_superseded_prepare_slots",
                "hover",
                "settings.frame_server.recent_superseded_prepare_slots.label",
                "Недавние заменённые цели",
                "Click-back retention для recently superseded hover prepare.",
                0,
                validation::MAX_FRAME_SERVER_RECENT_SUPERSEDED_PREPARE_SLOTS as i64,
                "slots",
            ),
            |config| SettingValue::Integer(i64::from(config.recent_superseded_prepare_slots)),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.software_recent_superseded_prepare_slots",
                "frame_server.software_recent_superseded_prepare_slots",
                "hover",
                "settings.frame_server.software_recent_superseded_prepare_slots.label",
                "Недавние software-цели",
                "Software-path click-back retention для recently superseded hover prepare.",
                0,
                validation::MAX_FRAME_SERVER_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS as i64,
                "slots",
            ),
            |config| {
                SettingValue::Integer(i64::from(config.software_recent_superseded_prepare_slots))
            },
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.hover_leave_grace_ms",
                "frame_server.hover_leave_grace_ms",
                "hover",
                "settings.frame_server.hover_leave_grace_ms.label",
                "Задержка ухода hover",
                "UX grace после ухода pointer с timeline; это не decode coverage.",
                validation::MIN_FRAME_SERVER_HOVER_LEAVE_GRACE_MS as i64,
                validation::MAX_FRAME_SERVER_HOVER_LEAVE_GRACE_MS as i64,
                "ms",
            ),
            |config| SettingValue::Integer(i64::from(config.hover_leave_grace_ms)),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.network_hover_prepare_throttle_ms",
                "frame_server.network_hover_prepare_throttle_ms",
                "network",
                "settings.frame_server.network_hover_prepare_throttle_ms.label",
                "Задержка network hover",
                "Inter-start throttle для network hover prepare; 0 означает no delay.",
                validation::MIN_FRAME_SERVER_NETWORK_HOVER_PREPARE_THROTTLE_MS as i64,
                validation::MAX_FRAME_SERVER_NETWORK_HOVER_PREPARE_THROTTLE_MS as i64,
                "ms",
            ),
            |config| SettingValue::Integer(i64::from(config.network_hover_prepare_throttle_ms)),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_bool_descriptor(
                "frame_server.live_scrub_enabled",
                "frame_server.live_scrub_enabled",
                "live_scrub",
                "settings.frame_server.live_scrub_enabled.label",
                "Живой scrub",
                "Включает live drag preview updates; click/release exact seek остаётся активным.",
                "false отключает live drag/main-preview updates, но не legacy seek route.",
            ),
            |config| SettingValue::Bool(config.live_scrub_enabled),
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_live_mode_descriptor(),
            |config| SettingValue::Select(config.live_scrub_decode_mode.stable_id().into()),
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
        )?;

        Ok(registry)
    }
}

struct FrameServerReadOnlyAccessor {
    get_value: fn(&FrameServerConfig) -> SettingValue,
}

impl SettingAccessor<FrameServerConfig> for FrameServerReadOnlyAccessor {
    fn get(&self, document: &FrameServerConfig) -> SettingsResult<SettingValue> {
        Ok((self.get_value)(document))
    }

    fn set(&self, _document: &mut FrameServerConfig, _value: SettingValue) -> SettingsResult<()> {
        Err(settings_core::SettingsError::access_failed(
            "frame_server settings are metadata-only in S12",
        ))
    }

    fn reset(
        &self,
        _document: &mut FrameServerConfig,
        _default_document: &FrameServerConfig,
    ) -> SettingsResult<()> {
        Err(settings_core::SettingsError::access_failed(
            "frame_server settings are metadata-only in S12",
        ))
    }
}

fn register_frame_server_setting(
    registry: &mut SettingsRegistry<FrameServerConfig>,
    descriptor: SettingDescriptor,
    get_value: fn(&FrameServerConfig) -> SettingValue,
) -> SettingsResult<()> {
    registry.register(descriptor, FrameServerReadOnlyAccessor { get_value })
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

fn frame_server_budget_descriptor(
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
        SettingValueType::Text,
        SettingEditor::Text(TextDescriptor::new(TextFormat::SingleLine)),
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
        "Поле read-only в Settings UI до отдельного runtime Frame Server wiring.",
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
        "Latest-only policy запуска decode-work во время live scrub.",
        "throttled_latest ограничивается live_scrub_max_hz; every_drag_event пробует каждый drag target.",
        SettingValueType::Select,
        SettingEditor::Select(SelectDescriptor::Static {
            options: vec![
                SettingOption::new(
                    "throttled_latest",
                    SettingText::new(
                        "settings.frame_server.live_scrub_decode_mode.throttled_latest",
                        "Latest с throttle",
                    ),
                ),
                SettingOption::new(
                    "every_drag_event",
                    SettingText::new(
                        "settings.frame_server.live_scrub_decode_mode.every_drag_event",
                        "Каждый drag event",
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
        placement: SettingPlacement::new("frame_server", group, "main-settings-window"),
        value_type,
        editor,
        access: SettingAccess::ReadOnly,
        default_behavior: DefaultBehavior::NoReset,
        route: "frame_server.apply".into(),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}
