//! Route настроек браузерного режима (`browser_session.apply`).
//!
//! Контракт применения для владельца (окно браузерного режима, сессия 04c моста):
//! - группа [`AppRuntimeRouteGroup::BrowserSessionBuffer`] — окно буфера источника
//!   «байты от страницы» (`buffer_ahead_s`/`buffer_behind_s`). Новое значение действует на
//!   **следующее** видео со страницы: источник уже идущего потока не перестраивается
//!   (поток от страницы нельзя открыть заново). Значение берётся из
//!   [`BrowserSessionRuntimeSettingsUpdate::buffer_window`];
//! - группа [`AppRuntimeRouteGroup::BrowserSessionWindow`] — запоминаемое окно (размер и
//!   полноэкранность). Это «запоминаемое состояние» (решение 137): окно записывает его
//!   само, а живое окно по маршруту **не** меняется — ни своё, ни соседнее. Следующее
//!   окно браузерного режима откроется с [`BrowserSessionRuntimeSettingsUpdate::remembered_window`].
//!
//! Обычному плееру маршрут ничего не меняет: браузерного источника и окна у него нет.
//! Маршрут не трогает player, renderer и источники обычного плеера, поэтому preflight
//! не нужен.

use fastiplayer_config::{
    AppConfig, BrowserSessionBufferWindow, BrowserSessionConfig, BrowserWindowConfig,
};

use super::{
    AppRuntimeRoute, AppRuntimeRouteGroup, RouteInput, RuntimeCommittedRoute,
    RuntimeCommittedUpdate,
};

/// Route id, который metadata выводит из `apply = "browser_session.apply"`.
pub const BROWSER_SESSION_ROUTE_ID: &str = "browser_session";

/// Префикс id ключей запоминаемого окна браузерного режима.
const BROWSER_WINDOW_SETTING_PREFIX: &str = "browser_session.window.";

/// Полный typed snapshot секции `[browser_session]` для владельца браузерного режима.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserSessionRuntimeSettingsUpdate {
    /// Новая committed секция браузерного режима целиком.
    pub browser_session: BrowserSessionConfig,
}

impl BrowserSessionRuntimeSettingsUpdate {
    /// Окно буфера для источника **следующего** видео со страницы.
    #[must_use]
    pub fn buffer_window(&self) -> BrowserSessionBufferWindow {
        self.browser_session.buffer_window()
    }

    /// Запоминаемое окно для **следующего** открытия окна браузерного режима.
    #[must_use]
    pub fn remembered_window(&self) -> BrowserWindowConfig {
        self.browser_session.window
    }
}

/// Группа владельца для одного ключа секции: окно буфера или запоминаемое окно.
pub(super) fn group_for_setting(setting_id: &str) -> AppRuntimeRouteGroup {
    if setting_id.starts_with(BROWSER_WINDOW_SETTING_PREFIX) {
        AppRuntimeRouteGroup::BrowserSessionWindow
    } else {
        AppRuntimeRouteGroup::BrowserSessionBuffer
    }
}

/// Собирает атомарный payload route браузерного режима.
pub(super) fn committed_route(current: &AppConfig, input: RouteInput) -> RuntimeCommittedRoute {
    let groups = input.groups();
    RuntimeCommittedRoute {
        route: AppRuntimeRoute::BrowserSession,
        source_routes: input.source_routes,
        affected_settings: input.affected_settings,
        groups,
        update: RuntimeCommittedUpdate::BrowserSession(BrowserSessionRuntimeSettingsUpdate {
            browser_session: current.browser_session,
        }),
    }
}
