//! Тестовая переменная процесса: выбор варианта страницы без мыши (ТОЛЬКО для тестов).
//!
//! Сквозные тесты 14a (`tests/browser_session_process/page_variants.rs`) проверяют путь
//! «выбор в меню ♪ → `select_variant` странице → новое поколение на той же секунде». Меню
//! рисуют виджеты (сессия 14a P2b), а у MPRIS выбора варианта нет. Переменная зовёт ТО ЖЕ
//! намерение окна, что позовёт пункт меню, — [`AppState::select_page_variant`] — один раз,
//! когда меню нужного вида уже пришло и плеер дошёл до заданной секунды. Правила окна и
//! сеанса (реклама, ревизия меню, склад) остаются настоящими.
//!
//! Формат: `<вид>:<номер пункта>@<секунда зрителя в мс>`, например `audio:1@3500`.
//! Пользователю не нужна и не документируется в справке (по образцу
//! `FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS`, `drain_override.rs`). Без переменной модуль
//! ничего не делает.

use std::ffi::OsStr;
use std::time::Duration;

use tracing::{info, warn};

use crate::page_variants::{PageVariantIndex, PageVariantKind};
use crate::state::{AppState, PageVariantSelectOutcome};

/// Переменная окружения ТОЛЬКО для тестов (формат — в описании модуля).
pub(super) const TEST_BROWSER_SELECT_VARIANT_VARIABLE: &str =
    "FASTIPLAYER_TEST_BROWSER_SELECT_VARIANT";

/// Строка журнала: выбор сделан тестовой переменной (её ждут сквозные тесты).
pub(super) const TEST_VARIANT_SELECTED_LINE: &str = "Test-only trigger selected a page variant";

/// Что выбрать и когда.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VariantTestRequest {
    kind: PageVariantKind,
    index: PageVariantIndex,
    not_before: Duration,
}

/// Значение переменной не в формате `<audio|quality>:<номер>@<мс>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{TEST_BROWSER_SELECT_VARIANT_VARIABLE} must look like audio:1@3500 or quality:0@1000")]
pub(super) struct InvalidVariantTestRequest;

/// Чистый разбор значения (`None` — переменной нет).
pub(super) fn parse_variant_test_request(
    value: Option<&OsStr>,
) -> Result<Option<VariantTestRequest>, InvalidVariantTestRequest> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let text = raw.to_str().ok_or(InvalidVariantTestRequest)?;
    let (choice, position) = text.split_once('@').ok_or(InvalidVariantTestRequest)?;
    let (kind, index) = choice.split_once(':').ok_or(InvalidVariantTestRequest)?;
    let kind = match kind {
        "audio" => PageVariantKind::Audio,
        "quality" => PageVariantKind::Quality,
        _ => return Err(InvalidVariantTestRequest),
    };
    let index: usize = index.parse().map_err(|_| InvalidVariantTestRequest)?;
    let milliseconds: u64 = position.parse().map_err(|_| InvalidVariantTestRequest)?;
    Ok(Some(VariantTestRequest {
        kind,
        index: PageVariantIndex::new(index),
        not_before: Duration::from_millis(milliseconds),
    }))
}

/// Тестовый выбор: ждёт меню и секунду, потом один раз зовёт намерение окна.
#[derive(Debug, Default)]
pub(super) struct VariantTestTrigger {
    pending: Option<VariantTestRequest>,
}

impl VariantTestTrigger {
    /// Из окружения процесса; незнакомое значение — предупреждение и ничего.
    pub(super) fn from_environment() -> Self {
        let raw = std::env::var_os(TEST_BROWSER_SELECT_VARIANT_VARIABLE);
        match parse_variant_test_request(raw.as_deref()) {
            Ok(pending) => Self { pending },
            Err(error) => {
                warn!(%error, "Ignoring the test page variant trigger");
                Self::default()
            }
        }
    }

    /// Проход окна: если пора — выбрать (один раз за процесс).
    pub(super) fn poll(&mut self, app_state: &mut AppState) {
        let Some(request) = self.pending else {
            return;
        };
        let Some(choice) = app_state
            .page_variant_facts()
            .menu(request.kind)
            .and_then(|menu| menu.choice(request.index))
        else {
            return;
        };
        if app_state.refresh_player_snapshot().current_position < request.not_before {
            return;
        }
        self.pending = None;
        let outcome = app_state.select_page_variant(choice);
        info!(
            kind = request.kind.log_label(),
            index = request.index.get(),
            queued = outcome == PageVariantSelectOutcome::Queued,
            "{TEST_VARIANT_SELECTED_LINE}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_the_variable_nothing_is_selected() {
        assert_eq!(parse_variant_test_request(None), Ok(None));
    }

    #[test]
    fn kind_index_and_second_are_read() {
        assert_eq!(
            parse_variant_test_request(Some(OsStr::new("audio:1@3500"))),
            Ok(Some(VariantTestRequest {
                kind: PageVariantKind::Audio,
                index: PageVariantIndex::new(1),
                not_before: Duration::from_millis(3500),
            }))
        );
        assert_eq!(
            parse_variant_test_request(Some(OsStr::new("quality:0@0")))
                .ok()
                .flatten()
                .map(|request| request.kind),
            Some(PageVariantKind::Quality)
        );
    }

    #[test]
    fn garbage_is_reported_not_guessed() {
        for value in [
            "",
            "audio",
            "audio:1",
            "subtitles:0@1",
            "audio:x@1",
            "audio:1@-5",
        ] {
            assert_eq!(
                parse_variant_test_request(Some(OsStr::new(value))),
                Err(InvalidVariantTestRequest),
                "{value:?}"
            );
        }
    }
}
