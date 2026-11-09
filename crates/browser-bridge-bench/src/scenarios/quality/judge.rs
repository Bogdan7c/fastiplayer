//! Чистый приговор `quality` (сессия 13c): по строкам журнала плеера, росту кадров и отчёту страницы.
//!
//! Что доказывается: страница сама переключила качество внутри одного SourceBuffer (360 → 720),
//! и ОКНО плеера показало новое качество (строка `Browser quality shown`), а кадры после смены
//! продолжили расти. Браузера и плеера здесь нет — тесты идут на синтетических данных.

use super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::outcome::Verdict;
use crate::observe::{BrowserQualityLine, HostLogEvidence, QualityBitrate};
use crate::site::{PageReport, SiteError};

/// Ожидаемая ступень качества: подпись и размер кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedTier {
    pub(super) label: &'static str,
    pub(super) width: u64,
    pub(super) height: u64,
}

/// Начальное качество страницы `mse-abr` (`video-360`, 640x360).
pub(super) const TIER_360: ExpectedTier = ExpectedTier {
    label: "360p",
    width: 640,
    height: 360,
};
/// Качество после переключения (`video-720`, 1280x720).
pub(super) const TIER_720: ExpectedTier = ExpectedTier {
    label: "720p",
    width: 1280,
    height: 720,
};

impl ExpectedTier {
    fn matches(self, line: &BrowserQualityLine) -> bool {
        line.label == self.label && line.width == self.width && line.height == self.height
    }
}

/// Всё, что собрал прогон.
pub(super) struct QualityEvidence {
    /// Шаги робота, на которых прогон споткнулся.
    pub(super) failed_steps: Vec<String>,
    /// Все строки «качество показано» по порядку журнала.
    pub(super) quality_lines: Vec<BrowserQualityLine>,
    /// Сколько кадров плеер добавил между двумя сводками ПОСЛЕ смены; `Err` — не выросли.
    pub(super) frames_after_switch: Result<u64, String>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

fn describe(line: &BrowserQualityLine) -> String {
    let bitrate = match line.bitrate {
        QualityBitrate::Known(bps) => bps.to_string(),
        QualityBitrate::Unknown => "unknown".to_owned(),
    };
    format!(
        "{} {}x{} bitrate_bps={bitrate}",
        line.label, line.width, line.height
    )
}

/// Последний известный битрейт среди строк одной подписи. Строк одной подписи бывает две:
/// первая — с первым кадром поколения, когда битрейт ещё неизвестен (поколение приводится к
/// странице на следующем проходе), вторая — когда он стал известен.
fn last_known_bitrate<'lines>(
    lines: impl DoubleEndedIterator<Item = &'lines BrowserQualityLine>,
) -> Option<u64> {
    lines.rev().find_map(|line| match line.bitrate {
        QualityBitrate::Known(bps) => Some(bps),
        QualityBitrate::Unknown => None,
    })
}

/// Последовательность 360p → 720p (720p строго ПОСЛЕ 360p) и битрейты. Возвращает проблемы.
///
/// Битрейт обязан быть известен у обеих подписей (у сегментов MSE он всегда считается), и у
/// 720p он больше: ассеты стенда — 500k и 1500k. Сравниваются последние известные числа
/// каждой подписи до смены и после неё.
fn sequence_failures(lines: &[BrowserQualityLine]) -> Vec<String> {
    let Some(first_360) = lines.iter().position(|line| TIER_360.matches(line)) else {
        return vec!["the window never showed 360p (640x360)".to_owned()];
    };
    let Some(offset_720) = lines[first_360..]
        .iter()
        .position(|line| TIER_720.matches(line))
    else {
        return vec![
            "the window showed 360p but never 720p (1280x720) after the switch".to_owned(),
        ];
    };
    let (before, after) = lines.split_at(first_360 + offset_720);
    let low = last_known_bitrate(before.iter().filter(|line| TIER_360.matches(line)));
    let high = last_known_bitrate(after.iter().filter(|line| TIER_720.matches(line)));
    match (low, high) {
        (Some(low_bps), Some(high_bps)) if high_bps <= low_bps => vec![format!(
            "720p bitrate ({high_bps}) is not larger than 360p bitrate ({low_bps})"
        )],
        (Some(_), Some(_)) => Vec::new(),
        (None, _) => vec!["the window never showed a bitrate for 360p".to_owned()],
        (_, None) => vec!["the window never showed a bitrate for 720p".to_owned()],
    }
}

/// Приговор страницы в одном браузере.
pub(super) fn judge_quality(page: &str, evidence: &QualityEvidence) -> Verdict {
    let mut failures: Vec<String> = evidence.failed_steps.clone();
    failures.extend(sequence_failures(&evidence.quality_lines));
    match &evidence.frames_after_switch {
        Ok(0) => failures.push("player frames did not grow after the quality switch".to_owned()),
        Ok(_) => {}
        Err(reason) => failures.push(format!(
            "player frames did not keep growing after the switch: {reason}"
        )),
    }
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, page));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    if failures.is_empty() {
        let shown: Vec<String> = evidence.quality_lines.iter().map(describe).collect();
        return Verdict::pass(format!(
            "window quality: {}; frames after switch +{}",
            shown.join(" -> "),
            evidence.frames_after_switch.as_ref().copied().unwrap_or(0)
        ));
    }
    Verdict::fail(format!("{}\n{}", failures.join("\n"), evidence.page_log))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};

    fn line(tier: ExpectedTier, bitrate: QualityBitrate) -> BrowserQualityLine {
        BrowserQualityLine {
            label: tier.label.to_owned(),
            width: tier.width,
            height: tier.height,
            bitrate,
        }
    }

    fn good_evidence() -> QualityEvidence {
        QualityEvidence {
            failed_steps: Vec::new(),
            quality_lines: vec![
                line(TIER_360, QualityBitrate::Known(500_000)),
                line(TIER_720, QualityBitrate::Known(1_500_000)),
            ],
            frames_after_switch: Ok(120),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                ..HostLogEvidence::default()
            },
            page_report: Ok(PageReport {
                run: RunId::new("r-1").expect("run id"),
                scenario: "mse-abr".to_owned(),
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

    fn judge(evidence: &QualityEvidence) -> Verdict {
        judge_quality("mse-abr", evidence)
    }

    #[test]
    fn the_window_showing_360p_then_720p_with_growing_frames_passes() {
        let verdict = judge(&good_evidence());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        assert!(verdict.detail.contains("360p 640x360"));
        assert!(verdict.detail.contains("720p 1280x720"));
    }

    #[test]
    fn the_first_unknown_line_of_a_label_is_followed_by_its_known_bitrate() {
        // Так пишет плеер: с первым кадром поколения битрейт ещё неизвестен, через проход — есть.
        let mut evidence = good_evidence();
        evidence.quality_lines = vec![
            line(TIER_360, QualityBitrate::Unknown),
            line(TIER_360, QualityBitrate::Known(500_000)),
            line(TIER_720, QualityBitrate::Unknown),
            line(TIER_720, QualityBitrate::Known(1_500_000)),
        ];
        assert_eq!(judge(&evidence).outcome, Outcome::Pass);
    }

    #[test]
    fn a_label_whose_bitrate_never_became_known_fails() {
        let mut evidence = good_evidence();
        evidence.quality_lines = vec![
            line(TIER_360, QualityBitrate::Unknown),
            line(TIER_720, QualityBitrate::Known(1_500_000)),
        ];
        let verdict = judge(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("bitrate for 360p"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn a_stale_720p_bitrate_that_later_grows_is_judged_by_the_last_number() {
        // Первое число 720p ниже 360p, последнее — выше: решает последнее известное.
        let mut evidence = good_evidence();
        evidence.quality_lines = vec![
            line(TIER_360, QualityBitrate::Known(500_000)),
            line(TIER_720, QualityBitrate::Known(450_000)),
            line(TIER_720, QualityBitrate::Known(1_500_000)),
        ];
        assert_eq!(judge(&evidence).outcome, Outcome::Pass);
    }

    #[test]
    fn only_360p_fails_and_says_why() {
        let mut evidence = good_evidence();
        evidence.quality_lines = vec![line(TIER_360, QualityBitrate::Known(500_000))];
        let verdict = judge(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("never 720p"), "{}", verdict.detail);
    }

    #[test]
    fn no_quality_line_at_all_fails() {
        let mut evidence = good_evidence();
        evidence.quality_lines.clear();
        let verdict = judge(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("never showed 360p"));
    }

    #[test]
    fn high_quality_before_low_is_not_a_switch() {
        let mut evidence = good_evidence();
        evidence.quality_lines.reverse();
        assert_eq!(judge(&evidence).outcome, Outcome::Fail);
    }

    #[test]
    fn a_wrong_picture_size_under_the_right_label_fails() {
        let mut evidence = good_evidence();
        evidence.quality_lines[1].height = 480;
        assert_eq!(judge(&evidence).outcome, Outcome::Fail);
    }

    #[test]
    fn a_720p_bitrate_not_above_360p_fails() {
        let mut evidence = good_evidence();
        evidence.quality_lines[1].bitrate = QualityBitrate::Known(500_000);
        let verdict = judge(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("not larger"));
    }

    #[test]
    fn frames_that_did_not_grow_fail() {
        let mut evidence = good_evidence();
        evidence.frames_after_switch = Err("timeout".to_owned());
        let verdict = judge(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("did not keep growing"));
        evidence.frames_after_switch = Ok(0);
        assert_eq!(judge(&evidence).outcome, Outcome::Fail);
    }

    #[test]
    fn a_failed_robot_step_a_missing_grant_and_forbidden_lines_fail() {
        let mut evidence = good_evidence();
        evidence
            .failed_steps
            .push("switch-quality: no ack".to_owned());
        evidence.host.session_granted = false;
        evidence
            .forbidden_player_lines
            .push("panic in render".to_owned());
        let verdict = judge(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("switch-quality: no ack"));
        assert!(verdict.detail.contains("did not grant the session"));
        assert!(verdict.detail.contains("panic in render"));
    }
}
