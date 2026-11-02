//! Сценарии прогона стенда: проверка страниц в чистом браузере, дымок и `capture`
//! («отдать вкладку» → кадры в плеере, браузер не декодирует), команды плеера,
//! статус настоящего экстеншена.
//!
//! Сценарии НИЧЕГО не печатают и не знают про командную строку: каждый возвращает
//! типизированные [`StepResult`], а вывод таблицы и код выхода делает бинарник (`report`).
//! Любая ошибка стенда внутри сценария превращается в `Fail` с текстом ошибки, поэтому
//! сбой одного браузера не прерывает прогон остальных.

mod capture;
mod context;
mod evidence;
mod extension_status;
mod handoff_run;
mod outcome;
mod page_catalog;
mod pages;
mod player_commands;

pub use context::{BrowserExecutables, RunSettings, ScenarioError, Stand};
pub use outcome::{Outcome, StepResult};
pub use page_catalog::{PAGE_SCENARIOS, PageScenario};

use crate::browsers::BrowserKind;

/// Проверка всех страниц стенда в чистом браузере (без экстеншенов стенда).
pub fn run_pages(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    pages::run(stand, browser)
}

/// Дымок: одна страница (`mse-fmp4`) путём `capture` — настоящий экстеншен, посредник, плеер.
pub fn run_smoke(stand: &Stand, browser: BrowserKind) -> StepResult {
    capture::run_page(stand, browser, "smoke", capture::SMOKE_PAGE)
}

/// Все MSE-страницы стенда: «отдать вкладку» → кадры в плеере, браузер не декодирует.
pub fn run_capture(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    capture::run_all(stand, browser)
}

/// Команды плеера (пауза, play, перемотка, закрытие окна) доходят до страницы под тенью.
pub fn run_player_commands(stand: &Stand, browser: BrowserKind) -> StepResult {
    player_commands::run(stand, browser)
}

/// Попап настоящего экстеншена показывает «Connected to Fastiplayer».
pub fn run_extension_status(stand: &Stand, browser: BrowserKind) -> StepResult {
    extension_status::run(stand, browser)
}
