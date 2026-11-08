//! `page-return` (сессия 11a, решения 236–238): заглушка «Playing in Fastiplayer [Return]» поверх видео
//! страницы, возврат видео странице и честные концы — настоящей мышью на невидимом экране.
//!
//! Что доказывает каждая строка (подробности — в модулях строк):
//!  1. `placeholder-geometry` — заглушка ровно над видео (± 2 px), клик по ней до видео не доходит,
//!     после прокрутки она едет за видео, с концом сеанса уходит;
//!  2. `return-click` — «Return» настоящим кликом: страница играет сама со своей секунды N ± 1 с,
//!     браузер снова декодирует, окно плеера закрылось сразу и молча (4010), нового сеанса нет;
//!  3. `close-window` — закрытие окна (`Quit`): страница на паузе на секунде закрытия, потом играет сама;
//!  4. `replace-from-second-tab` — вторая вкладка заменяет первую: первая на паузе на своей секунде,
//!     вторая показывает кадры в том же окне;
//!  5. `race-*` — «Return» и закрытие окна почти разом (оба порядка) на видео без родных кнопок
//!     (`controls=0`, см. `rows_return::race_page_query`): ровно один возврат, без дёрганья;
//!  6. `works-after-return` — после «Return» своя перемотка, смена качества и следующая серия идут
//!     силами браузера, нового сеанса нет;
//!  7. `return-click` на страницах со своей шкалой (смещённое начало, вшитая реклама) — та же секунда страницы;
//!  8. `live-edge` — живой эфир HLS (Chromium) возвращается к краю эфира;
//!  9. `page-player-failed*` — плеер страницы сломался: доигрывание и `cause=page_player_failed`;
//! 10. `link-return` — «Return» на вкладке входа по ссылке: страница играет, вкладка остаётся.
//!
//! Судьи чистые (`judge`), факты страницы пишет `site/lib/return-checks.js`, разбор — `facts`.

mod facts;
mod judge;
mod rows_after;
mod rows_ends;
mod rows_return;
mod steps;

use std::time::Instant;

use self::rows_ends::PageBreak;
use self::rows_return::RaceOrder;
use self::steps::RETURN_KIT_PAGE;
use super::context::{ScenarioError, Stand};
use super::hls_live::HLS_LIVE_PAGE;
use super::outcome::{StepResult, Verdict};
use super::page_filter::PageFilter;
use crate::browsers::BrowserKind;
use crate::environment::{BenchEnvironment, DesktopSetup, PlayerConfigProfile};

/// Переменная окружения для отладки: прогонять только строки, в подписи которых есть этот текст
/// (`BRIDGE_BENCH_RETURN_ROW=race`). Без неё идут все строки, отобранные `--page`.
const SUBJECT_FILTER_VARIABLE: &str = "BRIDGE_BENCH_RETURN_ROW";
/// Страница со смещённым началом потока (600 с) и страница со вшитой рекламой.
const OFFSET_TIMELINE_PAGE: &str = "return-offset";
const BAKED_AD_TIMELINE_PAGE: &str = "return-baked-ad";
/// Страница-источник входа по ссылке (у строки `link-return`).
const LINK_SOURCE_PAGE: &str = "link-source";

/// Что проверяет строка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReturnRow {
    PlaceholderGeometry,
    ReturnClick,
    CloseWindow,
    ReplaceFromSecondTab,
    Race(RaceOrder),
    WorksAfterReturn,
    LiveEdge,
    PagePlayerFailed(PageBreak),
    LinkReturn,
}

impl ReturnRow {
    fn name(self) -> &'static str {
        match self {
            Self::PlaceholderGeometry => "placeholder-geometry",
            Self::ReturnClick => "return-click",
            Self::CloseWindow => "close-window",
            Self::ReplaceFromSecondTab => "replace-from-second-tab",
            Self::Race(RaceOrder::ReturnThenClose) => "race-return-then-close",
            Self::Race(RaceOrder::CloseThenReturn) => "race-close-then-return",
            Self::WorksAfterReturn => "works-after-return",
            Self::LiveEdge => "live-edge",
            Self::PagePlayerFailed(PageBreak::DecodeError) => "page-player-failed",
            Self::PagePlayerFailed(PageBreak::DecodeErrorThenDrop) => {
                "page-player-failed-then-drop"
            }
            Self::LinkReturn => "link-return",
        }
    }
}

/// Строка таблицы: страница (для `--page`) и правило.
#[derive(Debug, Clone, Copy)]
struct Row {
    page_id: &'static str,
    kind: ReturnRow,
}

const fn row(page_id: &'static str, kind: ReturnRow) -> Row {
    Row { page_id, kind }
}

/// Строки по порядку: от заглушки к возвратам, гонке, жизни после возврата и честным концам.
const ROWS: &[Row] = &[
    row(RETURN_KIT_PAGE, ReturnRow::PlaceholderGeometry),
    row(RETURN_KIT_PAGE, ReturnRow::ReturnClick),
    row(RETURN_KIT_PAGE, ReturnRow::CloseWindow),
    row(RETURN_KIT_PAGE, ReturnRow::ReplaceFromSecondTab),
    row(RETURN_KIT_PAGE, ReturnRow::Race(RaceOrder::ReturnThenClose)),
    row(RETURN_KIT_PAGE, ReturnRow::Race(RaceOrder::CloseThenReturn)),
    row(RETURN_KIT_PAGE, ReturnRow::WorksAfterReturn),
    row(OFFSET_TIMELINE_PAGE, ReturnRow::ReturnClick),
    row(BAKED_AD_TIMELINE_PAGE, ReturnRow::ReturnClick),
    row(HLS_LIVE_PAGE, ReturnRow::LiveEdge),
    row(
        RETURN_KIT_PAGE,
        ReturnRow::PagePlayerFailed(PageBreak::DecodeError),
    ),
    row(
        RETURN_KIT_PAGE,
        ReturnRow::PagePlayerFailed(PageBreak::DecodeErrorThenDrop),
    ),
    row(LINK_SOURCE_PAGE, ReturnRow::LinkReturn),
];

/// Подпись строки таблицы: правило и страница.
fn subject(row: &Row) -> String {
    format!("{} x {}", row.kind.name(), row.page_id)
}

/// Идентификаторы страниц сценария (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for row in ROWS {
        if !ids.contains(&row.page_id) {
            ids.push(row.page_id);
        }
    }
    ids
}

/// Все строки одного браузера, каждая в свежем окружении с правом на настоящий ввод.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let subject_filter = std::env::var(SUBJECT_FILTER_VARIABLE).ok();
    ROWS.iter()
        .filter(|row| filter.admits(row.page_id))
        .filter(|row| {
            subject_filter
                .as_deref()
                .is_none_or(|part| subject(row).contains(part))
        })
        .map(|row| run_row(stand, browser, *row))
        .collect()
}

fn run_row(stand: &Stand, browser: BrowserKind, row: Row) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment_on_desktop(
        PlayerConfigProfile::Clean,
        DesktopSetup::pointer_stand(),
        |environment| drive(stand, environment, browser, row),
    );
    StepResult::new(
        "page-return",
        browser,
        subject(&row),
        verdict,
        started.elapsed(),
    )
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: Row,
) -> Result<Verdict, ScenarioError> {
    match row.kind {
        ReturnRow::PlaceholderGeometry => {
            rows_return::run_geometry(stand, environment, browser, row.page_id)
        }
        ReturnRow::ReturnClick => {
            rows_return::run_return_click(stand, environment, browser, row.page_id)
        }
        ReturnRow::CloseWindow => {
            rows_return::run_close_window(stand, environment, browser, row.page_id)
        }
        ReturnRow::ReplaceFromSecondTab => rows_after::run_replace(stand, environment, browser),
        ReturnRow::Race(order) => {
            rows_return::run_race(stand, environment, browser, row.page_id, order)
        }
        ReturnRow::WorksAfterReturn => {
            rows_after::run_works_after_return(stand, environment, browser)
        }
        ReturnRow::LiveEdge => rows_after::run_live_edge(stand, environment, browser, row.page_id),
        ReturnRow::PagePlayerFailed(page_break) => {
            rows_ends::run_page_player_failed(stand, environment, browser, page_break)
        }
        ReturnRow::LinkReturn => rows_ends::run_link_return(stand, environment, browser),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    fn page_source(id: &str) -> String {
        let site = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site");
        let mut source =
            std::fs::read_to_string(site.join(format!("pages/{id}.html"))).expect("page");
        for lib in [
            "lib/return-checks.js",
            "lib/handoff-mode.js",
            "lib/hls-live-page.js",
        ] {
            source.push_str(&std::fs::read_to_string(site.join(lib)).expect("lib"));
        }
        source
    }

    #[test]
    fn every_row_page_is_a_known_stand_page_and_subjects_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for row in ROWS {
            assert!(
                PAGE_SCENARIOS.iter().any(|page| page.id == row.page_id),
                "{} is not a stand page",
                row.page_id
            );
            assert!(
                seen.insert(subject(row)),
                "duplicate subject {}",
                subject(row)
            );
        }
    }

    #[test]
    fn the_table_covers_every_row_of_the_assignment_in_both_race_orders() {
        let kinds: Vec<ReturnRow> = ROWS.iter().map(|row| row.kind).collect();
        for wanted in [
            ReturnRow::PlaceholderGeometry,
            ReturnRow::ReturnClick,
            ReturnRow::CloseWindow,
            ReturnRow::ReplaceFromSecondTab,
            ReturnRow::Race(RaceOrder::ReturnThenClose),
            ReturnRow::Race(RaceOrder::CloseThenReturn),
            ReturnRow::WorksAfterReturn,
            ReturnRow::LiveEdge,
            ReturnRow::PagePlayerFailed(PageBreak::DecodeError),
            ReturnRow::PagePlayerFailed(PageBreak::DecodeErrorThenDrop),
            ReturnRow::LinkReturn,
        ] {
            assert!(kinds.contains(&wanted), "{wanted:?} is missing");
        }
        // Обе «свои шкалы» проходят тот же «Return».
        for timeline in [OFFSET_TIMELINE_PAGE, BAKED_AD_TIMELINE_PAGE] {
            assert!(
                ROWS.iter()
                    .any(|row| row.page_id == timeline && row.kind == ReturnRow::ReturnClick)
            );
        }
    }

    /// Команды робота — те, что объявили страницы и их библиотеки: иначе стенд упал бы только вживую.
    #[test]
    fn every_command_the_robot_sends_is_declared_by_the_page() {
        let kit = page_source(RETURN_KIT_PAGE);
        for hook in [
            "scroll-down",
            "own-seek",
            "quality-switch",
            "next-episode",
            "break-mse",
            "break-mse-then-drop",
        ] {
            assert!(
                kit.contains(&format!("'{hook}'")),
                "return-kit does not declare the hook '{hook}'"
            );
        }
        for command in [
            "mark-return",
            "observe-return",
            "count-video-clicks",
            "report-video-clicks",
            "expect-returned",
        ] {
            assert!(
                kit.contains(&format!("'{command}'")),
                "the hand-off libraries do not declare '{command}'"
            );
        }
        let live = page_source(HLS_LIVE_PAGE);
        for hook in ["mark-before-return", "expect-returned-live"] {
            assert!(
                live.contains(&format!("'{hook}'")),
                "hls-live does not declare '{hook}'"
            );
        }
    }

    #[test]
    fn page_ids_list_every_row_page_once() {
        let ids = page_ids();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len()
        );
        for page in [
            RETURN_KIT_PAGE,
            OFFSET_TIMELINE_PAGE,
            BAKED_AD_TIMELINE_PAGE,
            HLS_LIVE_PAGE,
            LINK_SOURCE_PAGE,
        ] {
            assert!(ids.contains(&page), "{page}");
        }
    }
}
