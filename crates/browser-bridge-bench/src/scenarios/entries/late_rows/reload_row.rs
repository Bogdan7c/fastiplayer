//! Ход строк позднего входа с перезагрузкой: страница играет, появляется экстеншен, человек
//! кликает по значку, жмёт «Reload and open», страница рождается заново, видео уходит в плеер.
//!
//! План строки ([`ReloadPlan`]) — данные; ход один на все строки, различия вынесены в перечисления.

use std::path::Path;
use std::time::{Duration, Instant};

use super::super::super::capture::plain_page;
use super::super::super::context::{ScenarioError, Stand};
use super::super::super::evidence::{
    SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::super::super::handoff_run::{ExtensionMode, real_extension_dir};
use super::super::super::outcome::Verdict;
use super::super::super::run_tweaks::{RunTweaks, RuntimeInstall};
use super::super::common::{
    EarlyUnsupported, Opened, ReadyRun, open_ready_with, with_extra_failures,
};
use super::super::entry_evidence::collect;
use super::super::menu_entry::BANNER_TIMEOUT;
use super::super::start_rows::{finish, read_start_position};
use super::extension_copy::{bump_version, copy_into_run_directory};
use super::steps::{
    LATE_QUERY, check_nothing_happened_yet, check_page_played_before_install,
    open_popup_expecting_reload_button, press_reload_button, read_beats, session_started,
    wait_for_reload, wait_until_played,
};
use crate::browsers::{BrowserKind, ExtensionSource};
use crate::environment::BenchEnvironment;
use crate::observe::FrameEvidence;
use crate::player_control::PlayerControl;

/// Цель калибровки: кнопка Play страницы `late-manual-play`.
pub(super) const PLAY_BUTTON_TARGET: &str = "play";
/// Допуск «та же секунда»: позиция плеера на старте против последней позиции страницы до перезагрузки.
const SAME_SECOND_TOLERANCE_US: i64 = 2_000_000;
/// Сайт-отказник: старт плеера обязан отстоять от запомненной секунды не меньше чем на это.
const MIN_GAP_FROM_REMEMBERED_US: i64 = 4_000_000;
/// Сайт-отказник: старт «с места, откуда начал сайт» — это начало плюс время, пока шла передача.
const MAX_SITE_START_US: i64 = 15_000_000;
/// Сколько ждём плашку «нажми play» после перезагрузки страницы без автозапуска (6 с и запас).
const PRESS_PLAY_BANNER_TIMEOUT: Duration = Duration::from_secs(40);
/// Сколько ждём, пока на перезагруженной странице реклама сменится фильмом.
const AD_PHASE_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём, пока страница сыграет заданные секунды фильма до установки.
const WARMUP_TIMEOUT: Duration = Duration::from_secs(60);
/// Шаг опроса фаз.
const POLL_STEP: Duration = Duration::from_millis(300);
/// Плашка из продукта: на странице без автозапуска после перезагрузки.
const PRESS_PLAY_BANNER: &str = "late_press_play";

/// Когда и как появляется экстеншен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtensionTiming {
    /// Браузер запущен без экстеншена, его ставят при играющей вкладке.
    InstallLate,
    /// Экстеншен (частная копия сборки) стоит до страницы, при играющей вкладке его обновляют новой версией.
    UpdateLate,
}

/// Что страница делает до появления экстеншена.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Warmup {
    /// Играет сама это время.
    Play(Duration),
    /// Человек нажимает Play на странице (автозапуска нет), потом она играет это время.
    PressPlayThenPlay(Duration),
    /// Играет, пока фильм не дойдёт до этой секунды.
    FilmReaches(f64),
}

/// Что делает страница после перезагрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AfterReload {
    /// Играет сама, плеер получает видео.
    Straight,
    /// Не играет: экстеншен подсказывает «нажми play», человек нажимает.
    PressPlay,
    /// Сначала реклама, потом фильм: пока играет реклама, сеанса нет.
    AdBeforeFilm,
}

/// Откуда плеер обязан начать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StartRule {
    /// С запомненной секунды ± допуск.
    Remembered,
    /// Сайт отбросил перемотку: с места, откуда начал сайт (далеко от запомненной секунды).
    SiteStart,
}

/// Чем судится конец строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Judge {
    /// Страница `mse-fmp4`: судья `judge_capture` (кадры, браузер не декодирует под тенью, файл).
    Capture,
    /// Общие улики входа: кадры плеера, посредник, отчёт страницы, журнал плеера.
    Entry,
}

/// Описание строки.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReloadPlan {
    pub(super) page_id: &'static str,
    /// Имя `<video>`, чья позиция «запомнена» (id элемента на странице).
    pub(super) main_video: &'static str,
    pub(super) timing: ExtensionTiming,
    pub(super) warmup: Warmup,
    pub(super) after_reload: AfterReload,
    /// Плашка, которую экстеншен показывает на странице при успехе.
    pub(super) banner: Option<&'static str>,
    pub(super) start_rule: StartRule,
    pub(super) judge: Judge,
}

/// Что дальше с прогоном после очередного шага.
enum Flow {
    Continue,
    Stop,
}

/// Прогон строки в свежем окружении.
pub(super) fn run(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    plan: &ReloadPlan,
) -> Result<Verdict, ScenarioError> {
    let run_started_at = Instant::now();
    let build = real_extension_dir(stand, browser);
    if !build.join("manifest.json").is_file() {
        return Err(ScenarioError::RealExtensionNotBuilt { path: build });
    }
    // Обновление: частная копия сборки (настоящую `dist` не трогаем). Установка: сама сборка.
    let (runtime_install, extension_dir) = match plan.timing {
        ExtensionTiming::InstallLate => (RuntimeInstall::ByTheRow, build),
        ExtensionTiming::UpdateLate => {
            let copy = copy_into_run_directory(&build, environment.work_dir())?;
            (RuntimeInstall::BeforeThePage(copy.clone()), copy)
        }
    };
    let tweaks = RunTweaks {
        runtime_install,
        ..RunTweaks::default()
    };
    let mut ready = match open_ready_with(
        stand,
        environment,
        browser,
        plan.page_id,
        ExtensionMode::InstalledLater,
        &tweaks,
        LATE_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    let mut failures = Vec::new();
    let mut notes = Vec::new();

    warm_up(&mut ready, plan, &mut failures)?;
    notes.push(switch_extension(&mut ready, plan.timing, &extension_dir)?);
    if !open_popup_expecting_reload_button(&mut ready, &mut failures)? {
        return abort(ready, failures);
    }
    check_nothing_happened_yet(&ready, "while the popup offers the reload", &mut failures)?;
    // Момент «до входа» для улик сервера: фон качает окно файла после нажатия кнопки.
    let handed_off_at = Instant::now();
    let Some(tab_presses) = press_reload_button(&mut ready, &mut failures)? else {
        return abort(ready, failures);
    };
    notes.push(format!(
        "popup showed the reload offer; Tab x{tab_presses} then Enter"
    ));
    let popup_problem = ready.wait_popup_closed();
    if plan.banner.is_some() || plan.after_reload == AfterReload::PressPlay {
        // Плашки читаются через слой страницы; служебная страница не меняет активную вкладку.
        ready.run.open_service_page()?;
    }
    if let Err(problem) = wait_for_reload(&ready) {
        failures.push(problem);
        return abort(ready, failures);
    }
    let beats = read_beats(&ready)?;
    let remembered_s = beats.last_seconds_of_first_page(plan.main_video);
    notes.push(format!(
        "page reloaded ({} births); remembered {} s",
        beats.origins().len(),
        remembered_s.map_or_else(|| "unknown".to_owned(), |seconds| format!("{seconds:.1}"))
    ));

    if let Flow::Stop = after_reload_phase(&mut ready, plan, &mut failures, &mut notes)? {
        return abort(ready, failures);
    }
    let granted = wait_for_session_granted(&ready.run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    if granted.is_ok() {
        // Страница отсчитывает «браузер не декодирует» от этой команды: пока сеанса нет, браузер играет сам.
        ready.run.tell_page("handed-off");
    }
    // Плашка живёт секунды, а позиция плеера уходит вперёд: сначала плашка (при успехе она уже
    // показана), потом позиция старта, потом кадры.
    if let Some(expected) = plan.banner
        && let Err(problem) = ready.wait_for_banner(expected, BANNER_TIMEOUT)
    {
        failures.push(problem);
    }
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let start_position = read_start_position(&control);
    let frames = ready.wait_frames()?;
    notes.push(judge_start(
        &start_position,
        remembered_s,
        plan.start_rule,
        &mut failures,
    ));
    conclude(
        stand,
        ready,
        plan,
        frames,
        (run_started_at, handed_off_at),
        popup_problem,
        failures,
        notes,
    )
}

/// Страница играет до появления экстеншена. Предусловие проверяется: без игры «позднее» ничего не значит.
fn warm_up(
    ready: &mut ReadyRun<'_, '_>,
    plan: &ReloadPlan,
    failures: &mut Vec<String>,
) -> Result<(), ScenarioError> {
    match plan.warmup {
        Warmup::Play(duration) => std::thread::sleep(duration),
        Warmup::PressPlayThenPlay(duration) => {
            click_page_target(ready, PLAY_BUTTON_TARGET)?;
            std::thread::sleep(duration);
        }
        Warmup::FilmReaches(seconds) => {
            if let Err(problem) = wait_until_played(ready, plan.main_video, seconds, WARMUP_TIMEOUT)
            {
                failures.push(problem);
            }
        }
    }
    check_page_played_before_install(ready, plan.main_video, failures);
    Ok(())
}

/// Появление экстеншена: установка из сборки или обновление копии с поднятой версией.
fn switch_extension(
    ready: &mut ReadyRun<'_, '_>,
    timing: ExtensionTiming,
    extension_dir: &Path,
) -> Result<String, ScenarioError> {
    let source = ExtensionSource::UnpackedDir(extension_dir.to_path_buf());
    match timing {
        ExtensionTiming::InstallLate => {
            ready.run.browser.install_extension_at_runtime(&source)?;
            Ok("extension installed while the tab was playing".to_owned())
        }
        ExtensionTiming::UpdateLate => {
            let version = bump_version(extension_dir)?;
            ready.run.browser.install_extension_at_runtime(&source)?;
            Ok(format!(
                "extension updated to version {version} while the tab was playing"
            ))
        }
    }
}

/// Настоящий левый клик по цели страницы (по свежей калибровке мышью).
fn click_page_target(ready: &ReadyRun<'_, '_>, target: &str) -> Result<(), ScenarioError> {
    let place = ready.locate_page()?;
    let point = place
        .point_in(target, 0.5, 0.5)
        .map_err(ScenarioError::EntryNotActivated)?;
    ready.robot.click_screen(point)
}

/// Что случается на перезагруженной странице до передачи.
fn after_reload_phase(
    ready: &mut ReadyRun<'_, '_>,
    plan: &ReloadPlan,
    failures: &mut Vec<String>,
    notes: &mut Vec<String>,
) -> Result<Flow, ScenarioError> {
    match plan.after_reload {
        AfterReload::Straight => Ok(Flow::Continue),
        AfterReload::PressPlay => {
            if let Err(problem) =
                ready.wait_for_banner(PRESS_PLAY_BANNER, PRESS_PLAY_BANNER_TIMEOUT)
            {
                failures.push(problem);
                return Ok(Flow::Stop);
            }
            if session_started(ready)? {
                failures.push(
                    "a session started although nothing plays: the extension must wait for play"
                        .to_owned(),
                );
            }
            click_page_target(ready, PLAY_BUTTON_TARGET)?;
            notes.push(
                "banner `late_press_play`, no session; then a real click on the page's play"
                    .to_owned(),
            );
            Ok(Flow::Continue)
        }
        AfterReload::AdBeforeFilm => {
            notes.push(wait_through_the_ad(ready, failures)?);
            Ok(Flow::Continue)
        }
    }
}

/// Пока на перезагруженной странице играет реклама, сеанса нет; цикл кончается, когда появился фильм.
fn wait_through_the_ad(
    ready: &ReadyRun<'_, '_>,
    failures: &mut Vec<String>,
) -> Result<String, ScenarioError> {
    let deadline = Instant::now() + AD_PHASE_TIMEOUT;
    let mut ad_seen_playing = false;
    loop {
        let beats = read_beats(ready)?;
        if let Some(beat) = beats.latest_of_reloaded_page() {
            let ad_playing = beat
                .video("ad")
                .is_some_and(|ad| ad.seconds > 0.3 && !ad.paused);
            ad_seen_playing |= ad_playing;
            let film_started = beat.video("film").is_some();
            if session_started(ready)? && !film_started {
                failures.push(format!(
                    "a player session started while the ad was playing (page beat: {beat:?})"
                ));
                break;
            }
            if film_started {
                break;
            }
        }
        if Instant::now() >= deadline {
            failures.push(format!(
                "the film never started after the reload within {AD_PHASE_TIMEOUT:?}"
            ));
            break;
        }
        std::thread::sleep(POLL_STEP);
    }
    if !ad_seen_playing {
        failures.push(
            "the ad was never seen playing after the reload: the ad phase was not exercised"
                .to_owned(),
        );
    }
    Ok("ad played after the reload with no session; the film followed".to_owned())
}

/// Сверяет стартовую позицию плеера с правилом строки; возвращает строку для сводки.
fn judge_start(
    start_position: &Result<i64, crate::player_control::PlayerControlError>,
    remembered_s: Option<f64>,
    rule: StartRule,
    failures: &mut Vec<String>,
) -> String {
    let started_us = match start_position {
        Ok(position) => *position,
        Err(error) => {
            failures.push(format!("cannot read the player position: {error}"));
            return "player start position unknown".to_owned();
        }
    };
    let Some(remembered_s) = remembered_s else {
        failures.push("the page never reported the remembered video position".to_owned());
        return format!("player started at {started_us} us");
    };
    let remembered_us = (remembered_s * 1_000_000.0) as i64;
    if let Some(problem) = start_problem(started_us, remembered_us, rule) {
        failures.push(problem);
    }
    format!("player started at {started_us} us; remembered {remembered_us} us")
}

/// Чистое правило: чем плохо `started_us` относительно запомненной секунды (`None` — хорошо).
fn start_problem(started_us: i64, remembered_us: i64, rule: StartRule) -> Option<String> {
    match rule {
        StartRule::Remembered => {
            ((started_us - remembered_us).abs() > SAME_SECOND_TOLERANCE_US).then(|| {
                format!(
                    "the player started at {started_us} us, the page stood at {remembered_us} us before the reload: must be within {SAME_SECOND_TOLERANCE_US} us"
                )
            })
        }
        StartRule::SiteStart => {
            if started_us + MIN_GAP_FROM_REMEMBERED_US > remembered_us || started_us > MAX_SITE_START_US {
                Some(format!(
                    "the site refuses seeks, so the player must start from the site's own position (at most {MAX_SITE_START_US} us and {MIN_GAP_FROM_REMEMBERED_US} us before the remembered {remembered_us} us), it started at {started_us} us"
                ))
            } else {
                None
            }
        }
    }
}

/// Строка оборвалась: браузер закрывается, провал называет причины и хвост журнала страницы.
fn abort(ready: ReadyRun<'_, '_>, failures: Vec<String>) -> Result<Verdict, ScenarioError> {
    let _ = ready.robot.take_screenshot("aborted");
    let log = format_browser_log(&ready.run.take_page_log());
    ready.run.browser.shutdown()?;
    Ok(Verdict::fail(format!("{}\n{log}", failures.join("; "))))
}

/// Итог строки по выбранному судье.
#[allow(clippy::too_many_arguments)]
fn conclude(
    stand: &Stand,
    ready: ReadyRun<'_, '_>,
    plan: &ReloadPlan,
    frames: Result<FrameEvidence, String>,
    (run_started_at, handed_off_at): (Instant, Instant),
    popup_problem: Option<String>,
    failures: Vec<String>,
    notes: Vec<String>,
) -> Result<Verdict, ScenarioError> {
    match plan.judge {
        Judge::Capture => finish(
            stand,
            ready,
            &frames,
            plain_page(plan.page_id),
            run_started_at,
            handed_off_at,
            popup_problem,
            &[(failures, notes.join("; "))],
        ),
        Judge::Entry => {
            let evidence = collect(ready, frames)?;
            let mut all_failures: Vec<String> = popup_problem.into_iter().collect();
            all_failures.extend(failures);
            all_failures.extend(evidence.failures(plan.page_id));
            let summary = format!(
                "{}; presented_frames={}",
                notes.join("; "),
                evidence.presented_frames()
            );
            Ok(if all_failures.is_empty() {
                Verdict::pass(summary)
            } else {
                with_extra_failures(
                    Verdict::pass(format!("{summary}\n{}", evidence.page_log)),
                    &all_failures,
                )
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000;

    #[test]
    fn the_same_second_rule_accepts_two_seconds_either_way_and_refuses_more() {
        assert_eq!(
            start_problem(14 * SECOND, 14 * SECOND, StartRule::Remembered),
            None
        );
        assert_eq!(
            start_problem(16 * SECOND, 14 * SECOND, StartRule::Remembered),
            None
        );
        assert_eq!(
            start_problem(12 * SECOND, 14 * SECOND, StartRule::Remembered),
            None
        );
        assert!(start_problem(16 * SECOND + 1, 14 * SECOND, StartRule::Remembered).is_some());
        assert!(
            start_problem(0, 14 * SECOND, StartRule::Remembered).is_some(),
            "from the start is the failure"
        );
    }

    #[test]
    fn the_site_start_rule_wants_a_start_far_below_the_remembered_second() {
        assert_eq!(
            start_problem(3 * SECOND, 14 * SECOND, StartRule::SiteStart),
            None
        );
        assert!(
            start_problem(13 * SECOND, 14 * SECOND, StartRule::SiteStart).is_some(),
            "the remembered second means the site did not refuse"
        );
        assert!(
            start_problem(16 * SECOND, 40 * SECOND, StartRule::SiteStart).is_some(),
            "too late to be a start from the beginning"
        );
    }

    #[test]
    fn a_missing_position_or_remembered_second_is_a_failure_not_a_pass() {
        let mut failures = Vec::new();
        let note = judge_start(
            &Err(crate::player_control::PlayerControlError::Spawn(
                "x".to_owned(),
            )),
            Some(5.0),
            StartRule::Remembered,
            &mut failures,
        );
        assert_eq!(failures.len(), 1);
        assert!(note.contains("unknown"));
        let mut failures = Vec::new();
        judge_start(&Ok(5 * SECOND), None, StartRule::Remembered, &mut failures);
        assert_eq!(failures.len(), 1);
        let mut failures = Vec::new();
        judge_start(
            &Ok(5 * SECOND),
            Some(5.0),
            StartRule::Remembered,
            &mut failures,
        );
        assert!(failures.is_empty());
    }
}
