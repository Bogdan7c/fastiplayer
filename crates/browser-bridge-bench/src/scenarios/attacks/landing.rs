//! Метрики страницы-атаки: «атака долетела» и «цена поглощения в пределах нормы» (09c).
//!
//! Чистые функции над отчётом страницы, проверяются без браузера. Нужны затем, чтобы атака,
//! которая до продукта не дошла (браузер отверг гигантский буфер сам), не прошла как «продукт
//! выдержал», а продукт, который честно ПОГЛОТИЛ шквал, не проваливался из-за отсутствия
//! сообщения о конце сеанса.

use crate::site::PageReport;

/// Что именно страница-атака обязана доказать своими метриками.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::scenarios) enum Landing {
    /// Метрик нет (атака проверяется только по концу сеанса).
    NotMeasured,
    /// Браузер принял хотя бы один байт вредных данных (`giant_bytes_accepted` > 0).
    BytesCrossed,
    /// Браузер принял хотя бы одну операцию шквала (`flood_accepted_operations` > 0).
    OperationsCrossed,
    /// Шквал воркера долетел, а продукт его поглотил: часы шли, главный поток не подвис.
    FloodAbsorbed(FloodBounds),
}

/// Границы поглощения шквала воркера. Это данные замера, а не «магия»: меняются в одном месте.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::scenarios) struct FloodBounds {
    /// Самый длинный провал главного потока страницы за шквал, мс. Замер живого прогона 09c:
    /// 113 000 сообщений/с, провал 4 мс. 250 мс даёт запас на загруженную машину, но остаётся
    /// ниже порога, с которого зритель замечает подвисание страницы (сотни мс).
    pub(super) max_main_thread_gap_ms: f64,
    /// Минимум сообщений в секунду, чтобы считать шквал дошедшим (замер: ~113 000).
    pub(super) min_messages_per_s: f64,
}

/// Границы для `attack-worker-flood`.
pub(in crate::scenarios) const WORKER_FLOOD_BOUNDS: FloodBounds = FloodBounds {
    max_main_thread_gap_ms: 250.0,
    min_messages_per_s: 1_000.0,
};

/// Достаёт метрику; отсутствие метрики — отдельный провал, а не «ноль».
fn metric(report: &PageReport, name: &str) -> Result<f64, String> {
    report
        .metrics
        .get(name)
        .copied()
        .ok_or_else(|| format!("the attack page reported no metric '{name}'"))
}

/// Провалы по метрикам атаки; пусто — атака долетела и цена в пределах.
pub(super) fn landing_failures(landing: Landing, report: &PageReport) -> Vec<String> {
    let mut failures = attack_step_failures(report);
    failures.extend(match landing {
        Landing::NotMeasured => Vec::new(),
        Landing::BytesCrossed => bytes_crossed_failures(report),
        Landing::OperationsCrossed => operations_crossed_failures(report),
        Landing::FloodAbsorbed(bounds) => flood_absorbed_failures(bounds, report),
    });
    failures
}

/// Шаг атаки упал на самой странице: значит, вредных данных могло и не быть.
fn attack_step_failures(report: &PageReport) -> Vec<String> {
    match metric(report, "attack_errors") {
        Ok(errors) if errors > 0.0 => vec![format!(
            "the attack itself failed on the page ({errors:.0} error(s)): it proves nothing"
        )],
        Ok(_) => Vec::new(),
        Err(message) => vec![message],
    }
}

fn operations_crossed_failures(report: &PageReport) -> Vec<String> {
    match metric(report, "flood_accepted_operations") {
        Ok(operations) if operations > 0.0 => Vec::new(),
        Ok(_) => vec!["the flood never landed: the browser accepted 0 operations".to_owned()],
        Err(message) => vec![message],
    }
}

fn bytes_crossed_failures(report: &PageReport) -> Vec<String> {
    match metric(report, "giant_bytes_accepted") {
        Ok(bytes) if bytes > 0.0 => Vec::new(),
        Ok(_) => vec![format!(
            "the attack never landed: the browser accepted 0 bytes (rejected appends: {:.0}); nothing crossed to the extension",
            report
                .metrics
                .get("giant_appends_rejected_by_browser")
                .copied()
                .unwrap_or(0.0)
        )],
        Err(message) => vec![message],
    }
}

fn flood_absorbed_failures(bounds: FloodBounds, report: &PageReport) -> Vec<String> {
    let mut failures = Vec::new();
    match metric(report, "flood_messages_per_s") {
        Ok(rate) if rate >= bounds.min_messages_per_s => {}
        Ok(rate) => failures.push(format!(
            "the flood never landed: {rate:.0} messages/s, expected at least {:.0}",
            bounds.min_messages_per_s
        )),
        Err(message) => failures.push(message),
    }
    match metric(report, "flood_main_thread_worst_gap_ms") {
        Ok(gap) if gap <= bounds.max_main_thread_gap_ms => {}
        Ok(gap) => failures.push(format!(
            "the flood froze the page main thread for {gap:.0} ms, bound {:.0} ms",
            bounds.max_main_thread_gap_ms
        )),
        Err(message) => failures.push(message),
    }
    match metric(report, "flood_clock_advance_s") {
        Ok(advance) if advance > 0.0 => {}
        Ok(advance) => failures.push(format!(
            "the page clock did not advance during the flood ({advance:.2} s): the session hung"
        )),
        Err(message) => failures.push(message),
    }
    failures
}

/// Цена поглощения для отчёта, одной строкой.
pub(super) fn absorbed_cost_line(report: &PageReport) -> String {
    let show = |name: &str| {
        report
            .metrics
            .get(name)
            .map_or_else(|| "-".to_owned(), |value| format!("{value:.1}"))
    };
    format!(
        "{} messages/s, worst main-thread gap {} ms (bound {:.0} ms), clock +{} s",
        show("flood_messages_per_s"),
        show("flood_main_thread_worst_gap_ms"),
        WORKER_FLOOD_BOUNDS.max_main_thread_gap_ms,
        show("flood_clock_advance_s"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{PageStatus, RunId};

    fn raw_report(metrics: &[(&str, f64)]) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "attack".into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: metrics.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect(),
        }
    }

    /// Отчёт страницы, у которой сама атака прошла без ошибок.
    fn report(metrics: &[(&str, f64)]) -> PageReport {
        let mut all = vec![("attack_errors", 0.0)];
        all.extend_from_slice(metrics);
        raw_report(&all)
    }

    #[test]
    fn an_attack_that_failed_on_the_page_proves_nothing() {
        for landing in [Landing::NotMeasured, Landing::BytesCrossed] {
            let broken = raw_report(&[("attack_errors", 1.0), ("giant_bytes_accepted", 1.0)]);
            assert_eq!(landing_failures(landing, &broken).len(), 1, "{landing:?}");
        }
        let silent = raw_report(&[]);
        let failures = landing_failures(Landing::NotMeasured, &silent);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("attack_errors"), "{failures:?}");
    }

    #[test]
    fn a_flood_of_operations_must_be_accepted_by_the_browser() {
        let landed = report(&[("flood_accepted_operations", 87_986.0)]);
        assert!(landing_failures(Landing::OperationsCrossed, &landed).is_empty());
        let refused = report(&[("flood_accepted_operations", 0.0)]);
        assert_eq!(
            landing_failures(Landing::OperationsCrossed, &refused).len(),
            1
        );
        let silent = report(&[]);
        assert_eq!(
            landing_failures(Landing::OperationsCrossed, &silent).len(),
            1
        );
    }

    fn healthy_flood() -> Vec<(&'static str, f64)> {
        vec![
            ("flood_messages_per_s", 113_000.0),
            ("flood_main_thread_worst_gap_ms", 4.0),
            ("flood_clock_advance_s", 4.0),
        ]
    }

    fn flood_with(name: &'static str, value: f64) -> Vec<String> {
        let mut metrics = healthy_flood();
        metrics.retain(|(n, _)| *n != name);
        metrics.push((name, value));
        landing_failures(
            Landing::FloodAbsorbed(WORKER_FLOOD_BOUNDS),
            &report(&metrics),
        )
    }

    #[test]
    fn bytes_that_crossed_pass_and_zero_bytes_fail() {
        let ok = report(&[("giant_bytes_accepted", 140.0 * 1_048_576.0)]);
        assert!(landing_failures(Landing::BytesCrossed, &ok).is_empty());
        let none = report(&[
            ("giant_bytes_accepted", 0.0),
            ("giant_appends_rejected_by_browser", 4.0),
        ]);
        let failures = landing_failures(Landing::BytesCrossed, &none);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(
            failures[0].contains("never landed") && failures[0].contains("rejected appends: 4")
        );
        // Метрики нет вовсе (страница не дошла до отчёта) — тоже провал.
        assert_eq!(
            landing_failures(Landing::BytesCrossed, &report(&[])).len(),
            1
        );
    }

    #[test]
    fn a_healthy_absorbed_flood_passes() {
        let failures = landing_failures(
            Landing::FloodAbsorbed(WORKER_FLOOD_BOUNDS),
            &report(&healthy_flood()),
        );
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_frozen_main_thread_fails() {
        let failures = flood_with("flood_main_thread_worst_gap_ms", 900.0);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("froze"));
    }

    #[test]
    fn a_stopped_clock_fails() {
        let failures = flood_with("flood_clock_advance_s", 0.0);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("clock"));
    }

    #[test]
    fn a_flood_that_never_landed_fails() {
        let failures = flood_with("flood_messages_per_s", 0.0);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("never landed"));
    }

    #[test]
    fn missing_flood_metrics_fail_instead_of_passing() {
        let failures = landing_failures(Landing::FloodAbsorbed(WORKER_FLOOD_BOUNDS), &report(&[]));
        assert_eq!(failures.len(), 3, "{failures:#?}");
    }

    #[test]
    fn the_cost_line_shows_the_measurement() {
        let line = absorbed_cost_line(&report(&healthy_flood()));
        assert!(
            line.contains("113000.0 messages/s") && line.contains("gap 4.0 ms"),
            "{line}"
        );
    }
}
