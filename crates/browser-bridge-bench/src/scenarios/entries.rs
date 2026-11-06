//! `entries` (10b): матрица «вход × страница» на НАСТОЯЩЕЙ мыши и клавиатуре.
//!
//! Человек входит в мост рукой: кликает по значку экстеншена на панели браузера, жмёт Tab и
//! Enter в попапе. Стенд делает то же самое: настоящий клик через `fake_input` невидимого kwin
//! (`desktop_input`), значок находит по снимку экрана (`screen_capture`), а передачу вкладки
//! НЕ вызывает сам (`hand_off_tab` в этой команде не зовётся). Каждая строка доказывается
//! по журналам плеера, посредника и сервера, а не по факту клика: клик только запускает цепь.
//!
//! Вход — перечисление [`Entry`]: сессия 10c добавит правый клик и ссылку на странице новыми
//! значениями, и каждая строка матрицы прогонится и для них.
//!
//! Строки:
//! - `start_rows` — вход на странице с видео: кадры в рендере, браузер не декодирует, попап
//!   закрылся сам; вход посреди проигрывания;
//! - `choice_row` — выбор другого видео в попапе (`banner-with-main`);
//! - `refusal_rows` — отказы текстом (`no_video`, `protected`, `not_supported_yet`) и страница
//!   браузера (`tab_not_scriptable`);
//! - `consent_rows` — вопрос про локальный адрес: Tab и Enter на Allow / Don't allow.

mod choice_row;
mod common;
mod consent_rows;
mod refusal_rows;
mod robot;
mod start_rows;

use std::time::Instant;

use super::capture::{CapturePage, plain_page};
use super::context::{ScenarioError, Stand};
use super::outcome::StepResult;
use super::page_filter::PageFilter;
use crate::browsers::BrowserKind;
use crate::environment::{DesktopSetup, PlayerConfigProfile};
use robot::Robot;

/// Чем человек входит в мост.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Entry {
    /// Кнопка экстеншена на панели браузера (этот вход есть в 10b).
    ToolbarButton,
}

impl Entry {
    /// Все входы матрицы.
    const ALL: &'static [Entry] = &[Entry::ToolbarButton];

    /// Короткое имя для строки таблицы.
    fn name(self) -> &'static str {
        match self {
            Self::ToolbarButton => "toolbar-button",
        }
    }

    /// Совершает вход настоящей рукой: после него попап (или другое меню входа) открыт.
    fn activate(self, robot: &Robot<'_>) -> Result<(), ScenarioError> {
        match self {
            Self::ToolbarButton => robot.click_toolbar_button(),
        }
    }
}

/// Что проверяет строка матрицы.
#[derive(Debug, Clone, Copy)]
enum RowKind {
    /// Вход на странице с видео → кадры в плеере, браузер не декодирует.
    Start(CapturePage),
    /// Страница играет несколько секунд, потом вход: плеер стартует с текущей секунды.
    LateStart(CapturePage),
    /// Выбор другого видео в попапе, когда главное уже в плеере.
    Choice(&'static str),
    /// Отказ текстом: попап остаётся, сеанса нет.
    Refusal(refusal_rows::RefusalRow),
    /// Вопрос про локальный адрес: Allow или Don't allow настоящими клавишами.
    Consent(consent_rows::Decision),
    /// Страница самого браузера: вкладку нельзя читать.
    BrowserPage,
}

/// Строка матрицы: страница (для `--page`) и правило.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// Идентификатор страницы стенда; у страницы браузера — условное имя.
    page_id: &'static str,
    kind: RowKind,
}

/// Условное имя строки «страница самого браузера» (её нет среди страниц стенда).
const BROWSER_PAGE_ID: &str = "browser-page";

/// Строки матрицы. Порядок: от простого входа к отказам и вопросам.
const ROWS: &[Row] = &[
    start("mse-fmp4"),
    start("mse-webm"),
    start("video-src"),
    start("iframe-cross-origin"),
    start("shadow-dom"),
    Row {
        page_id: "mse-fmp4",
        kind: RowKind::LateStart(plain_page("mse-fmp4")),
    },
    Row {
        page_id: "banner-with-main",
        kind: RowKind::Choice("banner-with-main"),
    },
    refusal("banner-autoplay", refusal_rows::RefusalRow::NO_VIDEO),
    refusal("drm-clearkey", refusal_rows::RefusalRow::PROTECTED),
    refusal("native-hls", refusal_rows::RefusalRow::NATIVE_HLS),
    refusal("m3u8-in-src", refusal_rows::RefusalRow::M3U8_IN_SRC),
    Row {
        page_id: "video-src-local",
        kind: RowKind::Consent(consent_rows::Decision::Allow),
    },
    Row {
        page_id: "video-src-local",
        kind: RowKind::Consent(consent_rows::Decision::Deny),
    },
    Row {
        page_id: BROWSER_PAGE_ID,
        kind: RowKind::BrowserPage,
    },
];

const fn start(page_id: &'static str) -> Row {
    Row {
        page_id,
        kind: RowKind::Start(plain_page(page_id)),
    }
}

const fn refusal(page_id: &'static str, expected: refusal_rows::RefusalRow) -> Row {
    Row {
        page_id,
        kind: RowKind::Refusal(expected),
    }
}

/// Идентификаторы страниц `entries` (для проверки `--page` в командной строке).
pub(super) fn entries_page_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for row in ROWS {
        if row.page_id != BROWSER_PAGE_ID && !ids.contains(&row.page_id) {
            ids.push(row.page_id);
        }
    }
    ids
}

/// Прогон матрицы одного браузера: каждая строка в свежем окружении (свой kwin, плеер, журналы).
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let mut results = Vec::new();
    for &entry in Entry::ALL {
        for row in ROWS.iter().filter(|row| admitted(row, filter)) {
            results.push(run_row(stand, browser, entry, *row));
        }
    }
    results
}

/// Строка проходит через `--page`; страница браузера идёт только в полном прогоне.
fn admitted(row: &Row, filter: &PageFilter) -> bool {
    if row.page_id == BROWSER_PAGE_ID {
        return filter.selects_everything();
    }
    filter.admits(row.page_id)
}

/// Подпись строки таблицы: вход, страница, правило.
fn subject(entry: Entry, row: &Row) -> String {
    let rule = match row.kind {
        RowKind::Start(_) => "start",
        RowKind::LateStart(_) => "start-after-playing",
        RowKind::Choice(_) => "choose-another-video",
        RowKind::Refusal(refusal) => refusal.name(),
        RowKind::Consent(decision) => decision.name(),
        RowKind::BrowserPage => "tab-not-scriptable",
    };
    format!("{} x {} [{rule}]", entry.name(), row.page_id)
}

fn run_row(stand: &Stand, browser: BrowserKind, entry: Entry, row: Row) -> StepResult {
    let started = Instant::now();
    // Мышиному сценарию нужен большой экран и право на настоящий ввод; ответа «ask» для
    // локального адреса плеер не касается, поэтому профиль настроек общий.
    let verdict = stand.with_environment_on_desktop(
        PlayerConfigProfile::Clean,
        DesktopSetup::pointer_stand(),
        |environment| match row.kind {
            RowKind::Start(page) => start_rows::run_start(stand, environment, browser, entry, page),
            RowKind::LateStart(page) => {
                start_rows::run_late_start(stand, environment, browser, entry, page)
            }
            RowKind::Choice(page_id) => {
                choice_row::run(stand, environment, browser, entry, page_id)
            }
            RowKind::Refusal(refusal) => {
                refusal_rows::run_refusal(stand, environment, browser, entry, row.page_id, refusal)
            }
            RowKind::Consent(decision) => {
                consent_rows::run(stand, environment, browser, entry, decision)
            }
            RowKind::BrowserPage => {
                refusal_rows::run_browser_page(stand, environment, browser, entry)
            }
        },
    );
    StepResult::new(
        "entries",
        browser,
        subject(entry, &row),
        verdict,
        started.elapsed(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    #[test]
    fn every_row_page_is_a_known_stand_page() {
        for id in entries_page_ids() {
            assert!(
                PAGE_SCENARIOS.iter().any(|known| known.id == id),
                "{id} is not a stand page"
            );
        }
    }

    #[test]
    fn the_matrix_names_every_pair_from_the_assignment() {
        let ids = entries_page_ids();
        for required in [
            "mse-fmp4",
            "mse-webm",
            "video-src",
            "iframe-cross-origin",
            "shadow-dom",
            "banner-with-main",
            "banner-autoplay",
            "drm-clearkey",
            "native-hls",
            "m3u8-in-src",
            "video-src-local",
        ] {
            assert!(ids.contains(&required), "{required} is missing from ROWS");
        }
        assert!(ROWS.iter().any(|row| row.page_id == BROWSER_PAGE_ID));
    }

    #[test]
    fn subjects_are_unique_so_the_table_names_every_pair() {
        let mut seen = std::collections::HashSet::new();
        for row in ROWS {
            assert!(
                seen.insert(subject(Entry::ToolbarButton, row)),
                "duplicate subject for {}",
                row.page_id
            );
        }
    }

    #[test]
    fn the_browser_page_row_runs_only_without_a_page_filter() {
        let browser_page = ROWS
            .iter()
            .find(|row| row.page_id == BROWSER_PAGE_ID)
            .expect("row exists");
        assert!(admitted(browser_page, &PageFilter::all()));
        assert!(!admitted(
            browser_page,
            &PageFilter::only(["mse-fmp4".to_owned()])
        ));
    }
}
