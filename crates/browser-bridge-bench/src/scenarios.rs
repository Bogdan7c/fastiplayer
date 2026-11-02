//! Сценарии прогона стенда: проверка страниц в чистом браузере, дымок «байты до рендера»,
//! статус настоящего экстеншена.
//!
//! Сценарии НИЧЕГО не печатают и не знают про командную строку: каждый возвращает
//! типизированные [`StepResult`], а вывод таблицы и код выхода делает бинарник (`report`).
//! Любая ошибка стенда внутри сценария превращается в `Fail` с текстом ошибки, поэтому
//! сбой одного браузера не прерывает прогон остальных.

mod context;
mod extension_status;
mod outcome;
mod page_catalog;
mod pages;
mod smoke;
pub mod stand_extension;

pub use context::{BrowserExecutables, RunSettings, ScenarioError, Stand};
pub use outcome::{Outcome, StepResult};
pub use page_catalog::{PAGE_SCENARIOS, PageScenario};
pub use stand_extension::{StandExtension, StandExtensionError};

use crate::browsers::BrowserKind;

/// Проверка всех страниц стенда в чистом браузере (без экстеншенов стенда).
pub fn run_pages(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    pages::run(stand, browser)
}

/// Дымок: стендовый перехватчик + настоящий посредник + настоящий плеер.
pub fn run_smoke(stand: &Stand, browser: BrowserKind) -> StepResult {
    smoke::run(stand, browser)
}

/// Попап настоящего экстеншена показывает «Connected to Fastiplayer».
pub fn run_extension_status(stand: &Stand, browser: BrowserKind) -> StepResult {
    extension_status::run(stand, browser)
}
