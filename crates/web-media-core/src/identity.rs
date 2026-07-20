use std::fmt;

/// Максимальная длина snapshot-local format identity.
///
/// Значение совпадает с S00 bound для неизвестных extractor identities и не
/// позволяет случайно протащить в selection snapshot неограниченную строку.
pub const MAX_CANDIDATE_FORMAT_IDENTITY_UTF8_BYTES: usize = 256;

/// Максимальная длина стабильного semantic key.
///
/// Semantic key не является URL или request material; тем не менее он остаётся
/// bounded и redacted, потому что его окончательное содержимое задаёт service.
pub const MAX_SEMANTIC_IDENTITY_UTF8_BYTES: usize = 512;

/// Поле, для которого строилась opaque identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityField {
    /// Snapshot-local format identity.
    CandidateFormat,
    /// Стабильный semantic key candidate-а.
    SemanticCandidate,
}

/// Ошибка построения bounded identity без echo исходного значения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityBuildError {
    /// Обязательная identity оказалась пустой.
    Empty {
        /// Безопасное имя поля.
        field: IdentityField,
    },
    /// UTF-8 представление превысило named bound.
    TooLong {
        /// Безопасное имя поля.
        field: IdentityField,
        /// Фактическая длина в UTF-8 bytes.
        provided_bytes: usize,
        /// Разрешённая длина в UTF-8 bytes.
        maximum_bytes: usize,
    },
}

impl fmt::Display for IdentityBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "{field:?} identity не может быть пустой"),
            Self::TooLong {
                field,
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "{field:?} identity занимает {provided_bytes} bytes при лимите {maximum_bytes}"
            ),
        }
    }
}

impl std::error::Error for IdentityBuildError {}

/// Макрос создаёт одинаково проверяемые opaque-string newtype-ы.
///
/// Все generated-типы сохраняют exact UTF-8 identity для equality/ordering, но
/// намеренно не показывают её через `Debug`/`Display`. Это не криптографическая
/// защита: explicit accessor существует только для owner-а, которому identity
/// нужна для сопоставления.
macro_rules! opaque_identity {
    (
        $(#[$metadata:meta])*
        $name:ident,
        field = $field:expr,
        max = $maximum:expr
    ) => {
        $(#[$metadata])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Проверяет named byte bound и сохраняет исходное значение без normalization.
            pub fn new(exact_value: impl Into<String>) -> Result<Self, IdentityBuildError> {
                let exact_value = exact_value.into();
                validate_identity(&exact_value, $field, $maximum)?;
                Ok(Self(exact_value))
            }

            /// Возвращает exact identity только явному owner-у сопоставления.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("utf8_bytes", &self.0.len())
                    .finish_non_exhaustive()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("<redacted>")
            }
        }
    };
}

opaque_identity!(
    /// Snapshot-local format identity, полученная из bounded extractor inventory.
    CandidateFormatIdentity,
    field = IdentityField::CandidateFormat,
    max = MAX_CANDIDATE_FORMAT_IDENTITY_UTF8_BYTES
);

/// Process-local identity web-media source без знания locator-а или service-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceIdentity(u64);

impl SourceIdentity {
    /// Создаёт identity из authority-owned монотонного значения.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает opaque numeric value для correlation внутри процесса.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Refresh-stable identity candidate-а внутри одной source lineage.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticIdentity {
    /// Source lineage запрещает случайный cross-source rematch.
    source: SourceIdentity,
    /// Service-neutral bounded semantic key.
    key: String,
}

impl SemanticIdentity {
    /// Проверяет semantic key и связывает его с authority-owned source.
    pub fn new(
        source: SourceIdentity,
        exact_key: impl Into<String>,
    ) -> Result<Self, IdentityBuildError> {
        let exact_key = exact_key.into();
        validate_identity(
            &exact_key,
            IdentityField::SemanticCandidate,
            MAX_SEMANTIC_IDENTITY_UTF8_BYTES,
        )?;
        Ok(Self {
            source,
            key: exact_key,
        })
    }

    /// Возвращает source lineage.
    pub const fn source(&self) -> SourceIdentity {
        self.source
    }

    /// Возвращает exact semantic key только owner-у refresh matching.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Debug for SemanticIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticIdentity")
            .field("source", &self.source)
            .field("utf8_bytes", &self.key.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SemanticIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Номер immutable extraction snapshot для одного source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExtractionGeneration(u64);

impl ExtractionGeneration {
    /// Создаёт generation из authority-owned монотонного значения.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает generation для stale-check.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Exact identity candidate-а только внутри конкретного extraction snapshot.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CandidateIdentity {
    /// Source, которому принадлежит snapshot.
    source: SourceIdentity,
    /// Generation immutable snapshot-а.
    generation: ExtractionGeneration,
    /// Exact format identity внутри snapshot-а.
    format: CandidateFormatIdentity,
}

impl CandidateIdentity {
    /// Собирает snapshot-local identity без переноса service/runtime типов.
    pub const fn new(
        source: SourceIdentity,
        generation: ExtractionGeneration,
        format: CandidateFormatIdentity,
    ) -> Self {
        Self {
            source,
            generation,
            format,
        }
    }

    /// Возвращает source для correlation.
    pub const fn source(&self) -> SourceIdentity {
        self.source
    }

    /// Возвращает extraction generation для stale-check.
    pub const fn generation(&self) -> ExtractionGeneration {
        self.generation
    }

    /// Возвращает exact format identity текущего snapshot-а.
    pub const fn format(&self) -> &CandidateFormatIdentity {
        &self.format
    }
}

impl fmt::Debug for CandidateIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CandidateIdentity")
            .field("source", &self.source)
            .field("generation", &self.generation)
            .field("format", &self.format)
            .finish()
    }
}

/// Проверяет обязательность и named byte bound opaque identity.
fn validate_identity(
    exact_value: &str,
    field: IdentityField,
    maximum_bytes: usize,
) -> Result<(), IdentityBuildError> {
    if exact_value.is_empty() {
        return Err(IdentityBuildError::Empty { field });
    }

    if exact_value.len() > maximum_bytes {
        return Err(IdentityBuildError::TooLong {
            field,
            provided_bytes: exact_value.len(),
            maximum_bytes,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opaque identities сохраняют raw bytes, но diagnostics их не раскрывает.
    #[test]
    fn identity_bounds_and_redaction_are_secret_safe() {
        let exact_identity = "format-with-sensitive-looking-material";
        let identity = CandidateFormatIdentity::new(exact_identity).expect("identity валидна");

        assert_eq!(identity.as_str(), exact_identity);
        assert!(!format!("{identity:?}").contains(exact_identity));
        assert_eq!(identity.to_string(), "<redacted>");

        let overflow = "я".repeat(MAX_CANDIDATE_FORMAT_IDENTITY_UTF8_BYTES / 2 + 1);
        let error =
            CandidateFormatIdentity::new(overflow).expect_err("bound считается в UTF-8 bytes");
        assert_eq!(
            error,
            IdentityBuildError::TooLong {
                field: IdentityField::CandidateFormat,
                provided_bytes: MAX_CANDIDATE_FORMAT_IDENTITY_UTF8_BYTES + 2,
                maximum_bytes: MAX_CANDIDATE_FORMAT_IDENTITY_UTF8_BYTES,
            }
        );
    }

    /// Semantic identity переживает refresh, а exact candidate identity — нет.
    #[test]
    fn semantic_identity_equality_is_independent_from_extraction_generation() {
        let source = SourceIdentity::new(7);
        let semantic_before =
            SemanticIdentity::new(source, "vp9-1080p-opus").expect("semantic key валиден");
        let semantic_after =
            SemanticIdentity::new(source, "vp9-1080p-opus").expect("semantic key валиден");
        let semantic_other_source = SemanticIdentity::new(SourceIdentity::new(8), "vp9-1080p-opus")
            .expect("semantic key валиден");
        let exact_before = CandidateIdentity::new(
            source,
            ExtractionGeneration::new(10),
            CandidateFormatIdentity::new("248+251").expect("format id валиден"),
        );
        let exact_after = CandidateIdentity::new(
            source,
            ExtractionGeneration::new(11),
            CandidateFormatIdentity::new("616+251").expect("format id валиден"),
        );

        assert_eq!(semantic_before, semantic_after);
        assert_ne!(semantic_before, semantic_other_source);
        assert_ne!(exact_before, exact_after);
    }
}
