//! Чистый приговор `page-subtitles` (сессия 13b2): по строкам журнала плеера, шагам робота и отчёту
//! страницы. Браузера и плеера здесь нет — проверяется тестами на синтетических данных.
//!
//! Что доказывается:
//! - плеер получил ровно ожидаемые дорожки (язык, формат, `selected`): включена та, что должна;
//! - у растущей дорожки (`addTextTrack`) документ был заменён БОЛЬШИМ (больше реплик);
//! - слой субтитров лёг на кадры (`subtitle_overlay_frames > 0` хотя бы в одной сводке);
//! - договор цел: пропуск посредника, отчёт страницы `pass`, в журнале плеера нет запретных строк.

use std::collections::BTreeMap;

use super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::outcome::Verdict;
use crate::observe::{HostLogEvidence, PageSubtitleDocumentLine, PageSubtitleTrackLine};
use crate::site::{PageReport, SiteError};

/// Какой документ обязан дойти до склада плеера (= дорожка в меню плеера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedDocument {
    /// Метка формата в строке `document received` (`web_vtt`, `srt`, `json3`).
    pub(super) format_hint: &'static str,
    /// Показывала ли дорожку сама страница (`page_display="showing"`).
    pub(super) page_showing: bool,
}

/// Дорожка, которую плеер включил (разбирается лениво: строка `ready` есть только у неё).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedSelected {
    /// Тег языка; `None` — язык не важен (у файла без подписи плеер пишет `unknown`).
    pub(super) language: Option<&'static str>,
    /// Метка формата в строке `ready`.
    pub(super) format: &'static str,
}

/// Что ждёт страница целиком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageExpectation {
    /// Документы на складе (порядок и номера не важны).
    pub(super) documents: &'static [ExpectedDocument],
    /// Единственная включённая дорожка.
    pub(super) selected: ExpectedSelected,
    /// Должен ли документ дорожки быть заменён большим (растущая дорожка `addTextTrack`).
    pub(super) growing_track: bool,
}

/// Всё, что собрал прогон одной страницы.
pub(super) struct PageSubtitlesEvidence {
    /// Шаги робота, на которых прогон споткнулся (пусто — все прошли).
    pub(super) failed_steps: Vec<String>,
    /// Все строки «дорожка готова» за сеанс по порядку (замена даёт вторую строку той же дорожки).
    pub(super) ready_lines: Vec<PageSubtitleTrackLine>,
    /// Все строки «склад принял документ».
    pub(super) document_lines: Vec<PageSubtitleDocumentLine>,
    /// Наибольший `subtitle_overlay_frames` среди сводок (`None` — сводок с полем не было).
    pub(super) overlay_frames: Option<u64>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Последняя строка «готова» по каждому номеру дорожки (после замены верна последняя).
fn final_tracks(ready_lines: &[PageSubtitleTrackLine]) -> Vec<&PageSubtitleTrackLine> {
    let mut latest: BTreeMap<u64, &PageSubtitleTrackLine> = BTreeMap::new();
    for line in ready_lines {
        latest.insert(line.track, line);
    }
    latest.into_values().collect()
}

fn describe(line: &PageSubtitleTrackLine) -> String {
    format!(
        "track {} language={} format={} cues={} selected={}",
        line.track, line.language, line.format, line.cues, line.selected
    )
}

/// Последний документ по каждому номеру дорожки (замена не создаёт новую дорожку).
fn final_documents(document_lines: &[PageSubtitleDocumentLine]) -> Vec<&PageSubtitleDocumentLine> {
    let mut latest: BTreeMap<u64, &PageSubtitleDocumentLine> = BTreeMap::new();
    for line in document_lines {
        latest.insert(line.track, line);
    }
    latest.into_values().collect()
}

/// Документы на складе совпадают с ожиданием (каждый ожидаемый находит РАЗНЫЙ настоящий), а
/// включённая дорожка — одна и та самая, что ждали.
fn track_failures(
    expectation: &PageExpectation,
    ready_lines: &[PageSubtitleTrackLine],
    document_lines: &[PageSubtitleDocumentLine],
) -> Vec<String> {
    let documents = final_documents(document_lines);
    let mut failures = Vec::new();
    if documents.len() != expectation.documents.len() {
        failures.push(format!(
            "the player shelved {} page subtitle track(s), expected {}",
            documents.len(),
            expectation.documents.len()
        ));
    }
    let mut unused = documents;
    for expected in expectation.documents {
        let found = unused.iter().position(|line| {
            line.format_hint == expected.format_hint && line.page_showing == expected.page_showing
        });
        match found {
            Some(index) => {
                unused.remove(index);
            }
            None => failures.push(format!(
                "no shelved track has format_hint={} page_showing={}",
                expected.format_hint, expected.page_showing
            )),
        }
    }
    let selected: Vec<&PageSubtitleTrackLine> = final_tracks(ready_lines)
        .into_iter()
        .filter(|line| line.selected)
        .collect();
    let wanted = &expectation.selected;
    let right_one = |line: &&PageSubtitleTrackLine| {
        line.format == wanted.format
            && wanted
                .language
                .is_none_or(|language| language == line.language)
    };
    match selected.as_slice() {
        [] => failures.push("no page track is selected in the player".to_owned()),
        [only] if right_one(only) => {}
        other => failures.push(format!(
            "selected track(s) [{}] do not match language={} format={}",
            other
                .iter()
                .map(|line| describe(line))
                .collect::<Vec<_>>()
                .join("; "),
            wanted.language.unwrap_or("any"),
            wanted.format
        )),
    }
    failures
}

/// У растущей дорожки: документ заменён (`replaced=true`) и реплик в последней версии больше, чем
/// в первой.
fn growth_failures(
    ready_lines: &[PageSubtitleTrackLine],
    document_lines: &[PageSubtitleDocumentLine],
) -> Vec<String> {
    let mut failures = Vec::new();
    if !document_lines.iter().any(|line| line.replaced) {
        failures
            .push("the growing track was never replaced (no 'replaced=true' document)".to_owned());
    }
    let grew = final_tracks(ready_lines).iter().any(|last| {
        ready_lines
            .iter()
            .find(|first| first.track == last.track)
            .is_some_and(|first| last.cues > first.cues)
    });
    if !grew {
        failures.push("no track got more cues after the replacement".to_owned());
    }
    failures
}

/// Приговор страницы в одном браузере.
pub(super) fn judge_page_subtitles(
    page: &str,
    expectation: &PageExpectation,
    evidence: &PageSubtitlesEvidence,
) -> Verdict {
    let mut failures: Vec<String> = evidence.failed_steps.clone();
    failures.extend(track_failures(
        expectation,
        &evidence.ready_lines,
        &evidence.document_lines,
    ));
    if expectation.growing_track {
        failures.extend(growth_failures(
            &evidence.ready_lines,
            &evidence.document_lines,
        ));
    }
    match evidence.overlay_frames {
        Some(frames) if frames > 0 => {}
        Some(_) => failures.push(
            "subtitle_overlay_frames stayed 0: the subtitle layer was on no frame".to_owned(),
        ),
        None => failures.push("no playback summary with subtitle_overlay_frames".to_owned()),
    }
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, page));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    if failures.is_empty() {
        let tracks: Vec<String> = final_tracks(&evidence.ready_lines)
            .iter()
            .map(|line| describe(line))
            .collect();
        return Verdict::pass(format!(
            "shelved={}; {}; subtitle_overlay_frames={}",
            final_documents(&evidence.document_lines).len(),
            tracks.join("; "),
            evidence.overlay_frames.unwrap_or(0)
        ));
    }
    Verdict::fail(format!("{}\n{}", failures.join("\n"), evidence.page_log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};

    const TWO_TRACKS: PageExpectation = PageExpectation {
        documents: &[
            ExpectedDocument {
                format_hint: "web_vtt",
                page_showing: true,
            },
            ExpectedDocument {
                format_hint: "web_vtt",
                page_showing: false,
            },
        ],
        selected: ExpectedSelected {
            language: Some("en"),
            format: "web_vtt",
        },
        growing_track: false,
    };
    const GROWING: PageExpectation = PageExpectation {
        documents: &[ExpectedDocument {
            format_hint: "web_vtt",
            page_showing: true,
        }],
        selected: ExpectedSelected {
            language: Some("en"),
            format: "web_vtt",
        },
        growing_track: true,
    };

    fn ready(track: u64, language: &str, cues: u64, selected: bool) -> PageSubtitleTrackLine {
        PageSubtitleTrackLine {
            track,
            language: language.to_owned(),
            format: "web_vtt".to_owned(),
            cues,
            selected,
        }
    }

    fn document(track: u64, showing: bool, replaced: bool) -> PageSubtitleDocumentLine {
        PageSubtitleDocumentLine {
            track,
            format_hint: "web_vtt".to_owned(),
            page_showing: showing,
            replaced,
        }
    }

    fn page_report(page: &str) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: page.to_owned(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: std::collections::BTreeMap::new(),
        }
    }

    fn good_evidence() -> PageSubtitlesEvidence {
        PageSubtitlesEvidence {
            failed_steps: Vec::new(),
            // Ленивый разбор: `ready` есть только у включённой дорожки.
            ready_lines: vec![ready(2, "en", 150, true)],
            document_lines: vec![document(1, false, false), document(2, true, false)],
            overlay_frames: Some(240),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                ..HostLogEvidence::default()
            },
            page_report: Ok(page_report("subtitles-track")),
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    fn judge(expectation: &PageExpectation, evidence: &PageSubtitlesEvidence) -> Verdict {
        judge_page_subtitles("subtitles-track", expectation, evidence)
    }

    #[test]
    fn the_right_tracks_with_the_overlay_on_frames_pass() {
        let verdict = judge(&TWO_TRACKS, &good_evidence());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        assert!(verdict.detail.contains("subtitle_overlay_frames=240"));
    }

    #[test]
    fn the_wrong_track_selected_fails_with_the_reason() {
        let mut evidence = good_evidence();
        evidence.ready_lines = vec![ready(1, "uk", 150, true)];
        let verdict = judge(&TWO_TRACKS, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("do not match language=en format=web_vtt")
        );
    }

    #[test]
    fn no_selected_track_fails() {
        let mut evidence = good_evidence();
        evidence.ready_lines.clear();
        let verdict = judge(&TWO_TRACKS, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("no page track is selected"));
    }

    #[test]
    fn a_missing_shelved_track_fails() {
        let mut evidence = good_evidence();
        evidence.document_lines = vec![document(2, true, false)];
        let verdict = judge(&TWO_TRACKS, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("shelved 1 page subtitle track(s), expected 2")
        );
    }

    #[test]
    fn a_track_the_page_showed_but_should_not_have_fails() {
        let mut evidence = good_evidence();
        evidence.document_lines = vec![document(1, true, false), document(2, true, false)];
        let verdict = judge(&TWO_TRACKS, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("page_showing=false"));
    }

    #[test]
    fn nothing_shelved_at_all_fails() {
        let mut evidence = good_evidence();
        evidence.ready_lines.clear();
        evidence.document_lines.clear();
        assert_eq!(judge(&TWO_TRACKS, &evidence).outcome, Outcome::Fail);
    }

    #[test]
    fn zero_overlay_frames_fail() {
        let mut evidence = good_evidence();
        evidence.overlay_frames = Some(0);
        let verdict = judge(&TWO_TRACKS, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("subtitle layer was on no frame"));
        evidence.overlay_frames = None;
        assert!(
            judge(&TWO_TRACKS, &evidence)
                .detail
                .contains("no playback summary")
        );
    }

    #[test]
    fn a_failed_robot_step_or_a_forbidden_line_fails() {
        let mut evidence = good_evidence();
        evidence.failed_steps = vec!["player-presents-frames: no frames".to_owned()];
        assert_eq!(judge(&TWO_TRACKS, &evidence).outcome, Outcome::Fail);
        let mut evidence = good_evidence();
        evidence.forbidden_player_lines = vec!["ERROR boom".to_owned()];
        assert!(
            judge(&TWO_TRACKS, &evidence)
                .detail
                .contains("forbidden player line: ERROR boom")
        );
    }

    #[test]
    fn a_replaced_track_counts_by_its_last_lines() {
        let mut evidence = good_evidence();
        evidence.ready_lines = vec![ready(1, "en", 20, true), ready(1, "en", 150, true)];
        evidence.document_lines = vec![document(1, true, false), document(1, true, true)];
        let verdict = judge(&GROWING, &evidence);
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    }

    #[test]
    fn a_growing_track_that_never_grew_fails() {
        let mut evidence = good_evidence();
        evidence.ready_lines = vec![ready(1, "en", 20, true)];
        evidence.document_lines = vec![document(1, true, false)];
        let verdict = judge(&GROWING, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("never replaced"));
        assert!(verdict.detail.contains("no track got more cues"));
    }

    #[test]
    fn the_language_is_ignored_when_the_page_does_not_name_one() {
        let expectation = PageExpectation {
            documents: &[ExpectedDocument {
                format_hint: "web_vtt",
                page_showing: true,
            }],
            selected: ExpectedSelected {
                language: None,
                format: "web_vtt",
            },
            growing_track: false,
        };
        let mut evidence = good_evidence();
        evidence.ready_lines = vec![ready(1, "unknown", 150, true)];
        evidence.document_lines = vec![document(1, true, false)];
        assert_eq!(judge(&expectation, &evidence).outcome, Outcome::Pass);
    }
}
