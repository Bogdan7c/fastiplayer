//! Вывод итоговой таблицы. Только форматирование: ничего не запускает и не решает.

use std::time::Duration;

use browser_bridge_bench::scenarios::{Outcome, StepResult};

/// Итоговые счётчики прогона.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub pass: usize,
    pub unsupported: usize,
    pub fail: usize,
}

impl Totals {
    pub fn of(results: &[StepResult]) -> Self {
        let mut totals = Self::default();
        for result in results {
            match result.outcome {
                Outcome::Pass => totals.pass += 1,
                Outcome::Unsupported => totals.unsupported += 1,
                Outcome::Fail => totals.fail += 1,
            }
        }
        totals
    }

    /// Успех: нет ни одного провала (`unsupported` — не провал).
    pub fn is_success(&self) -> bool {
        self.fail == 0
    }
}

fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Pass => "PASS",
        Outcome::Unsupported => "UNSUPPORTED",
        Outcome::Fail => "FAIL",
    }
}

fn seconds(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

/// Таблица результатов с подробностями под строками, у которых они есть.
pub fn format_table(results: &[StepResult], setup: Duration, total: Duration) -> String {
    let rows: Vec<[String; 5]> = results
        .iter()
        .map(|result| {
            [
                result.command.to_owned(),
                format!("{:?}", result.browser).to_lowercase(),
                result.subject.clone(),
                outcome_label(result.outcome).to_owned(),
                seconds(result.elapsed),
            ]
        })
        .collect();
    let header = ["COMMAND", "BROWSER", "SUBJECT", "RESULT", "TIME"];
    let mut widths = header.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: &[String; 5]| {
        cells
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut text = String::new();
    text.push_str(&line(&header.map(str::to_owned)));
    text.push('\n');
    for (row, result) in rows.iter().zip(results) {
        text.push_str(&line(row));
        text.push('\n');
        for detail_line in result.detail.lines().filter(|l| !l.trim().is_empty()) {
            text.push_str(&format!("    {detail_line}\n"));
        }
    }
    let totals = Totals::of(results);
    text.push_str(&format!(
        "\n{} pass, {} unsupported, {} fail\nsetup (media cache + site): {}\ntotal wall time: {}\nRESULT: {}\n",
        totals.pass,
        totals.unsupported,
        totals.fail,
        seconds(setup),
        seconds(total),
        if totals.is_success() { "PASS" } else { "FAIL" },
    ));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_bridge_bench::browsers::BrowserKind;

    fn result(outcome: Outcome, subject: &str, detail: &str) -> StepResult {
        StepResult {
            command: "pages",
            browser: BrowserKind::Firefox,
            subject: subject.to_owned(),
            outcome,
            detail: detail.to_owned(),
            elapsed: Duration::from_millis(1500),
        }
    }

    #[test]
    fn unsupported_does_not_fail_the_run_but_fail_does() {
        let ok = [
            result(Outcome::Pass, "a", ""),
            result(Outcome::Unsupported, "b", ""),
        ];
        assert!(Totals::of(&ok).is_success());
        let bad = [
            result(Outcome::Pass, "a", ""),
            result(Outcome::Fail, "b", "why"),
        ];
        let totals = Totals::of(&bad);
        assert_eq!((totals.pass, totals.fail), (1, 1));
        assert!(!totals.is_success());
    }

    #[test]
    fn table_shows_rows_details_times_and_the_verdict() {
        let results = [
            result(Outcome::Pass, "mse-fmp4", ""),
            result(Outcome::Fail, "mse-abr", "videoHeight stayed 360"),
        ];
        let text = format_table(&results, Duration::from_secs(2), Duration::from_secs(9));
        assert!(text.contains("mse-fmp4"));
        assert!(text.contains("FAIL"));
        assert!(text.contains("    videoHeight stayed 360"));
        assert!(text.contains("1.5s"));
        assert!(text.contains("total wall time: 9.0s"));
        assert!(text.ends_with("RESULT: FAIL\n"));
    }
}
