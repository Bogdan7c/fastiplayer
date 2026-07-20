//! Проверяемые бюджеты CUE parser-а.

use std::fmt;

/// Production budget на исходный CUE document.
pub const DEFAULT_MAX_CUE_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// Production budget на одну декодированную строку CUE.
pub const DEFAULT_MAX_CUE_LINE_BYTES: usize = 64 * 1024;
/// Production budget на число FILE-секций.
pub const DEFAULT_MAX_CUE_FILES: usize = 1_024;
/// Production budget на retained unknown commands.
pub const DEFAULT_MAX_CUE_UNKNOWN_COMMANDS: usize = 256;
/// Production budget на совокупные metadata/unknown-command bytes.
pub const DEFAULT_MAX_CUE_RETAINED_TEXT_BYTES: usize = 1024 * 1024;

/// Explicit bounded profile одного parse request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CueParserLimits {
    max_document_bytes: usize,
    max_line_bytes: usize,
    max_files: usize,
    max_unknown_commands: usize,
    max_retained_text_bytes: usize,
}

impl CueParserLimits {
    /// Создаёт полностью caller-defined профиль без hidden zero/unbounded значений.
    pub fn new(
        max_document_bytes: usize,
        max_line_bytes: usize,
        max_files: usize,
        max_unknown_commands: usize,
        max_retained_text_bytes: usize,
    ) -> Result<Self, CueParserLimitsError> {
        if max_document_bytes == 0 {
            return Err(CueParserLimitsError::ZeroDocumentBytes);
        }
        if max_line_bytes == 0 {
            return Err(CueParserLimitsError::ZeroLineBytes);
        }
        if max_files == 0 {
            return Err(CueParserLimitsError::ZeroFiles);
        }
        if max_unknown_commands == 0 {
            return Err(CueParserLimitsError::ZeroUnknownCommands);
        }
        if max_retained_text_bytes == 0 {
            return Err(CueParserLimitsError::ZeroRetainedTextBytes);
        }

        Ok(Self {
            max_document_bytes,
            max_line_bytes,
            max_files,
            max_unknown_commands,
            max_retained_text_bytes,
        })
    }

    /// Возвращает byte budget исходного документа.
    pub const fn max_document_bytes(self) -> usize {
        self.max_document_bytes
    }

    /// Возвращает byte budget одной декодированной строки.
    pub const fn max_line_bytes(self) -> usize {
        self.max_line_bytes
    }

    /// Возвращает maximum FILE-секций.
    pub const fn max_files(self) -> usize {
        self.max_files
    }

    /// Возвращает maximum retained unknown commands.
    pub const fn max_unknown_commands(self) -> usize {
        self.max_unknown_commands
    }

    /// Возвращает общий retained text budget.
    pub const fn max_retained_text_bytes(self) -> usize {
        self.max_retained_text_bytes
    }
}

impl Default for CueParserLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: DEFAULT_MAX_CUE_DOCUMENT_BYTES,
            max_line_bytes: DEFAULT_MAX_CUE_LINE_BYTES,
            max_files: DEFAULT_MAX_CUE_FILES,
            max_unknown_commands: DEFAULT_MAX_CUE_UNKNOWN_COMMANDS,
            max_retained_text_bytes: DEFAULT_MAX_CUE_RETAINED_TEXT_BYTES,
        }
    }
}

/// Ошибка конфигурации caller-owned CUE budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueParserLimitsError {
    /// Document byte budget обязан быть ненулевым.
    ZeroDocumentBytes,
    /// Line byte budget обязан быть ненулевым.
    ZeroLineBytes,
    /// FILE budget обязан быть ненулевым.
    ZeroFiles,
    /// Unknown-command budget обязан быть ненулевым.
    ZeroUnknownCommands,
    /// Retained text budget обязан быть ненулевым.
    ZeroRetainedTextBytes,
}

impl fmt::Display for CueParserLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let explanation = match self {
            Self::ZeroDocumentBytes => "лимит CUE document bytes не может быть нулевым",
            Self::ZeroLineBytes => "лимит CUE line bytes не может быть нулевым",
            Self::ZeroFiles => "лимит CUE FILE-секций не может быть нулевым",
            Self::ZeroUnknownCommands => {
                "лимит retained unknown CUE commands не может быть нулевым"
            }
            Self::ZeroRetainedTextBytes => {
                "лимит retained CUE metadata bytes не может быть нулевым"
            }
        };
        formatter.write_str(explanation)
    }
}

impl std::error::Error for CueParserLimitsError {}
