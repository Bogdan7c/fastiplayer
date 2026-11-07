//! `local-address` для родного HLS (сессия 10a2, решение 199 на КАЖДЫЙ ресурс): страница
//! `hls-local-segments` открывается под «публичным» именем (`public.bench.example`). Её корневой
//! плейлист публичный, первые сегменты тоже (иначе Chromium под публичным именем не начинает
//! играть), а остальные — абсолютные адреса `http://127.0.0.1:<порт стенда>/media-local-target/…`
//! (плейлист на лету строит маршрут `/cdn/hls-local-segments/`, см. `site::hls_local_segments`).
//! Отдельный префикс `/media-local-target/` отдаёт те же файлы, что `/media/`, и позволяет отличить
//! локальные запросы от публичных в журнале сервера независимо от `Host`.
//!
//! Что ждём:
//! - передача начинается (`started`): адрес корневого плейлиста публичный, проверка его пропускает;
//! - фон качает публичный плейлист и публичные сегменты, но НИ ОДНОГО запроса к
//!   `/media-local-target/` не делает: локальные сегменты закрыты проверкой на каждый ресурс
//!   (запросы самого `<video>` страницы — дело браузера, в счёт идут только запросы скрипта после
//!   передачи);
//! - кадры из публичной части плеер показывать вправе; сеанс обязан кончиться честно (дальше
//!   публичных сегментов поток не идёт). Причина конца не важна: сейчас плеер называет отказ на
//!   закрытом сегменте `unparsable_media` (известное ограничение, backlog 39), поэтому принимаем
//!   любой честный конец — сообщение о конце сеанса, отказ поколения или остановку воспроизведения.
//!
//! Судья [`judge_local_segments`] чистый и проверяется тестами на синтетических журналах.

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::super::outcome::{StepResult, Verdict};
use super::super::run_tweaks::RunTweaks;
use super::judge_early_report;
use super::local_address::{DIRECT_QUERY, PUBLIC_HOST};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::{BenchEnvironment, PlayerConfigProfile};
use crate::observe::{HostLogEvidence, parse_summary, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::site::{MediaRequest, MediaRequester};

/// Страница стенда.
pub(in crate::scenarios) const LOCAL_SEGMENTS_PAGE: &str = "hls-local-segments";
/// Подпись строки в таблице результатов.
const CASE_NAME: &str = "hls-local-segments-are-not-fetched";
/// Публичный плейлист, который обязан скачать фон (маршрут стенда + путь корневого плейлиста).
const PUBLIC_PLAYLIST_PATH: &str = "/cdn/hls-local-segments/hls-ts-long/stream.m3u8";
/// Каталог публичных сегментов: фон обязан скачать отсюда хотя бы один.
const PUBLIC_SEGMENTS_DIRECTORY: &str = "/media/hls-ts-long/";
/// Префикс локальных сегментов: ни одного запроса скрипта после передачи сюда быть не должно.
const LOCAL_SEGMENTS_DIRECTORY: &str = "/media-local-target/";
/// Строки журнала плеера, по которым узнаём честный конец сеанса.
const HONEST_END_MARKERS: [&str; 3] = [
    // Окно показало зрителю сообщение о конце сеанса.
    "Browser session end notice shown",
    // Плеер отказал поколению (первый сегмент не получен).
    "Stream generation rejected",
    // Плеер не смог проиграть поток.
    "Browser stream playback failed",
];
/// Строки журнала плеера, которых быть не должно (паника). `ERROR` здесь допустим: честный
/// отказ плеера пишет свою ошибку, и сценарий проверяет именно этот отказ.
const FORBIDDEN_PLAYER_LINES: &[&str] = &["panic"];
/// Сколько ждём конец сеанса: бюджет повторов профиля `ShortNetworkRetry` (3 с) с большим запасом.
const END_TIMEOUT: Duration = Duration::from_secs(40);

/// Единственный случай: прогон в свежем окружении.
pub(in crate::scenarios) fn run(stand: &Stand, browser: BrowserKind) -> StepResult {
    let started = Instant::now();
    // Короткий бюджет повторов: плеер не ждёт минуту, прежде чем честно отказать.
    let verdict = stand.with_environment_for_player_config(
        PlayerConfigProfile::ShortNetworkRetry,
        |environment| drive(stand, environment, browser, started),
    );
    StepResult::new(
        "local-address",
        browser,
        CASE_NAME,
        verdict,
        started.elapsed(),
    )
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    run_started_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let tweaks = RunTweaks {
        loopback_host_aliases: vec![PUBLIC_HOST.to_owned()],
        page_host: Some(PUBLIC_HOST.to_owned()),
        ..RunTweaks::default()
    };
    let mut run = HandoffRun::open_tweaked(
        stand,
        environment,
        browser_kind,
        LOCAL_SEGMENTS_PAGE,
        LOCAL_SEGMENTS_PAGE,
        ExtensionMode::Real,
        DIRECT_QUERY,
        &tweaks,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(LOCAL_SEGMENTS_PAGE, browser_kind, &report);
            run.browser.shutdown()?;
            return Ok(verdict);
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!(
                "the page never became ready (does the browser resolve {PUBLIC_HOST}?)\n{log}"
            )));
        }
    }
    let handed_off_at = Instant::now();
    let outcome = run.hand_off().map_err(|error| error.to_string());
    run.tell_page("handed-off");

    let honest_end = match &outcome {
        Ok(HandOffOutcome::Started) => {
            match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
                Ok(()) => press_play_then_wait_for_the_end(&mut run, environment),
                Err(message) => Err(format!("{message}; the end was not awaited")),
            }
        }
        other => Err(format!("hand_off_tab: {other:?}; the end was not awaited")),
    };
    let max_presented_frames = max_presented_frames(&run.player_log.snapshot_summary_lines()?);
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let page_log = format_browser_log(&run.take_page_log());
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    let mut requests = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    Ok(judge_local_segments(&LocalSegmentsEvidence {
        outcome,
        honest_end,
        host,
        requests,
        handed_off_at,
        max_presented_frames,
        forbidden_player_lines,
        page_log,
    }))
}

/// Зритель нажимает play и ждёт честный конец сеанса. Элемент страницы под публичным именем может
/// не начать играть (Chromium не всегда стартует HLS с локальными сегментами), и тогда плеер
/// принимает сеанс на паузе и до закрытых сегментов не дойдёт, пока его не запустят.
fn press_play_then_wait_for_the_end(
    run: &mut HandoffRun<'_>,
    environment: &BenchEnvironment,
) -> Result<String, String> {
    let bus_address = environment
        .bus_address()
        .map_err(|error| format!("no player bus: {error}"))?;
    let control = PlayerControl::on_bus(bus_address);
    control
        .wait_until_controllable(SESSION_GRANTED_TIMEOUT)
        .map_err(|error| format!("the player never became controllable: {error}"))?;
    control.play().map_err(|error| format!("Play: {error}"))?;
    run.player_log
        .wait_for_any_line(&HONEST_END_MARKERS, END_TIMEOUT)
        .map_err(|error| format!("the session did not end honestly: {error}"))
}

/// Наибольшее число показанных кадров среди сводок плеера (`0` — сводок нет или кадров не было).
fn max_presented_frames(summary_lines: &[String]) -> u64 {
    summary_lines
        .iter()
        .filter_map(|line| parse_summary(line))
        .map(|summary| summary.presented_frames)
        .max()
        .unwrap_or(0)
}

/// Вход чистого приговора.
struct LocalSegmentsEvidence {
    outcome: Result<HandOffOutcome, String>,
    /// Строка журнала плеера о честном конце сеанса.
    honest_end: Result<String, String>,
    host: HostLogEvidence,
    /// Запросы медиа этого прогона.
    requests: Vec<MediaRequest>,
    handed_off_at: Instant,
    max_presented_frames: u64,
    forbidden_player_lines: Vec<String>,
    page_log: String,
}

/// Чистый приговор: передача началась, фон взял публичный плейлист, но ни одного локального
/// сегмента; кадров нет; конец честный.
fn judge_local_segments(evidence: &LocalSegmentsEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    if !matches!(evidence.outcome, Ok(HandOffOutcome::Started)) {
        failures.push(format!(
            "hand_off_tab: {:?}, expected started: the root playlist is public and must pass the check",
            evidence.outcome
        ));
    }
    if evidence.host.session_grants != 1 {
        failures.push(format!(
            "the player granted {} session(s), expected 1",
            evidence.host.session_grants
        ));
    }
    let background_after_hand_off = |request: &&MediaRequest| {
        request.requester() == MediaRequester::Script && request.at >= evidence.handed_off_at
    };
    let background_requests = || evidence.requests.iter().filter(background_after_hand_off);
    let public_playlist_fetches = background_requests()
        .filter(|request| request.path == PUBLIC_PLAYLIST_PATH)
        .count();
    if public_playlist_fetches == 0 {
        failures.push(
            "the background never fetched the public root playlist: the ruler proves nothing"
                .to_owned(),
        );
    }
    let public_segment_fetches = background_requests()
        .filter(|request| request.path.starts_with(PUBLIC_SEGMENTS_DIRECTORY))
        .count();
    if public_segment_fetches == 0 {
        failures.push(
            "the background fetched no public segment: the hand-off did not carry the public part"
                .to_owned(),
        );
    }
    let local_fetches: Vec<&MediaRequest> = background_requests()
        .filter(|request| request.path.starts_with(LOCAL_SEGMENTS_DIRECTORY))
        .collect();
    if let Some(first) = local_fetches.first() {
        failures.push(format!(
            "the extension background fetched {} resource(s) from the local address, first: {}",
            local_fetches.len(),
            first.headers_summary()
        ));
    }
    if let Err(message) = &evidence.honest_end {
        failures.push(message.clone());
    }
    failures.extend(
        evidence
            .forbidden_player_lines
            .iter()
            .map(|line| format!("forbidden player line: {line}")),
    );
    let summary = format!(
        "outcome={:?}; background fetches of the public playlist={public_playlist_fetches}; background fetches of public segments={public_segment_fetches}; background fetches from the local address={}; presented frames={}; end: {:?}",
        evidence.outcome,
        local_fetches.len(),
        evidence.max_presented_frames,
        evidence.honest_end
    );
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;

    fn request(path: &str, dest: Option<&str>, at: Instant) -> MediaRequest {
        MediaRequest {
            at,
            path: path.to_owned(),
            status: 200,
            range: None,
            cookie: None,
            referer: None,
            origin: None,
            sec_fetch_site: None,
            sec_fetch_mode: None,
            sec_fetch_dest: dest.map(str::to_owned),
            page_fetch_marker: None,
            content_range: None,
        }
    }

    /// Честный прогон: фон взял публичный плейлист, сегменты (в журнале — запросы элемента до
    /// передачи) на локальном адресе не тронул, кадры публичной части есть, сеанс кончился сообщением.
    fn healthy() -> LocalSegmentsEvidence {
        let handed_off_at = Instant::now();
        LocalSegmentsEvidence {
            outcome: Ok(HandOffOutcome::Started),
            honest_end: Ok("Browser session end notice shown notice=stream_error".into()),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            requests: vec![
                request(
                    PUBLIC_PLAYLIST_PATH,
                    Some("video"),
                    handed_off_at - Duration::from_secs(1),
                ),
                // Элемент до передачи сам ходил и на публичный, и на локальный сегмент: это дело браузера.
                request(
                    "/media/hls-ts-long/seg-00000.ts",
                    Some("video"),
                    handed_off_at - Duration::from_millis(500),
                ),
                request(
                    "/media-local-target/hls-ts-long/seg-00003.ts",
                    Some("video"),
                    handed_off_at - Duration::from_millis(400),
                ),
                // Фон после передачи берёт публичный сегмент.
                request(
                    "/media/hls-ts-long/seg-00001.ts",
                    Some("empty"),
                    handed_off_at + Duration::from_millis(300),
                ),
                request(
                    PUBLIC_PLAYLIST_PATH,
                    Some("empty"),
                    handed_off_at + Duration::from_millis(200),
                ),
            ],
            handed_off_at,
            max_presented_frames: 25,
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    #[test]
    fn a_background_that_refuses_the_local_segments_passes() {
        let verdict = judge_local_segments(&healthy());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    }

    #[test]
    fn a_background_script_request_to_a_local_segment_fails() {
        let mut evidence = healthy();
        evidence.requests.push(request(
            "/media-local-target/hls-ts-long/seg-00003.ts",
            None,
            evidence.handed_off_at + Duration::from_secs(1),
        ));
        let verdict = judge_local_segments(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("from the local address"));
    }

    #[test]
    fn a_missing_public_playlist_fetch_means_the_ruler_proves_nothing() {
        let mut evidence = healthy();
        evidence
            .requests
            .retain(|request| request.sec_fetch_dest.as_deref() != Some("empty"));
        let verdict = judge_local_segments(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("never fetched the public root playlist")
        );
    }

    #[test]
    fn no_public_segment_a_refused_hand_off_and_a_missing_end_each_fail() {
        let mut no_public = healthy();
        no_public
            .requests
            .retain(|request| !request.path.ends_with("seg-00001.ts"));
        let verdict = judge_local_segments(&no_public);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("no public segment"));

        let mut refused = healthy();
        refused.outcome = Ok(HandOffOutcome::LocalAddressRefused);
        let verdict = judge_local_segments(&refused);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("expected started"));

        let mut no_end = healthy();
        no_end.honest_end = Err("the session did not end honestly: timeout".into());
        assert_eq!(judge_local_segments(&no_end).outcome, Outcome::Fail);

        let mut panicked = healthy();
        panicked.forbidden_player_lines = vec!["thread panicked".into()];
        assert_eq!(judge_local_segments(&panicked).outcome, Outcome::Fail);
    }

    #[test]
    fn max_presented_frames_reads_the_summaries_and_defaults_to_zero() {
        assert_eq!(max_presented_frames(&[]), 0);
        assert_eq!(max_presented_frames(&["not a summary".to_owned()]), 0);
    }
}
