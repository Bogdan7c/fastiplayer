//! Budgets и typed failures bounded topology boundary.

use thiserror::Error;

use crate::YtDlpLocatorParseError;
use crate::rejection_reason::YtDlpRejectionReason;

/// Максимальный stdout одного topology extraction по умолчанию.
pub const DEFAULT_TOPOLOGY_STDOUT_BYTES: usize = 16 * 1024 * 1024;

/// Максимальный stderr одного topology extraction по умолчанию.
pub const DEFAULT_TOPOLOGY_STDERR_BYTES: usize = 256 * 1024;

/// Максимальный размер одной JSON line по умолчанию.
pub const DEFAULT_TOPOLOGY_JSON_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Максимальное число retained topology entries по умолчанию.
pub const DEFAULT_TOPOLOGY_ENTRY_COUNT: usize = 2_000;

/// Максимальная вложенность structural topology по умолчанию.
pub const DEFAULT_TOPOLOGY_DEPTH: usize = 16;

/// Максимальная JSON-вложенность до запуска `serde_json`.
pub const DEFAULT_TOPOLOGY_JSON_DEPTH: usize = 64;

/// S00 bound для extractor-controlled identity strings.
pub const TOPOLOGY_IDENTITY_MAX_UTF8_BYTES: usize = 256;

/// Service-owned bound для одного compact summary text поля.
pub const TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES: usize = 4 * 1024;

/// Service-owned bound для одного exact delegated locator.
pub const TOPOLOGY_LOCATOR_MAX_UTF8_BYTES: usize = 16 * 1024;

/// Все budgets одного topology extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YtDlpTopologyBudgets {
    /// Совокупный stdout child process.
    pub stdout_bytes: usize,
    /// Совокупный stderr child process.
    pub stderr_bytes: usize,
    /// Одна line-delimited JSON запись.
    pub json_line_bytes: usize,
    /// Совокупное число retained child entries.
    pub entry_count: usize,
    /// Structural nesting playlist/multi-video.
    pub topology_depth: usize,
    /// Синтаксическая вложенность JSON object/array.
    pub json_depth: usize,
}

impl Default for YtDlpTopologyBudgets {
    fn default() -> Self {
        Self {
            stdout_bytes: DEFAULT_TOPOLOGY_STDOUT_BYTES,
            stderr_bytes: DEFAULT_TOPOLOGY_STDERR_BYTES,
            json_line_bytes: DEFAULT_TOPOLOGY_JSON_LINE_BYTES,
            entry_count: DEFAULT_TOPOLOGY_ENTRY_COUNT,
            topology_depth: DEFAULT_TOPOLOGY_DEPTH,
            json_depth: DEFAULT_TOPOLOGY_JSON_DEPTH,
        }
    }
}

impl YtDlpTopologyBudgets {
    /// Проверяет budgets до process spawn.
    pub(crate) fn validate(self) -> Result<Self, YtDlpTopologyError> {
        if self.stdout_bytes == 0 {
            return Err(YtDlpTopologyError::InvalidBudgets {
                field: YtDlpTopologyBudgetField::StdoutBytes,
            });
        }
        if self.stderr_bytes == 0 {
            return Err(YtDlpTopologyError::InvalidBudgets {
                field: YtDlpTopologyBudgetField::StderrBytes,
            });
        }
        if self.json_line_bytes == 0 || self.json_line_bytes > self.stdout_bytes {
            return Err(YtDlpTopologyError::InvalidBudgets {
                field: YtDlpTopologyBudgetField::JsonLineBytes,
            });
        }
        if self.entry_count == 0 {
            return Err(YtDlpTopologyError::InvalidBudgets {
                field: YtDlpTopologyBudgetField::EntryCount,
            });
        }
        if self.topology_depth == 0 {
            return Err(YtDlpTopologyError::InvalidBudgets {
                field: YtDlpTopologyBudgetField::TopologyDepth,
            });
        }
        if self.json_depth == 0 {
            return Err(YtDlpTopologyError::InvalidBudgets {
                field: YtDlpTopologyBudgetField::JsonDepth,
            });
        }

        Ok(self)
    }
}

/// Поле невалидного budget intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpTopologyBudgetField {
    /// Совокупный stdout.
    StdoutBytes,
    /// Совокупный stderr.
    StderrBytes,
    /// Одна JSON line.
    JsonLineBytes,
    /// Число entries.
    EntryCount,
    /// Structural topology depth.
    TopologyDepth,
    /// JSON syntax depth.
    JsonDepth,
}

/// Safe validation reason без raw JSON/value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpTopologyInvalidResponseReason {
    /// Нет ни одной app-owned JSON line.
    MissingJsonOutput,
    /// JSON line синтаксически невалидна.
    MalformedJson,
    /// JSON root не object.
    ExpectedObject,
    /// `_type` имеет неподдерживаемое значение.
    UnsupportedResultType,
    /// Обязательное поле отсутствует либо имеет неверный тип.
    MissingRequiredField,
    /// Video не содержит ни direct URL, ни non-empty formats inventory.
    MissingVideoSourceDescription,
    /// `entries` отсутствует либо не array.
    MissingEntries,
    /// Bounded structural identity/locator field превышает лимит.
    FieldBudgetExceeded,
    /// Active-stack identity повторилась.
    DelegationCycle,
}

/// Typed failure topology extraction.
#[derive(Debug, Error)]
pub enum YtDlpTopologyError {
    /// Adapter disabled committed config-ом.
    #[error("yt-dlp adapter disabled in settings")]
    AdapterDisabled,
    /// Cancellation intent был замечен до/во время child process.
    #[error("yt-dlp topology retrieval cancelled")]
    Cancellation,
    /// Child process превысил configured timeout.
    #[error("The waiting time for yt-dlp topology has expired")]
    Timeout,
    /// Программа `yt-dlp` не найдена в `PATH` (запуск вернул `NotFound`).
    ///
    /// Отдельный вариант (UX сессия 09): пользователю нужна подсказка «установите
    /// yt-dlp», а не общий «сбой процесса».
    #[error("yt-dlp program not found for topology")]
    ExecutableNotFound,
    /// OS/process plumbing failure без locator/argv payload.
    #[error("Failed to execute system yt-dlp for topology")]
    ProcessFailure {
        /// Safe OS/process source.
        #[source]
        source: anyhow::Error,
    },
    /// yt-dlp завершился non-zero.
    #[error(
        "yt-dlp extractor rejected topology URL (stderr hidden, {stderr_bytes} bytes, reason: {reason})"
    )]
    ExtractorRejection {
        /// Bounded observed byte count без stderr payload.
        stderr_bytes: usize,
        /// Причина, распознанная по строкам `ERROR:` (сам текст stderr не хранится).
        reason: YtDlpRejectionReason,
    },
    /// Caller передал логически невалидный budget.
    #[error("Incorrect topology budget: {field:?}")]
    InvalidBudgets {
        /// Поле, нарушившее инвариант.
        field: YtDlpTopologyBudgetField,
    },
    /// Совокупный stdout превысил budget.
    #[error("stdout yt-dlp topology exceeded budget")]
    StdoutBudgetExceeded,
    /// Совокупный stderr превысил budget.
    #[error("stderr yt-dlp topology exceeds budget")]
    StderrBudgetExceeded,
    /// Одна JSON line превысила budget.
    #[error("JSON line yt-dlp topology exceeds budget")]
    JsonLineBudgetExceeded,
    /// Число line-delimited либо retained entries превысило budget.
    #[error("The number of yt-dlp topology entries exceeded budget")]
    EntryBudgetExceeded,
    /// Structural topology глубже configured bound.
    #[error("nesting yt-dlp topology exceeded budget")]
    TopologyDepthExceeded,
    /// JSON syntax глубже configured bound.
    #[error("JSON yt-dlp topology exceeds budget")]
    JsonDepthExceeded,
    /// Serializable extractor response нарушает S00/S15 contract.
    #[error("yt-dlp returns incorrect topology response: {reason:?}")]
    InvalidExtractorResponse {
        /// Safe typed reason.
        reason: YtDlpTopologyInvalidResponseReason,
    },
    /// Internal locator parser error intentionally loses raw input.
    #[error("yt-dlp delegation locator fails to pass admission")]
    DelegationLocator {
        /// Secret-safe locator error.
        #[source]
        source: YtDlpLocatorParseError,
    },
}

impl YtDlpTopologyError {
    pub(crate) fn process(source: impl Into<anyhow::Error>) -> Self {
        Self::ProcessFailure {
            source: source.into(),
        }
    }

    pub(crate) const fn invalid(reason: YtDlpTopologyInvalidResponseReason) -> Self {
        Self::InvalidExtractorResponse { reason }
    }
}
