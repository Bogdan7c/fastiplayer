//! Прогон `series-missing-episode` (сессия 15c2, решения 295, 298, 301): у новой озвучки нет
//! текущей серии — страница остаётся, где была, а плеер говорит зрителю почему.
//!
//! Страница `series-variants` с `&dub_episodes=dub-b:2,3`: у Dub B только серии 2 и 3, а смотрим
//! серию 1 (так отстают озвучки Ashdi). Выбор Dub B «в нашем меню» (тестовая переменная плеера):
//! 1. адаптер видит, что у Dub B нет серии 1, и ничего не нажимает — страница не открывает нового
//!    поколения (`generation cause=dub` в журнале страницы нет);
//! 2. экстеншен отвечает плееру `variant_outcome{…, outcome: no_such_episode}` (протокол 1.8);
//! 3. плеер принимает исход как ответ на свой выбор, показывает причину вместо «Audio: Dub B» и
//!    снимает возврат на ту же секунду;
//! 4. видео играет дальше в прежней озвучке — кадры растут.
//!
//! Строки журнала плеера ищутся по ВСЕМУ журналу (курсор листает ожидание кадров).

use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::super::handoff_run::{HANDOFF_MODE_QUERY, HandoffRun};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::judge::common_failures;
use super::{
    AUDIO_MENU_LINE, GROWTH_TIMEOUT, LINE_TIMEOUT, MIN_FRAMES_GAINED, SERIES_PAGE,
    open_and_hand_off, wait_for_lines_anywhere,
};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::observe::{HostLogEvidence, read_host_evidence};
use crate::site::{BrowserLogLine, PageReport, SiteError};

/// Хвост адреса: у Dub B только серии 2 и 3, смотрим серию 1.
pub(super) const MISSING_EPISODE_QUERY: &str = "&dub_episodes=dub-b:2,3";
/// Выбор «в нашем меню»: Dub B (пункт 1 меню озвучек) на 3-й секунде.
pub(super) const MISSING_EPISODE_CHOICE: &str = "audio:1@3000";

/// Строка плеера: исход выбора пришёл и узнан как ответ на выбор зрителя
/// (`app-egui::browser_session::session_media`, сессия 15c2).
const OUTCOME_LINE: &str = "Page variant outcome kind=audio outcome=no_such_episode matched=true";
/// Строка окна: зритель сначала видит «Switching audio on the page…», а не обещание «Audio: …».
const WINDOW_SWITCHING_LINE: &str =
    "Browser window tells the viewer about a page variant: switching_audio";
/// Строка окна «Audio: <подпись>» (обещание смены): при отказе её быть не должно.
const WINDOW_AUDIO_PROMISE_LINE: &str =
    "Browser window tells the viewer about a page variant: audio";
/// Строка окна: зрителю показана причина отказа, а не «Audio: …».
const WINDOW_REFUSAL_LINE: &str =
    "Browser window tells the viewer about a page variant: refused_audio_no_such_episode";
/// Строка плеера: возврат на ту же секунду снят из-за отказа страницы.
const CARRY_REFUSED_LINE: &str =
    "Position carry after a variant switch ended end=PageRefusedSelection";
/// Строка страницы, которой быть не должно: новое поколение из-за смены озвучки.
const DUB_GENERATION_MARK: &str = "generation cause=dub";

/// Свидетельства прогона.
pub(super) struct MissingEpisodeEvidence {
    /// Шаги, которые не прошли (каждый со своей причиной).
    pub(super) failed_steps: Vec<String>,
    /// Строки журнала страницы о новом поколении озвучки: их быть не должно.
    pub(super) dub_generation_lines: Vec<String>,
    /// Строки окна «Audio: …» (обещание смены): при отказе их быть не должно.
    pub(super) audio_promise_lines: Vec<String>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Прогон в уже подготовленном окружении (тестовая переменная выбора задана снаружи).
pub(super) fn drive_missing_episode(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let query = format!("{HANDOFF_MODE_QUERY}{MISSING_EPISODE_QUERY}");
    let mut run = match open_and_hand_off(
        stand,
        environment,
        browser_kind,
        &query,
        &RunTweaks::default(),
    )? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let mut failed_steps: Vec<String> = Vec::new();
    match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        Ok(()) => walk_missing_episode(&mut run, &mut failed_steps),
        Err(reason) => failed_steps.push(format!("session-granted: {reason}")),
    }
    let audio_promise_lines = audio_promise_lines(
        run.player_log
            .snapshot_lines_containing(WINDOW_AUDIO_PROMISE_LINE)?,
    );
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    Ok(judge_missing_episode(
        SERIES_PAGE,
        &MissingEpisodeEvidence {
            failed_steps,
            dub_generation_lines: dub_generation_lines(&page_log),
            audio_promise_lines,
            host: read_host_evidence(&exit.stderr_log)?,
            page_report,
            forbidden_player_lines,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Шаги прогона по порядку; первый провалившийся шаг останавливает прогон.
fn walk_missing_episode(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) {
    let steps: [(&str, &str); 5] = [
        ("audio-menu", AUDIO_MENU_LINE),
        ("window-says-switching", WINDOW_SWITCHING_LINE),
        ("outcome-reached-the-player", OUTCOME_LINE),
        ("window-shows-the-reason", WINDOW_REFUSAL_LINE),
        ("position-carry-dropped", CARRY_REFUSED_LINE),
    ];
    for (step, line) in steps {
        if let Err(error) = wait_for_lines_anywhere(&mut run.player_log, line, 1, LINE_TIMEOUT) {
            failed_steps.push(format!("{step}: {error}"));
            return;
        }
    }
    if let Err(error) = run
        .player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED, GROWTH_TIMEOUT)
    {
        failed_steps.push(format!("frames-after-the-refusal: {error}"));
    }
}

/// Строки окна, где слово — ровно `audio` (обещание «Audio: …»), а не `switching_audio` или
/// `refused_audio_…`: поиск по подстроке их не различает, поэтому сверяем конец строки.
fn audio_promise_lines(candidates: Vec<String>) -> Vec<String> {
    candidates
        .into_iter()
        .filter(|line| line.trim_end().ends_with(WINDOW_AUDIO_PROMISE_LINE))
        .collect()
}

/// Строки страницы о новом поколении озвучки.
fn dub_generation_lines(page_log: &[BrowserLogLine]) -> Vec<String> {
    page_log
        .iter()
        .filter(|entry| entry.line.contains(DUB_GENERATION_MARK))
        .map(|entry| entry.line.clone())
        .collect()
}

/// Чистый приговор: шаги прошли, страница не открывала поколения озвучки, общие провалы пусты.
pub(super) fn judge_missing_episode(page: &str, evidence: &MissingEpisodeEvidence) -> Verdict {
    let mut failures = evidence.failed_steps.clone();
    failures.extend(
        evidence
            .dub_generation_lines
            .iter()
            .map(|line| format!("the page switched the dub although it lacks the episode: {line}")),
    );
    failures.extend(
        evidence
            .audio_promise_lines
            .iter()
            .map(|line| format!("the window promised the dub although the page refused: {line}")),
    );
    failures.extend(common_failures(
        page,
        &evidence.host,
        &evidence.page_report,
        &evidence.forbidden_player_lines,
    ));
    if failures.is_empty() {
        return Verdict::pass(
            "the new dub lacked the episode: the page stayed, the player got no_such_episode, \
             showed the reason, dropped the position carry, frames kept growing"
                .to_owned(),
        );
    }
    Verdict::fail(format!("{}\n{}", failures.join("\n"), evidence.page_log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};

    fn passing_evidence() -> MissingEpisodeEvidence {
        MissingEpisodeEvidence {
            failed_steps: Vec::new(),
            dub_generation_lines: Vec::new(),
            audio_promise_lines: Vec::new(),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                ..HostLogEvidence::default()
            },
            page_report: Ok(PageReport {
                run: RunId::new("r-1").expect("run id"),
                scenario: SERIES_PAGE.to_owned(),
                status: PageStatus::Pass,
                checks: Vec::new(),
                user_agent: String::new(),
                elapsed_ms: 0,
                metrics: std::collections::BTreeMap::new(),
            }),
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    #[test]
    fn a_refusal_with_the_page_staying_put_passes() {
        let verdict = judge_missing_episode(SERIES_PAGE, &passing_evidence());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    }

    #[test]
    fn a_dub_generation_on_the_page_fails_even_if_the_player_showed_the_reason() {
        let mut evidence = passing_evidence();
        evidence.dub_generation_lines =
            vec!["generation cause=dub episode=1 dub=dub-b quality=video-360 from=0".to_owned()];
        let verdict = judge_missing_episode(SERIES_PAGE, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("switched the dub"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_promise_of_the_dub_in_the_window_fails_and_only_the_exact_word_counts() {
        let promise = format!("12:00:01 INFO {WINDOW_AUDIO_PROMISE_LINE}");
        let lines = audio_promise_lines(vec![
            format!("12:00:00 INFO {WINDOW_SWITCHING_LINE}"),
            format!("12:00:02 INFO {WINDOW_REFUSAL_LINE}"),
            promise.clone(),
        ]);
        assert_eq!(lines, vec![promise]);
        let mut evidence = passing_evidence();
        evidence.audio_promise_lines = lines;
        let verdict = judge_missing_episode(SERIES_PAGE, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("promised the dub"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_missing_player_step_fails_with_its_name() {
        let mut evidence = passing_evidence();
        evidence
            .failed_steps
            .push("window-shows-the-reason: 0 of 1 player lines".to_owned());
        let verdict = judge_missing_episode(SERIES_PAGE, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("window-shows-the-reason"));
    }

    #[test]
    fn the_query_and_the_choice_match_the_page_contract() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("site/pages/{SERIES_PAGE}.html")),
        )
        .expect("page file");
        // Страница знает параметр и метку кнопки, по которой адаптер узнаёт отстающую озвучку.
        assert!(page.contains("ctx.params.raw.get('dub_episodes')"));
        assert!(page.contains("element.dataset.dubEpisodes"));
        assert!(page.contains(DUB_GENERATION_MARK.split(' ').next().unwrap_or_default()));
        // Dub B — второй пункт меню озвучек (номер 1), его и выбирает плеер.
        assert!(MISSING_EPISODE_QUERY.contains("dub-b:"));
        assert!(MISSING_EPISODE_CHOICE.starts_with("audio:1@"));
    }
}
