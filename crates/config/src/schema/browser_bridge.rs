//! Настройки моста с браузерами `[browser_bridge]` (схема v31).
//!
//! Решение владельца (browser-bridge, сессия 06): при первом запуске плеер один раз
//! предлагает подключить найденные браузеры. Единственный ключ секции — `offer_on_startup`:
//! показывать ли это предложение при старте. Значение читается **только при запуске**, живого
//! эффекта у него нет; когда пользователь отвечает на предложение (любой кнопкой или
//! крестиком), приложение само пишет `false` одиночным runtime-коммитом, а в окне настроек
//! (раздел «Browser integration») пользователь может вернуть `true`.

use serde::{Deserialize, Serialize};

/// Предлагать ли подключить браузеры при запуске, по умолчанию (новый пользователь видит
/// предложение один раз).
pub const DEFAULT_BROWSER_BRIDGE_OFFER_ON_STARTUP: bool = true;

/// Секция `[browser_bridge]`: поведение предложения «подключить браузеры».
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserBridgeConfig {
    /// Показывать предложение подключить браузеры при запуске.
    #[setting(
        id = "browser_bridge.offer_on_startup",
        path = "browser_bridge.offer_on_startup",
        section = "browser_integration",
        group = "offer",
        surface = "main-settings-window",
        label_id = "settings.browser_bridge.offer_on_startup.label",
        label_ru = "Suggest connecting browsers at startup",
        description_id = "settings.browser_bridge.offer_on_startup.description",
        description_ru = "When on, Fastiplayer offers once at startup to connect the browsers it finds, so videos from web pages can play in it. It is read only when the app starts and switches off by itself after you answer the offer.",
        editor = "toggle",
        apply = "browser_session.apply"
    )]
    pub offer_on_startup: bool,
}

impl Default for BrowserBridgeConfig {
    /// Предложение включено.
    fn default() -> Self {
        Self {
            offer_on_startup: DEFAULT_BROWSER_BRIDGE_OFFER_ON_STARTUP,
        }
    }
}
