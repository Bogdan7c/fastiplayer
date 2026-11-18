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
//! Строки таблицы [`FEED_ROWS`]: три формы ленты (`many`, `moved`, `pool`) и два случая, где окно
//! НЕ переезжает. Приговор общий и чистый: [`judge::judge_feed`].

mod judge;
mod steps;

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
use crate::observe::read_host_evidence;
use judge::{ExpectedWindow, FeedEvidence, ScrollRecord, StayRecord, judge_feed};

/// Ширины кадра трёх роликов ленты (ассеты `reel-1..3` каталога медиа: 640x360, 512x288, 384x216). По
/// строке журнала плеера о размере потока видно, какой ролик сейчас в окне.
const REEL_STREAM_WIDTHS: &[u64] = &[640, 512, 384];
/// Переменная окружения: гонять только строки, в имени шага которых есть эта подстрока (отладка одной
/// строки: `BRIDGE_BENCH_FEED_ROW=pool`).
const ROW_FILTER_VARIABLE: &str = "BRIDGE_BENCH_FEED_ROW";
/// Строка журнала плеера: окно показало зрителю сообщение о конце сеанса.
const END_NOTICE_MARKER: &str = "Browser session end notice shown";

/// Что делает строка с лентой.
#[derive(Debug, Clone, Copy)]
enum FeedScenario {
    /// Листаем колесом вперёд на `scrolls` роликов; окно обязано идти за лентой.
    Scroll { scrolls: usize },
    /// Настоящая мышь наводится на превью рядом с главным видео; окно остаётся.
    HoverPreview,
    /// Колесо уводит главный ролик с экрана (страница его не останавливает), ниже заигрывает превью.
    ScrollToComments,
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
}

/// Строки сценария. Каждая — отдельное прогонное окружение (свой плеер и браузер).
const FEED_ROWS: &[FeedRow] = &[
    // Instagram: свой элемент и MediaSource на ролик; пауза ушедшего, смена адреса, запуск следующего.
    FeedRow {
        step_id: "feed-many",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=many",
        scenario: FeedScenario::Scroll { scrolls: 2 },
    },
    // YouTube Shorts: один элемент переезжает по секциям, новый MediaSource, паузы нет.
    FeedRow {
        step_id: "feed-moved",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=moved",
        scenario: FeedScenario::Scroll { scrolls: 1 },
    },
    // TikTok: два элемента по очереди, прежний убирается из документа и ставится на паузу.
    FeedRow {
        step_id: "feed-pool",
        page_id: "feed",
        query_suffix: "&mode=handoff&shape=pool",
        scenario: FeedScenario::Scroll { scrolls: 2 },
    },
    // Превью по наведению рядом с главным видео: наш ролик на экране, переезда нет.
    FeedRow {
        step_id: "feed-hover-preview",
        page_id: "feed-hover",
        query_suffix: "&mode=handoff&variant=hover",
        scenario: FeedScenario::HoverPreview,
    },
    // «Листаем к комментариям»: ролик ушёл с экрана, но играет; ниже заиграло превью. Переезда нет.
    FeedRow {
        step_id: "feed-scroll-to-comments",
        page_id: "feed-hover",
        query_suffix: "&mode=handoff&variant=comments",
        scenario: FeedScenario::ScrollToComments,
    },
];

/// Идентификаторы страниц сценария (для `--page` и таблицы `capture`).
pub(in crate::scenarios) fn page_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for row in FEED_ROWS {
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
    FEED_ROWS
        .iter()
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
        FeedScenario::Scroll { scrolls } => ExpectedWindow::FollowsFeed {
            reel_stream_widths: &REEL_STREAM_WIDTHS[..=scrolls],
        },
        FeedScenario::HoverPreview | FeedScenario::ScrollToComments => {
            ExpectedWindow::StaysOnFirstReel {
                stream_width: REEL_STREAM_WIDTHS[0],
            }
        }
    }
}

/// Улики, собранные ПОСЛЕ передачи ролика 1.
struct AfterHandOff {
    first_reel_frames: Result<(), String>,
    scrolls: Vec<ScrollRecord>,
    stay: Option<StayRecord>,
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
    let hand_off = hand_off_visible(hand.run()).map_err(|error| error.to_string());
    let after = if matches!(hand_off, Ok(HandOffOutcome::Started)) {
        follow_the_feed(&mut hand, row.scenario)?
    } else {
        AfterHandOff {
            first_reel_frames: Err("not awaited: reel 1 was not handed off".to_owned()),
            scrolls: Vec::new(),
            stay: None,
        }
    };
    let page_report = hand.run_ref().finish_page();
    let page_log = hand.run_ref().take_page_log();
    let generations = steps::loaded_generations(&mut hand)?;
    let shown_stream_widths = steps::shown_stream_widths(&mut hand)?;
    let player_log = &mut hand.run().player_log;
    let forbidden_player_lines = player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let end_notice_lines = player_log.snapshot_forbidden(&[END_NOTICE_MARKER])?;
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
    scenario: FeedScenario,
) -> Result<AfterHandOff, ScenarioError> {
    let nothing_after = |reason: String| AfterHandOff {
        first_reel_frames: Err(reason),
        scrolls: Vec::new(),
        stay: None,
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
        });
    }
    let mut after = AfterHandOff {
        first_reel_frames,
        scrolls: Vec::new(),
        stay: None,
    };
    match scenario {
        FeedScenario::Scroll { scrolls } => {
            for target_reel in 2..=(scrolls as u32 + 1) {
                after
                    .scrolls
                    .push(steps::scroll_to_reel(hand, target_reel)?);
            }
        }
        FeedScenario::HoverPreview => after.stay = Some(steps::hover_over_preview(hand)?),
        FeedScenario::ScrollToComments => after.stay = Some(steps::scroll_to_comments(hand)?),
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
        for row in FEED_ROWS {
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
        assert_eq!(page_ids(), ["feed", "feed-hover"]);
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

    #[test]
    fn a_scroll_row_expects_one_more_reel_than_it_scrolls() {
        let window = expected_window(FeedScenario::Scroll { scrolls: 2 });
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
