//! Файл итогов дочернего процесса: строки таблицы ребёнка для родителя.
//!
//! Ребёнок печатает свою таблицу в журнал части (для человека), а родителю отдаёт те же
//! строки файлом JSON: текст таблицы разбирать хрупко, а строки нужны родителю целиком —
//! итог, подробности и время.

use std::path::Path;
use std::time::Duration;

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::scenarios::{Outcome, StepResult};
use serde::{Deserialize, Serialize};

/// Версия формата: родитель и ребёнок — один и тот же бинарь, но файл мог остаться от
/// другой сборки в каталоге частей.
const FORMAT_VERSION: u32 = 1;

/// Содержимое файла.
#[derive(Debug, Serialize, Deserialize)]
struct ResultFile {
    format_version: u32,
    rows: Vec<RowRecord>,
}

/// Одна строка таблицы в файле.
#[derive(Debug, Serialize, Deserialize)]
struct RowRecord {
    command: String,
    browser: BrowserRecord,
    subject: String,
    outcome: OutcomeRecord,
    detail: String,
    elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BrowserRecord {
    Chromium,
    Firefox,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutcomeRecord {
    Pass,
    Unsupported,
    Fail,
}

/// Почему файл итогов не прочитан.
#[derive(Debug, thiserror::Error)]
pub enum ResultFileError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not a results file: {source}")]
    Format {
        path: String,
        source: serde_json::Error,
    },
    #[error("{path} has format version {found}, this bench writes {FORMAT_VERSION}")]
    Version { path: String, found: u32 },
}

/// Записывает строки итога ребёнка.
pub fn write_results(path: &Path, results: &[StepResult]) -> Result<(), ResultFileError> {
    let file = ResultFile {
        format_version: FORMAT_VERSION,
        rows: results.iter().map(record_of).collect(),
    };
    let text = serde_json::to_string(&file).map_err(|source| ResultFileError::Format {
        path: path.display().to_string(),
        source,
    })?;
    std::fs::write(path, text).map_err(|source| ResultFileError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// Читает строки итога ребёнка.
pub fn read_results(path: &Path) -> Result<Vec<StepResult>, ResultFileError> {
    let text = std::fs::read_to_string(path).map_err(|source| ResultFileError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let file: ResultFile =
        serde_json::from_str(&text).map_err(|source| ResultFileError::Format {
            path: path.display().to_string(),
            source,
        })?;
    if file.format_version != FORMAT_VERSION {
        return Err(ResultFileError::Version {
            path: path.display().to_string(),
            found: file.format_version,
        });
    }
    Ok(file.rows.into_iter().map(result_of).collect())
}

fn record_of(result: &StepResult) -> RowRecord {
    RowRecord {
        command: result.command.clone(),
        browser: match result.browser {
            BrowserKind::Chromium => BrowserRecord::Chromium,
            BrowserKind::Firefox => BrowserRecord::Firefox,
        },
        subject: result.subject.clone(),
        outcome: match result.outcome {
            Outcome::Pass => OutcomeRecord::Pass,
            Outcome::Unsupported => OutcomeRecord::Unsupported,
            Outcome::Fail => OutcomeRecord::Fail,
        },
        detail: result.detail.clone(),
        elapsed_ms: u64::try_from(result.elapsed.as_millis()).unwrap_or(u64::MAX),
    }
}

fn result_of(record: RowRecord) -> StepResult {
    StepResult {
        command: record.command,
        browser: match record.browser {
            BrowserRecord::Chromium => BrowserKind::Chromium,
            BrowserRecord::Firefox => BrowserKind::Firefox,
        },
        subject: record.subject,
        outcome: match record.outcome {
            OutcomeRecord::Pass => Outcome::Pass,
            OutcomeRecord::Unsupported => Outcome::Unsupported,
            OutcomeRecord::Fail => Outcome::Fail,
        },
        detail: record.detail,
        elapsed: Duration::from_millis(record.elapsed_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(outcome: Outcome, browser: BrowserKind) -> StepResult {
        StepResult {
            command: "series-memory-autoplay".to_owned(),
            browser,
            subject: "series-variants".to_owned(),
            outcome,
            detail: "frames +75\nline two".to_owned(),
            elapsed: Duration::from_millis(12_345),
        }
    }

    #[test]
    fn rows_survive_the_trip_through_the_file() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("01.json");
        let rows = vec![
            row(Outcome::Pass, BrowserKind::Chromium),
            row(Outcome::Unsupported, BrowserKind::Firefox),
            row(Outcome::Fail, BrowserKind::Firefox),
        ];
        write_results(&path, &rows).expect("written");
        assert_eq!(read_results(&path).expect("read"), rows);
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_empty_run() {
        let dir = tempfile::tempdir().expect("dir");
        let error = read_results(&dir.path().join("absent.json")).expect_err("no file");
        assert!(matches!(error, ResultFileError::Read { .. }), "{error}");
    }

    #[test]
    fn garbage_and_other_versions_are_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "COMMAND BROWSER SUBJECT").expect("written");
        assert!(matches!(
            read_results(&path),
            Err(ResultFileError::Format { .. })
        ));
        std::fs::write(&path, r#"{"format_version":99,"rows":[]}"#).expect("written");
        assert!(matches!(
            read_results(&path),
            Err(ResultFileError::Version { found: 99, .. })
        ));
    }
}
