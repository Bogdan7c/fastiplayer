//! `capture` и `smoke`: страница играет, робот «отдаёт вкладку», и плеер показывает кадры,
//! а браузер при этом НЕ декодирует (сессия 09a).
//!
//! Цепочка проверяется по звеньям, чтобы при провале было видно, где она порвалась:
//! страница сыграла и готова → фон принял команду и вернул `started` → посредник выдал
//! пропуск → плеер показал кадры (`presented_frames` в журнале плеера) → если у страницы
//! есть действие под тенью (смена качества, смена MediaSource), плеер показывает кадры и
//! после него → страница под тенью жива, её часы идут, а НАСТОЯЩИЙ счётчик кадров браузера
//! (мимо тени) почти не растёт. Улики — журналы посредника и плеера и отчёт страницы; журнала
//! экстеншена в продукте нет.

use std::time::{Duration, Instant};

mod ad_hop;
pub(super) mod decoys;
pub(super) mod file_evidence;
pub(super) mod hls_evidence;
mod judge;
pub(super) mod local_address;
mod local_address_hls;
mod referer_evidence;
pub(super) mod refusals;

#[cfg(test)]
pub(super) use ad_hop::AD_HOP_PAGES;
pub(super) use ad_hop::AD_HOST;
pub(super) use decoys::AD_FILE_PATH;
use judge::HookEvidence;
pub(super) use judge::browser_does_not_decode;
/// Тот же потолок «браузер под тенью почти не декодирует» судит и вернувшуюся вкладку (`tab-return`, 11b2).
pub(super) use judge::max_real_frames;
pub(super) use judge::{CaptureEvidence, judge_capture};

use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, page_failures,
    wait_for_session_granted,
};
use super::handoff_run::{
    ACK_TIMEOUT, ExtensionMode, HANDOFF_MODE_QUERY, HandoffRun, PageReadiness,
};
use super::outcome::{StepResult, Verdict};
use super::page_catalog::PAGE_SCENARIOS;
use super::page_filter::PageFilter;
use super::run_tweaks::RunTweaks;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameGrowth, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::site::{PageReport, PageStatus, SiteError};

/// Сколько кадров должен показать плеер: 2 с видео при 25 к/с — «играет», а не «открылся».
pub(super) const MIN_PRESENTED_FRAMES: u64 = 50;
/// Сводки плеера приходят раз в 10 с, плюс запуск плеера и сеанса.
pub(super) const FRAMES_TIMEOUT: Duration = Duration::from_secs(60);
/// На сколько должны вырасти кадры плеера между двумя сводками ПОСЛЕ действия страницы
/// (интервал сводок 10 с, 25 к/с: целый интервал даёт ~250; берём меньше четверти).
const MIN_FRAMES_GAINED_AFTER_HOOK: u64 = 60;
/// Сколько ждать сводки после действия страницы: сводка-точка отсчёта и ещё одна.
const HOOK_FRAMES_TIMEOUT: Duration = Duration::from_secs(45);

/// Сколько дать плееру устояться после открытия, прежде чем мерить паузу.
const PAUSED_START_SETTLE: Duration = Duration::from_secs(2);
/// Сколько плеер обязан простоять на паузе: дольше сторожа тишины сеанса (10 с).
const PAUSED_HOLD: Duration = Duration::from_secs(12);
/// Сколько позиция может уйти за удержание: запас на округление, но не на игру.
const PAUSED_DRIFT_LIMIT_US: i64 = 100_000;

/// Когда робот посылает действие страницы относительно ожидания кадров плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HookTiming {
    /// Обычно: сначала кадры плеера, потом действие и рост кадров после него.
    AfterFrames,
    /// Сразу после передачи, до ожидания кадров: страница стоит на паузе (главное видео
    /// `banner-with-main`), и кадров не будет, пока зритель не нажмёт play (действие страницы).
    /// Робот сначала проверяет, что плеер действительно стоит.
    BeforeFrames,
}

/// Страница, которую проверяет `capture`, и её действие под тенью (если есть).
#[derive(Debug, Clone, Copy)]
pub(super) struct CapturePage {
    /// Идентификатор страницы стенда.
    pub(super) id: &'static str,
    /// Имя действия страницы по команде робота (`hook:<имя>`), см. `site/pages`.
    pub(super) hook: Option<&'static str>,
    /// Когда посылать действие.
    pub(super) hook_timing: HookTiming,
}

/// Страница без действия под тенью.
pub(super) const fn plain_page(id: &'static str) -> CapturePage {
    CapturePage {
        id,
        hook: None,
        hook_timing: HookTiming::AfterFrames,
    }
}

/// Страница с действием под тенью: кадры, потом действие и рост кадров после него.
const fn page_with_hook(id: &'static str, hook: &'static str) -> CapturePage {
    CapturePage {
        id,
        hook: Some(hook),
        hook_timing: HookTiming::AfterFrames,
    }
}

/// Страница, которая стоит на паузе до действия: действие идёт ДО ожидания кадров.
const fn page_resumed_by_hook(id: &'static str, hook: &'static str) -> CapturePage {
    CapturePage {
        id,
        hook: Some(hook),
        hook_timing: HookTiming::BeforeFrames,
    }
}

/// Страница дымка: самая простая MSE-страница, без действий под тенью.
pub(super) const SMOKE_PAGE: CapturePage = plain_page("mse-fmp4");
/// ABR: смена качества в тот же SourceBuffer под тенью, играет дальше.
const ABR_PAGE: CapturePage = page_with_hook("mse-abr", "switch-quality");
/// Реклама → фильм: новый MediaSource на том же элементе уже под тенью.
const GENERATION_SWAP_PAGE: CapturePage = page_with_hook("mse-generation-swap", "swap-generation");

/// MSE-страницы, которые должны дойти до кадра в рендере плеера (список задания 09a).
pub(super) const CAPTURE_PAGES: &[CapturePage] = &[
    SMOKE_PAGE,
    plain_page("mse-webm"),
    plain_page("mse-av1"),
    plain_page("mse-muxed"),
    // ABR: смена качества в тот же SourceBuffer под тенью, играет дальше.
    page_with_hook("mse-abr", "switch-quality"),
    plain_page("mse-stall-reload"),
    // Реклама → фильм: новый MediaSource на том же элементе уже под тенью.
    page_with_hook("mse-generation-swap", "swap-generation"),
    // Д-1: тот же обмен, но старый источник снят иначе (так делает YouTube перед рекламой).
    page_with_hook("mse-generation-swap-srcobject-null", "swap-generation"),
    page_with_hook("mse-generation-swap-reload", "swap-generation"),
    // Д-4: данные нового поколения приходят через 5 с; пока их нет, часы стоят у начала.
    page_with_hook("mse-generation-swap-delayed", "swap-generation"),
    // Д-9: страница кэшировала геттер currentTime и play ДО передачи и зовёт их под тенью.
    page_with_hook("cached-descriptors", "call-cached-api"),
    plain_page("shadow-dom"),
    plain_page("iframe-cross-origin"),
    plain_page("cdn-guard"),
    // 09b: MSE в модульном Dedicated Worker (Chromium; Firefox честно `unsupported`), под тенью —
    // новый MediaSource в воркере (`srcObject = null`, новый handle) → новое поколение в плеере.
    page_with_hook("mse-worker", "swap-generation"),
    // 09b: то же в классическом воркере, как у Twitch.
    plain_page("mse-worker-classic"),
    // 09b: `<video src>` — фон качает файл по `fetch_range`, источник элемента снят.
    plain_page("video-src"),
    // 10a2: родной HLS VOD — плейлисты, сегменты и ключ качает фон по дескрипторам, элемент молчит
    // (судья `hls_evidence`): MPEG-TS, fMP4 с двумя качествами, MPEG-TS под AES-128.
    plain_page("m3u8-in-src"),
    plain_page("hls-fmp4-abr"),
    plain_page("hls-aes128"),
    // 09b: тот же файл за входом на CDN — фон несёт куку сайта.
    plain_page("video-src-cdn"),
    // 09c, решение 202: файл на `/cdn/media/` требует ещё и Referer страницы; фон ставит его
    // правилом только на свой запрос, запросы вкладки не меняются (проба утечки — действие).
    page_with_hook("video-src-referer", "own-fetch-without-referer"),
    // 09c: опрос ключевых систем DRM защитой не считается — видео передаётся и доходит до кадра.
    plain_page("drm-probe"),
    // 09c: серия и одностраничник — НОВЫЕ поколения под тенью доходят до кадра в плеере.
    page_with_hook("series", "next-episode"),
    page_with_hook("spa-navigation", "navigate-spa"),
    // 09c: главное видео на паузе, рядом играют баннер и превью. Главное уходит в плеер (зритель
    // жмёт play — действие), баннер и превью остаются у браузера (по журналу сервера и счётчикам).
    page_resumed_by_hook("banner-with-main", "resume-main"),
    // 15c: ленивый iframe ниже экрана так и не грузится (живой klonua). Опрос фреймов не ждёт его
    // загрузки: передача проходит и доходит до кадра.
    plain_page("frame-never-loads"),
];

/// Все MSE-страницы одного браузера, каждая в свежем окружении (свой плеер и журналы).
/// Первым идёт контроль линейки: браузер без экстеншена играет сам, и счётчик это видит.
/// С фильтром `--page` идут только выбранные страницы (и их контроли, если выбраны).
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let mut results: Vec<StepResult> = CONTROL_PAGES
        .iter()
        .filter(|page| filter.admits(page.id))
        .map(|page| run_decoder_control(stand, browser, *page))
        .collect();
    results.extend(
        CAPTURE_PAGES
            .iter()
            .filter(|page| filter.admits(page.id))
            .map(|page| run_page(stand, browser, "capture", *page)),
    );
    // 09b/09c: видео, которое экстеншен честно отказывается передавать (остаётся у браузера).
    results.extend(
        refusals::REFUSAL_PAGES
            .iter()
            .filter(|page| filter.admits(page.id))
            .map(|page| refusals::run_refusal(stand, browser, *page)),
    );
    // 09c2: реклама → фильм в одном окне (отдельный элемент, фрейм, тот же элемент, цепочка,
    // пропуск, баннер). Варианты одной страницы идут вместе: `--page ad-preroll` запускает оба.
    results.extend(
        ad_hop::AD_HOP_PAGES
            .iter()
            .filter(|page| filter.admits(page.page_id))
            .map(|page| ad_hop::run(stand, browser, *page)),
    );
    results
}

/// Защита локального адреса (решение 199): случаи файла и один случай HLS (сегменты на локальном
/// адресе) в свежих окружениях одного браузера.
pub(super) fn run_local_address(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    let mut results = local_address::run_all(stand, browser);
    results.push(local_address_hls::run(stand, browser));
    results
}

/// Идентификаторы страниц `capture` (контрольные страницы и честные отказы входят в тот же список).
pub(super) fn capture_page_ids() -> Vec<&'static str> {
    CAPTURE_PAGES
        .iter()
        .map(|page| page.id)
        .chain(refusals::REFUSAL_PAGES.iter().map(|page| page.id))
        .chain(ad_hop::page_ids())
        .collect()
}

/// Сколько секунд контрольный прогон даёт браузеру играть самому, пока идёт «окно».
const CONTROL_WINDOW: Duration = Duration::from_secs(8);
/// Не меньше стольких настоящих кадров в секунду должен насчитать счётчик у играющего
/// браузера (ролики стенда — 25 к/с; запас на фоновые вкладки и нагрузку машины).
const MIN_CONTROL_REAL_FRAMES_PER_SECOND: f64 = 12.0;

/// Страницы контроля: простая и обе с действием под тенью (контроль заодно доказывает, что
/// действия страницы — смена качества, смена MediaSource — сами по себе работают).
const CONTROL_PAGES: &[CapturePage] = &[SMOKE_PAGE, ABR_PAGE, GENERATION_SWAP_PAGE];

/// Контроль линейки: тот же сценарий, но БЕЗ экстеншена. Браузер играет сам, поэтому
/// настоящий счётчик обязан насчитать нормальное воспроизведение и приговор
/// «браузер не декодирует» обязан не пройти. Если контроль не проходит, измерение
/// под тенью (`capture`) ничего не доказывает.
fn run_decoder_control(stand: &Stand, browser: BrowserKind, page: CapturePage) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| {
        let run = HandoffRun::open(
            stand,
            environment,
            browser,
            page.id,
            "control",
            ExtensionMode::ControlWithoutExtension,
        )?;
        let readiness = run.wait_page_ready();
        let verdict = match readiness {
            PageReadiness::Ready => {
                run.tell_page("handed-off");
                let hook = page.hook.map(|name| {
                    let command = format!("hook:{name}");
                    run.tell_page(&command);
                    run.wait_ack(&command, ACK_TIMEOUT)
                });
                std::thread::sleep(CONTROL_WINDOW);
                let report = run.finish_page();
                let verdict = judge_decoder_control(&report, page.id);
                match hook {
                    Some(Err(message)) => Verdict::fail(format!(
                        "the page did not finish its action: {message}\n{}",
                        verdict.detail
                    )),
                    _ => verdict,
                }
            }
            PageReadiness::ReportedEarly(_) | PageReadiness::TimedOut => {
                Verdict::fail("the control page never became ready")
            }
        };
        run.browser.shutdown()?;
        Ok(verdict)
    });
    StepResult::new(
        "capture",
        browser,
        format!("(control) {}", page.id),
        verdict,
        started.elapsed(),
    )
}

/// Чистый приговор контроля: честный браузер насчитал нормальное воспроизведение, и
/// пороговая проверка «не декодирует» его бы не пропустила.
fn judge_decoder_control(report: &Result<PageReport, SiteError>, page_id: &str) -> Verdict {
    let mut failures = page_failures(report, page_id);
    let report = match report {
        Ok(report) => report,
        Err(error) => return Verdict::fail(format!("control page: {error}")),
    };
    let metric = |name: &str| report.metrics.get(name).copied();
    let (Some(window_s), Some(real_frames)) =
        (metric("shadow_window_s"), metric("real_frames_in_window"))
    else {
        return Verdict::fail("the control page report has no real-frame metrics");
    };
    let needed = window_s * MIN_CONTROL_REAL_FRAMES_PER_SECOND;
    if real_frames < needed {
        failures.push(format!(
            "the real counter saw only {real_frames} frames in {window_s:.1} s of ordinary playback (need {needed:.0}): the ruler is broken"
        ));
    }
    if browser_does_not_decode(report).is_empty() {
        failures.push(
            "the 'browser does not decode' check accepted a browser that plays by itself"
                .to_owned(),
        );
    }
    Verdict::from_failures(
        &failures,
        format!("control: {real_frames} real frames in {window_s:.1} s without the extension"),
    )
}

/// Одна страница; `command` — имя шага в таблице (`capture` или `smoke`).
pub(super) fn run_page(
    stand: &Stand,
    browser: BrowserKind,
    command: &'static str,
    page: CapturePage,
) -> StepResult {
    run_page_tweaked(
        stand,
        browser,
        command,
        page,
        &RunTweaks::default(),
        page.id.to_owned(),
    )
}

/// Та же страница с отклонениями прогона (порядок установки, блокировщик): матрица
/// блокировщиков. `subject` — подпись строки таблицы (страница и конфигурация).
pub(super) fn run_page_tweaked(
    stand: &Stand,
    browser: BrowserKind,
    command: &'static str,
    page: CapturePage,
    tweaks: &RunTweaks,
    subject: String,
) -> StepResult {
    let started = Instant::now();
    let verdict = stand
        .with_environment(|environment| drive(stand, environment, browser, page, tweaks, started));
    StepResult::new(command, browser, subject, verdict, started.elapsed())
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page: CapturePage,
    tweaks: &RunTweaks,
    run_started_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open_tweaked(
        stand,
        environment,
        browser_kind,
        page.id,
        page.id,
        ExtensionMode::Real,
        HANDOFF_MODE_QUERY,
        tweaks,
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

    // Критическая развилка: если фон не принял передачу, дальше ждать нечего.
    let outcome = run.hand_off();
    let hand_off = match outcome {
        Ok(HandOffOutcome::Started) => Ok(()),
        Ok(other) => Err(format!("hand_off_tab outcome: {other:?}")),
        Err(error) => Err(format!("hand_off_tab: {error}")),
    };
    if let Err(message) = &hand_off {
        let log = format_browser_log(&run.take_page_log());
        run.browser.shutdown()?;
        return Ok(Verdict::fail(format!("{message}\n{log}")));
    }
    // Для `<video src>`: с этого мига файл должен качать только фон (сервер это видит).
    let handed_off_at = Instant::now();
    run.tell_page("handed-off");
    // Страница на паузе (главное видео `banner-with-main`): зритель «нажимает play» сразу,
    // иначе кадров в плеере не будет и ждать их бессмысленно.
    let early_hook = page
        .hook
        .filter(|_| page.hook_timing == HookTiming::BeforeFrames)
        .map(|name| {
            hold_player_paused(environment).and_then(|()| send_hook_and_wait_ack(&run, name))
        });

    // Сначала пропуск от НАШЕГО посредника: без него «кадры есть» ничего бы не доказывало.
    let session = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    let frames = match &session {
        Ok(()) => run
            .player_log
            .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
            .map_err(|error| format!("player did not present frames: {error}")),
        Err(message) => Err(format!("{message}; frames were not awaited")),
    };
    let hook = page.hook.map(|name| match early_hook {
        Some(acknowledged) => growth_after_early_hook(&mut run, name, acknowledged),
        None => run_hook(&mut run, name),
    });

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let file = server_evidence(stand, page.id, run_started_at, handed_off_at);
    let player_log = &mut run.player_log;
    let forbidden = player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    // Журнал посредника лежит в stderr браузера: читаем после мягкого закрытия.
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_capture(&CaptureEvidence {
        page_id: page.id,
        frames,
        hook,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
        file,
    }))
}

/// Улики со стороны сервера для страниц, где они есть: файл `<video src>` (кто и с чем его
/// качал), каталог родного HLS (элемент молчит, фон качает плейлисты, сегменты, ключ), Referer на `/cdn/media/`, приманка рядом с главным видео. `None` — у страницы их нет.
/// В счёт идут только запросы этого прогона (журнал сервера общий на весь запуск стенда).
pub(super) fn server_evidence(
    stand: &Stand,
    page_id: &str,
    run_started_at: Instant,
    handed_off_at: Instant,
) -> Option<(Vec<String>, String)> {
    let mut requests = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    if let Some(file_page) = file_evidence::file_page(page_id) {
        let sorted = file_evidence::sort_requests(file_page, &requests, handed_off_at);
        let mut failures = file_evidence::file_failures(file_page, &sorted);
        if referer_evidence::is_referer_page(page_id) {
            failures.extend(referer_evidence::referer_failures(
                &sorted,
                &stand.site.main_origin(),
            ));
        }
        return Some((failures, file_evidence::file_summary(&sorted)));
    }
    if let Some(hls_page) = hls_evidence::hls_page(page_id) {
        let sorted = hls_evidence::sort_requests(hls_page, &requests, handed_off_at);
        return Some((
            hls_evidence::hls_failures(hls_page, &sorted),
            hls_evidence::hls_summary(hls_page, &sorted),
        ));
    }
    decoys::decoy_page(page_id).map(|decoy| {
        decoys::decoy_not_fetched_by_background(decoy.file_path, &requests, handed_off_at)
    })
}

/// Страница стояла на паузе, значит, и плеер обязан стоять, пока она не запустит видео. Держим
/// паузу дольше сторожа тишины (10 с): реклама рядом с фильмом идёт десятки секунд, и сеанс обязан
/// это пережить. Позицию читаем по MPRIS: сводки кадров выходят редко и на паузе молчат.
fn hold_player_paused(environment: &BenchEnvironment) -> Result<(), String> {
    let bus_address = environment
        .bus_address()
        .map_err(|error| format!("no player bus: {error}"))?;
    let control = PlayerControl::on_bus(bus_address);
    control
        .wait_until_controllable(SESSION_GRANTED_TIMEOUT)
        .map_err(|error| format!("the player never became controllable: {error}"))?;
    std::thread::sleep(PAUSED_START_SETTLE);
    let position_at = |moment: &str| {
        control
            .position_micros()
            .map_err(|error| format!("cannot read the player position {moment}: {error}"))
    };
    let before = position_at("before the hold")?;
    std::thread::sleep(PAUSED_HOLD);
    let after = position_at("after the hold")?;
    judge_paused_hold(before, after)
}

/// Чистый приговор удержанию паузы.
fn judge_paused_hold(before_us: i64, after_us: i64) -> Result<(), String> {
    let drift_us = (after_us - before_us).abs();
    if drift_us <= PAUSED_DRIFT_LIMIT_US {
        Ok(())
    } else {
        Err(format!(
            "the page was paused but the player moved {before_us} -> {after_us} us"
        ))
    }
}

/// Посылает действие страницы и ждёт подтверждение (кадры плеера здесь не ждём).
fn send_hook_and_wait_ack(run: &HandoffRun<'_>, name: &str) -> Result<(), String> {
    let command = format!("hook:{name}");
    run.tell_page(&command);
    run.wait_ack(&command, ACK_TIMEOUT)
        .map_err(|message| format!("hook was not acknowledged: {message}"))
}

/// Ждёт, пока кадры плеера вырастут между двумя сводками (после действия страницы).
fn wait_growth_after_hook(run: &mut HandoffRun<'_>) -> Result<FrameGrowth, String> {
    run.player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED_AFTER_HOOK, HOOK_FRAMES_TIMEOUT)
        .map_err(|error| format!("player frames did not keep growing after the hook: {error}"))
}

/// Действие страницы под тенью: команда, подтверждение и рост кадров плеера после него.
fn run_hook(run: &mut HandoffRun<'_>, name: &str) -> HookEvidence {
    // Точка отсчёта кадров — сводки ПОСЛЕ этого места журнала (старые уже учтены).
    let skipped = run.player_log.skip_history();
    let acknowledged = send_hook_and_wait_ack(run, name);
    let growth = match (skipped, acknowledged) {
        (Err(error), _) => Err(format!("cannot read the player log: {error}")),
        (Ok(()), Err(message)) => Err(message),
        (Ok(()), Ok(())) => wait_growth_after_hook(run),
    };
    HookEvidence {
        name: name.to_owned(),
        growth,
    }
}

/// Действие было послано ДО кадров (`HookTiming::BeforeFrames`): здесь, когда кадры уже дошли,
/// остаётся проверить, что они растут и дальше.
fn growth_after_early_hook(
    run: &mut HandoffRun<'_>,
    name: &str,
    acknowledged: Result<(), String>,
) -> HookEvidence {
    let growth = match (run.player_log.skip_history(), acknowledged) {
        (Err(error), _) => Err(format!("cannot read the player log: {error}")),
        (Ok(()), Err(message)) => Err(message),
        (Ok(()), Ok(())) => wait_growth_after_hook(run),
    };
    HookEvidence {
        name: name.to_owned(),
        growth,
    }
}

/// Страница отчиталась до передачи: `unsupported` честен только там, где таблица его разрешает.
pub(super) fn judge_early_report(
    page_id: &str,
    browser: BrowserKind,
    report: &PageReport,
) -> Verdict {
    let allowed = PAGE_SCENARIOS
        .iter()
        .find(|scenario| scenario.id == page_id)
        .is_some_and(|scenario| scenario.may_be_unsupported_in(browser));
    let failed: Vec<String> = report
        .checks
        .iter()
        .filter(|check| !check.ok)
        .map(|check| format!("{} ({})", check.name, check.detail))
        .collect();
    match report.status {
        PageStatus::Unsupported if allowed => Verdict::unsupported("the page is unsupported here"),
        PageStatus::Unsupported => Verdict::fail(format!(
            "unsupported is not allowed for {page_id} in {browser:?}"
        )),
        _ => Verdict::fail(format!(
            "the page reported {:?} before the hand-off: {}",
            report.status,
            failed.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::judge::tests::report_with;
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::PageStatus;

    #[test]
    fn a_player_that_stands_still_passes_the_paused_hold() {
        assert_eq!(judge_paused_hold(5_000_000, 5_000_000), Ok(()));
        assert_eq!(
            judge_paused_hold(5_000_000, 5_000_000 + PAUSED_DRIFT_LIMIT_US),
            Ok(())
        );
    }

    #[test]
    fn a_player_that_plays_over_the_paused_page_fails_the_hold() {
        let verdict = judge_paused_hold(5_000_000, 5_000_000 + PAUSED_DRIFT_LIMIT_US + 1);
        assert!(verdict.is_err_and(|message| message.contains("the player moved")));
        // Назад — тоже движение: стоящий плеер позицию не меняет.
        assert!(judge_paused_hold(5_000_000, 1_000_000).is_err());
    }

    #[test]
    fn the_hold_outlives_the_session_silence_watchdog() {
        const SILENCE_WATCHDOG: Duration = Duration::from_secs(10);
        assert!(PAUSED_HOLD > SILENCE_WATCHDOG);
    }

    #[test]
    fn early_unsupported_is_honest_only_where_the_table_allows_it() {
        let mut report = report_with(&[]);
        report.status = PageStatus::Unsupported;
        report.scenario = "mse-av1".into();
        // В таблице страниц `unsupported` не разрешён нигде для mse-av1 → провал.
        let verdict = judge_early_report("mse-av1", BrowserKind::Firefox, &report);
        assert_eq!(verdict.outcome, Outcome::Fail);
        // Для mse-worker в Firefox таблица его разрешает.
        let verdict = judge_early_report("mse-worker", BrowserKind::Firefox, &report);
        assert_eq!(verdict.outcome, Outcome::Unsupported);
        report.status = PageStatus::Fail;
        assert_eq!(
            judge_early_report("mse-fmp4", BrowserKind::Chromium, &report).outcome,
            Outcome::Fail
        );
    }

    fn control_report(window_s: f64, real_frames: f64) -> Result<PageReport, SiteError> {
        Ok(report_with(&[
            ("real_probe_available", 1.0),
            ("shadow_window_s", window_s),
            ("real_frames_in_window", real_frames),
        ]))
    }

    #[test]
    fn control_passes_only_when_the_ruler_sees_ordinary_playback() {
        // 8 с обычной игры по 25 к/с: линейка видит ~200 кадров, проверка «не декодирует» их отвергает.
        assert_eq!(
            judge_decoder_control(&control_report(8.0, 200.0), "mse-fmp4").outcome,
            Outcome::Pass
        );
        // Линейка, которая молчит у играющего браузера, сломана.
        let broken = judge_decoder_control(&control_report(8.0, 2.0), "mse-fmp4");
        assert_eq!(broken.outcome, Outcome::Fail);
        assert!(broken.detail.contains("the ruler is broken"));
        // Нет метрик — это не успех.
        assert_eq!(
            judge_decoder_control(&Ok(report_with(&[])), "mse-fmp4").outcome,
            Outcome::Fail
        );
    }

    #[test]
    fn capture_table_lists_only_known_pages_with_distinct_ids() {
        let mut seen = std::collections::HashSet::new();
        for page in CAPTURE_PAGES {
            assert!(
                PAGE_SCENARIOS.iter().any(|known| known.id == page.id),
                "{} is not a stand page",
                page.id
            );
            assert!(seen.insert(page.id), "{} listed twice", page.id);
        }
        assert_eq!(CAPTURE_PAGES.len(), 26);
    }
}
