use std::fmt;

/// Стандартный hard cap HLS document: 8 MiB.
pub const DEFAULT_MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Стандартный hard cap физической строки: 64 KiB.
pub const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;
/// Стандартный segment cap защищает построение владеющего VOD timeline.
pub const DEFAULT_MAX_SEGMENTS: usize = 50_000;
/// Стандартный variant cap намеренно меньше segment cap.
pub const DEFAULT_MAX_VARIANTS: usize = 4_096;
/// Стандартный cap rendition descriptors.
pub const DEFAULT_MAX_RENDITIONS: usize = 4_096;
/// Стандартный attribute cap для одного tag.
pub const DEFAULT_MAX_ATTRIBUTES_PER_TAG: usize = 128;

/// Parse budgets принадлежат caller-у; скрытой process-global конфигурации нет.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlsParserLimits {
    max_document_bytes: usize,
    max_line_bytes: usize,
    max_segments: usize,
    max_variants: usize,
    max_renditions: usize,
    max_attributes_per_tag: usize,
}

impl HlsParserLimits {
    /// Создаёт validated ненулевые budgets.
    pub const fn new(
        max_document_bytes: usize,
        max_line_bytes: usize,
        max_segments: usize,
        max_variants: usize,
        max_renditions: usize,
        max_attributes_per_tag: usize,
    ) -> Result<Self, HlsParserLimitsError> {
        if max_document_bytes == 0
            || max_line_bytes == 0
            || max_segments == 0
            || max_variants == 0
            || max_renditions == 0
            || max_attributes_per_tag == 0
        {
            return Err(HlsParserLimitsError::ZeroBudget);
        }
        Ok(Self {
            max_document_bytes,
            max_line_bytes,
            max_segments,
            max_variants,
            max_renditions,
            max_attributes_per_tag,
        })
    }

    /// Максимальный размер input в bytes.
    pub const fn max_document_bytes(self) -> usize {
        self.max_document_bytes
    }

    /// Максимальный размер одной строки до line ending.
    pub const fn max_line_bytes(self) -> usize {
        self.max_line_bytes
    }

    /// Максимум сохраняемых media segments.
    pub const fn max_segments(self) -> usize {
        self.max_segments
    }

    /// Максимум сохраняемых variant streams.
    pub const fn max_variants(self) -> usize {
        self.max_variants
    }

    /// Максимум сохраняемых rendition descriptors.
    pub const fn max_renditions(self) -> usize {
        self.max_renditions
    }

    /// Максимум attributes в одном attribute-list.
    pub const fn max_attributes_per_tag(self) -> usize {
        self.max_attributes_per_tag
    }
}

impl Default for HlsParserLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_segments: DEFAULT_MAX_SEGMENTS,
            max_variants: DEFAULT_MAX_VARIANTS,
            max_renditions: DEFAULT_MAX_RENDITIONS,
            max_attributes_per_tag: DEFAULT_MAX_ATTRIBUTES_PER_TAG,
        }
    }
}

/// Недопустимая caller-конфигурация.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsParserLimitsError {
    /// Каждый safety budget должен быть ненулевым.
    ZeroBudget,
}

impl fmt::Display for HlsParserLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HLS parser budget равен нулю")
    }
}

impl std::error::Error for HlsParserLimitsError {}
