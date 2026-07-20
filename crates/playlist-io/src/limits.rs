use std::fmt;

/// Default hard cap одного M3U/M3U8 документа: 8 MiB.
pub const DEFAULT_MAX_M3U_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Default hard cap одной logical line: 64 KiB.
pub const DEFAULT_MAX_M3U_LINE_BYTES: usize = 64 * 1024;
/// Default retained item cap совпадает с canonical queue capacity.
pub const DEFAULT_MAX_M3U_ITEMS: usize = playlist_core::MAX_PLAYLIST_ITEMS;
/// Default retained issue cap не даёт malformed input раздувать diagnostics.
pub const DEFAULT_MAX_M3U_ISSUES: usize = 256;

/// Явные caller-owned budgets одного parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M3uParserLimits {
    /// Максимальный размер входного byte slice.
    max_document_bytes: usize,
    /// Максимальный размер одной строки без line ending.
    max_line_bytes: usize,
    /// Максимальное число возвращаемых generic entries.
    max_items: usize,
    /// Максимальное число материализованных issues.
    max_issues: usize,
}

impl M3uParserLimits {
    /// Создаёт обязательные ненулевые budgets.
    pub const fn new(
        max_document_bytes: usize,
        max_line_bytes: usize,
        max_items: usize,
        max_issues: usize,
    ) -> Result<Self, M3uParserLimitsError> {
        if max_document_bytes == 0 {
            return Err(M3uParserLimitsError::ZeroDocumentBytes);
        }
        if max_line_bytes == 0 {
            return Err(M3uParserLimitsError::ZeroLineBytes);
        }
        if max_items == 0 {
            return Err(M3uParserLimitsError::ZeroItems);
        }
        if max_items > playlist_core::MAX_PLAYLIST_ITEMS {
            return Err(M3uParserLimitsError::ItemLimitExceedsDomainCapacity {
                provided: max_items,
                maximum: playlist_core::MAX_PLAYLIST_ITEMS,
            });
        }
        if max_issues == 0 {
            return Err(M3uParserLimitsError::ZeroIssues);
        }

        Ok(Self {
            max_document_bytes,
            max_line_bytes,
            max_items,
            max_issues,
        })
    }

    /// Возвращает byte cap документа.
    pub const fn max_document_bytes(self) -> usize {
        self.max_document_bytes
    }

    /// Возвращает byte cap строки.
    pub const fn max_line_bytes(self) -> usize {
        self.max_line_bytes
    }

    /// Возвращает retained item cap.
    pub const fn max_items(self) -> usize {
        self.max_items
    }

    /// Возвращает retained issue cap.
    pub const fn max_issues(self) -> usize {
        self.max_issues
    }
}

impl Default for M3uParserLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: DEFAULT_MAX_M3U_DOCUMENT_BYTES,
            max_line_bytes: DEFAULT_MAX_M3U_LINE_BYTES,
            max_items: DEFAULT_MAX_M3U_ITEMS,
            max_issues: DEFAULT_MAX_M3U_ISSUES,
        }
    }
}

/// Ошибка некорректной caller-конфигурации budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M3uParserLimitsError {
    /// Document budget не может быть нулевым.
    ZeroDocumentBytes,
    /// Line budget не может быть нулевым.
    ZeroLineBytes,
    /// Item budget не может быть нулевым.
    ZeroItems,
    /// Issue budget не может быть нулевым.
    ZeroIssues,
    /// Parser не обещает preview больше canonical domain capacity.
    ItemLimitExceedsDomainCapacity {
        /// Caller-provided cap.
        provided: usize,
        /// Максимум playlist domain.
        maximum: usize,
    },
}

impl fmt::Display for M3uParserLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDocumentBytes => formatter.write_str("M3U document budget равен нулю"),
            Self::ZeroLineBytes => formatter.write_str("M3U line budget равен нулю"),
            Self::ZeroItems => formatter.write_str("M3U item budget равен нулю"),
            Self::ZeroIssues => formatter.write_str("M3U issue budget равен нулю"),
            Self::ItemLimitExceedsDomainCapacity { provided, maximum } => write!(
                formatter,
                "M3U item budget {provided} превышает domain capacity {maximum}"
            ),
        }
    }
}

impl std::error::Error for M3uParserLimitsError {}
