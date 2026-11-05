//! `blocker-matrix` (09c, решения 72 и 171): блокировщик рекламы ставит свои обёртки на те же
//! API страницы, что и наш экстеншен (`Worker`, `MediaSource`, `fetch`), поэтому порядок
//! установки и само присутствие блокировщика могут сломать передачу. Матрица: страницы ×
//! конфигурации; каждая клетка судится до КАДРА в рендере плеера обычным приговором `capture`.
//!
//! Конфигурации (в одном браузере):
//!   - без блокировщика (контроль матрицы);
//!   - блокировщик ставится ПЕРВЫМ, наш экстеншен вторым;
//!   - наш экстеншен ПЕРВЫМ, блокировщик вторым.
//!
//! Перед клетками каждой конфигурации идёт приманка `blocker-bait`: она доказывает, что блокировщик
//! правда работает в браузере. Без неё молча не вставший блокировщик дал бы те же зелёные клетки,
//! что и контроль.
//!
//! Страницы: `mse-fmp4` (обычный MSE), `mse-worker-classic` (обёртка Worker; в Firefox честно
//! `unsupported`), `video-src` (файл), `codec-choice` (ответы на вопросы о кодеках + выбранный
//! ролик играет).
//!
//! Блокировщики стенд НЕ хранит: их качает `scripts/browser-bridge-bench-fetch-blockers.sh` из
//! официальных релизов в `target/browser-bridge-bench/blockers/<имя>/`. Нет файлов — шаг честно
//! падает с командой для запуска. Сценарий не входит в `all` (нужна сеть для первой закачки).

use std::path::{Path, PathBuf};

use std::time::{Duration, Instant};

use super::capture::{
    AD_FILE_PATH, AD_HOST, CapturePage, SMOKE_PAGE, plain_page, run_page_tweaked,
};
use super::context::{ScenarioError, Stand};
use super::handoff_run::{ExtensionMode, HANDOFF_MODE_QUERY, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::run_tweaks::{ExtraExtensions, InstallOrder, RunTweaks};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;

/// Команда, которая кладёт блокировщики на место (путь от корня репозитория).
const FETCH_COMMAND: &str = "scripts/browser-bridge-bench-fetch-blockers.sh";
/// Каталог блокировщиков внутри `target/`.
const BLOCKERS_DIRECTORY: &str = "target/browser-bridge-bench/blockers";
/// Каталог uBlock Origin Lite (Chromium: распакованный экстеншен с `manifest.json`).
const CHROMIUM_BLOCKER_DIR: &str = "ublock-origin-lite";
/// Каталог uBlock Origin (Firefox: внутри один подписанный `.xpi`).
const FIREFOX_BLOCKER_DIR: &str = "ublock-origin";

/// Страницы матрицы.
const MATRIX_PAGES: &[CapturePage] = &[
    SMOKE_PAGE,
    plain_page("mse-worker-classic"),
    plain_page("video-src"),
    plain_page("codec-choice"),
];

/// Страница-приманка блокировщика.
const BAIT_PAGE: &str = "blocker-bait";
/// Подождать запись запроса в журнале сервера после отчёта страницы.
const BAIT_LOG_SETTLE: Duration = Duration::from_millis(200);

/// Конфигурация клетки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Configuration {
    /// Блокировщика нет.
    WithoutBlocker,
    /// Блокировщик первым, наш экстеншен вторым.
    BlockerFirst,
    /// Наш экстеншен первым, блокировщик вторым.
    OurExtensionFirst,
}

const CONFIGURATIONS: [Configuration; 3] = [
    Configuration::WithoutBlocker,
    Configuration::BlockerFirst,
    Configuration::OurExtensionFirst,
];

impl Configuration {
    /// Подпись для таблицы.
    fn label(self) -> &'static str {
        match self {
            Self::WithoutBlocker => "no blocker",
            Self::BlockerFirst => "blocker first",
            Self::OurExtensionFirst => "ours first",
        }
    }

    /// Отклонения прогона для этой конфигурации; `blocker` — путь блокировщика.
    fn tweaks(self, blocker: &Path) -> RunTweaks {
        match self {
            // Явно пустой список: матрица не зависит от `--extra-extension` пользователя.
            Self::WithoutBlocker => RunTweaks {
                extras: ExtraExtensions::Explicit(Vec::new()),
                ..RunTweaks::default()
            },
            Self::BlockerFirst => RunTweaks {
                order: InstallOrder::ExtrasFirst,
                extras: ExtraExtensions::Explicit(vec![blocker.to_path_buf()]),
                ..RunTweaks::default()
            },
            Self::OurExtensionFirst => RunTweaks {
                order: InstallOrder::ScenarioFirst,
                extras: ExtraExtensions::Explicit(vec![blocker.to_path_buf()]),
                ..RunTweaks::default()
            },
        }
    }
}

/// Находит блокировщик браузера в `blockers_root`; `Err` — понятный текст с командой закачки.
fn locate_blocker(browser: BrowserKind, blockers_root: &Path) -> Result<PathBuf, String> {
    let missing = |what: &str| {
        format!(
            "{what} in {}: run {FETCH_COMMAND} (it downloads the pinned release from GitHub and checks its SHA-256)",
            blockers_root.display()
        )
    };
    match browser {
        BrowserKind::Chromium => {
            let directory = blockers_root.join(CHROMIUM_BLOCKER_DIR);
            if directory.join("manifest.json").is_file() {
                Ok(directory)
            } else {
                Err(missing(
                    "uBlock Origin Lite (an unpacked directory with manifest.json) is absent",
                ))
            }
        }
        BrowserKind::Firefox => {
            let directory = blockers_root.join(FIREFOX_BLOCKER_DIR);
            let packages: Vec<PathBuf> = std::fs::read_dir(&directory)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .filter(|path| path.extension().is_some_and(|extension| extension == "xpi"))
                        .collect()
                })
                .unwrap_or_default();
            match packages.as_slice() {
                [only] => Ok(only.clone()),
                [] => Err(missing("uBlock Origin (.xpi) is absent")),
                _ => Err(format!(
                    "{} holds several .xpi files; leave exactly one (re-run {FETCH_COMMAND})",
                    directory.display()
                )),
            }
        }
    }
}

/// Страницы матрицы выбранного браузера с подписями клеток: чистая раскладка (проверяется тестом).
fn matrix_cells() -> Vec<(CapturePage, Configuration)> {
    MATRIX_PAGES
        .iter()
        .flat_map(|page| CONFIGURATIONS.iter().map(move |config| (*page, *config)))
        .collect()
}

pub(super) fn run(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    let blockers_root = stand.settings.repo_root.join(BLOCKERS_DIRECTORY);
    let blocker = match locate_blocker(browser, &blockers_root) {
        Ok(path) => path,
        Err(problem) => {
            return vec![StepResult::new(
                "blocker-matrix",
                browser,
                "(blocker artifact)",
                Verdict::fail(problem),
                std::time::Duration::ZERO,
            )];
        }
    };
    let baits = CONFIGURATIONS
        .iter()
        .map(|configuration| run_bait(stand, browser, *configuration, &blocker));
    let cells = matrix_cells().into_iter().map(|(page, configuration)| {
        run_page_tweaked(
            stand,
            browser,
            "blocker-matrix",
            page,
            &configuration.tweaks(&blocker),
            format!("{} [{}]", page.id, configuration.label()),
        )
    });
    baits.chain(cells).collect()
}

/// Одна проверка приманки: блокировщик этой конфигурации режет рекламный адрес или нет.
fn run_bait(
    stand: &Stand,
    browser: BrowserKind,
    configuration: Configuration,
    blocker: &Path,
) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| {
        drive_bait(stand, environment, browser, configuration, blocker, started)
    });
    StepResult::new(
        "blocker-matrix",
        browser,
        format!("{BAIT_PAGE} [{}]", configuration.label()),
        verdict,
        started.elapsed(),
    )
}

fn drive_bait(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    configuration: Configuration,
    blocker: &Path,
    run_started_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let tweaks = RunTweaks {
        loopback_host_aliases: vec![AD_HOST.to_owned()],
        ..configuration.tweaks(blocker)
    };
    let run = HandoffRun::open_tweaked(
        stand,
        environment,
        browser,
        BAIT_PAGE,
        BAIT_PAGE,
        ExtensionMode::Real,
        HANDOFF_MODE_QUERY,
        &tweaks,
    )?;
    let readiness = run.wait_page_ready();
    run.browser.shutdown()?;
    let PageReadiness::ReportedEarly(report) = readiness else {
        return Ok(Verdict::fail("the bait page never reported"));
    };
    std::thread::sleep(BAIT_LOG_SETTLE);
    let bait_requests_on_server = stand
        .site
        .media_requests()
        .iter()
        .filter(|request| request.at >= run_started_at && request.path == AD_FILE_PATH)
        .count();
    Ok(judge_bait(
        configuration,
        report.metrics.get("bait_attempts").copied(),
        bait_requests_on_server,
    ))
}

/// Приговор приманки: `attempts` — сколько запросов сделала страница, `on_server` — сколько дошло.
/// Судит сервер: страница не отличит отрезанный запрос от подменённого ответа. Первые запросы
/// блокировщик вправе пропустить, пока поднимает правила, поэтому «дошли не все», а не «ни одного».
fn judge_bait(configuration: Configuration, attempts: Option<f64>, on_server: usize) -> Verdict {
    let attempts = match attempts {
        Some(attempts) if attempts >= 1.0 => attempts as usize,
        _ => return Verdict::fail("the bait page made no attempts"),
    };
    match configuration {
        // Контроль: адрес живой. Иначе «не дошло» в других клетках значило бы только сеть.
        Configuration::WithoutBlocker if on_server == attempts => Verdict::pass(format!(
            "control: {on_server} of {attempts} requests reached the server"
        )),
        Configuration::WithoutBlocker => Verdict::fail(format!(
            "control: the ad address must work without a blocker: {on_server} of {attempts} requests reached the server"
        )),
        _ if on_server < attempts => Verdict::pass(format!(
            "the blocker stopped the ad address: {on_server} of {attempts} requests reached the server"
        )),
        _ => Verdict::fail(format!(
            "the blocker is not active, {on_server} of {attempts} requests reached the server: this configuration proves nothing"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bait_proves_the_blocker_works_and_the_control_keeps_the_address_alive() {
        use super::super::outcome::Outcome;
        let blockers = [
            Configuration::BlockerFirst,
            Configuration::OurExtensionFirst,
        ];
        for configuration in blockers {
            let cut = judge_bait(configuration, Some(8.0), 0);
            assert_eq!(cut.outcome, Outcome::Pass, "{}", cut.detail);
            // Правила поднялись не сразу: первые запросы прошли.
            let late = judge_bait(configuration, Some(8.0), 2);
            assert_eq!(late.outcome, Outcome::Pass, "{}", late.detail);
            // Блокировщик не встал: клетки ничего не доказывают.
            assert_eq!(
                judge_bait(configuration, Some(8.0), 8).outcome,
                Outcome::Fail
            );
            assert_eq!(judge_bait(configuration, None, 0).outcome, Outcome::Fail);
        }
        let control = Configuration::WithoutBlocker;
        let alive = judge_bait(control, Some(8.0), 8);
        assert_eq!(alive.outcome, Outcome::Pass, "{}", alive.detail);
        // Адрес мёртв и без блокировщика: «не дошло» в других клетках значило бы только сеть.
        assert_eq!(judge_bait(control, Some(8.0), 0).outcome, Outcome::Fail);
        assert_eq!(judge_bait(control, Some(8.0), 7).outcome, Outcome::Fail);
    }

    #[test]
    fn the_matrix_has_every_page_in_every_configuration() {
        let cells = matrix_cells();
        assert_eq!(cells.len(), MATRIX_PAGES.len() * CONFIGURATIONS.len());
        for page in [
            "mse-fmp4",
            "mse-worker-classic",
            "video-src",
            "codec-choice",
        ] {
            for configuration in CONFIGURATIONS {
                assert!(
                    cells
                        .iter()
                        .any(|(cell_page, cell)| cell_page.id == page && *cell == configuration),
                    "{page} / {configuration:?} is missing"
                );
            }
        }
    }

    #[test]
    fn matrix_pages_exist_in_the_page_catalog() {
        for page in MATRIX_PAGES {
            assert!(
                super::super::page_catalog::PAGE_SCENARIOS
                    .iter()
                    .any(|scenario| scenario.id == page.id),
                "{} is not a stand page",
                page.id
            );
        }
    }

    #[test]
    fn install_orders_put_the_blocker_where_the_configuration_says() {
        let blocker = Path::new("/blocker");
        let real = vec![crate::browsers::ExtensionSource::UnpackedDir(
            "/real".into(),
        )];
        let order_of = |configuration: Configuration| -> Vec<String> {
            configuration
                .tweaks(blocker)
                .arrange_extensions(real.clone(), &["/user-flag".into()])
                .into_iter()
                .map(|source| match source {
                    crate::browsers::ExtensionSource::UnpackedDir(path)
                    | crate::browsers::ExtensionSource::PackedFile(path) => {
                        path.to_string_lossy().into_owned()
                    }
                })
                .collect()
        };
        assert_eq!(order_of(Configuration::WithoutBlocker), ["/real"]);
        assert_eq!(order_of(Configuration::BlockerFirst), ["/blocker", "/real"]);
        assert_eq!(
            order_of(Configuration::OurExtensionFirst),
            ["/real", "/blocker"]
        );
    }

    #[test]
    fn a_missing_blocker_reports_the_fetch_command_instead_of_passing() {
        let empty = std::env::temp_dir().join("bench-no-such-blockers-dir");
        for browser in [BrowserKind::Chromium, BrowserKind::Firefox] {
            let problem = locate_blocker(browser, &empty).expect_err("nothing was fetched");
            assert!(problem.contains(FETCH_COMMAND), "{problem}");
        }
    }

    #[test]
    fn blockers_are_found_where_the_fetch_script_puts_them() {
        let root = std::env::temp_dir().join(format!("bench-blockers-{}", std::process::id()));
        let chromium = root.join(CHROMIUM_BLOCKER_DIR);
        let firefox = root.join(FIREFOX_BLOCKER_DIR);
        std::fs::create_dir_all(&chromium).expect("chromium dir");
        std::fs::create_dir_all(&firefox).expect("firefox dir");
        std::fs::write(chromium.join("manifest.json"), "{}").expect("manifest");
        std::fs::write(firefox.join("ublock-origin-1.xpi"), "x").expect("xpi");
        assert_eq!(locate_blocker(BrowserKind::Chromium, &root), Ok(chromium));
        assert_eq!(
            locate_blocker(BrowserKind::Firefox, &root),
            Ok(firefox.join("ublock-origin-1.xpi"))
        );
        // Два пакета — неоднозначно: ошибка, а не «взять любой».
        std::fs::write(firefox.join("other.xpi"), "x").expect("second xpi");
        assert!(locate_blocker(BrowserKind::Firefox, &root).is_err());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
