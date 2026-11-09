//! Тестовая переменная процесса: действие зрителя с субтитрами без клавиатуры (ТОЛЬКО для
//! тестов).
//!
//! Сквозной тест 14a (`tests/browser_session_process/page_subtitle_preference.rs`) проверяет
//! путь «зритель сам выключил субтитры → странице ровно один `subtitle_choice`». У MPRIS
//! субтитров нет, а настоящая клавиатура в частном `kwin_wayland` теста недоступна.
//! Переменная зовёт ТО ЖЕ действие, что клавиши C и V ([`SubtitleControlAction`]) — через
//! [`AppState::apply_subtitle_control_action`], один раз, когда у медиа уже есть дорожки
//! субтитров и плеер дошёл до заданной секунды. Правила владельца субтитров и сеанса
//! (что сообщать странице, версия протокола) остаются настоящими.
//!
//! Формат: `<toggle|next_track>@<секунда зрителя в мс>`, например `toggle@1500` (клавиша C).
//! Пользователю не нужна и не документируется в справке (по образцу
//! `variant_test_trigger.rs`). Без переменной модуль ничего не делает.

use std::ffi::OsStr;
use std::time::Duration;

use tracing::{info, warn};
use winit::window::Window;

use crate::state::AppState;
use crate::ui::player_controls::SubtitleControlAction;

/// Переменная окружения ТОЛЬКО для тестов (формат — в описании модуля).
pub(super) const TEST_BROWSER_SUBTITLE_ACTION_VARIABLE: &str =
    "FASTIPLAYER_TEST_BROWSER_SUBTITLE_ACTION";

/// Строка журнала: действие сделано тестовой переменной (её ждут сквозные тесты).
pub(super) const TEST_SUBTITLE_ACTION_LINE: &str = "Test-only trigger applied a subtitle action";

/// Какая клавиша зрителя и когда.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SubtitleTestRequest {
    action: SubtitleControlAction,
    not_before: Duration,
}

/// Значение переменной не в формате `<toggle|next_track>@<мс>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{TEST_BROWSER_SUBTITLE_ACTION_VARIABLE} must look like toggle@1500 or next_track@0")]
pub(super) struct InvalidSubtitleTestRequest;

/// Чистый разбор значения (`None` — переменной нет).
pub(super) fn parse_subtitle_test_request(
    value: Option<&OsStr>,
) -> Result<Option<SubtitleTestRequest>, InvalidSubtitleTestRequest> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let text = raw.to_str().ok_or(InvalidSubtitleTestRequest)?;
    let (action, position) = text.split_once('@').ok_or(InvalidSubtitleTestRequest)?;
    let action = match action {
        // Клавиша C.
        "toggle" => SubtitleControlAction::ToggleVisibility,
        // Клавиша V.
        "next_track" => SubtitleControlAction::SelectNextTrack,
        _ => return Err(InvalidSubtitleTestRequest),
    };
    let milliseconds: u64 = position.parse().map_err(|_| InvalidSubtitleTestRequest)?;
    Ok(Some(SubtitleTestRequest {
        action,
        not_before: Duration::from_millis(milliseconds),
    }))
}

/// Тестовое действие: ждёт дорожки и секунду, потом один раз зовёт действие зрителя.
#[derive(Debug, Default)]
pub(super) struct SubtitleTestTrigger {
    pending: Option<SubtitleTestRequest>,
}

impl SubtitleTestTrigger {
    /// Из окружения процесса; незнакомое значение — предупреждение и ничего.
    pub(super) fn from_environment() -> Self {
        let raw = std::env::var_os(TEST_BROWSER_SUBTITLE_ACTION_VARIABLE);
        match parse_subtitle_test_request(raw.as_deref()) {
            Ok(pending) => Self { pending },
            Err(error) => {
                warn!(%error, "Ignoring the test subtitle action trigger");
                Self::default()
            }
        }
    }

    /// Проход окна: если пора — действовать (один раз за процесс).
    pub(super) fn poll(&mut self, app_state: &mut AppState, window: &Window) {
        let Some(request) = self.pending else {
            return;
        };
        if !app_state.subtitle_menu_view().has_tracks {
            return;
        }
        if app_state.refresh_player_snapshot().current_position < request.not_before {
            return;
        }
        self.pending = None;
        app_state.apply_subtitle_control_action(request.action, window);
        info!(action = ?request.action, "{TEST_SUBTITLE_ACTION_LINE}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_the_variable_nothing_happens() {
        assert_eq!(parse_subtitle_test_request(None), Ok(None));
    }

    #[test]
    fn the_key_and_the_second_are_read() {
        assert_eq!(
            parse_subtitle_test_request(Some(OsStr::new("toggle@1500"))),
            Ok(Some(SubtitleTestRequest {
                action: SubtitleControlAction::ToggleVisibility,
                not_before: Duration::from_millis(1500),
            }))
        );
        assert_eq!(
            parse_subtitle_test_request(Some(OsStr::new("next_track@0")))
                .ok()
                .flatten()
                .map(|request| request.action),
            Some(SubtitleControlAction::SelectNextTrack)
        );
    }

    #[test]
    fn garbage_is_reported_not_guessed() {
        for value in ["", "toggle", "off@1", "toggle@x", "toggle@-1"] {
            assert_eq!(
                parse_subtitle_test_request(Some(OsStr::new(value))),
                Err(InvalidSubtitleTestRequest),
                "{value:?}"
            );
        }
    }
}
