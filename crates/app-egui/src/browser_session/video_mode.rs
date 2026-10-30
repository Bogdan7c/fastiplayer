//! Как процесс браузерного режима показывает видео: окном (по умолчанию) или без окна.
//!
//! С сессии 04a видео потока страницы рисует окно браузерного режима (решение 120).
//! Безоконный путь 03b (счётчик кадров вместо рендера) остаётся ТОЛЬКО для тестов
//! (решение основного агента 04a, согласовано с владельцем): сквозные тесты процесса
//! идут на CI без дисплея. Включается переменной окружения — по образцу
//! `FASTIPLAYER_TEST_BROWSER_AUDIO` ([`super::playback::metered_audio`]).

use std::ffi::OsStr;

use tracing::{info, warn};

/// Переменная окружения ТОЛЬКО для тестов: `headless` — без окна.
///
/// Пользователю она не нужна и не документируется в справке.
pub(crate) const TEST_BROWSER_VIDEO_VARIABLE: &str = "FASTIPLAYER_TEST_BROWSER_VIDEO";

/// Значение переменной, выключающее окно.
const HEADLESS_VIDEO_VALUE: &str = "headless";

/// Как показывать видео потока страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserVideoMode {
    /// Окно браузерного режима с рендером обычного плеера.
    Window,
    /// Без окна: кадры только считаются (тесты на машине без дисплея).
    HeadlessForTests,
}

/// Переменная окружения задана, но значение незнакомо.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown value of {TEST_BROWSER_VIDEO_VARIABLE}; only `{HEADLESS_VIDEO_VALUE}` is known")]
pub(crate) struct UnknownVideoOverride;

impl BrowserVideoMode {
    /// Чистый разбор значения тестовой переменной (`None` — переменной нет).
    pub(crate) fn from_test_override(value: Option<&OsStr>) -> Result<Self, UnknownVideoOverride> {
        match value {
            None => Ok(Self::Window),
            Some(text) if text == HEADLESS_VIDEO_VALUE => Ok(Self::HeadlessForTests),
            Some(_) => Err(UnknownVideoOverride),
        }
    }

    /// Край процесса: читает переменную один раз при старте. Незнакомое значение не
    /// ломает запуск: окно и предупреждение (значение в журнал не пишется — чужой ввод).
    pub(crate) fn from_environment() -> Self {
        let raw_value = std::env::var_os(TEST_BROWSER_VIDEO_VARIABLE);
        match Self::from_test_override(raw_value.as_deref()) {
            Ok(Self::HeadlessForTests) => {
                info!(
                    variable = TEST_BROWSER_VIDEO_VARIABLE,
                    "Browser session runs without a window (test-only override)"
                );
                Self::HeadlessForTests
            }
            Ok(Self::Window) => Self::Window,
            Err(error) => {
                warn!(error = %error, "Ignoring the test video override");
                Self::Window
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_the_variable_the_process_opens_a_window() {
        assert_eq!(
            BrowserVideoMode::from_test_override(None),
            Ok(BrowserVideoMode::Window)
        );
    }

    #[test]
    fn headless_value_keeps_the_windowless_test_path() {
        assert_eq!(
            BrowserVideoMode::from_test_override(Some(OsStr::new("headless"))),
            Ok(BrowserVideoMode::HeadlessForTests)
        );
    }

    #[test]
    fn unknown_value_is_reported_not_guessed() {
        assert_eq!(
            BrowserVideoMode::from_test_override(Some(OsStr::new("Headless"))),
            Err(UnknownVideoOverride)
        );
        assert_eq!(
            BrowserVideoMode::from_test_override(Some(OsStr::new(""))),
            Err(UnknownVideoOverride)
        );
    }
}
