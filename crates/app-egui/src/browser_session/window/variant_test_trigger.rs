//! Тестовая переменная процесса: выбор варианта страницы без мыши (ТОЛЬКО для тестов).
//!
//! Сквозные тесты 14a (`tests/browser_session_process/page_variants.rs`) проверяют путь
//! «выбор в меню ♪ → `select_variant` странице → новое поколение на той же секунде». Меню
//! рисуют виджеты (сессия 14a P2b), а у MPRIS выбора варианта нет. Переменная зовёт ТО ЖЕ
//! намерение окна, что позовёт пункт меню, — [`AppState::select_page_variant`] — один раз,
//! когда меню нужного вида уже пришло и плеер дошёл до заданной секунды. Правила окна и
//! сеанса (реклама, ревизия меню, склад) остаются настоящими.
//!
//! Формат: `<вид>:<номер пункта>@<секунда зрителя в мс>`, например `audio:1@3500`; несколько выборов —
//! через запятую, по очереди (`audio:1@4000,quality:1@9000`, стенд «сериал» сессии 14b): следующий
//! ждёт, пока сделан предыдущий и плеер дошёл до его секунды.
//! Пользователю не нужна и не документируется в справке (по образцу
//! `FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS`, `drain_override.rs`). Без переменной модуль
//! ничего не делает.

use std::collections::VecDeque;
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

/// Значение переменной не в формате `<audio|quality>:<номер>@<мс>[,…]`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{TEST_BROWSER_SELECT_VARIANT_VARIABLE} must look like audio:1@3500 or audio:1@4000,quality:0@9000"
)]
pub(super) struct InvalidVariantTestRequest;

/// Чистый разбор значения: выборы по порядку (пусто — переменной нет).
pub(super) fn parse_variant_test_requests(
    value: Option<&OsStr>,
) -> Result<VecDeque<VariantTestRequest>, InvalidVariantTestRequest> {
    let Some(raw) = value else {
        return Ok(VecDeque::new());
    };
    let text = raw.to_str().ok_or(InvalidVariantTestRequest)?;
    text.split(',').map(parse_one_request).collect()
}

/// Один выбор `<вид>:<номер>@<мс>`.
fn parse_one_request(text: &str) -> Result<VariantTestRequest, InvalidVariantTestRequest> {
    let (choice, position) = text.split_once('@').ok_or(InvalidVariantTestRequest)?;
    let (kind, index) = choice.split_once(':').ok_or(InvalidVariantTestRequest)?;
    let kind = match kind {
        "audio" => PageVariantKind::Audio,
        "quality" => PageVariantKind::Quality,
        _ => return Err(InvalidVariantTestRequest),
    };
    let index: usize = index.parse().map_err(|_| InvalidVariantTestRequest)?;
    let milliseconds: u64 = position.parse().map_err(|_| InvalidVariantTestRequest)?;
    Ok(VariantTestRequest {
        kind,
        index: PageVariantIndex::new(index),
        not_before: Duration::from_millis(milliseconds),
    })
}

/// Тестовые выборы: каждый ждёт меню и секунду, потом один раз зовёт намерение окна.
#[derive(Debug, Default)]
pub(super) struct VariantTestTrigger {
    pending: VecDeque<VariantTestRequest>,
}

impl VariantTestTrigger {
    /// Из окружения процесса; незнакомое значение — предупреждение и ничего.
    pub(super) fn from_environment() -> Self {
        let raw = std::env::var_os(TEST_BROWSER_SELECT_VARIANT_VARIABLE);
        match parse_variant_test_requests(raw.as_deref()) {
            Ok(pending) => Self { pending },
            Err(error) => {
                warn!(%error, "Ignoring the test page variant trigger");
                Self::default()
            }
        }
    }

    /// Проход окна: если пора — сделать очередной выбор (каждый — один раз за процесс).
    pub(super) fn poll(&mut self, app_state: &mut AppState) {
        let Some(&request) = self.pending.front() else {
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
        self.pending.pop_front();
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
        assert_eq!(parse_variant_test_requests(None), Ok(VecDeque::new()));
    }

    #[test]
    fn kind_index_and_second_are_read() {
        assert_eq!(
            parse_variant_test_requests(Some(OsStr::new("audio:1@3500"))),
            Ok(VecDeque::from([VariantTestRequest {
                kind: PageVariantKind::Audio,
                index: PageVariantIndex::new(1),
                not_before: Duration::from_millis(3500),
            }]))
        );
        assert_eq!(
            parse_variant_test_requests(Some(OsStr::new("quality:0@0")))
                .ok()
                .and_then(|requests| requests.front().map(|request| request.kind)),
            Some(PageVariantKind::Quality)
        );
    }

    #[test]
    fn several_choices_keep_their_order() {
        let requests = parse_variant_test_requests(Some(OsStr::new("audio:1@4000,quality:0@9000")))
            .expect("two choices");
        let summary: Vec<_> = requests
            .iter()
            .map(|request| {
                (
                    request.kind,
                    request.index.get(),
                    request.not_before.as_millis(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (PageVariantKind::Audio, 1, 4000),
                (PageVariantKind::Quality, 0, 9000)
            ]
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
            "audio:1@5,",
            "audio:1@5,quality",
        ] {
            assert_eq!(
                parse_variant_test_requests(Some(OsStr::new(value))),
                Err(InvalidVariantTestRequest),
                "{value:?}"
            );
        }
    }
}
