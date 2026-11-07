//! Чистый приговор `hls-live` (сессия 10a3): по журналу сервера, шагам робота, кадрам плеера и
//! отчёту страницы. Браузера и плеера здесь нет — проверяется тестами на синтетических журналах.
//!
//! Что доказывается сервером (журнал `/cdn/hls-live/<прогон>/…`):
//! - ДО передачи плейлист просил сам `<video>` (`Sec-Fetch-Dest: video`) — линейка видит элемент;
//! - ПОД ТЕНЬЮ элемент не просит НИЧЕГО (ни плейлист, ни сегменты);
//! - фон после передачи перезапрашивает плейлист не меньше [`MIN_BACKGROUND_PLAYLIST_FETCHES`]
//!   раз (живой плейлист обновляется) с `Sec-Fetch-Dest: empty` и дошёл до сегмента с номером
//!   за пределами начального окна (эфир двигается, а не стоит);
//! - после возврата настоящий элемент снова сам просит плейлист (его `src` перезагружен) и грузит
//!   СВЕЖЕЕ окно: первый его сегмент не старше окна, в котором шёл фон перед закрытием плеера.
//!   У нативного Chromium `seekable` пуст, а `currentTime` отсчитывается от начала загрузки, поэтому
//!   «стоит у края эфира» по странице не измерить; журнал сервера это видит точно.

use std::time::Instant;

use super::super::capture::file_evidence::IN_FLIGHT_GRACE;
use super::super::capture::hls_evidence::segment_index;
use super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::outcome::Verdict;
use crate::observe::{FrameGrowth, HostLogEvidence};
use crate::site::{
    HLS_LIVE_INITIAL_WINDOW_SEGMENTS, HLS_LIVE_PLAYLIST_NAME, HLS_LIVE_ROUTE_PREFIX,
    HLS_LIVE_WINDOW_SEGMENTS, MediaRequest, MediaRequester, PageReport, SiteError,
};

/// Сколько раз фон обязан запросить плейлист после передачи (живой плейлист обновляют).
pub(super) const MIN_BACKGROUND_PLAYLIST_FETCHES: usize = 3;
/// Сколько кадров плеер обязан добавить между двумя сводками, пока эфир идёт под тенью.
pub(super) const MIN_FRAMES_WHILE_LIVE: u64 = 100;

/// Результат одного шага робота.
pub(super) struct StepEvidence {
    pub(super) name: &'static str,
    pub(super) result: Result<(), String>,
}

/// Всё, что собрал прогон.
pub(super) struct HlsLiveEvidence {
    pub(super) steps: Vec<StepEvidence>,
    /// Рост кадров плеера, пока эфир идёт под тенью (`Err` — не вырос или шаг не дошёл).
    pub(super) frame_growth: Result<FrameGrowth, String>,
    /// Запросы медиа этого прогона (весь журнал сервера уже отфильтрован по времени прогона).
    pub(super) requests: Vec<MediaRequest>,
    pub(super) handed_off_at: Instant,
    /// Когда закрыли окно плеера (`None` — до возврата не дошли).
    pub(super) returned_at: Option<Instant>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Запросы к эфиру, разложенные по «кто» и «когда».
#[derive(Debug, Default)]
struct LiveRequests<'log> {
    element_before: Vec<&'log MediaRequest>,
    element_under_shadow: Vec<&'log MediaRequest>,
    element_after_return: Vec<&'log MediaRequest>,
    background_playlists: Vec<&'log MediaRequest>,
    background_segments: Vec<&'log MediaRequest>,
}

fn is_playlist(request: &MediaRequest) -> bool {
    request.path.ends_with(HLS_LIVE_PLAYLIST_NAME)
}

/// Раскладывает журнал: допуск [`IN_FLIGHT_GRACE`] на запросы «в полёте» вокруг передачи и возврата.
fn sort_requests(
    requests: &[MediaRequest],
    handed_off_at: Instant,
    returned_at: Option<Instant>,
) -> LiveRequests<'_> {
    let mut sorted = LiveRequests::default();
    // Под тенью — до возврата; если до возврата не дошли, тень длится до конца журнала.
    let before_return = |at: Instant| returned_at.is_none_or(|returned| at < returned);
    for request in requests
        .iter()
        .filter(|request| request.path.starts_with(HLS_LIVE_ROUTE_PREFIX))
    {
        match request.requester() {
            MediaRequester::MediaElement => {
                if request.at < handed_off_at {
                    sorted.element_before.push(request);
                } else if request.at >= handed_off_at + IN_FLIGHT_GRACE && before_return(request.at)
                {
                    sorted.element_under_shadow.push(request);
                } else if returned_at.is_some_and(|returned| request.at >= returned) {
                    sorted.element_after_return.push(request);
                }
            }
            MediaRequester::Script => {
                let in_shadow_window = request.at >= handed_off_at && before_return(request.at);
                if in_shadow_window && is_playlist(request) {
                    sorted.background_playlists.push(request);
                } else if in_shadow_window {
                    sorted.background_segments.push(request);
                }
            }
        }
    }
    sorted
}

/// Приговор: провалы собираются все сразу, чтобы разбор шёл по одному прогону.
pub(super) fn judge_hls_live(evidence: &HlsLiveEvidence) -> Verdict {
    let mut failures: Vec<String> = evidence
        .steps
        .iter()
        .filter_map(|step| {
            step.result
                .as_ref()
                .err()
                .map(|message| format!("step '{}': {message}", step.name))
        })
        .collect();
    let gained = match &evidence.frame_growth {
        Ok(growth) if growth.frames_gained() >= MIN_FRAMES_WHILE_LIVE => {
            Some(growth.frames_gained())
        }
        Ok(growth) => {
            failures.push(format!(
                "the player added only {} frame(s) while the live stream ran, need {MIN_FRAMES_WHILE_LIVE}",
                growth.frames_gained()
            ));
            None
        }
        Err(message) => {
            failures.push(message.clone());
            None
        }
    };
    let sorted = sort_requests(
        &evidence.requests,
        evidence.handed_off_at,
        evidence.returned_at,
    );
    failures.extend(server_failures(&sorted, evidence.returned_at.is_some()));
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, "hls-live"));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let highest_background_segment = highest_segment(&sorted.background_segments);
    let summary = format!(
        "frames gained while live: {gained:?} (need {MIN_FRAMES_WHILE_LIVE}); element requests before/under shadow/after return: {}/{}/{}; background playlist fetches: {}; highest background segment: {highest_background_segment:?} (initial window ends at {HLS_LIVE_INITIAL_WINDOW_SEGMENTS})",
        sorted.element_before.len(),
        sorted.element_under_shadow.len(),
        sorted.element_after_return.len(),
        sorted.background_playlists.len(),
    );
    // Цифры окна эфира, которое страница видела под тенью, — в итоговую строку:
    // по ним отчёт сессии показывает, насколько окно уехало, не открывая журнал страницы.
    let summary = format!("{summary}; {}", page_window_metrics(&evidence.page_report));
    let summary = if failures.is_empty() {
        summary
    } else {
        // При провале — запросы элемента после возврата с ответами сервера: так видно, что
        // элемент спросил у сайта, когда заново входил в эфир (разбор сбоев возврата).
        let after_return =
            describe_requests_after(&sorted.element_after_return, evidence.returned_at);
        format!(
            "{summary}\nelement requests after return: {after_return}\n{}",
            evidence.page_log
        )
    };
    Verdict::from_failures(&failures, summary)
}

/// Запросы одной строкой: «+секунды от возврата, имя файла, ответ сервера».
fn describe_requests_after(requests: &[&MediaRequest], returned_at: Option<Instant>) -> String {
    if requests.is_empty() {
        return "none".to_owned();
    }
    requests
        .iter()
        .map(|request| {
            // Имя файла без маршрута: маршрут у всех запросов эфира один.
            let file_name = request.path.rsplit('/').next().unwrap_or(&request.path);
            let offset = returned_at
                .map(|returned| request.at.saturating_duration_since(returned).as_secs_f64())
                .map_or_else(|| "?".to_owned(), |seconds| format!("+{seconds:.2}s"));
            format!("{offset} {file_name} {}", request.status)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Метрики страницы, которые доказывают «эфир под тенью», одной строкой.
///
/// Отсутствующая метрика печатается как `-`: судья выше уже засчитал провал страницы,
/// здесь только показ цифр.
fn page_window_metrics(report: &Result<PageReport, SiteError>) -> String {
    const SHOWN_METRICS: [&str; 4] = [
        "seekable_end_advance_s",
        "seekable_start_s",
        "seekable_end_s",
        "shadow_clock_advance_s",
    ];
    let Ok(report) = report else {
        return "page metrics: no report".to_owned();
    };
    let shown: Vec<String> = SHOWN_METRICS
        .iter()
        .map(|name| match report.metrics.get(*name) {
            Some(value) => format!("{name}={value:.2}"),
            None => format!("{name}=-"),
        })
        .collect();
    format!("page metrics: {}", shown.join(", "))
}

/// Наибольший номер сегмента среди запросов (`None` — сегментов не было).
fn highest_segment(requests: &[&MediaRequest]) -> Option<u32> {
    requests
        .iter()
        .filter_map(|request| segment_index(&request.path))
        .max()
}

/// Провалы по журналу сервера.
fn server_failures(sorted: &LiveRequests<'_>, returned: bool) -> Vec<String> {
    let mut failures = Vec::new();
    if !sorted.element_before.iter().copied().any(is_playlist) {
        failures.push(
            "the page element never requested the live playlist before the hand-off: the ruler proves nothing"
                .to_owned(),
        );
    }
    if let Some(first) = sorted.element_under_shadow.first() {
        failures.push(format!(
            "the real element kept loading the live stream under the shadow ({} request(s)), first: {}",
            sorted.element_under_shadow.len(),
            first.headers_summary()
        ));
    }
    if sorted.background_playlists.len() < MIN_BACKGROUND_PLAYLIST_FETCHES {
        failures.push(format!(
            "the background fetched the live playlist {} time(s), need at least {MIN_BACKGROUND_PLAYLIST_FETCHES}: the playlist is not being refreshed",
            sorted.background_playlists.len()
        ));
    }
    if let Some(wrong) = sorted
        .background_playlists
        .iter()
        .find(|request| request.sec_fetch_dest.as_deref() != Some("empty"))
    {
        failures.push(format!(
            "a background playlist request is not Sec-Fetch-Dest: empty: {}",
            wrong.headers_summary()
        ));
    }
    match highest_segment(&sorted.background_segments) {
        Some(index) if index >= HLS_LIVE_INITIAL_WINDOW_SEGMENTS as u32 => {}
        other => failures.push(format!(
            "the background never went past the initial window (segments 0..{}): highest segment {other:?}",
            HLS_LIVE_INITIAL_WINDOW_SEGMENTS - 1
        )),
    }
    if returned {
        failures.extend(return_failures(sorted));
    }
    failures
}

/// Провалы возврата: элемент перезагрузил `src` и грузит свежее окно (а не «старую секунду»).
fn return_failures(sorted: &LiveRequests<'_>) -> Vec<String> {
    if !sorted.element_after_return.iter().copied().any(is_playlist) {
        return vec![
            "after the return the real element never requested the live playlist again: its src was not reloaded"
                .to_owned(),
        ];
    }
    // Край эфира перед закрытием плеера — самый поздний сегмент, до которого дошёл фон.
    let Some(edge_before_return) = highest_segment(&sorted.background_segments) else {
        return Vec::new(); // нет линейки края: об этом уже сказал провал «фон не вышел за начальное окно»
    };
    let oldest_fresh = edge_before_return.saturating_sub(HLS_LIVE_WINDOW_SEGMENTS as u32);
    let first_segment = sorted
        .element_after_return
        .iter()
        .filter(|request| !is_playlist(request))
        .min_by_key(|request| request.at)
        .and_then(|request| segment_index(&request.path));
    match first_segment {
        Some(index) if index >= oldest_fresh => Vec::new(),
        Some(index) => vec![format!(
            "after the return the real element started from segment {index}, but the live edge was at {edge_before_return} (the oldest fresh segment is {oldest_fresh}): it resumed an old position instead of the live edge"
        )],
        None => vec![
            "after the return the real element requested the playlist but no segment".to_owned(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::observe::FrameEvidence;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};

    const RUN_PATH: &str = "/cdn/hls-live/r-1";

    fn request(name: &str, dest: &str, at: Instant) -> MediaRequest {
        MediaRequest {
            at,
            path: format!("{RUN_PATH}/{name}"),
            status: 200,
            range: None,
            cookie: None,
            referer: None,
            origin: None,
            sec_fetch_site: None,
            sec_fetch_mode: None,
            sec_fetch_dest: Some(dest.to_owned()),
            page_fetch_marker: None,
            content_range: None,
        }
    }

    fn frames(count: u64) -> FrameEvidence {
        FrameEvidence {
            presented_frames: count,
            position_regressions: 0,
            summary_line: String::new(),
        }
    }

    fn healthy() -> HlsLiveEvidence {
        let base = Instant::now();
        let handed_off_at = base + Duration::from_secs(10);
        let returned_at = base + Duration::from_secs(40);
        let after = |seconds: u64| handed_off_at + Duration::from_secs(seconds);
        let mut requests = vec![
            request("stream.m3u8", "video", base + Duration::from_secs(1)),
            request("seg-00000.ts", "video", base + Duration::from_secs(1)),
        ];
        // Фон: плейлист каждые 2 с и сегменты вслед за эфиром.
        for second in (2..28).step_by(2) {
            requests.push(request("stream.m3u8", "empty", after(second)));
            requests.push(request(
                &format!("seg-{:05}.ts", 3 + second / 2),
                "empty",
                after(second),
            ));
        }
        // Возврат: настоящий элемент снова сам просит плейлист.
        requests.push(request(
            "stream.m3u8",
            "video",
            returned_at + Duration::from_secs(1),
        ));
        // Фон дошёл до сегмента 16; элемент после возврата начинает со свежего окна (13..16).
        requests.push(request(
            "seg-00014.ts",
            "video",
            returned_at + Duration::from_millis(1_200),
        ));
        HlsLiveEvidence {
            steps: vec![StepEvidence {
                name: "session-granted",
                result: Ok(()),
            }],
            frame_growth: Ok(FrameGrowth {
                before: frames(300),
                after: frames(700),
            }),
            requests,
            handed_off_at,
            returned_at: Some(returned_at),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            page_report: Ok(PageReport {
                run: RunId::new("r-1").expect("run id"),
                scenario: "hls-live".into(),
                status: PageStatus::Pass,
                checks: Vec::new(),
                user_agent: String::new(),
                elapsed_ms: 0,
                metrics: Default::default(),
            }),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".into(),
        }
    }

    fn failed_with(evidence: &HlsLiveEvidence, needle: &str) {
        let verdict = judge_hls_live(evidence);
        assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
        assert!(verdict.detail.contains(needle), "{}", verdict.detail);
    }

    #[test]
    fn a_live_stream_followed_by_the_background_and_returned_to_the_edge_passes() {
        let verdict = judge_hls_live(&healthy());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        assert!(
            verdict
                .detail
                .contains("frames gained while live: Some(400)")
        );
    }

    #[test]
    fn the_summary_shows_the_live_window_the_page_saw_under_the_shadow() {
        let mut evidence = healthy();
        if let Ok(report) = evidence.page_report.as_mut() {
            report.metrics.insert("seekable_end_advance_s".into(), 12.5);
        }
        let verdict = judge_hls_live(&evidence);
        assert!(
            verdict.detail.contains("seekable_end_advance_s=12.50"),
            "{}",
            verdict.detail
        );
        // Не присланная страницей метрика честно показана прочерком, а не нулём.
        assert!(
            verdict.detail.contains("seekable_start_s=-"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_failed_run_lists_what_the_returned_element_asked_the_site_with_statuses() {
        let mut evidence = healthy();
        evidence.frame_growth = Err("player frames did not grow".into());
        let verdict = judge_hls_live(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
        // Отсчёт — от мига возврата: плейлист через 1 с, сегмент через 1,2 с, с ответами сервера.
        assert!(
            verdict.detail.contains(
                "element requests after return: +1.00s stream.m3u8 200, +1.20s seg-00014.ts 200"
            ),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_passing_run_keeps_the_summary_short_without_the_request_list() {
        let verdict = judge_hls_live(&healthy());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        assert!(
            !verdict.detail.contains("element requests after return:"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_background_stuck_inside_the_initial_window_fails() {
        let mut evidence = healthy();
        evidence.requests.retain(|request| {
            !request.path.ends_with(".ts") || segment_index(&request.path).is_some_and(|n| n < 4)
        });
        failed_with(&evidence, "never went past the initial window");
    }

    #[test]
    fn a_background_that_does_not_refresh_the_playlist_fails() {
        let mut evidence = healthy();
        let mut kept = 0;
        evidence.requests.retain(|request| {
            let background_playlist =
                is_playlist(request) && request.sec_fetch_dest.as_deref() == Some("empty");
            if background_playlist {
                kept += 1;
                kept <= 2
            } else {
                true
            }
        });
        failed_with(&evidence, "need at least 3");
    }

    #[test]
    fn a_playlist_request_with_a_wrong_fetch_destination_fails() {
        let mut evidence = healthy();
        let handed_off_at = evidence.handed_off_at;
        let mut odd = request(
            "stream.m3u8",
            "document",
            handed_off_at + Duration::from_secs(3),
        );
        odd.sec_fetch_dest = None;
        evidence.requests.push(odd);
        failed_with(&evidence, "not Sec-Fetch-Dest: empty");
    }

    #[test]
    fn the_real_element_loading_under_the_shadow_fails() {
        let mut evidence = healthy();
        evidence.requests.push(request(
            "seg-00009.ts",
            "video",
            evidence.handed_off_at + Duration::from_secs(12),
        ));
        failed_with(&evidence, "kept loading the live stream under the shadow");
    }

    #[test]
    fn element_requests_right_after_the_hand_off_are_in_flight_not_a_failure() {
        let mut evidence = healthy();
        evidence.requests.push(request(
            "seg-00001.ts",
            "video",
            evidence.handed_off_at + Duration::from_millis(300),
        ));
        assert_eq!(judge_hls_live(&evidence).outcome, Outcome::Pass);
    }

    #[test]
    fn a_missing_element_request_before_the_hand_off_means_the_ruler_proves_nothing() {
        let mut evidence = healthy();
        evidence
            .requests
            .retain(|request| request.sec_fetch_dest.as_deref() != Some("video"));
        let verdict = judge_hls_live(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("ruler proves nothing"));
        assert!(verdict.detail.contains("src was not reloaded"));
    }

    #[test]
    fn no_playlist_request_after_the_return_means_the_src_was_not_reloaded() {
        let mut evidence = healthy();
        let returned_at = evidence.returned_at.expect("returned");
        evidence
            .requests
            .retain(|request| !(request.at > returned_at && is_playlist(request)));
        failed_with(&evidence, "src was not reloaded");
    }

    #[test]
    fn a_returned_element_that_resumes_an_old_segment_fails() {
        let mut evidence = healthy();
        let returned_at = evidence.returned_at.expect("returned");
        evidence.requests.retain(|request| {
            !(request.at > returned_at
                && request.sec_fetch_dest.as_deref() == Some("video")
                && !is_playlist(request))
        });
        evidence.requests.push(request(
            "seg-00006.ts",
            "video",
            returned_at + Duration::from_millis(1_200),
        ));
        failed_with(&evidence, "resumed an old position");
    }

    #[test]
    fn a_returned_element_that_loads_the_playlist_but_no_segment_fails() {
        let mut evidence = healthy();
        let returned_at = evidence.returned_at.expect("returned");
        evidence.requests.retain(|request| {
            !(request.at > returned_at
                && request.sec_fetch_dest.as_deref() == Some("video")
                && !is_playlist(request))
        });
        failed_with(&evidence, "but no segment");
    }

    #[test]
    fn a_run_that_never_returned_is_judged_without_the_return_rule() {
        let mut evidence = healthy();
        evidence.returned_at = None;
        evidence.steps.push(StepEvidence {
            name: "return-to-page",
            result: Err("Quit: no window".into()),
        });
        let verdict = judge_hls_live(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("step 'return-to-page'"));
        assert!(!verdict.detail.contains("src was not reloaded"));
    }

    #[test]
    fn frames_that_do_not_grow_enough_or_a_failed_page_fail() {
        let mut evidence = healthy();
        evidence.frame_growth = Ok(FrameGrowth {
            before: frames(300),
            after: frames(350),
        });
        failed_with(&evidence, "added only 50 frame(s)");

        let mut stalled = healthy();
        stalled.frame_growth = Err("player frames did not grow".into());
        failed_with(&stalled, "player frames did not grow");

        let mut page_failed = healthy();
        page_failed.page_report = Err(SiteError::ReportTimeout {
            run: RunId::new("r-1").expect("run id"),
            waited: Duration::from_secs(1),
        });
        assert_eq!(judge_hls_live(&page_failed).outcome, Outcome::Fail);
    }
}
