//! `local-address` (09c, решение 199): страница из интернета просит скачать файл с ЛОКАЛЬНОГО
//! адреса — это вектор атаки на домашнюю сеть, поэтому фон без согласия человека такой файл не
//! качает. Стенд делает «страницу из интернета» из своего сервера: имя `public.bench.example`
//! браузер разрешает в `127.0.0.1` (Chromium `--host-resolver-rules`, Firefox
//! `network.dns.localDomains`), страница `video-src-local` открывается под этим именем, а её
//! `<video src>` указывает на `http://127.0.0.1:<порт стенда>`.
//!
//! Случаи (каждый в свежем окружении):
//!   - настройка по умолчанию («блокировать») → `local_address_refused`;
//!   - настройка «спрашивать» → `local_address_needs_consent{host}` (вопрос рисует попап 10);
//!   - согласие человека на этот хост (`consented_local_host` в сообщении робота) при настройке
//!     «спрашивать» → `started` и кадры (попап спрашивает только в режиме `ask`, как в настоящем потоке);
//!   - согласие на ДРУГОЙ хост → новый вопрос про настоящий хост, сеанса нет: страница могла сменить
//!     файл, пока человек думал;
//!   - то же согласие при настройке «блокировать» → всё равно `local_address_refused`: согласие
//!     никогда не отменяет `block` (решение 199);
//!   - файл под публичным именем отвечает 302 на `127.0.0.1` → передача начинается (адрес публичный),
//!     но запрос на локальный адрес закрывает браузер, правилом `block` фона: журнал сервера не видит фона
//!     на файле, плеер честно отказывается от поколения.
//!
//! В отказах: сеанса с плеером нет, фон файл не качал, браузер играет сам.

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, page_failures,
    unexpected_player_lines, wait_for_session_granted,
};
use super::super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::super::outcome::{StepResult, Verdict};
use super::super::run_tweaks::RunTweaks;
use super::file_evidence::{FilePage, file_failures, file_summary, sort_requests};
use super::judge::{CaptureEvidence, judge_capture};
use super::refusals::{REFUSAL_WINDOW, browser_keeps_playing_failures, browser_real_frame_metrics};
use super::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, judge_early_report};
use crate::browsers::{BrowserKind, HandOffOptions, HandOffOutcome};
use crate::environment::{BenchEnvironment, PlayerConfigProfile};
use crate::observe::{HostLogEvidence, read_host_evidence};
use crate::site::{MediaRequest, MediaRequester, PageReport, SiteError};

/// Страница стенда: `<video src>` на локальном адресе.
pub(in crate::scenarios) const LOCAL_PAGE: &str = "video-src-local";
/// Файл страницы на сервере стенда (тот же ролик, что у `video-src`) и ожидания от его запросов.
pub(in crate::scenarios) const LOCAL_FILE_PAGE: FilePage = FilePage {
    id: LOCAL_PAGE,
    file_path: "/media/progressive/h264-aac-long.mp4",
    required_cookie: None,
};
/// «Публичное» имя, под которым открывается страница (ведёт на сервер стенда).
pub(in crate::scenarios) const PUBLIC_HOST: &str = "public.bench.example";
/// Хост файла, который назовёт фон в вопросе о согласии (адрес сервера стенда).
const LOCAL_FILE_HOST: &str = "127.0.0.1";
/// Хост, на который «человек согласился» в случае с чужим согласием: не тот, что у файла.
const OTHER_LOCAL_HOST: &str = "192.168.1.50";
/// Куда страница ставит файл в случае с перенаправлением: сервер отвечает 302 на `/media/…`.
const REDIRECT_PATH: &str = "/cdn/redirect-local/progressive/h264-aac-long.mp4";
/// Строка плеера о честном отказе от поколения: заслон закрыл файл, повторы кончились.
const GENERATION_REJECTED_LINE: &str = "Stream generation rejected";
/// Сколько ждём честный конец: бюджет повторов профиля `ShortNetworkRetry` (3 с) с запасом.
const REDIRECT_END_TIMEOUT: Duration = Duration::from_secs(20);
/// Что страница добавляет к адресу, чтобы взять файл с перенаправлением.
const REDIRECT_QUERY: &str = "&mode=handoff&file_route=redirect-local";
/// Обычный запрос режима передачи.
pub(in crate::scenarios) const DIRECT_QUERY: &str = "&mode=handoff";
/// Ключ настройки в `storage.local` экстеншена (значения `block` | `ask`, по умолчанию `block`).
/// Зеркало ключа в `extensions/browser/src/shared/` — сверить с экстеншеном при первом живом прогоне.
const LOCAL_ADDRESS_SETTING_KEY: &str = "localAddressSetting";

/// Что должен ответить фон.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedLocal {
    Refused,
    NeedsConsent,
    Started,
    /// Передача начата, но файл закрыт заслоном и поколение кончилось отказом плеера.
    RedirectBlocked,
}

/// Согласие человека в сообщении робота.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consent {
    None,
    /// На хост файла: то, что сказал бы человек в ответ на вопрос.
    ForFileHost,
    /// На другой хост: вопрос был про другой файл.
    ForOtherHost,
}

/// Случай проверки.
#[derive(Debug, Clone, Copy)]
struct LocalAddressCase {
    name: &'static str,
    /// Значение настройки (`None` — не трогать, остаётся умолчание «блокировать»).
    setting: Option<&'static str>,
    consent: Consent,
    expected: ExpectedLocal,
}

const CASES: &[LocalAddressCase] = &[
    LocalAddressCase {
        name: "default-blocks",
        setting: None,
        consent: Consent::None,
        expected: ExpectedLocal::Refused,
    },
    LocalAddressCase {
        name: "ask-needs-consent",
        setting: Some("ask"),
        consent: Consent::None,
        expected: ExpectedLocal::NeedsConsent,
    },
    // Попап спрашивает только в режиме `ask`, поэтому и согласие приходит только оттуда.
    LocalAddressCase {
        name: "consent-allows",
        setting: Some("ask"),
        consent: Consent::ForFileHost,
        expected: ExpectedLocal::Started,
    },
    // Человек согласился на один хост, а файл уже на другом: новый вопрос, а не разрешение.
    LocalAddressCase {
        name: "consent-for-another-host-asks-again",
        setting: Some("ask"),
        consent: Consent::ForOtherHost,
        expected: ExpectedLocal::NeedsConsent,
    },
    // Согласие, пришедшее при `block`, ничего не меняет: фон обязан отказать.
    LocalAddressCase {
        name: "consent-under-block-stays-refused",
        setting: None,
        consent: Consent::ForFileHost,
        expected: ExpectedLocal::Refused,
    },
    // Адрес публичный, проверка его пропускает; закрыть запрос на локальный адрес должен браузер.
    LocalAddressCase {
        name: "redirect-to-local-is-blocked",
        setting: None,
        consent: Consent::None,
        expected: ExpectedLocal::RedirectBlocked,
    },
];

/// Сообщение робота для случая.
fn hand_off_options(consent: Consent) -> HandOffOptions {
    match consent {
        Consent::None => HandOffOptions::default(),
        Consent::ForFileHost => HandOffOptions::with_consent_for_host(LOCAL_FILE_HOST),
        Consent::ForOtherHost => HandOffOptions::with_consent_for_host(OTHER_LOCAL_HOST),
    }
}

/// Настройки плеера для случая: перенаправлению нужен короткий бюджет повторов.
fn player_config_for(case: LocalAddressCase, standard: PlayerConfigProfile) -> PlayerConfigProfile {
    if case.expected == ExpectedLocal::RedirectBlocked {
        PlayerConfigProfile::ShortNetworkRetry
    } else {
        standard
    }
}

/// Скрипт страницы настроек: записать значение настройки в `storage.local`.
pub(in crate::scenarios) fn write_setting_function(value: &str) -> String {
    format!(
        "async (api) => {{ await api.storage.local.set({{ {LOCAL_ADDRESS_SETTING_KEY}: {value:?} }}); return 'saved'; }}"
    )
}

pub(in crate::scenarios) fn run_all(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    CASES
        .iter()
        .map(|case| {
            let started = Instant::now();
            let player_config = player_config_for(*case, stand.settings.player_config);
            let verdict = stand.with_environment_for_player_config(player_config, |environment| {
                drive(stand, environment, browser, *case, started)
            });
            StepResult::new(
                "local-address",
                browser,
                case.name,
                verdict,
                started.elapsed(),
            )
        })
        .collect()
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    case: LocalAddressCase,
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
        LOCAL_PAGE,
        LOCAL_PAGE,
        ExtensionMode::Real,
        if case.expected == ExpectedLocal::RedirectBlocked {
            REDIRECT_QUERY
        } else {
            DIRECT_QUERY
        },
        &tweaks,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(LOCAL_PAGE, browser_kind, &report);
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
    if let Some(value) = case.setting {
        let saved = run
            .browser
            .run_in_extension_page(&write_setting_function(value))?;
        if saved != "saved" {
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!(
                "could not write the setting: {saved}"
            )));
        }
    }
    let handed_off_at = Instant::now();
    let outcome = run
        .hand_off_with_options(hand_off_options(case.consent))
        .map_err(|error| error.to_string());
    run.tell_page("handed-off");

    let verdict = if case.expected == ExpectedLocal::Started {
        judge_started_case(stand, run, &outcome, run_started_at, handed_off_at)?
    } else if case.expected == ExpectedLocal::RedirectBlocked {
        observe_blocked_redirect(stand, run, outcome, run_started_at, handed_off_at)?
    } else {
        // Отказ: сеанса нет, браузер играет сам; ждём окно наблюдения страницы.
        std::thread::sleep(REFUSAL_WINDOW);
        let report = run.finish_page();
        let page_log = format_browser_log(&run.take_page_log());
        let exit = run.browser.shutdown()?;
        let host = read_host_evidence(&exit.stderr_log)?;
        let sorted = sorted_file_requests(stand, run_started_at, handed_off_at);
        judge_local_refusal(&LocalRefusalEvidence {
            expected: case.expected,
            outcome,
            host,
            file_requests: sorted,
            report,
            page_log,
        })
    };
    Ok(verdict)
}

/// Запросы к файлу страницы за этот прогон.
pub(in crate::scenarios) fn sorted_file_requests(
    stand: &Stand,
    run_started_at: Instant,
    handed_off_at: Instant,
) -> super::file_evidence::FileRequests {
    let mut requests: Vec<MediaRequest> = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    sort_requests(LOCAL_FILE_PAGE, &requests, handed_off_at)
}

/// Согласие дано: обычная цепочка `capture` — пропуск, кадры плеера, браузер не декодирует, файл
/// качает фон (а не элемент страницы).
fn judge_started_case(
    stand: &Stand,
    mut run: HandoffRun<'_>,
    outcome: &Result<HandOffOutcome, String>,
    run_started_at: Instant,
    handed_off_at: Instant,
) -> Result<Verdict, ScenarioError> {
    if !matches!(outcome, Ok(HandOffOutcome::Started)) {
        let log = format_browser_log(&run.take_page_log());
        let message = format!("hand_off_tab with the consent: {outcome:?}\n{log}");
        // Страница ждёт `finish`, но передачи не было: просто закрываем браузер.
        run.browser.shutdown()?;
        return Ok(Verdict::fail(message));
    }
    let frames = match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        Ok(()) => run
            .player_log
            .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
            .map_err(|error| format!("player did not present frames: {error}")),
        Err(message) => Err(format!("{message}; frames were not awaited")),
    };
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let sorted = sorted_file_requests(stand, run_started_at, handed_off_at);
    let file = Some((
        file_failures(LOCAL_FILE_PAGE, &sorted),
        file_summary(&sorted),
    ));
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_capture(&CaptureEvidence {
        page_id: LOCAL_PAGE,
        frames,
        hook: None,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
        file,
    }))
}

/// Запросы фона к адресу с перенаправлением за этот прогон (шаг, который заслон не закрывает).
fn background_redirect_hops(stand: &Stand, handed_off_at: Instant) -> Vec<MediaRequest> {
    stand
        .site
        .media_requests()
        .into_iter()
        .filter(|request| {
            request.path == REDIRECT_PATH
                && request.at >= handed_off_at
                && request.requester() == MediaRequester::Script
        })
        .collect()
}

/// Передача начата, ждём честный конец: заслон закрыл запрос, плеер исчерпал повторы.
fn observe_blocked_redirect(
    stand: &Stand,
    mut run: HandoffRun<'_>,
    outcome: Result<HandOffOutcome, String>,
    run_started_at: Instant,
    handed_off_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let honest_end = match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        Ok(()) => run
            .player_log
            .wait_for_line(GENERATION_REJECTED_LINE, REDIRECT_END_TIMEOUT)
            .map_err(|error| format!("the player did not reject the generation: {error}")),
        Err(message) => Err(format!("{message}; the end was not awaited")),
    };
    let page_log = format_browser_log(&run.take_page_log());
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_blocked_redirect(&BlockedRedirectEvidence {
        outcome,
        honest_end,
        host,
        redirect_hops: background_redirect_hops(stand, handed_off_at),
        file_requests: sorted_file_requests(stand, run_started_at, handed_off_at),
        forbidden_player_lines,
        page_log,
    }))
}

/// Вход чистого приговора перенаправления.
struct BlockedRedirectEvidence {
    outcome: Result<HandOffOutcome, String>,
    /// Строка плеера об отказе от поколения.
    honest_end: Result<String, String>,
    host: HostLogEvidence,
    redirect_hops: Vec<MediaRequest>,
    file_requests: super::file_evidence::FileRequests,
    forbidden_player_lines: Vec<String>,
    page_log: String,
}

/// Чистый приговор: сеанс был, фон дошёл до публичного адреса, но НЕ до локального; конец честный.
fn judge_blocked_redirect(evidence: &BlockedRedirectEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    if !matches!(evidence.outcome, Ok(HandOffOutcome::Started)) {
        failures.push(format!(
            "hand_off_tab: {:?}, expected started: the public address must pass the check",
            evidence.outcome
        ));
    }
    if evidence.host.session_grants != 1 {
        failures.push(format!(
            "the player granted {} session(s), expected 1",
            evidence.host.session_grants
        ));
    }
    if evidence.redirect_hops.is_empty() {
        failures.push(
            "the background never asked the public address: the ruler proves nothing".to_owned(),
        );
    }
    if let Some(hop) = evidence
        .redirect_hops
        .iter()
        .find(|hop| hop.status != HTTP_FOUND)
    {
        failures.push(format!(
            "the public address did not redirect: {}",
            hop.headers_summary()
        ));
    }
    if evidence.file_requests.element_before.is_empty() {
        failures.push(
            "the page's own <video> never followed the redirect: the ruler is blind".to_owned(),
        );
    }
    if let Some(request) = evidence.file_requests.background.first() {
        failures.push(format!(
            "the extension background reached the local address after the redirect ({} request(s)), first: {}",
            evidence.file_requests.background.len(),
            request.headers_summary()
        ));
    }
    if let Err(message) = &evidence.honest_end {
        failures.push(message.clone());
    }
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let summary = format!(
        "outcome={:?}; background hops to the public address={}; background requests to the local file={}; end: {:?}",
        evidence.outcome,
        evidence.redirect_hops.len(),
        evidence.file_requests.background.len(),
        evidence.honest_end
    );
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Ответ сервера «перенаправление».
const HTTP_FOUND: u16 = 302;

/// Вход чистого приговора для отказов.
struct LocalRefusalEvidence {
    expected: ExpectedLocal,
    outcome: Result<HandOffOutcome, String>,
    host: HostLogEvidence,
    file_requests: super::file_evidence::FileRequests,
    report: Result<PageReport, SiteError>,
    page_log: String,
}

/// Чистый приговор отказа: правильный исход, сеанса нет, фон файл не качал, браузер играет сам.
fn judge_local_refusal(evidence: &LocalRefusalEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    match (&evidence.outcome, evidence.expected) {
        (Ok(HandOffOutcome::LocalAddressRefused), ExpectedLocal::Refused) => {}
        (Ok(HandOffOutcome::LocalAddressNeedsConsent { host }), ExpectedLocal::NeedsConsent)
            if host == LOCAL_FILE_HOST => {}
        (Ok(other), expected) => failures.push(format!(
            "hand_off_tab outcome {other:?}, expected {expected:?} (host {LOCAL_FILE_HOST})"
        )),
        (Err(error), _) => failures.push(format!("hand_off_tab: {error}")),
    }
    if evidence.host.session_grants != 0 {
        failures.push(format!(
            "the player granted {} session(s) for a local-address file",
            evidence.host.session_grants
        ));
    }
    if evidence.file_requests.element_before.is_empty() {
        failures.push(
            "the page's own <video> never loaded the local file: the ruler is blind".to_owned(),
        );
    }
    if let Some(request) = evidence.file_requests.background.first() {
        failures.push(format!(
            "the extension background downloaded the local file without consent: {}",
            request.headers_summary()
        ));
    }
    failures.extend(page_failures(&evidence.report, LOCAL_PAGE));
    let (window_s, real_frames) = browser_real_frame_metrics(&evidence.report);
    failures.extend(browser_keeps_playing_failures(window_s, real_frames));
    let summary = format!(
        "outcome={:?}; session_grants={}; {}",
        evidence.outcome,
        evidence.host.session_grants,
        file_summary(&evidence.file_requests)
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
    use crate::scenarios::capture::file_evidence::FileRequests;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};

    fn request(dest: &str) -> MediaRequest {
        MediaRequest {
            at: Instant::now(),
            path: "/media/progressive/h264-aac-long.mp4".into(),
            status: 206,
            range: Some("bytes=0-".into()),
            cookie: None,
            referer: None,
            origin: None,
            sec_fetch_site: None,
            sec_fetch_mode: None,
            sec_fetch_dest: Some(dest.into()),
            page_fetch_marker: None,
            content_range: None,
        }
    }

    fn report(real_frames: f64) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: LOCAL_PAGE.into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: [
                ("shadow_window_s".to_owned(), 5.0),
                ("real_frames_in_window".to_owned(), real_frames),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn evidence(expected: ExpectedLocal, outcome: HandOffOutcome) -> LocalRefusalEvidence {
        LocalRefusalEvidence {
            expected,
            outcome: Ok(outcome),
            host: HostLogEvidence::default(),
            file_requests: FileRequests {
                element_before: vec![request("video")],
                ..FileRequests::default()
            },
            report: Ok(report(120.0)),
            page_log: String::new(),
        }
    }

    fn needs_consent(host: &str) -> HandOffOutcome {
        HandOffOutcome::LocalAddressNeedsConsent { host: host.into() }
    }

    #[test]
    fn honest_refusals_pass() {
        for (expected, outcome) in [
            (ExpectedLocal::Refused, HandOffOutcome::LocalAddressRefused),
            (ExpectedLocal::NeedsConsent, needs_consent(LOCAL_FILE_HOST)),
        ] {
            let verdict = judge_local_refusal(&evidence(expected, outcome));
            assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        }
    }

    #[test]
    fn a_wrong_answer_a_session_a_download_or_a_frozen_browser_fails() {
        let refused = || evidence(ExpectedLocal::Refused, HandOffOutcome::LocalAddressRefused);
        let mut cases: Vec<(&str, LocalRefusalEvidence)> = vec![
            (
                "ask-answer where block is expected",
                evidence(ExpectedLocal::Refused, needs_consent(LOCAL_FILE_HOST)),
            ),
            (
                "block-answer where ask is expected",
                evidence(
                    ExpectedLocal::NeedsConsent,
                    HandOffOutcome::LocalAddressRefused,
                ),
            ),
            (
                "consent asked about another host",
                evidence(ExpectedLocal::NeedsConsent, needs_consent("example.com")),
            ),
            (
                "started",
                evidence(ExpectedLocal::Refused, HandOffOutcome::Started),
            ),
        ];
        let mut with_session = refused();
        with_session.host.session_grants = 1;
        cases.push(("a session was granted", with_session));
        let mut downloaded = refused();
        downloaded.file_requests.background.push(request("empty"));
        cases.push(("the background downloaded the file", downloaded));
        let mut blind = refused();
        blind.file_requests.element_before.clear();
        cases.push(("the ruler is blind", blind));
        let mut frozen = refused();
        frozen.report = Ok(report(3.0));
        cases.push(("the browser stopped playing", frozen));
        for (what, case) in cases {
            assert_eq!(judge_local_refusal(&case).outcome, Outcome::Fail, "{what}");
        }
    }

    #[test]
    fn the_setting_script_writes_the_exact_key_and_value() {
        let script = write_setting_function("ask");
        assert!(script.contains("localAddressSetting: \"ask\""), "{script}");
        assert!(script.contains("storage.local.set"));
    }

    #[test]
    fn the_cases_cover_block_ask_consent_other_host_block_and_redirect() {
        let shape: Vec<_> = CASES
            .iter()
            .map(|case| (case.setting, case.consent, case.expected))
            .collect();
        assert_eq!(
            shape,
            vec![
                (None, Consent::None, ExpectedLocal::Refused),
                (Some("ask"), Consent::None, ExpectedLocal::NeedsConsent),
                (Some("ask"), Consent::ForFileHost, ExpectedLocal::Started),
                (
                    Some("ask"),
                    Consent::ForOtherHost,
                    ExpectedLocal::NeedsConsent
                ),
                (None, Consent::ForFileHost, ExpectedLocal::Refused),
                (None, Consent::None, ExpectedLocal::RedirectBlocked),
            ]
        );
    }

    #[test]
    fn the_robot_names_the_host_the_case_consents_to() {
        assert_eq!(hand_off_options(Consent::None), HandOffOptions::default());
        assert_eq!(
            hand_off_options(Consent::ForFileHost),
            HandOffOptions::with_consent_for_host(LOCAL_FILE_HOST)
        );
        assert_eq!(
            hand_off_options(Consent::ForOtherHost),
            HandOffOptions::with_consent_for_host(OTHER_LOCAL_HOST)
        );
    }

    #[test]
    fn only_the_redirect_case_shortens_the_player_retries() {
        let redirect = CASES
            .iter()
            .find(|case| case.expected == ExpectedLocal::RedirectBlocked)
            .expect("redirect case");
        assert_eq!(
            player_config_for(*redirect, PlayerConfigProfile::Clean),
            PlayerConfigProfile::ShortNetworkRetry
        );
        assert_eq!(
            player_config_for(CASES[0], PlayerConfigProfile::Gles2Vaapi),
            PlayerConfigProfile::Gles2Vaapi
        );
    }

    fn hop(status: u16) -> MediaRequest {
        MediaRequest {
            path: REDIRECT_PATH.into(),
            status,
            ..request("empty")
        }
    }

    fn blocked_redirect() -> BlockedRedirectEvidence {
        BlockedRedirectEvidence {
            outcome: Ok(HandOffOutcome::Started),
            honest_end: Ok(GENERATION_REJECTED_LINE.to_owned()),
            host: HostLogEvidence {
                session_grants: 1,
                ..HostLogEvidence::default()
            },
            redirect_hops: vec![hop(HTTP_FOUND)],
            file_requests: FileRequests {
                element_before: vec![request("video")],
                ..FileRequests::default()
            },
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    #[test]
    fn a_blocked_redirect_with_an_honest_end_passes() {
        let verdict = judge_blocked_redirect(&blocked_redirect());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    }

    #[test]
    fn a_background_request_to_the_local_file_fails_the_redirect_case() {
        let mut reached = blocked_redirect();
        reached.file_requests.background.push(request("empty"));
        let verdict = judge_blocked_redirect(&reached);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("reached the local address"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_blind_ruler_or_a_dishonest_end_fails_the_redirect_case() {
        let mut cases: Vec<(&str, BlockedRedirectEvidence)> = Vec::new();
        let mut never_asked = blocked_redirect();
        never_asked.redirect_hops.clear();
        cases.push(("the background never asked", never_asked));
        let mut no_redirect = blocked_redirect();
        no_redirect.redirect_hops = vec![hop(206)];
        cases.push(("the address did not redirect", no_redirect));
        let mut blind = blocked_redirect();
        blind.file_requests.element_before.clear();
        cases.push(("the element never played", blind));
        let mut no_end = blocked_redirect();
        no_end.honest_end = Err("no rejection".into());
        cases.push(("no honest end", no_end));
        let mut refused = blocked_redirect();
        refused.outcome = Ok(HandOffOutcome::LocalAddressRefused);
        cases.push(("refused before the session", refused));
        let mut no_session = blocked_redirect();
        no_session.host.session_grants = 0;
        cases.push(("no session", no_session));
        let mut crashed = blocked_redirect();
        crashed.forbidden_player_lines = vec!["T ERROR something broke".to_owned()];
        cases.push(("a player error", crashed));
        for (what, case) in cases {
            let verdict = judge_blocked_redirect(&case);
            assert_eq!(verdict.outcome, Outcome::Fail, "{what}");
        }
    }
}
