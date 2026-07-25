//! Обязательные бюджеты fragment inspection-а.

use std::fmt;

use super::error::FragmentInspectionLimitKind;

/// Ошибка сборки обязательных лимитов.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentInspectionLimitBuildError {
    /// Поле не было явно задано.
    Missing {
        /// Незаданный лимит.
        kind: FragmentInspectionLimitKind,
    },
    /// Нулевой предел не имеет полезной безопасной семантики.
    Zero {
        /// Нулевой лимит.
        kind: FragmentInspectionLimitKind,
    },
}

impl fmt::Display for FragmentInspectionLimitBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid fragment inspection limits")
    }
}

impl std::error::Error for FragmentInspectionLimitBuildError {}

/// Полный набор обязательных bounded budgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragmentInspectionLimits {
    max_input_bytes: usize,
    max_box_count: usize,
    max_box_depth: usize,
    max_traf_count: usize,
    max_trun_count: usize,
    max_samples: usize,
    max_sample_table_bytes: usize,
    max_box_payload_bytes: usize,
}

impl FragmentInspectionLimits {
    /// Начинает builder без скрытых defaults.
    pub fn builder() -> FragmentInspectionLimitsBuilder {
        FragmentInspectionLimitsBuilder::new()
    }

    /// Возвращает предел входных bytes.
    pub const fn max_input_bytes(&self) -> usize {
        self.max_input_bytes
    }

    /// Возвращает предел общего числа boxes.
    pub const fn max_box_count(&self) -> usize {
        self.max_box_count
    }

    /// Возвращает предел глубины boxes.
    pub const fn max_box_depth(&self) -> usize {
        self.max_box_depth
    }

    /// Возвращает предел `traf`.
    pub const fn max_traf_count(&self) -> usize {
        self.max_traf_count
    }

    /// Возвращает предел `trun`.
    pub const fn max_trun_count(&self) -> usize {
        self.max_trun_count
    }

    /// Возвращает предел samples.
    pub const fn max_samples(&self) -> usize {
        self.max_samples
    }

    /// Возвращает предел owned sample metadata.
    pub const fn max_sample_table_bytes(&self) -> usize {
        self.max_sample_table_bytes
    }

    /// Возвращает предел payload одного box-а.
    pub const fn max_box_payload_bytes(&self) -> usize {
        self.max_box_payload_bytes
    }
}

/// Builder заставляет caller-а осознанно задать каждый budget.
#[derive(Clone, Debug, Default)]
pub struct FragmentInspectionLimitsBuilder {
    max_input_bytes: Option<usize>,
    max_box_count: Option<usize>,
    max_box_depth: Option<usize>,
    max_traf_count: Option<usize>,
    max_trun_count: Option<usize>,
    max_samples: Option<usize>,
    max_sample_table_bytes: Option<usize>,
    max_box_payload_bytes: Option<usize>,
}

impl FragmentInspectionLimitsBuilder {
    /// Создаёт пустой builder.
    pub const fn new() -> Self {
        Self {
            max_input_bytes: None,
            max_box_count: None,
            max_box_depth: None,
            max_traf_count: None,
            max_trun_count: None,
            max_samples: None,
            max_sample_table_bytes: None,
            max_box_payload_bytes: None,
        }
    }

    /// Задаёт предел входных bytes.
    pub fn max_input_bytes(mut self, value: usize) -> Self {
        self.max_input_bytes = Some(value);
        self
    }

    /// Задаёт предел общего числа boxes.
    pub fn max_box_count(mut self, value: usize) -> Self {
        self.max_box_count = Some(value);
        self
    }

    /// Задаёт предел глубины boxes.
    pub fn max_box_depth(mut self, value: usize) -> Self {
        self.max_box_depth = Some(value);
        self
    }

    /// Задаёт предел `traf`.
    pub fn max_traf_count(mut self, value: usize) -> Self {
        self.max_traf_count = Some(value);
        self
    }

    /// Задаёт предел `trun`.
    pub fn max_trun_count(mut self, value: usize) -> Self {
        self.max_trun_count = Some(value);
        self
    }

    /// Задаёт предел samples.
    pub fn max_samples(mut self, value: usize) -> Self {
        self.max_samples = Some(value);
        self
    }

    /// Задаёт предел owned sample metadata.
    pub fn max_sample_table_bytes(mut self, value: usize) -> Self {
        self.max_sample_table_bytes = Some(value);
        self
    }

    /// Задаёт предел payload одного box-а.
    pub fn max_box_payload_bytes(mut self, value: usize) -> Self {
        self.max_box_payload_bytes = Some(value);
        self
    }

    /// Проверяет полноту и ненулевую семантику builder-а.
    pub fn build(self) -> Result<FragmentInspectionLimits, FragmentInspectionLimitBuildError> {
        Ok(FragmentInspectionLimits {
            max_input_bytes: required_nonzero(
                self.max_input_bytes,
                FragmentInspectionLimitKind::InputBytes,
            )?,
            max_box_count: required_nonzero(
                self.max_box_count,
                FragmentInspectionLimitKind::BoxCount,
            )?,
            max_box_depth: required_nonzero(
                self.max_box_depth,
                FragmentInspectionLimitKind::BoxDepth,
            )?,
            max_traf_count: required_nonzero(
                self.max_traf_count,
                FragmentInspectionLimitKind::TrackFragments,
            )?,
            max_trun_count: required_nonzero(
                self.max_trun_count,
                FragmentInspectionLimitKind::TrackRuns,
            )?,
            max_samples: required_nonzero(self.max_samples, FragmentInspectionLimitKind::Samples)?,
            max_sample_table_bytes: required_nonzero(
                self.max_sample_table_bytes,
                FragmentInspectionLimitKind::SampleTableBytes,
            )?,
            max_box_payload_bytes: required_nonzero(
                self.max_box_payload_bytes,
                FragmentInspectionLimitKind::BoxPayloadBytes,
            )?,
        })
    }
}

/// Извлекает обязательный ненулевой budget.
fn required_nonzero(
    value: Option<usize>,
    kind: FragmentInspectionLimitKind,
) -> Result<usize, FragmentInspectionLimitBuildError> {
    match value {
        None => Err(FragmentInspectionLimitBuildError::Missing { kind }),
        Some(0) => Err(FragmentInspectionLimitBuildError::Zero { kind }),
        Some(value) => Ok(value),
    }
}
