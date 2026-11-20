//! Что дала часть прогона: строки ребёнка или честный провал вместо них.
//!
//! Родитель верит только файлу итогов ребёнка. Каждая беда ребёнка становится строкой FAIL
//! с путём к журналу части, а не пропажей строк: упал до записи итогов, вышел не тем кодом,
//! выдал пусто или не те строки, которых ждал быстрый набор.

use std::path::Path;
use std::time::Duration;

use browser_bridge_bench::scenarios::{Outcome, StepResult};

use super::plan::RunPart;

/// Чем кончился процесс ребёнка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildExit {
    /// Вышел сам с этим кодом: 0 — всё прошло, 1 — есть провал (обычный исход строки FAIL).
    Code(i32),
    /// Убит сигналом (или код недоступен).
    Killed,
    /// Не запустился вовсе.
    NotStarted(String),
}

impl ChildExit {
    /// Нормальный выход стенда: 0 (все PASS) или 1 (есть FAIL).
    fn is_normal(&self) -> bool {
        matches!(self, Self::Code(0 | 1))
    }

    fn describe(&self) -> String {
        match self {
            Self::Code(code) => format!("exit code {code}"),
            Self::Killed => "killed by a signal".to_owned(),
            Self::NotStarted(reason) => format!("not started: {reason}"),
        }
    }
}

/// Отчёт ребёнка, как его увидел родитель.
#[derive(Debug)]
pub struct ChildReport<'a> {
    /// Как кончился процесс.
    pub exit: ChildExit,
    /// Строки из файла итогов или почему их нет.
    pub rows: Result<Vec<StepResult>, String>,
    /// Журнал части (stdout и stderr ребёнка).
    pub log: &'a Path,
    /// Сколько шла часть по часам родителя.
    pub elapsed: Duration,
}

/// Строки части для общей таблицы.
pub fn rows_of_part(part: &RunPart, report: ChildReport<'_>) -> Vec<StepResult> {
    let log_note = format!("part log: {}", report.log.display());
    let mut rows = match report.rows {
        Ok(rows) => rows,
        Err(reason) => {
            return vec![part_failure(
                part,
                format!(
                    "the part left no results ({reason}); {}\n{log_note}",
                    report.exit.describe()
                ),
                report.elapsed,
            )];
        }
    };
    let mut problems: Vec<String> = Vec::new();
    if !report.exit.is_normal() {
        problems.push(format!(
            "the part ended abnormally: {}",
            report.exit.describe()
        ));
    }
    if rows.is_empty() {
        problems.push("the part ran no rows".to_owned());
    }
    if let Some(expected) = part.expected {
        let actual: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.command.as_str(), row.subject.as_str()))
            .collect();
        let wanted: Vec<(&str, &str)> = expected
            .iter()
            .map(|row| (row.command, row.subject))
            .collect();
        if actual != wanted {
            problems.push(format!(
                "the part was expected to run {wanted:?}, it ran {actual:?}"
            ));
        }
    }
    for row in rows.iter_mut().filter(|row| row.outcome == Outcome::Fail) {
        row.detail = append_line(&row.detail, &log_note);
    }
    if !problems.is_empty() {
        rows.push(part_failure(
            part,
            format!("{}\n{log_note}", problems.join("\n")),
            report.elapsed,
        ));
    }
    rows
}

/// Строка провала самой части (не строки сценария).
fn part_failure(part: &RunPart, detail: String, elapsed: Duration) -> StepResult {
    StepResult {
        command: part.command.cli_name().to_owned(),
        browser: part.browser,
        subject: format!("(part) {}", part.label()),
        outcome: Outcome::Fail,
        detail,
        elapsed,
    }
}

fn append_line(text: &str, line: &str) -> String {
    if text.is_empty() {
        line.to_owned()
    } else {
        format!("{text}\n{line}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use browser_bridge_bench::browsers::BrowserKind;

    use super::super::plan::ExpectedRow;
    use super::*;
    use crate::cli::Command;

    const EXPECTED: &[ExpectedRow] = &[ExpectedRow {
        command: "episodes",
        subject: "delayed-next",
    }];

    fn part(expected: Option<&'static [ExpectedRow]>) -> RunPart {
        RunPart {
            command: Command::Episodes,
            browser: BrowserKind::Firefox,
            page: Some("episode-gap"),
            row: None,
            expected,
            start: super::super::plan::StartOrder::Normal,
        }
    }

    fn row(subject: &str, outcome: Outcome) -> StepResult {
        StepResult {
            command: "episodes".to_owned(),
            browser: BrowserKind::Firefox,
            subject: subject.to_owned(),
            outcome,
            detail: String::new(),
            elapsed: Duration::from_secs(9),
        }
    }

    fn report(
        exit: ChildExit,
        rows: Result<Vec<StepResult>, String>,
        log: &Path,
    ) -> ChildReport<'_> {
        ChildReport {
            exit,
            rows,
            log,
            elapsed: Duration::from_secs(10),
        }
    }

    fn log() -> PathBuf {
        PathBuf::from("/work/jobs/007-episodes.log")
    }

    #[test]
    fn a_clean_part_gives_its_rows_unchanged() {
        let log = log();
        let rows = rows_of_part(
            &part(Some(EXPECTED)),
            report(
                ChildExit::Code(0),
                Ok(vec![row("delayed-next", Outcome::Pass)]),
                &log,
            ),
        );
        assert_eq!(rows, vec![row("delayed-next", Outcome::Pass)]);
    }

    #[test]
    fn a_failed_row_points_at_the_part_log() {
        let log = log();
        let rows = rows_of_part(
            &part(Some(EXPECTED)),
            report(
                ChildExit::Code(1),
                Ok(vec![row("delayed-next", Outcome::Fail)]),
                &log,
            ),
        );
        assert_eq!(rows.len(), 1, "a normal FAIL exit adds no part failure");
        assert!(
            rows[0]
                .detail
                .ends_with("part log: /work/jobs/007-episodes.log")
        );
    }

    #[test]
    fn no_results_file_is_a_failure_not_a_silent_gap() {
        let log = log();
        let rows = rows_of_part(
            &part(None),
            report(
                ChildExit::Killed,
                Err("cannot read 007.json".to_owned()),
                &log,
            ),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, Outcome::Fail);
        assert_eq!(rows[0].subject, "(part) episodes firefox episode-gap");
        assert!(rows[0].detail.contains("cannot read 007.json"));
        assert!(rows[0].detail.contains("killed by a signal"));
    }

    #[test]
    fn an_empty_part_fails() {
        let log = log();
        let rows = rows_of_part(
            &part(None),
            report(ChildExit::Code(0), Ok(Vec::new()), &log),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, Outcome::Fail);
        assert!(rows[0].detail.contains("ran no rows"));
    }

    #[test]
    fn rows_other_than_expected_add_a_failure() {
        let log = log();
        let rows = rows_of_part(
            &part(Some(EXPECTED)),
            report(
                ChildExit::Code(0),
                Ok(vec![
                    row("delayed-next", Outcome::Pass),
                    row("delayed-next-hidden", Outcome::Pass),
                ]),
                &log,
            ),
        );
        assert_eq!(
            rows.len(),
            3,
            "both rows stay visible, plus the part failure"
        );
        assert_eq!(rows[2].outcome, Outcome::Fail);
        assert!(rows[2].detail.contains("delayed-next-hidden"));
    }

    #[test]
    fn an_abnormal_exit_after_passing_rows_still_fails() {
        let log = log();
        let rows = rows_of_part(
            &part(None),
            report(
                ChildExit::Code(2),
                Ok(vec![row("delayed-next", Outcome::Pass)]),
                &log,
            ),
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[1].detail.contains("exit code 2"));
    }
}
