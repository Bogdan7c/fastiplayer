//! Окно плеера идёт за лентой коротких видео (сессия 16b2, решения 354–360).
//!
//! Человек отдал плееру один ролик ленты (Instagram, YouTube Shorts, TikTok) и листает ленту колесом
//! мыши. Когда ролик ушёл с экрана и страница сама его остановила (или убрала), окно плеера переезжает в
//! ТОМ ЖЕ окне на ролик, который страница запустит следующим. Если ролик просто уехал с экрана, но играет
//! (человек листает к комментариям), или остался на экране (превью рядом), окно остаётся на месте.
//!
//! Все руки настоящие: колесо и наведение мыши идут через `fake_input` невидимого kwin (стенд
//! `DesktopSetup::pointer_stand`). Ленту листает САМА страница по прокрутке, как настоящий сайт
//! (`site/pages/feed.html`, `feed-hover.html`); робот только отдаёт ролик 1 и крутит колесо.
//!
//! Строки таблицы [`FEED_ROWS`]: формы ленты (`many`, `moved`, `pool`, `halfway`) и два случая, где окно
//! НЕ переезжает. Приговор общий и чистый: [`judge::judge_feed`].
//!
//! Строка `feed-halfway-after-long-download` — вторая живая проверка Instagram 18 ноября 2026 («после
//! прокрутки нет мгновенного воспроизведения»): страница качает больше, чем мост помнит (64 МиБ на документ),
//! а следующий ролик обязан уйти плееру с начала и без ожидания (`site/lib/feed-history.js`).
//!
//! Строка `feed-halfway-after-passed-reels-detached` — замер во вкладке владельца 19 ноября 2026 (находка 3,
//! решение 364): Instagram держит живыми около 20 MediaSource, пройденные ролики сбрасывает пачками, и после
//! сброса самый старый живой MediaSource — текущий ролик, за ним следующий. Память моста обязана отдавать
//! начала дальних роликов, а не текущий и следующий.
//!
//! Форма `halfway` — живой Instagram 18 ноября 2026, без входа (находки живой проверки, решение 361): лента листается внутри
//! блока, страница переключает ролик на полпути прокрутки (прежний ещё на экране), а колесо человек
//! крутит прямо над роликом, то есть над нашей заглушкой «Playing in Fastiplayer».

mod back;
mod back_judge;
mod back_steps;
mod edge_judge;
mod edge_steps;
mod edges;
mod history_back;
mod history_back_judge;
mod judge;
mod steps;
mod switch_judge;

use std::time::Instant;

use super::super::context::{ScenarioError, Stand};
use super::super::entries::return_hand::{HandOpened, ReturnHand, open_hand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::super::handoff_run::HandoffRun;
use super::super::outcome::{StepResult, Verdict};
use super::super::page_filter::PageFilter;
use super::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES};
use crate::browsers::{BrowserError, BrowserKind, HandOffOutcome, hand_off_from_service_page};
use crate::environment::{BenchEnvironment, DesktopSetup, PlayerConfigProfile};
use crate::observe::{MEDIA_SWITCH_MARKER, read_host_evidence};
use crate::player_control::PlayerControl;
use back::{BACK_ROWS, BackScenario};
use back_judge::BackRecord;
use edge_judge::EdgeRecord;
use edges::{EDGE_ROWS, EdgeScenario};
use history_back::HISTORY_BACK_ROWS;
use history_back_judge::HistoryBackRecord;
use judge::{ExpectedWindow, FeedEvidence, ScrollRecord, StayRecord, judge_feed};
use steps::WheelPlace;

/// Ширины кадра трёх роликов ленты (ассеты `reel-1..3` каталога медиа: 640x360, 512x288, 384x216). По
/// строке журнала плеера о размере потока видно, какой ролик сейчас в окне.
const REEL_STREAM_WIDTHS: &[u64] = &[640, 512, 384];
/// Переменная окружения: гонять только строки, в имени шага которых есть эта подстрока (отладка одной
/// строки: `BRIDGE_BENCH_FEED_ROW=pool`).
const ROW_FILTER_VARIABLE: &str = "BRIDGE_BENCH_FEED_ROW";
/// Строка журнала плеера: окно показало зрителю сообщение о конце сеанса.
const END_NOTICE_MARKER: &str = "Browser session end notice shown";
/// Строка журнала плеера: новое поколение прождало данных у цели весь срок и встало там, где данные есть.
const TARGET_WAIT_MARKER: &str = "The new stream generation got no data at its target";

/// Что страница делает после передачи ролика 1, до первой прокрутки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeforeScrolling {
    /// Ничего: робот сразу листает.
    Nothing,
    /// Страница качает больше, чем помнит мост (параметр адреса `history=past-bridge-memory`).
    PageDownloadsPastBridgeMemory,
    /// Страница сбрасывает пройденные ролики и качает начала дальних сверх памяти моста (параметр адреса
    /// `history=passed-reels-detached`).
    PageDetachesPassedReelsAndPreloadsFarAhead,
}

impl BeforeScrolling {
    /// Нагрузка страницы, которую робот запускает после передачи (`None` — строка её не просит).
    fn page_load(self) -> Option<steps::PageLoad> {
        match self {
            Self::Nothing => None,
            Self::PageDownloadsPastBridgeMemory => Some(steps::DOWNLOAD_PAST_BRIDGE_MEMORY),
            Self::PageDetachesPassedReelsAndPreloadsFarAhead => {
                Some(steps::DETACH_PASSED_REELS_AND_PRELOAD_FAR_AHEAD)
            }
        }
    }

    /// Значение параметра адреса `history`, с которым страница умеет эту нагрузку (`None` — параметра нет).
    #[cfg(test)]
    fn history_query(self) -> Option<&'static str> {
        match self {
            Self::Nothing => None,
            Self::PageDownloadsPastBridgeMemory => Some("history=past-bridge-memory"),
            Self::PageDetachesPassedReelsAndPreloadsFarAhead => {
                Some("history=passed-reels-detached")
            }
        }
    }
}

/// Что делает строка с лентой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedScenario {
    /// Листаем колесом вперёд на `scrolls` роликов; окно обязано идти за лентой. `wheel` — где на
    /// странице стоит курсор, когда крутится колесо.
    Scroll { scrolls: usize, wheel: WheelPlace },
    /// Настоящая мышь наводится на превью рядом с главным видео; окно остаётся.
    HoverPreview,
    /// Колесо уводит главный ролик с экрана (страница его не останавливает), ниже заигрывает превью.
    ScrollToComments,
    /// Края ленты (16b3): быстрая серия, удаление элемента, уход в другой раздел, конец ленты (`edges.rs`).
    Edge(EdgeScenario),
    /// Прокрутка назад (16b3b): вперёд на ролик 2, потом колесом вверх к ролику 1 (`back.rs`).
    Back(BackScenario),
    /// «Назад» в истории после ухода из ленты (16b3b): подхват вернувшегося ролика вторым сеансом (`history_back.rs`).
    HistoryBack,
}

/// Строка таблицы: страница, адрес и сценарий.
#[derive(Debug, Clone, Copy)]
struct FeedRow {
    /// Имя шага в отчёте.
    step_id: &'static str,
    /// Страница стенда (`site/pages/<id>.html`).
    page_id: &'static str,
    /// Хвост адреса страницы (режим передачи и форма ленты).
    query_suffix: &'static str,
    scenario: FeedScenario,
    before_scrolling: BeforeScrolling,
}

/// Строки сценария. Каждая — отдельное прогонное окружение (свой плеер и браузер).
const FEED_ROWS: &[FeedRow] = &[
    // Instagram: свой элемент и MediaSource на ролик; пауза ушедшего, смена адреса, запуск следующего.
    FeedRow {
        step_id: "feed-many",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=many",
        scenario: FeedScenario::Scroll {
            scrolls: 2,
            wheel: WheelPlace::OverReel,
        },
        before_scrolling: BeforeScrolling::Nothing,
    },
    // YouTube Shorts: один элемент переезжает по секциям, новый MediaSource, паузы нет.
    FeedRow {
        step_id: "feed-moved",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=moved",
        scenario: FeedScenario::Scroll {
            scrolls: 1,
            wheel: WheelPlace::OverReel,
        },
        before_scrolling: BeforeScrolling::Nothing,
    },
    // TikTok: два элемента по очереди, прежний убирается из документа и ставится на паузу.
    FeedRow {
        step_id: "feed-pool",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=pool",
        scenario: FeedScenario::Scroll {
            scrolls: 2,
            wheel: WheelPlace::OverReel,
        },
        before_scrolling: BeforeScrolling::Nothing,
    },
    // Живой Instagram (18 ноября 2026, без входа): страница переключает ролик на полпути (прежний ещё на экране и потом
    // выглядывает полоской). Колесо — мимо ролика: строка проверяет только переезд окна.
    FeedRow {
        step_id: "feed-halfway",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=halfway",
        scenario: FeedScenario::Scroll {
            scrolls: 2,
            wheel: WheelPlace::BesideReel,
        },
        before_scrolling: BeforeScrolling::Nothing,
    },
    // То же, но колесо крутят над роликом, то есть над заглушкой: лента в блоке обязана листаться.
    FeedRow {
        step_id: "feed-halfway-over-plate",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=halfway",
        scenario: FeedScenario::Scroll {
            scrolls: 2,
            wheel: WheelPlace::OverReel,
        },
        before_scrolling: BeforeScrolling::Nothing,
    },
    // Вторая живая проверка Instagram: страница скачала больше, чем помнит мост, а следующий ролик, начало
    // которого она загрузила заранее, обязан заиграть в окне сразу и с начала.
    FeedRow {
        step_id: "feed-halfway-after-long-download",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=halfway&history=past-bridge-memory",
        scenario: FeedScenario::Scroll {
            scrolls: 1,
            wheel: WheelPlace::OverReel,
        },
        before_scrolling: BeforeScrolling::PageDownloadsPastBridgeMemory,
    },
    // Замер во вкладке владельца (находка 3): пройденные ролики страница сбросила пачкой, начала дальних
    // скачаны сверх памяти моста. Текущий и следующий ролики теперь самые старые в документе, а следующий
    // всё равно обязан заиграть в окне сразу и с начала.
    FeedRow {
        step_id: "feed-halfway-after-passed-reels-detached",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=halfway&history=passed-reels-detached",
        scenario: FeedScenario::Scroll {
            scrolls: 1,
            wheel: WheelPlace::OverReel,
        },
        before_scrolling: BeforeScrolling::PageDetachesPassedReelsAndPreloadsFarAhead,
    },
    // Превью по наведению рядом с главным видео: наш ролик на экране, переезда нет.
    FeedRow {
        step_id: "feed-hover-preview",
        page_id: "feed-hover",
        query_suffix: "&mode=handoff&variant=hover",
        scenario: FeedScenario::HoverPreview,
        before_scrolling: BeforeScrolling::Nothing,
    },
    // «Листаем к комментариям»: ролик ушёл с экрана, но играет; ниже заиграло превью. Переезда нет.
    FeedRow {
        step_id: "feed-scroll-to-comments",
        page_id: "feed-hover",
        query_suffix: "&mode=handoff&variant=comments",
        scenario: FeedScenario::ScrollToComments,
        before_scrolling: BeforeScrolling::Nothing,
    },
];

/// Все строки сценария: спокойная лента, затем края (16b3).
fn all_rows() -> impl Iterator<Item = &'static FeedRow> {
    FEED_ROWS
        .iter()
        .chain(EDGE_ROWS)
        .chain(BACK_ROWS)
        .chain(HISTORY_BACK_ROWS)
}

/// Идентификаторы страниц сценария (для `--page` и таблицы `capture`).
pub(in crate::scenarios) fn page_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for row in all_rows() {
        if !ids.contains(&row.page_id) {
            ids.push(row.page_id);
        }
    }
    ids
}

/// Все строки выбранных страниц одного браузера.
pub(in crate::scenarios) fn run_all(
    stand: &Stand,
    browser: BrowserKind,
    filter: &PageFilter,
) -> Vec<StepResult> {
    let row_filter = std::env::var(ROW_FILTER_VARIABLE).ok();
    all_rows()
        .filter(|row| filter.admits(row.page_id))
        .filter(|row| {
            row_filter
                .as_deref()
                .is_none_or(|part| row.step_id.contains(part))
        })
        .map(|row| run_row(stand, browser, *row))
        .collect()
}

fn run_row(stand: &Stand, browser: BrowserKind, row: FeedRow) -> StepResult {
    let started = Instant::now();
    // Настоящая мышь: большой невидимый экран с правом на ввод.
    let verdict = stand.with_environment_on_desktop(
        PlayerConfigProfile::Clean,
        DesktopSetup::pointer_stand(),
        |environment| drive(stand, environment, browser, row),
    );
    StepResult::new("capture", browser, row.step_id, verdict, started.elapsed())
}

/// Ожидание приговора для сценария строки.
fn expected_window(scenario: FeedScenario) -> ExpectedWindow {
    match scenario {
        FeedScenario::Scroll { scrolls, .. } => ExpectedWindow::FollowsFeed {
            reel_stream_widths: &REEL_STREAM_WIDTHS[..=scrolls],
        },
        FeedScenario::HoverPreview | FeedScenario::ScrollToComments => {
            ExpectedWindow::StaysOnFirstReel {
                stream_width: REEL_STREAM_WIDTHS[0],
            }
        }
        FeedScenario::Edge(edge) => edges::expected_window(edge),
        FeedScenario::Back(back) => back::expected_window(back),
        FeedScenario::HistoryBack => history_back::EXPECTED_WINDOW,
    }
}

/// Улики, собранные ПОСЛЕ передачи ролика 1.
struct AfterHandOff {
    first_reel_frames: Result<(), String>,
    scrolls: Vec<ScrollRecord>,
    stay: Option<StayRecord>,
    /// Докачка страницы сверх памяти моста (`None` — строка её не просит или до неё не дошло).
    page_download: Option<Result<(), String>>,
    /// Улики края ленты (`None` — строка не про края или до них не дошло).
    edge: Option<EdgeRecord>,
    /// Улики прокрутки назад (`None` — строка не про возврат или до него не дошло).
    back: Option<BackRecord>,
    /// Улики «Назад» в истории (`None` — строка не про это или до него не дошло).
    history_back: Option<HistoryBackRecord>,
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: FeedRow,
) -> Result<Verdict, ScenarioError> {
    let mut hand = match open_hand(stand, environment, browser, row.page_id, row.query_suffix)? {
        HandOpened::Ready(hand) => *hand,
        HandOpened::Finished(verdict) => return Ok(verdict),
    };
    // Как человек: служебная страница экстеншена открыта фоновой вкладкой, вкладка ленты остаётся видимой
    // (лента в скрытой вкладке ставила бы ролик на паузу сама).
    hand.run().open_service_page()?;
    // Окно плеера на шине сеанса: строки краёв проверяют, что оно закрылось или осталось на месте.
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let hand_off = hand_off_visible(hand.run()).map_err(|error| error.to_string());
    let after = if matches!(hand_off, Ok(HandOffOutcome::Started)) {
        follow_the_feed(&mut hand, &control, row)?
    } else {
        AfterHandOff {
            first_reel_frames: Err("not awaited: reel 1 was not handed off".to_owned()),
            scrolls: Vec::new(),
            stay: None,
            page_download: None,
            edge: None,
            back: None,
            history_back: None,
        }
    };
    let page_report = hand.run_ref().finish_page();
    let page_log = hand.run_ref().take_page_log();
    let generations = steps::loaded_generations(&mut hand)?;
    let shown_stream_widths = steps::shown_stream_widths(&mut hand)?;
    let player_log = &mut hand.run().player_log;
    let forbidden_player_lines = player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let end_notice_lines = player_log.snapshot_forbidden(&[END_NOTICE_MARKER])?;
    let target_wait_lines = player_log.snapshot_forbidden(&[TARGET_WAIT_MARKER])?;
    let media_switch_lines = player_log.snapshot_lines_containing(MEDIA_SWITCH_MARKER)?;
    let exit = hand.into_run().browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_feed(
        expected_window(row.scenario),
        &FeedEvidence {
            page_id: row.page_id,
            hand_off,
            host,
            first_reel_frames: after.first_reel_frames,
            scrolls: after.scrolls,
            stay: after.stay,
            page_download: after.page_download,
            edge: after.edge,
            back: after.back,
            history_back: after.history_back,
            target_wait_lines,
            media_switch_lines,
            generations,
            shown_stream_widths,
            end_notice_lines,
            forbidden_player_lines,
            page_report,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// «Отдать вкладку» из уже открытой служебной страницы (вкладка ленты остаётся видимой).
fn hand_off_visible(run: &mut HandoffRun<'_>) -> Result<HandOffOutcome, BrowserError> {
    let prefix = run.page_url_prefix();
    hand_off_from_service_page(run.browser.as_mut(), &prefix)
}

/// После передачи: пропуск посредника, кадры ролика 1 в окне, затем сценарий строки.
fn follow_the_feed(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
    row: FeedRow,
) -> Result<AfterHandOff, ScenarioError> {
    let nothing_after = |reason: String| AfterHandOff {
        first_reel_frames: Err(reason),
        scrolls: Vec::new(),
        stay: None,
        page_download: None,
        edge: None,
        back: None,
        history_back: None,
    };
    if let Err(message) =
        wait_for_session_granted(&hand.run_ref().stderr_log, SESSION_GRANTED_TIMEOUT)?
    {
        return Ok(nothing_after(message));
    }
    hand.run().tell_page("handed-off");
    let first_reel_frames = hand
        .run()
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
        .map(|_| ())
        .map_err(|error| error.to_string());
    if first_reel_frames.is_err() {
        return Ok(AfterHandOff {
            first_reel_frames,
            scrolls: Vec::new(),
            stay: None,
            page_download: None,
            edge: None,
            back: None,
            history_back: None,
        });
    }
    let mut after = AfterHandOff {
        first_reel_frames,
        scrolls: Vec::new(),
        stay: None,
        page_download: None,
        edge: None,
        back: None,
        history_back: None,
    };
    if let Some(page_load) = row.before_scrolling.page_load() {
        // Листаем в любом случае: если докачки не было, приговор назовёт это отдельной причиной.
        after.page_download = Some(steps::run_page_load(hand, page_load));
    }
    match row.scenario {
        FeedScenario::Scroll { scrolls, wheel } => {
            for target_reel in 2..=(scrolls as u32 + 1) {
                after
                    .scrolls
                    .push(steps::scroll_to_reel(hand, target_reel, wheel)?);
            }
        }
        FeedScenario::HoverPreview => after.stay = Some(steps::hover_over_preview(hand)?),
        FeedScenario::ScrollToComments => after.stay = Some(steps::scroll_to_comments(hand)?),
        FeedScenario::Edge(edge) => {
            let run = edge_steps::run_edge(hand, control, edge)?;
            after.scrolls = run.scrolls;
            after.edge = Some(run.edge);
        }
        FeedScenario::Back(back) => {
            let run = back_steps::run_back(hand, control, back)?;
            after.scrolls = run.scrolls;
            after.back = Some(run.record);
        }
        FeedScenario::HistoryBack => {
            let run = history_back::run_history_back(hand, control)?;
            after.scrolls = run.scrolls;
            after.history_back = Some(run.record);
        }
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;
    use std::collections::HashSet;
    use std::path::Path;

    fn site_file(relative: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("site")
            .join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    #[test]
    fn steps_are_unique_and_pages_are_in_the_catalog_and_on_disk() {
        let mut steps = HashSet::new();
        for row in all_rows() {
            assert!(steps.insert(row.step_id), "duplicate step {}", row.step_id);
            assert!(
                PAGE_SCENARIOS.iter().any(|page| page.id == row.page_id),
                "{} is not in the page catalog",
                row.page_id
            );
            assert!(
                row.query_suffix.starts_with("&mode=handoff"),
                "{} must run in the hand-off mode",
                row.step_id
            );
        }
        assert_eq!(page_ids(), ["feed", "feed-hover", "feed-edges"]);
    }

    #[test]
    fn every_query_of_a_row_is_something_its_page_understands() {
        for row in FEED_ROWS {
            let source = site_file(&format!("pages/{}.html", row.page_id));
            for pair in row.query_suffix.trim_start_matches('&').split('&') {
                let (key, value) = pair.split_once('=').expect("key=value");
                if key == "mode" {
                    continue;
                }
                assert!(
                    source.contains(&format!("'{key}'")) && source.contains(value),
                    "page {} does not read {key}={value}",
                    row.page_id
                );
            }
        }
    }

    #[test]
    fn the_reel_stream_widths_match_the_generator() {
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("media/generate.sh"),
        )
        .expect("the media generator");
        // Три ролика ленты: ширина кадра 640 / 512 / 384 различает их в журнале плеера.
        for (number, size) in [(1, "640x360 360"), (2, "512x288 288"), (3, "384x216 216")] {
            assert!(
                script.contains(&format!("make_reel {number} {size} ")),
                "reel {number} moved in the generator"
            );
        }
        assert_eq!(REEL_STREAM_WIDTHS, [640, 512, 384]);
    }

    /// Решение 385: строка `feed-hover-preview` начинает наблюдение посреди главного ролика и ждёт две сводки
    /// плеера по 10 с. Главный ролик страницы — 90-секундный ролик 6 той же ширины 640, что и ролик 1
    /// (судья узнаёт главный ролик по ширине), а не 30-секундный ролик 1, который успевал доиграть.
    #[test]
    fn the_hover_page_main_reel_outlives_the_stay_observation() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let script =
            std::fs::read_to_string(root.join("media/generate.sh")).expect("the media generator");
        assert!(
            script.contains("make_reel 6 640x360 360 90 "),
            "reel 6 must stay 640 wide and 90 s long"
        );
        assert!(
            script.contains("\"reel-6\": 90.0"),
            "the catalog must know reel 6 lasts 90 s"
        );
        let page = std::fs::read_to_string(root.join("site/pages/feed-hover.html"))
            .expect("the hover page");
        assert!(
            page.contains("source.prepare(6, { muted: false })"),
            "the hover page main video must be reel 6"
        );
        assert!(
            !page.contains("source.prepare(1,"),
            "reel 1 (30 s) ends inside the stay observation"
        );
        assert_eq!(
            REEL_STREAM_WIDTHS[0], 640,
            "the stay judge expects the main reel 640 wide"
        );
    }

    #[test]
    fn a_scroll_row_expects_one_more_reel_than_it_scrolls() {
        let window = expected_window(FeedScenario::Scroll {
            scrolls: 2,
            wheel: WheelPlace::OverReel,
        });
        assert_eq!(
            window,
            ExpectedWindow::FollowsFeed {
                reel_stream_widths: &[640, 512, 384]
            }
        );
        assert_eq!(
            expected_window(FeedScenario::HoverPreview),
            ExpectedWindow::StaysOnFirstReel { stream_width: 640 }
        );
    }

    #[test]
    fn the_live_instagram_shape_is_scrolled_both_beside_the_reel_and_over_the_plate() {
        // Две находки живой проверки — две строки одной формы: переезд окна (колесо мимо ролика) и колесо
        // над заглушкой. Пропади любая — находка осталась бы без строки стенда.
        let wheels: Vec<WheelPlace> = FEED_ROWS
            .iter()
            .filter(|row| row.query_suffix.ends_with("shape=halfway"))
            .filter_map(|row| match row.scenario {
                FeedScenario::Scroll { scrolls: 2, wheel } => Some(wheel),
                _ => None,
            })
            .collect();
        assert_eq!(wheels, [WheelPlace::BesideReel, WheelPlace::OverReel]);
        // Страница обязана знать форму и писать метку, по которой видно: пауза застала ролик на экране.
        let shapes = site_file("lib/feed-shapes.js");
        for needle in [
            "case 'halfway'",
            "halfway-old-paused n=",
            "LAYOUT_INNER_SCROLLER",
        ] {
            assert!(shapes.contains(needle), "feed-shapes.js lost '{needle}'");
        }
        let feed = site_file("pages/feed.html");
        assert!(
            feed.contains("html.inner-scroller #feed{height:100vh;overflow-y:auto;scroll-snap-type:y mandatory}"),
            "feed.html lost the inner scroller layout of the halfway shape"
        );
    }

    #[test]
    fn the_long_download_row_downloads_more_than_the_bridge_remembers() {
        // Параметр адреса и шаг робота ходят парой: страница без хука или робот без команды — строка вхолостую.
        for row in FEED_ROWS {
            let history_in_query = row
                .query_suffix
                .split('&')
                .find(|pair| pair.starts_with("history="));
            assert_eq!(
                history_in_query,
                row.before_scrolling.history_query(),
                "{}: the page query and the robot step disagree",
                row.step_id
            );
        }
        let long_download_rows = FEED_ROWS
            .iter()
            .filter(|row| row.before_scrolling == BeforeScrolling::PageDownloadsPastBridgeMemory)
            .count();
        assert_eq!(long_download_rows, 1);
        let history = site_file("lib/feed-history.js");
        let hook = steps::DOWNLOAD_PAST_BRIDGE_MEMORY
            .command
            .strip_prefix("hook:")
            .expect("a page hook command");
        assert!(
            history.contains(&format!("DOWNLOAD_HOOK = '{hook}'")),
            "feed-history.js lost the hook '{hook}'"
        );
        assert!(
            history.contains(&format!(
                "`{} mib=",
                steps::DOWNLOAD_PAST_BRIDGE_MEMORY.finished_mark
            )),
            "feed-history.js lost the mark the robot waits for"
        );
        // Докачка обязана быть больше потолка памяти моста, иначе мосту нечего выкидывать и строка ничего
        // не проверяет. Потолок подняли — стенд должен качать больше.
        let budget = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../extensions/browser/src/page/journal/memory-budget.ts"),
        )
        .expect("the bridge memory budget source");
        assert!(
            budget.contains("DOCUMENT_MEMORY_BUDGET_BYTES = 64 * 1024 * 1024"),
            "the bridge memory cap changed: resize DOWNLOAD_BYTES in feed-history.js"
        );
        assert!(
            history.contains("const DOWNLOAD_BYTES = 80 * MEBIBYTE"),
            "feed-history.js must download more than the 64 MiB bridge memory cap"
        );
    }

    #[test]
    fn the_detached_passed_reels_row_reproduces_the_measured_instagram_shape() {
        let rows: Vec<&FeedRow> = FEED_ROWS
            .iter()
            .filter(|row| {
                row.before_scrolling == BeforeScrolling::PageDetachesPassedReelsAndPreloadsFarAhead
            })
            .collect();
        assert_eq!(rows.len(), 1);
        // Одна прокрутка: судится ролик, который в миг нехватки памяти был следующим по ленте.
        assert!(matches!(
            rows[0].scenario,
            FeedScenario::Scroll { scrolls: 1, .. }
        ));
        let history = site_file("lib/feed-history.js");
        let page_load = steps::DETACH_PASSED_REELS_AND_PRELOAD_FAR_AHEAD;
        let hook = page_load
            .command
            .strip_prefix("hook:")
            .expect("a page hook command");
        assert!(
            history.contains(&format!("CROWD_HOOK = '{hook}'")),
            "feed-history.js lost the hook '{hook}'"
        );
        // Отметка страницы несёт доказательство формы: пройденные закрыты все, дальние все живы.
        assert!(
            history.contains(
                "`far-reels-preloaded passed_closed=${passedClosed} far_open=${farOpen} mib=${Math.round(downloadedBytes / MEBIBYTE)}`"
            ),
            "feed-history.js lost the mark the robot waits for"
        );
        assert_eq!(
            page_load.finished_mark,
            "far-reels-preloaded passed_closed=3 far_open=8 mib="
        );
        // Числа отметки — те же, что страница берёт для нагрузки: сменили одно, меняйте и другое.
        for needle in [
            "const PASSED_REEL_COUNT = 3;",
            "const FAR_REEL_COUNT = 8;",
            "const FAR_REEL_BYTES = 10 * MEBIBYTE;",
            "for (const reel of this.#passedReels) reel.detach();",
        ] {
            assert!(history.contains(needle), "feed-history.js lost '{needle}'");
        }
    }

    #[test]
    fn the_hover_target_and_the_log_marks_the_robot_waits_for_are_written_by_the_page() {
        let hover = site_file("pages/feed-hover.html");
        // Цель наведения, метки раздражителя и отчёта — то, на что опираются шаги робота и приговор.
        for needle in [
            "registerEntryTarget('preview'",
            "preview-start-trigger why=",
            "preview-playing muted=",
            "main-left-screen paused=",
            "'main_paused'",
            "'preview_playing'",
        ] {
            assert!(hover.contains(needle), "feed-hover.html lost '{needle}'");
        }
        let kit = site_file("lib/feed-kit.js");
        for needle in ["reel-started n=", "wheel trusted=", "handoff-ready"] {
            assert!(kit.contains(needle), "feed-kit.js lost '{needle}'");
        }
        let feed = site_file("pages/feed.html");
        for needle in ["'reels_started'", "'wheel_events_trusted'"] {
            assert!(feed.contains(needle), "feed.html lost {needle}");
        }
    }
}
