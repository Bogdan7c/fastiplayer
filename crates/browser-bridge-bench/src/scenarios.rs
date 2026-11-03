//! Сценарии прогона стенда: проверка страниц в чистом браузере, дымок и `capture`
//! («отдать вкладку» → кадры в плеере, браузер не декодирует), `page-ends` (страница сама
//! заканчивает передачу — плеер честно это показывает), команды плеера, статус настоящего
//! экстеншена.
//!
//! Сценарии НИЧЕГО не печатают и не знают про командную строку: каждый возвращает
//! типизированные [`StepResult`], а вывод таблицы и код выхода делает бинарник (`report`).
//! Любая ошибка стенда внутри сценария превращается в `Fail` с текстом ошибки, поэтому
//! сбой одного браузера не прерывает прогон остальных.

mod capture;
#[cfg(test)]
mod capture_pages_contract;
mod codec_cases;
mod codecs;
mod context;
mod evidence;
mod extension_status;
mod handoff_run;
mod outcome;
mod page_catalog;
mod page_ends;
mod page_filter;
mod pages;
mod player_commands;

pub use context::{BrowserExecutables, RunSettings, ScenarioError, Stand};
pub use outcome::{Outcome, StepResult};
pub use page_catalog::{PAGE_SCENARIOS, PageScenario};
pub use page_ends::end_page_ids;
pub use page_filter::PageFilter;

use crate::browsers::BrowserKind;

/// Проверка страниц стенда (все или выбранные `--page`) в чистом браузере, без экстеншенов.
pub fn run_pages(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    pages::run(stand, browser, filter)
}

/// Дымок: одна страница (`mse-fmp4`) путём `capture` — настоящий экстеншен, посредник, плеер.
pub fn run_smoke(stand: &Stand, browser: BrowserKind) -> StepResult {
    capture::run_page(stand, browser, "smoke", capture::SMOKE_PAGE)
}

/// MSE-страницы стенда (все или выбранные `--page`): «отдать вкладку» → кадры в плеере,
/// браузер не декодирует.
pub fn run_capture(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    capture::run_all(stand, browser, filter)
}

/// Страницы, которые сами заканчивают передачу (все или выбранные `--page`): плеер обязан
/// честно, быстро и правильно показать конец.
pub fn run_page_ends(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    page_ends::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `capture` (для проверки `--page` в командной строке).
pub fn capture_page_ids() -> Vec<&'static str> {
    capture::capture_page_ids()
}

/// Команды плеера (пауза, play, перемотка, закрытие окна) доходят до страницы под тенью.
pub fn run_player_commands(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    player_commands::run(stand, browser)
}

/// Попап настоящего экстеншена показывает «Connected to Fastiplayer».
pub fn run_extension_status(stand: &Stand, browser: BrowserKind) -> StepResult {
    extension_status::run(stand, browser)
}

/// Плеер сайта спрашивает кодеки AV1 → VP9 → H.264; экстеншен (политика кодеков, HLS) отвечает
/// за плеер по настройке и метке плеера, выбранное играет.
pub fn run_codecs(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    codecs::run(stand, browser)
}
