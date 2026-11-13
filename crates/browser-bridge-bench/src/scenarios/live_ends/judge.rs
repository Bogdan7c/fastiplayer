//! Чистый приговор `live-ends` (сессия 16d1): по шагам робота, журналу плеера и отчёту страницы.
//! Браузера и плеера здесь нет — проверяется тестами на синтетических данных.
//!
//! Что доказывается (в обеих строках):
//! - журнал плеера: строка `Live timeline became a recording` (шкала эфира стала записью);
//! - кадры плеера ПОСЛЕ этой строки выросли не меньше чем на [`MIN_FRAMES_AFTER_RECORDING`]
//!   и плеер дошёл до конца (`Browser playback ended` после строки перехода);
//! - в журнале нет отказов потока ((`REJECTION_MARKERS` в `journal.rs`)) и запрещённых строк (паника, ERROR);
//! - страница под тенью видела: эфир (`duration === Infinity`), потом конечную длину и `ended`
//!   (метрики отчёта страницы, имена — в `site/lib/live-ends-observer.js`).
//!
//! Строка `hls-live-ends` строже: под тенью ровно ОДНО `durationchange` после отметки «в эфире».
//! Для `mse-live-ends` число изменений длины не фиксируется (длину ставит сама страница, браузер и
//! тень вправе слать одно или два события), требуется хотя бы одно.

use super::journal::{Transition, read_transition, rejection_lines};
use crate::observe::HostLogEvidence;
use crate::scenarios::evidence::{host_failures, page_failures, unexpected_player_lines};
use crate::scenarios::outcome::Verdict;
use crate::site::{PageReport, SiteError};

/// Сколько кадров плеер обязан показать после строки перехода (с запасом на кадры между
/// последней сводкой и строкой: сводки идут раз в 10 с). Остаток буфера после конца эфира — ~4 с
/// по 25 кадров/с, то есть ~100 кадров.
pub(super) const MIN_FRAMES_AFTER_RECORDING: u64 = 60;

/// Какая строка таблицы судится.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LiveEndsRow {
    /// MSE: страница ставит конечную длину и зовёт `endOfStream()`.
    MseLiveEnds,
    /// Родной HLS: `ENDLIST` в плейлисте, длину меняет браузер (под тенью - тень).
    HlsLiveEnds,
}

impl LiveEndsRow {
    /// Идентификатор страницы стенда (он же `scenario` в её отчёте).
    pub(super) const fn page_id(self) -> &'static str {
        match self {
            Self::MseLiveEnds => "mse-live-ends",
            Self::HlsLiveEnds => "hls-live-ends",
        }
    }

    /// Требуется ли РОВНО одно `durationchange` после отметки «в эфире».
    const fn requires_exactly_one_duration_change(self) -> bool {
        matches!(self, Self::HlsLiveEnds)
    }
}

/// Результат одного шага робота.
pub(super) struct StepEvidence {
    pub(super) name: &'static str,
    pub(super) result: Result<(), String>,
}

/// Всё, что собрал прогон.
pub(super) struct LiveEndsEvidence {
    pub(super) row: LiveEndsRow,
    pub(super) steps: Vec<StepEvidence>,
    /// Отобранные строки журнала плеера в порядке записи (см. `journal_needles`).
    pub(super) journal: Vec<String>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Приговор: провалы собираются все сразу, чтобы разбор шёл по одному прогону.
pub(super) fn judge_live_ends(evidence: &LiveEndsEvidence) -> Verdict {
    let mut failures: Vec<String> = evidence
        .steps
        .iter()
        .filter_map(|step| {
            step.result
                .as_ref()
                .err()
                .map(|message| format!("step '{}': {message}", step.name))
        })
        .collect();
    let transition = read_transition(&evidence.journal);
    failures.extend(transition_failures(&transition));
    failures.extend(
        rejection_lines(&evidence.journal)
            .into_iter()
            .map(|line| format!("the player rejected the stream: {line}")),
    );
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, evidence.row.page_id()));
    failures.extend(page_metric_failures(evidence.row, &evidence.page_report));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let summary = format!(
        "{}; {}",
        describe_transition(&transition),
        describe_page_metrics(&evidence.page_report)
    );
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Провалы по журналу плеера: переход, кадры после него, конец записи.
fn transition_failures(transition: &Transition) -> Vec<String> {
    let facts = match transition {
        Transition::Missing => {
            return vec![
                "the player log has no 'Live timeline became a recording' line: the timeline stayed a live stream"
                    .to_owned(),
            ];
        }
        Transition::Seen(facts) => facts,
    };
    let mut failures = Vec::new();
    if facts.frames_gained() < MIN_FRAMES_AFTER_RECORDING {
        failures.push(format!(
            "the player presented only {} frame(s) after the timeline became a recording (before: {}, after: {:?}), need {MIN_FRAMES_AFTER_RECORDING}",
            facts.frames_gained(),
            facts.frames_before,
            facts.frames_after
        ));
    }
    if !facts.reached_the_end {
        failures.push(
            "the player never reached the end of the recording (no 'Browser playback ended' after the transition)"
                .to_owned(),
        );
    }
    failures
}

/// Строка о переходе для итога.
fn describe_transition(transition: &Transition) -> String {
    match transition {
        Transition::Missing => "recording line: missing".to_owned(),
        Transition::Seen(facts) => format!(
            "frames around the recording line: {} -> {:?} (+{}, need {MIN_FRAMES_AFTER_RECORDING}); reached the end: {}",
            facts.frames_before,
            facts.frames_after,
            facts.frames_gained(),
            facts.reached_the_end
        ),
    }
}

/// Метрики страницы, которые судья читает по именам (`site/lib/live-ends-observer.js`).
const METRIC_ON_AIR_INFINITE: &str = "on_air_duration_infinite";
const METRIC_DURATION_CHANGES: &str = "duration_changes_after_on_air";
const METRIC_FINAL_FINITE: &str = "final_duration_finite";
const METRIC_FINAL_DURATION_S: &str = "final_duration_s";
const METRIC_ENDED_SEEN: &str = "ended_seen";

/// Провалы по метрикам страницы. Если отчёта нет, об этом уже сказал `page_failures`.
fn page_metric_failures(row: LiveEndsRow, report: &Result<PageReport, SiteError>) -> Vec<String> {
    let Ok(report) = report else {
        return Vec::new();
    };
    let metric = |name: &str| report.metrics.get(name).copied();
    let flag_failure = |name: &str, meaning: &str| match metric(name) {
        Some(value) if value >= 1.0 => None,
        Some(value) => Some(format!("page metric {name}={value}: {meaning}")),
        None => Some(format!("the page report has no metric '{name}': {meaning}")),
    };
    let mut failures: Vec<String> = [
        (
            METRIC_ON_AIR_INFINITE,
            "the page did not see duration = Infinity while the stream was on air",
        ),
        (
            METRIC_FINAL_FINITE,
            "the page never saw a finite duration after the stream ended",
        ),
        (
            METRIC_ENDED_SEEN,
            "the page never saw 'ended' after the stream ended",
        ),
    ]
    .into_iter()
    .filter_map(|(name, meaning)| flag_failure(name, meaning))
    .collect();
    match metric(METRIC_DURATION_CHANGES) {
        None => failures.push(format!(
            "the page report has no metric '{METRIC_DURATION_CHANGES}'"
        )),
        Some(changes) if row.requires_exactly_one_duration_change() && changes != 1.0 => {
            failures.push(format!(
                "the page saw {changes} durationchange event(s) after the stream was on air, expected exactly 1"
            ));
        }
        Some(changes) if changes < 1.0 => failures.push(format!(
            "the page saw no durationchange after the stream was on air (metric {METRIC_DURATION_CHANGES}={changes})"
        )),
        Some(_) => {}
    }
    failures
}

/// Цифры страницы одной строкой (отсутствующая метрика печатается как `-`).
fn describe_page_metrics(report: &Result<PageReport, SiteError>) -> String {
    let Ok(report) = report else {
        return "page metrics: no report".to_owned();
    };
    let shown: Vec<String> = [
        METRIC_ON_AIR_INFINITE,
        METRIC_DURATION_CHANGES,
        METRIC_FINAL_FINITE,
        METRIC_FINAL_DURATION_S,
        METRIC_ENDED_SEEN,
    ]
    .iter()
    .map(|name| match report.metrics.get(*name) {
        Some(value) => format!("{name}={value:.2}"),
        None => format!("{name}=-"),
    })
    .collect();
    format!("page metrics: {}", shown.join(", "))
}

#[cfg(test)]
mod tests;
