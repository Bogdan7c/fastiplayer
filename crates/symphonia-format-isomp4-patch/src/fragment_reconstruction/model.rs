//! Crate-private intent types и normalized sample plan.

use std::fmt;
use std::num::NonZeroU32;
use std::ops::Range;

use super::error::{FragmentArithmeticOperation, FragmentInspectionError};
use super::limits::FragmentInspectionLimits;

/// Авторитетный ISO BMFF track ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentTrackId(NonZeroU32);

impl FragmentTrackId {
    /// Создаёт непустой track ID.
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    /// Возвращает ISO track ID.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Авторитетное начало decode timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentBaseDecodeTime(u64);

impl FragmentBaseDecodeTime {
    /// Создаёт точное время в media timescale track-а.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает ticks.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Семантика битового поля `sample_composition_time_offset` во входном fragment-е.
///
/// ISO BMFF связывает signedness с версией `trun`, тогда как legacy PIFF/Smooth
/// хранит signed 32-bit offsets в `trun` version 0. Политика задаётся вызывающим
/// boundary явно, поэтому общий MP4 parser не получает Smooth-specific эвристику.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentCompositionOffsetSemantics {
    /// Стандартная ISO BMFF семантика: version 0 — `u32`, version 1 — `i32`.
    IsoBmffVersioned,
    /// Legacy PIFF/Smooth семантика: 32 бита всегда интерпретируются как `i32`.
    PiffSigned32Bit,
}

/// Явная RAP policy без позиционного `bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FragmentRapRequirement {
    /// Первый video sample обязан иметь proven ISO RAP flags.
    RequireProvenVideoRandomAccess,
    /// Для audio RAP evidence не требуется.
    NotRequiredForAudio,
}

/// Defaults из авторитетного init `trex`, если они действительно заданы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentSampleDefaults {
    sample_duration: Option<NonZeroU32>,
    sample_size: Option<NonZeroU32>,
    sample_flags: Option<u32>,
}

impl FragmentSampleDefaults {
    /// Создаёт отсутствие defaults без неявного нуля.
    pub const fn absent() -> Self {
        Self {
            sample_duration: None,
            sample_size: None,
            sample_flags: None,
        }
    }

    /// Добавляет default sample duration.
    pub const fn with_sample_duration(mut self, value: NonZeroU32) -> Self {
        self.sample_duration = Some(value);
        self
    }

    /// Добавляет default sample size.
    pub const fn with_sample_size(mut self, value: NonZeroU32) -> Self {
        self.sample_size = Some(value);
        self
    }

    /// Добавляет default sample flags, где ноль остаётся осмысленным ISO значением.
    pub const fn with_sample_flags(mut self, value: u32) -> Self {
        self.sample_flags = Some(value);
        self
    }

    /// Возвращает duration default.
    pub const fn sample_duration(self) -> Option<u32> {
        match self.sample_duration {
            Some(value) => Some(value.get()),
            None => None,
        }
    }

    /// Возвращает size default.
    pub const fn sample_size(self) -> Option<u32> {
        match self.sample_size {
            Some(value) => Some(value.get()),
            None => None,
        }
    }

    /// Возвращает flags default.
    pub const fn sample_flags(self) -> Option<u32> {
        self.sample_flags
    }
}

/// Все authoritative ожидания одного track fragment-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FragmentTrackExpectation {
    track_id: FragmentTrackId,
    base_decode_time: FragmentBaseDecodeTime,
    rap_requirement: FragmentRapRequirement,
    sample_defaults: FragmentSampleDefaults,
}

impl FragmentTrackExpectation {
    /// Группирует typed caller evidence в один intent.
    pub(crate) const fn new(
        track_id: FragmentTrackId,
        base_decode_time: FragmentBaseDecodeTime,
        rap_requirement: FragmentRapRequirement,
        sample_defaults: FragmentSampleDefaults,
    ) -> Self {
        Self {
            track_id,
            base_decode_time,
            rap_requirement,
            sample_defaults,
        }
    }

    /// Возвращает track ID.
    pub(crate) const fn track_id(self) -> FragmentTrackId {
        self.track_id
    }

    /// Возвращает decode-time anchor.
    pub(crate) const fn base_decode_time(self) -> FragmentBaseDecodeTime {
        self.base_decode_time
    }

    /// Возвращает RAP policy.
    pub(crate) const fn rap_requirement(self) -> FragmentRapRequirement {
        self.rap_requirement
    }

    /// Возвращает init defaults.
    pub(crate) const fn sample_defaults(self) -> FragmentSampleDefaults {
        self.sample_defaults
    }
}

/// Полный запрос F1A с borrowed input и injected cancellation.
pub(crate) struct FragmentInspectionRequest<'input, 'config> {
    input: &'input [u8],
    composition_offset_semantics: FragmentCompositionOffsetSemantics,
    expectation: FragmentTrackExpectation,
    limits: &'config FragmentInspectionLimits,
    cancellation: &'config dyn Fn() -> bool,
}

impl<'input, 'config> FragmentInspectionRequest<'input, 'config> {
    /// Создаёт запрос без hidden defaults и без positional flags.
    pub(crate) const fn new(
        input: &'input [u8],
        composition_offset_semantics: FragmentCompositionOffsetSemantics,
        expectation: FragmentTrackExpectation,
        limits: &'config FragmentInspectionLimits,
        cancellation: &'config dyn Fn() -> bool,
    ) -> Self {
        Self {
            input,
            composition_offset_semantics,
            expectation,
            limits,
            cancellation,
        }
    }

    /// Возвращает raw fragment только для bounded parsing.
    pub(crate) const fn input(&self) -> &'input [u8] {
        self.input
    }

    /// Возвращает явно выбранную caller-ом семантику composition offsets.
    pub(crate) const fn composition_offset_semantics(
        &self,
    ) -> FragmentCompositionOffsetSemantics {
        self.composition_offset_semantics
    }

    /// Возвращает authoritative expectation.
    pub(crate) const fn expectation(&self) -> FragmentTrackExpectation {
        self.expectation
    }

    /// Возвращает обязательные limits.
    pub(crate) const fn limits(&self) -> &FragmentInspectionLimits {
        self.limits
    }

    /// Проверяет injected cancellation.
    pub(crate) fn is_cancelled(&self) -> bool {
        (self.cancellation)()
    }
}

/// Нормализованная metadata одного sample-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedFragmentSample {
    dts: u64,
    pts: u64,
    duration: u32,
    composition_offset: i64,
    flags: Option<u32>,
    payload_range: Range<usize>,
}

impl NormalizedFragmentSample {
    /// Возвращает decode timestamp.
    pub(crate) const fn dts(&self) -> u64 {
        self.dts
    }

    /// Возвращает presentation timestamp.
    pub(crate) const fn pts(&self) -> u64 {
        self.pts
    }

    /// Возвращает sample duration.
    pub(crate) const fn duration(&self) -> u32 {
        self.duration
    }

    /// Возвращает signed/unsigned composition offset без потери знака.
    pub(crate) const fn composition_offset(&self) -> i64 {
        self.composition_offset
    }

    /// Возвращает effective flags либо явное отсутствие evidence.
    pub(crate) const fn flags(&self) -> Option<u32> {
        self.flags
    }

    /// Возвращает range внутри исходного fragment-а.
    pub(crate) const fn payload_range(&self) -> &Range<usize> {
        &self.payload_range
    }
}

/// Фактическое coded покрытие fragment-а без manifest presentation policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentCodedCoverage {
    start: u64,
    end_exclusive: u64,
}

impl FragmentCodedCoverage {
    /// Проверяет направление interval-а после checked accumulation sample durations.
    pub(super) fn checked(start: u64, end_exclusive: u64) -> Result<Self, FragmentInspectionError> {
        end_exclusive
            .checked_sub(start)
            .ok_or(FragmentInspectionError::ArithmeticOverflow {
                operation: FragmentArithmeticOperation::DecodeTime,
            })?;
        Ok(Self {
            start,
            end_exclusive,
        })
    }

    /// Возвращает первый coded DTS.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Возвращает exclusive конец coded interval.
    pub const fn end_exclusive(self) -> u64 {
        self.end_exclusive
    }

    /// Возвращает checked coded duration.
    pub const fn duration(self) -> u64 {
        // Constructor уже доказал invariant `end_exclusive >= start`.
        self.end_exclusive - self.start
    }
}

/// Bounded normalized plan, который заимствует raw fragment и не копирует media bytes.
pub(crate) struct NormalizedFragmentPlan<'input> {
    input: &'input [u8],
    sequence_number: u32,
    track_id: FragmentTrackId,
    coded_coverage: FragmentCodedCoverage,
    mdat_payload_range: Range<usize>,
    samples: Vec<NormalizedFragmentSample>,
}

impl<'input> NormalizedFragmentPlan<'input> {
    /// Создаётся только inspector-ом после всех доказательств.
    pub(super) fn verified(
        input: &'input [u8],
        sequence_number: u32,
        track_id: FragmentTrackId,
        coded_coverage: FragmentCodedCoverage,
        mdat_payload_range: Range<usize>,
        samples: Vec<NormalizedFragmentSample>,
    ) -> Self {
        Self {
            input,
            sequence_number,
            track_id,
            coded_coverage,
            mdat_payload_range,
            samples,
        }
    }

    /// Возвращает sequence number из `mfhd`.
    pub(crate) const fn sequence_number(&self) -> u32 {
        self.sequence_number
    }

    /// Возвращает проверенный track ID.
    pub(crate) const fn track_id(&self) -> FragmentTrackId {
        self.track_id
    }

    /// Возвращает decode-time anchor.
    pub(crate) const fn base_decode_time(&self) -> u64 {
        self.coded_coverage.start()
    }

    /// Возвращает фактическое coded покрытие.
    pub(crate) const fn coded_coverage(&self) -> FragmentCodedCoverage {
        self.coded_coverage
    }

    /// Возвращает payload range единственного `mdat`.
    pub(crate) const fn mdat_payload_range(&self) -> &Range<usize> {
        &self.mdat_payload_range
    }

    /// Возвращает normalized metadata.
    pub(crate) fn samples(&self) -> &[NormalizedFragmentSample] {
        &self.samples
    }

    /// Заимствует sample bytes по уже доказанному range без копирования.
    pub(crate) fn sample_payload(&self, sample_index: usize) -> Option<&'input [u8]> {
        let range = self.samples.get(sample_index)?.payload_range.clone();
        self.input.get(range)
    }
}

impl fmt::Debug for NormalizedFragmentPlan<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Debug показывает только bounded metadata и никогда не форматирует input/sample bytes.
        formatter
            .debug_struct("NormalizedFragmentPlan")
            .field("sequence_number", &self.sequence_number)
            .field("track_id", &self.track_id)
            .field("coded_coverage", &self.coded_coverage)
            .field("mdat_payload_range", &self.mdat_payload_range)
            .field("sample_count", &self.samples.len())
            .finish()
    }
}

/// Inspector использует этот constructor, чтобы поля sample-а не стали mutable boundary.
pub(super) fn verified_sample(
    dts: u64,
    pts: u64,
    duration: u32,
    composition_offset: i64,
    flags: Option<u32>,
    payload_range: Range<usize>,
) -> NormalizedFragmentSample {
    NormalizedFragmentSample {
        dts,
        pts,
        duration,
        composition_offset,
        flags,
        payload_range,
    }
}
