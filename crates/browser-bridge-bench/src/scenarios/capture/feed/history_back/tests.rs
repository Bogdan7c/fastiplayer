//! Тесты строки «Назад» в истории: таблица, страница и робот ходят парой.

use super::*;
use std::path::Path;

fn site_file(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("site")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn the_row_is_the_tiktok_shape_returning_through_the_history() {
    assert_eq!(HISTORY_BACK_ROWS.len(), 1);
    let row = &HISTORY_BACK_ROWS[0];
    assert_eq!(row.step_id, "feed-history-back-pool");
    assert_eq!(row.page_id, "feed-edges");
    assert!(row.query_suffix.starts_with("&mode=handoff"));
    assert!(row.query_suffix.contains("shape=pool"));
    assert!(row.query_suffix.contains("back=history"));
    assert_eq!(row.scenario, FeedScenario::HistoryBack);
    assert_eq!(EXPECTED_WINDOW, ExpectedWindow::PicksUpAfterHistoryBack);
}

#[test]
fn the_page_knows_the_hook_the_marks_and_the_query_the_robot_relies_on() {
    let sources = [
        "pages/feed-edges.html",
        "lib/feed-edge-back.js",
        "lib/feed-edge-shapes.js",
    ]
    .iter()
    .map(|file| site_file(file))
    .collect::<Vec<_>>()
    .join("\n");
    for needle in [
        "'history-back'",
        "BACK_HISTORY = 'history'",
        "history-back-popstate",
        "popstate",
        "class HistoryBackPool extends PoolEdge",
        "leave-url-pushed path=/explore",
    ] {
        assert!(sources.contains(needle), "the page lost '{needle}'");
    }
    // Метка «позвала play()» страницы строится так, что метка робота для ролика 2 — её частный случай.
    assert!(
        sources.contains("reel-back-play-called n=${number} shape=${shape} element=${element}")
    );
    assert_eq!(
        HISTORY_BACK_MARKS[1],
        "reel-back-play-called n=2 shape=pool element=same"
    );
    // Уход — тот же сценарий T5, что у строки `feed-leave-section-pool`: метки ухода те же.
    assert!(MARKS_LEAVE_POOL.contains(&"leave-url-pushed path=/explore"));
}
