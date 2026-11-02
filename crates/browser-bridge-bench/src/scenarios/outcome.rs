//! Результат шага прогона: чистые данные без вывода.

use std::time::Duration;

use crate::browsers::BrowserKind;

/// Итог шага. `Unsupported` — честное «в этом браузере такой формы нет» и НЕ провал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Unsupported,
    Fail,
}

/// Один проверенный пункт: команда × браузер × предмет проверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// Команда: `pages`, `smoke`, `extension-status`.
    pub command: &'static str,
    /// В каком браузере.
    pub browser: BrowserKind,
    /// Что проверяли: идентификатор страницы или короткое имя проверки.
    pub subject: String,
    /// Итог.
    pub outcome: Outcome,
    /// Свидетельства (числа, строки журнала) или причина провала; для чистого успеха может быть пустым.
    pub detail: String,
    /// Сколько заняла проверка.
    pub elapsed: Duration,
}

impl StepResult {
    /// Собирает результат из вердикта и измеренного времени.
    pub(crate) fn new(
        command: &'static str,
        browser: BrowserKind,
        subject: impl Into<String>,
        verdict: Verdict,
        elapsed: Duration,
    ) -> Self {
        Self {
            command,
            browser,
            subject: subject.into(),
            outcome: verdict.outcome,
            detail: verdict.detail,
            elapsed,
        }
    }
}

/// Вердикт без привязки к браузеру и времени: то, что решает логика сценария.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub outcome: Outcome,
    pub detail: String,
}

impl Verdict {
    pub(crate) fn pass(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Pass,
            detail: detail.into(),
        }
    }

    pub(crate) fn unsupported(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Unsupported,
            detail: detail.into(),
        }
    }

    pub(crate) fn fail(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Fail,
            detail: detail.into(),
        }
    }

    /// Вердикт по списку найденных проблем: пусто — успех с `evidence`, иначе провал.
    pub(crate) fn from_failures(failures: &[String], evidence: String) -> Self {
        if failures.is_empty() {
            Self::pass(evidence)
        } else {
            Self::fail(format!("{}\n{evidence}", failures.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_failures_gives_pass_with_evidence() {
        let verdict = Verdict::from_failures(&[], "frames=60".to_owned());
        assert_eq!(verdict, Verdict::pass("frames=60"));
    }

    #[test]
    fn any_failure_gives_fail_listing_every_problem() {
        let problems = vec!["no frames".to_owned(), "no attach".to_owned()];
        let verdict = Verdict::from_failures(&problems, "evidence".to_owned());
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("no frames; no attach"));
        assert!(verdict.detail.contains("evidence"));
    }
}
