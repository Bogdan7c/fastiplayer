use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, Visitor},
};
use settings_core::{
    AutoFixedPositiveIntegerDescriptor, DefaultBehavior, NumericDescriptor, NumericRange,
    NumericStep, SelectDescriptor, SettingAccess, SettingAccessor, SettingApplyMode,
    SettingDescriptor, SettingDescriptorText, SettingEditor, SettingOption, SettingPlacement,
    SettingText, SettingValue, SettingValueType, SettingsRegistry, SettingsResult, SettingsSchema,
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

    /// Бюджет кадровых ресурсов hover-preview: `auto` или явное положительное число.
    pub hover_pool_frames: FrameServerBudgetConfig,

    /// Бюджет потоков hover work: `auto` или явное положительное число.
    pub hover_thread_count: FrameServerBudgetConfig,

    /// Ёмкость основного hover prepare window.
    pub hover_prepare_window_slots: u8,

    /// Ёмкость основного hover prepare window для software path.
    pub software_hover_prepare_window_slots: u8,

    /// Удержание недавних заменённых целей для быстрого возврата на hardware/general path.
    pub recent_superseded_prepare_slots: u8,

    /// Удержание недавних заменённых целей для быстрого возврата на software path.
    pub software_recent_superseded_prepare_slots: u8,

    /// UX grace после pointer leave; это не гарантия decode coverage.
    pub hover_leave_grace_ms: u16,

    /// Межстартовый throttle для network hover prepare; `0` означает отсутствие delay.
    pub network_hover_prepare_throttle_ms: u16,

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

/// Сохраняемый resource budget: автоматический resolver или явное fixed значение.
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

    /// Stable text для нейтрального Auto/Fixed-positive Settings editor-а.
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
        formatter.write_str("`auto` или целочисленный frame-server budget")
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
    /// Default: latest-only live scrub с ограничением по частоте.
    fn default() -> Self {
        Self::ThrottledLatest
    }
}

impl SettingsSchema for FrameServerConfig {
    /// Ручная registry нужна только потому, что budget имеет compound TOML форму.
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
            FrameServerField::HoverPreviewEnabled,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_budget_descriptor(
                "frame_server.hover_pool_frames",
                "frame_server.hover_pool_frames",
                "resources",
                "settings.frame_server.hover_pool_frames.label",
                "Бюджет hover-кадров",
                "Бюджет кадрового пула hover: auto или положительное число.",
                "Конфигурация не применяет минимумы provider-а, верхний лимит, clamp или runtime resolver.",
            ),
            |config| SettingValue::Text(config.hover_pool_frames.metadata_text()),
            FrameServerField::HoverPoolFrames,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_budget_descriptor(
                "frame_server.hover_thread_count",
                "frame_server.hover_thread_count",
                "resources",
                "settings.frame_server.hover_thread_count.label",
                "Потоки hover",
                "Бюджет потоков hover: auto или положительное число.",
                "Конфигурация только сохраняет TOML форму; runtime admission подключается отдельно.",
            ),
            |config| SettingValue::Text(config.hover_thread_count.metadata_text()),
            FrameServerField::HoverThreadCount,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.hover_prepare_window_slots",
                "frame_server.hover_prepare_window_slots",
                "hover",
                "settings.frame_server.hover_prepare_window_slots.label",
                "Слоты hover prepare",
                "Количество основных слотов hover prepare window.",
                1,
                validation::MAX_FRAME_SERVER_HOVER_PREPARE_WINDOW_SLOTS as i64,
                "slots",
            ),
            |config| SettingValue::Integer(i64::from(config.hover_prepare_window_slots)),
            FrameServerField::HoverPrepareWindowSlots,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.software_hover_prepare_window_slots",
                "frame_server.software_hover_prepare_window_slots",
                "hover",
                "settings.frame_server.software_hover_prepare_window_slots.label",
                "Software-слоты hover prepare",
                "Количество основных слотов hover prepare window для software path.",
                1,
                validation::MAX_FRAME_SERVER_SOFTWARE_HOVER_PREPARE_WINDOW_SLOTS as i64,
                "slots",
            ),
            |config| SettingValue::Integer(i64::from(config.software_hover_prepare_window_slots)),
            FrameServerField::SoftwareHoverPrepareWindowSlots,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.recent_superseded_prepare_slots",
                "frame_server.recent_superseded_prepare_slots",
                "hover",
                "settings.frame_server.recent_superseded_prepare_slots.label",
                "Недавние заменённые цели",
                "Удержание недавно заменённых hover prepare целей для быстрого возврата.",
                0,
                validation::MAX_FRAME_SERVER_RECENT_SUPERSEDED_PREPARE_SLOTS as i64,
                "slots",
            ),
            |config| SettingValue::Integer(i64::from(config.recent_superseded_prepare_slots)),
            FrameServerField::RecentSupersededPrepareSlots,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.software_recent_superseded_prepare_slots",
                "frame_server.software_recent_superseded_prepare_slots",
                "hover",
                "settings.frame_server.software_recent_superseded_prepare_slots.label",
                "Недавние software-цели",
                "Удержание недавно заменённых software hover prepare целей для быстрого возврата.",
                0,
                validation::MAX_FRAME_SERVER_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS as i64,
                "slots",
            ),
            |config| {
                SettingValue::Integer(i64::from(config.software_recent_superseded_prepare_slots))
            },
            FrameServerField::SoftwareRecentSupersededPrepareSlots,
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
            FrameServerField::HoverLeaveGraceMs,
        )?;
        register_frame_server_setting(
            &mut registry,
            frame_server_integer_descriptor(
                "frame_server.network_hover_prepare_throttle_ms",
                "frame_server.network_hover_prepare_throttle_ms",
                "network",
                "settings.frame_server.network_hover_prepare_throttle_ms.label",
                "Задержка network hover",
                "Межстартовый throttle для network hover prepare; 0 убирает только delay.",
                validation::MIN_FRAME_SERVER_NETWORK_HOVER_PREPARE_THROTTLE_MS as i64,
                validation::MAX_FRAME_SERVER_NETWORK_HOVER_PREPARE_THROTTLE_MS as i64,
                "ms",
            ),
            |config| SettingValue::Integer(i64::from(config.network_hover_prepare_throttle_ms)),
            FrameServerField::NetworkHoverPrepareThrottleMs,
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
    HoverPreviewEnabled,
    HoverPoolFrames,
    HoverThreadCount,
    HoverPrepareWindowSlots,
    SoftwareHoverPrepareWindowSlots,
    RecentSupersededPrepareSlots,
    SoftwareRecentSupersededPrepareSlots,
    HoverLeaveGraceMs,
    NetworkHoverPrepareThrottleMs,
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
            Self::HoverPreviewEnabled => {
                config.hover_preview_enabled = bool_from_setting_value(value)?;
            }
            Self::HoverPoolFrames => {
                config.hover_pool_frames = budget_from_setting_value(value)?;
            }
            Self::HoverThreadCount => {
                config.hover_thread_count = budget_from_setting_value(value)?;
            }
            Self::HoverPrepareWindowSlots => {
                config.hover_prepare_window_slots = u8_from_setting_value(value)?;
            }
            Self::SoftwareHoverPrepareWindowSlots => {
                config.software_hover_prepare_window_slots = u8_from_setting_value(value)?;
            }
            Self::RecentSupersededPrepareSlots => {
                config.recent_superseded_prepare_slots = u8_from_setting_value(value)?;
            }
            Self::SoftwareRecentSupersededPrepareSlots => {
                config.software_recent_superseded_prepare_slots = u8_from_setting_value(value)?;
            }
            Self::HoverLeaveGraceMs => {
                config.hover_leave_grace_ms = u16_from_setting_value(value)?;
            }
            Self::NetworkHoverPrepareThrottleMs => {
                config.network_hover_prepare_throttle_ms = u16_from_setting_value(value)?;
            }
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
            Self::HoverPreviewEnabled => {
                config.hover_preview_enabled = default_config.hover_preview_enabled;
            }
            Self::HoverPoolFrames => {
                config.hover_pool_frames = default_config.hover_pool_frames;
            }
            Self::HoverThreadCount => {
                config.hover_thread_count = default_config.hover_thread_count;
            }
            Self::HoverPrepareWindowSlots => {
                config.hover_prepare_window_slots = default_config.hover_prepare_window_slots;
            }
            Self::SoftwareHoverPrepareWindowSlots => {
                config.software_hover_prepare_window_slots =
                    default_config.software_hover_prepare_window_slots;
            }
            Self::RecentSupersededPrepareSlots => {
                config.recent_superseded_prepare_slots =
                    default_config.recent_superseded_prepare_slots;
            }
            Self::SoftwareRecentSupersededPrepareSlots => {
                config.software_recent_superseded_prepare_slots =
                    default_config.software_recent_superseded_prepare_slots;
            }
            Self::HoverLeaveGraceMs => {
                config.hover_leave_grace_ms = default_config.hover_leave_grace_ms;
            }
            Self::NetworkHoverPrepareThrottleMs => {
                config.network_hover_prepare_throttle_ms =
                    default_config.network_hover_prepare_throttle_ms;
            }
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

fn u8_from_setting_value(value: SettingValue) -> SettingsResult<u8> {
    let SettingValue::Integer(value) = value else {
        return Err(settings_core::SettingsError::access_failed(
            "frame_server u8 accessor received non-integer value",
        ));
    };

    u8::try_from(value).map_err(|_| {
        settings_core::SettingsError::access_failed(
            "frame_server u8 accessor value is out of range",
        )
    })
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

fn budget_from_setting_value(value: SettingValue) -> SettingsResult<FrameServerBudgetConfig> {
    let SettingValue::Text(value) = value else {
        return Err(settings_core::SettingsError::access_failed(
            "frame_server budget accessor received non-text value",
        ));
    };

    let trimmed_value = value.trim();
    if trimmed_value == "auto" {
        return Ok(FrameServerBudgetConfig::Auto);
    }

    let fixed_value = trimmed_value.parse::<usize>().map_err(|_| {
        settings_core::SettingsError::access_failed(
            "frame_server budget must be `auto` or a positive integer",
        )
    })?;
    if fixed_value == 0 {
        return Err(settings_core::SettingsError::access_failed(
            "frame_server budget fixed value must be positive",
        ));
    }

    Ok(FrameServerBudgetConfig::Fixed(fixed_value))
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
        SettingEditor::AutoFixedPositiveInteger(AutoFixedPositiveIntegerDescriptor::new(
            SettingText::new("settings.frame_server.budget.auto", "Авто"),
            SettingText::new("settings.frame_server.budget.fixed", "Фиксированно"),
            None,
        )),
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
