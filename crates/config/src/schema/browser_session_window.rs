//! Запоминаемое окно браузерного режима `[browser_session.window]` (схема v29).
//!
//! Решения владельца 137–138: размер и полноэкранность запоминает только окно браузерного
//! режима (свои ключи; обычное окно их не читает и не пишет). Это «запоминаемое состояние»,
//! а не настройка: окно само записывает значения, когда пользователь меняет размер или
//! входит в полноэкранный режим, а соседние копии плеера на лету их **не** применяют —
//! новое окно браузерного режима просто открывается с последним размером.
//!
//! Поэтому ключи зарегистрированы в реестре настроек (их можно записать по id одним
//! runtime-коммитом через общую доску), но лежат на отдельной поверхности
//! [`REMEMBERED_STATE_SURFACE_ID`], а не на поверхности окна настроек.

use serde::{Deserialize, Serialize};

/// Поверхность настроек «запоминаемое состояние»: ключи, которые программа записывает
/// сама и которые окно настроек не показывает (решение 137).
///
/// Совпадает с литералом `surface = "remembered-state"` в метаданных ниже; тест
/// `browser_window_keys_live_on_remembered_state_surface` держит их согласованными.
pub const REMEMBERED_STATE_SURFACE_ID: &str = "remembered-state";

/// Внутренняя ширина окна браузерного режима по умолчанию, логических точек.
pub const DEFAULT_BROWSER_WINDOW_WIDTH_POINTS: u16 = 1280;
/// Внутренняя высота окна браузерного режима по умолчанию, логических точек (16:9 к ширине).
pub const DEFAULT_BROWSER_WINDOW_HEIGHT_POINTS: u16 = 720;

/// Минимальная ширина: как у главного окна (`app-egui/src/app_shell/window_limits.rs`,
/// `MIN_WINDOW_WIDTH_POINTS = 400`) — уже́ не помещается порядок кнопок нижней панели.
pub const MIN_BROWSER_WINDOW_WIDTH_POINTS: u16 = 400;
/// Минимальная высота: как у главного окна (`MIN_WINDOW_HEIGHT_POINTS` = titlebar 40 +
/// нижняя панель 160 + полоса видео 32) — ниже панели налезают друг на друга.
pub const MIN_BROWSER_WINDOW_HEIGHT_POINTS: u16 = 232;

/// Максимальная ширина: 7680 — экран 8K при масштабе 1. Окно больше любого экрана
/// оконный менеджер всё равно ужмёт, а большее число в файле говорит о мусоре.
pub const MAX_BROWSER_WINDOW_WIDTH_POINTS: u16 = 7680;
/// Максимальная высота: 4320 — экран 8K при масштабе 1 (та же причина).
pub const MAX_BROWSER_WINDOW_HEIGHT_POINTS: u16 = 4320;

/// Запоминаемый размер и полноэкранность окна браузерного режима.
///
/// Размер — внутренняя область окна (без рамки) в логических точках, чтобы значение не
/// зависело от масштаба экрана. В полноэкранном режиме хранится последний оконный размер:
/// при выходе из полноэкранного режима окно возвращается к нему.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserWindowConfig {
    /// Внутренняя ширина окна, логических точек.
    #[setting(
        id = "browser_session.window.inner_width_points",
        path = "browser_session.window.inner_width_points",
        section = "browser_mode",
        group = "window",
        surface = "remembered-state",
        label_id = "settings.browser_session.window.inner_width_points.label",
        label_ru = "Browser window width",
        description_id = "settings.browser_session.window.inner_width_points.description",
        description_ru = "Remembered width of the browser mode window in logical points. The window saves it when you resize it.",
        editor = "integer",
        min = crate::MIN_BROWSER_WINDOW_WIDTH_POINTS,
        max = crate::MAX_BROWSER_WINDOW_WIDTH_POINTS,
        step = 1,
        unit = "pt",
        apply = "browser_session.apply"
    )]
    pub inner_width_points: u16,

    /// Внутренняя высота окна, логических точек.
    #[setting(
        id = "browser_session.window.inner_height_points",
        path = "browser_session.window.inner_height_points",
        section = "browser_mode",
        group = "window",
        surface = "remembered-state",
        label_id = "settings.browser_session.window.inner_height_points.label",
        label_ru = "Browser window height",
        description_id = "settings.browser_session.window.inner_height_points.description",
        description_ru = "Remembered height of the browser mode window in logical points. The window saves it when you resize it.",
        editor = "integer",
        min = crate::MIN_BROWSER_WINDOW_HEIGHT_POINTS,
        max = crate::MAX_BROWSER_WINDOW_HEIGHT_POINTS,
        step = 1,
        unit = "pt",
        apply = "browser_session.apply"
    )]
    pub inner_height_points: u16,

    /// Открывать окно браузерного режима сразу во весь экран.
    #[setting(
        id = "browser_session.window.fullscreen",
        path = "browser_session.window.fullscreen",
        section = "browser_mode",
        group = "window",
        surface = "remembered-state",
        label_id = "settings.browser_session.window.fullscreen.label",
        label_ru = "Browser window fullscreen",
        description_id = "settings.browser_session.window.fullscreen.description",
        description_ru = "Whether the browser mode window was last left in fullscreen. The window saves it when you enter or leave fullscreen.",
        editor = "toggle",
        apply = "browser_session.apply"
    )]
    pub fullscreen: bool,
}

impl Default for BrowserWindowConfig {
    /// Окно 1280×720 точек, не во весь экран.
    fn default() -> Self {
        Self {
            inner_width_points: DEFAULT_BROWSER_WINDOW_WIDTH_POINTS,
            inner_height_points: DEFAULT_BROWSER_WINDOW_HEIGHT_POINTS,
            fullscreen: false,
        }
    }
}
