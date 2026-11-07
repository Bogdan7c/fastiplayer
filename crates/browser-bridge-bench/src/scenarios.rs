//! Сценарии прогона стенда: проверка страниц в чистом браузере, дымок и `capture`
//! («отдать вкладку» → кадры в плеере, браузер не декодирует), `page-ends` (страница сама
//! заканчивает передачу — плеер честно это показывает), команды плеера, статус настоящего
//! экстеншена.
//!
//! Сценарии НИЧЕГО не печатают и не знают про командную строку: каждый возвращает
//! типизированные [`StepResult`], а вывод таблицы и код выхода делает бинарник (`report`).
//! Любая ошибка стенда внутри сценария превращается в `Fail` с текстом ошибки, поэтому
//! сбой одного браузера не прерывает прогон остальных.

mod attacks;
mod blocker_matrix;
mod capture;
#[cfg(test)]
mod capture_pages_contract;
mod codec_cases;
mod codecs;
mod context;
mod entries;
mod evidence;
mod extension_status;
mod handoff_run;
mod hls_live;
mod live_unload;
mod outcome;
mod page_catalog;
mod page_ends;
mod page_filter;
mod pages;
mod player_commands;
mod replay;
mod run_tweaks;

pub use attacks::attack_page_ids;
pub use context::{BrowserExecutables, RunSettings, ScenarioError, Stand};
pub use outcome::{Outcome, StepResult};
pub use page_catalog::{PAGE_SCENARIOS, PageScenario};
pub use page_ends::end_page_ids;
pub use page_filter::PageFilter;
pub use replay::{DEFAULT_MIN_FRAMES, ReplayOptions};

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

/// Вредные страницы (гигантские сегменты, шквал событий, мусор, шквал сообщений воркера): после
/// каждой в том же браузере чистая страница обязана сыграть (все или выбранные `--page`).
pub fn run_attacks(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    attacks::run_all(stand, browser, filter)
}

/// Защита локального адреса (решение 199): блокировать, спрашивать, разрешить по согласию.
pub fn run_local_address(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    capture::run_local_address(stand, browser)
}

/// Матрица «вход × страница» на настоящей мыши и клавиатуре (10b): значок на панели, Tab и Enter
/// в попапе. Каждая строка доказана по журналам плеера, посредника и сервера.
pub fn run_entries(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    entries::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `entries` (для проверки `--page` в командной строке).
pub fn entries_page_ids() -> Vec<&'static str> {
    entries::entries_page_ids()
}

/// Матрица блокировщика рекламы: страницы × (без / блокировщик первым / наш экстеншен первым).
pub fn run_blocker_matrix(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    blocker_matrix::run(stand, browser)
}

/// Идентификаторы страниц `capture` (для проверки `--page` в командной строке).
pub fn capture_page_ids() -> Vec<&'static str> {
    capture::capture_page_ids()
}

/// Команды плеера (пауза, play, перемотка, закрытие окна) доходят до страницы под тенью;
/// страницы «живой эфир как Twitch» оживают после паузы и play зрителя (все или выбранные `--page`).
pub fn run_player_commands(
    stand: &Stand,
    browser: BrowserKind,
    filter: &PageFilter,
) -> Vec<StepResult> {
    player_commands::run(stand, browser, filter)
}

/// Идентификаторы страниц `player-commands` (для проверки `--page` в командной строке).
pub fn player_command_page_ids() -> Vec<&'static str> {
    player_commands::page_ids()
}

/// Родной HLS LIVE под тенью (10a3): эфир идёт, страница видит `duration = Infinity` и едущее окно
/// `seekable`, после закрытия окна плеера элемент стоит у края эфира.
pub fn run_hls_live(stand: &Stand, browser: BrowserKind) -> StepResult {
    hls_live::run(stand, browser)
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

/// Повтор записи потока страницы (`*.fprec`) с передачей плееру в записанный момент.
pub fn run_replay(stand: &Stand, browser: BrowserKind, options: &ReplayOptions) -> StepResult {
    replay::run(stand, browser, options)
}
