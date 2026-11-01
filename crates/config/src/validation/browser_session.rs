//! Validation секции `[browser_session]`: диапазоны окна буфера источника браузерного режима
//! и запоминаемого размера окна `[browser_session.window]`.
//!
//! Ошибки называют полный путь поля (`browser_session.<field>`), сообщения английские,
//! как у остальных секций; значение вне диапазона идёт общей политикой `InvalidValue`
//! (при запуске приложения — переименование файла в `.bak` и defaults).

use crate::{
    BrowserSessionConfig, BrowserWindowConfig, ConfigResult, MAX_BROWSER_BUFFER_AHEAD_S,
    MAX_BROWSER_MAX_HELD_MB, MAX_BROWSER_WINDOW_HEIGHT_POINTS, MAX_BROWSER_WINDOW_WIDTH_POINTS,
    MIN_BROWSER_BUFFER_AHEAD_S, MIN_BROWSER_MAX_HELD_MB, MIN_BROWSER_WINDOW_HEIGHT_POINTS,
    MIN_BROWSER_WINDOW_WIDTH_POINTS,
};

use super::validate_u16_range;

/// Проверяет запас вперёд и потолок памяти потока.
pub(super) fn validate_browser_session_section(
    browser_session: &BrowserSessionConfig,
) -> ConfigResult<()> {
    validate_u16_range(
        "browser_session.buffer_ahead_s",
        browser_session.buffer_ahead_s,
        MIN_BROWSER_BUFFER_AHEAD_S,
        MAX_BROWSER_BUFFER_AHEAD_S,
    )?;
    validate_u16_range(
        "browser_session.max_held_mb",
        browser_session.max_held_mb,
        MIN_BROWSER_MAX_HELD_MB,
        MAX_BROWSER_MAX_HELD_MB,
    )?;
    validate_browser_window(&browser_session.window)
}

/// Проверяет запоминаемый размер окна браузерного режима (полноэкранность — `bool`).
fn validate_browser_window(window: &BrowserWindowConfig) -> ConfigResult<()> {
    validate_u16_range(
        "browser_session.window.inner_width_points",
        window.inner_width_points,
        MIN_BROWSER_WINDOW_WIDTH_POINTS,
        MAX_BROWSER_WINDOW_WIDTH_POINTS,
    )?;
    validate_u16_range(
        "browser_session.window.inner_height_points",
        window.inner_height_points,
        MIN_BROWSER_WINDOW_HEIGHT_POINTS,
        MAX_BROWSER_WINDOW_HEIGHT_POINTS,
    )
}
