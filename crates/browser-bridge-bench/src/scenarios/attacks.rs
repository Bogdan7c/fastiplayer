//! `attacks` (09c): вредная страница не должна уронить ни экстеншен, ни плеер, ни СЛЕДУЮЩИЙ сеанс.
//!
//! Путь каждой страницы-атаки в одном браузере:
//!   1. честный старт: `started`, пропуск посредника, кадры плеера (иначе атака ничего не значит);
//!   2. команда `hook:attack`: страница начинает вредить под тенью (гигантские сегменты, шквал
//!      событий, мусор вместо fMP4, шквал сообщений воркера);
//!   3. честный конец: плеер показывает сообщение `stream_error` (или, где это честно, отказ
//!      поколения — см. [`EndPolicy`]), окно закрывается, страница дошла до отчёта `pass`;
//!   4. В ТОМ ЖЕ браузере открывается чистая страница `mse-fmp4`: передача → `started`, посредник
//!      выдал ЕЩЁ ОДИН пропуск (`session_grants` вырос), кадры нового окна растут, браузер новый
//!      ролик не декодирует, паники нет.
//!
//! Судья [`judge_attack`] чистый и НЕ расширяется ради прохождения: если продукт переживает атаку
//! без конца сеанса, сценарий честно скажет «конца не было».

mod landing;

use std::time::{Duration, Instant};

use self::landing::{Landing, WORKER_FLOOD_BOUNDS, absorbed_cost_line, landing_failures};
use super::capture::{
    FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, browser_does_not_decode, judge_early_report,
};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines,
};
use super::handoff_run::{ACK_TIMEOUT, ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_ends::start_of_session;
use super::page_filter::PageFilter;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{
    EndNotice, FrameEvidence, HostLogEvidence, ObserveError, parse_end_notice, read_host_evidence,
};
use crate::player_control::PlayerControl;
use crate::site::{PageReport, SiteError};

/// Чистая страница, которая должна сыграть ПОСЛЕ атаки в том же браузере.
const FOLLOW_UP_PAGE: &str = "mse-fmp4";
/// Сообщение окна, которое честно говорит об обрыве потока (`Browser session end notice shown notice=…`).
const STREAM_ERROR_NOTICE: &str = "stream_error";
/// Строка журнала плеера: отказ поколения (мусорный fMP4 плеер может отвергнуть, не обрывая сеанс).
const GENERATION_REJECTED_MARKER: &str = "Stream generation rejected";
/// Строка журнала плеера: плеер не смог проиграть поток страницы.
const PLAYBACK_FAILED_MARKER: &str = "Browser stream playback failed";
/// Причина отказа поколения, которую обязан назвать плеер на мусоре.
const UNPARSABLE_MEDIA_REASON: &str = "reason=UnparsableMedia";
/// Причина конца: сеанс закрыла сторона страницы (экстеншен), а не падение и не уход вкладки.
const PAGE_STREAM_ERROR_CAUSE: &str = "page_stream_error";
/// Ошибка демуксера после мусора: сеанс уже закрыт, строка только фиксирует причину в журнале.
const GARBAGE_DEMUX_ERROR: &str = "PlayerEvent::FatalError kind=DemuxError";
/// Строка журнала: окно показало сообщение о конце сеанса.
const END_NOTICE_MARKER: &str = "Browser session end notice shown";
/// Сколько ждём конец сеанса после команды атаки (шквал идёт ~4 с, плеер разбирается ещё).
const END_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько окно плеера может висеть после сообщения о конце.
const WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(20);
/// Сколько ждём второй пропуск посредника на чистой странице.
const SECOND_GRANT_TIMEOUT: Duration = SESSION_GRANTED_TIMEOUT;
/// Сколько после шквала ждём «тишины»: если конец сеанса не пришёл за это время, шквал поглощён.
const ABSORB_QUIET_WINDOW: Duration = Duration::from_secs(3);
/// Сколько кадров плеер обязан показать после поглощённой атаки, чтобы считать сеанс живым.
const MIN_FRAMES_GAINED_AFTER_ABSORBED_ATTACK: u64 = 30;
/// Шаг опроса журнала посредника.
const GRANT_POLL_STEP: Duration = Duration::from_millis(200);

/// Какие концы сеанса честны для страницы-атаки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndPolicy {
    /// Только сообщение `stream_error` (экстеншен/плеер оборвали поток).
    StreamErrorNotice,
    /// Сначала плеер сам отказал поколению как `unparsable_media`, затем экстеншен закрыл сеанс
    /// сообщением `stream_error`. Обрыв по другой причине здесь не считается: страница доказывает
    /// именно то, что плеер распознал мусор, а не молча стоял до чужого таймаута.
    RejectionThenStreamErrorNotice,
    /// Продукт обязан ПОГЛОТИТЬ атаку: конца сеанса нет, воспроизведение идёт дальше (шквал
    /// сообщений воркера — закрывать там нечего). Любое сообщение о конце — провал. Окно после
    /// проверки закрывает стенд.
    SessionAbsorbsAttack,
}

/// Страница-атака.
#[derive(Debug, Clone, Copy)]
pub(super) struct AttackPage {
    /// Идентификатор страницы стенда.
    pub(super) id: &'static str,
    /// Честные концы.
    pub(super) end_policy: EndPolicy,
    /// Что страница обязана доказать метриками (атака долетела / цена поглощения).
    pub(super) landing: Landing,
    /// Часть строки ошибки плеера, которая здесь ожидаема: только для этой страницы, чтобы та же
    /// строка на чистой странице осталась провалом.
    pub(super) expected_player_error: Option<&'static str>,
}

/// Страницы-атаки. Порядок: от самой простой к самой тяжёлой.
pub(super) const ATTACK_PAGES: &[AttackPage] = &[
    AttackPage {
        id: "attack-giant-segments",
        end_policy: EndPolicy::StreamErrorNotice,
        landing: Landing::BytesCrossed,
        expected_player_error: None,
    },
    AttackPage {
        id: "attack-event-flood",
        end_policy: EndPolicy::StreamErrorNotice,
        landing: Landing::OperationsCrossed,
        expected_player_error: None,
    },
    // Плеер обязан сам распознать мусор: отказ поколения, затем конец сеанса с сообщением.
    // Демуксер, доиграв прочитанное впрок, честно пишет свою ошибку в журнал.
    AttackPage {
        id: "attack-garbage",
        end_policy: EndPolicy::RejectionThenStreamErrorNotice,
        landing: Landing::NotMeasured,
        expected_player_error: Some(GARBAGE_DEMUX_ERROR),
    },
    // Только Chromium (в Firefox страница честно `unsupported`).
    AttackPage {
        id: "attack-worker-flood",
        end_policy: EndPolicy::SessionAbsorbsAttack,
        landing: Landing::FloodAbsorbed(WORKER_FLOOD_BOUNDS),
        expected_player_error: None,
    },
];

/// Идентификаторы страниц-атак (для проверки `--page` в командной строке).
pub fn attack_page_ids() -> Vec<&'static str> {
    ATTACK_PAGES.iter().map(|page| page.id).collect()
}

/// Все страницы-атаки одного браузера (каждая в свежем окружении), с учётом фильтра.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    ATTACK_PAGES
        .iter()
        .filter(|page| filter.admits(page.id))
        .map(|page| {
            let started = Instant::now();
            let verdict =
                stand.with_environment(|environment| drive(stand, environment, browser, *page));
            StepResult::new("attacks", browser, page.id, verdict, started.elapsed())
        })
        .collect()
}

/// Как кончился сеанс атаки.
#[derive(Debug)]
enum ObservedEnd {
    /// Окно показало сообщение о конце сеанса.
    Notice(EndNotice),
    /// Плеер отказал поколению, сообщения о конце нет. Честным концом не бывает ни для одной
    /// страницы: сеанс без сообщения — это зритель перед спиннером.
    GenerationRejected(String),
    /// Конца сеанса нет, воспроизведение продолжилось: продукт поглотил атаку.
    Absorbed { frames_gained: u64 },
}

/// Что стенд увидел после команды атаки.
#[derive(Debug)]
struct AttackAftermath {
    end: Result<ObservedEnd, String>,
    /// Окно плеера закрылось (само после сообщения или стендом после отказа поколения).
    window_closed: Result<(), String>,
    /// Страница подтвердила, что выполнила атаку до конца.
    hook_acknowledged: Result<(), String>,
}

/// Чистая страница после атаки.
#[derive(Debug)]
struct FollowUp {
    outcome: Result<HandOffOutcome, String>,
    frames: Result<FrameEvidence, String>,
    page_report: Result<PageReport, SiteError>,
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page: AttackPage,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        page.id,
        page.id,
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(page.id, browser_kind, &report);
            run.browser.shutdown()?;
            return Ok(verdict);
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!(
                "the page never became ready for the hand-off\n{log}"
            )));
        }
    }
    // 1. Честный старт.
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("hand_off_tab: {other:?}\n{log}")));
        }
    }
    run.tell_page("handed-off");
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let start = start_of_session(&mut run, &control)?;

    // 2-3. Атака и честный конец.
    let aftermath = match &start {
        Ok(()) => provoke_attack(&mut run, &control, page),
        Err(reason) => AttackAftermath::not_reached(reason),
    };
    let attack_report = run.finish_page();

    // 4. Чистая страница в том же браузере.
    let grants_before_follow_up = read_host_evidence(&run.stderr_log)?.session_grants;
    let follow_up = drive_follow_up(&mut run, grants_before_follow_up)?;

    let page_log = run.take_page_log();
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_attack(
        &page,
        &AttackEvidence {
            start,
            aftermath,
            attack_report,
            grants_before_follow_up,
            follow_up,
            host,
            forbidden_player_lines: forbidden,
            page_log: format_browser_log(&page_log),
        },
    ))
}

impl AttackAftermath {
    fn not_reached(reason: &str) -> Self {
        let skipped = format!("not awaited: {reason}");
        Self {
            end: Err(skipped.clone()),
            window_closed: Err(skipped.clone()),
            hook_acknowledged: Err(skipped),
        }
    }
}

/// Команда атаки → ждём конец сеанса → ждём, пока окно уйдёт → ждём подтверждение страницы.
fn provoke_attack(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    page: AttackPage,
) -> AttackAftermath {
    let command = "hook:attack";
    // Строки журнала до команды (кадры, старт) в счёт конца не идут.
    if let Err(error) = run.player_log.skip_history() {
        return AttackAftermath::not_reached(&format!("cannot read the player log: {error}"));
    }
    run.tell_page(command);
    let absorbs = page.end_policy == EndPolicy::SessionAbsorbsAttack;
    // Поглощаемая атака (шквал) сперва доигрывает до подтверждения страницы: «конца нет» можно
    // утверждать только после того, как шквал кончился.
    let early_acknowledgement = absorbs.then(|| acknowledge_attack(run, command));
    let end = if absorbs {
        observe_absorption(run)
    } else {
        observe_end(run, page.end_policy)
    };
    // Окно: после сообщения оно закрывается само; после поглощённой атаки или отказа без сообщения
    // сеанс жив, и закрыть окно должен стенд, иначе следующий сеанс упёрся бы в занятого плеера.
    let window_closed = match &end {
        Ok(ObservedEnd::GenerationRejected(_) | ObservedEnd::Absorbed { .. }) => control
            .quit_window()
            .map_err(|error| format!("cannot close the player window: {error}")),
        _ => control
            .wait_until_gone(WINDOW_GONE_TIMEOUT)
            .map_err(|error| format!("the player window did not close: {error}")),
    };
    let hook_acknowledged =
        early_acknowledgement.unwrap_or_else(|| acknowledge_attack(run, command));
    AttackAftermath {
        end,
        window_closed,
        hook_acknowledged,
    }
}

/// Ждёт подтверждение страницы, что она довела атаку до конца.
fn acknowledge_attack(run: &mut HandoffRun<'_>, command: &str) -> Result<(), String> {
    run.wait_ack(command, ACK_TIMEOUT)
        .map_err(|message| format!("the page did not finish its attack: {message}"))
}

/// Ждёт честный конец сеанса, допустимый политикой страницы.
fn observe_end(run: &mut HandoffRun<'_>, policy: EndPolicy) -> Result<ObservedEnd, String> {
    if policy == EndPolicy::RejectionThenStreamErrorNotice {
        // Строки читаются по порядку: отказ поколения обязан быть раньше сообщения о конце.
        let rejection = wait_for_end_line(run, GENERATION_REJECTED_MARKER)?;
        check_rejection_reason(&rejection)?;
    }
    classify_end_line(&wait_for_end_line(run, END_NOTICE_MARKER)?)
}

/// Ждёт строку журнала плеера, которая описывает конец сеанса.
fn wait_for_end_line(run: &mut HandoffRun<'_>, needle: &str) -> Result<String, String> {
    run.player_log
        .wait_for_any_line(&[needle], END_TIMEOUT)
        .map_err(|error| format!("no end of the session after the attack: {error}"))
}

/// Плеер обязан назвать мусор мусором, а не любой другой отказ.
fn check_rejection_reason(line: &str) -> Result<(), String> {
    if line.contains(UNPARSABLE_MEDIA_REASON) {
        return Ok(());
    }
    Err(format!(
        "the player rejected the stream for another reason, expected {UNPARSABLE_MEDIA_REASON}: {}",
        line.trim()
    ))
}

/// Проверка поглощения: за тихое окно после шквала нет ни одного конца сеанса (в том числе
/// случившегося во время шквала: строки читаются с команды атаки), и кадры плеера продолжают расти.
fn observe_absorption(run: &mut HandoffRun<'_>) -> Result<ObservedEnd, String> {
    let end_needles = [
        END_NOTICE_MARKER,
        GENERATION_REJECTED_MARKER,
        PLAYBACK_FAILED_MARKER,
    ];
    match run
        .player_log
        .wait_for_any_line(&end_needles, ABSORB_QUIET_WINDOW)
    {
        Ok(line) => return classify_end_line(&line),
        Err(ObserveError::Timeout { .. }) => {}
        Err(error) => return Err(format!("cannot read the player log: {error}")),
    }
    run.player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED_AFTER_ABSORBED_ATTACK, FRAMES_TIMEOUT)
        .map(|growth| ObservedEnd::Absorbed {
            frames_gained: growth.frames_gained(),
        })
        .map_err(|error| format!("playback did not continue after the absorbed attack: {error}"))
}

/// Ошибка, которую плеер обязан записать именно на этой странице.
fn is_expected_player_error(page: &AttackPage, line: &str) -> bool {
    page.expected_player_error
        .is_some_and(|expected| line.contains(expected))
}

/// Строка журнала → какой это конец. Строка с маркером конца сеанса без полей — сломанный формат.
fn classify_end_line(line: &str) -> Result<ObservedEnd, String> {
    if line.contains(END_NOTICE_MARKER) {
        return parse_end_notice(line)
            .map(ObservedEnd::Notice)
            .ok_or_else(|| format!("malformed end notice line: {line}"));
    }
    Ok(ObservedEnd::GenerationRejected(line.to_owned()))
}

/// Ждёт, пока посредник выдаст больше пропусков, чем `grants_before`.
fn wait_for_new_grant(
    run: &HandoffRun<'_>,
    grants_before: u32,
) -> Result<Result<(), String>, ScenarioError> {
    let deadline = Instant::now() + SECOND_GRANT_TIMEOUT;
    loop {
        if read_host_evidence(&run.stderr_log)?.session_grants > grants_before {
            return Ok(Ok(()));
        }
        if Instant::now() >= deadline {
            return Ok(Err(format!(
                "the host did not grant a new session within {SECOND_GRANT_TIMEOUT:?} (grants stayed at {grants_before})"
            )));
        }
        std::thread::sleep(GRANT_POLL_STEP);
    }
}

/// Чистая страница в том же браузере: передача, новый пропуск, кадры нового окна.
fn drive_follow_up(
    run: &mut HandoffRun<'_>,
    grants_before: u32,
) -> Result<FollowUp, ScenarioError> {
    run.open_next_page(FOLLOW_UP_PAGE, "attack-follow-up")?;
    let follow_up_run = run.run.clone();
    let unreached = |reason: String| FollowUp {
        outcome: Err(reason.clone()),
        frames: Err(format!("not awaited: {reason}")),
        page_report: Err(SiteError::ReportTimeout {
            run: follow_up_run.clone(),
            waited: Duration::ZERO,
        }),
    };
    if !matches!(run.wait_page_ready(), PageReadiness::Ready) {
        return Ok(unreached(
            "the follow-up page never became ready".to_owned(),
        ));
    }
    // Сводки старого окна в счёт не идут.
    run.player_log.skip_history()?;
    let outcome = run.hand_off().map_err(|error| error.to_string());
    if !matches!(outcome, Ok(HandOffOutcome::Started)) {
        let reason = format!("hand_off_tab on the clean page: {outcome:?}");
        run.tell_page("finish");
        return Ok(unreached(reason));
    }
    run.tell_page("handed-off");
    let frames = match wait_for_new_grant(run, grants_before)? {
        Ok(()) => run
            .player_log
            .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
            .map_err(|error| format!("player did not present frames of the clean page: {error}")),
        Err(message) => Err(format!("{message}; frames were not awaited")),
    };
    let page_report = run.finish_page();
    Ok(FollowUp {
        outcome,
        frames,
        page_report,
    })
}

/// Всё, что собрал прогон: вход чистого приговора.
struct AttackEvidence {
    /// Честный старт состоялся (пропуск, кадры, окно управляемо).
    start: Result<(), String>,
    aftermath: AttackAftermath,
    /// Отчёт страницы-атаки (обязан быть `pass`: атака не должна ломать саму страницу).
    attack_report: Result<PageReport, SiteError>,
    /// Сколько пропусков было до чистой страницы.
    grants_before_follow_up: u32,
    follow_up: FollowUp,
    /// Итоговые отметки посредника.
    host: HostLogEvidence,
    forbidden_player_lines: Vec<String>,
    page_log: String,
}

/// Чистый приговор: честный конец атаки и играющий следующий сеанс.
fn judge_attack(page: &AttackPage, evidence: &AttackEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    if let Err(message) = &evidence.start {
        failures.push(format!("the honest start did not happen: {message}"));
    }
    let aftermath = &evidence.aftermath;
    let mut how_it_ended = String::from("no end");
    match &aftermath.end {
        Ok(ObservedEnd::Notice(found)) => {
            how_it_ended = found.line.trim().to_owned();
            if found.notice != STREAM_ERROR_NOTICE {
                failures.push(format!(
                    "end notice {} for the attack, expected {STREAM_ERROR_NOTICE}: {}",
                    found.notice,
                    found.line.trim()
                ));
            }
            if found.cause != PAGE_STREAM_ERROR_CAUSE {
                failures.push(format!(
                    "end cause {} for the attack, expected {PAGE_STREAM_ERROR_CAUSE}: {}",
                    found.cause,
                    found.line.trim()
                ));
            }
        }
        Ok(ObservedEnd::GenerationRejected(line)) => {
            how_it_ended = line.trim().to_owned();
            failures.push(format!(
                "the player only rejected the stream generation, but an end of the session was expected: {}",
                line.trim()
            ));
        }
        Ok(ObservedEnd::Absorbed { frames_gained }) => {
            how_it_ended = format!("absorbed, no end of the session, +{frames_gained} frames");
            if page.end_policy != EndPolicy::SessionAbsorbsAttack {
                failures.push(
                    "the stand reports an absorbed attack for a page that must end the session"
                        .to_owned(),
                );
            }
        }
        Err(message) => failures.push(message.clone()),
    }
    if page.end_policy == EndPolicy::SessionAbsorbsAttack
        && !matches!(aftermath.end, Ok(ObservedEnd::Absorbed { .. }) | Err(_))
    {
        failures.push(format!(
            "the attack must be absorbed without an end of the session, but it ended: {how_it_ended}"
        ));
    }
    for step in [&aftermath.window_closed, &aftermath.hook_acknowledged] {
        if let Err(message) = step {
            failures.push(message.clone());
        }
    }
    failures.extend(page_failures(&evidence.attack_report, page.id));
    // Если отчёта нет, причина уже названа в `page_failures`.
    if let Ok(report) = &evidence.attack_report {
        failures.extend(landing_failures(page.landing, report));
    }
    failures.extend(follow_up_failures(evidence));
    failures.extend(host_failures(&evidence.host));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .filter(|line| !is_expected_player_error(page, line))
            .map(|line| format!("forbidden player line (panic or error): {line}")),
    );
    let attack_metrics = evidence.attack_report.as_ref().ok().map(|report| {
        report
            .metrics
            .iter()
            .filter(|(name, _)| !name.starts_with("shadow") && !name.contains("frames_in_window"))
            .map(|(name, value)| format!("{name}={value:.0}"))
            .collect::<Vec<_>>()
            .join(" ")
    });
    let absorbed_cost = match (page.landing, &evidence.attack_report) {
        (Landing::FloodAbsorbed(_), Ok(report)) => {
            format!("; absorbed at {}", absorbed_cost_line(report))
        }
        _ => String::new(),
    };
    let summary = format!(
        "end: {how_it_ended}{absorbed_cost}; sessions {} -> {}; attack cost: {}",
        evidence.grants_before_follow_up,
        evidence.host.session_grants,
        attack_metrics.unwrap_or_else(|| "-".to_owned())
    );
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Чистая страница после атаки: передача, новый пропуск, кадры, браузер не декодирует.
fn follow_up_failures(evidence: &AttackEvidence) -> Vec<String> {
    let follow_up = &evidence.follow_up;
    let mut failures = Vec::new();
    match &follow_up.outcome {
        Ok(HandOffOutcome::Started) => {}
        Ok(other) => failures.push(format!(
            "the clean page after the attack: hand_off_tab {other:?}, expected started"
        )),
        Err(message) => failures.push(format!("the clean page after the attack: {message}")),
    }
    if evidence.grants_before_follow_up == 0
        || evidence.host.session_grants <= evidence.grants_before_follow_up
    {
        failures.push(format!(
            "no new session for the clean page (grants {} -> {}): the attack broke the next hand-off",
            evidence.grants_before_follow_up, evidence.host.session_grants
        ));
    }
    match &follow_up.frames {
        Ok(found) if found.position_regressions > 0 => failures.push(format!(
            "clean page: playback position went backwards {} time(s): {}",
            found.position_regressions, found.summary_line
        )),
        Ok(_) => {}
        Err(message) => failures.push(message.clone()),
    }
    failures.extend(page_failures(&follow_up.page_report, FOLLOW_UP_PAGE));
    if let Ok(report) = &follow_up.page_report {
        failures.extend(browser_does_not_decode(report));
    }
    failures
}

#[cfg(test)]
mod tests;
