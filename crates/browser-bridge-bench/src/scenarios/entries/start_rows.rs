//! Строки «вход на странице с видео»: кадры в рендере плеера, браузер под тенью не декодирует,
//! попап закрылся сам, стенд `hand_off_tab` не вызывал. И вход посреди проигрывания: плеер
//! стартует с текущей секунды, а не с нуля, и начало потока не потеряно.

use std::time::{Duration, Instant};

use super::super::capture::{CaptureEvidence, CapturePage, judge_capture, server_evidence};
use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    DEFAULT_QUERY, EarlyUnsupported, Opened, ReadyRun, open_ready, with_extra_failures,
};
use super::menu_entry::ClickSpot;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::observe::{FrameEvidence, read_host_evidence};
use crate::player_control::{PlayerControl, PlayerControlError};

/// Сколько страница играет сама после готовности, прежде чем человек входит (T в задании).
const PLAY_BEFORE_ENTRY: Duration = Duration::from_secs(6);
/// Позиция плеера на старте обязана быть не меньше этой доли T: «с текущей секунды», не с нуля.
const MIN_START_POSITION_SHARE: f64 = 0.8;
/// Запас сверху: сколько страница успела сыграть до готовности, пока вход и передача шли.
const MAX_LEAD_AFTER_ENTRY: Duration = Duration::from_secs(14);
/// Перемотка назад к нулю внутри скачанного: позиция сразу после неё не дальше этого от начала.
const MAX_POSITION_AFTER_REWIND: Duration = Duration::from_secs(4);
/// Сколько даём плееру отработать перемотку, прежде чем читать позицию.
const REWIND_SETTLE: Duration = Duration::from_secs(1);
/// На сколько должны вырасти показанные кадры после перемотки к нулю (два интервала сводок).
const MIN_FRAMES_AFTER_REWIND: u64 = 60;
/// Сколько ждём первую ненулевую позицию плеера.
const START_POSITION_TIMEOUT: Duration = Duration::from_secs(5);
/// Шаг опроса позиции.
const START_POSITION_POLL: Duration = Duration::from_millis(50);
/// Сколько ждём рост кадров после перемотки.
const REWIND_FRAMES_TIMEOUT: Duration = Duration::from_secs(45);

/// Куда целится рука на странице с видео: во фрейм у страницы с iframe, иначе в `<video>`.
pub(super) fn spot_of_video(page_id: &str) -> ClickSpot {
    if page_id == "iframe-cross-origin" {
        ClickSpot::center_of("iframe")
    } else {
        ClickSpot::VIDEO
    }
}

/// Подготовка к входу: входам без попапа нужна служебная страница экстеншена для чтения слоя страницы.
pub(super) fn prepare_entry(
    ready: &mut ReadyRun<'_, '_>,
    entry: Entry,
) -> Result<(), ScenarioError> {
    if !entry.opens_popup() {
        ready.run.open_service_page()?;
    }
    Ok(())
}

/// Вход на странице с видео в самом начале.
pub(super) fn run_start(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    entry: Entry,
    page: CapturePage,
) -> Result<Verdict, ScenarioError> {
    let run_started_at = Instant::now();
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        page.id,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    // Момент «до входа»: фон может скачать окно файла за доли секунды после `started`, раньше,
    // чем стенд заметит закрытие попапа.
    prepare_entry(&mut ready, entry)?;
    let handed_off_at = Instant::now();
    ready.enter_at(entry, spot_of_video(page.id))?;
    let popup_problem = ready.wait_entry_started(entry);
    ready.run.tell_page("handed-off");
    let frames = ready.wait_frames()?;
    finish(
        stand,
        ready,
        &frames,
        page,
        run_started_at,
        handed_off_at,
        popup_problem,
        &[],
    )
}

/// Вход посреди проигрывания: страница играет `PLAY_BEFORE_ENTRY`, потом клик.
pub(super) fn run_late_start(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    entry: Entry,
    page: CapturePage,
) -> Result<Verdict, ScenarioError> {
    let run_started_at = Instant::now();
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        page.id,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    std::thread::sleep(PLAY_BEFORE_ENTRY);
    // Момент «до входа»: фон может скачать окно файла за доли секунды после `started`, раньше,
    // чем стенд заметит закрытие попапа.
    let handed_off_at = Instant::now();
    ready.enter(entry)?;
    let popup_problem = ready.wait_popup_closed();
    ready.run.tell_page("handed-off");
    // Позиция плеера сразу после пропуска, ДО ожидания кадров: он стартовал с текущей секунды
    // страницы, а не с нуля. Страница играла PLAY_BEFORE_ENTRY плюс время до готовности, а вход и
    // передача заняли ещё несколько секунд: верхняя граница с запасом отсекает «не туда».
    let mut extra = Vec::new();
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let start_position = read_start_position(&control);
    let frames = ready.wait_frames()?;
    let needed_us = (PLAY_BEFORE_ENTRY.as_micros() as f64 * MIN_START_POSITION_SHARE) as i64;
    let limit_us = (PLAY_BEFORE_ENTRY + MAX_LEAD_AFTER_ENTRY).as_micros() as i64;
    let start_summary = match &start_position {
        Ok(position) if (needed_us..=limit_us).contains(position) => {
            format!("player started at {position} us")
        }
        Ok(position) => {
            extra.push(format!(
                "the player started at {position} us; the page had played {PLAY_BEFORE_ENTRY:?} before the entry, so it must start between {needed_us} and {limit_us} us"
            ));
            format!("player started at {position} us")
        }
        Err(error) => {
            extra.push(format!("cannot read the player position: {error}"));
            "player start position unknown".to_owned()
        }
    };
    let rewind = check_stream_start_is_kept(&mut ready, &control);
    extra.extend(rewind.failures);
    let summary = format!("{start_summary}; {}", rewind.summary);
    finish(
        stand,
        ready,
        &frames,
        page,
        run_started_at,
        handed_off_at,
        popup_problem,
        &[(extra, summary)],
    )
}

/// Первая позиция плеера. Окно начинает отвечать по MPRIS раньше, чем публикует `Position`: полный
/// прогон 10b прочитал ноль при журнале `target_ms=10287`. Ждём первое ненулевое значение; плеер,
/// который и правда стартовал с нуля, покажет доли секунды и не пройдёт нижнюю границу.
pub(super) fn read_start_position(control: &PlayerControl) -> Result<i64, PlayerControlError> {
    control.wait_until_controllable(SESSION_GRANTED_TIMEOUT)?;
    let deadline = Instant::now() + START_POSITION_TIMEOUT;
    loop {
        let position = control.position_micros()?;
        if position > 0 || Instant::now() >= deadline {
            return Ok(position);
        }
        std::thread::sleep(START_POSITION_POLL);
    }
}

/// Итог проверки «начало потока не потеряно».
struct RewindCheck {
    failures: Vec<String>,
    summary: String,
}

/// Начало потока не пропущено: сеанс получил данные с начала видео. Честная улика: перемотка
/// плеера назад к нулю ВНУТРИ уже скачанного (страница новых запросов не делает) даёт позицию
/// у начала и растущие кадры. Если бы сеанс начинался с середины, плеер не нашёл бы кадров перед
/// стартовой точкой и встал бы.
fn check_stream_start_is_kept(
    ready: &mut ReadyRun<'_, '_>,
    control: &PlayerControl,
) -> RewindCheck {
    // Откуда перематываем: позиция сейчас (плеер уже сыграл кадры и ушёл вперёд).
    let start_position_us = match control.position_micros() {
        Ok(position) => position,
        Err(error) => {
            return RewindCheck {
                failures: vec![format!(
                    "cannot read the position before the rewind: {error}"
                )],
                summary: "rewind not tried".to_owned(),
            };
        }
    };
    let mut failures = Vec::new();
    if let Err(error) = ready.run.player_log.skip_history() {
        failures.push(format!("cannot read the player log: {error}"));
    }
    if let Err(error) = control.seek_by(-start_position_us) {
        failures.push(format!("rewind to the start: {error}"));
    }
    std::thread::sleep(REWIND_SETTLE);
    let after = control.position_micros();
    match &after {
        Ok(position) if *position > MAX_POSITION_AFTER_REWIND.as_micros() as i64 => failures.push(
            format!("after the rewind the player stands at {position} us: the start of the stream is not there"),
        ),
        Ok(_) => {}
        Err(error) => failures.push(format!("cannot read the position after the rewind: {error}")),
    }
    let growth = ready
        .run
        .player_log
        .wait_for_frame_growth(MIN_FRAMES_AFTER_REWIND, REWIND_FRAMES_TIMEOUT);
    let growth_summary = match &growth {
        Ok(growth) => format!("frames +{} after the rewind", growth.frames_gained()),
        Err(error) => {
            failures.push(format!(
                "frames did not grow after the rewind to the start: {error}"
            ));
            "no frame growth after the rewind".to_owned()
        }
    };
    RewindCheck {
        failures,
        summary: format!("rewound from {start_position_us} us to {after:?}; {growth_summary}"),
    }
}

/// Общий хвост: отчёт страницы, улики сервера и журналов, вердикт `judge_capture` плюс провалы строки.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish(
    stand: &Stand,
    mut ready: ReadyRun<'_, '_>,
    frames: &Result<FrameEvidence, String>,
    page: CapturePage,
    run_started_at: Instant,
    handed_off_at: Instant,
    popup_problem: Option<String>,
    extras: &[(Vec<String>, String)],
) -> Result<Verdict, ScenarioError> {
    let page_report = ready.run.finish_page();
    let page_log = ready.run.take_page_log();
    let file = server_evidence(stand, page.id, run_started_at, handed_off_at);
    let forbidden = ready
        .run
        .player_log
        .snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    // Снимок до закрытия: при провале он остаётся в сохранённом каталоге прогона.
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    let verdict = judge_capture(&CaptureEvidence {
        page_id: page.id,
        frames: frames.clone(),
        hook: None,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
        file,
    });
    let mut failures: Vec<String> = popup_problem.into_iter().collect();
    let mut notes = vec!["hand_off_tab was not called by the stand".to_owned()];
    for (more, note) in extras {
        failures.extend(more.iter().cloned());
        notes.push(note.clone());
    }
    let verdict = with_extra_failures(verdict, &failures);
    Ok(Verdict {
        detail: format!("{}\n{}", verdict.detail, notes.join("; ")),
        ..verdict
    })
}
