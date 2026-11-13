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
mod auto_handoff;
mod blocker_matrix;
mod capture;
#[cfg(test)]
mod capture_pages_contract;
mod codec_cases;
mod codecs;
mod context;
mod entries;
mod episodes;
mod evidence;
mod extension_status;
mod handoff_run;
mod hls_live;
mod live_ends;
mod live_unload;
mod outcome;
mod page_catalog;
mod page_ends;
mod page_filter;
mod page_reload;
mod page_return;
mod page_subtitles;
mod pages;
mod player_commands;
mod quality;
mod replay;
mod run_tweaks;
mod series;
mod series_memory;
mod tab_return;

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

/// Заглушка поверх видео, «Return» и честные концы (11a, решения 236–238) настоящей мышью
/// (все строки или строки выбранных `--page`).
pub fn run_page_return(
    stand: &Stand,
    browser: BrowserKind,
    filter: &PageFilter,
) -> Vec<StepResult> {
    page_return::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `page-return` (для проверки `--page` в командной строке).
pub fn page_return_page_ids() -> Vec<&'static str> {
    page_return::page_ids()
}

/// Возврат закрытой вкладки в тот же сеанс (11b2, решения 240–241): страница уходит, play в окне
/// открывает её заново (все строки или страница из `--page`).
pub fn run_tab_return(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    tab_return::run_all(stand, browser, filter)
}

/// Перезагрузка и переход страницы в той же вкладке (16c, решения 59, 304, 305): замена в том же окне
/// или честный конец (все строки или страница из `--page`).
pub fn run_page_reload(
    stand: &Stand,
    browser: BrowserKind,
    filter: &PageFilter,
) -> Vec<StepResult> {
    page_reload::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `page-reload` (для проверки `--page` в командной строке).
pub fn page_reload_page_ids() -> Vec<&'static str> {
    page_reload::page_ids()
}

/// Идентификаторы страниц `tab-return` (для проверки `--page` в командной строке).
pub fn tab_return_page_ids() -> Vec<&'static str> {
    tab_return::page_ids()
}

/// Режим 2 «автозапуск» (12a): настоящий клик человека по Play уводит видео в плеер сам, без кнопки
/// «Отдать»; фильтры (звук, длина, правила сайта) и цепочки (реклама → фильм, «Return», три вкладки)
/// — настоящей мышью (все строки или строки выбранных `--page`).
pub fn run_auto_handoff(
    stand: &Stand,
    browser: BrowserKind,
    filter: &PageFilter,
) -> Vec<StepResult> {
    auto_handoff::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `auto-handoff` (для проверки `--page` в командной строке).
pub fn auto_handoff_page_ids() -> Vec<&'static str> {
    auto_handoff::page_ids()
}

/// Родной HLS LIVE под тенью (10a3): эфир идёт, страница видит `duration = Infinity` и едущее окно
/// `seekable`, после закрытия окна плеера элемент стоит у края эфира.
pub fn run_hls_live(stand: &Stand, browser: BrowserKind) -> StepResult {
    hls_live::run(stand, browser)
}

/// Эфир, который становится записью (16d1): MSE (страница ставит конечную длину и `endOfStream()`) и
/// родной HLS (`ENDLIST` в плейлисте); плеер доигрывает запись, страница видит конечную длину и `ended`.
pub fn run_live_ends(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    live_ends::run(stand, browser)
}

/// Субтитры страницы (13b2): `<track>`, `fetch`, XHR и `addTextTrack` доходят до плеера, слой
/// субтитров лёг на кадры (все страницы или выбранные `--page`).
pub fn run_page_subtitles(
    stand: &Stand,
    browser: BrowserKind,
    filter: &PageFilter,
) -> Vec<StepResult> {
    page_subtitles::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `page-subtitles` (для проверки `--page` в командной строке).
pub fn page_subtitles_page_ids() -> Vec<&'static str> {
    page_subtitles::page_ids()
}

/// Смена качества на странице (13c): окно плеера показывает новое качество, кадры растут
/// (страница `mse-abr`, 360p -> 720p).
pub fn run_quality(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    quality::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `quality` (для проверки `--page` в командной строке).
pub fn quality_page_ids() -> Vec<&'static str> {
    quality::page_ids()
}

/// Сценарий `series` (сессия 14b): адаптер страницы «сериал» — меню вариантов, смена озвучки и
/// качества из нашего меню, Next, сломанный адаптер.
pub fn run_series(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    series::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `series` (для проверки `--page` в командной строке).
pub fn series_page_ids() -> Vec<&'static str> {
    series::page_ids()
}

/// Пауза между сериями и уход одностраничника (16a, решения 303-309): серия 2 приходит через ~20 с,
/// не приходит вовсе, вкладка спрятана, сайт ушёл `pushState`-ом (все строки или строки выбранных `--page`).
pub fn run_episodes(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    episodes::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `episodes` (для проверки `--page` в командной строке).
pub fn episodes_page_ids() -> Vec<&'static str> {
    episodes::page_ids()
}

/// Сценарии `series-memory-*` (сессия 14b2): память выбора зрителя — перезапуск браузера на том же
/// профиле, «забыть», приватное окно.
pub fn run_series_memory(
    stand: &Stand,
    browser: BrowserKind,
    filter: &PageFilter,
) -> Vec<StepResult> {
    series_memory::run_all(stand, browser, filter)
}

/// Идентификаторы страниц `series-memory` (для проверки `--page` в командной строке).
pub fn series_memory_page_ids() -> Vec<&'static str> {
    series_memory::page_ids()
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
