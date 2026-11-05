//! Страницы «реклама → фильм» (сессия 09c2, решения 201 и 204): играет рекламный ролик, робот
//! отдаёт вкладку ПОСРЕДИ рекламы, реклама кончается (или её пропускают), страница запускает фильм.
//! Плеер обязан доиграть рекламу, переехать на фильм В ТОМ ЖЕ окне (один сеанс) и показать его
//! кадрами, а браузер при этом почти не декодирует.
//!
//! Форма стыка у каждой страницы своя (в таблице [`AD_HOP_PAGES`]): отдельный элемент, элемент
//! рядом с готовым фильмом, рекламный iframe другого origin, один и тот же элемент, цепочка
//! роликов, пропуск рекламы посреди ролика, баннер рядом с фильмом. Приговор общий и чистый:
//! [`judge::judge_ad_hop`]. Страница без фильма (`ad-no-film`) идёт другим сценарием, `page-ends`.

mod judge;

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::super::handoff_run::{ACK_TIMEOUT, ExtensionMode, HandoffRun, PageReadiness};
use super::super::outcome::{StepResult, Verdict};
use super::super::run_tweaks::RunTweaks;
use super::decoys::{AD_FILE_PATH, count_fetches};
use super::judge_early_report;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameGrowth, read_host_evidence};
use crate::player_control::PlayerControl;
use judge::{AdHopEvidence, AdHopExpectation, MIN_FILM_FRAMES_AFTER_START, judge_ad_hop};

/// Рекламный домен файла ролика: правила экстеншена считают его рекламным по суффиксу
/// `doubleclick.net`. Имя дублирует `AD_HOST` в `site/pages/ad-preroll.html` (тест их сверяет).
pub(in crate::scenarios) const AD_HOST: &str = "ad.doubleclick.net";
/// Файл второго рекламного ролика (цепочка) на сервере стенда.
const SECOND_AD_FILE_PATH: &str = "/media/progressive/ad2.mp4";
/// Файл фильма в варианте «обычный файл в том же элементе» (60 с: за окно прогона не кончится).
const FILM_FILE_PATH: &str = "/media/progressive/h264-aac-long.mp4";
/// Файл баннера рядом с фильмом (`ad-then-banner`).
const BANNER_FILE_PATH: &str = "/media/progressive/h264-aac.mp4";

/// Сколько ждём метку страницы «передавать можно» (реклама заиграла).
const AD_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём, пока страница запустит фильм: самая длинная цепочка (три ролика) — около 30 с
/// после передачи, остальное — запас на медленную машину.
const FILM_STARTED_TIMEOUT: Duration = Duration::from_secs(120);
/// Метка страницы: фильм запущен (`film-started play=<исход>`).
const FILM_STARTED_MARKER: &str = "film-started";
/// Сколько ждём две сводки плеера, между которыми кадры фильма выросли (сводки раз в 10 с).
const FILM_GROWTH_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём, пока плеер станет управляемым и сядет на рекламу, прежде чем её пропускать.
const PLAYER_ON_AD_TIMEOUT: Duration = Duration::from_secs(45);
/// С какой позиции плеера пропуск рекламы честен: страница уже оторвалась от начала ролика.
const SKIP_MIN_POSITION_MICROS: i64 = 2_000_000;
/// Шаг опроса позиции плеера.
const POSITION_POLL_STEP: Duration = Duration::from_millis(200);
/// Строка журнала плеера: окно показало зрителю сообщение о конце сеанса.
const END_NOTICE_MARKER: &str = "Browser session end notice shown";

/// Страница стенда в этом сценарии и то, что от неё ждёт приговор.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios) struct AdHopPage {
    /// Имя шага в отчёте и в фильтре `--page` (у вариантов одной страницы разное).
    pub(in crate::scenarios) step_id: &'static str,
    /// Идентификатор страницы стенда (`site/pages/<id>.html`).
    pub(in crate::scenarios) page_id: &'static str,
    /// Что добавить к адресу страницы.
    query_suffix: &'static str,
    /// Нужен ли рекламный домен из списка экстеншена (ведёт на сервер стенда).
    uses_listed_ad_host: bool,
    /// Действие страницы по команде робота (`hook:<имя>`), если оно нужно.
    pub(in crate::scenarios) hook: Option<&'static str>,
    /// Что обязана показать страница.
    pub(in crate::scenarios::capture) expectation: AdHopExpectation,
}

/// Реклама → один фильм: два захода (реклама и фильм), реклама ушла в плеер.
const fn single_ad(
    step_id: &'static str,
    page_id: &'static str,
    query_suffix: &'static str,
) -> AdHopPage {
    AdHopPage {
        step_id,
        page_id,
        query_suffix,
        uses_listed_ad_host: false,
        hook: None,
        expectation: AdHopExpectation {
            ads_ended: 1,
            min_loads: 2,
            background_files: &[AD_FILE_PATH],
            banner_file: None,
        },
    }
}

/// Та же страница с другими ожиданиями.
const fn expecting(page: AdHopPage, expectation: AdHopExpectation) -> AdHopPage {
    AdHopPage {
        expectation,
        ..page
    }
}

/// Страница с рекламным доменом из списка правил экстеншена.
const fn on_listed_ad_host(page: AdHopPage) -> AdHopPage {
    AdHopPage {
        uses_listed_ad_host: true,
        ..page
    }
}

/// Страница с действием по команде робота.
const fn with_hook(page: AdHopPage, hook: &'static str) -> AdHopPage {
    AdHopPage {
        hook: Some(hook),
        ..page
    }
}

/// Страницы сценария. Каждая — отдельное прогонное окружение и отдельная строка отчёта.
pub(in crate::scenarios) const AD_HOP_PAGES: &[AdHopPage] = &[
    // Реклама с рекламного домена из списка, отдельный элемент, фильм в новом элементе.
    on_listed_ad_host(single_ad("ad-preroll", "ad-preroll", "&mode=handoff")),
    // Живая форма Tortuga: домен не из списка, рядом пустой `<video>` той же площади.
    single_ad(
        "ad-preroll-unlisted",
        "ad-preroll",
        "&mode=handoff&ad=unlisted",
    ),
    // Живая форма Ashdi: фильм уже загружен на паузе, реклама поверх.
    single_ad("ad-beside-film", "ad-beside-film", "&mode=handoff"),
    // Реклама в iframe другого origin, фильм в родительском документе.
    single_ad("ad-film-frames", "ad-film-frames", "&mode=handoff"),
    // Реклама и фильм в одном элементе: новый MediaSource.
    single_ad(
        "ad-film-same-element",
        "ad-film-same-element",
        "&mode=handoff",
    ),
    // То же, но фильм — обычный файл: его качает фон по `fetch_range`.
    expecting(
        single_ad(
            "ad-film-same-element-file",
            "ad-film-same-element",
            "&mode=handoff&film=file",
        ),
        AdHopExpectation {
            ads_ended: 1,
            min_loads: 2,
            background_files: &[AD_FILE_PATH, FILM_FILE_PATH],
            banner_file: None,
        },
    ),
    // Три ролика подряд (Tortuga, потом Ashdi) и фильм: четыре загрузки в одном окне.
    expecting(
        single_ad("ad-chain", "ad-chain", "&mode=handoff"),
        AdHopExpectation {
            ads_ended: 3,
            min_loads: 4,
            background_files: &[AD_FILE_PATH, SECOND_AD_FILE_PATH],
            banner_file: None,
        },
    ),
    // Пропуск рекламы посреди ролика: конца у рекламы нет, фильм идёт в том же окне.
    expecting(
        with_hook(
            single_ad("ad-skipped-midway", "ad-skipped-midway", "&mode=handoff"),
            "skip-ad",
        ),
        AdHopExpectation {
            ads_ended: 0,
            min_loads: 2,
            background_files: &[AD_FILE_PATH],
            banner_file: None,
        },
    ),
    // Рядом с фильмом заиграл баннер: фильм в плеере, баннер у браузера.
    expecting(
        single_ad("ad-then-banner", "ad-then-banner", "&mode=handoff"),
        AdHopExpectation {
            ads_ended: 1,
            min_loads: 2,
            background_files: &[AD_FILE_PATH],
            banner_file: Some(BANNER_FILE_PATH),
        },
    ),
];

/// Идентификаторы страниц сценария (для `--page` и таблицы `capture`).
pub(in crate::scenarios) fn page_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for page in AD_HOP_PAGES {
        if !ids.contains(&page.page_id) {
            ids.push(page.page_id);
        }
    }
    ids
}

pub(in crate::scenarios) fn run(
    stand: &Stand,
    browser: BrowserKind,
    page: AdHopPage,
) -> StepResult {
    let started = Instant::now();
    let verdict =
        stand.with_environment(|environment| drive(stand, environment, browser, page, started));
    StepResult::new("capture", browser, page.step_id, verdict, started.elapsed())
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page: AdHopPage,
    run_started_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let tweaks = RunTweaks {
        loopback_host_aliases: if page.uses_listed_ad_host {
            vec![AD_HOST.to_owned()]
        } else {
            Vec::new()
        },
        ..RunTweaks::default()
    };
    let mut run = HandoffRun::open_tweaked(
        stand,
        environment,
        browser_kind,
        page.page_id,
        page.step_id,
        ExtensionMode::Real,
        page.query_suffix,
        &tweaks,
    )?;
    match run.wait_page_marker("handoff-ready", AD_READY_TIMEOUT) {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(page.page_id, browser_kind, &report);
            run.browser.shutdown()?;
            return Ok(verdict);
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!(
                "the page never showed the ad\n{log}"
            )));
        }
    }

    // Передача ПОСРЕДИ рекламы: с 09c2 это обычный `started`, а не отказ.
    let hand_off = run.hand_off().map_err(|error| error.to_string());
    let handed_off_at = Instant::now();
    let film_growth = if matches!(hand_off, Ok(HandOffOutcome::Started)) {
        run.tell_page("handed-off");
        follow_the_hop(&mut run, environment, page.hook)?
    } else {
        Err("not awaited: the ad was not handed off".to_owned())
    };

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let player_log = &mut run.player_log;
    let forbidden = player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let end_notice_lines = player_log.snapshot_forbidden(&[END_NOTICE_MARKER])?;
    let max_loads = player_log.snapshot_max_loads()?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    let fetches = fetch_counts(stand, &page, run_started_at, handed_off_at);
    Ok(judge_ad_hop(
        &page.expectation,
        &AdHopEvidence {
            page_id: page.page_id,
            hand_off,
            host,
            film_growth,
            max_loads,
            fetches,
            end_notice_lines,
            page_report,
            forbidden_player_lines: forbidden,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Журнал сервера (только этого прогона): кто и сколько раз качал файлы рекламы и баннера.
fn fetch_counts(
    stand: &Stand,
    page: &AdHopPage,
    run_started_at: Instant,
    handed_off_at: Instant,
) -> Vec<super::decoys::FetchCounts> {
    let mut requests = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    page.expectation
        .background_files
        .iter()
        .copied()
        .chain(page.expectation.banner_file)
        .map(|path| count_fetches(path, &requests, handed_off_at))
        .collect()
}

/// Передача состоялась: ждём пропуск посредника, при необходимости выполняем действие страницы
/// (пропуск рекламы), ждём запуск фильма страницей и рост кадров плеера ПОСЛЕ него.
/// Внешний `Result` — ошибка стенда, внутренний — звено цепи не сработало.
fn follow_the_hop(
    run: &mut HandoffRun<'_>,
    environment: &BenchEnvironment,
    hook: Option<&'static str>,
) -> Result<Result<FrameGrowth, String>, ScenarioError> {
    if let Err(message) = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        return Ok(Err(format!("{message}; the film was not awaited")));
    }
    if let Some(name) = hook
        && let Err(message) = skip_the_ad(run, environment, name)
    {
        return Ok(Err(message));
    }
    if let Err(message) = run.wait_page_log_line(FILM_STARTED_MARKER, FILM_STARTED_TIMEOUT) {
        return Ok(Err(format!("the page never started the film: {message}")));
    }
    // Точка отсчёта — сводки ПОСЛЕ запуска фильма: кадры рекламы в счёт не идут.
    run.player_log.skip_history()?;
    Ok(run
        .player_log
        .wait_for_frame_growth(MIN_FILM_FRAMES_AFTER_START, FILM_GROWTH_TIMEOUT)
        .map_err(|error| {
            format!("player frames did not grow after the film started in the page: {error}")
        }))
}

/// Пропуск рекламы: ждём, пока плеер играет рекламу (позиция ушла от начала), и шлём странице действие.
fn skip_the_ad(
    run: &HandoffRun<'_>,
    environment: &BenchEnvironment,
    hook: &str,
) -> Result<(), String> {
    let control = PlayerControl::on_bus(
        environment
            .bus_address()
            .map_err(|error| format!("no player bus: {error}"))?,
    );
    control
        .wait_until_controllable(PLAYER_ON_AD_TIMEOUT)
        .map_err(|error| format!("the player never became controllable: {error}"))?;
    let deadline = Instant::now() + PLAYER_ON_AD_TIMEOUT;
    loop {
        let position = control
            .position_micros()
            .map_err(|error| format!("cannot read the player position: {error}"))?;
        if position >= SKIP_MIN_POSITION_MICROS {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the player never moved past {SKIP_MIN_POSITION_MICROS} us on the ad (at {position} us)"
            ));
        }
        std::thread::sleep(POSITION_POLL_STEP);
    }
    let command = format!("hook:{hook}");
    run.tell_page(&command);
    run.wait_ack(&command, ACK_TIMEOUT)
        .map_err(|message| format!("the page did not finish '{hook}': {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    fn site_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("site")
    }

    #[test]
    fn the_ad_host_is_the_one_the_page_uses_and_the_rules_call_an_ad_domain() {
        let page = std::fs::read_to_string(site_dir().join("pages/ad-preroll.html"))
            .expect("the ad-preroll page");
        assert!(page.contains(&format!("const AD_HOST = '{AD_HOST}';")));
        // Рекламный суффикс из правил экстеншена (`doubleclick.net`).
        assert!(AD_HOST.ends_with(".doubleclick.net"));
    }

    #[test]
    fn steps_are_unique_and_pages_exist_in_the_catalog_and_on_disk() {
        let mut steps = HashSet::new();
        for page in AD_HOP_PAGES {
            assert!(
                steps.insert(page.step_id),
                "duplicate step {}",
                page.step_id
            );
            assert!(
                crate::scenarios::page_catalog::PAGE_SCENARIOS
                    .iter()
                    .any(|known| known.id == page.page_id),
                "{} is not in the page catalog",
                page.page_id
            );
            assert!(
                site_dir()
                    .join(format!("pages/{}.html", page.page_id))
                    .is_file()
            );
            assert!(page.expectation.min_loads >= 2, "ad and film are two loads");
            assert!(
                page.query_suffix.starts_with("&mode=handoff"),
                "{} must run in the hand-off mode",
                page.step_id
            );
        }
        assert_eq!(
            page_ids(),
            [
                "ad-preroll",
                "ad-beside-film",
                "ad-film-frames",
                "ad-film-same-element",
                "ad-chain",
                "ad-skipped-midway",
                "ad-then-banner",
            ]
        );
    }

    #[test]
    fn every_hook_is_declared_in_its_page_and_media_paths_match_the_generator() {
        for page in AD_HOP_PAGES {
            let Some(hook) = page.hook else { continue };
            let source =
                std::fs::read_to_string(site_dir().join(format!("pages/{}.html", page.page_id)))
                    .expect("the page");
            assert!(
                source.contains(&format!("'{hook}'")),
                "page {} does not declare the hook '{hook}'",
                page.page_id
            );
        }
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("media/generate.sh"),
        )
        .expect("the media generator");
        assert!(
            script.contains("make_ad_clip ad2.mp4 "),
            "the second ad moved"
        );
        assert_eq!(SECOND_AD_FILE_PATH, "/media/progressive/ad2.mp4");
        assert!(script.contains("progressive-long-h264-aac file progressive/h264-aac-long.mp4"));
        assert_eq!(FILM_FILE_PATH, "/media/progressive/h264-aac-long.mp4");
        assert!(script.contains("progressive-h264-aac file progressive/h264-aac.mp4"));
        assert_eq!(BANNER_FILE_PATH, "/media/progressive/h264-aac.mp4");
    }

    /// Рекламные ролики ДОЛЖНЫ быть короткими и без цикла: страница, которая зациклила бы рекламу,
    /// не проверяла бы конец рекламы вовсе.
    #[test]
    fn ad_pages_never_loop_the_ad() {
        for page in AD_HOP_PAGES {
            let source =
                std::fs::read_to_string(site_dir().join(format!("pages/{}.html", page.page_id)))
                    .expect("the page");
            for line in source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
            {
                // Цикл разрешён только баннеру.
                if line.contains("loop") && !line.contains("banner") {
                    panic!(
                        "page {} loops something that is not a banner: {line}",
                        page.page_id
                    );
                }
            }
        }
    }
}
