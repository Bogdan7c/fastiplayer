//! Accounting обязательных fragment inspection budgets.

use std::mem::size_of;

use crate::atoms::AtomHeader;

use super::atom::known_payload_size;
use super::error::{
    FragmentArithmeticOperation, FragmentInspectionError, FragmentInspectionLimitKind,
};
use super::limits::FragmentInspectionLimits;
use super::model::NormalizedFragmentSample;
use super::support::{checked_add, checked_multiply, enforce_limit};

/// Общий budget state одного вызова.
pub(super) struct InspectionBudget<'limits> {
    limits: &'limits FragmentInspectionLimits,
    box_count: usize,
    traf_count: usize,
    trun_count: usize,
    sample_count: usize,
    sample_metadata_bytes: usize,
}

impl<'limits> InspectionBudget<'limits> {
    /// Начинает нулевой accounting с обязательными limits.
    pub(super) const fn new(limits: &'limits FragmentInspectionLimits) -> Self {
        Self {
            limits,
            box_count: 0,
            traf_count: 0,
            trun_count: 0,
            sample_count: 0,
            sample_metadata_bytes: 0,
        }
    }

    /// Возвращает limits для allocation preflight.
    pub(super) const fn limits(&self) -> &FragmentInspectionLimits {
        self.limits
    }

    /// Возвращает уже принятое число samples.
    pub(super) const fn sample_count(&self) -> usize {
        self.sample_count
    }

    /// Учитывает header до чтения payload-а.
    pub(super) fn accept_header(
        &mut self,
        header: &AtomHeader,
        depth: usize,
    ) -> Result<(), FragmentInspectionError> {
        enforce_limit(
            FragmentInspectionLimitKind::BoxDepth,
            self.limits.max_box_depth(),
            depth,
        )?;
        self.box_count = checked_add(self.box_count, 1, FragmentArithmeticOperation::SampleCount)?;
        enforce_limit(
            FragmentInspectionLimitKind::BoxCount,
            self.limits.max_box_count(),
            self.box_count,
        )?;
        enforce_limit(
            FragmentInspectionLimitKind::BoxPayloadBytes,
            self.limits.max_box_payload_bytes(),
            known_payload_size(header)?,
        )
    }

    /// Учитывает следующий `traf`.
    pub(super) fn accept_traf(&mut self) -> Result<(), FragmentInspectionError> {
        self.traf_count =
            checked_add(self.traf_count, 1, FragmentArithmeticOperation::SampleCount)?;
        enforce_limit(
            FragmentInspectionLimitKind::TrackFragments,
            self.limits.max_traf_count(),
            self.traf_count,
        )
    }

    /// Учитывает следующий `trun` и его allocations до parser loop-а.
    pub(super) fn accept_trun(
        &mut self,
        sample_count: usize,
        encoded_table_bytes: usize,
    ) -> Result<(), FragmentInspectionError> {
        self.trun_count =
            checked_add(self.trun_count, 1, FragmentArithmeticOperation::SampleCount)?;
        enforce_limit(
            FragmentInspectionLimitKind::TrackRuns,
            self.limits.max_trun_count(),
            self.trun_count,
        )?;
        self.sample_count = checked_add(
            self.sample_count,
            sample_count,
            FragmentArithmeticOperation::SampleCount,
        )?;
        enforce_limit(
            FragmentInspectionLimitKind::Samples,
            self.limits.max_samples(),
            self.sample_count,
        )?;
        let normalized_bytes = checked_multiply(
            sample_count,
            size_of::<NormalizedFragmentSample>(),
            FragmentArithmeticOperation::SampleMetadataBytes,
        )?;
        let owned_bytes = checked_add(
            encoded_table_bytes,
            normalized_bytes,
            FragmentArithmeticOperation::SampleMetadataBytes,
        )?;
        self.sample_metadata_bytes = checked_add(
            self.sample_metadata_bytes,
            owned_bytes,
            FragmentArithmeticOperation::SampleMetadataBytes,
        )?;
        enforce_limit(
            FragmentInspectionLimitKind::SampleTableBytes,
            self.limits.max_sample_table_bytes(),
            self.sample_metadata_bytes,
        )
    }
}
