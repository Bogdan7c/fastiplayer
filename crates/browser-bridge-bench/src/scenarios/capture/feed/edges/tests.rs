//! Тесты строк краёв: таблица, страница и приговор ходят парой. Рассинхрон (робот ждёт метку, которую
//! страница не пишет; страница не знает параметр адреса) оставил бы строку проходящей вхолостую или
//! вечно падающей, поэтому связь проверяется по исходникам страницы.

use super::*;
use std::path::Path;
use std::time::Duration;

fn site_file(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("site")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Все исходники страницы краёв вместе: страница и её модули.
fn edge_site_sources() -> String {
    [
        "pages/feed-edges.html",
        "lib/feed-kit.js",
        "lib/feed-edge-feed.js",
        "lib/feed-edge-shapes.js",
        "lib/feed-edge-previews.js",
    ]
    .iter()
    .map(|file| site_file(file))
    .collect::<Vec<_>>()
    .join("\n")
}

/// Значение параметра адреса строки (`None` — параметра нет).
fn query_value(row: &FeedRow, key: &str) -> Option<&'static str> {
    row.query_suffix
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
}

#[test]
fn every_edge_row_runs_the_edge_page_in_the_hand_off_mode() {
    assert!(!EDGE_ROWS.is_empty());
    for row in EDGE_ROWS {
        assert_eq!(row.page_id, "feed-edges", "{}", row.step_id);
        assert!(
            row.query_suffix.starts_with("&mode=handoff"),
            "{}",
            row.step_id
        );
        assert!(
            matches!(row.scenario, FeedScenario::Edge(_)),
            "{}",
            row.step_id
        );
    }
}

#[test]
fn every_query_of_an_edge_row_is_something_the_page_understands() {
    let sources = edge_site_sources();
    for row in EDGE_ROWS {
        for pair in row.query_suffix.trim_start_matches('&').split('&') {
            let (key, value) = pair.split_once('=').expect("key=value");
            if key == "mode" {
                continue;
            }
            assert!(
                sources.contains(&format!("'{key}'")),
                "{}: the page does not read '{key}'",
                row.step_id
            );
            // Число роликов — число, остальные значения — слова, которые страница обязана знать.
            if key != "reels" {
                assert!(
                    sources.contains(&format!("'{value}'")),
                    "{}: the page does not know {key}={value}",
                    row.step_id
                );
            }
        }
    }
}

#[test]
fn a_burst_row_has_five_reels_with_five_different_widths() {
    let mut widths = EDGE_REEL_STREAM_WIDTHS.to_vec();
    widths.sort_unstable();
    widths.dedup();
    assert_eq!(widths.len(), 5, "the widths must tell the reels apart");
    for row in EDGE_ROWS
        .iter()
        .filter(|row| row.scenario == FeedScenario::Edge(EdgeScenario::BurstToLastReel))
    {
        assert_eq!(query_value(row, "reels"), Some("5"), "{}", row.step_id);
    }
    assert_eq!(
        expected_window(EdgeScenario::BurstToLastReel),
        ExpectedWindow::EndsOnLastReel {
            reel_stream_widths: EDGE_REEL_STREAM_WIDTHS
        }
    );
}

#[test]
fn the_edge_reels_match_the_media_generator() {
    let script =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("media/generate.sh"))
            .expect("the media generator");
    for (number, size) in [
        (1, "640x360 360"),
        (2, "512x288 288"),
        (3, "384x216 216"),
        (4, "320x180 180"),
        (5, "448x252 252"),
    ] {
        assert!(
            script.contains(&format!("make_reel {number} {size} ")),
            "reel {number} moved in the generator"
        );
    }
    assert!(script.contains("\"reel-4\": 40.0") && script.contains("\"reel-5\": 34.0"));
    assert_eq!(EDGE_REEL_STREAM_WIDTHS, [640, 512, 384, 320, 448]);
}

#[test]
fn the_page_writes_every_mark_and_serves_every_hook_the_robot_needs() {
    let sources = edge_site_sources();
    let mut needed: Vec<&str> = vec![
        // Раздражители и метки серии, конца ленты и удаления элементов (`edge_steps`).
        "reel-step n=",
        "reel-started n=",
        "feed-tail-reached",
        "tail-reel-paused n=",
        "stimulus old-removed-during-move n=",
        "stimulus current-reel-removed n=",
        "leave-url-pushed path=",
        "'remove-current-reel'",
        "'leave-section'",
        "'leave-from-tail'",
        // 16b3c: превью на том же элементе Shorts и снятый источник ролика без смены адреса.
        "'leave-with-same-element-preview'",
        "'drop-current-source'",
        "stimulus current-source-dropped n=",
        // Метрики, на которые опирается приговор серии.
        "final_reel",
        "burst_intermediates",
    ];
    for row in EDGE_ROWS {
        if let FeedScenario::Edge(
            EdgeScenario::LeaveSection { page_marks, .. }
            | EdgeScenario::LeaveBeforeStep { page_marks },
        ) = row.scenario
        {
            needed.extend(page_marks.iter().copied());
        }
    }
    for mark in needed {
        // Метки с полями («kind=file muted=true») собираются из кусков: ищем начало до первого `=`-поля.
        let probe = mark.split(" kind=").next().unwrap_or(mark);
        let probe = probe.split(" path=").next().unwrap_or(probe);
        let probe = probe.split(" n=").next().unwrap_or(probe);
        assert!(
            sources.contains(probe),
            "the edge page lost '{probe}' (needed: '{mark}')"
        );
    }
}

/// Строка изображает сериал с номером серии в адресе (форма `moved`, `address=serial`), а не ленту из списка.
fn is_serial_row(row: &FeedRow) -> bool {
    query_value(row, "address") == Some("serial")
}

#[test]
fn the_leave_rows_cover_the_three_live_shapes_and_each_demands_a_visible_muted_preview() {
    let leave_rows: Vec<&FeedRow> = EDGE_ROWS
        .iter()
        .filter(|row| {
            matches!(
                row.scenario,
                FeedScenario::Edge(EdgeScenario::LeaveSection { .. })
            )
        })
        .collect();
    let shapes: Vec<_> = leave_rows
        .iter()
        .filter_map(|row| query_value(row, "shape"))
        .collect();
    assert_eq!(shapes, ["many", "pool", "moved", "moved", "moved"]);
    for row in leave_rows {
        let FeedScenario::Edge(EdgeScenario::LeaveSection {
            page_marks, window, ..
        }) = row.scenario
        else {
            unreachable!("filtered above");
        };
        // Решение 370: уход из ленты оставляет окно на паузе, Shorts тоже (381); доигрывает только сериал (306).
        let expected = if is_serial_row(row) {
            WindowAfterLeave::PlaysOut
        } else {
            WindowAfterLeave::StandsPaused
        };
        assert_eq!(window, expected, "{}", row.step_id);
        assert!(
            page_marks
                .iter()
                .any(|mark| mark.contains("muted=true visible=true")),
            "{}: without a playing visible preview the row would pass idly",
            row.step_id
        );
    }
}

#[test]
fn the_tail_rows_ask_for_the_section_without_video() {
    for row in EDGE_ROWS.iter().filter(|row| {
        matches!(
            row.scenario,
            FeedScenario::Edge(EdgeScenario::LeaveWhileWaiting | EdgeScenario::WaitAtFeedEnd)
        )
    }) {
        assert_eq!(
            query_value(row, "tail"),
            Some("comments"),
            "{}",
            row.step_id
        );
        assert_eq!(query_value(row, "shape"), Some("many"), "{}", row.step_id);
    }
}

#[test]
fn the_removal_rows_expect_the_window_to_follow_the_feed_not_to_end() {
    assert_eq!(
        expected_window(EdgeScenario::OldRemovedDuringMove),
        ExpectedWindow::FollowsFeed {
            reel_stream_widths: &[640, 512]
        }
    );
    assert_eq!(
        expected_window(EdgeScenario::CurrentRemovedAfterMove),
        ExpectedWindow::FollowsFeed {
            reel_stream_widths: &[640, 512, 384]
        }
    );
    // Причуда страницы обязана быть в адресе строки, иначе страница удалила бы элемент, когда не просили.
    let mid_move: Vec<_> = EDGE_ROWS
        .iter()
        .filter(|row| row.scenario == FeedScenario::Edge(EdgeScenario::OldRemovedDuringMove))
        .collect();
    assert_eq!(mid_move.len(), 2);
    for row in mid_move {
        assert_eq!(query_value(row, "quirk"), Some("remove-old-during-move"));
    }
}

#[test]
fn the_leave_before_step_rows_never_scroll_and_expect_an_honest_end_on_reel_one() {
    let rows: Vec<&FeedRow> = EDGE_ROWS
        .iter()
        .filter(|row| {
            matches!(
                row.scenario,
                FeedScenario::Edge(EdgeScenario::LeaveBeforeStep { .. })
            )
        })
        .collect();
    let shapes: Vec<_> = rows
        .iter()
        .filter_map(|row| query_value(row, "shape"))
        .collect();
    assert_eq!(shapes, ["many", "pool"]);
    for row in rows {
        let FeedScenario::Edge(scenario @ EdgeScenario::LeaveBeforeStep { page_marks }) =
            row.scenario
        else {
            unreachable!("filtered above");
        };
        // Ни одной спокойной прокрутки: приговор ждёт ровно ноль прокруток и один показанный ролик.
        assert_eq!(
            expected_window(scenario),
            ExpectedWindow::EndsHonestly {
                reel_stream_widths: &[640],
                window: WindowAfterLeave::StandsPaused,
                end_deadline: EndAfterAddressLeave::NotMeasured,
            }
        );
        assert!(
            page_marks
                .iter()
                .any(|mark| mark.contains("muted=true visible=true")),
            "{}: without a playing visible preview the row would pass idly",
            row.step_id
        );
    }
}

#[test]
fn the_shorts_leave_rows_time_the_pause_from_the_address_and_the_serial_row_does_not() {
    let moved_leaves: Vec<&FeedRow> = EDGE_ROWS
        .iter()
        .filter(|row| {
            query_value(row, "shape") == Some("moved")
                && matches!(
                    row.scenario,
                    FeedScenario::Edge(EdgeScenario::LeaveSection { .. })
                )
        })
        .collect();
    let ids: Vec<_> = moved_leaves.iter().map(|row| row.step_id).collect();
    assert_eq!(
        ids,
        [
            "feed-leave-section-moved",
            "feed-leave-preview-same-element-moved",
            "feed-leave-section-serial"
        ]
    );
    for row in moved_leaves {
        let FeedScenario::Edge(EdgeScenario::LeaveSection {
            hook,
            page_marks,
            end_deadline,
            ..
        }) = row.scenario
        else {
            unreachable!("filtered above");
        };
        // Время до конца отсчитывается от ПЕРВОЙ метки — смены адреса.
        assert_eq!(
            page_marks.first(),
            Some(&"leave-url-pushed path=/"),
            "{}",
            row.step_id
        );
        let expected_deadline = if is_serial_row(row) {
            EndAfterAddressLeave::NotMeasured
        } else {
            EndAfterAddressLeave::Within(SHORTS_LEAVE_END_DEADLINE)
        };
        assert_eq!(end_deadline, expected_deadline, "{}", row.step_id);
        let expected_hook = if row.step_id.contains("same-element") {
            LeaveHook::SameElementPreview
        } else {
            LeaveHook::SectionScenario
        };
        assert_eq!(hook, expected_hook, "{}", row.step_id);
    }
    // Предел отделяет уход по адресу (~1 с) от прежнего пути через тень (1,45 с + 5 с).
    assert!(SHORTS_LEAVE_END_DEADLINE >= Duration::from_secs(2));
    assert!(SHORTS_LEAVE_END_DEADLINE < Duration::from_millis(6_450));
}

#[test]
fn the_shorts_rows_step_through_list_addresses_and_the_serial_row_through_serial_ones() {
    let shapes = site_file("lib/feed-edge-shapes.js");
    // Адрес шага Shorts лежит под `/shorts/` — записью стенда в списке лент одного элемента экстеншена.
    assert!(shapes.contains("moved: (to) => `/shorts/Sh${to}aBcDeFgHiJ`"));
    assert!(shapes.contains("serial: (to) => `/serial/123/${to}`"));
    let feeds = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../extensions/browser/src/background/handoff/feed/single-element-feeds.ts"),
    )
    .expect("the extension list of single-element feeds");
    assert!(feeds.contains("\"127.0.0.1/shorts\", \"localhost/shorts\""));
    // Ни одной записи-сериала и ни одного голого локального хоста: иначе сериал стенда стал бы лентой.
    assert!(
        !feeds.contains("/serial\""),
        "a serial must never be a single-element feed"
    );
    assert!(
        !feeds.contains("\"127.0.0.1\""),
        "a bare bench host would make every bench page a feed"
    );
}

#[test]
fn the_shorts_dropped_reel_row_expects_the_window_to_wait_on_reel_two() {
    let rows: Vec<&FeedRow> = EDGE_ROWS
        .iter()
        .filter(|row| row.scenario == FeedScenario::Edge(EdgeScenario::ReelDroppedWaits))
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].step_id, "feed-shorts-reel-dropped-waits");
    assert_eq!(query_value(rows[0], "shape"), Some("moved"));
    assert_eq!(
        query_value(rows[0], "address"),
        None,
        "Shorts, not a serial"
    );
    // Сообщение о конце в этой строке запрещено (`WaitsAtFeedEnd`): 4009 и «Source tab closed» — провал.
    assert_eq!(
        expected_window(EdgeScenario::ReelDroppedWaits),
        ExpectedWindow::WaitsAtFeedEnd {
            reel_stream_widths: &[640, 512]
        }
    );
}

#[test]
fn no_row_filter_picks_a_shorts_row_together_with_another_one() {
    // `BRIDGE_BENCH_FEED_ROW` выбирает строки ПОДСТРОКОЙ: имя строки не должно быть началом другой.
    for row in EDGE_ROWS {
        let picked: Vec<_> = EDGE_ROWS
            .iter()
            .filter(|other| other.step_id.contains(row.step_id))
            .map(|other| other.step_id)
            .collect();
        assert_eq!(picked, [row.step_id], "{}", row.step_id);
    }
}
