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
//! - `consent_rows` — вопрос про локальный адрес: Tab и Enter на Allow / Don't allow;
//! - `late_rows` — поздний вход (10d): вкладка открыта раньше экстеншена, кнопка «Reload and open».

mod already_open_rows;
mod choice_row;
mod common;
mod consent_rows;
mod disabled_rows;
mod entry_evidence;
mod extension_probe;
mod late_rows;
mod link_rows;
mod menu_entry;
mod menu_matrix;
mod native_menu;
mod overlay_rows;
mod page_geometry;
mod pick_rows;
mod refusal_rows;
mod report_rows;
pub(super) mod return_hand;
mod robot;
mod start_rows;

use std::time::Instant;

use super::capture::{CapturePage, plain_page};
use super::context::{ScenarioError, Stand};
use super::outcome::StepResult;
use super::page_filter::PageFilter;
use crate::browsers::BrowserKind;
use crate::environment::{DesktopSetup, PlayerConfigProfile};

/// Чем человек входит в мост.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Entry {
    /// Кнопка экстеншена на панели браузера (10b).
    ToolbarButton,
    /// Правый клик по `<video>` и пункт родного меню «Play in Fastiplayer» (10c).
    VideoMenu,
    /// Правый клик по странице вокруг видео и пункт «Play video from this page…» (10c).
    PageMenu,
    /// Правый клик по ссылке и пункт «Play link in Fastiplayer» (10c).
    LinkMenu,
    /// Кнопка поверх видео при наведении (10c).
    OverlayButton,
}

impl Entry {
    /// Входы общей матрицы `ROWS` (остальные входы имеют свои строки в `menu_matrix`).
    const ALL: &'static [Entry] = &[Entry::ToolbarButton];

    /// Короткое имя для строки таблицы.
    fn name(self) -> &'static str {
        match self {
            Self::ToolbarButton => "toolbar-button",
            Self::VideoMenu => "video-menu",
            Self::PageMenu => "page-menu",
            Self::LinkMenu => "link-menu",
            Self::OverlayButton => "overlay-button",
        }
    }

    /// После входа открыт попап значка (иначе итог человеку показывает плашка на странице).
    fn opens_popup(self) -> bool {
        matches!(self, Self::ToolbarButton)
    }

    /// Совершает вход настоящей рукой: после него попап (или другое меню входа) открыт.
    fn activate(
        self,
        ready: &mut common::ReadyRun<'_, '_>,
        spot: menu_entry::ClickSpot,
    ) -> Result<(), ScenarioError> {
        match self {
            Self::ToolbarButton => ready.robot.click_toolbar_button(),
            Self::VideoMenu | Self::PageMenu | Self::LinkMenu => ready.pick_from_native_menu(spot),
            Self::OverlayButton => ready.press_overlay_button(spot),
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
    /// Поздний вход (10d): вкладка открыта раньше экстеншена.
    Late(late_rows::LateRow),
}

/// Строка матрицы: страница (для `--page`) и правило.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// Идентификатор страницы стенда; у страницы браузера — условное имя.
    page_id: &'static str,
    kind: RowKind,
}

/// Переменная окружения для отладки: прогонять только строки, в подписи которых есть этот текст
/// (`BRIDGE_BENCH_ENTRY_ROW=video-menu`). Без неё идут все строки, отобранные `--page`.
const SUBJECT_FILTER_VARIABLE: &str = "BRIDGE_BENCH_ENTRY_ROW";

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
    // 10a2: m3u8 прямо в `src` больше не отказ, а обычный вход (родной HLS VOD уходит в плеер);
    // отказом `not_supported_yet` теперь служит `blob:`-файл.
    start("m3u8-in-src"),
    refusal("blob-file", refusal_rows::RefusalRow::BLOB_FILE),
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
    late(late_rows::LateRow::LateInstall),
    late(late_rows::LateRow::LateUpdate),
    late(late_rows::LateRow::TypedText),
    late(late_rows::LateRow::PressPlay),
    late(late_rows::LateRow::SiteRefusesSecond),
    late(late_rows::LateRow::AdThenFilm),
];

/// Строка позднего входа; страницу ей назначает сама строка.
const fn late(row: late_rows::LateRow) -> Row {
    Row {
        page_id: row.page_id(),
        kind: RowKind::Late(row),
    }
}

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
    for row in menu_matrix::MENU_ROWS {
        if !ids.contains(&row.page_id) {
            ids.push(row.page_id);
        }
    }
    ids
}

/// Прогон матрицы одного браузера: каждая строка в свежем окружении (свой kwin, плеер, журналы).
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let mut results = Vec::new();
    let subject_filter = std::env::var(SUBJECT_FILTER_VARIABLE).ok();
    let wanted = |subject: &str| {
        subject_filter
            .as_deref()
            .is_none_or(|part| subject.contains(part))
    };
    for &entry in Entry::ALL {
        for row in ROWS.iter().filter(|row| admitted(row, filter)) {
            if wanted(&subject(entry, row)) {
                results.push(run_row(stand, browser, entry, *row));
            }
        }
    }
    for row in menu_matrix::MENU_ROWS
        .iter()
        .filter(|row| filter.admits(row.page_id))
    {
        if wanted(&menu_matrix::subject(row)) {
            results.push(run_menu_row(stand, browser, *row));
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
        RowKind::Late(late) => late.name(),
    };
    format!("{} x {} [{rule}]", entry.name(), row.page_id)
}

/// Строка матрицы входов 10c в свежем окружении.
fn run_menu_row(stand: &Stand, browser: BrowserKind, row: menu_matrix::MenuRow) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment_on_desktop(
        PlayerConfigProfile::Clean,
        DesktopSetup::pointer_stand(),
        |environment| menu_matrix::run(stand, environment, browser, row),
    );
    StepResult::new(
        "entries",
        browser,
        menu_matrix::subject(&row),
        verdict,
        started.elapsed(),
    )
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
            RowKind::Late(late) => late_rows::run(stand, environment, browser, late),
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
            "blob-file",
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
    fn the_matrix_runs_every_late_row_exactly_once() {
        for late in late_rows::LateRow::ALL {
            let count = ROWS
                .iter()
                .filter(|row| matches!(row.kind, RowKind::Late(found) if found == *late))
                .count();
            assert_eq!(count, 1, "{late:?}");
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
