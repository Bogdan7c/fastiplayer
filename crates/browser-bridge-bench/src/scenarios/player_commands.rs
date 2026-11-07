//! `player-commands`: окно плеера управляет страницей, пока страница под тенью (сессия 09a).
//!
//! Стенд управляет окном браузерного режима так же, как рабочий стол, — по MPRIS на частной
//! шине (`PlayerControl`). Порядок на странице `mse-fmp4`, играющей под тенью:
//!   1. пауза плеера → страница видит `paused`, её часы стоят;
//!   2. play плеера → страница видит игру, часы идут;
//!   3. перемотка плеером → часы страницы у цели, страница докачала новое место, плеер
//!      продолжает показывать кадры (рост между двумя сводками), позиция плеера идёт;
//!   4. закрытие окна плеера (`Quit`) → экстеншен снимает тень: браузер снова декодирует,
//!      страница сама играет с позиции T, на которой закрыли окно.
//!
//! Каждый шаг подтверждает страница (`ack`), и только потом идёт следующий.
//!
//! Тот же порядок проходит файл `<video src>` (`video-src`, сессия 09b), с одним отличием в
//! шаге 3б. У файла под тенью `buffered` — весь файл (решение 187), поэтому «за конец буфера
//! страницы» у него нет; есть «за то, что плеер уже скачал». Плеер запускается с самым малым
//! окном предзагрузки (профиль `SmallFileWindow`), перемотка уходит за скачанные фоном байты, и
//! доказательство — новый `fetch_range` у цели в журнале сервера, рост кадров и часы страницы
//! у цели. Там же видно, что `<video>` страницы под тенью файл не качал, а после закрытия окна
//! снова грузит свой файл.
//!
//! Страницы «живой эфир как Twitch» (решение 194) идут той же командой, их порядок —
//! в [`super::live_unload`].

use std::time::{Duration, Instant};

mod hls_steps;

use self::hls_steps::HlsContext;
use super::capture::file_evidence::{
    FilePage, SeekBeyondFetched, commands_file_evidence, fetch_front, file_page, seek_fetch_check,
};
use super::capture::hls_evidence::hls_page;
use super::capture::{MIN_PRESENTED_FRAMES, judge_early_report};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines, wait_for_session_granted,
};
use super::handoff_run::{ACK_TIMEOUT, ExtensionMode, HandoffRun, PageReadiness};
use super::live_unload::{self, LIVE_UNLOAD_PAGES};
use super::outcome::{StepResult, Verdict};
use super::page_filter::PageFilter;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::environment::PlayerConfigProfile;
use crate::observe::{FrameEvidence, FrameGrowth, HostLogEvidence, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::site::{PageReport, SiteError, StandSite};

/// Страницы, на которых проверяются команды плеера: MSE, файл `<video src>` (09b) и родной HLS VOD
/// (10a2, длинный ролик: запас на паузу, перемотку вперёд и возврат).
const COMMANDS_PAGES: &[&str] = &["mse-fmp4", "video-src", "hls-vod-long"];
/// Сколько ждать, пока окно плеера опубликует управление по MPRIS.
pub(super) const CONTROLLABLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждать первые кадры плеера до команд.
pub(super) const FRAMES_TIMEOUT: Duration = Duration::from_secs(60);
/// На сколько должны вырасти кадры плеера после перемотки между двумя сводками.
const MIN_FRAMES_GAINED_AFTER_SEEK: u64 = 60;
/// Сколько ждать две сводки плеера после перемотки.
const POST_SEEK_FRAMES_TIMEOUT: Duration = Duration::from_secs(45);
/// Размер перемотки, микросекунды. Назад, если плеер уже далеко от начала: ролик стенда
/// 30 с, а после перемотки нужно ещё ~20 с игры, чтобы получить две сводки плеера.
const SEEK_STEP_MICROS: i64 = 8_000_000;
/// Дальше этой позиции перематываем назад, иначе вперёд.
const SEEK_BACKWARD_FROM_MICROS: i64 = 8_000_000;
/// На сколько секунд позиция плеера должна уйти вперёд после перемотки: «играет дальше».
const POSITION_ADVANCE_AFTER_SEEK_MICROS: i64 = 1_000_000;
/// Дальше этого от цели позиция не должна уйти за время проверки: «играет от цели», а не «ушла».
const POSITION_ADVANCE_MAX_MICROS: i64 = 6_000_000;
/// Сколько ждём рост позиции плеера после перемотки.
const POSITION_ADVANCE_TIMEOUT: Duration = Duration::from_secs(10);
/// На сколько секунд цель дальше конца буфера страницы (минимум одна-две длины сегмента).
const BEYOND_BUFFER_MARGIN_S: f64 = 4.0;
/// Сколько секунд ролика должно остаться после цели, чтобы было что играть.
const END_OF_MEDIA_MARGIN_S: f64 = 3.0;
/// Сколько ждать первую сводку плеера после паузы (интервал сводок 10 с).
pub(super) const BASELINE_SUMMARY_TIMEOUT: Duration = Duration::from_secs(20);
/// Дальше этой позиции окно перед закрытием отматывается назад (ролик стенда 30 с).
const RETURN_MAX_POSITION_MICROS: i64 = 20_000_000;
/// На сколько отматывается окно перед закрытием.
const RETURN_REWIND_MICROS: i64 = 12_000_000;
/// Микросекунд в секунде (страница получает секунды).
const MICROS_PER_SECOND: f64 = 1_000_000.0;

/// Все страницы команды (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    COMMANDS_PAGES
        .iter()
        .chain(LIVE_UNLOAD_PAGES)
        .copied()
        .collect()
}

pub(super) fn run(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let mut results: Vec<StepResult> = COMMANDS_PAGES
        .iter()
        .filter(|page| filter.admits(page))
        .map(|page| {
            let started = Instant::now();
            let body = |environment: &BenchEnvironment| drive(stand, environment, browser, page);
            // Файл и HLS — с самым малым окном предзагрузки: плеер не может взять всё впрок, и
            // перемотка за скачанное обязана дойти до сети (`fetch_range` / новый сегмент у цели).
            let verdict = if file_page(page).is_some() || hls_page(page).is_some() {
                stand.with_environment_for_player_config(PlayerConfigProfile::SmallFileWindow, body)
            } else {
                stand.with_environment(body)
            };
            StepResult::new(
                "player-commands",
                browser,
                *page,
                verdict,
                started.elapsed(),
            )
        })
        .collect();
    results.extend(live_unload::run(stand, browser, filter));
    results
}

/// Результат одного шага сценария.
struct StepEvidence {
    name: &'static str,
    result: Result<(), String>,
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page_id: &'static str,
) -> Result<Verdict, ScenarioError> {
    let file = file_page(page_id);
    let hls = hls_page(page_id);
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        page_id,
        "commands",
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            // Тот же судья, что у capture: `unsupported` честен там, где его разрешает таблица страниц
            // (нативный HLS в Firefox на Linux), остальное — провал.
            return Ok(judge_early_report(page_id, browser_kind, &report));
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("the page never became ready\n{log}")));
        }
    }
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("hand_off_tab: {other:?}\n{log}")));
        }
    }
    let handed_off_at = Instant::now();
    run.tell_page("handed-off");

    let mut steps: Vec<StepEvidence> = Vec::new();
    let mut seek_growth: Result<FrameGrowth, String> = Err("the seek step did not run".to_owned());
    // Когда закрыли окно плеера (`None` — до возврата не дошли).
    let mut returned_at: Option<Instant> = None;
    let mut file_seek: Option<String> = None;
    let hls_context = hls.map(|page| HlsContext::new(page, &stand.site, handed_off_at));
    let granted = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    steps.push(StepEvidence {
        name: "session-granted",
        result: granted,
    });
    if steps.iter().all(|step| step.result.is_ok()) {
        let control = PlayerControl::on_bus(environment.bus_address()?);
        let marks = StepMarks {
            seek_growth: &mut seek_growth,
            returned_at: &mut returned_at,
            file_seek: &mut file_seek,
        };
        let file_context = file.map(|page| FileContext {
            page,
            site: &stand.site,
            handed_off_at,
        });
        run_commands(
            &mut run,
            &control,
            &mut steps,
            marks,
            ScenarioSources {
                file: file_context,
                hls: hls_context,
            },
        );
    }

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let file = file.map(|file_page| {
        let (failures, summary) = commands_file_evidence(
            file_page,
            &stand.site.media_requests(),
            handed_off_at,
            returned_at,
        );
        let seek = file_seek
            .as_deref()
            .unwrap_or("the seek beyond the fetched bytes did not run");
        (failures, format!("{summary}; {seek}"))
    });
    let file = file.or_else(|| {
        hls_context
            .map(|context| hls_steps::server_evidence(context, returned_at, file_seek.as_deref()))
    });
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_commands(&CommandsEvidence {
        page_id,
        steps,
        seek_growth,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
        file,
    }))
}

/// Файл `<video src>`: где смотреть журнал сервера и с какого мига файл качает фон.
#[derive(Clone, Copy)]
struct FileContext<'site> {
    page: FilePage,
    site: &'site StandSite,
    handed_off_at: Instant,
}

/// Откуда сценарий берёт журнал сервера, если у страницы он нужен (файл или HLS; у MSE — ничего).
#[derive(Clone, Copy)]
struct ScenarioSources<'site> {
    file: Option<FileContext<'site>>,
    hls: Option<HlsContext<'site>>,
}

/// Отметки шагов, которые нужны вердикту после прогона.
struct StepMarks<'marks> {
    seek_growth: &'marks mut Result<FrameGrowth, String>,
    returned_at: &'marks mut Option<Instant>,
    /// Улики перемотки файла за скачанное (байты до и запрос у цели).
    file_seek: &'marks mut Option<String>,
}

/// Шаги управления; останавливается на первом проваленном (следующие не имели бы смысла).
fn run_commands(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    steps: &mut Vec<StepEvidence>,
    marks: StepMarks<'_>,
    sources: ScenarioSources<'_>,
) {
    let ScenarioSources { file, hls } = sources;
    let mut record = |name: &'static str, result: Result<(), String>| -> bool {
        let ok = result.is_ok();
        steps.push(StepEvidence { name, result });
        ok
    };
    let frames = run
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
        .map(|_| ())
        .map_err(|error| format!("player did not present frames: {error}"));
    if !record("player-presents-frames", frames) {
        return;
    }
    let controllable = control
        .wait_until_controllable(CONTROLLABLE_TIMEOUT)
        .map_err(|error| error.to_string());
    if !record("window-mpris-controllable", controllable) {
        return;
    }

    // 1. Пауза плеера.
    let paused = control
        .pause()
        .map_err(|error| format!("Pause: {error}"))
        .and_then(|()| tell_and_wait(run, "expect-paused"));
    if !record("pause-reaches-page", paused) {
        return;
    }
    // 2. Play плеера.
    let played = control
        .play()
        .map_err(|error| format!("Play: {error}"))
        .and_then(|()| tell_and_wait(run, "expect-playing"));
    if !record("play-reaches-page", played) {
        return;
    }
    // 3. Файл: перемотка ЗА край окна предзагрузки плеера — первой, пока плеер у начала ролика:
    //    вперёд от курсора плеер держит ~12 с (окно 1 МиБ ≈ 4 с плюс очередь демуксера), и в
    //    ролике после других перемоток для неё могло не остаться места. Фон обязан
    //    запросить диапазон у цели по `fetch_range` плеера.
    if let Some(file) = file {
        let beyond_fetched = seek_beyond_fetched(run, control, file, marks.seek_growth)
            .map(|facts| *marks.file_seek = Some(facts));
        if !record("seek-beyond-fetched-reaches-network", beyond_fetched) {
            return;
        }
    }
    // 3. HLS: перемотка вперёд ЗА сегменты, которых фон ещё не скачал, — новый сегмент у цели.
    if let Some(hls) = hls {
        let beyond_fetched =
            hls_steps::seek_beyond_fetched_segments(run, control, hls, marks.seek_growth)
                .map(|facts| *marks.file_seek = Some(facts));
        if !record("seek-beyond-fetched-reaches-network", beyond_fetched) {
            return;
        }
    }
    // 3. Перемотка плеером В буфер страницы (данные уже скачаны: запросов может не быть).
    let into_buffer = seek_into_buffer(run, control).map(|_| ());
    if !record("seek-reaches-page", into_buffer) {
        return;
    }
    // 4. MSE: перемотка ЗА конец буфера страницы — страница обязана докачать сегмент у цели.
    if file.is_none() && hls.is_none() {
        let beyond_buffer = seek_beyond_buffer(run, control, marks.seek_growth);
        if !record("seek-beyond-buffer-reaches-page", beyond_buffer) {
            return;
        }
    }
    // 5. Закрытие окна плеера → возврат к странице.
    *marks.returned_at = Some(Instant::now());
    let returned = close_window_and_check_return(run, control);
    record("return-to-page", returned);
}

/// Команда странице и ожидание её подтверждения.
pub(super) fn tell_and_wait(run: &HandoffRun<'_>, command: &str) -> Result<(), String> {
    run.tell_page(command);
    run.wait_ack(command, ACK_TIMEOUT)
}

/// Куда перематывать из позиции `position_micros` (чистая функция: проверяется тестами).
fn seek_offset_for(position_micros: i64) -> i64 {
    if position_micros > SEEK_BACKWARD_FROM_MICROS {
        -SEEK_STEP_MICROS
    } else {
        SEEK_STEP_MICROS
    }
}

/// Ставит плеер на паузу и берёт сводку-точку отсчёта: на паузе кадры не растут, поэтому
/// всё, что сверх точки, показано уже ПОСЛЕ перемотки и `play` (две сводки подряд при игре
/// не годятся: ролик стенда всего 30 с, а на перемотку уйдёт большая часть).
fn pause_and_take_frame_baseline(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
) -> Result<FrameEvidence, String> {
    control.pause().map_err(|error| format!("Pause: {error}"))?;
    tell_and_wait(run, "expect-paused")?;
    run.player_log
        .skip_history()
        .map_err(|error| format!("player log: {error}"))?;
    // Первая сводка ПОСЛЕ паузы вобрала все кадры, показанные до неё.
    run.player_log
        .wait_for_presented_frames(0, BASELINE_SUMMARY_TIMEOUT)
        .map_err(|error| format!("no player summary after the pause: {error}"))
}

/// Ждёт, пока плеер покажет ещё `MIN_FRAMES_GAINED_AFTER_SEEK` кадров сверх точки отсчёта.
fn wait_frames_beyond(
    run: &mut HandoffRun<'_>,
    baseline: FrameEvidence,
) -> Result<FrameGrowth, String> {
    let after = run
        .player_log
        .wait_for_presented_frames(
            baseline.presented_frames + MIN_FRAMES_GAINED_AFTER_SEEK,
            POST_SEEK_FRAMES_TIMEOUT,
        )
        .map_err(|error| format!("player frames did not grow after the seek: {error}"))?;
    Ok(FrameGrowth {
        before: baseline,
        after,
    })
}

/// Перемотка в уже скачанное место: часы страницы у цели, плеер показывает кадры дальше.
fn seek_into_buffer(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
) -> Result<FrameGrowth, String> {
    let baseline = pause_and_take_frame_baseline(run, control)?;
    let before = control
        .position_micros()
        .map_err(|error| format!("Position: {error}"))?;
    let offset = seek_offset_for(before);
    control
        .seek_by(offset)
        .map_err(|error| format!("Seek: {error}"))?;
    control.play().map_err(|error| format!("Play: {error}"))?;
    let target_s = (before + offset) as f64 / MICROS_PER_SECOND;
    // Позицию плеера проверяем сразу, до разговора со страницей: её ожидание может занять
    // секунды, и позиция успела бы уйти от цели.
    wait_position_advance(control, before + offset)?;
    tell_and_wait(run, &format!("expect-seek:{target_s:.2}"))?;
    wait_frames_beyond(run, baseline)
}

/// Перемотка вперёд ЗА конец буфера страницы. Цель выбирается по тому, что страница сама
/// сообщила о своём буфере; строго требуется, чтобы страница запросила сегмент у цели.
fn seek_beyond_buffer(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    seek_growth: &mut Result<FrameGrowth, String>,
) -> Result<(), String> {
    let baseline = pause_and_take_frame_baseline(run, control)?;
    tell_and_wait(run, "report-buffer")?;
    let report = run.wait_page_log_line("buffer-report", ACK_TIMEOUT)?;
    let (buffered_end_s, duration_s) = parse_buffer_report(&report)
        .ok_or_else(|| format!("cannot read the page buffer report: {report:?}"))?;
    let target_s = beyond_buffer_target(buffered_end_s, duration_s).ok_or_else(|| {
        format!(
            "no room to seek beyond the buffer (buffered to {buffered_end_s} s of {duration_s} s)"
        )
    })?;
    let before = control
        .position_micros()
        .map_err(|error| format!("Position: {error}"))?;
    let target_micros = (target_s * MICROS_PER_SECOND) as i64;
    control
        .seek_by(target_micros - before)
        .map_err(|error| format!("Seek: {error}"))?;
    control.play().map_err(|error| format!("Play: {error}"))?;
    let advanced = wait_position_advance(control, target_micros);
    let page_agrees = tell_and_wait(run, &format!("expect-seek-beyond:{target_s:.2}"));
    *seek_growth = wait_frames_beyond(run, baseline);
    advanced?;
    page_agrees?;
    seek_growth.clone().map(|_| ())
}

/// Перемотка файла ЗА байты, которые фон уже получил. Цель — от того, докуда скачано (переведено
/// во время по размеру и длительности файла), а не от `buffered` страницы (у файла под тенью это
/// весь файл, решение 187). Доказательства: новый запрос фона у цели (журнал сервера), часы
/// страницы у цели, кадры плеера растут после перемотки. Возвращает улики числами.
fn seek_beyond_fetched(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    file: FileContext<'_>,
    seek_growth: &mut Result<FrameGrowth, String>,
) -> Result<String, String> {
    // На паузе окно предзагрузки доходит до предела и стоит: «скачано» не уплывает.
    let baseline = pause_and_take_frame_baseline(run, control)?;
    tell_and_wait(run, "report-buffer")?;
    let report = run.wait_page_log_line("buffer-report", ACK_TIMEOUT)?;
    let (_, duration_s) = parse_buffer_report(&report)
        .ok_or_else(|| format!("cannot read the page buffer report: {report:?}"))?;
    let now = Instant::now();
    // Край окна предзагрузки плеера: дальше него байтов у плеера нет.
    let (fetched_end, total) = fetch_front(
        file.page,
        &file.site.media_requests(),
        file.handed_off_at,
        now,
    )
    .ok_or("the background has not fetched any bytes of the file yet")?;
    let fetched_s = fetched_end as f64 / total as f64 * duration_s;
    let target_s = beyond_buffer_target(fetched_s, duration_s).ok_or_else(|| {
        format!("no room to seek beyond the prefetch window (front at ~{fetched_s:.1} s of {duration_s} s)")
    })?;
    let before = control
        .position_micros()
        .map_err(|error| format!("Position: {error}"))?;
    let target_micros = (target_s * MICROS_PER_SECOND) as i64;
    let seek_at = Instant::now();
    control
        .seek_by(target_micros - before)
        .map_err(|error| format!("Seek: {error}"))?;
    control.play().map_err(|error| format!("Play: {error}"))?;
    let advanced = wait_position_advance(control, target_micros);
    let page_agrees = tell_and_wait(run, &format!("expect-seek:{target_s:.2}"));
    *seek_growth = wait_frames_beyond(run, baseline);
    let fetched = seek_fetch_check(
        file.page,
        &file.site.media_requests(),
        SeekBeyondFetched {
            seek_at,
            fetched_end,
            total,
            target_s,
            duration_s,
        },
    );
    advanced?;
    page_agrees?;
    let facts = fetched?;
    seek_growth.clone().map(|_| facts)
}

/// Разбирает строку страницы `... buffer-report end=<с> duration=<с>`.
fn parse_buffer_report(line: &str) -> Option<(f64, f64)> {
    let field = |name: &str| -> Option<f64> {
        let rest = line.split(&format!("{name}=")).nth(1)?;
        rest.split_whitespace().next()?.parse().ok()
    };
    Some((field("end")?, field("duration")?))
}

/// Цель перемотки за буфером: на `BEYOND_BUFFER_MARGIN_S` дальше его конца, но не ближе
/// `END_OF_MEDIA_MARGIN_S` к концу ролика (иначе после перемотки нечего играть).
fn beyond_buffer_target(buffered_end_s: f64, duration_s: f64) -> Option<f64> {
    let target = buffered_end_s + BEYOND_BUFFER_MARGIN_S;
    (target <= duration_s - END_OF_MEDIA_MARGIN_S).then_some(target)
}

/// Ждёт, пока позиция плеера окажется в окне «цель .. цель + запас» и уйдёт вперёд от
/// `from_micros` минимум на заданную величину. Верхняя граница важна: позиция в конце
/// ролика (после перемотки, которую плеер проигнорировал) не должна сойти за успех.
fn wait_position_advance(control: &PlayerControl, from_micros: i64) -> Result<(), String> {
    let deadline = Instant::now() + POSITION_ADVANCE_TIMEOUT;
    let mut last = from_micros;
    loop {
        last = control.position_micros().unwrap_or(last);
        let advanced = last >= from_micros + POSITION_ADVANCE_AFTER_SEEK_MICROS;
        let near = last <= from_micros + POSITION_ADVANCE_MAX_MICROS;
        if advanced && near {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the player position is {last} µs, expected between {} and {} µs after the seek to {from_micros} µs",
                from_micros + POSITION_ADVANCE_AFTER_SEEK_MICROS,
                from_micros + POSITION_ADVANCE_MAX_MICROS
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn close_window_and_check_return(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
) -> Result<(), String> {
    let mut position = control
        .position_micros()
        .map_err(|error| format!("Position: {error}"))?;
    // Ролик стенда короткий, и к этому шагу он мог доиграть до конца: закрывать окно в
    // конце бессмысленно (страница вернётся «досмотренной»). Возвращаемся в середину.
    if position > RETURN_MAX_POSITION_MICROS {
        let target = position - RETURN_REWIND_MICROS;
        control
            .seek_by(-RETURN_REWIND_MICROS)
            .map_err(|error| format!("Seek before the return: {error}"))?;
        wait_position_advance(control, target)?;
        position = control
            .position_micros()
            .map_err(|error| format!("Position: {error}"))?;
    }
    control
        .quit_window()
        .map_err(|error| format!("Quit: {error}"))?;
    let position_s = position as f64 / MICROS_PER_SECOND;
    tell_and_wait(run, &format!("expect-returned:{position_s:.2}"))
}

/// Всё, что собрал сценарий, для чистого вердикта.
struct CommandsEvidence {
    page_id: &'static str,
    steps: Vec<StepEvidence>,
    seek_growth: Result<FrameGrowth, String>,
    host: HostLogEvidence,
    page_report: Result<PageReport, SiteError>,
    forbidden_player_lines: Vec<String>,
    page_log: String,
    /// `<video src>`: провалы и сводка запросов файла со стороны сервера (`None` — не файл).
    file: Option<(Vec<String>, String)>,
}

/// Чистое решение: все шаги прошли, страница согласна с плеером, плеер играл после перемотки.
fn judge_commands(evidence: &CommandsEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    for step in &evidence.steps {
        if let Err(message) = &step.result {
            failures.push(format!("step '{}': {message}", step.name));
        }
    }
    // Рост кадров после перемотки нужен, только если сам шаг перемотки дошёл до конца.
    let seek_step_ran = evidence.steps.iter().any(|step| {
        step.name == "seek-beyond-buffer-reaches-page"
            || step.name == "seek-beyond-fetched-reaches-network"
    });
    if seek_step_ran && let Err(message) = &evidence.seek_growth {
        failures.push(message.clone());
    }
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, evidence.page_id));
    if let Some((file_failures, _)) = &evidence.file {
        failures.extend(file_failures.iter().cloned());
    }
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let done: Vec<&str> = evidence
        .steps
        .iter()
        .filter(|step| step.result.is_ok())
        .map(|step| step.name)
        .collect();
    let gained = evidence
        .seek_growth
        .as_ref()
        .ok()
        .map(FrameGrowth::frames_gained);
    let summary = format!(
        "steps done: {}; frames gained after seek: {gained:?}",
        done.join(", ")
    );
    let summary = match &evidence.file {
        Some((_, file_summary)) => format!("{summary}; {file_summary}"),
        None => summary,
    };
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
    use crate::observe::FrameEvidence;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};

    fn step(name: &'static str, result: Result<(), String>) -> StepEvidence {
        StepEvidence { name, result }
    }

    fn all_steps_ok() -> Vec<StepEvidence> {
        [
            "session-granted",
            "player-presents-frames",
            "window-mpris-controllable",
            "pause-reaches-page",
            "play-reaches-page",
            "seek-reaches-page",
            "seek-beyond-buffer-reaches-page",
            "return-to-page",
        ]
        .into_iter()
        .map(|name| step(name, Ok(())))
        .collect()
    }

    fn growth() -> FrameGrowth {
        let evidence = |frames| FrameEvidence {
            presented_frames: frames,
            position_regressions: 0,
            summary_line: String::new(),
        };
        FrameGrowth {
            before: evidence(100),
            after: evidence(360),
        }
    }

    fn passing_report() -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "mse-fmp4".into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }
    }

    fn healthy() -> CommandsEvidence {
        CommandsEvidence {
            page_id: "mse-fmp4",
            steps: all_steps_ok(),
            seek_growth: Ok(growth()),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            page_report: Ok(passing_report()),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".into(),
            file: None,
        }
    }

    #[test]
    fn seek_goes_back_when_the_player_is_far_from_the_start_and_forward_otherwise() {
        assert_eq!(seek_offset_for(13_000_000), -SEEK_STEP_MICROS);
        assert_eq!(seek_offset_for(3_000_000), SEEK_STEP_MICROS);
        // Граница: ровно на пороге идём вперёд (назад было бы почти в начало ролика).
        assert_eq!(seek_offset_for(SEEK_BACKWARD_FROM_MICROS), SEEK_STEP_MICROS);
    }

    #[test]
    fn buffer_report_is_parsed_and_the_target_lies_beyond_the_buffer_with_room_left() {
        assert_eq!(
            parse_buffer_report("mse-fmp4: buffer-report end=21.00 duration=30.00"),
            Some((21.0, 30.0))
        );
        assert_eq!(
            parse_buffer_report("buffer-report end=abc duration=30"),
            None
        );
        assert_eq!(parse_buffer_report("nothing"), None);
        assert_eq!(beyond_buffer_target(21.0, 30.0), Some(25.0));
        // Буфер дошёл почти до конца ролика: перематывать вперёд некуда.
        assert_eq!(beyond_buffer_target(27.0, 30.0), None);
        // Граница: цель ровно на запасе от конца допустима.
        assert_eq!(beyond_buffer_target(23.0, 30.0), Some(27.0));
    }

    #[test]
    fn healthy_run_passes_and_lists_the_steps() {
        let verdict = judge_commands(&healthy());
        assert_eq!(verdict.outcome, Outcome::Pass);
        assert!(verdict.detail.contains("return-to-page"));
        assert!(
            verdict
                .detail
                .contains("frames gained after seek: Some(260)")
        );
    }

    #[test]
    fn every_failed_step_is_named() {
        for failing in [
            "pause-reaches-page",
            "play-reaches-page",
            "seek-reaches-page",
            "seek-beyond-buffer-reaches-page",
            "return-to-page",
        ] {
            let mut evidence = healthy();
            for entry in &mut evidence.steps {
                if entry.name == failing {
                    entry.result = Err("page did not agree".into());
                }
            }
            let verdict = judge_commands(&evidence);
            assert_eq!(verdict.outcome, Outcome::Fail, "{failing}");
            assert!(
                verdict.detail.contains(&format!("step '{failing}'")),
                "{failing}"
            );
        }
    }

    #[test]
    fn no_frame_growth_after_the_seek_fails() {
        let mut evidence = healthy();
        evidence.seek_growth = Err("player frames did not grow after the seek".into());
        let verdict = judge_commands(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("did not grow after the seek"));
    }

    #[test]
    fn a_page_that_disagrees_with_the_player_fails_the_run() {
        let mut evidence = healthy();
        let mut report = passing_report();
        report.status = PageStatus::Fail;
        evidence.page_report = Ok(report);
        assert_eq!(judge_commands(&evidence).outcome, Outcome::Fail);
    }

    #[test]
    fn an_early_stop_reports_the_failed_step_not_a_missing_seek_growth() {
        let mut evidence = healthy();
        evidence.steps = vec![step("session-granted", Err("host did not grant".into()))];
        evidence.seek_growth = Err("the seek step did not run".into());
        let verdict = judge_commands(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("step 'session-granted'"));
        assert!(!verdict.detail.contains("the seek step did not run"));
    }
}
