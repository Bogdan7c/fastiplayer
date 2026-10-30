//! Validation секции `[browser_session]`: диапазоны окна буфера источника браузерного режима.
//!
//! Ошибки называют полный путь поля (`browser_session.<field>`), сообщения английские,
//! как у остальных секций; значение вне диапазона идёт общей политикой `InvalidValue`
//! (при запуске приложения — переименование файла в `.bak` и defaults).

use crate::{
    BrowserSessionConfig, ConfigResult, MAX_BROWSER_BUFFER_AHEAD_S, MAX_BROWSER_BUFFER_BEHIND_S,
    MIN_BROWSER_BUFFER_AHEAD_S, MIN_BROWSER_BUFFER_BEHIND_S,
};

use super::validate_u16_range;

/// Проверяет запас вперёд и окно позади позиции.
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
        "browser_session.buffer_behind_s",
        browser_session.buffer_behind_s,
        MIN_BROWSER_BUFFER_BEHIND_S,
        MAX_BROWSER_BUFFER_BEHIND_S,
    )
}
