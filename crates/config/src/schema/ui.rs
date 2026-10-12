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
        label_ru = "Show telemetry",
        description_id = "settings.ui.show_telemetry.description",
        description_ru = "Show the diagnostic telemetry panel.",
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
        label_ru = "UI language",
        description_id = "settings.ui.language.description",
        description_ru = "Reserved for future localization. The current interface uses English regardless of this setting. Language code, for example `ru` or `en`.",
        editor = "text",
        min_len = crate::validation::MIN_UI_LANGUAGE_LEN,
        max_len = crate::validation::MAX_UI_LANGUAGE_LEN,
        apply = "ui.apply"
    )]
    // Настройка, default и маршрут сохранены для будущей локализации.
    // Текущие пользовательские строки всегда английские, включая поля metadata *_ru.
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
        description_ru = "Stable UI skin ID; an unknown ID is a configuration error.",
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
    /// Сохраняет исторический default языка для совместимости; UI пока всегда английский.
    fn default() -> Self {
        Self {
            // Диагностическая панель нужна разработчику, а не зрителю: по умолчанию
            // скрыта. Явно записанное `true` у прежних установок не мигрируется
            // (решение владельца 2026-10-11).
            show_telemetry: false,
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
        label_ru = "Window titlebar height",
        description_id = "settings.ui.window.titlebar_height_px.description",
        description_ru = "Custom Fastiplayer titlebar height in logical UI pixels.",
        help_id = "settings.ui.window.titlebar_height_px.help",
        help_ru = "The panel overlays the video: it does not shrink the viewport or add an exclusion rectangle.",
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
        label_ru = "Window corner radius",
        description_id = "settings.ui.window.corner_radius_px.description",
        description_ru = "Radius of transparent window corners in logical UI pixels.",
        help_id = "settings.ui.window.corner_radius_px.help",
        help_ru = "0 disables rounding. Maximized/fullscreen windows always have square corners; if the compositor does not support transparency, a safe square fallback is used.",
        editor = "integer",
        min = MIN_WINDOW_CORNER_RADIUS_PX,
        max = MAX_WINDOW_CORNER_RADIUS_PX,
        step = 1,
        unit = "px",
        apply = "ui.apply"
    )]
    pub corner_radius_px: u16,

    /// Через сколько миллисекунд бездействия в полноэкранном режиме прятать панели и курсор.
    ///
    /// `0` отключает автоскрытие. Окно в обычном режиме ничего не прячет (решение владельца,
    /// сессия UX 14). Скорость уезда панелей берётся из анимации сайдбара.
    #[setting(
        id = "ui.window.fullscreen_autohide_delay_ms",
        path = "ui.window.fullscreen_autohide_delay_ms",
        section = "ui",
        group = "window",
        surface = "main-settings-window",
        label_id = "settings.ui.window.fullscreen_autohide_delay_ms.label",
        label_ru = "Fullscreen panel hide delay",
        description_id = "settings.ui.window.fullscreen_autohide_delay_ms.description",
        description_ru = "Idle time without mouse movement or key presses before hiding the titlebar, bottom panel, and cursor, in milliseconds.",
        help_id = "settings.ui.window.fullscreen_autohide_delay_ms.help",
        help_ru = "0 means never hide. Panels remain visible while video is paused, a sidebar or menu is open, the cursor is over a panel, or a drag is in progress. Panels use the same animation speed as the sidebar.",
        editor = "integer",
        min = crate::validation::MIN_FULLSCREEN_AUTOHIDE_DELAY_MS,
        max = crate::validation::MAX_FULLSCREEN_AUTOHIDE_DELAY_MS,
        step = 250,
        unit = "ms",
        apply = "ui.apply"
    )]
    pub fullscreen_autohide_delay_ms: u16,
}

impl Default for UiWindowConfig {
    /// Возвращает высоту titlebar, близкую к обычным desktop chrome controls.
    fn default() -> Self {
        Self {
            titlebar_height_px: 40,
            corner_radius_px: 12,
            fullscreen_autohide_delay_ms: 2500,
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
        label_ru = "Sidebar width",
        description_id = "settings.ui.sidebar.width_points.description",
        description_ru = "Shared width of Playlist, Settings, URL, and Info in logical UI points.",
        help_id = "settings.ui.sidebar.width_points.help",
        help_ru = "You can also resize an open panel by dragging its right edge.",
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
        label_ru = "Live preview frequency",
        description_id = "settings.ui.settings.live_preview_max_hz.description",
        description_ru = "Maximum live preview update frequency after Apply.",
        help_id = "settings.ui.settings.live_preview_max_hz.help",
        help_ru = "This is a committed setting: an open draft/preview transaction keeps using the last applied value.",
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
        label_ru = "Reduce motion",
        description_id = "settings.ui.animations.reduced_motion.description",
        description_ru = "Show the sidebar and speed indicator without movement, and buttons without scaling.",
        help_id = "settings.ui.animations.reduced_motion.help",
        help_ru = "Short color transitions are preserved to keep button states clear.",
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
        label_ru = "Sidebar animation",
        description_id = "settings.ui.animations.sidebar_slide_duration_ms.description",
        description_ru = "Settings panel slide and video resize animation duration, in milliseconds.",
        help_id = "settings.ui.animations.sidebar_slide_duration_ms.help",
        help_ru = "0 disables animation: the panel appears immediately.",
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
    /// По умолчанию анимации включены (решение владельца 2026-10-08); `reduced_motion = true`
    /// делает переходы мгновенными, сохранённая длительность при этом не теряется.
    fn default() -> Self {
        Self {
            reduced_motion: false,
            sidebar_slide_duration_ms: 500,
        }
    }
}
