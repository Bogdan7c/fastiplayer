//! Договор команды `live-ends` со страницами и сервером стенда: команды робота объявлены
//! страницами, страницы в каталоге с правильным `unsupported`, адрес эфира — тот, что знает сервер.

use super::*;
use crate::scenarios::page_catalog::PAGE_SCENARIOS;

/// Исходник страницы вместе с её общими телами (`site/pages/<id>.html` и подключённые `lib/*.js`).
fn page_source(page_id: &str) -> String {
    let site = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site");
    let mut source = std::fs::read_to_string(site.join(format!("pages/{page_id}.html")))
        .unwrap_or_else(|error| panic!("page {page_id}: {error}"));
    for lib in [
        format!("lib/{page_id}-page.js"),
        "lib/live-ends-observer.js".to_owned(),
    ] {
        source.push_str(
            &std::fs::read_to_string(site.join(&lib))
                .unwrap_or_else(|error| panic!("page lib {lib}: {error}")),
        );
    }
    source
}

/// Команды, которые робот шлёт странице строки, - те, что страница объявила; иначе стенд упал бы
/// только на живом прогоне.
#[test]
fn every_hook_the_robot_sends_is_declared_by_the_page() {
    let hooks_of = |row: LiveEndsRow| -> &'static [&'static str] {
        match row {
            LiveEndsRow::MseLiveEnds => &["mark-on-air", "end-live", "expect-recording"],
            LiveEndsRow::HlsLiveEnds => &["mark-on-air", "expect-recording"],
        }
    };
    for row in ROWS {
        let source = page_source(row.page_id());
        for hook in hooks_of(row) {
            assert!(
                source.contains(&format!("'{hook}'")),
                "{} does not declare the hook '{hook}'",
                row.page_id()
            );
        }
    }
}

#[test]
fn the_hls_hook_the_robot_never_sends_is_not_needed_and_end_live_is_mse_only() {
    // HLS кончает сервер по часам: страница не должна иметь команды «закончить эфир».
    assert!(!page_source("hls-live-ends").contains("'end-live'"));
}

#[test]
fn both_pages_are_in_the_catalog_and_only_hls_may_be_unsupported_in_firefox() {
    let find = |id: &str| {
        PAGE_SCENARIOS
            .iter()
            .find(|scenario| scenario.id == id)
            .unwrap_or_else(|| panic!("{id} is in the catalog"))
    };
    assert!(find("hls-live-ends").may_be_unsupported_in(BrowserKind::Firefox));
    assert!(!find("hls-live-ends").may_be_unsupported_in(BrowserKind::Chromium));
    assert!(!find("mse-live-ends").may_be_unsupported_in(BrowserKind::Firefox));
    assert!(!find("mse-live-ends").may_be_unsupported_in(BrowserKind::Chromium));
}

#[test]
fn the_page_ids_are_the_two_rows_in_run_order() {
    assert_eq!(
        ROWS.map(LiveEndsRow::page_id),
        ["mse-live-ends", "hls-live-ends"]
    );
}

/// Страница берёт эфир из маршрута стенда с префиксом, который знает сервер, и не путает его с `hls-live`.
#[test]
fn the_hls_page_plays_the_ending_stand_route() {
    let source = page_source("hls-live-ends");
    assert!(source.contains(crate::site::HLS_LIVE_ENDS_ROUTE_PREFIX));
    assert!(!source.contains(crate::site::HLS_LIVE_ROUTE_PREFIX));
}

/// Метрики, по которым судья принимает страницу, действительно пишет общий наблюдатель страниц.
#[test]
fn the_observer_writes_every_metric_the_judge_reads() {
    let page_code = page_source("mse-live-ends");
    for metric in [
        "on_air_duration_infinite",
        "duration_changes_after_on_air",
        "final_duration_finite",
        "final_duration_s",
        "ended_seen",
    ] {
        assert!(
            page_code.contains(&format!("'{metric}'")),
            "the page code never writes the metric '{metric}'"
        );
    }
}

/// Эфир кончается позже, чем робот успевает передать вкладку и увидеть эфир под тенью: порог
/// сервера оставляет не меньше 20 с (публикация раз в 2 с, начальное окно 4 сегмента).
#[test]
fn the_server_keeps_the_stream_on_air_long_enough_for_the_hand_off() {
    const PUBLISH_PERIOD_S: usize = 2;
    let on_air_s = (crate::site::HLS_LIVE_ENDS_AFTER_PUBLISHED_SEGMENTS
        - crate::site::HLS_LIVE_INITIAL_WINDOW_SEGMENTS)
        * PUBLISH_PERIOD_S;
    assert!(on_air_s >= 20, "the stream is on air for only {on_air_s} s");
}
