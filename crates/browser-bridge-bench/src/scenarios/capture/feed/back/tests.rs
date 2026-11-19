//! Тесты строк возврата: таблица, страница и приговор ходят парой. Рассинхрон (робот ждёт метку, которую
//! страница не пишет; страница не знает параметр адреса) оставил бы строку проходящей вхолостую или
//! вечно падающей, поэтому связь проверяется по исходникам страницы.

use super::*;
use std::path::Path;

fn site_file(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("site")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn the_back_rows_run_the_edge_page_in_the_hand_off_mode() {
    let steps: Vec<&str> = BACK_ROWS.iter().map(|row| row.step_id).collect();
    assert_eq!(
        steps,
        [
            "feed-back-live-many",
            "feed-back-live-many-pressure",
            "feed-back-live-many-far",
            "feed-back-recreated-many",
            "feed-back-recreated-pool",
        ]
    );
    for row in BACK_ROWS {
        assert_eq!(row.page_id, "feed-edges", "{}", row.step_id);
        assert!(
            row.query_suffix.starts_with("&mode=handoff"),
            "{}",
            row.step_id
        );
        assert!(row.query_suffix.contains("&reels=3"), "{}", row.step_id);
        assert!(
            matches!(row.scenario, FeedScenario::Back(_)),
            "{}",
            row.step_id
        );
    }
}

#[test]
fn a_row_asks_the_page_for_the_return_its_judge_expects() {
    for row in BACK_ROWS {
        let FeedScenario::Back(scenario) = row.scenario else {
            panic!("{} is not a back row", row.step_id);
        };
        let wants_live = row.query_suffix.contains("&back=live");
        let wants_recreated = row.query_suffix.contains("&back=recreated");
        match scenario {
            BackScenario::LiveElement
            | BackScenario::LiveElementUnderMemoryPressure
            | BackScenario::LiveElementFarUnderMemoryPressure => {
                assert!(wants_live && !wants_recreated, "{}", row.step_id);
                assert!(row.query_suffix.contains("shape=many"), "{}", row.step_id);
            }
            BackScenario::RecreatedElement => {
                assert!(wants_recreated && !wants_live, "{}", row.step_id);
            }
        }
        // Давление памяти просит ровно строка-замер.
        assert_eq!(
            row.query_suffix.contains("history=passed-reels-detached"),
            scenario.presses_bridge_memory(),
            "{}",
            row.step_id
        );
    }
}

#[test]
fn the_expected_window_returns_to_reel_one_and_measures_only_the_pressure_row() {
    assert_eq!(BACK_REEL_STREAM_WIDTHS, [640, 512, 640]);
    assert_eq!(
        expected_window(BackScenario::LiveElement),
        ExpectedWindow::ReturnsToEarlierReel {
            reel_stream_widths: &[640, 512, 640],
            start: BackStart::NearPausedSecond
        }
    );
    assert_eq!(
        BackScenario::LiveElementUnderMemoryPressure.start(),
        BackStart::Measured
    );
    assert_eq!(
        BackScenario::RecreatedElement.start(),
        BackStart::FromTheBeginning
    );
    assert_eq!(
        BackScenario::LiveElementFarUnderMemoryPressure.start(),
        BackStart::MeasuredFar
    );
    // Дальний возврат идёт вперёд на ролик 3 (две прокрутки), остальные на ролик 2.
    assert_eq!(
        BackScenario::LiveElementFarUnderMemoryPressure.forward_reels(),
        3
    );
    assert_eq!(BackScenario::LiveElement.forward_reels(), 2);
    assert_eq!(
        expected_window(BackScenario::LiveElementFarUnderMemoryPressure),
        ExpectedWindow::ReturnsToEarlierReel {
            reel_stream_widths: &[640, 512, 384, 640],
            start: BackStart::MeasuredFar
        }
    );
}

#[test]
fn every_query_of_a_back_row_and_every_mark_the_robot_waits_for_is_written_by_the_page() {
    let sources = [
        "pages/feed-edges.html",
        "lib/feed-edge-back.js",
        "lib/feed-edge-feed.js",
        "lib/feed-history.js",
    ]
    .iter()
    .map(|file| site_file(file))
    .collect::<Vec<_>>()
    .join("\n");
    for row in BACK_ROWS {
        for pair in row.query_suffix.trim_start_matches('&').split('&') {
            let (key, value) = pair.split_once('=').expect("key=value");
            if key == "mode" || key == "reels" {
                continue;
            }
            assert!(
                sources.contains(&format!("'{key}'")),
                "{}: '{key}'",
                row.step_id
            );
            assert!(
                sources.contains(&format!("'{value}'")),
                "{}: {key}={value}",
                row.step_id
            );
        }
    }
    for needle in [
        "reel-step-back n=",
        "reel-back-started n=${number} shape=${shape} element=${element}",
        "reel-back-play-called n=${number} shape=${shape} element=${element}",
        "play-rejected back reel",
        "reel-paused n=${number} at_ms=",
        "'same'",
        "'new'",
        "reel_back_started",
        "reel_steps_back",
    ] {
        assert!(sources.contains(needle), "the page lost '{needle}'");
    }
    // Метки, которые робот ждёт в журнале страницы, начинаются так, как страница их пишет.
    assert_eq!(
        BackScenario::LiveElement.started_mark(),
        "reel-back-play-called n=1 shape=many element=same"
    );
    assert_eq!(
        BackScenario::RecreatedElement.started_mark(),
        "reel-back-play-called n=1 shape="
    );
}
