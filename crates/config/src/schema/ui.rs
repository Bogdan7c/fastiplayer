use serde::{Deserialize, Serialize};

/// Ширина sidebar по умолчанию в логических egui points.
pub const DEFAULT_SIDEBAR_WIDTH_POINTS: u16 = 420;

/// Минимальная ширина sidebar, при которой контент остаётся читаемым.
pub const MIN_SIDEBAR_WIDTH_POINTS: u16 = 350;

/// Максимальная ширина sidebar, чтобы панель не вытесняла основную часть видео.
pub const MAX_SIDEBAR_WIDTH_POINTS: u16 = 600;

/// Минимальный радиус контура окна: ноль полностью отключает скругление.
pub const MIN_WINDOW_CORNER_RADIUS_PX: u16 = 0;

/// Максимальный радиус сохраняет угловые controls в непрозрачной области.
pub const MAX_WINDOW_CORNER_RADIUS_PX: u16 = 24;

/// Настройки UI shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Показывать диагностическую панель telemetry.
    #[setting(
        id = "ui.show_telemetry",
        path = "ui.show_telemetry",
        section = "ui",
        group = "shell",
        surface = "main-settings-window",
        label_id = "settings.ui.show_telemetry.label",
        label_ru = "Показывать telemetry",
        description_id = "settings.ui.show_telemetry.description",
        description_ru = "Показывать диагностическую панель telemetry.",
        editor = "toggle",
        apply = "ui.apply"
    )]
    pub show_telemetry: bool,

    /// Язык UI.
    #[setting(
        id = "ui.language",
        path = "ui.language",
        section = "ui",
        group = "shell",
        surface = "main-settings-window",
        label_id = "settings.ui.language.label",
        label_ru = "Язык UI",
        description_id = "settings.ui.language.description",
        description_ru = "Короткий код языка UI, например `ru` или `en`.",
        editor = "text",
        min_len = crate::validation::MIN_UI_LANGUAGE_LEN,
        max_len = crate::validation::MAX_UI_LANGUAGE_LEN,
        apply = "ui.apply"
    )]
    pub language: String,

    /// Идентификатор skin-а UI.
    #[setting(
        id = "ui.skin",
        path = "ui.skin",
        section = "ui",
        group = "shell",
        surface = "main-settings-window",
        label_id = "settings.ui.skin.label",
        label_ru = "Skin UI",
        description_id = "settings.ui.skin.description",
        description_ru = "Stable id UI skin; неизвестный id является config error.",
        editor = "select",
        apply = "ui.apply",
        options(option(
            id = "minimal",
            label_id = "settings.ui.skin.minimal",
            label_ru = "Minimal"
        ),)
    )]
    pub skin: String,

    /// Настройки кастомного заголовка окна.
    #[serde(default)]
    #[setting(nested)]
    pub window: UiWindowConfig,

    /// Геометрия общего sidebar host для Playlist/Settings/URL/Info.
    #[serde(default)]
    #[setting(nested)]
    pub sidebar: UiSidebarConfig,

    /// Настройки будущего Settings UI.
    #[serde(default)]
    #[setting(nested)]
    pub settings: UiSettingsConfig,

    /// Настройки UI-анимаций.
    #[serde(default)]
    #[setting(nested)]
    pub animations: UiAnimationsConfig,
}

impl Default for UiConfig {
    /// Возвращает русскоязычный UI по умолчанию.
    fn default() -> Self {
        Self {
            show_telemetry: true,
            language: "ru".to_string(),
            skin: "minimal".to_string(),
            window: UiWindowConfig::default(),
            sidebar: UiSidebarConfig::default(),
            settings: UiSettingsConfig::default(),
            animations: UiAnimationsConfig::default(),
        }
    }
}

/// Настройки кастомного window chrome; применяются после Apply/OK.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiWindowConfig {
    /// Высота titlebar в логических UI pixels/egui points.
    #[setting(
        id = "ui.window.titlebar_height_px",
        path = "ui.window.titlebar_height_px",
        section = "ui",
        group = "window",
        surface = "main-settings-window",
        label_id = "settings.ui.window.titlebar_height_px.label",
        label_ru = "Высота заголовка окна",
        description_id = "settings.ui.window.titlebar_height_px.description",
        description_ru = "Высота кастомного titlebar Rustiplayer в логических UI pixels.",
        help_id = "settings.ui.window.titlebar_height_px.help",
        help_ru = "Панель остаётся overlay поверх видео: viewport не сжимается и exclusion rect не добавляется.",
        editor = "integer",
        min = crate::validation::MIN_TITLEBAR_HEIGHT_PX,
        max = crate::validation::MAX_TITLEBAR_HEIGHT_PX,
        step = 1,
        unit = "px",
        apply = "ui.apply"
    )]
    pub titlebar_height_px: u16,

    /// Радиус прозрачного контура окна в логических UI pixels/egui points.
    #[setting(
        id = "ui.window.corner_radius_px",
        path = "ui.window.corner_radius_px",
        section = "ui",
        group = "window",
        surface = "main-settings-window",
        label_id = "settings.ui.window.corner_radius_px.label",
        label_ru = "Скругление окна",
        description_id = "settings.ui.window.corner_radius_px.description",
        description_ru = "Радиус прозрачного скругления всего окна в логических UI pixels.",
        help_id = "settings.ui.window.corner_radius_px.help",
        help_ru = "0 отключает скругление. В maximized/fullscreen контур всегда квадратный; если compositor не поддерживает прозрачность, используется безопасный квадратный fallback.",
        editor = "integer",
        min = MIN_WINDOW_CORNER_RADIUS_PX,
        max = MAX_WINDOW_CORNER_RADIUS_PX,
        step = 1,
        unit = "px",
        apply = "ui.apply"
    )]
    pub corner_radius_px: u16,
}

impl Default for UiWindowConfig {
    /// Возвращает высоту titlebar, близкую к обычным desktop chrome controls.
    fn default() -> Self {
        Self {
            titlebar_height_px: 40,
            corner_radius_px: 12,
        }
    }
}

/// Настройки геометрии единственного переиспользуемого sidebar host.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiSidebarConfig {
    /// Полностью открытая ширина общей панели в логических egui points.
    #[setting(
        id = "ui.sidebar.width_points",
        path = "ui.sidebar.width_points",
        section = "ui",
        group = "sidebar",
        surface = "main-settings-window",
        label_id = "settings.ui.sidebar.width_points.label",
        label_ru = "Ширина сайдбара",
        description_id = "settings.ui.sidebar.width_points.description",
        description_ru = "Общая ширина Playlist, Settings, URL и Info в логических UI-пунктах.",
        help_id = "settings.ui.sidebar.width_points.help",
        help_ru = "Ширину также можно менять мышью за правую границу открытой панели.",
        editor = "integer",
        min = crate::MIN_SIDEBAR_WIDTH_POINTS,
        max = crate::MAX_SIDEBAR_WIDTH_POINTS,
        step = 1,
        unit = "pt",
        apply = "ui.apply"
    )]
    pub width_points: u16,
}

impl Default for UiSidebarConfig {
    /// Возвращает ширину, достаточную для Settings и Playlist без чрезмерного сжатия видео.
    fn default() -> Self {
        Self {
            width_points: DEFAULT_SIDEBAR_WIDTH_POINTS,
        }
    }
}

/// Настройки поведения Settings UI, которые применяются только после Apply/OK.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettingsConfig {
    /// Верхняя граница частоты live preview updates, чтобы slider не перегружал runtime.
    #[setting(
        id = "ui.settings.live_preview_max_hz",
        path = "ui.settings.live_preview_max_hz",
        section = "ui",
        group = "settings_runtime",
        surface = "main-settings-window",
        label_id = "settings.ui.settings.live_preview_max_hz.label",
        label_ru = "Частота live preview",
        description_id = "settings.ui.settings.live_preview_max_hz.description",
        description_ru = "Максимальная частота live preview updates после Apply.",
        help_id = "settings.ui.settings.live_preview_max_hz.help",
        help_ru = "Это committed setting: открытая draft/preview transaction продолжает использовать последнее применённое значение.",
        editor = "integer",
        min = crate::validation::MIN_LIVE_PREVIEW_MAX_HZ,
        max = crate::validation::MAX_LIVE_PREVIEW_MAX_HZ,
        step = 1,
        unit = "hz",
        apply = "ui.apply"
    )]
    pub live_preview_max_hz: u16,
}

impl Default for UiSettingsConfig {
    /// Возвращает плавный default без привязки к конкретному refresh rate монитора.
    fn default() -> Self {
        Self {
            live_preview_max_hz: 60,
        }
    }
}

/// Настройки UI-анимаций; применяются после Apply/OK, идущую анимацию не перезапускают.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiAnimationsConfig {
    /// Отключает пространственные анимации и масштабирование для комфортного UI.
    #[setting(
        id = "ui.animations.reduced_motion",
        path = "ui.animations.reduced_motion",
        section = "ui",
        group = "animations",
        surface = "main-settings-window",
        label_id = "settings.ui.animations.reduced_motion.label",
        label_ru = "Уменьшить движение",
        description_id = "settings.ui.animations.reduced_motion.description",
        description_ru = "Показывать сайдбар и индикатор скорости без движения, а кнопки — без масштабирования.",
        help_id = "settings.ui.animations.reduced_motion.help",
        help_ru = "Короткие переходы цвета сохраняются, чтобы состояние кнопок оставалось понятным.",
        editor = "toggle",
        apply = "ui.apply"
    )]
    pub reduced_motion: bool,

    /// Длительность выезда/заезда settings sidebar; `0` отключает анимацию.
    #[setting(
        id = "ui.animations.sidebar_slide_duration_ms",
        path = "ui.animations.sidebar_slide_duration_ms",
        section = "ui",
        group = "animations",
        surface = "main-settings-window",
        label_id = "settings.ui.animations.sidebar_slide_duration_ms.label",
        label_ru = "Анимация сайдбара",
        description_id = "settings.ui.animations.sidebar_slide_duration_ms.description",
        description_ru = "Длительность выезда панели настроек и сжатия видео, мс.",
        help_id = "settings.ui.animations.sidebar_slide_duration_ms.help",
        help_ru = "0 отключает анимацию: панель появляется мгновенно.",
        editor = "integer",
        min = crate::validation::MIN_SIDEBAR_SLIDE_DURATION_MS,
        max = crate::validation::MAX_SIDEBAR_SLIDE_DURATION_MS,
        step = 50,
        unit = "ms",
        apply = "ui.apply"
    )]
    pub sidebar_slide_duration_ms: u16,
}

impl Default for UiAnimationsConfig {
    /// По умолчанию движение уменьшено, а сохранённая длительность доступна после отключения режима.
    fn default() -> Self {
        Self {
            reduced_motion: true,
            sidebar_slide_duration_ms: 500,
        }
    }
}
