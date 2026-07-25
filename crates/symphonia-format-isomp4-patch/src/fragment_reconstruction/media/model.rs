//! Public intent и owning result canonical media reconstruction-а.

use std::fmt;

use super::super::limits::FragmentInspectionLimits;
use super::super::model::{
    FragmentBaseDecodeTime, FragmentCodedCoverage, FragmentSampleDefaults, FragmentTrackId,
};
use super::error::FragmentWriteLimitBuildError;

/// Media kind одновременно задаёт допустимую RAP policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentMediaKind {
    /// Video требует proven RAP на первом sample-е и flags на каждом sample-е.
    VideoWithRequiredProvenRandomAccess,
    /// Audio не получает искусственной RAP semantics.
    AudioWithoutRandomAccessRequirement,
}

/// Authoritative track intent, отдельный от raw fragment storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentTrackReconstructionIntent {
    track_id: FragmentTrackId,
    base_decode_time: FragmentBaseDecodeTime,
    media_kind: FragmentMediaKind,
    sample_defaults: FragmentSampleDefaults,
}

impl FragmentTrackReconstructionIntent {
    /// Создаёт полный intent без positional flags и скрытых defaults.
    pub const fn new(
        track_id: FragmentTrackId,
        base_decode_time: FragmentBaseDecodeTime,
        media_kind: FragmentMediaKind,
        sample_defaults: FragmentSampleDefaults,
    ) -> Self {
        Self {
            track_id,
            base_decode_time,
            media_kind,
            sample_defaults,
        }
    }

    /// Возвращает expected ISO track ID.
    pub const fn track_id(self) -> FragmentTrackId {
        self.track_id
    }

    /// Возвращает authoritative decode-time anchor.
    pub const fn base_decode_time(self) -> FragmentBaseDecodeTime {
        self.base_decode_time
    }

    /// Возвращает media/RAP policy.
    pub const fn media_kind(self) -> FragmentMediaKind {
        self.media_kind
    }

    /// Возвращает caller-provided `trex` defaults.
    pub const fn sample_defaults(self) -> FragmentSampleDefaults {
        self.sample_defaults
    }
}

/// Mandatory budget только для owning canonical output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentWriteLimits {
    maximum_output_bytes: usize,
}

impl FragmentWriteLimits {
    /// Создаёт write limits без default и отвергает бесполезный ноль.
    pub const fn try_new(
        maximum_output_bytes: usize,
    ) -> Result<Self, FragmentWriteLimitBuildError> {
        if maximum_output_bytes == 0 {
            return Err(FragmentWriteLimitBuildError::ZeroMaximumOutputBytes);
        }
        Ok(Self {
            maximum_output_bytes,
        })
    }

    /// Возвращает maximum owning output.
    pub const fn maximum_output_bytes(self) -> usize {
        self.maximum_output_bytes
    }
}

/// Полный borrowed request с независимыми inspection/write budgets.
pub struct FragmentReconstructionRequest<'input, 'policy> {
    input: &'input [u8],
    track: FragmentTrackReconstructionIntent,
    inspection_limits: &'policy FragmentInspectionLimits,
    write_limits: FragmentWriteLimits,
    cancellation: &'policy dyn Fn() -> bool,
}

impl<'input, 'policy> FragmentReconstructionRequest<'input, 'policy> {
    /// Создаёт opt-in request без runtime/provider policy.
    pub const fn new(
        input: &'input [u8],
        track: FragmentTrackReconstructionIntent,
        inspection_limits: &'policy FragmentInspectionLimits,
        write_limits: FragmentWriteLimits,
        cancellation: &'policy dyn Fn() -> bool,
    ) -> Self {
        Self {
            input,
            track,
            inspection_limits,
            write_limits,
            cancellation,
        }
    }

    /// Возвращает недоверенный raw input только внутреннему inspector-у.
    pub(super) const fn input(&self) -> &'input [u8] {
        self.input
    }

    /// Возвращает authoritative track intent.
    pub const fn track(&self) -> FragmentTrackReconstructionIntent {
        self.track
    }

    /// Возвращает mandatory inspection budgets.
    pub const fn inspection_limits(&self) -> &FragmentInspectionLimits {
        self.inspection_limits
    }

    /// Возвращает mandatory write budget.
    pub const fn write_limits(&self) -> FragmentWriteLimits {
        self.write_limits
    }

    /// Проверяет injected cancellation.
    pub(super) fn is_cancelled(&self) -> bool {
        (self.cancellation)()
    }

    /// Передаёт callback внутренним фазам без ownership leak-а.
    pub(super) const fn cancellation(&self) -> &'policy dyn Fn() -> bool {
        self.cancellation
    }
}

/// Owning canonical `moof+mdat` и подтверждённая identity/timeline metadata.
pub struct ReconstructedMediaSegment {
    bytes: Vec<u8>,
    sequence_number: u32,
    track_id: FragmentTrackId,
    coded_coverage: FragmentCodedCoverage,
}

impl ReconstructedMediaSegment {
    /// Публикует только результат полностью проверенного writer-а.
    pub(super) fn verified(
        bytes: Vec<u8>,
        sequence_number: u32,
        track_id: FragmentTrackId,
        coded_coverage: FragmentCodedCoverage,
    ) -> Self {
        Self {
            bytes,
            sequence_number,
            track_id,
            coded_coverage,
        }
    }

    /// Возвращает canonical bytes без дополнительной копии.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Передаёт ownership canonical bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Возвращает exact `mfhd.sequence_number`.
    pub const fn sequence_number(&self) -> u32 {
        self.sequence_number
    }

    /// Возвращает exact track ID.
    pub const fn track_id(&self) -> FragmentTrackId {
        self.track_id
    }

    /// Возвращает фактическое coded coverage без manifest policy.
    pub const fn coded_coverage(&self) -> FragmentCodedCoverage {
        self.coded_coverage
    }
}

impl fmt::Debug for ReconstructedMediaSegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconstructedMediaSegment")
            .field("byte_length", &self.bytes.len())
            .field("sequence_number", &self.sequence_number)
            .field("track_id", &self.track_id)
            .field("coded_coverage", &self.coded_coverage)
            .finish()
    }
}
