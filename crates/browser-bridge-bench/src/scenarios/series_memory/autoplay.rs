//! Строка `series-memory-autoplay` (16e, пункт 5 файла 16a): «озвучка та же после автоперехода».
//!
//! Серия доиграла, страница САМА ставит следующую (автопереход сайта, не Next нашего окна), причём
//! в озвучке сайта по умолчанию — такой сайт выбор зрителя не помнит. Вернуть выбранную озвучку
//! обязана память выбора экстеншена (14b2), так же, как после Next. Окно при этом то же, серия 2
//! идёт с начала, кадры растут.
//!
//! Два сеанса на одном профиле (Chromium и Firefox):
//! 1. **Контроль** (профиль свежий, памяти нет, зритель ничего не выбирает): после автоперехода за
//!    окно [`QUIET_WINDOW`] озвучку никто не меняет — серия 2 остаётся в Dub A. Без контроля «Dub B
//!    пришёл сам» мог бы оказаться свойством страницы или стенда, а не памяти.
//! 2. **Память**: зритель выбрал Dub B на странице в серии 1 (запись в памяти есть), серия 1
//!    доиграла, страница поставила серию 2 в Dub A, экстеншен сам нажал Dub B. Приговор требует
//!    именно этот порядок строк страницы и то, что нажатие уложилось в то же окно [`QUIET_WINDOW`].
//!
//! Серию 1 к концу ведёт робот: позиция плеера за [`EPISODE_TAIL`] до конца (MPRIS `Seek`), дальше
//! плеер доигрывает сам и тень отдаёт странице `ended` — как в сценарии `episodes`.
//! Приговор — чистая функция в [`super::autoplay_judge`].

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::handoff_run::HandoffRun;
use super::super::outcome::Verdict;
use super::super::series::SERIES_PAGE;
use super::autoplay_judge::{
    AutoplayEvidence, AutoplaySession, EpisodeTwoStart, WindowCount, judge_autoplay,
};
use super::judge::PICKED_DUB;
use super::session::{
    EpisodeAutoplay, GENERATION_LINE_PREFIX, SessionPlan, begin_session, end_session,
    wait_for_grant,
};
use super::{
    PAGE_PICK, QUIET_WINDOW, bench_fingerprint, open_service_page, read_memory_when_written,
    viewer_picks_dub_on_page, wait_for_first_frames, wait_for_frame_growth,
};
use crate::browsers::{BrowserKind, WindowPrivacy};
use crate::environment::BenchEnvironment;
use crate::player_control::PlayerControl;

/// Длина серии: ассет `h264-aac` каталога стенда, его играет `series-variants.html`.
const EPISODE_LENGTH: Duration = Duration::from_secs(30);
/// Сколько секунд серии 1 остаётся после перемотки робота: дальше плеер доигрывает сам.
const EPISODE_TAIL: Duration = Duration::from_secs(8);
/// Сколько ждём конца серии 1 на странице после перемотки (хвост и запас на медленную машину).
const EPISODE_END_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём, пока страница поставит серию 2 после конца серии 1 (она ставит сразу).
const AUTOPLAY_TIMEOUT: Duration = Duration::from_secs(20);
/// Метка журнала страницы: серия 1 доиграла (`playNextEpisodeInDefaultDub`).
const PAGE_EPISODE_ONE_ENDED: &str = "episode-1-ended";
/// Строка плеера о создании окна — та же метка, что у приговора `episodes` (`episodes/judge.rs`).
const WINDOW_CREATED_MARKER: &str = "Browser window created";

/// Всю строку: контроль, затем память, на одном профиле окружения.
pub(super) fn drive_autoplay(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let fingerprint = bench_fingerprint(stand);
    let control_plan = SessionPlan {
        window_privacy: WindowPrivacy::Normal,
        page_pick: None,
        episode_autoplay: EpisodeAutoplay::NextInDefaultDub,
    };
    let control = match walk_session(stand, environment, browser, control_plan, Role::Control)? {
        Ok(session) => session,
        Err(verdict) => return Ok(verdict),
    };
    let memory_plan = SessionPlan {
        page_pick: Some(PAGE_PICK),
        ..control_plan
    };
    let remembered = match walk_session(stand, environment, browser, memory_plan, Role::Memory)? {
        Ok(session) => session,
        Err(verdict) => return Ok(verdict),
    };
    Ok(judge_autoplay(&AutoplayEvidence {
        page: SERIES_PAGE,
        fingerprint,
        control,
        remembered,
        quiet_window: QUIET_WINDOW,
    }))
}

/// Роль сеанса в строке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Памяти нет: смотрим, что озвучку после автоперехода никто не меняет.
    Control,
    /// Зритель выбрал Dub B: ждём, что экстеншен вернёт его серии 2.
    Memory,
}

/// Один сеанс: передача, (выбор зрителя), конец серии 1, автопереход, наблюдение серии 2, закрытие.
/// Внутренний `Err` — готовый вердикт провала открытия сеанса.
fn walk_session(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    plan: SessionPlan,
    role: Role,
) -> Result<Result<AutoplaySession, Verdict>, ScenarioError> {
    let mut run = match begin_session(stand, environment, browser, plan)? {
        Ok(run) => run,
        Err(verdict) => return Ok(Err(verdict)),
    };
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let mut failed_steps = Vec::new();
    let mut observed = Observed::default();
    walk_episodes(&mut run, &control, role, &mut observed, &mut failed_steps);
    // Окна считаем ещё при живом плеере, до закрытия браузера.
    let windows_at_end = count_windows(&mut run, &mut failed_steps);
    let record = end_session(run, environment, &mut failed_steps)?;
    Ok(Ok(AutoplaySession {
        record,
        failed_steps,
        memory_after_pick: observed.memory_after_pick,
        windows_before_end: observed.windows_before_end,
        windows_at_end,
        dub_applied_after: observed.dub_applied_after,
        episode_two_start: observed.episode_two_start,
    }))
}

/// Что сеанс увидел по ходу (заполняется шагами, пустое поле — шаг не дошёл).
#[derive(Default)]
struct Observed {
    memory_after_pick: Option<super::storage::StoredMemory>,
    windows_before_end: Option<WindowCount>,
    dub_applied_after: Option<Duration>,
    episode_two_start: Option<EpisodeTwoStart>,
}

/// Шаги сеанса по порядку; первый провал останавливает дальнейшие (их улики были бы пустыми).
fn walk_episodes(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    role: Role,
    observed: &mut Observed,
    failed_steps: &mut Vec<String>,
) {
    if !wait_for_grant(run, failed_steps) || !wait_for_first_frames(run, failed_steps) {
        return;
    }
    if role == Role::Memory {
        if !open_service_page(run, failed_steps) || !viewer_picks_dub_on_page(run, failed_steps) {
            return;
        }
        observed.memory_after_pick = read_memory_when_written(run, failed_steps);
    }
    observed.windows_before_end = count_windows(run, failed_steps);
    if let Err(reason) = carry_episode_one_to_its_end(run, control) {
        failed_steps.push(reason);
        return;
    }
    let autoplay_line = format!("{GENERATION_LINE_PREFIX}autoplay episode=2");
    if let Err(reason) = run.wait_page_log_line(&autoplay_line, AUTOPLAY_TIMEOUT) {
        failed_steps.push(format!("page-autoplays-episode-2: {reason}"));
        return;
    }
    let autoplayed_at = Instant::now();
    match role {
        Role::Control => watch_episode_two_untouched(run, autoplayed_at, failed_steps),
        Role::Memory => {
            watch_memory_restores_dub(run, control, autoplayed_at, observed, failed_steps)
        }
    }
}

/// Ставит позицию плеера за [`EPISODE_TAIL`] до конца серии 1 и ждёт, что страница увидела конец.
fn carry_episode_one_to_its_end(
    run: &HandoffRun<'_>,
    control: &PlayerControl,
) -> Result<(), String> {
    let position_micros = control
        .position_micros()
        .map_err(|error| format!("read-player-position: {error}"))?;
    let target = EPISODE_LENGTH.saturating_sub(EPISODE_TAIL);
    let target_micros =
        i64::try_from(target.as_micros()).map_err(|error| format!("seek-target: {error}"))?;
    // Плеер уже дальше цели (медленная машина) — перематывать не нужно: до конца и так близко.
    let offset_micros = target_micros.saturating_sub(position_micros);
    if offset_micros > 0 {
        control
            .seek_by(offset_micros)
            .map_err(|error| format!("seek-near-the-end: {error}"))?;
    }
    run.wait_page_log_line(PAGE_EPISODE_ONE_ENDED, EPISODE_END_TIMEOUT)
        .map(|_| ())
        .map_err(|reason| format!("episode-1-ended-on-page: {reason}"))
}

/// Контроль: серия 2 играет, а озвучку за окно тишины никто не меняет (это судит приговор по
/// строкам страницы; здесь — только ожидание и кадры).
fn watch_episode_two_untouched(
    run: &mut HandoffRun<'_>,
    autoplayed_at: Instant,
    failed_steps: &mut Vec<String>,
) {
    skip_player_history(run, failed_steps);
    wait_for_frame_growth(run, "frames-in-episode-2", failed_steps);
    if let Some(left) = QUIET_WINDOW.checked_sub(autoplayed_at.elapsed()) {
        std::thread::sleep(left);
    }
}

/// Память: экстеншен сам нажимает Dub B на серии 2; серия 2 в Dub B идёт с начала, кадры растут.
fn watch_memory_restores_dub(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    autoplayed_at: Instant,
    observed: &mut Observed,
    failed_steps: &mut Vec<String>,
) {
    let restored_line = format!("{GENERATION_LINE_PREFIX}dub episode=2 dub={PICKED_DUB}");
    // Ждём дольше окна тишины: опоздание — тоже провал, но его меряет приговор, а не таймаут.
    if let Err(reason) = run.wait_page_log_line(&restored_line, QUIET_WINDOW * 2) {
        failed_steps.push(format!("extension-restores-dub-on-episode-2: {reason}"));
        return;
    }
    let switched_at = Instant::now();
    observed.dub_applied_after = Some(autoplayed_at.elapsed());
    skip_player_history(run, failed_steps);
    wait_for_frame_growth(run, "frames-in-episode-2-restored-dub", failed_steps);
    match control.position_micros() {
        Ok(position_micros) => {
            observed.episode_two_start = Some(EpisodeTwoStart {
                position_micros,
                since_dub_switch: switched_at.elapsed(),
            });
        }
        Err(error) => failed_steps.push(format!("read-episode-2-position: {error}")),
    }
}

/// Забывает прошлые сводки кадров: рост кадров должен быть ПОСЛЕ события, а не до.
fn skip_player_history(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) {
    if let Err(error) = run.player_log.skip_history() {
        failed_steps.push(format!("skip-player-history: {error}"));
    }
}

/// Сколько раз за жизнь окружения плеер создавал окно браузерного режима.
fn count_windows(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) -> Option<WindowCount> {
    match run
        .player_log
        .snapshot_lines_containing(WINDOW_CREATED_MARKER)
    {
        Ok(lines) => Some(WindowCount(lines.len())),
        Err(error) => {
            failed_steps.push(format!("count-player-windows: {error}"));
            None
        }
    }
}
