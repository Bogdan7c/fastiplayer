//! Чистый приговор сценария `series` (сессия 14b): по шагам прогона, позиции после смены озвучки,
//! строкам журнала плеера и отчёту страницы. Браузера и плеера здесь нет — тесты на синтетике.
//!
//! Что доказывается двумя прогонами:
//! - **варианты**: меню адаптера дошли до плеера; выбор в нашем меню переключил озвучку и качество
//!   НА СТРАНИЦЕ (страница открыла новые потоки); после смены озвучки плеер вернулся на ту же
//!   секунду ± 1 с; Next открыл следующую серию в той же озвучке; кадры после каждого шага в рендере;
//! - **сломанный адаптер**: плеер страницы бросает, адаптер упал — видео играет обычным перехватом,
//!   а меню вариантов у плеера нет.

use std::time::Duration;

use super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::outcome::Verdict;
use crate::observe::HostLogEvidence;
use crate::site::{PageReport, SiteError};

/// Допуск «та же секунда» после смены озвучки (решение 280: ± 1 с).
pub(super) const SAME_SECOND_TOLERANCE: Duration = Duration::from_secs(1);

/// Сколько плеер успевает проиграть между постановкой на секунду и чтением позиции стендом: стенд ждёт
/// строку «перенос применён», а плеер пишет её через 2 с после показа (решение 280, `position_carry`), плюс
/// опрос журнала раз в 100 мс и MPRIS — ещё доля секунды. Верхняя граница окна позиции.
pub(super) const POSITION_READ_SLACK: Duration = Duration::from_secs(4);

/// Свидетельства прогона «варианты».
pub(super) struct VariantsEvidence {
    /// Шаги, которые не прошли (каждый со своей причиной).
    pub(super) failed_steps: Vec<String>,
    /// Секунда зрителя, на которой тестовый выбор сменил озвучку (из сценария).
    pub(super) dub_selected_at: Duration,
    /// Позиция плеера сразу после строки «позиция перенесена» для поколения озвучки; `None` — не дошли.
    pub(super) position_after_dub: Option<Duration>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Свидетельства прогона «сломанный адаптер».
pub(super) struct BrokenAdapterEvidence {
    pub(super) failed_steps: Vec<String>,
    /// Строки журнала плеера о меню вариантов: их быть не должно.
    pub(super) variant_menu_lines: Vec<String>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Позиция после смены озвучки: не раньше «секунды выбора − 1 с» и не позже, чем плеер успел
/// бы доиграть. Позиция у нуля значит «страница начала с нуля, а плеер не вернул».
fn position_failures(selected_at: Duration, position: Option<Duration>) -> Vec<String> {
    let Some(position) = position else {
        return vec!["the player position after the dub switch was never read".to_owned()];
    };
    let earliest = selected_at.saturating_sub(SAME_SECOND_TOLERANCE);
    let latest = selected_at + SAME_SECOND_TOLERANCE + POSITION_READ_SLACK;
    if position < earliest || position > latest {
        return vec![format!(
            "after the dub switch the player stood at {:.1} s, expected {:.1}..{:.1} s (the second of the choice)",
            position.as_secs_f64(),
            earliest.as_secs_f64(),
            latest.as_secs_f64()
        )];
    }
    Vec::new()
}

/// Общие провалы: посредник, отчёт страницы, запрещённые строки плеера.
pub(super) fn common_failures(
    page: &str,
    host: &HostLogEvidence,
    report: &Result<PageReport, SiteError>,
    forbidden: &[String],
) -> Vec<String> {
    let mut failures = host_failures(host);
    failures.extend(page_failures(report, page));
    failures.extend(
        unexpected_player_lines(forbidden).map(|line| format!("forbidden player line: {line}")),
    );
    failures
}

pub(super) fn judge_variants(page: &str, evidence: &VariantsEvidence) -> Verdict {
    let mut failures = evidence.failed_steps.clone();
    failures.extend(position_failures(
        evidence.dub_selected_at,
        evidence.position_after_dub,
    ));
    failures.extend(common_failures(
        page,
        &evidence.host,
        &evidence.page_report,
        &evidence.forbidden_player_lines,
    ));
    if failures.is_empty() {
        let position = evidence
            .position_after_dub
            .map_or(0.0, |position| position.as_secs_f64());
        return Verdict::pass(format!(
            "page menus reached the player; dub switched on the page and the player returned to {position:.1} s \
             (chosen at {:.1} s); quality 720p shown; Next opened episode 2 in the same dub; frames grew after each step",
            evidence.dub_selected_at.as_secs_f64()
        ));
    }
    Verdict::fail(format!("{}\n{}", failures.join("\n"), evidence.page_log))
}

pub(super) fn judge_broken_adapter(page: &str, evidence: &BrokenAdapterEvidence) -> Verdict {
    let mut failures = evidence.failed_steps.clone();
    failures.extend(
        evidence
            .variant_menu_lines
            .iter()
            .map(|line| format!("a broken adapter still gave the player a menu: {line}")),
    );
    failures.extend(common_failures(
        page,
        &evidence.host,
        &evidence.page_report,
        &evidence.forbidden_player_lines,
    ));
    if failures.is_empty() {
        return Verdict::pass(
            "the page player threw, the adapter fell back: frames in the player, no variant menu"
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

    const PAGE: &str = "series-variants";

    fn passing_report() -> Result<PageReport, SiteError> {
        Ok(PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: PAGE.to_owned(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: std::collections::BTreeMap::new(),
        })
    }

    fn healthy_host() -> HostLogEvidence {
        HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            ..HostLogEvidence::default()
        }
    }

    fn good_variants() -> VariantsEvidence {
        VariantsEvidence {
            failed_steps: Vec::new(),
            dub_selected_at: Duration::from_secs(6),
            position_after_dub: Some(Duration::from_millis(6_400)),
            host: healthy_host(),
            page_report: passing_report(),
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    #[test]
    fn all_steps_and_the_same_second_pass() {
        let verdict = judge_variants(PAGE, &good_variants());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        assert!(verdict.detail.contains("6.4 s"), "{}", verdict.detail);
    }

    #[test]
    fn a_player_left_at_zero_after_the_dub_switch_fails() {
        // Страница начала озвучку с нуля, плеер не вернул: ровно то, что ловит решение 280.
        let mut evidence = good_variants();
        evidence.position_after_dub = Some(Duration::from_millis(1_200));
        let verdict = judge_variants(PAGE, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("expected 5.0..11.0 s"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_position_far_ahead_and_an_unread_position_fail() {
        let mut evidence = good_variants();
        evidence.position_after_dub = Some(Duration::from_secs(25));
        assert_eq!(judge_variants(PAGE, &evidence).outcome, Outcome::Fail);
        evidence.position_after_dub = None;
        assert!(
            judge_variants(PAGE, &evidence)
                .detail
                .contains("never read")
        );
    }

    #[test]
    fn a_failed_step_or_a_forbidden_line_fails_even_with_a_good_position() {
        let mut evidence = good_variants();
        evidence.failed_steps = vec!["next-episode: no new generation".to_owned()];
        assert_eq!(judge_variants(PAGE, &evidence).outcome, Outcome::Fail);
        let mut evidence = good_variants();
        evidence.forbidden_player_lines = vec!["ERROR something broke".to_owned()];
        assert_eq!(judge_variants(PAGE, &evidence).outcome, Outcome::Fail);
    }

    fn good_broken() -> BrokenAdapterEvidence {
        BrokenAdapterEvidence {
            failed_steps: Vec::new(),
            variant_menu_lines: Vec::new(),
            host: healthy_host(),
            page_report: passing_report(),
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    #[test]
    fn a_broken_adapter_without_a_menu_passes_and_with_a_menu_fails() {
        assert_eq!(
            judge_broken_adapter(PAGE, &good_broken()).outcome,
            Outcome::Pass
        );
        let mut evidence = good_broken();
        evidence.variant_menu_lines = vec!["Page variant menu kind=audio options=2".to_owned()];
        let verdict = judge_broken_adapter(PAGE, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("still gave the player a menu"));
    }
}
